#!/usr/bin/env bash
# Builds og-phone-audio + the phone APK and installs them. The service is
# installed but not enabled — og-settings' Audio → Phone toggle does that.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release

bin="../../target/release/og-phone-audio"
dest="$HOME/.local/bin/og-phone-audio"
cp "$bin" "${dest}.new"
mv -f "${dest}.new" "$dest"
echo "Installed $dest"

unit_dir="$HOME/.config/systemd/user"
mkdir -p "$unit_dir"
cp scripts/og-phone-audio.service "$unit_dir/og-phone-audio.service"
systemctl --user daemon-reload
echo "Installed $unit_dir/og-phone-audio.service"

# Pick up the new binary if it's already running.
if systemctl --user is-active --quiet og-phone-audio; then
    systemctl --user restart og-phone-audio
fi

if scripts/build-apk.sh; then
    share="$HOME/.local/share/og-phone-audio"
    mkdir -p "$share"
    cp target/og-phone-audio.apk "$share/og-phone-audio.apk.new"
    mv -f "$share/og-phone-audio.apk.new" "$share/og-phone-audio.apk"
    echo "Installed $share/og-phone-audio.apk"
else
    echo "APK build failed (Android SDK missing?) — PC side installed anyway." >&2
fi
