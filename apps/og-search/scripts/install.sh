#!/usr/bin/env bash
# Builds og-search and installs it to ~/.local/bin, replacing the binary
# atomically (a plain cp fails with ETXTBSY while the old one is running).
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release

bin="../../target/release/og-search"
dest="$HOME/.local/bin/og-search"
tmp="${dest}.new"

cp "$bin" "$tmp"
mv -f "$tmp" "$dest"

echo "Installed $dest"
