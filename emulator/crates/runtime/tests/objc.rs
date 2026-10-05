//! Objective-C bridge tests: a hand-built class hierarchy in the guest image,
//! driven through `_objc_msgSend` the way The Simpsons Arcade's `main` will
//! drive it (`[NSAutoreleasePool alloc] init`, `[[EAGLView alloc]
//! initWithFrame:]`, …).
//!
//! The image's `__data` receives a `Base`/`Sub` class pair in the modern
//! (`class_ro_t`) layout, two methods with real guest code as their IMPs, and
//! an instance.  Every test dispatches a message through the same
//! `hle::lookup` path the trampoline uses, then lets the machine run — an IMP
//! that fires pushes its return through `HLE_RETURN` and lands in a caller
//! that `exit(r0)`s, so `StopReason::Exited(n)` is the observable result.

use macho::test_support::{build, Program};
use runtime::{LoadOptions, Machine, StopReason};

mod asm {
    /// `mov rd, #imm8`.
    pub fn mov_imm(rd: u32, imm: u32) -> u32 {
        assert!(imm <= 0xff && rd < 16);
        0xe3a0_0000 | (rd << 12) | imm
    }

    /// `bx rm`.
    pub fn bx(rm: u32) -> u32 {
        assert!(rm < 16);
        0xe12f_ff10 | rm
    }

    /// `svc #imm24`.
    pub fn svc(imm: u32) -> u32 {
        0xef00_0000 | (imm & 0x00ff_ffff)
    }

    pub fn arm(words: &[u32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(words.len() * 4);
        for word in words {
            out.extend_from_slice(&word.to_le_bytes());
        }
        out
    }
}

const GRAPH_SIZE: usize = 512;

// Code section layout (all word offsets):
//   +0x00  imp_answer:  Base's `answer:` IMP — `mov r0, #7; bx lr`
//   +0x08  imp_extra:   Sub's `extra:` IMP — `mov r0, #9; bx lr`
//   +0x10  caller:      `mov r12, #1; svc #0x80` — `exit(r0)`
const IMP_ANSWER: u32 = 0x00;
const IMP_EXTRA: u32 = 0x08;
const CALLER: u32 = 0x10;

fn code() -> Vec<u8> {
    asm::arm(&[
        asm::mov_imm(0, 7),
        asm::bx(14),
        asm::mov_imm(0, 9),
        asm::bx(14),
        asm::mov_imm(12, 1),
        asm::svc(0x80),
    ])
}

/// A little builder for the Objective-C metadata the `__data` section holds.
struct Blob {
    bytes: Vec<u8>,
    base: u32,
}

impl Blob {
    fn new(base: u32) -> Blob {
        Blob { bytes: Vec::new(), base }
    }

    fn off(&self) -> u32 {
        self.bytes.len() as u32
    }

    fn addr(&self, off: u32) -> u32 {
        self.base + off
    }

