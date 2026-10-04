//! The Mach-O reader: turns a byte slice into [`MachO`].

use crate::consts::*;
use crate::error::{MachOError, Result};
use crate::types::*;

/// Where execution starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryPoint {
    /// `LC_MAIN`: offset from the start of the `__TEXT` segment.
    Main { entryoff: u64, stacksize: u64 },
    /// `LC_UNIXTHREAD`: a fully specified initial register set (ARMv7).
    Thread(ThreadRegisters),
}

/// A parsed 32-bit Mach-O image (header + all load commands + symbol tables).
#[derive(Debug, Clone, Default)]
pub struct MachO {
    pub data: Vec<u8>,
    pub header: Header,
    pub segments: Vec<Segment>,
    pub symtab: Option<Symtab>,
    pub dysymtab: Option<Dysymtab>,
    pub dyld_info: Option<DyldInfo>,
    pub dylibs: Vec<Dylib>,
    pub dylinker: Option<String>,
    pub uuid: Option<[u8; 16]>,
    pub entry: Option<EntryPoint>,
    pub encryption: Option<(u32, u32, u32)>,
    pub linkedit: Vec<LinkeditData>,
    pub symbols: Vec<Symbol>,
    pub indirect_symbols: Vec<u32>,
    /// `(platform, version, sdk)` from `LC_VERSION_MIN_IPHONEOS`/`_MACOSX`.
    pub version_min: Option<(&'static str, u32, u32)>,
    pub source_version: Option<u64>,
    pub rpaths: Vec<String>,
    pub unknown_commands: Vec<(u32, u32)>,
}

fn u32_at(data: &[u8], off: usize, what: &'static str) -> Result<u32> {
    if off + 4 > data.len() {
        return Err(MachOError::Truncated { what, offset: off, need: 4, have: data.len() - off });
    }
    Ok(u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]))
}

fn u64_at(data: &[u8], off: usize, what: &'static str) -> Result<u64> {
    if off + 8 > data.len() {
        return Err(MachOError::Truncated { what, offset: off, need: 8, have: data.len() - off });
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[off..off + 8]);
    Ok(u64::from_le_bytes(b))
}

