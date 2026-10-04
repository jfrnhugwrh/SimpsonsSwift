//! End-to-end tests: build a real (if small) ARMv7 Mach-O executable, load it
//! the way XNU + dyld would, and run it on the interpreter.
//!
//! They prove the pipeline *The Simpsons Arcade* needs is wired up end to end:
//! header and load-command parsing, segment mapping, dyld rebase/bind, symbol
//! stubs, the Darwin syscall ABI (`svc #0x80`), the HLE trampoline page, and the
//! OpenGL ES software renderer.

use macho::test_support::{build, Layout, Program, Stub, PAGE};
use runtime::{LoadOptions, Machine, StopReason};

// ---------------------------------------------------------------------------
// A small ARM/Thumb assembler: enough for the fixtures below, written so a
// mistake is an assertion failure rather than silently different code.
// ---------------------------------------------------------------------------

mod asm {
    /// `mov rd, #imm8`.
    pub fn mov_imm(rd: u32, imm: u32) -> u32 {
        assert!(imm <= 0xff && rd < 16);
        0xe3a0_0000 | (rd << 12) | imm
    }

    /// `movw rd, #imm16`.
    pub fn movw(rd: u32, imm: u32) -> u32 {
        assert!(imm <= 0xffff && rd < 16);
        0xe300_0000 | ((imm >> 12) << 16) | (rd << 12) | (imm & 0xfff)
    }

    /// `movt rd, #imm16`.
    pub fn movt(rd: u32, imm: u32) -> u32 {
        assert!(imm <= 0xffff && rd < 16);
        0xe340_0000 | ((imm >> 12) << 16) | (rd << 12) | (imm & 0xfff)
    }

    /// `ldr rd, [pc, #imm]` loading the absolute address `target`, where
    /// `insn_addr` is the instruction's own address.
    pub fn ldr_pc_at(rd: u32, insn_addr: u32, target: u32) -> u32 {
        let imm = target as i64 - (insn_addr as i64 + 8);
        assert!((0..=0xfff).contains(&imm) && imm % 4 == 0, "bad pc-relative load: {imm}");
        0xe59f_0000 | (rd << 12) | imm as u32
    }

    /// `ldr rd, [rn]`.
    pub fn ldr_reg(rd: u32, rn: u32) -> u32 {
        0xe590_0000 | (rn << 16) | (rd << 12)
    }

    /// `svc #imm24` — the Darwin ABI uses 0x80 with the call number in r12.
    pub fn svc(imm: u32) -> u32 {
        0xef00_0000 | (imm & 0x00ff_ffff)
    }

    /// `bl target`, where `pc` is the branch's own address.
    pub fn bl(pc: u32, target: u32) -> u32 {
        let offset = (target as i64 - (pc as i64 + 8)) >> 2;
        assert!((-0x80_0000..0x80_0000).contains(&offset), "bl out of range");
        0xeb00_0000 | ((offset as u32) & 0x00ff_ffff)
    }

    /// `blx rm`.
    pub fn blx_reg(rm: u32) -> u32 {
        assert!(rm < 16);
        0xe12f_ff30 | rm
    }

