//! Builds a small but complete ARMv7 Mach-O executable and writes it to disk.
//!
//! It exists so the pipeline the game needs can be exercised without the game:
//! a real Mach-O header with the load commands dyld looks at, a `__TEXT.__text`
//! segment, a `__cstring` message, a lazy symbol stub for `_puts` that goes
//! through the dyld tables into the HLE trampoline page, and a Darwin syscall
//! (`svc #0x80`) to write to stdout and exit.
//!
//! ```sh
//! cargo run --example make_demo -- /tmp/demo-armv7
//! cargo run --release -- run /tmp/demo-armv7 --trace --stats
//! ```

use macho::test_support::{build, Program, Stub};

/// `mov rd, #imm8`.
fn mov_imm(rd: u32, imm: u32) -> u32 {
    0xe3a0_0000 | (rd << 12) | imm
}

/// `ldr rd, [pc, #imm]` loading the absolute address `target`.
fn ldr_pc_at(rd: u32, insn_addr: u32, target: u32) -> u32 {
    let imm = target as i64 - (insn_addr as i64 + 8);
    assert!((0..=0xfff).contains(&imm) && imm % 4 == 0, "bad pc-relative load: {imm}");
    0xe59f_0000 | (rd << 12) | imm as u32
}

/// `svc #imm`.
fn svc(imm: u32) -> u32 {
    0xef00_0000 | (imm & 0x00ff_ffff)
}

/// `bl target`.
fn bl(pc: u32, target: u32) -> u32 {
    let offset = (target as i64 - (pc as i64 + 8)) >> 2;
    0xeb00_0000 | ((offset as u32) & 0x00ff_ffff)
}

fn arm(words: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(words.len() * 4);
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn main() -> Result<(), String> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: make_demo <output-path>");
        std::process::exit(2);
    });

    let message = b"hello from the guest\n\0";
    let mut program = Program {
        cstrings: message.to_vec(),
        stubs: vec![Stub {
            symbol: "_puts".to_string(),
            bytes: Vec::new(),
        }],
        lazy: vec!["_puts".to_string()],
        defines: vec![("_main".to_string(), 0)],
        ..Program::default()
    };

    // First pass: lay everything out with placeholder code so the addresses are
    // final (the layout depends only on the section sizes).
    let mut probe = program.clone();
    probe.code = vec![0u8; 11 * 4];
    let (_, layout) = build(&probe);

    let code = layout.code_vmaddr;
    let stub = layout.stub_for("_puts", &program).expect("stub exists");
    let slot = layout.la_ptr_vmaddr[0];
    // The literal pool sits after the instructions; `ldr_pc_at` loads from
    // there, so it wants the pool's address and the pool holds the message.
    let pool = code + 10 * 4;
    program.code = arm(&[
        ldr_pc_at(0, code, pool), // r0 = message
        bl(code + 4, stub),       // bl _puts (through dyld -> HLE)
        // write(1, message, len) using the Darwin syscall ABI: the call number
        // goes in r12 and `svc #0x80` traps to the host.
        ldr_pc_at(0, code + 8, pool),
        mov_imm(1, message.len() as u32),
        mov_imm(2, 1),
        mov_imm(12, 4), // SYS_write
        svc(0x80),
        mov_imm(0, 0),
        mov_imm(12, 1), // SYS_exit
        svc(0x80),
        layout.cstring_vmaddr, // literal pool
    ]);
    program.stubs[0].bytes = arm(&[
        0xe59f_c000, // ldr r12, [pc]      ; the lazy pointer slot
        0xe59c_f000, // ldr pc, [r12]      ; jump to the trampoline
        slot,        // .word __la_symbol_ptr(_puts)
    ]);

    let (data, layout) = build(&program);
    std::fs::write(&path, &data).map_err(|e| format!("{path}: {e}"))?;
    println!("wrote {path} ({} bytes)", data.len());
    println!("  entry       {:#010x}", layout.entry_vmaddr);
    println!("  __text      {:#010x}", layout.code_vmaddr);
    println!("  _puts stub  {:#010x}", stub);
    println!("  message     {:#010x}", layout.cstring_vmaddr);
    Ok(())
}
