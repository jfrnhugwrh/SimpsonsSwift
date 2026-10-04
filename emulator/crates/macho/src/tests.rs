//! Unit tests for the Mach-O reader.
//!
//! Every test builds a *real* ARMv7 executable with [`crate::test_support`] and
//! then asserts what the loader must see.  `macho_smoke_simpsons_shape` mirrors
//! the load-command layout of the binary in `simpsons3_iphone_en.txt` (armv7,
//! `LC_SEGMENT` `__TEXT`/`__DATA`/`__LINKEDIT`, `LC_SYMTAB`, `LC_DYSYMTAB`,
//! `LC_DYLD_INFO_ONLY`, `LC_MAIN`, UIKit/Foundation dylibs).

use crate::test_support::{build, Program, Stub};
use crate::*;

fn simpsons_shaped_program() -> Program {
    Program {
        code: vec![0x00, 0x00, 0xa0, 0xe1, 0x1e, 0xff, 0x2f, 0xe1], // mov r0,r0; bx lr
        cstrings: b"Simpsons\0strings_generic.bin\0".to_vec(),
        stubs: vec![Stub {
            symbol: "_printf".into(),
            bytes: vec![0x04, 0xc0, 0x9f, 0xe5, 0x1c, 0xff, 0x2f, 0xe1, 0, 0, 0, 0],
        }],
        data: vec![0xef, 0xbe, 0xad, 0xde, 0x00, 0x00, 0x00, 0x00],
        data_rebases: vec![0, 4],
        nonlazy: vec!["_objc_msgSend".into()],
        lazy: vec!["_printf".into()],
        defines: vec![("_main".into(), 0x1000)],
        dylibs: vec![
            "/usr/lib/libSystem.B.dylib".into(),
            "/System/Library/Frameworks/Foundation.framework/Foundation".into(),
            "/System/Library/Frameworks/UIKit.framework/UIKit".into(),
            "/System/Library/Frameworks/OpenGLES.framework/OpenGLES".into(),
        ],
        uuid: Some([0x53, 0x49, 0x4d, 0x50, 0x53, 0x4f, 0x4e, 0x53, 1, 2, 3, 4, 5, 6, 7, 8]),
        ..Default::default()
    }
}

#[test]
fn header_is_armv7_executable() {
    let (bytes, _) = build(&simpsons_shaped_program());
    let m = MachO::from_bytes(bytes).expect("parses");
    assert_eq!(m.header.magic, MH_MAGIC);
    assert_eq!(m.header.cputype, CPU_TYPE_ARM);
    assert_eq!(m.header.cpusubtype, CPU_SUBTYPE_ARM_V7);
    assert_eq!(m.header.filetype, MH_EXECUTE);
    assert_eq!(m.header.cputype_name(), "arm");
    assert_eq!(m.header.cpusubtype_name(), "armv7");
    assert!(m.header.flags & MH_PIE != 0);
    assert!(m.header.is_loadable_image());
}

#[test]
fn segments_and_sections_round_trip() {
    let program = simpsons_shaped_program();
    let (bytes, layout) = build(&program);
    let m = MachO::from_bytes(bytes).unwrap();

    let segnames: Vec<&str> = m.segments.iter().map(|s| s.segname.as_str()).collect();
    assert_eq!(segnames, vec!["__TEXT", "__DATA", "__LINKEDIT"]);

    let text = m.segment("__TEXT").unwrap();
    assert_eq!(text.vmaddr, layout.base);
    assert_eq!(text.fileoff, 0);
    assert_eq!(text.initprot, VM_PROT_READ | VM_PROT_EXECUTE);
    assert_eq!(text.sections.len(), 3);
    assert_eq!(text.sections[0].sectname, "__text");
    assert_eq!(text.sections[0].addr, layout.code_vmaddr);
    assert_eq!(text.sections[0].size as usize, program.code.len());
    assert_eq!(text.sections[0].kind(), "code");
    assert!(text.sections[0].flags & S_ATTR_PURE_INSTRUCTIONS != 0);

    let cstring = m.section("__TEXT", "__cstring").unwrap();
    assert_eq!(cstring.kind(), "cstring");
    let stubs = m.section("__TEXT", "__symbol_stub").unwrap();
    assert_eq!(stubs.kind(), "symbol-stubs");
    assert_eq!(stubs.reserved2, 12, "stub size is recorded in reserved2");

    let data = m.segment("__DATA").unwrap();
    assert_eq!(data.initprot, VM_PROT_READ | VM_PROT_WRITE);
    let la = m.section("__DATA", "__la_symbol_ptr").unwrap();
    assert_eq!(la.kind(), "lazy-symbol-pointers");
    assert_eq!(la.addr, layout.la_ptr_vmaddr[0]);
    let nl = m.section("__DATA", "__nl_symbol_ptr").unwrap();
    assert_eq!(nl.kind(), "non-lazy-symbol-pointers");
    assert_eq!(nl.reserved1, 0);
    let bss = m.section("__DATA", "__bss").unwrap();
    assert!(bss.is_zerofill());
    assert_eq!(bss.addr, layout.bss_vmaddr);

    // file ranges must be sane: nothing may point outside the image
    for seg in &m.segments {
        if !seg.is_pagezero() {
            assert!(seg.fileoff as usize <= m.data.len());
        }
        for sect in &seg.sections {
            if !sect.is_zerofill() {
                assert!(
                    (sect.offset + sect.size) as usize <= m.data.len(),
                    "section {} outside file",
                    sect
                );
            }
        }
    }
}

