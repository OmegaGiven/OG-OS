#!/bin/sh
# Installs OG Files from this folder for the current user (no root):
# binary to ~/.local/bin, menu entry and icon to ~/.local/share.
#   ./install.sh            install
#   ./install.sh --uninstall
set -eu
cd "$(dirname "$0")"
BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/og-files" "$DATA/applications/og-files.desktop" \
        "$DATA/icons/hicolor/512x512/apps/og-files.png" "$DATA/icons/hicolor/scalable/apps/og-files.svg"
    echo "OG Files removed."
    exit 0
fi
install -Dm755 og-files "$BIN/og-files"
install -Dm644 og-files.desktop "$DATA/applications/og-files.desktop"
install -Dm644 og-files.png "$DATA/icons/hicolor/512x512/apps/og-files.png"
install -Dm644 og-files.svg "$DATA/icons/hicolor/scalable/apps/og-files.svg"
command -v update-desktop-database >/dev/null && update-desktop-database "$DATA/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$DATA/icons/hicolor" 2>/dev/null || true
echo "OG Files installed to $BIN/og-files"
case ":$PATH:" in *":$BIN:"*) ;; *) echo "Note: $BIN is not on your PATH." ;; esac
command -v mpv >/dev/null || ldconfig -p 2>/dev/null | grep -q 'libmpv\.so\.2' || \
    echo "Tip: install libmpv (mpv) for video playback."
