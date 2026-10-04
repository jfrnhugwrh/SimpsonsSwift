#!/usr/bin/env bash
# Runs inside the Android emulator job, once the device has booted.
#
# Three questions, in order of how embarrassing a "no" would be:
#
#   1. does the cross-compiled binary run on Android at all?
#   2. does the APK install?
#   3. does the app actually exec the binary out of its native library
#      directory and get the preview server up?
#
# Everything it learns is written to smoke/ and uploaded, so a failure comes
# with the device log instead of just a red cross.

set -uo pipefail

package=com.simpsonsswift.emulator
activity="$package/.MainActivity"
mkdir -p smoke
: > smoke/verdict.txt

failures=0

check() {
    local what="$1"
    local ok="$2"
    if [ "$ok" = "yes" ]; then
        echo "* PASS — $what" >> smoke/verdict.txt
        echo "PASS  $what"
    else
        failures=$((failures + 1))
        echo "* FAIL — $what" >> smoke/verdict.txt
        echo "::warning::smoke test: $what"
        echo "FAIL  $what"
    fi
}

adb shell getprop ro.build.version.release > smoke/device.txt 2>&1
adb shell getprop ro.product.cpu.abi >> smoke/device.txt 2>&1
echo "device: $(tr '\n' ' ' < smoke/device.txt)"

# ---------------------------------------------------------------------------
# 1. the binary itself
# ---------------------------------------------------------------------------

adb push downloads/jnilibs-x86_64/libsimpsons-emu.so /data/local/tmp/simpsons-emu >/dev/null
adb push downloads/demo-image/demo-armv7 /data/local/tmp/demo-armv7 >/dev/null
adb shell chmod 755 /data/local/tmp/simpsons-emu
adb shell /data/local/tmp/simpsons-emu run /data/local/tmp/demo-armv7 --stats \
    > smoke/cli.txt 2>&1
echo "--- simpsons-emu run demo (on the device) ---"
tail -30 smoke/cli.txt
grep -q "hello from the guest" smoke/cli.txt && booted=yes || booted=no
check "the emulator binary boots the demo image on Android" "$booted"

# ---------------------------------------------------------------------------
# 2. the APK
# ---------------------------------------------------------------------------

apk="$(ls downloads/*.apk 2>/dev/null | head -1)"
if [ -z "$apk" ]; then
    check "the APK was downloaded" no
    cat smoke/verdict.txt
    exit 1
fi
echo "installing $apk"
adb install -r "$apk" > smoke/install.txt 2>&1
cat smoke/install.txt
grep -q "Success" smoke/install.txt && installed=yes || installed=no
check "the APK installs" "$installed"

# ---------------------------------------------------------------------------
# 3. the app
# ---------------------------------------------------------------------------

adb logcat -c || true
adb shell am start -n "$activity" --ez autostart_demo true > smoke/launch.txt 2>&1
cat smoke/launch.txt

ready=no
alive=no
for _ in $(seq 1 45); do
    sleep 2
    adb logcat -d -s SimpsonsEmu:I > smoke/logcat.txt 2>&1
    if grep -q "preview ready on 127.0.0.1" smoke/logcat.txt; then
        ready=yes
        break
    fi
done
[ -n "$(adb shell pidof "$package" | tr -d '\r\n')" ] && alive=yes || alive=no

adb logcat -d > smoke/logcat-full.txt 2>&1
adb exec-out screencap -p > smoke/screen.png 2>/dev/null || true

echo "--- app log (logcat, tag SimpsonsEmu) ---"
tail -40 smoke/logcat.txt

check "the app launches and stays up" "$alive"
check "the app runs the packaged binary and its preview answers" "$ready"
grep -q "hello from the guest" smoke/logcat.txt && guest=yes || guest=no
check "the guest's output reaches the app" "$guest"

{
    echo
    echo "Android $(head -1 smoke/device.txt), $(sed -n 2p smoke/device.txt), API 30 emulator."
    echo "Artifacts: device log, the app's log, and a screenshot of the running app."
} >> smoke/verdict.txt

echo "--- verdict ---"
cat smoke/verdict.txt
exit "$failures"
