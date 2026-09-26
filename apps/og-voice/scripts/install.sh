#!/usr/bin/env bash
# Builds og-voice and installs it to ~/.local/bin, replacing the binary
# atomically (a plain cp fails/corrupts mid-write while the old one is
# running — see the toolkit's install.sh convention).
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release

bin="../../target/release/og-voice"
dest="$HOME/.local/bin/og-voice"
tmp="${dest}.new"

cp "$bin" "$tmp"
mv -f "$tmp" "$dest"
echo "Installed $dest"

launch_src="scripts/og-voice-launch"
launch_dest="$HOME/.local/bin/og-voice-launch"
launch_tmp="${launch_dest}.new"

cp "$launch_src" "$launch_tmp"
chmod +x "$launch_tmp"
mv -f "$launch_tmp" "$launch_dest"
echo "Installed $launch_dest"

echo
echo "Rust binary + launch wrapper installed. The STT server is a separate"
echo "step — run scripts/install-stt.sh once to set up its Python venv."
echo
echo "Add these to ~/.config/sway/config (not done automatically):"
echo '  bindsym $mod+v exec ~/.local/bin/og-voice-launch'
echo '  bindsym --release $mod+v exec ~/.local/bin/og-voice --stop'