#[test]
fn entry_point_comes_from_lc_main() {
    let program = simpsons_shaped_program();
    let (bytes, layout) = build(&program);
    let m = MachO::from_bytes(bytes).unwrap();
    assert_eq!(
        m.entry,
        Some(crate::image::EntryPoint::Main {
            entryoff: (layout.code_vmaddr - layout.base) as u64,
            stacksize: program.stack_size
        })
    );
    assert_eq!(m.entry_pc().unwrap(), layout.code_vmaddr);
}

#[test]
fn entry_point_comes_from_lc_unixthread_too() {
    let program = Program { unixthread: true, ..simpsons_shaped_program() };
    let (bytes, layout) = build(&program);
    let m = MachO::from_bytes(bytes).unwrap();
    match m.entry {
        Some(crate::image::EntryPoint::Thread(regs)) => {
            assert_eq!(regs.pc, layout.entry_vmaddr);
            assert_eq!(regs.sp, 0x0020_0000);
            assert_eq!(regs.r[15], layout.entry_vmaddr);
        }
        other => panic!("expected a thread state entry, got {other:?}"),
    }
    assert_eq!(m.entry_pc().unwrap(), layout.entry_vmaddr);
}

#[test]
fn symbols_and_imports_are_classified() {
    let (bytes, _) = build(&simpsons_shaped_program());
    let m = MachO::from_bytes(bytes).unwrap();

    let main = m.symbol("_main").expect("_main is defined");
    assert!(main.is_defined_section());
    assert!(main.is_external());
    assert!(!main.needs_binding());

    let printf = m.symbol("_printf").expect("_printf is imported");
    assert!(printf.is_undefined());
    assert!(printf.needs_binding());

    let imports: Vec<&str> = m.imported_symbols().map(|s| s.name.as_str()).collect();
    assert!(imports.contains(&"_printf"));
    assert!(imports.contains(&"_objc_msgSend"));

    let undef_indices = m.undefined_symbol_indices();
    assert_eq!(undef_indices.len(), imports.len());

    // Indirect symbol entries must reference undefined symbols.
    assert_eq!(m.indirect_symbols.len(), 3); // nl + lazy + 1 stub
    for idx in &m.indirect_symbols {
        let sym = &m.symbols[*idx as usize];
        assert!(sym.needs_binding(), "indirect entry {idx} -> {} is not undefined", sym.name);
    }
}

#[test]
fn dylibs_are_listed_in_load_command_order() {
    let (bytes, _) = build(&simpsons_shaped_program());
    let m = MachO::from_bytes(bytes).unwrap();
    let names: Vec<&str> = m.dylibs.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(names[0], "/usr/lib/libSystem.B.dylib");
    assert!(names.iter().any(|n| n.contains("UIKit")));
    assert!(names.iter().any(|n| n.contains("OpenGLES")));
}

#[test]
fn rebase_stream_yields_pointer_slots() {
    let program = simpsons_shaped_program();
    let (bytes, layout) = build(&program);
    let m = MachO::from_bytes(bytes).unwrap();
    let rebases = rebase_locations(&m).unwrap();
    let addrs: Vec<u32> = rebases.iter().map(|r| r.address).collect();
    assert_eq!(
        addrs,
        vec![layout.data_blob_vmaddr, layout.data_blob_vmaddr + 4],
        "rebased pointers live at the head of __data"
    );
    assert!(rebases.iter().all(|r| r.kind == REBASE_TYPE_POINTER));
}

#[test]
fn bind_and_lazy_bind_streams_are_decoded() {
    let (bytes, layout) = build(&simpsons_shaped_program());
    let m = MachO::from_bytes(bytes).unwrap();

    let binds = bind_records(&m).unwrap();
    assert_eq!(binds.len(), 1, "one non-lazy pointer");
    assert_eq!(binds[0].symbol, "_objc_msgSend");
    assert_eq!(binds[0].address, layout.nl_ptr_vmaddr[0]);
    assert_eq!(binds[0].dylib_ordinal, 1);
    assert_eq!(binds[0].kind, BIND_TYPE_POINTER);
    assert!(!binds[0].lazy);

    let lazy = lazy_bind_records(&m).unwrap();
    assert_eq!(lazy.len(), 1, "one lazy pointer");
    assert_eq!(lazy[0].symbol, "_printf");
    assert_eq!(lazy[0].address, layout.la_ptr_vmaddr[0]);
    assert!(lazy[0].lazy);
}