    fn word(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn cstr(&mut self, s: &str) -> u32 {
        let off = self.off();
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        while self.bytes.len() % 4 != 0 {
            self.bytes.push(0);
        }
        off
    }
}

/// All the addresses the test pokes at afterwards.
struct Graph {
    obj: u32,
    sel_answer: u32,
    sel_extra: u32,
    sel_missing: u32,
    class_sub: u32,
    imp_answer: u32,
    imp_extra: u32,
    caller: u32,
}

/// Write the two class objects (plus metaclasses), their `class_ro_t`s, two
/// 12-byte-entry method lists, the selector names and one `Sub` instance.
fn make_blob(base: u32, code: u32) -> (Vec<u8>, Graph) {
    let mut b = Blob::new(base);

    let name_base = b.cstr("Base");
    let name_sub = b.cstr("Sub");
    let name_answer = b.cstr("answer:");
    let name_extra = b.cstr("extra:");
    let name_missing = b.cstr("missing:");

    // method_list_t: [entsizeAndFlags=12, count, {SEL, types, IMP} ...]
    let ml_base = b.off();
    b.word(12);
    b.word(1);
    b.word(b.addr(name_answer));
    b.word(0);
    b.word(code + IMP_ANSWER);

    let ml_sub = b.off();
    b.word(12);
    b.word(1);
    b.word(b.addr(name_extra));
    b.word(0);
    b.word(code + IMP_EXTRA);

    // class_ro_t: [flags, instanceStart, instanceSize, ivarLayout, name,
    //              baseMethods, baseProtocols, ivars, weakIvarLayout,
    //              baseProperties] — 10 words.
    let ro_base = b.off();
    b.word(0);
    b.word(4);
    b.word(32);
    b.word(0);
    b.word(b.addr(name_base));
    b.word(b.addr(ml_base));
    for _ in 0..4 {
        b.word(0);
    }

    let ro_base_meta = b.off();
    b.word(0);
    b.word(4);
    b.word(0);
    b.word(0);
    b.word(b.addr(name_base));
    for _ in 0..5 {
        b.word(0);
    }

    let ro_sub = b.off();
    b.word(0);
    b.word(4);
    b.word(32);
    b.word(0);
    b.word(b.addr(name_sub));
    b.word(b.addr(ml_sub));
    for _ in 0..4 {
        b.word(0);
    }

    let ro_sub_meta = b.off();
    b.word(0);
    b.word(4);
    b.word(0);
    b.word(0);
    b.word(b.addr(name_sub));
    for _ in 0..5 {
        b.word(0);
    }

    // struct objc_class: [isa, superclass, cache, vtable, pad, bits] — 0x18.
    let class_base_meta = b.off();
    b.word(b.addr(class_base_meta)); // metaclasses are their own isa
    b.word(0);
    b.word(0);
    b.word(0);
    b.word(0);
    b.word(b.addr(ro_base_meta));

    let class_base = b.off();
    b.word(b.addr(class_base_meta));
    b.word(0); // Base is the root class
    b.word(0);
    b.word(0);
    b.word(0);
    b.word(b.addr(ro_base));

    let class_sub_meta = b.off();
    b.word(b.addr(class_sub_meta));
    b.word(b.addr(class_base_meta));
    b.word(0);
    b.word(0);
    b.word(0);
    b.word(b.addr(ro_sub_meta));

    let class_sub = b.off();
    b.word(b.addr(class_sub_meta));
    b.word(b.addr(class_base));
    b.word(0);
    b.word(0);
    b.word(0);
    b.word(b.addr(ro_sub));

    // One `Sub` instance: isa + three zeroed words of "ivars".
    let obj = b.off();
    b.word(b.addr(class_sub));
    for _ in 0..3 {
        b.word(0);
    }

    assert!(b.bytes.len() <= GRAPH_SIZE, "metadata graph grew beyond {GRAPH_SIZE} bytes");
    let graph = Graph {
        obj: b.addr(obj),
        sel_answer: b.addr(name_answer),
        sel_extra: b.addr(name_extra),
        sel_missing: b.addr(name_missing),
        class_sub: b.addr(class_sub),
        imp_answer: code + IMP_ANSWER,
        imp_extra: code + IMP_EXTRA,
        caller: code + CALLER,
    };
    (b.bytes, graph)
}

fn program() -> (Program, Graph) {
    let mut probe = Program {
        code: vec![0u8; 24],
        data: vec![0u8; GRAPH_SIZE],
        defines: vec![("_main".to_string(), 0x100)],
        ..Program::default()
    };
    probe.cstrings = b"unused\0".to_vec();
    let (_, layout) = build(&probe);

    let (blob, graph) = make_blob(layout.data_blob_vmaddr, layout.code_vmaddr);
    let mut real = Program {
        code: code(),
        data: blob,
        defines: vec![("_main".to_string(), 0x100)],
        ..Program::default()
    };
    real.cstrings = b"unused\0".to_vec();
    (real, graph)
}

fn options() -> LoadOptions {
    LoadOptions {
        program_name: "/var/mobile/Applications/Simpsons.app/Simpsons".to_string(),
        ..LoadOptions::default()
    }
}

fn boot() -> (Machine, Graph) {
    let (program, graph) = program();
    let (bytes, _) = build(&program);
    let image = macho::MachO::from_bytes(bytes).expect("fixture parses");
    let machine = Machine::boot(image, &options()).expect("fixture loads");
    (machine, graph)
}

/// Dispatch `objc_msgSend(r0=receiver, r1=selector)` exactly as if the guest
/// had executed the trampoline, with the caller at `graph.caller`.
fn dispatch(machine: &mut Machine, graph: &Graph, receiver: u32, selector: u32) {
    machine.cpu.r[0] = receiver;
    machine.cpu.r[1] = selector;
    machine.cpu.r[14] = graph.caller;
    machine.hle_dispatch("_objc_msgSend").expect("msgSend dispatches");
}

#[test]
fn a_message_reaches_the_guest_implementation() {
    let (mut machine, graph) = boot();
    dispatch(&mut machine, &graph, graph.obj, graph.sel_extra);
    assert_eq!(machine.cpu.pc(), graph.imp_extra, "probability: dispatch jumped to the IMP");
    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(9), "the IMP ran and its r0 reached the caller");
    assert_eq!(machine.sys.objc.guest_calls, 1);
}

#[test]
fn method_lookup_follows_the_superclass_chain() {
    let (mut machine, graph) = boot();
    dispatch(&mut machine, &graph, graph.obj, graph.sel_answer);
    assert_eq!(machine.cpu.pc(), graph.imp_answer, "Base's `answer:` runs for a Sub instance");
    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(7));
}

#[test]
fn messaging_nil_is_a_safe_no_op() {
    let (mut machine, graph) = boot();
    dispatch(&mut machine, &graph, 0, graph.sel_extra);
    // Answered 0, control went straight back to the caller — no IMP ran.
    assert_eq!(machine.cpu.pc(), graph.caller);
    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(machine.sys.objc.nil_messages.get("extra:"), Some(&1));
    assert_eq!(machine.sys.objc.guest_calls, 0);
}

