#!/usr/bin/env bash
# Builds the phone app with the bare Android SDK tools (aapt2 + javac + d8 +
# apksigner) — no Gradle, nothing downloaded. Output: target/og-phone-audio.apk
set -euo pipefail

cd "$(dirname "$0")/../android"
SDK="${ANDROID_HOME:-/opt/android-sdk}"
BT="$(ls -d "$SDK"/build-tools/* | sort -V | tail -1)"
PLATFORM="$(ls -d "$SDK"/platforms/android-* | sort -V | tail -1)/android.jar"
OUT="../target"
KEYSTORE="${OG_APK_KEYSTORE:-$HOME/.android/debug.keystore}"

rm -rf "$OUT/apk" && mkdir -p "$OUT/apk/classes" "$OUT/apk/dex"

"$BT/aapt2" link -o "$OUT/apk/base.apk" -I "$PLATFORM" \
    --manifest AndroidManifest.xml \
    --min-sdk-version 29 --target-sdk-version 34 \
    --version-code "$(date +%s | cut -c1-9)" --version-name 0.1

javac -source 8 -target 8 -Xlint:-options -bootclasspath "$PLATFORM" -classpath "$BT/core-lambda-stubs.jar" \
    -d "$OUT/apk/classes" $(find src -name '*.java')

"$BT/d8" --release --min-api 29 --lib "$PLATFORM" \
    --output "$OUT/apk/dex" $(find "$OUT/apk/classes" -name '*.class')

( cd "$OUT/apk/dex" && "$BT/aapt" add ../base.apk classes.dex >/dev/null )

if [ ! -f "$KEYSTORE" ]; then
    mkdir -p "$(dirname "$KEYSTORE")"
    keytool -genkeypair -keystore "$KEYSTORE" -storepass android -keypass android \
        -alias androiddebugkey -keyalg RSA -keysize 2048 -validity 10000 \
        -dname "CN=Android Debug,O=Android,C=US" >/dev/null
fi

"$BT/zipalign" -f 4 "$OUT/apk/base.apk" "$OUT/apk/aligned.apk"
"$BT/apksigner" sign --ks "$KEYSTORE" --ks-pass pass:android --key-pass pass:android \
    --out "$OUT/og-phone-audio.apk" "$OUT/apk/aligned.apk"

echo "Built $(cd "$OUT" && pwd)/og-phone-audio.apk"