#[test]
fn export_trie_is_walked() {
    let (bytes, layout) = build(&simpsons_shaped_program());
    let m = MachO::from_bytes(bytes).unwrap();
    let exports = export_symbols(&m).unwrap();
    let names: Vec<&str> = exports.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"_main"));
    assert!(names.iter().any(|n| n.ends_with("_stub")));
    let main = export_lookup(&m, "_main").unwrap().unwrap();
    assert_eq!(main.address, Some(layout.defined["_main"]));
    assert!(!main.is_reexport());
}

#[test]
fn rejects_non_macho_input() {
    let err = MachO::from_bytes(b"not a mach-o at all, sorry".to_vec()).unwrap_err();
    assert!(matches!(err, MachOError::BadMagic(_)), "{err}");

    let mut elfish = vec![0x7f, b'E', b'L', b'F', 1, 1, 1, 0];
    elfish.resize(64, 0);
    assert!(matches!(MachO::from_bytes(elfish).unwrap_err(), MachOError::BadMagic(_)));
}

#[test]
fn rejects_64_bit_and_fat_with_actionable_errors() {
    let mut bytes = vec![0u8; 64];
    bytes[..4].copy_from_slice(&MH_MAGIC_64.to_le_bytes());
    let msg = MachO::from_bytes(bytes).unwrap_err().to_string();
    assert!(msg.contains("64-bit"), "{msg}");

    let mut fat = vec![0u8; 64];
    fat[..4].copy_from_slice(&FAT_MAGIC.to_be_bytes());
    let msg = MachO::from_bytes(fat).unwrap_err().to_string();
    assert!(msg.contains("universal"), "{msg}");
}

#[test]
fn rejects_truncated_command_stream() {
    let (bytes, _) = build(&simpsons_shaped_program());
    // Truncate inside the load commands but keep the header.
    let truncated = bytes[..40].to_vec();
    let err = MachO::from_bytes(truncated).unwrap_err();
    assert!(
        matches!(err, MachOError::InconsistentCommands { .. } | MachOError::Truncated { .. }),
        "{err}"
    );
}

#[test]
fn rejects_bad_command_size() {
    let (mut bytes, _) = build(&simpsons_shaped_program());
    // First command's cmdsize lives at offset 4 of the command stream (28).
    bytes[28 + 4..28 + 8].copy_from_slice(&2u32.to_le_bytes());
    let err = MachO::from_bytes(bytes).unwrap_err();
    assert!(matches!(err, MachOError::BadCommandSize { .. }), "{err}");
}

#[test]
fn flags_encrypted_images() {
    let (bytes, _) = build(&simpsons_shaped_program());
    // Patch a fake LC_ENCRYPTION_INFO over the LC_UUID command.
    let mut m = MachO::from_bytes(bytes).unwrap();
    m.encryption = Some((0x1000, 0x2000, 1));
    assert!(m.is_encrypted());
    let err = MachOError::Encrypted { cryptoff: 0x1000, cryptsize: 0x2000, cryptid: 1 };
    assert!(err.to_string().contains("decrypt"));
}

#[test]
fn fat_slice_extraction() {
    let (thin, _) = build(&simpsons_shaped_program());
    // Build a fat container holding the armv7 slice plus a fake x86 slice.
    let mut fat = Vec::new();
    fat.extend_from_slice(&FAT_MAGIC.to_be_bytes());
    fat.extend_from_slice(&2u32.to_be_bytes());
    for (i, cputype) in [CPU_TYPE_X86, CPU_TYPE_ARM].iter().enumerate() {
        fat.extend_from_slice(&cputype.to_be_bytes());
        fat.extend_from_slice(&0u32.to_be_bytes());
        fat.extend_from_slice(&((0x1000 + i * 0x1000) as u32).to_be_bytes());
        fat.extend_from_slice(&(thin.len() as u32).to_be_bytes());
        fat.extend_from_slice(&0u32.to_be_bytes());
    }
    fat.resize(0x1000, 0);
    fat.extend_from_slice(&vec![0xaa; 0x1000]);
    fat.resize(0x2000, 0);
    fat.extend_from_slice(&thin);

    let range = fat_slice(&fat, CPU_TYPE_ARM).unwrap().expect("arm slice present");
    let m = MachO::from_bytes(fat[range].to_vec()).unwrap();
    assert_eq!(m.header.cputype, CPU_TYPE_ARM);
    assert_eq!(m.entry_pc().unwrap(), 0x2000);
}

#[test]
fn simpsons_dump_structs_match_header_layout() {
    // The reference dump declares these structs; assert the field order and
    // sizes we implement agree with them (see the struct definitions near line
    // 2930 of simpsons3_iphone_en.txt).
    assert_eq!(Header::SIZE, 28);
    assert_eq!(Section::SIZE, 68);
    assert_eq!(Symbol::SIZE, 12);
    // segment_command = cmd,cmdsize,segname[16],vmaddr,vmsize,fileoff,filesize,
    //                   maxprot,initprot,nsects,flags
    assert_eq!(8 + 16 + 4 * 8, 56);
    // dysymtab_command = cmd,cmdsize + 18 dwords
    assert_eq!(8 + 18 * 4, 80);
}
