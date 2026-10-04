//! `LC_DYLD_INFO` processing: rebase opcodes, bind opcodes and the export trie.
//!
//! These streams are what `dyld` walks when it slides a non-`MH_PIE` image and
//! when it writes addresses of imported functions into `__nl_symbol_ptr` /
//! `__la_symbol_ptr`.  An emulator has to do the same thing: the rebase pass
//! turns file-relative pointers into guest-virtual ones, and the bind pass is
//! where the syscall/HLE layer gets to substitute host implementations for
//! `_objc_msgSend`, `_glDrawArrays`, `_malloc`, ...
//!
//! Encoding reference: `<mach-o/loader.h>` (`REBASE_OPCODE_*`, `BIND_OPCODE_*`)
//! and dyld's `ImageLoaderMachOCompressed.cpp`.

use crate::consts::*;
use crate::error::{MachOError, Result};
use crate::image::MachO;
use crate::leb::Reader;

/// Byte width of a rebased / bound pointer in this 32-bit image.
pub const POINTER_SIZE: u32 = 4;

/// One pointer that must be slid by the image's load address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebaseLocation {
    /// Guest virtual address of the pointer.
    pub address: u32,
    /// `REBASE_TYPE_*`.
    pub kind: u8,
}

/// One pointer that must be rewritten with the address of an imported symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindRecord {
    /// Guest virtual address of the pointer slot.
    pub address: u32,
    /// Symbol name as it appears in the bind stream (with a leading `_` for C
    /// symbols, e.g. `_printf`).
    pub symbol: String,
    /// `BIND_SYMBOL_FLAGS_*` from `SET_SYMBOL_TRAILING_FLAGS_IMM`.
    pub flags: u8,
    /// Library ordinal, negative values are the `BIND_SPECIAL_DYLIB_*` markers.
    pub dylib_ordinal: i32,
    pub addend: i64,
    /// `BIND_TYPE_*`.
    pub kind: u8,
    /// True when the record came from the lazy bind stream.
    pub lazy: bool,
}

fn segment_vmaddr(macho: &MachO, index: usize) -> Result<u32> {
    macho
        .segments
        .get(index)
        .map(|s| s.vmaddr)
        .ok_or_else(|| MachOError::BadIndex { what: "segment index in dyld stream", index: index as u64, len: macho.segments.len() })
}

/// Walk the `rebase` opcode stream.
pub fn rebase_locations(macho: &MachO) -> Result<Vec<RebaseLocation>> {
    let mut out = Vec::new();
    let Some(info) = macho.dyld_info else { return Ok(out) };
    if !info.has_rebases() {
        return Ok(out);
    }
    let bytes = macho.file_bytes(info.rebase_off, info.rebase_size);
    let mut r = Reader::new(bytes, "rebase");
    let mut kind = REBASE_TYPE_POINTER;
    let mut seg_index = 0usize;
    let mut seg_offset: u64 = 0;

    while !r.is_empty() {
        let byte = r.u8()?;
        let opcode = byte & REBASE_OPCODE_MASK;
        let imm = byte & REBASE_IMMEDIATE_MASK;
        match opcode {
            REBASE_OPCODE_DONE => break,
            REBASE_OPCODE_SET_TYPE_IMM => kind = imm,
            REBASE_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB => {
                seg_index = imm as usize;
                seg_offset = r.uleb()?;
            }
            REBASE_OPCODE_ADD_ADDR_ULEB => seg_offset += r.uleb()?,
            REBASE_OPCODE_ADD_ADDR_IMM_SCALED => seg_offset += (imm as u64) * POINTER_SIZE as u64,
            REBASE_OPCODE_DO_REBASE_IMM_TIMES => {
                for _ in 0..imm {
                    out.push(RebaseLocation {
                        address: segment_vmaddr(macho, seg_index)? + seg_offset as u32,
                        kind,
                    });
                    seg_offset += POINTER_SIZE as u64;
                }
            }
            REBASE_OPCODE_DO_REBASE_ULEB_TIMES => {
                let count = r.uleb()?;
                for _ in 0..count {
                    out.push(RebaseLocation {
                        address: segment_vmaddr(macho, seg_index)? + seg_offset as u32,
                        kind,
                    });
                    seg_offset += POINTER_SIZE as u64;
                }
            }
            REBASE_OPCODE_DO_REBASE_ADD_ADDR_ULEB => {
                out.push(RebaseLocation {
                    address: segment_vmaddr(macho, seg_index)? + seg_offset as u32,
                    kind,
                });
                seg_offset += POINTER_SIZE as u64 + r.uleb()?;
            }
            REBASE_OPCODE_DO_REBASE_ULEB_TIMES_SKIPPING_ULEB => {
                let count = r.uleb()?;
                let skip = r.uleb()?;
                for _ in 0..count {
                    out.push(RebaseLocation {
                        address: segment_vmaddr(macho, seg_index)? + seg_offset as u32,
                        kind,
                    });
                    seg_offset += POINTER_SIZE as u64 + skip;
                }
            }
            _ => {
                return Err(MachOError::BadDyldOpcode {
                    stream: "rebase",
                    offset: r.pos - 1,
                    byte,
                })
            }
        }
    }
    Ok(out)
}

