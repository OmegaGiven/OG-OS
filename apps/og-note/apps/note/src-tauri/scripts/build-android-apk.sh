#!/usr/bin/env bash
# Build a signed, installable release APK for OG Note.
#
# First run generates apps/note/src-tauri/keystore/og-note-release.jks —
# back that file (and its .password sibling) up somewhere safe. Every
# future release must be signed with the same key or Android will refuse
# to install it as an update over an existing install.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
KEYSTORE_DIR="$APP_DIR/src-tauri/keystore"
KEYSTORE="$KEYSTORE_DIR/og-note-release.jks"
PASSWORD_FILE="$KEYSTORE_DIR/og-note-release.password"
KEY_ALIAS="og-note"

: "${ANDROID_HOME:=/opt/android-sdk}"
: "${ANDROID_SDK_ROOT:=$ANDROID_HOME}"
: "${JAVA_HOME:=/usr/lib/jvm/java-17-openjdk}"
NDK_VERSION="$(ls "$ANDROID_HOME/ndk" | sort -V | tail -1)"
: "${NDK_HOME:=$ANDROID_HOME/ndk/$NDK_VERSION}"
BUILD_TOOLS_VERSION="$(ls "$ANDROID_HOME/build-tools" | sort -V | tail -1)"
BUILD_TOOLS="$ANDROID_HOME/build-tools/$BUILD_TOOLS_VERSION"
TARGET="${1:-aarch64}"

export ANDROID_HOME ANDROID_SDK_ROOT JAVA_HOME NDK_HOME

mkdir -p "$KEYSTORE_DIR"
if [[ ! -f "$KEYSTORE" ]]; then
  echo "No release keystore found — generating one at $KEYSTORE"
  PASS="$(openssl rand -base64 24 | tr -d '/+=' | head -c 24)"
  keytool -genkeypair -v \
    -keystore "$KEYSTORE" \
    -alias "$KEY_ALIAS" \
    -keyalg RSA -keysize 2048 -validity 10000 \
    -storepass "$PASS" -keypass "$PASS" \
    -dname "CN=OmegaGiven, OU=OG Note, O=OmegaGiven, L=Unknown, S=Unknown, C=US"
  echo "$PASS" > "$PASSWORD_FILE"
  chmod 600 "$KEYSTORE" "$PASSWORD_FILE"
  echo "Keystore password saved to $PASSWORD_FILE — back this up, it is not in git."
fi
PASS="$(cat "$PASSWORD_FILE")"

cd "$APP_DIR"
npx tauri android build --apk --target "$TARGET"

VERSION="$(node -p "require('$APP_DIR/package.json').version")"

UNSIGNED="$APP_DIR/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk"
OUT_DIR="$APP_DIR/src-tauri/gen/android/app/build/outputs/apk/universal/release"
ALIGNED="$OUT_DIR/app-universal-release-aligned.apk"
SIGNED="$OUT_DIR/og-note-v$VERSION-release-signed.apk"

"$BUILD_TOOLS/zipalign" -f -p 4 "$UNSIGNED" "$ALIGNED"
"$BUILD_TOOLS/apksigner" sign \
  --ks "$KEYSTORE" --ks-key-alias "$KEY_ALIAS" \
  --ks-pass "pass:$PASS" --key-pass "pass:$PASS" \
  --out "$SIGNED" "$ALIGNED"
"$BUILD_TOOLS/apksigner" verify "$SIGNED"

echo "Signed APK ready: $SIGNED"
