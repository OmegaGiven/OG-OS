#!/usr/bin/env bash
# Builds og-settings and installs it to ~/.local/bin, replacing the binary
# atomically (a plain cp fails with ETXTBSY while the old one is running).
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release

bin="../../target/release/og-settings"
dest="$HOME/.local/bin/og-settings"
tmp="${dest}.new"

cp "$bin" "$tmp"
mv -f "$tmp" "$dest"

echo "Installed $dest"

for script in runtime/og-power-apply runtime/og-controller-idle-watch; do
    name="$(basename "$script")"
    dest="$HOME/.local/bin/$name"
    tmp="${dest}.new"
    cp "$script" "$tmp"
    chmod +x "$tmp"
    mv -f "$tmp" "$dest"
    echo "Installed $dest"
done
