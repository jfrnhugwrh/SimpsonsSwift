//! A minimal ZIP reader: enough of the format to open an `.ipa`.
//!
//! An `.ipa` is an ordinary ZIP with a `Payload/` directory, so the importer
//! needs the end-of-central-directory record, the central directory and the
//! local file headers — plus CRC-32, because "the file is 50 MB of plausible
//! looking bytes" is not the same as "the file is intact".  ZIP64 is supported
//! (App Store packages are large and `zip` happily switches to it), as are the
//! data descriptors that streaming writers use: the central directory is
//! authoritative for sizes and CRCs, so the local header is only consulted for
//! the offset at which the compressed bytes start.
//!
//! No external dependencies, no `unsafe`, and every offset is range-checked
//! before it is used, in the same spirit as the `macho` crate.

use crate::error::{IpaError, Result};
use crate::inflate::inflate_limited;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_ZIP64_EOCD: u32 = 0x0606_4b50;
const SIG_ZIP64_LOCATOR: u32 = 0x0706_4b50;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

/// A single entry is read into memory before it is written out, so cap it: a
/// 50 MB IPA never contains a 4 GB file, and a central directory that claims one
/// is a bomb rather than a game.
pub const MAX_ENTRY_BYTES: usize = 1 << 30;

/// The longest archive comment (64 KiB) plus the 22-byte end record: how far
/// back from the end of the file the end-of-central-directory can sit.
const EOCD_MAX_SCAN: usize = 22 + 0xffff;

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { 0xedb8_8320 ^ (crc >> 1) } else { crc >> 1 };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
};

/// The ZIP CRC-32 (the reflected polynomial `0xEDB88320`).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc = CRC_TABLE[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

fn u16_at(data: &[u8], offset: usize, what: &'static str) -> Result<u16> {
    if offset + 2 > data.len() {
        return Err(IpaError::Corrupt { what: format!("reading {what} at {offset:#x} runs past the end of the file") });
    }
    Ok(u16::from_le_bytes([data[offset], data[offset + 1]]))
}

fn u32_at(data: &[u8], offset: usize, what: &'static str) -> Result<u32> {
    if offset + 4 > data.len() {
        return Err(IpaError::Corrupt { what: format!("reading {what} at {offset:#x} runs past the end of the file") });
    }
    Ok(u32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]]))
}

fn u64_at(data: &[u8], offset: usize, what: &'static str) -> Result<u64> {
    if offset + 8 > data.len() {
        return Err(IpaError::Corrupt { what: format!("reading {what} at {offset:#x} runs past the end of the file") });
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    Ok(u64::from_le_bytes(bytes))
}

/// One central-directory record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path as stored in the archive (`/` separated, no leading `/`).
    pub name: String,
    /// Compression method (0 = stored, 8 = deflate).
    pub method: u16,
    pub crc32: u32,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// High byte is the host system that wrote the entry (3 = Unix).
    pub version_made_by: u16,
    pub external_attrs: u32,
    header_offset: u64,
}

impl Entry {
    /// Directory entries are stored with a trailing slash and nothing else.
    pub fn is_directory(&self) -> bool {
        self.name.ends_with('/')
    }

    /// The Unix permission bits, when the entry was written by a Unix `zip`.
    pub fn unix_mode(&self) -> Option<u32> {
        if self.version_made_by >> 8 == 3 {
            Some((self.external_attrs >> 16) & 0xffff)
        } else {
            None
        }
    }

    /// True for an `S_IFLNK` entry; the importer skips those rather than
    /// creating a symlink that could point outside the destination.
    pub fn is_symlink(&self) -> bool {
        self.unix_mode().is_some_and(|mode| mode & 0o17_0000 == 0o12_0000)
    }

    /// True when the entry's Unix mode has any execute bit set — how the
    /// importer knows to `chmod +x` the app's Mach-O.
    pub fn is_executable(&self) -> bool {
        self.unix_mode().is_some_and(|mode| mode & 0o111 != 0)
    }
}

/// An opened archive.
#[derive(Debug)]
pub struct Zip<'a> {
    data: &'a [u8],
    entries: Vec<Entry>,
    /// Set when the archive needed the ZIP64 end-of-directory record.
    pub zip64: bool,
    /// The archive comment (Xcode writes nothing here; some tools note the
    /// packaging tool).
    pub comment: String,
}

