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
android/                       installable Android launcher and Gradle APK project
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

## The Objective-C runtime bridge

The game is a C++ engine wrapped in a thin Objective-C layer (`EAGLView`,
`RuntimeAppDelegate`, …), and every message goes through `objc_msgSend` — the
decompilation calls it 747 times. The bridge in `crates/runtime/src/hle/objc.rs`
implements the runtime surface:

* **Class symbols are data, not code.** `_OBJC_CLASS_$_*` imports are
  materialised as synthetic `struct objc_class` objects (in their own
  `0x7001_0000` region) instead of being bound to trampolines, so
  `+[UIDevice currentDevice]` behaves like messaging a real class.
* **The image's own classes are real.** `__objc_classlist`,
  `__objc_catlist` and `__objc_selrefs` are registered at boot; method
  lookup walks the guest's own `method_list_t`s (both the modern
  `class_ro_t` and legacy layouts), following the superclass chain for
  instance methods and the metaclass chain for class methods.  A resolved
  IMP is *executed by the guest interpreter*, and its return flows back to
  the caller.
* **Messaging `nil` is the ABI-exact no-op**: zero in `r0`/`r1`, and a
  zeroed struct-return buffer for `objc_msgSend_stret`.
* **Actionable failures.** A message with no implementation is reported
  (once, with a count) and answered `nil` — never a jump to `0x0`.  Every
  guest IMP entry is validated against mapped, executable memory first,
  and HLE control returns go through a shadow return stack plus a
  Thumb-strict `jump_to` on the CPU.
* **The full runtime surface** — `objc_msgSend{,_stret,Super,Super2,_stret,_fpret}`,
  `objc_getClass(…)`, `sel_registerName`, the `class_*`/`method_*`/`object_*`
  reflection family, ARC-era `objc_storeStrong/Weak/…`, associated objects,
  autorelease pools, sync / exception entry points and
  `NSClassFromString` & friends.  See `docs/objc-runtime.md` for the
  catalogue.

Run diagnostics:

```sh
simpsons-emu run <ipa> --objc-trace     # every dispatch, with resolution
simpsons-emu run <ipa> --objc-quiet     # only hard failures in the guest log
simpsons-emu run <ipa> --stats          # guest-IMP/host/miss counts + miss table
```

Any instruction-fetch fault also prints the last dispatch, the shadow return
stack and the recent HLE history, which is where a NULL IMP shows up.

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

The workflows live in `.github/workflows/`:

* **`android.yml`** — tests the Rust workspace, cross-compiles `simpsons-emu`
  for `arm64-v8a`, `armeabi-v7a`, `x86_64` and `x86`, then packages those binaries
  with the Android launcher as one signed, installable universal APK. It runs on
  relevant pushes and pull requests, or on demand from the *Actions* tab.
  Download `SimpsonsSwift-android-apk` from the run's artifacts.
* **`cleanup-workflow-runs.yml`** — every three hours (UTC), removes all
  **completed workflow runs** and their logs/artifacts from the repository.
  Queued and in-progress runs are preserved, as are the workflow definition
  files. The scheduled workflow must be present on the repository's default
  branch; repository/organization settings must allow its requested Actions write
  permission for `GITHUB_TOKEN`.

### Building for Android

The Android project under `android/` is a small Java launcher around the actual
Rust CLI. It lets you select a decrypted IPA through Android's document picker,
imports it into private app storage, launches the matching ABI's emulator binary,
and displays the emulator's local framebuffer/log preview in a WebView. The
workflow produces one universal APK containing all four supported Android ABIs;
it is debug-key signed so it can be installed directly for sideload testing.

The APK contains **no game binary or game assets**. Choose a decrypted copy of
*The Simpsons Arcade* that you obtained yourself after installing. The imported
IPA is removed from the app's temporary cache after import; the validated bundle
is retained in private app storage. The real game's end-to-end compatibility
remains unverified, as described above.

To build locally, install JDK 17, Android SDK platform 35, NDK `27.2.12479018`,
Gradle 8.9, and the four Rust Android targets. Cross-compile `simpsons-emu` with
the NDK clang linkers and stage each executable as
`android/app/src/main/jniLibs/<abi>/libsimpsons-emu.so`; then run
`gradle -p android :app:assembleRelease`. Gradle fails early if any ABI binary
is missing, rather than silently producing an APK that cannot launch the
emulator.

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
