# SimpsonsSwift — emulator for the iOS build of The Simpsons Arcade

A from-scratch, dependency-free Rust emulator for the ARMv7 iOS binary that
`simpsons3_iphone_en.txt` (the Ghidra decompilation in the repository root) was
produced from:

```
simpsons3_iphone_en.txt        Ghidra decompilation of the app binary
emulator/                      the emulator itself (std-only Rust workspace)
  crates/macho/                Mach-O reader: headers, load commands, dyld info
  crates/guestmem/             guest address space, permissions, regions
  crates/arm/                  ARM + Thumb/Thumb-2 + VFP interpreter
  crates/runtime/              loader, syscall layer, HLE for the iOS frameworks
  crates/cli/                  `simpsons-emu info | dump | run`
```

The C listing was used as the behavioural reference (symbol surface, syscall
usage, entry sequence), not translated line by line; the interpreter, loader and
HLE are written against the ARM architecture reference and validated against
QEMU, as described below.

## Building and running

The workspace has **no third-party dependencies** and builds with any Rust 1.88+
toolchain:

```sh
cd emulator
cargo test            # 334 differential instruction cases + crate tests
cargo build --release # target/release/simpsons-emu
```

To run the real game binary (which is **not** in this repository, so this path
is unverified end to end):

```sh
simpsons-emu info  Simpsons.app/Simpsons                  # segments, imports, entry
simpsons-emu dump  Simpsons.app/Simpsons --section __TEXT.__text --length 256
simpsons-emu run   Simpsons.app/Simpsons --bundle Simpsons.app \
                   --trace --stats --screenshot frame.bmp --serve 8080
```

To exercise the whole pipeline without the game, build a synthetic ARMv7
Mach-O — real header, dyld tables, a lazy `_puts` stub and Darwin syscalls — and
run it:

```sh
cargo run --example make_demo -- /tmp/demo-armv7     # writes the image
simpsons-emu run /tmp/demo-armv7 --trace --stats     # -> "hello from the guest"
```

`run` boots the image (segments mapped at their Mach-O addresses, dyld imports
bound to HLE trampolines, a Darwin-style initial stack), executes until the
instruction budget is exhausted, the guest exits, or it traps. On a trap it
prints the address, the raw encoding and whether the fault was a memory access,
an undefined instruction or a syscall/HLE call. `--tolerate-undefined` steps over
unknown instructions and logs them, which turns a hard stop into a list of what
still has to be implemented.

## What is verified, and how

`crates/arm/src/unicorn_golden.rs` is a generated table of **334 instruction
cases**: each one executes a snippet in QEMU (through Unicorn) from a fixed
initial register/memory/FP state and records r0–r12, SP, LR, PC, NZCV, the Thumb
state, a few words of memory and the floating-point registers. The test
`arm::unicorn_golden::interpreter_matches_qemu` runs the same snippet on the Rust
interpreter and requires an identical result. Multi-instruction cases cover
carry propagation, conditional execution, `IT` blocks and the GE flags.

The table is produced by `tools/gen_unicorn_golden.py`, which assembles each case
with Keystone, checks the bytes back with Capstone (so a mis-assembled case is
skipped rather than enshrined), runs it in Unicorn and writes the Rust file:

```sh
pip3 download --no-deps -d /tmp/armtools unicorn keystone-engine capstone
python3 -m zipfile -e /tmp/armtools/unicorn-*.whl /home/user/.cache/pyarm/
python3 -m zipfile -e /tmp/armtools/keystone_engine-*.whl /home/user/.cache/pyarm/
PYTHONPATH=/home/user/.cache/pyarm python3 emulator/tools/gen_unicorn_golden.py
```

Coverage: ARM data processing (immediates, all shift types, flags), the
multiply/divide space, `MOVW`/`MOVT`, `MRS`/`MSR`, `CLZ`, the bitfield families
(`SBFX`/`UBFX`/`BFI`/`BFC`), the media instructions (extends, `REV`/`REV16`/
`REVSH`/`RBIT`, `SSAT`/`USAT` in both shift directions, `PKHBT`/`PKHTB`,
`sadd`/`uadd`/`ssub`/`usub` 8- and 16-bit with the GE flags, `SEL`, `SMMLA`,
`USAD8`/`USADA8`, halfword multiplies), loads and stores (word/byte/halfword,
signed, `LDRD`/`STRD`, pre/post-indexed, writeback forms, `LDM`/`STM`,
`LDMDB`/`STMDB`), branches (`B`/`BL`, `BX`/`BLX` including interworking), the
`SWP` instructions, VFP (arithmetic, conversions, comparisons, loads/stores,
multiple transfers, `VMRS`/`VMSR`, core↔single moves and the immediate forms),
Thumb-1, Thumb-2 (modified and plain immediates, the whole `0xfb00` multiply
space, extend/reverse, parallel add/sub, load/store all four forms, dual and
exclusive accesses, `TBB`/`TBH`, branches, misc control) and `IT` blocks.

The bugs this found in the hand-written decoder are listed in the git history;
they include several that would have crashed or silently corrupted a real
program (Thumb-2 immediates read from the wrong bits, `SMUL*`/`UMULL` collisions,
VFP register numbering, branches that cleared the Thumb bit, and the Thumb-2
misc-control space being decoded as a branch).

## What is *not* verified

* **The game itself.** No game binary or ROM is present in this repository, so
  the end-to-end path (boot → render → playable) has never been executed. Every
  claim above is about the emulator's components, not about the game.
* **The HLE surface.** The syscall layer and the framework shims (libSystem,
  CoreFoundation, CoreGraphics, OpenGL ES, OpenAL, AudioToolbox, the Objective-C
  runtime) are written against the symbol list extracted from the Ghidra dump.
  Their behaviour is inferred, not observed: functions are grouped by what the
  dump's call sites imply, and several are deliberately shallow (a placeholder
  texture decode, `NSDate`/`NSTimer` stubs, one class object for every CF type).
* **NEON.** VFP is implemented and tested; the Advanced SIMD instruction set is
  not implemented and traps as an undefined instruction. If the game's binary
  contains NEON it will stop there with a precise diagnosis.
* **Rare encodings.** A handful of media instructions are left unimplemented on
  purpose rather than guessed (`SMLALD`/`SMLSLD`, `SMMLS`, the `x`-variants of the
  parallel add/subtract, `SMMULR`/`SMMLAR`, `VCVT` fixed-point, NEON).

## Architecture notes

* **Mach-O.** 32-bit headers, `LC_SEGMENT`, `LC_SYMTAB`, `LC_DYSYMTAB`,
  `LC_LOAD_DYLINKER`, `LC_UUID`, the `LC_DYLD_INFO(_ONLY)` tables and the lazy
  and non-lazy symbol pointers are parsed; `__TEXT`/`__DATA` are mapped at their
  `vmaddr` with their section permissions, `__bss`-style sections are zeroed, and
  dyld rebase/bind streams are applied so the guest sees concrete pointers.
* **Execution.** The entry point comes from the Mach-O header; from there the
  ARM/Thumb interpreter runs. `svc #0x80` (Darwin's trap) and the `__symbol_stub`
  trampolines both stop the interpreter and hand control to the runtime, which
  either performs the syscall or dispatches the HLE implementation of the
  requested symbol and resumes the guest.
* **Interpreter.** Not a cycle-accurate model: the differential test pins down
  architecturally visible behaviour (including flags, `IT` state, GE bits and
  VFP registers), which is what a game can observe.