impl<'a> Zip<'a> {
    /// Parse the end-of-central-directory record and the central directory.
    pub fn open(data: &'a [u8]) -> Result<Zip<'a>> {
        if data.len() < 22 {
            return Err(IpaError::NotAZip(format!("the file is only {} bytes", data.len())));
        }
        let magic = u32_at(data, 0, "file signature")?;
        if magic != SIG_LOCAL && magic != SIG_EOCD && magic != 0x0807_4b50 {
            return Err(IpaError::NotAZip(format!(
                "the file starts with {magic:#010x}, not a ZIP signature (PK\\x03\\x04)"
            )));
        }

        let eocd = find_eocd(data)?;
        let disk = u16_at(data, eocd + 4, "disk number")?;
        let cd_disk = u16_at(data, eocd + 6, "central directory disk")?;
        if disk != 0 || cd_disk != 0 {
            return Err(IpaError::Unsupported {
                what: format!("multi-disk ZIP archives (this one spans disks {disk} and {cd_disk})"),
            });
        }
        let mut count = u16_at(data, eocd + 10, "entry count")? as u64;
        let mut cd_size = u32_at(data, eocd + 12, "central directory size")? as u64;
        let mut cd_offset = u32_at(data, eocd + 16, "central directory offset")? as u64;
        let comment_len = u16_at(data, eocd + 20, "comment length")? as usize;
        let comment = data
            .get(eocd + 22..(eocd + 22 + comment_len).min(data.len()))
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_default();

        // A ZIP64 end-of-directory locator sits immediately before the EOCD.
        let mut zip64 = false;
        if eocd >= 20 && u32_at(data, eocd - 20, "zip64 locator")? == SIG_ZIP64_LOCATOR {
            let record = u64_at(data, eocd - 12, "zip64 end record offset")? as usize;
            if record + 56 <= data.len() && u32_at(data, record, "zip64 end record")? == SIG_ZIP64_EOCD {
                count = u64_at(data, record + 32, "zip64 entry count")?;
                cd_size = u64_at(data, record + 40, "zip64 central directory size")?;
                cd_offset = u64_at(data, record + 48, "zip64 central directory offset")?;
                zip64 = true;
            }
        }
        if count == 0xffff || cd_offset == 0xffff_ffff || cd_size == 0xffff_ffff {
            return Err(IpaError::Corrupt {
                what: "the archive needs ZIP64 but has no ZIP64 end-of-central-directory record"
                    .to_string(),
            });
        }
        if count > 1 << 20 {
            return Err(IpaError::TooLarge { what: "archive entry count".to_string(), limit: 1 << 20 });
        }

        let cd_start = cd_offset as usize;
        let cd_end = (cd_offset + cd_size).min(data.len() as u64) as usize;
        if cd_start >= data.len() {
            return Err(IpaError::Corrupt {
                what: format!("central directory starts at {cd_offset:#x}, past the end of the {len}-byte file", len = data.len()),
            });
        }

        let mut entries = Vec::with_capacity(count as usize);
        let mut offset = cd_start;
        while entries.len() < count as usize && offset + 46 <= cd_end {
            if u32_at(data, offset, "central directory signature")? != SIG_CENTRAL {
                break;
            }
            let version_made_by = u16_at(data, offset + 4, "version made by")?;
            let method = u16_at(data, offset + 10, "compression method")?;
            let crc32 = u32_at(data, offset + 16, "crc32")?;
            let mut compressed_size = u32_at(data, offset + 20, "compressed size")? as u64;
            let mut uncompressed_size = u32_at(data, offset + 24, "uncompressed size")? as u64;
            let name_len = u16_at(data, offset + 28, "file name length")? as usize;
            let extra_len = u16_at(data, offset + 30, "extra field length")? as usize;
            let comment_len = u16_at(data, offset + 32, "file comment length")? as usize;
            let external_attrs = u32_at(data, offset + 38, "external attributes")?;
            let mut header_offset = u32_at(data, offset + 42, "local header offset")? as u64;

            let name_start = offset + 46;
            let name_end = name_start + name_len;
            if name_end + extra_len + comment_len > data.len() {
                return Err(IpaError::Corrupt {
                    what: format!("the central directory record at {offset:#x} runs past the end of the file"),
                });
            }
            let name = String::from_utf8_lossy(&data[name_start..name_end]).into_owned();

            // ZIP64 extra field: id 0x0001, then only the fields that were
            // written as 0xffff_ffff in the record itself, in the order
            // uncompressed, compressed, local header offset, disk.
            if uncompressed_size == 0xffff_ffff
                || compressed_size == 0xffff_ffff
                || header_offset == 0xffff_ffff
            {
                let extra = &data[name_end..name_end + extra_len];
                let mut at = 0usize;
                while at + 4 <= extra.len() {
                    let id = u16::from_le_bytes([extra[at], extra[at + 1]]);
                    let size = u16::from_le_bytes([extra[at + 2], extra[at + 3]]) as usize;
                    let body = at + 4;
                    if body + size > extra.len() {
                        break;
                    }
                    if id == 0x0001 {
                        let mut field = body;
                        if uncompressed_size == 0xffff_ffff && field + 8 <= body + size {
                            uncompressed_size = u64_at(extra, field, "zip64 uncompressed size")?;
                            field += 8;
                        }
                        if compressed_size == 0xffff_ffff && field + 8 <= body + size {
                            compressed_size = u64_at(extra, field, "zip64 compressed size")?;
                            field += 8;
                        }
                        if header_offset == 0xffff_ffff && field + 8 <= body + size {
                            header_offset = u64_at(extra, field, "zip64 local header offset")?;
                        }
                        zip64 = true;
                        break;
                    }
                    at = body + size;
                }
            }

            entries.push(Entry {
                name,
                method,
                crc32,
                compressed_size,
                uncompressed_size,
                version_made_by,
                external_attrs,
                header_offset,
            });
            offset = name_end + extra_len + comment_len;
        }
        if entries.is_empty() && count > 0 {
            return Err(IpaError::Corrupt {
                what: format!("the central directory at {cd_offset:#x} holds no readable records"),
            });
        }
        Ok(Zip { data, entries, zip64, comment })
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Look an entry up by exact path.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }

    /// Read and verify an entry's contents.
    pub fn read(&self, entry: &Entry) -> Result<Vec<u8>> {
        if entry.uncompressed_size > MAX_ENTRY_BYTES as u64 {
            return Err(IpaError::TooLarge {
                what: format!("archive entry {:?}", entry.name),
                limit: MAX_ENTRY_BYTES as u64,
            });
        }
        let limit = entry.uncompressed_size as usize;

        let local = entry.header_offset as usize;
        if u32_at(self.data, local, "local file header")? != SIG_LOCAL {
            return Err(IpaError::Corrupt {
                what: format!(
                    "entry {:?} points at {local:#x}, which is not a local file header",
                    entry.name
                ),
            });
        }
        // The *local* name/extra lengths can differ from the central ones (the
        // data descriptor and alignment padding live in the local extra field),
        // so the data offset has to be computed from this header.
        let name_len = u16_at(self.data, local + 26, "local name length")? as usize;
        let extra_len = u16_at(self.data, local + 28, "local extra length")? as usize;
        let start = local + 30 + name_len + extra_len;
        let end = start
            .checked_add(entry.compressed_size as usize)
            .ok_or_else(|| IpaError::Corrupt { what: format!("entry {:?} has an absurd compressed size", entry.name) })?;
        if end > self.data.len() {
            return Err(IpaError::Corrupt {
                what: format!(
                    "entry {:?} needs bytes {start:#x}..{end:#x} but the file is only {} bytes long",
                    entry.name,
                    self.data.len()
                ),
            });
        }
        let raw = &self.data[start..end];

        let bytes = match entry.method {
            METHOD_STORED => {
                if raw.len() as u64 != entry.uncompressed_size {
                    return Err(IpaError::Corrupt {
                        what: format!(
                            "stored entry {:?} is {} bytes but the directory says {}",
                            entry.name,
                            raw.len(),
                            entry.uncompressed_size
                        ),
                    });
                }
                raw.to_vec()
            }
            METHOD_DEFLATE => inflate_limited(raw, limit, limit)?,
            other => {
                return Err(IpaError::Unsupported {
                    what: format!("compression method {other} used by entry {:?}", entry.name),
                })
            }
        };
        if bytes.len() as u64 != entry.uncompressed_size {
            return Err(IpaError::Corrupt {
                what: format!(
                    "entry {:?} decompressed to {} bytes but the directory says {}",
                    entry.name,
                    bytes.len(),
                    entry.uncompressed_size
                ),
            });
        }
        let actual = crc32(&bytes);
        if actual != entry.crc32 {
            return Err(IpaError::Corrupt {
                what: format!(
                    "entry {:?} failed its CRC check (expected {:#010x}, got {actual:#010x}) — the file is damaged",
                    entry.name, entry.crc32
                ),
            });
        }
        Ok(bytes)
    }
}

/// Scan backwards for the end-of-central-directory signature.  The record is at
/// the very end unless the archive has a comment, which is why this cannot just
/// look at a fixed offset.
fn find_eocd(data: &[u8]) -> Result<usize> {
    let earliest = data.len().saturating_sub(EOCD_MAX_SCAN);
    let mut offset = data.len() - 22;
    loop {
        if u32_at(data, offset, "end of central directory")? == SIG_EOCD {
            // The comment length has to account for the rest of the file,
            // otherwise this is a signature that happens to appear in data.
            let comment_len = u16_at(data, offset + 20, "comment length")? as usize;
            if offset + 22 + comment_len == data.len() {
                return Ok(offset);
            }
        }
        if offset == earliest {
            break;
        }
        offset -= 1;
    }
    Err(IpaError::NotAZip(
        "no end-of-central-directory record in the last 64 KiB — the file is truncated or not a ZIP"
            .to_string(),
    ))
}
