#!/usr/bin/env bash
# Sets up the local STT server: a Python venv under
# ~/.local/share/og-voice/stt-venv with faster-whisper installed, plus a
# copy of stt_server.py the Rust app can find and spawn on demand (see
# src/stt.rs's ensure_running — it looks for exactly these two paths).
#
# Run once (and again after editing stt-server/stt_server.py, to pick up
# the change — the copy under ~/.local/share is what actually runs).
set -euo pipefail

cd "$(dirname "$0")/.."

install_dir="$HOME/.local/share/og-voice"
venv_dir="$install_dir/stt-venv"

mkdir -p "$install_dir"
cp stt-server/stt_server.py "$install_dir/stt_server.py"

if [ ! -d "$venv_dir" ]; then
    echo "Creating venv at $venv_dir…"
    python3 -m venv "$venv_dir"
fi

echo "Installing faster-whisper into the venv (this downloads a model on first run of the server, not here)…"
"$venv_dir/bin/pip" install --upgrade pip >/dev/null
"$venv_dir/bin/pip" install -r stt-server/requirements.txt

echo
echo "STT server installed. og-voice will spawn it on demand the first time"
echo "you hold the push-to-talk hotkey and it isn't already running (see"
echo "src/stt.rs::ensure_running). To start/test it manually instead:"
echo
echo "  $venv_dir/bin/python3 $install_dir/stt_server.py"
echo
echo "First run downloads the whisper model (~small.en by default, a few"
echo "hundred MB) — that happens once, cached under ~/.cache/huggingface."