    pub fn arm(words: &[u32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(words.len() * 4);
        for word in words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }

    // --- Thumb-1 (16-bit) --------------------------------------------------
    /// `movs rd, #imm8`.
    pub fn t_movs_imm(rd: u32, imm: u32) -> u16 {
        assert!(imm <= 0xff && rd < 8);
        0x2000 | ((rd as u16) << 8) | (imm as u16)
    }

    /// `adds rd, rd, rn` (T1 encoding: `0001 100 Rm Rn Rd`).
    pub fn t_adds_reg(rd: u32, rn: u32) -> u16 {
        assert!(rd < 8 && rn < 8);
        0x1800 | ((rn as u16) << 6) | ((rd as u16) << 3) | rd as u16
    }

    /// `mov r12, rn` (high-register `mov`, `0100 0110 1 Rm Rd`).
    pub fn t_mov_r12(rn: u32) -> u16 {
        assert!(rn < 16);
        0x4680 | (1 << 7) | ((rn as u16) << 3) | 4
    }

    /// `ldr rd, [pc, #imm]` (literal), `imm = target - (insn_addr + 4)`.
    pub fn t_ldr_pc_at(rd: u32, insn_addr: u32, target: u32) -> u16 {
        let imm = target as i64 - (insn_addr as i64 + 4);
        assert!((0..=0x3fc).contains(&imm) && imm % 4 == 0, "bad Thumb literal load: {imm}");
        0x4800 | ((rd as u16) << 8) | ((imm / 4) as u16)
    }

    /// `str rd, [rn]` (`0110 000 imm5 Rn Rd`, offset 0).
    pub fn t_str_reg(rd: u32, rn: u32) -> u16 {
        assert!(rd < 8 && rn < 8);
        0x6000 | ((rn as u16) << 3) | rd as u16
    }

    /// `nop` (`mov r8, r8`).
    pub fn t_nop() -> u16 {
        0x46c0
    }

    /// `svc #0x80`.
    pub fn t_svc_0x80() -> u16 {
        0xdf80
    }

    pub fn thumb(words: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(words.len() * 2);
        for word in words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }
}

fn options() -> LoadOptions {
    LoadOptions {
        program_name: "/var/mobile/Applications/Simpsons.app/Simpsons".to_string(),
        ..LoadOptions::default()
    }
}

/// Boot a fixture: parse the synthesised Mach-O and load it.
fn boot(program: &Program) -> (Machine, Layout) {
    let (bytes, layout) = build(program);
    let image = macho::MachO::from_bytes(bytes).expect("fixture parses");
    let machine = Machine::boot(image, &options()).expect("fixture loads");
    (machine, layout)
}

/// Build a fixture whose code is `body` words long, run a first pass with a
/// zeroed body to learn the layout, then assemble the real body with the
/// addresses the layout produced.  (The layout depends only on the *sizes* of
/// the sections, so both passes agree.)
fn boot_two_pass(
    program: &Program,
    body_words: usize,
    assemble: impl FnOnce(&Layout) -> Vec<u32>,
) -> (Machine, Layout) {
    let mut probe = program.clone();
    probe.code = vec![0u8; body_words * 4];
    let (_, layout) = build(&probe);
    let mut real = program.clone();
    real.code = asm::arm(&assemble(&layout));
    boot(&real)
}

/// `_main`'s offset from the image base (`__text` starts one page in).
const MAIN_OFFSET: u32 = PAGE;

// ---------------------------------------------------------------------------
// 1. Load, run, write to stdout, exit — straight through the syscall ABI
// ---------------------------------------------------------------------------

#[test]
fn boots_and_exits_through_the_syscall_abi() {
    let message = "hello from the guest\n";
    let program = Program {
        cstrings: {
            let mut bytes = message.as_bytes().to_vec();
            bytes.push(0);
            bytes
        },
        defines: vec![("_main".to_string(), MAIN_OFFSET)],
        ..Program::default()
    };
    // mov r0, #1            ; stdout
    // ldr r1, [pc, #pool]   ; r1 = message
    // mov r2, #len          ; length
    // mov r12, #4           ; write
    // svc #0x80
    // mov r0, #0            ; exit(0)
    // mov r12, #1
    // svc #0x80
    // .word <message>
    let (mut machine, layout) = boot_two_pass(&program, 9, |layout| {
        let code = layout.code_vmaddr;
        vec![
            asm::mov_imm(0, 1),
            asm::ldr_pc_at(1, code + 4, code + 8 * 4), // loads the pool word below
            asm::mov_imm(2, message.len() as u32),
            asm::mov_imm(12, 4),
            asm::svc(0x80),
            asm::mov_imm(0, 0),
            asm::mov_imm(12, 1),
            asm::svc(0x80),
            layout.cstring_vmaddr,
        ]
    });

    assert_eq!(machine.image.entry, layout.entry_vmaddr, "LC_MAIN points at __text");
    assert_eq!(machine.cpu.pc(), layout.entry_vmaddr, "execution starts at the entry point");
    assert!(!machine.cpu.thumb(), "LC_MAIN enters in ARM mode");
    assert!(machine.stack.sp > 0, "an initial stack was built");
    assert_eq!(machine.image.main_symbol(), Some(layout.code_vmaddr), "_main resolves");

    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(String::from_utf8_lossy(&machine.sys.stdout), message);
    assert!(machine.stats.syscalls >= 2, "write and exit went through svc #0x80");
    assert_eq!(machine.stats.undefined_instructions, 0);
}

// ---------------------------------------------------------------------------
// 2. Imports: stub -> lazy pointer -> HLE trampoline -> host implementation
// ---------------------------------------------------------------------------

#[test]
fn lazy_bound_imports_reach_the_hle_layer() {
    let mut program = Program {
        stubs: vec![Stub {
            symbol: "_puts".to_string(),
            // `ldr r12, [pc, #0]; ldr pc, [r12]; .word <__la_symbol_ptr>`
            bytes: asm::arm(&[0xe59f_c000, 0xe59c_f000, 0]),
        }],
        lazy: vec!["_puts".to_string()],
        cstrings: b"printed by the HLE layer\0".to_vec(),
        defines: vec![("_main".to_string(), MAIN_OFFSET)],
        ..Program::default()
    };
    // Pass 1: same sizes, so the addresses below are the real ones.
    let mut probe = program.clone();
    probe.code = vec![0u8; 6 * 4];
    let (_, layout) = build(&probe);

    let code = layout.code_vmaddr;
    let stub = layout.stub_for("_puts", &program).expect("stub exists");
    let la_slot = layout.la_ptr_vmaddr[0];
    program.code = asm::arm(&[
        asm::ldr_pc_at(0, code, code + 5 * 4), // r0 = message (pool word below)
        asm::bl(code + 4, stub),                       // bl <_puts stub>
        asm::mov_imm(0, 0),                            // exit(0)
        asm::mov_imm(12, 1),
        asm::svc(0x80),
        layout.cstring_vmaddr,
    ]);
    program.stubs[0].bytes = asm::arm(&[0xe59f_c000, 0xe59c_f000, la_slot]);

    let (mut machine, _) = boot(&program);

    // dyld's job: the lazy pointer must already hold the trampoline, and the
    // stub must therefore jump straight to the HLE layer.
    let import = machine
        .image
        .imports
        .iter()
        .find(|i| i.symbol == "_puts")
        .expect("_puts is bound");
    assert!(import.lazy, "bound from the lazy bind stream");
    assert_ne!(import.trampoline, 0);
    assert_eq!(machine.image.symbol_at_trampoline(import.trampoline), Some("_puts"));
    assert_eq!(machine.mem.read_u32(import.slot).unwrap(), import.trampoline);
    assert_eq!(machine.mem.read_u32(la_slot).unwrap(), import.trampoline);

    let reason = machine.run(10_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(String::from_utf8_lossy(&machine.sys.stdout), "printed by the HLE layer\n");
    assert!(machine.stats.hle_calls >= 1, "the trampoline dispatched to a HLE handler");
    assert_eq!(machine.stats.undefined_instructions, 0);
}

// ---------------------------------------------------------------------------
// 3. Rendering: OpenGL ES through the HLE layer produces pixels
// ---------------------------------------------------------------------------

/// Builds a call sequence: arguments, then `ldr r3, [pc, #pool]; ldr r3, [r3];
/// blx r3`, with one literal per call holding the `__nl_symbol_ptr` slot.
struct Emitter {
    insns: Vec<u32>,
    /// `(instruction index, literal index)` of every `ldr r3, [pc, #…]`.
    loads: Vec<(usize, usize)>,
    symbols: Vec<&'static str>,
}

impl Emitter {
    fn new() -> Emitter {
        Emitter { insns: Vec::new(), loads: Vec::new(), symbols: Vec::new() }
    }

    /// Emit `args`, then call the imported function `symbol`.
    ///
    /// r0-r3 hold the arguments, so the function pointer is loaded into r12
    /// (the intra-procedure scratch register) instead.
    fn call(&mut self, args: &[u32], symbol: &'static str) {
        self.insns.extend_from_slice(args);
        let index = self.insns.len();
        self.insns.push(0); // patched by `finish`: `ldr r12, [pc, #pool]`
        self.insns.push(asm::ldr_reg(12, 12));
        self.insns.push(asm::blx_reg(12));
        self.loads.push((index, self.symbols.len()));
        self.symbols.push(symbol);
    }

    /// Emit instructions that are not a call (the epilogue).
    fn raw(&mut self, words: &[u32]) {
        self.insns.extend_from_slice(words);
    }

    /// Number of words the finished stream occupies.
    fn words(&self) -> usize {
        self.insns.len() + self.symbols.len()
    }

    /// Resolve the literal pool: `slot(name)` gives the pointer's address.
    fn finish(mut self, code_vmaddr: u32, slot: impl Fn(&str) -> u32) -> Vec<u32> {
        let pool_base = code_vmaddr + (self.insns.len() as u32) * 4;
        for (index, literal) in &self.loads {
            let insn_addr = code_vmaddr + (*index as u32) * 4;
            let target = pool_base + (*literal as u32) * 4;
            self.insns[*index] = asm::ldr_pc_at(12, insn_addr, target);
        }
        let mut out = self.insns;
        out.extend(self.symbols.iter().map(|name| slot(name)));
        out
    }
}

#[test]
fn opengl_es_draws_into_the_framebuffer() {
    let imports: &[&'static str] = &[
        "_glViewport",
        "_glClearColor",
        "_glClear",
        "_glEnableClientState",
        "_glVertexPointer",
        "_glColor4f",
        "_glDrawArrays",
    ];

    // A triangle in clip space, stored in __data so the guest code only has to
    // pass a pointer.
    let mut data = Vec::new();
    for value in [-0.75f32, -0.75, 0.0, 0.75, -0.75, 0.0, 0.0, 0.75, 0.0] {
        data.extend_from_slice(&value.to_le_bytes());
    }

    let floats = |r: u32, value: f32| {
        vec![asm::movw(r, f32::to_bits(value) & 0xffff), asm::movt(r, f32::to_bits(value) >> 16)]
    };

    let mut emitter = Emitter::new();
    emitter.call(
        &[asm::mov_imm(0, 0), asm::mov_imm(1, 0), asm::movw(2, 480), asm::movw(3, 320)],
        "_glViewport",
    );
    let mut clear_color = floats(0, 0.1);
    clear_color.extend(floats(1, 0.2));
    clear_color.extend(floats(2, 0.3));
    clear_color.extend(floats(3, 1.0));
    emitter.call(&clear_color, "_glClearColor");
    emitter.call(&[asm::movw(0, 0x4100)], "_glClear"); // colour | depth
    emitter.call(&[asm::movw(0, 0x8074)], "_glEnableClientState"); // GL_VERTEX_ARRAY
    let mut vertex_pointer = vec![asm::mov_imm(0, 3), asm::movw(1, 0x1406), asm::mov_imm(2, 0)];
    vertex_pointer.extend(floats(3, 0.0)); // patched once the data address is known
    emitter.call(&vertex_pointer, "_glVertexPointer");
    let mut color = floats(0, 1.0);
    color.extend(floats(1, 0.0));
    color.extend(floats(2, 0.0));
    color.extend(floats(3, 1.0));
    emitter.call(&color, "_glColor4f");
    emitter.call(&[asm::mov_imm(0, 4), asm::mov_imm(1, 0), asm::mov_imm(2, 3)], "_glDrawArrays");
    // exit(0), through the syscall layer
    emitter.raw(&[asm::mov_imm(0, 0), asm::mov_imm(12, 1), asm::svc(0x80)]);

    let mut program = Program {
        data,
        nonlazy: imports.iter().map(|s| s.to_string()).collect(),
        defines: vec![("_main".to_string(), MAIN_OFFSET)],
        ..Program::default()
    };

    // Pass 1: a zeroed body of the same length fixes the layout.
    let words = emitter.words();
    let mut probe = program.clone();
    probe.code = vec![0u8; words * 4];
    let (_, layout) = build(&probe);

    // Patch the two addresses the emitter could not know: the vertex array and
    // the slot of each imported function.
    let slot = |name: &str| {
        let index = imports.iter().position(|i| *i == name).expect("imported symbol");
        layout.nl_ptr_vmaddr[index]
    };
    let mut words = emitter.finish(layout.code_vmaddr, slot);

    // `_glVertexPointer`'s pointer argument: the two `movw`/`movt` words that
    // follow `mov r0,#3; movw r1,#0x1406; mov r2,#0` in the emitted stream.
    let vp_call = words
        .iter()
        .position(|word| *word == asm::mov_imm(0, 3))
        .expect("the vertex pointer call is in the stream");
    words[vp_call + 3] = asm::movw(3, layout.data_blob_vmaddr & 0xffff);
    words[vp_call + 4] = asm::movt(3, layout.data_blob_vmaddr >> 16);

    program.code = asm::arm(&words);
    let (mut machine, _) = boot(&program);

    let reason = machine.run(200_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0), "the GL sequence ran to completion");
    assert_eq!(machine.stats.undefined_instructions, 0);

    let framebuffer = machine.sys.framebuffer.as_ref().expect("a framebuffer was created");
    assert_eq!((framebuffer.width, framebuffer.height), (480, 320), "glViewport sized it");
    assert_eq!(machine.sys.gl.triangles, 1, "one triangle was rasterised");
    assert_eq!(machine.sys.gl.draws, 1);

    // Cleared to (0.1, 0.2, 0.3) — truncation, like the GL layer — and then
    // covered by the red triangle in the middle of the viewport.
    assert_eq!(framebuffer.get(2, 2), [25, 51, 76, 255], "the clear colour fills the frame");
    assert_eq!(framebuffer.get(240, 160), [255, 0, 0, 255], "the triangle is in the centre");
    assert_eq!(framebuffer.get(240, 2), [25, 51, 76, 255], "above the triangle: cleared");
}

// ---------------------------------------------------------------------------
// 4. Thumb execution (LC_UNIXTHREAD entry with the Thumb bit set)
// ---------------------------------------------------------------------------

#[test]
fn executes_thumb_code() {
    let mut program = Program {
        thumb_entry: true,
        unixthread: true,
        defines: vec![("_main".to_string(), MAIN_OFFSET)],
        ..Program::default()
    };
    // movs r0, #5
    // movs r1, #7
    // adds r0, r0, r1        -> 12
    // nop                    (keeps `ldr` 4-byte aligned: no PC-alignment subtlety)
    // ldr  r2, [pc, #lit]    -> &__data
    // str  r0, [r2]          -> *__data = 12
    // movs r0, #0
    // movs r1, #1
    // mov  r12, r1
    // svc  #0x80             -> exit(0)
    // .word <__data address>
    let (mut machine, layout) = {
        let mut probe = program.clone();
        probe.code = vec![0u8; 24];
        let (_, layout) = build(&probe);
        let code = layout.code_vmaddr;
        program.code = asm::thumb(&[
            asm::t_movs_imm(0, 5),
            asm::t_movs_imm(1, 7),
            asm::t_adds_reg(0, 1),
            asm::t_nop(),
            asm::t_ldr_pc_at(2, code + 8, code + 20),
            asm::t_str_reg(0, 2),
            asm::t_movs_imm(0, 0),
            asm::t_movs_imm(1, 1),
            asm::t_mov_r12(1),
            asm::t_svc_0x80(),
        ]);
        program.code.extend_from_slice(&layout.data_vmaddr.to_le_bytes());
        boot(&program)
    };
    let data_vmaddr = layout.data_vmaddr;

    assert_eq!(machine.image.entry, layout.entry_vmaddr | 1, "LC_UNIXTHREAD's Thumb bit");
    assert!(machine.cpu.thumb(), "the entry point's bit 0 selects Thumb state");
    assert_eq!(machine.cpu.pc(), layout.entry_vmaddr);

    let reason = machine.run(100).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(machine.mem.read_u32(data_vmaddr).unwrap(), 12, "the Thumb ALU result was stored");
    assert!(machine.stats.syscalls >= 1, "Thumb `svc #0x80` reaches the syscall layer");
    assert_eq!(machine.stats.undefined_instructions, 0);
}
