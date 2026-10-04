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
  crates/ipa/                  .ipa reader: ZIP, DEFLATE, plist, validation, import
  crates/cli/                  `simpsons-emu import | games | info | dump | run`
android/                       Android app that runs the emulator on a phone
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

## Importing the game

**This repository contains no game.** No ROM, no binary and no asset is bundled,
downloaded, distributed or hard-coded anywhere; the emulator is useless until you
point it at a copy of *The Simpsons Arcade* (iOS, EA, 2009) that you obtained
legally — the release this repository's decompilation came from is
`The Simpsons Arcade v1.1.43.ipa`, bundle id `com.ea.simpsonsarcade.bv`.

```sh
simpsons-emu import "The Simpsons Arcade v1.1.43.ipa"   # validate + extract
simpsons-emu games                                      # what is in the library
simpsons-emu run "The Simpsons Arcade v1.1.43.ipa" --serve 8080
```

`import` reads the archive, checks it, and extracts the bundle into the game
library — `$XDG_DATA_HOME/simpsons-emu/games/<bundle-id>/` by default
(`$SIMPSONS_EMU_GAMES` or `--dest` overrides it).  The extracted bundle is an
ordinary iOS app bundle, so the pre-existing entry point is all the emulator
needs:

```sh
simpsons-emu run  ~/.local/share/simpsons-emu/games/com.ea.simpsonsarcade.bv/TheSimpsons.app/TheSimpsons \
                  --bundle ~/.local/share/simpsons-emu/games/com.ea.simpsonsarcade.bv/TheSimpsons.app \
                  --trace --serve 8080
```

`info`, `dump` and `run` also take the `.ipa` directly: it is imported on demand
and reused from then on, so running the same archive twice does not re-extract
it.  The browser preview served by `--serve` has an import panel too — pick the
file, press *Import*, and the same validation runs server-side.

What is checked before anything is written, each with its own error message:

| check | what it catches |
|---|---|
| ZIP end-of-central-directory, central directory, CRC-32 per entry | a truncated or damaged download |
| `Payload/<Name>.app/` with a readable `Info.plist` | a file that is not an iOS app package |
| `CFBundleExecutable` exists and is a Mach-O | a repackaged or hollowed-out bundle |
| the Mach-O has a 32-bit ARM slice | an arm64-only or simulator build (this emulator is ARMv7) |
| `LC_ENCRYPTION_INFO` `cryptid == 0` | a FairPlay-encrypted App Store download, which no emulator can read |
| bundle id / display name / version | some other iOS app, or a different version of this one |

A valid IPA that is not this game is refused unless `--allow-other-app` is
passed; a different *version* of this game imports with a warning, because the
HLE surface is written against 1.1.43.  Extraction refuses `..` components,
absolute paths and symlinks, and caps the entry count and total size.  Nothing
is ever re-uploaded anywhere, and only the extracted bundle is kept — the `.ipa`
itself is not copied into the library.

The ZIP reader, the DEFLATE inflater and the XML/binary plist parser are part of
`crates/ipa`: the workspace has no external dependencies, and `cargo build
--offline` has to keep working.

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
  claim above is about the emulator's components, not about the game. The import
  path *is* tested end to end, but against a synthetic ARMv7 Mach-O packaged as
  an `.ipa` — not against the real one, which nobody may redistribute.
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

## Continuous integration

Two workflows live in `.github/workflows/`:

* **`android.yml`** — builds the APK.  Runs on a push to `main`, on a `v*` tag
  and from the *Actions* tab.  It cross-compiles `simpsons-emu` for `arm64-v8a`,
  `armeabi-v7a`, `x86_64` and `x86` with the NDK the runner already ships, builds
  the demo image, assembles and signs `android/`, checks the result really is a
  signed APK carrying all four binaries, and attaches it to the run *and* to the
  rolling `android-latest` release.  A last, non-blocking job installs that APK
  on an Android emulator, launches it, and checks that the packaged binary boots
  the demo and that the preview reaches the app's `WebView` — the screenshot and
  the device log are attached to the run.  See [Android](#android).
* **`cleanup-runs.yml`** — deletes old workflow runs every three hours
  (`cron: 0 */3 * * *`), keeping only the run doing the deleting.  Dispatch it by
  hand to keep the newest few per workflow, to protect recent runs, or to do a
  dry run.  It never touches releases or tags, which is why the APK is published
  as a release asset: deleting a run deletes its artifacts with it.  GitHub only
  runs schedules from the default branch, so this has to be on `main` to fire.

## Android

[`android/`](android/README.md) is a small, dependency-free app around the
emulator: the APK carries the ordinary `simpsons-emu` binary as
`lib/<abi>/libsimpsons-emu.so` (the only place Android still allows an exec),
runs it, pipes its output into a log view, and points a `WebView` at the live
framebuffer the emulator serves on loopback.

Install the APK from the `android-latest` release, then

* **Demo** boots the synthetic ARMv7 Mach-O that ships in the APK — the loader,
  the interpreter and the HLE, with no copyrighted file involved;
* **Import .ipa** runs the emulator's own importer, with the same validation as
  the desktop CLI, on a decrypted copy of the game that you supply;
* **Play** runs what you imported.

Cross-compiling by hand, if you would rather not use CI:

```sh
cd emulator
rustup target add aarch64-linux-android
export NDK="$ANDROID_NDK_HOME"                       # or the NDK you installed
export CC="$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang"
cat >> ~/.cargo/config.toml <<EOF
[target.aarch64-linux-android]
linker = "$CC"
EOF
cargo build --release --target aarch64-linux-android --bin simpsons-emu
```

That binary runs on a device on its own, too — `adb push` it to
`/data/local/tmp/` and use it exactly like the desktop CLI.  The game is never
part of any artifact: import your own decrypted copy with `simpsons-emu import`.

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
* **IPA import.** An `.ipa` is a ZIP, so `crates/ipa` is a ZIP reader (central
  directory, ZIP64, CRC-32), a raw DEFLATE decoder, a property-list reader for
  both `Info.plist` dialects, the validation described above and the extraction
  into the game library.  It is the only part of the workspace that touches a
  file the user supplies, so every offset is range-checked and every entry is
  checked for path escapes before it is written.