/// Fixed size NUL padded char array (`char name[16]`).
fn name16(data: &[u8], off: usize) -> String {
    if off + 16 > data.len() {
        return String::new();
    }
    let raw = &data[off..off + 16];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

/// `lc_str`: 32-bit offset from the start of the command to a C string.
fn lc_str(data: &[u8], cmd_off: usize, str_off_field: usize) -> String {
    let rel = match u32_at(data, cmd_off + str_off_field, "lc_str offset") {
        Ok(v) => v as usize,
        Err(_) => return String::new(),
    };
    let start = cmd_off + rel;
    if start >= data.len() {
        return String::new();
    }
    let end = data[start..].iter().position(|&b| b == 0).map(|p| start + p).unwrap_or(data.len());
    String::from_utf8_lossy(&data[start..end]).into_owned()
}

pub fn parse_bytes(data: Vec<u8>) -> Result<MachO> {
    let len = data.len();
    // The magic comes first so that "this is not a Mach-O at all" is reported as
    // such rather than as a truncation error.
    let magic = u32_at(&data, 0, "magic")?;
    match magic {
        MH_MAGIC => {}
        MH_CIGAM => {
            return Err(MachOError::UnsupportedFileType {
                what: "byte order (big endian / swapped magic)",
                value: magic as u64,
            })
        }
        MH_MAGIC_64 | MH_CIGAM_64 => {
            return Err(MachOError::UnsupportedFileType {
                what: "64-bit image (this emulator runs ARMv7)",
                value: magic as u64,
            })
        }
        FAT_MAGIC | FAT_CIGAM => {
            return Err(MachOError::UnsupportedFileType {
                what: "universal/fat container — extract the armv7 slice first",
                value: magic as u64,
            })
        }
        other => return Err(MachOError::BadMagic(other)),
    }

    if len < Header::SIZE {
        return Err(MachOError::Truncated {
            what: "mach_header",
            offset: 0,
            need: Header::SIZE,
            have: len,
        });
    }

    let header = Header {
        magic,
        cputype: u32_at(&data, 4, "cputype")?,
        cpusubtype: u32_at(&data, 8, "cpusubtype")?,
        filetype: u32_at(&data, 12, "filetype")?,
        ncmds: u32_at(&data, 16, "ncmds")?,
        sizeofcmds: u32_at(&data, 20, "sizeofcmds")?,
        flags: u32_at(&data, 24, "flags")?,
    };

    let cmds_start = Header::SIZE;
    let cmds_end = cmds_start + header.sizeofcmds as usize;
    if cmds_end > len {
        return Err(MachOError::InconsistentCommands { expected_end: cmds_end, actual_end: len });
    }

    let mut macho = MachO { data, header, ..Default::default() };

    let mut off = cmds_start;
    for _ in 0..header.ncmds {
        let cmd = u32_at(&macho.data, off, "load command")?;
        let cmdsize = u32_at(&macho.data, off + 4, "load command size")?;
        if cmdsize < 8 || off + cmdsize as usize > cmds_end {
            return Err(MachOError::BadCommandSize { cmd, cmdsize, offset: off });
        }
        parse_command(&mut macho, cmd, off, cmdsize as usize)?;
        off += cmdsize as usize;
    }
    if off != cmds_end {
        return Err(MachOError::InconsistentCommands { expected_end: cmds_end, actual_end: off });
    }

    // Validate segment file ranges now that all commands are known.
    for seg in &macho.segments {
        if seg.is_pagezero() {
            continue;
        }
        let end = seg.fileoff as usize + seg.filesize as usize;
        if end > len {
            if seg.is_linkedit() && seg.fileoff as usize <= len {
                // Some stripped/repacked executables keep a __LINKEDIT that
                // extends past EOF; clamp instead of failing the load.
                continue;
            }
            return Err(MachOError::SegmentOutOfFile {
                name: seg.segname.clone(),
                fileoff: seg.fileoff,
                filesize: seg.filesize,
                file_len: len,
            });
        }
    }

    macho.symbols = read_symbols(&macho)?;
    macho.indirect_symbols = read_indirect_symbols(&macho)?;
    Ok(macho)
}

fn parse_command(m: &mut MachO, cmd: u32, off: usize, size: usize) -> Result<()> {
    let d = &m.data;
    match cmd {
        LC_SEGMENT => {
            let segname = name16(d, off + 8);
            let vmaddr = u32_at(d, off + 24, "vmaddr")?;
            let vmsize = u32_at(d, off + 28, "vmsize")?;
            let fileoff = u32_at(d, off + 32, "fileoff")?;
            let filesize = u32_at(d, off + 36, "filesize")?;
            let maxprot = u32_at(d, off + 40, "maxprot")?;
            let initprot = u32_at(d, off + 44, "initprot")?;
            let nsects = u32_at(d, off + 48, "nsects")?;
            let flags = u32_at(d, off + 52, "flags")?;

            let mut sections = Vec::with_capacity(nsects as usize);
            let mut soff = off + 56;
            for _ in 0..nsects {
                if soff + Section::SIZE > off + size {
                    return Err(MachOError::BadCommandSize { cmd, cmdsize: size as u32, offset: off });
                }
                sections.push(Section {
                    sectname: name16(d, soff),
                    segname: name16(d, soff + 16),
                    addr: u32_at(d, soff + 32, "section.addr")?,
                    size: u32_at(d, soff + 36, "section.size")?,
                    offset: u32_at(d, soff + 40, "section.offset")?,
                    align: u32_at(d, soff + 44, "section.align")?,
                    reloff: u32_at(d, soff + 48, "section.reloff")?,
                    nreloc: u32_at(d, soff + 52, "section.nreloc")?,
                    flags: u32_at(d, soff + 56, "section.flags")?,
                    reserved1: u32_at(d, soff + 60, "section.reserved1")?,
                    reserved2: u32_at(d, soff + 64, "section.reserved2")?,
                });
                soff += Section::SIZE;
            }
            m.segments.push(Segment {
                segname,
                vmaddr,
                vmsize,
                fileoff,
                filesize,
                maxprot,
                initprot,
                nsects,
                flags,
                sections,
            });
        }
        LC_SYMTAB => {
            m.symtab = Some(Symtab {
                symoff: u32_at(d, off + 8, "symoff")?,
                nsyms: u32_at(d, off + 12, "nsyms")?,
                stroff: u32_at(d, off + 16, "stroff")?,
                strsize: u32_at(d, off + 20, "strsize")?,
            });
        }
        LC_DYSYMTAB => {
            m.dysymtab = Some(Dysymtab {
                ilocalsym: u32_at(d, off + 8, "ilocalsym")?,
                nlocalsym: u32_at(d, off + 12, "nlocalsym")?,
                iextdefsym: u32_at(d, off + 16, "iextdefsym")?,
                nextdefsym: u32_at(d, off + 20, "nextdefsym")?,
                iundefsym: u32_at(d, off + 24, "iundefsym")?,
                nundefsym: u32_at(d, off + 28, "nundefsym")?,
                tocoff: u32_at(d, off + 32, "tocoff")?,
                ntoc: u32_at(d, off + 36, "ntoc")?,
                modtaboff: u32_at(d, off + 40, "modtaboff")?,
                nmodtab: u32_at(d, off + 44, "nmodtab")?,
                extrefsymoff: u32_at(d, off + 48, "extrefsymoff")?,
                nextrefsyms: u32_at(d, off + 52, "nextrefsyms")?,
                indirectsymoff: u32_at(d, off + 56, "indirectsymoff")?,
                nindirectsyms: u32_at(d, off + 60, "nindirectsyms")?,
                extreloff: u32_at(d, off + 64, "extreloff")?,
                nextrel: u32_at(d, off + 68, "nextrel")?,
                locreloff: u32_at(d, off + 72, "locreloff")?,
                nlocrel: u32_at(d, off + 76, "nlocrel")?,
            });
        }
        LC_DYLD_INFO | LC_DYLD_INFO_ONLY => {
            m.dyld_info = Some(DyldInfo {
                rebase_off: u32_at(d, off + 8, "rebase_off")?,
                rebase_size: u32_at(d, off + 12, "rebase_size")?,
                bind_off: u32_at(d, off + 16, "bind_off")?,
                bind_size: u32_at(d, off + 20, "bind_size")?,
                weak_bind_off: u32_at(d, off + 24, "weak_bind_off")?,
                weak_bind_size: u32_at(d, off + 28, "weak_bind_size")?,
                lazy_bind_off: u32_at(d, off + 32, "lazy_bind_off")?,
                lazy_bind_size: u32_at(d, off + 36, "lazy_bind_size")?,
                export_off: u32_at(d, off + 40, "export_off")?,
                export_size: u32_at(d, off + 44, "export_size")?,
            });
        }
        LC_LOAD_DYLIB | LC_ID_DYLIB | LC_LOAD_WEAK_DYLIB | LC_REEXPORT_DYLIB
        | LC_LOAD_UPWARD_DYLIB | LC_LAZY_LOAD_DYLIB => {
            let kind = match cmd {
                LC_LOAD_DYLIB | LC_LAZY_LOAD_DYLIB => DylibKind::Load,
                LC_LOAD_WEAK_DYLIB => DylibKind::WeakLoad,
                LC_REEXPORT_DYLIB => DylibKind::Reexport,
                LC_LOAD_UPWARD_DYLIB => DylibKind::UpwardLoad,
                LC_ID_DYLIB => DylibKind::Id,
                _ => DylibKind::Load,
            };
            m.dylibs.push(Dylib {
                kind,
                name: lc_str(d, off, 8),
                timestamp: u32_at(d, off + 12, "timestamp")?,
                current_version: u32_at(d, off + 16, "current_version")?,
                compatibility_version: u32_at(d, off + 20, "compatibility_version")?,
            });
        }
        LC_LOAD_DYLINKER | LC_ID_DYLINKER | LC_DYLD_ENVIRONMENT => {
            let name = lc_str(d, off, 8);
            if cmd == LC_LOAD_DYLINKER {
                m.dylinker = Some(name);
            }
        }
        LC_UUID => {
            if off + 24 <= d.len() {
                let mut uuid = [0u8; 16];
                uuid.copy_from_slice(&d[off + 8..off + 24]);
                m.uuid = Some(uuid);
            }
        }
        LC_MAIN => {
            m.entry = Some(EntryPoint::Main {
                entryoff: u64_at(d, off + 8, "entryoff")?,
                stacksize: u64_at(d, off + 16, "stacksize")?,
            });
        }
        LC_UNIXTHREAD | LC_THREAD => {
            let flavor = u32_at(d, off + 8, "flavor")?;
            let count = u32_at(d, off + 12, "count")?;
            let mut words = Vec::with_capacity(count as usize);
            for i in 0..count as usize {
                let woff = off + 16 + i * 4;
                if woff + 4 > off + size {
                    break;
                }
                words.push(u32_at(d, woff, "thread state word")?);
            }
            let state = ThreadState { flavor, count, words };
            if cmd == LC_UNIXTHREAD {
                if let Some(regs) = state.registers() {
                    m.entry = Some(EntryPoint::Thread(regs));
                }
            }
        }
        LC_ENCRYPTION_INFO | LC_ENCRYPTION_INFO_64 => {
            let (cryptoff, cryptsize, cryptid) = (
                u32_at(d, off + 8, "cryptoff")?,
                u32_at(d, off + 12, "cryptsize")?,
                u32_at(d, off + 16, "cryptid")?,
            );
            m.encryption = Some((cryptoff, cryptsize, cryptid));
        }
        LC_CODE_SIGNATURE => {
            m.linkedit.push(LinkeditData {
                kind: "code signature",
                dataoff: u32_at(d, off + 8, "dataoff")?,
                datasize: u32_at(d, off + 12, "datasize")?,
            });
        }
        LC_SEGMENT_SPLIT_INFO => {
            m.linkedit.push(LinkeditData {
                kind: "segment split info",
                dataoff: u32_at(d, off + 8, "dataoff")?,
                datasize: u32_at(d, off + 12, "datasize")?,
            });
        }
        LC_FUNCTION_STARTS => {
            m.linkedit.push(LinkeditData {
                kind: "function starts",
                dataoff: u32_at(d, off + 8, "dataoff")?,
                datasize: u32_at(d, off + 12, "datasize")?,
            });
        }
        LC_DATA_IN_CODE => {
            m.linkedit.push(LinkeditData {
                kind: "data in code",
                dataoff: u32_at(d, off + 8, "dataoff")?,
                datasize: u32_at(d, off + 12, "datasize")?,
            });
        }
        LC_DYLIB_CODE_SIGN_DRS => {
            m.linkedit.push(LinkeditData {
                kind: "code signing DRs",
                dataoff: u32_at(d, off + 8, "dataoff")?,
                datasize: u32_at(d, off + 12, "datasize")?,
            });
        }
        LC_VERSION_MIN_IPHONEOS => {
            m.version_min = Some((
                "iphoneos",
                u32_at(d, off + 8, "version")?,
                u32_at(d, off + 12, "sdk")?,
            ));
        }
        LC_VERSION_MIN_MACOSX => {
            m.version_min = Some((
                "macosx",
                u32_at(d, off + 8, "version")?,
                u32_at(d, off + 12, "sdk")?,
            ));
        }
        LC_SOURCE_VERSION => {
            m.source_version = Some(u64_at(d, off + 8, "source version")?);
        }
        LC_RPATH => {
            m.rpaths.push(lc_str(d, off, 8));
        }
        // Commands that carry no information we need at run time.
        LC_IDENT | LC_SYMSEG | LC_TWOLEVEL_HINTS | LC_PREBIND_CKSUM | LC_LINKER_OPTION
        | LC_LINKER_OPTIMIZATION_HINT | LC_SUB_FRAMEWORK | LC_SUB_UMBRELLA | LC_SUB_CLIENT
        | LC_SUB_LIBRARY | LC_PREBOUND_DYLIB | LC_ROUTINES | LC_FVMFILE | LC_PREPAGE => {}
        other => m.unknown_commands.push((other, size as u32)),
    }
    Ok(())
}

fn read_symbols(m: &MachO) -> Result<Vec<Symbol>> {
    let Some(symtab) = m.symtab else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(symtab.nsyms as usize);
    for i in 0..symtab.nsyms as usize {
        let off = symtab.symoff as usize + i * Symbol::SIZE;
        if off + Symbol::SIZE > m.data.len() {
            break;
        }
        let n_strx = u32_at(&m.data, off, "n_strx")? as usize;
        let n_type = m.data[off + 4];
        let n_sect = m.data[off + 5];
        let n_desc = u16::from_le_bytes([m.data[off + 6], m.data[off + 7]]);
        let n_value = u32_at(&m.data, off + 8, "n_value")?;
        let name = if n_strx == 0 {
            String::new()
        } else {
            let s = symtab.stroff as usize + n_strx;
            if s >= m.data.len() {
                String::new()
            } else {
                let end = m.data[s..].iter().position(|&b| b == 0).map(|p| s + p).unwrap_or(m.data.len());
                String::from_utf8_lossy(&m.data[s..end]).into_owned()
            }
        };
        out.push(Symbol { name, n_type, n_sect, n_desc, n_value });
    }
    Ok(out)
}

fn read_indirect_symbols(m: &MachO) -> Result<Vec<u32>> {
    let Some(dysym) = m.dysymtab else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(dysym.nindirectsyms as usize);
    for i in 0..dysym.nindirectsyms as usize {
        let off = dysym.indirectsymoff as usize + i * 4;
        if off + 4 > m.data.len() {
            break;
        }
        out.push(u32_at(&m.data, off, "indirect symbol")?);
    }
    Ok(out)
}

impl MachO {
    pub fn segment(&self, name: &str) -> Option<&Segment> {
        self.segments.iter().find(|s| s.segname == name)
    }

    pub fn section(&self, seg: &str, sect: &str) -> Option<&Section> {
        self.segments
            .iter()
            .filter(|s| s.segname == seg)
            .flat_map(|s| s.sections.iter())
            .find(|s| s.sectname == sect)
    }

    pub fn sections(&self) -> impl Iterator<Item = &Section> {
        self.segments.iter().flat_map(|s| s.sections.iter())
    }

    pub fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|s| s.name == name)
    }

    /// The `__TEXT` segment (or the first with file data), used as the base for
    /// `LC_MAIN`'s `entryoff`.
    pub fn text_segment(&self) -> Option<&Segment> {
        self.segment("__TEXT").or_else(|| self.segments.iter().find(|s| s.fileoff == 0))
    }

    /// Resolve the initial program counter.
    pub fn entry_pc(&self) -> Result<u32> {
        match self.entry {
            Some(EntryPoint::Main { entryoff, .. }) => {
                let text = self.text_segment().ok_or_else(|| {
                    MachOError::NotFound("__TEXT segment (needed to resolve LC_MAIN entryoff)".into())
                })?;
                Ok(text.vmaddr.wrapping_add(entryoff as u32))
            }
            Some(EntryPoint::Thread(regs)) => Ok(regs.pc),
            None => Err(MachOError::NoEntryPoint),
        }
    }

    /// Section that contains an address, if any.
    pub fn section_containing(&self, addr: u32) -> Option<&Section> {
        self.sections().find(|s| s.contains(addr))
    }

    pub fn is_encrypted(&self) -> bool {
        matches!(self.encryption, Some((_, _, cryptid)) if cryptid != 0)
    }

    /// Number of imported (undefined, external) symbols — the HLE surface.
    pub fn imported_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter().filter(|s| s.needs_binding())
    }

    pub fn undefined_symbol_indices(&self) -> Vec<usize> {
        let (start, count) = match self.dysymtab {
            Some(d) if d.nundefsym > 0 => (d.iundefsym as usize, d.nundefsym as usize),
            _ => {
                return self
                    .symbols
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.needs_binding())
                    .map(|(i, _)| i)
                    .collect()
            }
        };
        (start..start + count).take_while(|&i| i < self.symbols.len()).collect()
    }

    /// Read a byte range of the file, clamped to what is actually present.
    pub fn file_bytes(&self, offset: u32, size: u32) -> &[u8] {
        let start = (offset as usize).min(self.data.len());
        let end = (offset as usize + size as usize).min(self.data.len());
        &self.data[start..end]
    }
}