/// Walk the regular (non-lazy) bind stream, or the weak bind stream.
pub fn bind_records(macho: &MachO) -> Result<Vec<BindRecord>> {
    let Some(info) = macho.dyld_info else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    if info.has_binds() {
        walk_bind_stream(
            macho.file_bytes(info.bind_off, info.bind_size),
            false,
            macho,
            &mut out,
        )?;
    }
    if info.has_weak_binds() {
        walk_bind_stream(
            macho.file_bytes(info.weak_bind_off, info.weak_bind_size),
            false,
            macho,
            &mut out,
        )?;
    }
    Ok(out)
}

/// Walk the lazy bind stream.
///
/// Each lazy entry is terminated by `BIND_OPCODE_DONE`; dyld resets the state
/// after each one, and so do we.
pub fn lazy_bind_records(macho: &MachO) -> Result<Vec<BindRecord>> {
    let Some(info) = macho.dyld_info else { return Ok(Vec::new()) };
    let mut out = Vec::new();
    if info.has_lazy_binds() {
        walk_bind_stream(
            macho.file_bytes(info.lazy_bind_off, info.lazy_bind_size),
            true,
            macho,
            &mut out,
        )?;
    }
    Ok(out)
}

fn walk_bind_stream(
    bytes: &[u8],
    lazy: bool,
    macho: &MachO,
    out: &mut Vec<BindRecord>,
) -> Result<()> {
    let stream = if lazy { "lazy bind" } else { "bind" };
    let mut r = Reader::new(bytes, stream);
    let mut ordinal: i32 = 0;
    let mut symbol = String::new();
    let mut flags = 0u8;
    let mut kind = BIND_TYPE_POINTER;
    let mut addend: i64 = 0;
    let mut seg_index = 0usize;
    let mut seg_offset: u64 = 0;

    macro_rules! emit {
        () => {{
            if symbol.is_empty() {
                return Err(MachOError::BadDyldOpcode { stream, offset: r.pos.saturating_sub(1), byte: 0x90 });
            }
            out.push(BindRecord {
                address: segment_vmaddr(macho, seg_index)? + seg_offset as u32,
                symbol: symbol.clone(),
                flags,
                dylib_ordinal: ordinal,
                addend,
                kind,
                lazy,
            });
        }};
    }

    while !r.is_empty() {
        let byte = r.u8()?;
        let opcode = byte & BIND_OPCODE_MASK;
        let imm = byte & BIND_IMMEDIATE_MASK;
        match opcode {
            BIND_OPCODE_DONE => {
                if lazy {
                    // End of one lazy entry: reset the recorder state.
                    ordinal = 0;
                    symbol.clear();
                    flags = 0;
                    kind = BIND_TYPE_POINTER;
                    addend = 0;
                    seg_index = 0;
                    seg_offset = 0;
                    continue;
                }
                break;
            }
            BIND_OPCODE_SET_DYLIB_ORDINAL_IMM => ordinal = imm as i32,
            BIND_OPCODE_SET_DYLIB_ORDINAL_ULEB => ordinal = r.uleb()? as i32,
            BIND_OPCODE_SET_DYLIB_SPECIAL_IMM => {
                ordinal = if imm == 0 { 0 } else { (imm | 0xf0) as i8 as i32 }
            }
            BIND_OPCODE_SET_SYMBOL_TRAILING_FLAGS_IMM => {
                flags = imm;
                symbol = r.cstr()?;
            }
            BIND_OPCODE_SET_TYPE_IMM => kind = imm,
            BIND_OPCODE_SET_ADDEND_SLEB => addend = r.sleb()?,
            BIND_OPCODE_SET_SEGMENT_AND_OFFSET_ULEB => {
                seg_index = imm as usize;
                seg_offset = r.uleb()?;
            }
            BIND_OPCODE_ADD_ADDR_ULEB => seg_offset += r.uleb()?,
            BIND_OPCODE_DO_BIND => {
                emit!();
                seg_offset += POINTER_SIZE as u64;
            }
            BIND_OPCODE_DO_BIND_ADD_ADDR_ULEB => {
                emit!();
                seg_offset += POINTER_SIZE as u64 + r.uleb()?;
            }
            BIND_OPCODE_DO_BIND_ADD_ADDR_IMM_SCALED => {
                emit!();
                seg_offset += POINTER_SIZE as u64 + (imm as u64) * POINTER_SIZE as u64;
            }
            BIND_OPCODE_DO_BIND_ULEB_TIMES_SKIPPING_ULEB => {
                let count = r.uleb()?;
                let skip = r.uleb()?;
                for _ in 0..count {
                    emit!();
                    seg_offset += POINTER_SIZE as u64 + skip;
                }
            }
            BIND_OPCODE_THREADED => {
                return Err(MachOError::BadDyldOpcode { stream, offset: r.pos - 1, byte })
            }
            _ => {
                return Err(MachOError::BadDyldOpcode { stream, offset: r.pos - 1, byte })
            }
        }
    }
    Ok(())
}

