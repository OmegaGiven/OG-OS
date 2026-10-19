#!/usr/bin/env bash
# Runtime check of the seed sway config, without an ISO or a VM: starts a
# private headless sway on a fresh-install-like HOME (no ~/.local/bin),
# fires every startup exec and every `bindsym ... exec` through sway
# itself, and verifies each one reached the program it names.
#
# Every program the config can run is replaced by a logging stub in a
# throwaway bin dir placed first on PATH — standing in for /usr/bin on an
# installed system — so nothing real is launched and nothing touches the
# calling user's session: own XDG_RUNTIME_DIR, no D-Bus session address,
# and `pkill`/`systemctl`/`dbus-update-activation-environment` are stubs
# too (a real `pkill -x og-wallpaper` would hit the caller's processes).
#
# Usage: distro/testing/headless-launch-test.sh [sway-config]
#        (default: distro/airootfs/etc/skel/.config/sway/config)
# Needs: sway, swaymsg, python3. Not run in CI (needs a working sway).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CONFIG="${1:-$ROOT/distro/airootfs/etc/skel/.config/sway/config}"
T="$(mktemp -d "${TMPDIR:-/tmp}/og-launch-test.XXXXXX")"
SWAY_PID=""
cleanup() {
    [ -n "$SWAY_PID" ] && kill "$SWAY_PID" 2>/dev/null || true
    rm -rf "$T"
}
trap cleanup EXIT

mkdir -p "$T/bin" "$T/home/.config/sway" "$T/run"
chmod 700 "$T/run"
cp "$CONFIG" "$T/home/.config/sway/config"
LOG="$T/calls.log"
: > "$LOG"

# Programs the config runs, as sway would run them (variables expanded).
mapfile -t PROGRAMS < <(python3 - "$CONFIG" <<'PY'
import re, shlex, sys
text = open(sys.argv[1]).read().replace("\\\n", " ")
vars_, progs = {}, set()
for line in text.splitlines():
    s = line.strip()
    m = re.match(r"set \$(\w+) (.*)", s)
    if m:
        vars_[m.group(1)] = m.group(2)
        continue
    for k, v in sorted(vars_.items(), key=lambda kv: -len(kv[0])):
        s = s.replace("$" + k, v)
    for w in re.findall(r"[A-Za-z0-9_./+-]+", s):
        if re.match(r"^(og-[\w-]+|mako|wl-paste|alacritty|grimshot|pactl|brightnessctl|swaylock|swaynag|pkill|pgrep|systemctl|dbus-update-activation-environment|calamares|sudo)$", w):
            progs.add(w)
print("\n".join(sorted(progs)))
PY
)
for p in "${PROGRAMS[@]}"; do
    printf '#!/bin/sh\necho "%s $*" >> "%s"\n' "$p" "$LOG" > "$T/bin/$p"
    chmod +x "$T/bin/$p"
done

export HOME="$T/home" XDG_RUNTIME_DIR="$T/run" XDG_CONFIG_HOME="$T/home/.config"
export PATH="$T/bin:/usr/bin" WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1
unset DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY SWAYSOCK DISPLAY I3SOCK

sway -c "$HOME/.config/sway/config" > "$T/sway.log" 2>&1 &
SWAY_PID=$!
for _ in $(seq 50); do
    SOCK="$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null | head -1 || true)"
    [ -n "$SOCK" ] && break
    sleep 0.1
done
[ -n "${SOCK:-}" ] || { echo "FAIL sway didn't start:"; cat "$T/sway.log"; exit 1; }
export SWAYSOCK="$SOCK"
sleep 1.5   # startup execs

# Fire every keybinding's exec through sway (same sh -c path a key press takes).
python3 - "$CONFIG" <<'PY' > "$T/binds.txt"
import re, sys
text = open(sys.argv[1]).read().replace("\\\n", " ")
vars_ = {}
for line in text.splitlines():
    s = line.strip()
    m = re.match(r"set \$(\w+) (.*)", s)
    if m:
        vars_[m.group(1)] = m.group(2)
        continue
    w = s.split()
    if not w or w[0] != "bindsym":
        continue
    w = w[1:]
    while w and w[0].startswith("--"):
        w = w[1:]
    if len(w) > 2 and w[1] == "exec":
        cmd = " ".join(w[2:])
        for k, v in sorted(vars_.items(), key=lambda kv: -len(kv[0])):
            cmd = cmd.replace("$" + k, v)
        print(f"{w[0]}\t{cmd}")
PY
while IFS=$'\t' read -r key cmd; do
    swaymsg exec "$cmd" > /dev/null
done < "$T/binds.txt"
sleep 1.5

fail=0
check() {  # $1 = what, $2 = program that must have been called
    if grep -q "^$2\( \|\$\)" "$LOG"; then
        echo "ok   $1 -> $2"
    else
        echo "FAIL $1 -> $2 never ran"; fail=1
    fi
}
check "startup" mako
check "startup" og-notify
check "startup" og-power-apply
check "startup (og-bar unit restart)" systemctl
check "startup (wallpaper)" og-wallpaper
check "startup (clipboard watcher)" wl-paste
while IFS=$'\t' read -r key cmd; do
    prog="$(python3 -c 'import shlex,sys; w=shlex.split(sys.argv[1]); print(w[0])' "$cmd")"
    # swaymsg-guarded launchers: the real program is after `||`
    case "$cmd" in *"|| "*) prog="${cmd##*|| }"; prog="${prog%% *}";; esac
    [ "$prog" = swaymsg ] && continue
    [ "$prog" = sh ] && prog=sudo   # live config: sh -c 'sudo env ... calamares'
    check "$key" "$prog"
done < "$T/binds.txt"

if grep -qiE "error|failed" "$T/sway.log"; then
    echo "--- sway log errors:"; grep -iE "error|failed" "$T/sway.log" | grep -v 'polkit-gnome' | head -20 || true
fi
if [ -n "${VERBOSE:-}" ]; then echo "--- calls:"; cat "$LOG"; echo "--- sway log:"; cat "$T/sway.log"; fi
[ "$fail" -eq 0 ] && echo "PASS: every startup exec and keybinding reached its program with no ~/.local/bin"
exit "$fail"
