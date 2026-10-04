//! A small Mach-O writer used by tests (and by the emulator's end-to-end
//! test-suite) to produce *real* ARMv7 iOS executables — header, load commands,
//! `__TEXT`/`__DATA`/`__LINKEDIT`, symbol table, indirect symbols, dyld rebase
//! and bind streams, export trie and `LC_MAIN`.
//!
//! Building images here (rather than shipping opaque binaries) means the tests
//! exercise the very same code paths the real *Simpsons Arcade* executable
//! needs: dyld must be able to rebase pointers, bind lazy/non-lazy pointers and
//! find the entry point of these images.

use crate::consts::*;
use std::collections::HashMap;

pub const PAGE: u32 = 0x1000;
pub const DEFAULT_BASE: u32 = 0x1000;

/// One `__symbol_stub` entry: the assembled instructions plus the symbol it
/// jumps through.
#[derive(Debug, Clone)]
pub struct Stub {
    pub symbol: String,
    pub bytes: Vec<u8>,
}

/// Declarative description of the executable to synthesise.
#[derive(Debug, Clone)]
pub struct Program {
    /// Machine code placed at the start of `__TEXT,__text` (the entry point).
    pub code: Vec<u8>,
    /// Set the Thumb bit in the entry state (`LC_MAIN` entry is entered in ARM
    /// mode; real iOS executables start in ARM mode at a `b`/`bl` stub, but
    /// tests can also start directly in Thumb).
    pub thumb_entry: bool,
    /// Bytes placed in `__TEXT,__cstring`.
    pub cstrings: Vec<u8>,
    /// `__TEXT,__symbol_stub` entries (each `symbol` also gets a lazy pointer).
    pub stubs: Vec<Stub>,
    /// Bytes placed in `__DATA,__data`.
    pub data: Vec<u8>,
    /// Offsets inside `data` that hold slid pointers (rebase stream).
    pub data_rebases: Vec<u32>,
    /// Symbols bound into `__DATA,__nl_symbol_ptr` (regular bind stream).
    pub nonlazy: Vec<String>,
    /// Symbols bound into `__DATA,__la_symbol_ptr` (lazy bind stream).
    pub lazy: Vec<String>,
    /// Defined/exported symbols: `(name, address relative to __TEXT)`.
    pub defines: Vec<(String, u32)>,
    /// Installed dylibs.
    pub dylibs: Vec<String>,
    /// Emit `LC_UNIXTHREAD` instead of `LC_MAIN`.
    pub unixthread: bool,
    pub stack_size: u64,
    pub uuid: Option<[u8; 16]>,
    pub base: u32,
}

impl Default for Program {
    fn default() -> Self {
        Program {
            code: Vec::new(),
            thumb_entry: false,
            cstrings: Vec::new(),
            stubs: Vec::new(),
            data: Vec::new(),
            data_rebases: Vec::new(),
            nonlazy: Vec::new(),
            lazy: Vec::new(),
            defines: Vec::new(),
            dylibs: vec!["/usr/lib/libSystem.B.dylib".to_string()],
            unixthread: false,
            stack_size: 0x0010_0000,
            uuid: None,
            base: DEFAULT_BASE,
        }
    }
}

/// Addresses assigned by [`build`], so tests can poke at the guest image.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub base: u32,
    pub code_vmaddr: u32,
    pub cstring_vmaddr: u32,
    pub stubs_vmaddr: u32,
    pub stub_vmaddr: Vec<u32>,
    pub data_vmaddr: u32,
    pub data_blob_vmaddr: u32,
    pub nl_ptr_vmaddr: Vec<u32>,
    pub la_ptr_vmaddr: Vec<u32>,
    pub bss_vmaddr: u32,
    pub bss_size: u32,
    pub linkedit_vmaddr: u32,
    pub entry_vmaddr: u32,
    pub file_len: usize,
    pub defined: HashMap<String, u32>,
    pub indirect_base: u32,
}

impl Layout {
    pub fn stub_for(&self, symbol: &str, program: &Program) -> Option<u32> {
        program
            .stubs
            .iter()
            .position(|s| s.symbol == symbol)
            .map(|i| self.stub_vmaddr[i])
    }

    pub fn lazy_slot_for(&self, symbol: &str, program: &Program) -> Option<u32> {
        program
            .lazy
            .iter()
            .position(|s| s == symbol)
            .map(|i| self.la_ptr_vmaddr[i])
    }
}