/// A symbol exported by the image (dyld export trie).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportSymbol {
    pub name: String,
    pub flags: u64,
    /// Guest virtual address for regular exports.
    pub address: Option<u32>,
    /// `(ordinal, name)` for re-exports.
    pub import: Option<(u64, String)>,
}

impl ExportSymbol {
    pub fn is_reexport(&self) -> bool {
        self.flags & EXPORT_SYMBOL_FLAGS_REEXPORT != 0
    }
    pub fn is_thread_local(&self) -> bool {
        self.flags & EXPORT_SYMBOL_FLAGS_KIND_MASK == EXPORT_SYMBOL_FLAGS_KIND_THREAD_LOCAL
    }
    pub fn is_weak(&self) -> bool {
        self.flags & EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION != 0
    }
}

pub const EXPORT_SYMBOL_FLAGS_KIND_MASK: u64 = 0x03;
pub const EXPORT_SYMBOL_FLAGS_KIND_REGULAR: u64 = 0x00;
pub const EXPORT_SYMBOL_FLAGS_KIND_THREAD_LOCAL: u64 = 0x01;
pub const EXPORT_SYMBOL_FLAGS_KIND_ABSOLUTE: u64 = 0x02;
pub const EXPORT_SYMBOL_FLAGS_WEAK_DEFINITION: u64 = 0x04;
pub const EXPORT_SYMBOL_FLAGS_REEXPORT: u64 = 0x08;
pub const EXPORT_SYMBOL_FLAGS_STUB_AND_RESOLVER: u64 = 0x10;

/// Decode the whole export trie.
pub fn export_symbols(macho: &MachO) -> Result<Vec<ExportSymbol>> {
    let mut out = Vec::new();
    let Some(info) = macho.dyld_info else { return Ok(out) };
    if !info.has_exports() {
        return Ok(out);
    }
    let bytes = macho.file_bytes(info.export_off, info.export_size);
    // The trie is a DAG in theory but dyld emits a tree; guard against cycles.
    let mut visited = vec![false; bytes.len()];
    walk_trie_node(macho, bytes, 0, String::new(), &mut out, &mut visited)?;
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

/// Look up a single export without decoding the whole trie.
pub fn export_lookup(macho: &MachO, name: &str) -> Result<Option<ExportSymbol>> {
    let mut out = Vec::new();
    let Some(info) = macho.dyld_info else { return Ok(None) };
    if !info.has_exports() {
        return Ok(None);
    }
    let bytes = macho.file_bytes(info.export_off, info.export_size);
    let mut visited = vec![false; bytes.len()];
    walk_trie_node(macho, bytes, 0, String::new(), &mut out, &mut visited)?;
    Ok(out.into_iter().find(|e| e.name == name))
}

fn walk_trie_node(
    macho: &MachO,
    bytes: &[u8],
    node: usize,
    prefix: String,
    out: &mut Vec<ExportSymbol>,
    visited: &mut Vec<bool>,
) -> Result<()> {
    if node >= bytes.len() || visited[node] {
        return Ok(());
    }
    visited[node] = true;

    let mut r = Reader::new(&bytes[node..], "export trie");
    let terminal_size = r.uleb()? as usize;
    let terminal_start = r.pos;
    if terminal_size > 0 {
        let flags = r.uleb()?;
        let text = text_base(macho);
        if flags & EXPORT_SYMBOL_FLAGS_REEXPORT != 0 {
            let ordinal = r.uleb()?;
            let import = r.cstr().unwrap_or_default();
            out.push(ExportSymbol {
                name: prefix.clone(),
                flags,
                address: None,
                import: Some((ordinal, import)),
            });
        } else {
            let offset = r.uleb()? as u32;
            let absolute = flags & EXPORT_SYMBOL_FLAGS_KIND_MASK == EXPORT_SYMBOL_FLAGS_KIND_ABSOLUTE;
            let address = if absolute { offset } else { text.wrapping_add(offset) };
            out.push(ExportSymbol { name: prefix.clone(), flags, address: Some(address), import: None });
        }
    }

    // The child table always starts right after the terminal payload, whose
    // length is given explicitly by `terminal_size` (the payload may contain
    // more than just the address, e.g. a stub-and-resolver pair).
    let children_start = node + terminal_start + terminal_size;
    if children_start >= bytes.len() {
        return Ok(());
    }
    let mut cr = Reader::new(&bytes[children_start..], "export trie children");
    let children = cr.u8()? as usize;
    for _ in 0..children {
        let edge = cr.cstr()?;
        let child_off = cr.uleb()? as usize;
        let mut name = prefix.clone();
        name.push_str(&edge);
        walk_trie_node(macho, bytes, child_off, name, out, visited)?;
    }
    Ok(())
}

fn text_base(macho: &MachO) -> u32 {
    macho.text_segment().map(|s| s.vmaddr).unwrap_or(0)
}
