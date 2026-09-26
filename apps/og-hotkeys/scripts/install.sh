#!/usr/bin/env bash
# Builds og-hotkeys and installs it to ~/.local/bin, replacing the binary
# atomically (a plain cp fails with ETXTBSY while the old one is running).
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release

bin="../../target/release/og-hotkeys"
dest="$HOME/.local/bin/og-hotkeys"
tmp="${dest}.new"

cp "$bin" "$tmp"
mv -f "$tmp" "$dest"

echo "Installed $dest"
