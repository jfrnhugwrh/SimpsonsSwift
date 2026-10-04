# Android app

A thin, dependency-free front end around the emulator: the APK carries the
ordinary `simpsons-emu` command line binary, cross-compiled for the four Android
ABIs, and this app runs it.

```
android/
  app/src/main/java/com/simpsonsswift/emulator/
    MainActivity.java      the whole UI: toolbar, log, preview
    EmulatorSession.java   one `simpsons-emu run … --serve` process
    NativeTool.java        where the binary is and what environment it gets
    GameLibrary.java       reads the importer's import.json manifests
  app/src/main/jniLibs/<abi>/libsimpsons-emu.so   staged by CI (not in git)
  app/src/main/assets/demo-armv7                  staged by CI (not in git)
  tools/make_launcher_icons.py                    draws the launcher icon
```

No AndroidX, no Kotlin, no third-party library — the same rule the Rust
workspace follows. The only build dependency is the Android Gradle plugin.

## How it works

* **The binary is packaged as `lib/<abi>/libsimpsons-emu.so`.** It is an
  executable, not a JNI library, and it is never `dlopen`ed. Android only lets
  an app exec files from its native library directory, so that name (and
  `useLegacyPackaging = true`, which makes the installer unpack it instead of
  mapping it out of the APK) is what makes it runnable at all.
* **The app runs it and reads its stdout** into the log view.
* **`--serve <port> --bind 127.0.0.1`** gives the emulator's live framebuffer
  page to a `WebView`. The port is loopback-only, so the preview never leaves
  the device; cleartext is allowed for `127.0.0.1` alone.
* **Imports go through the emulator's own importer.** The file picker hands back
  a document, the app copies it into its cache and runs
  `simpsons-emu import … --dest <app files>/games`, so the validation is
  identical to the desktop CLI's — a corrupt archive, an arm64-only build, a
  FairPlay-encrypted App Store download or some other app all come back as the
  same readable error.

The app ships **no game**. *Demo* boots the synthetic ARMv7 Mach-O that
`make_demo` builds (real header, dyld tables, a lazy `_puts` stub, Darwin
syscalls), which exercises the loader, the interpreter and the HLE without any
copyrighted file. *Play* runs whatever you imported.

## Building it

CI does this for you — run the **Android APK** workflow (or push to `main`) and
take the APK from the run's artifacts or from the `android-latest` release.

By hand you need the Android SDK (platform 34, build-tools 34.0.0), JDK 17,
Gradle 8.9+ and the NDK:

```sh
# 1. cross-compile the emulator for each ABI you want
cd emulator
rustup target add aarch64-linux-android
NDK=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
cat >> ~/.cargo/config.toml <<EOF
[target.aarch64-linux-android]
linker = "$NDK/aarch64-linux-android24-clang"
EOF
cargo build --release --target aarch64-linux-android --bin simpsons-emu

# 2. stage it under the name Android will unpack
install -D -m755 target/aarch64-linux-android/release/simpsons-emu \
  ../android/app/src/main/jniLibs/arm64-v8a/libsimpsons-emu.so

# 3. (optional) the demo image
cargo run -p cli --example make_demo -- ../android/app/src/main/assets/demo-armv7

# 4. build the APK
cd ../android
gradle :app:assembleRelease     # app/build/outputs/apk/release/app-release.apk
```

Without a keystore in the environment the release APK is signed with the local
debug key, which is enough to install it. CI signs with
`ANDROID_KEYSTORE_BASE64` / `ANDROID_KEYSTORE_PASSWORD` / `ANDROID_KEY_ALIAS` /
`ANDROID_KEY_PASSWORD` when those secrets exist, and with a throwaway key when
they do not.

There is no Gradle wrapper in the repository on purpose: the wrapper jar is a
binary, and CI installs Gradle itself. Use your own `gradle` or Android Studio.

## Installing

`minSdk 24` (Android 7.0), `targetSdk 34`, one universal APK with all four
ABIs. Allow your browser or file manager to install unknown apps, open the
`.apk`, confirm. When the APK is signed with a throwaway CI key the signature
changes from build to build, so an update will ask you to uninstall the previous
one first.
