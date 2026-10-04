//! Minimal-but-complete 32-bit Mach-O reader aimed at running an ARMv7 iOS
//! executable (this repository's target is *The Simpsons Arcade* for iPhone,
//! whose decompilation lives in `simpsons3_iphone_en.txt`).
//!
//! The crate deliberately has no dependencies and no `unsafe`: it validates the
//! file as it parses, so a corrupt or hostile Mach-O can never make the loader
//! read outside the image.
//!
//! ```no_run
//! let image = macho::MachO::from_path("Simpsons.app/Simpsons")?;
//! println!("entry = {:#x}", image.entry_pc()?);
//! # Ok::<(), macho::MachOError>(())
//! ```
//!
//! Layout of the crate:
//!
//! * [`consts`] — numeric constants from the XNU headers.
//! * [`types`] — `mach_header`, `segment_command`, `section`, `nlist`, ...
//! * [`image`] — the parser (`parse_bytes`) and [`image::MachO`] queries.
//! * [`dyld`] — rebase/bind opcode streams and the export trie.

pub mod consts;
pub mod dyld;
pub mod error;
pub mod image;
pub mod leb;
pub mod test_support;
pub mod types;

pub use consts::*;
pub use dyld::{bind_records, export_lookup, export_symbols, lazy_bind_records, rebase_locations, BindRecord, ExportSymbol, RebaseLocation};
pub use error::{MachOError, Result};
pub use image::{parse_bytes, EntryPoint, MachO};
pub use types::*;

impl MachO {
    /// Parse an in-memory Mach-O image.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        parse_bytes(data)
    }

    /// Read and parse a file from disk.
    pub fn from_path<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        let data = std::fs::read(path.as_ref()).map_err(|e| {
            MachOError::NotFound(format!("{}: {e}", path.as_ref().display()))
        })?;
        Self::from_bytes(data)
    }

    /// Parse a file that may be a universal ("fat") container, returning the
    /// slice that matches `want_cputype`.  iOS app store binaries are usually
    /// thin `armv7`, but locally built ones can be fat.
    pub fn from_path_slice<P: AsRef<std::path::Path>>(path: P, want_cputype: u32) -> Result<Self> {
        let data = std::fs::read(path.as_ref()).map_err(|e| {
            MachOError::NotFound(format!("{}: {e}", path.as_ref().display()))
        })?;
        match fat_slice(&data, want_cputype)? {
            Some(range) => Self::from_bytes(data[range].to_vec()),
            None => Self::from_bytes(data),
        }
    }
}

/// If `data` starts with a fat header, return the byte range of the requested
/// architecture (or, when `want_cputype` is not present, the armv7 slice).
pub fn fat_slice(data: &[u8], want_cputype: u32) -> Result<Option<core::ops::Range<usize>>> {
    if data.len() < 8 {
        return Ok(None);
    }
    let magic = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    if magic != FAT_MAGIC {
        return Ok(None);
    }
    let nfat = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let mut candidates: Vec<(u32, usize, usize)> = Vec::new();
    for i in 0..nfat {
        let off = 8 + i * 20;
        if off + 20 > data.len() {
            break;
        }
        let cputype = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let slice_off = u32::from_be_bytes([data[off + 8], data[off + 9], data[off + 10], data[off + 11]]) as usize;
        let size = u32::from_be_bytes([data[off + 12], data[off + 13], data[off + 14], data[off + 15]]) as usize;
        if slice_off + size > data.len() {
            continue;
        }
        candidates.push((cputype, slice_off, size));
    }
    let pick = candidates
        .iter()
        .find(|(ty, _, _)| *ty == want_cputype)
        .or_else(|| candidates.iter().find(|(ty, _, _)| *ty == CPU_TYPE_ARM))
        .or_else(|| candidates.first());
    Ok(pick.map(|(_, off, size)| *off..off + size))
}

#[cfg(test)]
mod tests;