fn align_up(value: u32, align: u32) -> u32 {
    (value + align - 1) & !(align - 1)
}

fn pad_to(v: &mut Vec<u8>, len: usize) {
    if v.len() < len {
        v.resize(len, 0);
    }
}

/// Emit a ULEB128 value (used by both the dyld streams and the export trie).
pub fn uleb(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

pub fn sleb(mut v: i64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        out.push(if done { byte } else { byte | 0x80 });
        if done {
            break;
        }
    }
}

pub fn cstr(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

fn name16(s: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let bytes = s.as_bytes();
    let n = bytes.len().min(16);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// Build the executable image.
pub fn build(program: &Program) -> (Vec<u8>, Layout) {
    // ------------------------------------------------------------------
    // 1. Fixed section payload sizes
    // ------------------------------------------------------------------
    let stub_size: u32 = program
        .stubs
        .iter()
        .map(|s| s.bytes.len() as u32)
        .max()
        .unwrap_or(0);
    let stubs_bytes = stub_size * program.stubs.len() as u32;
    let code_len = program.code.len() as u32;
    let cstring_len = program.cstrings.len() as u32;

    let code_file = PAGE; // first page is the header + load commands
    let cstring_file = code_file + code_len;
    let stubs_file = align_up(cstring_file + cstring_len, 4);

    let text_filesize = align_up(stubs_file + stubs_bytes, 4);
    let text_vmsize = align_up(text_filesize.max(PAGE), PAGE);

    // Data segment
    let data_file = align_up(text_filesize, PAGE);
    let data_vmaddr = program.base + data_file; // keep offset == vmaddr - base
    let nl_count = program.nonlazy.len() as u32;
    let la_count = program.lazy.len() as u32;
    let nl_bytes = nl_count * 4;
    let la_bytes = la_count * 4;
    let blob_off = nl_bytes + la_bytes;
    let blob_len = program.data.len() as u32;
    let data_filesize = align_up(blob_off + blob_len, 4);
    let bss_size: u32 = 0x40; // a small bss section exercises zero-fill
    // `ld` rounds each segment's virtual size up to a page and starts the next
    // segment on the next page boundary: a well-formed image never overlaps
    // once the kernel maps `round_page(vmsize)` bytes.
    let data_vmsize = align_up(data_filesize + bss_size, PAGE);

    let linkedit_file = align_up(data_file + data_filesize, 16);
    let linkedit_vmaddr = data_vmaddr + data_vmsize;

    // ------------------------------------------------------------------
    // 2. Addresses
    // ------------------------------------------------------------------
    let mut layout = Layout {
        base: program.base,
        code_vmaddr: program.base + code_file,
        cstring_vmaddr: program.base + cstring_file,
        stubs_vmaddr: program.base + stubs_file,
        stub_vmaddr: (0..program.stubs.len() as u32)
            .map(|i| program.base + stubs_file + i * stub_size)
            .collect(),
        data_vmaddr,
        data_blob_vmaddr: data_vmaddr + blob_off,
        nl_ptr_vmaddr: (0..nl_count).map(|i| data_vmaddr + i * 4).collect(),
        la_ptr_vmaddr: (0..la_count).map(|i| data_vmaddr + nl_bytes + i * 4).collect(),
        bss_vmaddr: data_vmaddr + data_filesize,
        bss_size,
        linkedit_vmaddr,
        entry_vmaddr: program.base + code_file,
        file_len: 0,
        defined: HashMap::new(),
        indirect_base: 0,
    };

    // ------------------------------------------------------------------
    // 3. Symbol table contents
    // ------------------------------------------------------------------
    // Defined (external) symbols first, then undefined ones, as `dysymtab`
    // requires (`iextdefsym` < `iundefsym`).
    let mut defined: Vec<(String, u32)> = program
        .defines
        .iter()
        .map(|(name, addr)| (name.clone(), program.base + addr))
        .collect();
    for (i, stub) in program.stubs.iter().enumerate() {
        defined.push((format!("{}_stub", stub.symbol), layout.stub_vmaddr[i]));
    }
    defined.sort();
    defined.dedup();

    let mut undef: Vec<String> = Vec::new();
    for sym in program.nonlazy.iter().chain(program.lazy.iter()) {
        if !undef.contains(sym) {
            undef.push(sym.clone());
        }
    }

    let mut symbol_names: Vec<(String, u32, u8, u8, u16)> = Vec::new(); // name, value, type, sect, desc
    for (name, addr) in &defined {
        layout.defined.insert(name.clone(), *addr);
        symbol_names.push((name.clone(), *addr, 0x0f, 1, 0)); // N_SECT | N_EXT
    }
    for name in &undef {
        symbol_names.push((name.clone(), 0, 0x01, 0, 0)); // N_UNDF | N_EXT
    }

    // indirect symbol table: one entry per pointer slot, plus stub entries.
    let mut indirect: Vec<u32> = Vec::new();
    let undef_index = |name: &str| -> u32 {
        defined.len() as u32 + undef.iter().position(|u| u == name).unwrap_or(0) as u32
    };
    for name in &program.nonlazy {
        indirect.push(undef_index(name));
    }
    for name in &program.lazy {
        indirect.push(undef_index(name));
    }
    let stub_indirect_start = indirect.len() as u32;
    for stub in &program.stubs {
        indirect.push(undef_index(&stub.symbol));
    }
    layout.indirect_base = stub_indirect_start;

    // ------------------------------------------------------------------
    // 4. dyld opcode streams (need the addresses computed above)
    // ------------------------------------------------------------------
    let mut rebase_stream = Vec::new();
    if !program.data_rebases.is_empty() {
        // SET_TYPE_IMM(POINTER); each location then sets both the segment and
        // its offset explicitly.
        rebase_stream.push(0x10 | 1);
        let mut offsets = program.data_rebases.clone();
        offsets.sort_unstable();
        for off in &offsets {
            // SET_SEGMENT_AND_OFFSET_ULEB(1, off + blob_off) then DO_REBASE_IMM_TIMES(1):
            // DO_REBASE advances the offset by the pointer size, so setting it
            // explicitly keeps the stream unambiguous.
            rebase_stream.push(0x20 | 1);
            uleb(*off as u64 + blob_off as u64, &mut rebase_stream);
            rebase_stream.push(0x50 | 1);
        }
    }
    rebase_stream.push(0x00); // DONE

    let mut bind_stream = Vec::new();
    if !program.nonlazy.is_empty() {
        bind_stream.push(0x10 | 1); // SET_DYLIB_ORDINAL_IMM(1)
        bind_stream.push(0x50 | 1); // SET_TYPE_IMM(POINTER)
        bind_stream.push(0x70 | 1); // SET_SEGMENT_AND_OFFSET_ULEB(data)
        uleb(0, &mut bind_stream); // offset 0 == first __nl_symbol_ptr slot
        for name in &program.nonlazy {
            bind_stream.push(0x40); // SET_SYMBOL_TRAILING_FLAGS_IMM(0)
            cstr(name, &mut bind_stream);
            bind_stream.push(0x90); // DO_BIND
        }
    }
    bind_stream.push(0x00);

    let mut lazy_stream = Vec::new();
    for (i, name) in program.lazy.iter().enumerate() {
        lazy_stream.push(0x10 | 1); // SET_DYLIB_ORDINAL_IMM(1)
        lazy_stream.push(0x50 | 1); // SET_TYPE_IMM(POINTER)
        lazy_stream.push(0x70 | 1); // SET_SEGMENT_AND_OFFSET_ULEB(data)
        uleb(nl_bytes as u64 + i as u64 * 4, &mut lazy_stream); // this entry's slot
        lazy_stream.push(0x40);
        cstr(name, &mut lazy_stream);
        lazy_stream.push(0x90); // DO_BIND
        lazy_stream.push(0x00); // DONE terminates this lazy entry
    }

    // The export trie stores addresses relative to the image base (`__TEXT`),
    // and it exports the generated stub symbols as well as `Program::defines`.
    let export_defs: Vec<(String, u32)> = defined.iter().map(|(name, addr)| (name.clone(), addr - program.base)).collect();
    let export_trie = build_export_trie(&export_defs, program.base);

    // ------------------------------------------------------------------
    // 5. String table + symbol table layout inside __LINKEDIT
    // ------------------------------------------------------------------
    let indirect_bytes = indirect.len() as u32 * 4;
    let symtab_off = indirect_bytes;
    let nsyms = symbol_names.len() as u32;
    let stroff = symtab_off + nsyms * 12;
    let mut strtab: Vec<u8> = Vec::new();
    strtab.push(0); // index 0 is the empty string
    let mut name_offsets: Vec<u32> = Vec::new();
    for (name, ..) in &symbol_names {
        name_offsets.push(strtab.len() as u32);
        cstr(name, &mut strtab);
    }
    let strsize = strtab.len() as u32;

    let rebase_off = stroff + strsize;
    let bind_off = rebase_off + rebase_stream.len() as u32;
    let weak_off = bind_off; // no weak binds
    let lazy_off = bind_off + bind_stream.len() as u32;
    let export_off = lazy_off + lazy_stream.len() as u32;
    let linkedit_size = export_off + export_trie.len() as u32;

    // ------------------------------------------------------------------
    // 6. Load commands
    // ------------------------------------------------------------------
    let dylibs: Vec<(String, bool)> = program
        .dylibs
        .iter()
        .map(|d| (d.clone(), d.contains("libSystem")))
        .collect();

    let seg_cmd_size = |nsects: u32| 56 + 68 * nsects;
    let text_nsects = 3u32; // __text, __cstring, __symbol_stub
    let data_nsects = 3u32; // __nl_symbol_ptr, __la_symbol_ptr, __data (+ __bss folded below)
    let data_nsects = if bss_size > 0 { data_nsects + 1 } else { data_nsects };

    let mut cmds: Vec<u8> = Vec::new();
    // 3 segments (__TEXT, __DATA, __LINKEDIT) + symtab + dysymtab + dyld_info +
    // one entry command, plus one LC_LOAD_DYLIB per library and an optional UUID.
    let ncmds_extra = 7 + dylibs.len() as u32 + if program.uuid.is_some() { 1 } else { 0 };

    // 6a. compute command bytes (sizes are needed for sizeofcmds first)
    let text_cmd = seg_cmd_size(text_nsects);
    let data_cmd = seg_cmd_size(data_nsects);
    let link_cmd = 56u32;
    let symtab_cmd = 24u32;
    let dysymtab_cmd = 80u32;
    let dyld_cmd = 48u32;
    let main_cmd = 24u32;
    let uuid_cmd = 24u32;
    // cmd+cmdsize (8) + flavor/count (8) + r0-r12/sp/lr/pc/cpsr (17 words) + pad (4)
    let thread_cmd = 8 + 8 + 17 * 4 + 4;
    let dylib_sizes: Vec<u32> = dylibs.iter().map(|(name, _)| align_up(24 + name.len() as u32 + 1, 4)).collect();

    let mut total = text_cmd + data_cmd + link_cmd + symtab_cmd + dysymtab_cmd + dyld_cmd + dylib_sizes.iter().sum::<u32>();
    total += if program.uuid.is_some() { uuid_cmd } else { 0 };
    total += if program.unixthread { thread_cmd } else { main_cmd };
    let ncmds = 7 + dylibs.len() as u32 + if program.uuid.is_some() { 1 } else { 0 };
    debug_assert_eq!(ncmds, ncmds_extra);

    // 6b. header
    let mut header = Vec::new();
    header.extend_from_slice(&MH_MAGIC.to_le_bytes());
    header.extend_from_slice(&CPU_TYPE_ARM.to_le_bytes());
    header.extend_from_slice(&CPU_SUBTYPE_ARM_V7.to_le_bytes());
    header.extend_from_slice(&MH_EXECUTE.to_le_bytes());
    header.extend_from_slice(&ncmds.to_le_bytes());
    header.extend_from_slice(&total.to_le_bytes());
    header.extend_from_slice(&(MH_NOUNDEFS | MH_DYLDLINK | MH_TWOLEVEL | MH_PIE).to_le_bytes());
    cmds.extend_from_slice(&header);

    let push_segment = |cmds: &mut Vec<u8>, name: &str, vmaddr: u32, vmsize: u32, fileoff: u32, filesize: u32, initprot: u32, sections: &[SectionSpec]| {
        cmds.extend_from_slice(&LC_SEGMENT.to_le_bytes());
        cmds.extend_from_slice(&seg_cmd_size(sections.len() as u32).to_le_bytes());
        cmds.extend_from_slice(&name16(name));
        cmds.extend_from_slice(&vmaddr.to_le_bytes());
        cmds.extend_from_slice(&vmsize.to_le_bytes());
        cmds.extend_from_slice(&fileoff.to_le_bytes());
        cmds.extend_from_slice(&filesize.to_le_bytes());
        cmds.extend_from_slice(&VM_PROT_ALL.to_le_bytes()); // maxprot
        cmds.extend_from_slice(&initprot.to_le_bytes());
        cmds.extend_from_slice(&(sections.len() as u32).to_le_bytes());
        cmds.extend_from_slice(&0u32.to_le_bytes()); // flags
        for s in sections {
            cmds.extend_from_slice(&name16(&s.name));
            cmds.extend_from_slice(&name16(name));
            cmds.extend_from_slice(&s.addr.to_le_bytes());
            cmds.extend_from_slice(&s.size.to_le_bytes());
            cmds.extend_from_slice(&s.offset.to_le_bytes());
            cmds.extend_from_slice(&s.align.to_le_bytes());
            cmds.extend_from_slice(&0u32.to_le_bytes()); // reloff
            cmds.extend_from_slice(&0u32.to_le_bytes()); // nreloc
            cmds.extend_from_slice(&s.flags.to_le_bytes());
            cmds.extend_from_slice(&s.reserved1.to_le_bytes());
            cmds.extend_from_slice(&s.reserved2.to_le_bytes());
        }
    };

    let text_sections = vec![
        SectionSpec::new("__text", layout.code_vmaddr, code_len, code_file, 2, S_REGULAR | S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS, 0, 0),
        SectionSpec::new("__cstring", layout.cstring_vmaddr, cstring_len, cstring_file, 0, S_CSTRING_LITERALS, 0, 0),
        SectionSpec::new("__symbol_stub", layout.stubs_vmaddr, stubs_bytes, stubs_file, 2, S_SYMBOL_STUBS | S_ATTR_PURE_INSTRUCTIONS | S_ATTR_SOME_INSTRUCTIONS, stub_indirect_start, stub_size),
    ];
    let mut data_sections = vec![
        SectionSpec::new("__nl_symbol_ptr", data_vmaddr, nl_bytes, data_file, 2, S_NON_LAZY_SYMBOL_POINTERS, 0, 0),
        SectionSpec::new("__la_symbol_ptr", data_vmaddr + nl_bytes, la_bytes, data_file + nl_bytes, 2, S_LAZY_SYMBOL_POINTERS, nl_count, 0),
        SectionSpec::new("__data", layout.data_blob_vmaddr, blob_len, data_file + blob_off, 2, S_REGULAR, 0, 0),
    ];
    if bss_size > 0 {
        data_sections.push(SectionSpec::new("__bss", layout.bss_vmaddr, bss_size, data_file + data_filesize, 3, S_ZEROFILL, 0, 0));
    }

    push_segment(&mut cmds, "__TEXT", program.base, text_vmsize, 0, text_filesize, VM_PROT_READ | VM_PROT_EXECUTE, &text_sections);
    push_segment(&mut cmds, "__DATA", data_vmaddr, data_vmsize, data_file, data_filesize, VM_PROT_READ | VM_PROT_WRITE, &data_sections);
    push_segment(&mut cmds, "__LINKEDIT", linkedit_vmaddr, align_up(linkedit_size, PAGE), linkedit_file, linkedit_size, VM_PROT_READ, &[]);

    // LC_SYMTAB
    cmds.extend_from_slice(&LC_SYMTAB.to_le_bytes());
    cmds.extend_from_slice(&symtab_cmd.to_le_bytes());
    cmds.extend_from_slice(&(linkedit_file + symtab_off).to_le_bytes());
    cmds.extend_from_slice(&nsyms.to_le_bytes());
    cmds.extend_from_slice(&(linkedit_file + stroff).to_le_bytes());
    cmds.extend_from_slice(&strsize.to_le_bytes());

    // LC_DYSYMTAB
    let extdef_count = defined.len() as u32;
    let undef_count = undef.len() as u32;
    cmds.extend_from_slice(&LC_DYSYMTAB.to_le_bytes());
    cmds.extend_from_slice(&dysymtab_cmd.to_le_bytes());
    for field in [
        0u32,                 // ilocalsym
        0,                    // nlocalsym
        0,                    // iextdefsym
        extdef_count,         // nextdefsym
        extdef_count,         // iundefsym
        undef_count,          // nundefsym
        0, 0, 0, 0, 0, 0,     // toc / modtab / extref
        linkedit_file + 0,    // indirectsymoff
        indirect.len() as u32,// nindirectsyms
        0, 0, 0, 0,           // extrel / locrel
    ] {
        cmds.extend_from_slice(&field.to_le_bytes());
    }

    // LC_DYLD_INFO_ONLY
    cmds.extend_from_slice(&LC_DYLD_INFO_ONLY.to_le_bytes());
    cmds.extend_from_slice(&dyld_cmd.to_le_bytes());
    for field in [
        linkedit_file + rebase_off, rebase_stream.len() as u32,
        linkedit_file + bind_off, bind_stream.len() as u32,
        linkedit_file + weak_off, 0u32,
        linkedit_file + lazy_off, lazy_stream.len() as u32,
        linkedit_file + export_off, export_trie.len() as u32,
    ] {
        cmds.extend_from_slice(&field.to_le_bytes());
    }

    // LC_LOAD_DYLIB entries
    for (name, _) in &dylibs {
        let size = align_up(24 + name.len() as u32 + 1, 4);
        cmds.extend_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
        cmds.extend_from_slice(&size.to_le_bytes());
        cmds.extend_from_slice(&24u32.to_le_bytes()); // name offset
        cmds.extend_from_slice(&2u32.to_le_bytes()); // timestamp
        cmds.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // current version
        cmds.extend_from_slice(&0x0001_0000u32.to_le_bytes()); // compatibility
        cmds.extend_from_slice(name.as_bytes());
        cmds.push(0);
        while cmds.len() % 4 != 0 {
            cmds.push(0);
        }
    }

    if let Some(uuid) = program.uuid {
        cmds.extend_from_slice(&LC_UUID.to_le_bytes());
        cmds.extend_from_slice(&uuid_cmd.to_le_bytes());
        cmds.extend_from_slice(&uuid);
    }

    if program.unixthread {
        cmds.extend_from_slice(&LC_UNIXTHREAD.to_le_bytes());
        cmds.extend_from_slice(&thread_cmd.to_le_bytes());
        cmds.extend_from_slice(&ARM_THREAD_STATE.to_le_bytes());
        cmds.extend_from_slice(&ARM_THREAD_STATE_COUNT.to_le_bytes());
        for _ in 0..13 {
            cmds.extend_from_slice(&0u32.to_le_bytes());
        }
        cmds.extend_from_slice(&0x0020_0000u32.to_le_bytes()); // sp (patched by loader)
        cmds.extend_from_slice(&0u32.to_le_bytes()); // lr
        let pc = if program.thumb_entry { layout.entry_vmaddr | 1 } else { layout.entry_vmaddr };
        cmds.extend_from_slice(&pc.to_le_bytes());
        cmds.extend_from_slice(&0x10u32.to_le_bytes()); // cpsr: user mode
        cmds.extend_from_slice(&0u32.to_le_bytes()); // padding
    } else {
        cmds.extend_from_slice(&LC_MAIN.to_le_bytes());
        cmds.extend_from_slice(&main_cmd.to_le_bytes());
        let entryoff = (layout.entry_vmaddr - program.base) as u64;
        cmds.extend_from_slice(&entryoff.to_le_bytes());
        cmds.extend_from_slice(&program.stack_size.to_le_bytes());
    }

    debug_assert_eq!(cmds.len() as u32, 28 + total, "command stream size mismatch");

    // ------------------------------------------------------------------
    // 7. Assemble the file
    // ------------------------------------------------------------------
    let mut file = cmds;
    pad_to(&mut file, code_file as usize);
    file.extend_from_slice(&program.code);
    pad_to(&mut file, cstring_file as usize);
    file.extend_from_slice(&program.cstrings);
    pad_to(&mut file, stubs_file as usize);
    for stub in &program.stubs {
        let start = file.len();
        file.extend_from_slice(&stub.bytes);
        pad_to(&mut file, start + stub_size as usize);
    }
    // Real images pad the end of __TEXT out to the next segment's page-aligned
    // file offset, which is what `data_file` is.
    pad_to(&mut file, data_file as usize);

    // __DATA: non-lazy pointers are pre-filled with 0 (bound at load), lazy
    // pointers point at their stub, exactly like dyld pre-fills them.
    let data_start = file.len();
    debug_assert_eq!(data_start as u32, data_file);
    for _ in 0..nl_count {
        file.extend_from_slice(&0u32.to_le_bytes());
    }
    for name in &program.lazy {
        // dyld pre-fills lazy pointers with the address of the matching stub.
        let addr = program
            .stubs
            .iter()
            .position(|s| &s.symbol == name)
            .and_then(|i| layout.stub_vmaddr.get(i).copied())
            .unwrap_or(0);
        file.extend_from_slice(&addr.to_le_bytes());
    }
    while file.len() < data_start + blob_off as usize {
        file.push(0);
    }
    file.extend_from_slice(&program.data);
    pad_to(&mut file, data_start + data_filesize as usize);

    // __LINKEDIT
    pad_to(&mut file, linkedit_file as usize);
    for idx in &indirect {
        file.extend_from_slice(&idx.to_le_bytes());
    }
    for (i, (name, value, ntype, nsect, ndesc)) in symbol_names.iter().enumerate() {
        file.extend_from_slice(&name_offsets[i].to_le_bytes());
        file.push(*ntype);
        file.push(*nsect);
        file.extend_from_slice(&ndesc.to_le_bytes());
        file.extend_from_slice(&value.to_le_bytes());
        let _ = name;
    }
    let file_strtab_start = file.len();
    file.extend_from_slice(&strtab);
    debug_assert_eq!(file.len() as u32, linkedit_file + stroff + strsize);
    debug_assert_eq!(file_strtab_start as u32, linkedit_file + stroff);
    file.extend_from_slice(&rebase_stream);
    file.extend_from_slice(&bind_stream);
    file.extend_from_slice(&lazy_stream);
    file.extend_from_slice(&export_trie);
    layout.file_len = file.len();

    (file, layout)
}

struct SectionSpec {
    name: String,
    addr: u32,
    size: u32,
    offset: u32,
    align: u32,
    flags: u32,
    reserved1: u32,
    reserved2: u32,
}

impl SectionSpec {
    #[allow(clippy::too_many_arguments)]
    fn new(
        name: &str,
        addr: u32,
        size: u32,
        offset: u32,
        align: u32,
        flags: u32,
        reserved1: u32,
        reserved2: u32,
    ) -> Self {
        SectionSpec {
            name: name.to_string(),
            addr,
            size,
            offset,
            align,
            flags,
            reserved1,
            reserved2,
        }
    }
}

/// Build a valid export trie: the root has one child per symbol, whose edge is
/// the whole name and whose node carries the terminal payload.
fn build_export_trie(defines: &[(String, u32)], _base: u32) -> Vec<u8> {
    if defines.is_empty() {
        // A trie with a single empty node.
        return vec![0, 0];
    }

    // Node 0 is the root; every other node is a terminal leaf.
    let mut nodes: Vec<Vec<u8>> = Vec::new();
    let mut root: Vec<u8> = Vec::new();

    // Terminal payload length is encoded first, so build each leaf body first.
    let mut leaves: Vec<Vec<u8>> = Vec::new();
    for (_, addr) in defines {
        let mut body = Vec::new();
        uleb(0, &mut body); // flags: regular export
        uleb(*addr as u64, &mut body); // address, relative to __TEXT
        let mut node = Vec::new();
        uleb(body.len() as u64, &mut node); // terminal_size
        node.extend_from_slice(&body);
        uleb(0, &mut node); // children count
        leaves.push(node);
    }

    // Compute the offset of each child node (offsets are relative to the trie).
    let mut cursor = 0usize;
    // Root size depends on its children table, which we now know.
    let mut root_size = 1 /* first uleb(0) */ + 1 /* child count */;
    for (name, _) in defines {
        root_size += name.len() + 1 + uleb_len(leaves[cursor].len() as u64);
        cursor += 1;
    }
    let root_size = root_size;
    let mut child_offsets = Vec::new();
    let mut off = root_size;
    for leaf in &leaves {
        child_offsets.push(off);
        off += leaf.len();
    }

    // Root
    uleb(0, &mut root); // terminal_size = 0
    root.push(defines.len() as u8);
    for (i, (name, _)) in defines.iter().enumerate() {
        root.extend_from_slice(name.as_bytes());
        root.push(0);
        uleb(child_offsets[i] as u64, &mut root);
    }
    debug_assert_eq!(root.len(), root_size);
    nodes.push(root);
    for leaf in leaves {
        nodes.push(leaf);
    }
    nodes.concat()
}

fn uleb_len(mut v: u64) -> usize {
    let mut n = 1;
    v >>= 7;
    while v != 0 {
        n += 1;
        v >>= 7;
    }
    n
}