#[test]
fn an_unimplemented_selector_is_reported_not_crash() {
    let (mut machine, graph) = boot();
    dispatch(&mut machine, &graph, graph.obj, graph.sel_missing);
    assert_eq!(machine.cpu.pc(), graph.caller, "the machine returns instead of fetching 0");
    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(machine.sys.objc.missing, 1);
    assert!(
        machine.sys.objc.unrecognized.keys().any(|sel| sel.contains("missing:")),
        "the unrecognized selector was recorded: {:?}",
        machine.sys.objc.unrecognized
    );
}

#[test]
fn a_null_imp_cannot_hijack_the_program_counter() {
    let (mut machine, graph) = boot();
    // Rip the IMP out of Base's method list (as a corrupt/mis-parsed image
    // would): the message then resolves to a NULL implementation.
    //
    // Walk the same path the loader exposed: Base's method list lives in the
    // blob; find `answer:`'s entry through the class and zero its IMP.
    let bits = machine.mem.read_u32(graph.class_sub + 4).unwrap(); // superclass
    let ro = machine.mem.read_u32(bits + 0x14).unwrap();
    let list = machine.mem.read_u32(ro + 0x14).unwrap();
    assert_eq!(machine.mem.read_u32(list).unwrap(), 12, "the test wrote a real method list");
    let imp_slot = list + 8 + 8;
    let before = machine.mem.read_u32(imp_slot).unwrap();
    assert_eq!(before, graph.imp_answer);
    machine.mem.write_u32(imp_slot, 0).unwrap();

    dispatch(&mut machine, &graph, graph.obj, graph.sel_answer);
    assert_eq!(machine.cpu.pc(), graph.caller, "a NULL IMP answers nil instead of jumping");
    let reason = machine.run(1_000).expect("no fault");
    assert_eq!(reason, StopReason::Exited(0));
    assert_eq!(machine.stats.memory_faults, 0);
}

#[test]
fn retain_and_class_come_from_the_runtime() {
    let (mut machine, graph) = boot();

    // `retain` returns self.
    let sel = write_selector(&mut machine, "retain");
    machine.cpu.r[0] = graph.obj;
    machine.cpu.r[1] = sel;
    machine.cpu.r[14] = graph.caller;
    machine.hle_dispatch("_objc_msgSend").expect("retain dispatches");
    assert_eq!(machine.cpu.r[0], graph.obj, "retain returns self");
    assert_eq!(machine.cpu.pc(), graph.caller, "control resumed at the caller");

    // `class` returns the object's class.
    let sel = write_selector(&mut machine, "class");
    machine.cpu.r[0] = graph.obj;
    machine.cpu.r[1] = sel;
    machine.cpu.r[14] = graph.caller;
    machine.hle_dispatch("_objc_msgSend").expect("class dispatches");
    assert_eq!(machine.cpu.r[0], graph.class_sub, "-[Sub class] -> the Sub class object");
}

/// Write a selector name into scratch guest memory (the stack is RW) for the
/// dispatch tests that use selectors the image does not define.
fn write_selector(machine: &mut Machine, name: &str) -> u32 {
    let addr = machine.stack.sp - 64;
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    machine.mem.poke_bytes(addr, &bytes).expect("selector name lands");
    addr
}

#[test]
fn imported_objc_class_symbols_get_real_class_objects() {
    let program = Program {
        code: code(),
        nonlazy: vec!["_OBJC_CLASS_$_Widget".to_string()],
        defines: vec![("_main".to_string(), 0x100)],
        ..Program::default()
    };
    let (bytes, _layout) = build(&program);
    let image = macho::MachO::from_bytes(bytes).expect("fixture parses");
    let machine = Machine::boot(image, &options()).expect("fixture loads");

    // Copy the address out of the import table right away: `machine` is moved
    // below, so no borrow of `machine.image.imports` may stay alive.
    let binding = machine
        .image
        .imports
        .iter()
        .find(|i| i.symbol == "_OBJC_CLASS_$_Widget")
        .expect("the class symbol is bound")
        .trampoline;
    assert!(
        binding >= 0x7001_0000 && binding < 0x7002_0000,
        "the slot holds a synthetic class object, not a trampoline ({binding:#x})"
    );
    assert_eq!(
        machine.sys.host_classes.get(&binding).map(String::as_str),
        Some("Widget"),
        "the bridge knows the class the slot points at"
    );

    // And the class is a working receiver: `+[Widget alloc]` allocates.
    let mut machine = machine;
    let sel = write_selector(&mut machine, "alloc");
    machine.cpu.r[0] = binding;
    machine.cpu.r[1] = sel;
    machine.cpu.r[14] = machine.image.entry; // any executable address
    machine.hle_dispatch("_objc_msgSend").expect("alloc dispatches");
    let instance = machine.cpu.r[0];
    assert!(instance != 0 && instance != binding, "alloc produced an object");
    assert_eq!(machine.mem.read_u32(instance).unwrap(), binding, "isa is the class");
}
