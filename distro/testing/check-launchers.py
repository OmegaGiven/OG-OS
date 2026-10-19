#!/usr/bin/env python3
"""Static check: everything the OG-OS desktop launches is actually shipped.

A fresh install only has what packages.x86_64 (plus the depends of our own
[ogos] packages) puts on disk, and sway/systemd run programs from /usr/bin
with no ~/.local/bin on PATH. This fails when something the desktop starts
would silently do nothing there:

  1. every program the seed sway config runs (exec, exec_always, bindsym
     ... exec, including inside `sh -c '...'`) resolves to a shipped package
  2. the live-session config is the seed config verbatim plus its own block
  3. every program an app spawns (Command::new("x"), og_command("x"),
     og_bin("x"), og-bar Launch/launcher commands) is shipped, or listed in
     OPTIONAL below with the reason it may be missing
  4. no code path hardcodes ~/.local/bin without a /usr/bin fallback
  5. NOPASSWD sudoers rules only name package-installed, root-owned paths
     outside any home directory
  6. every [ogos] package in packages.x86_64 is built by build-repo.sh, its
     binaries are in workspace-apps, its .desktop/runtime files exist, and
     mimeapps.list defaults point at shipped .desktop files
  7. every workspace app binary is packaged or listed in UNPACKAGED

Run from anywhere: python3 distro/testing/check-launchers.py
Exit status 0 = clean, 1 = problems (each printed on its own line).
"""
import os
import re
import shlex
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DISTRO = ROOT / "distro"
PKGBUILDS = DISTRO / "pkgbuilds"
SKEL_SWAY = DISTRO / "airootfs/etc/skel/.config/sway/config"
LIVE_SWAY = DISTRO / "airootfs/home/liveuser/.config/sway/config"
MIMEAPPS = DISTRO / "airootfs/etc/xdg/mimeapps.list"

# Command -> official-repo package that provides it. Only commands the
# desktop actually launches belong here; an unknown command is an error
# (add it here once you've confirmed the package, e.g. `pacman -Qo $(which x)`).
COMMAND_PACKAGES = {
    # sway session
    "sway": "sway", "swaymsg": "sway", "swaynag": "sway", "swaybg": "swaybg",
    "swayidle": "swayidle", "swaylock": "swaylock", "grimshot": "sway-contrib",
    "grim": "grim", "slurp": "slurp", "wl-paste": "wl-clipboard", "wl-copy": "wl-clipboard",
    "mako": "mako", "makoctl": "mako", "notify-send": "libnotify",
    "dbus-update-activation-environment": "dbus", "systemctl": "systemd",
    "xdg-open": "xdg-utils", "update-desktop-database": "desktop-file-utils",
    "/usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1": "polkit-gnome",
    "alacritty": "alacritty", "calamares": "calamares", "sudo": "sudo",
    # audio / hardware
    "pactl": "libpulse", "parec": "libpulse", "pw-play": "pipewire", "pw-link": "pipewire",
    "pw-metadata": "pipewire", "speaker-test": "alsa-utils", "brightnessctl": "brightnessctl",
    "bluetoothctl": "bluez-utils", "blueman-manager": "blueman", "blueman-adapters": "blueman",
    "blueman-sendto": "blueman", "rfkill": "util-linux", "iwctl": "iwd",
    "lsusb": "usbutils", "lspci": "pciutils", "lpadmin": "cups", "lp": "cups",
    # base tools
    "sh": "bash", "bash": "bash", "pgrep": "procps-ng", "pkill": "procps-ng", "kill": "util-linux",
    "find": "findutils", "which": "which", "curl": "curl", "fc-match": "fontconfig",
    "python3": "python", "flatpak": "flatpak", "pacman": "pacman", "checkupdates": "pacman-contrib",
    "yay": "yay", "btop": "btop", "tailscale": "tailscale", "rclone": "rclone", "ffmpeg": "ffmpeg",
    "mpv": "mpv",
}

# Packages that are on every install without being listed in
# packages.x86_64, and which listed package pulls each in (checked against
# `pacman -Si` when this table was written; re-check if one disappears).
IMPLIED = {
    "bash": "base", "coreutils": "base", "findutils": "base", "procps-ng": "base",
    "util-linux": "base", "systemd": "base", "pciutils": "base", "iproute2": "base",
    "psmisc": "base", "dbus": "base -> systemd", "curl": "base -> pacman",
    "which": "base-devel", "fontconfig": "sway -> cairo",
    "desktop-file-utils": "lightdm-gtk-greeter -> gtk3", "librsvg": "lightdm-gtk-greeter -> gtk3",
    "libpulse": "pipewire-pulse", "pipewire": "pipewire-pulse", "libnotify": "sway-contrib",
}

# Spawned somewhere, but allowed to be missing on a fresh install: the
# feature that uses it degrades (or is platform-specific) instead of being
# a dead button/hotkey. Keep the reason honest.
OPTIONAL = {
    "nvidia-smi": "og-bar GPU module probes for it; NVIDIA proprietary driver only",
    "nmcli": "og-settings network backend probe; iwd is the shipped backend",
    "waybar": "og-settings legacy waybar support; og-bar replaced it",
    "soffice": "og-files office-document preview via LibreOffice when installed",
    "pdfsig": "og-files PDF signature check (poppler) when installed",
    "openssl": "og-files PDF signing helpers; openssl is in base on Arch anyway",
    "convert": "og-settings cursor import (imagemagick optdepend)",
    "rsvg-convert": "og-settings cursor import (librsvg optdepend)",
    "xcursorgen": "og-settings cursor import (xorg-xcursorgen optdepend)",
    "adb": "og-settings phone relay APK install (android-tools optdepend)",
    "spatial": "og-settings spatial audio; unpackaged until HRIR licensing is resolved (distro/CI.md)",
    "og-phone-audio": "og-settings phone relay; needs the Android APK, unpackaged",
    "/usr/sbin/diskutil": "og-files macOS-only volume listing",
    "/sbin/mount": "og-files macOS-only network mounts",
    "claude": "og-voice AI CLI (user-configured); og-voice is unpackaged",
}

# Workspace binaries deliberately not shipped as packages yet.
UNPACKAGED = {
    "og-voice": "needs the faster-whisper STT venv (scripts/install-stt.sh) and an AI CLI; not bound by the seed config",
    "og-phone-audio": "phone side is an Android APK that CI can't build yet",
    "tray-spike": "og-bar development spike binary",
}

# User-facing apps that must ship a .desktop file.
DESKTOP_APPS = ["og-files", "og-settings", "og-apps", "og-hotkeys", "og-wallpaper-studio",
                "og-clip", "og-notif-center"]

# Words that precede the real program (the next word is still a command).
SHELL_PREFIX = {
    "exec", "nohup", "setsid", "env", "command", "then", "else", "elif", "do", "if", "while",
    "!", "{", "(",
}
# Shell builtins/keywords: not programs, and their arguments aren't either.
SHELL_BUILTIN = {
    "fi", "done", "for", "in", "}", ")", "[", "[[", "test", "true", "false", "echo", "printf",
    "read", "cd", "sleep", "hash", "export", "set", "local", "return", "exit", "wait", "trap",
}

problems = []


def problem(msg):
    problems.append(msg)


# ── shell / sway parsing ───────────────────────────────────────────────


def shell_programs(cmd):
    """Programs a shell command line runs (first word of each simple
    command), recursing into `sh -c '...'` / `bash -c '...'` and skipping
    variable assignments, dynamic words and builtins."""
    try:
        lex = shlex.shlex(cmd, posix=True, punctuation_chars=";&|<>")
        lex.whitespace_split = True
        tokens = list(lex)
    except ValueError:
        problem(f"unparseable shell command: {cmd!r}")
        return []
    out = []
    start = True
    skip_next = False
    i = 0
    while i < len(tokens):
        t = tokens[i]
        i += 1
        if t and set(t) <= set(";&|"):
            start = True
            continue
        if t and set(t) <= set("<>"):
            skip_next = True  # redirection target
            continue
        if skip_next:
            skip_next = False
            continue
        if not start:
            continue
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", t) or t in SHELL_PREFIX:
            continue
        if t in SHELL_BUILTIN:
            start = False
            continue
        if t.startswith("$") or t.startswith('"$'):
            start = False
            continue
        if t in ("sh", "bash") and i < len(tokens) and tokens[i] == "-c":
            if i + 1 < len(tokens):
                out.extend(shell_programs(tokens[i + 1]))
            i += 2
            start = False
            continue
        out.append(t)
        start = False
        if t == "sudo":
            # the program sudo runs: first non-option word after it
            while i < len(tokens) and tokens[i].startswith("-"):
                i += 1
            start = True
    return out


def sway_exec_commands(text):
    """(line_no, shell command) for every exec in a sway config, with
    `set $var` values substituted (sway does that before running it)."""
    lines = []
    buf, first = "", None
    for n, raw in enumerate(text.splitlines(), 1):
        s = raw.strip()
        if first is None:
            first = n
        if s.endswith("\\"):
            buf += s[:-1] + " "
            continue
        lines.append((first, buf + s))
        buf, first = "", None
    variables = {}
    out = []
    for n, s in lines:
        if not s or s.startswith("#"):
            continue
        m = re.match(r"^set\s+\$(\w+)\s+(.*)$", s)
        if m:
            variables[m.group(1)] = m.group(2).strip()
            continue
        for name, val in sorted(variables.items(), key=lambda kv: -len(kv[0])):
            s = s.replace("$" + name, val)
        words = s.split()
        if not words:
            continue
        if words[0] == "bindsym":
            rest = words[1:]
            while rest and rest[0].startswith("--"):
                rest = rest[1:]
            rest = rest[1:]  # the key combo
        else:
            rest = words
        if not rest or rest[0] not in ("exec", "exec_always"):
            continue
        rest = rest[1:]
        while rest and rest[0].startswith("--"):
            rest = rest[1:]
        cmd = " ".join(rest)
        # sway hands exec a single fully-quoted argument without its quotes
        if len(cmd) > 1 and cmd[0] == cmd[-1] and cmd[0] in "\"'":
            cmd = cmd[1:-1]
        out.append((n, cmd))
    return out


def strip_test_modules(text):
    """Blank out `#[cfg(test)] mod x { ... }` bodies (keeping line numbers):
    tests spawn fixtures, not desktop programs."""
    out = list(text)
    for m in re.finditer(r"#\[cfg\((?:all\()?test[^\]]*\]\s*mod\s+\w+\s*\{", text):
        depth, i = 1, m.end()
        while i < len(text) and depth:
            depth += {"{": 1, "}": -1}.get(text[i], 0)
            i += 1
        for j in range(m.start(), i):
            if out[j] != "\n":
                out[j] = " "
    return "".join(out)


# ── package metadata ───────────────────────────────────────────────────


def read_list(path):
    return [l.strip() for l in path.read_text().splitlines()
            if l.strip() and not l.strip().startswith("#")]


def pkgbuild_info(d):
    text = (d / "PKGBUILD").read_text()

    def arr(name):
        m = re.search(rf"^{name}=\((.*?)\)", text, re.S | re.M)
        if not m:
            return []
        body = re.sub(r"#[^\n]*", "", m.group(1))
        return [re.split(r"[<>=:]", w.strip("'\""))[0] for w in re.findall(r"'[^']*'|\"[^\"]*\"|\S+", body)]

    installs = re.findall(r'"\$pkgdir(/[^"$]+)"', text)
    # `for s in a b c; do install ... "$pkgdir/usr/bin/$s"; done`
    for var, names, body in re.findall(r"for (\w+) in ([^;\n]+); do(.*?)done", text, re.S):
        for dest in re.findall(rf'"\$pkgdir(/[^"$]*)\${var}"', body):
            installs.extend(dest + n for n in names.split())
    return {
        "name": re.search(r"^pkgname=(\S+)", text, re.M).group(1),
        "depends": arr("depends"),
        "optdepends": arr("optdepends"),
        "installs": installs,
        "text": text,
    }


PKGS = {}
for d in sorted(PKGBUILDS.iterdir()):
    if (d / "PKGBUILD").is_file():
        info = pkgbuild_info(d)
        PKGS[info["name"]] = info

SHIPPED_LIST = read_list(DISTRO / "packages.x86_64")
shipped = set(SHIPPED_LIST)
for pkg, via in IMPLIED.items():
    if via.split(" ")[0] in shipped:
        shipped.add(pkg)
# own packages pull in their depends (recursively through other own packages)
frontier = [p for p in SHIPPED_LIST if p in PKGS]
while frontier:
    p = frontier.pop()
    for dep in PKGS[p]["depends"]:
        if dep not in shipped:
            shipped.add(dep)
            if dep in PKGS:
                frontier.append(dep)

# commands installed by our own packages
OWN_COMMANDS = {}
for name, info in PKGS.items():
    for path in info["installs"]:
        if path.startswith("/usr/bin/"):
            OWN_COMMANDS[path[len("/usr/bin/"):]] = name


def provider(cmd):
    """Package that ships `cmd` on a fresh install, or None."""
    base = cmd if cmd.startswith("/usr/lib/") else cmd.rsplit("/", 1)[-1] if cmd.startswith("/usr/bin/") else cmd
    for table in (OWN_COMMANDS, COMMAND_PACKAGES):
        pkg = table.get(base)
        if pkg:
            return pkg if pkg in shipped else None
    return None


def check_command(cmd, where, allow_optional=False):
    if cmd.startswith("$") or not cmd:
        return
    if ".local/bin" in cmd:
        problem(f"{where}: runs {cmd!r} from ~/.local/bin, which a fresh install doesn't have")
        return
    base = cmd if cmd.startswith("/usr/lib/") else cmd.rsplit("/", 1)[-1] if cmd.startswith("/usr/bin/") else cmd
    known = base in OWN_COMMANDS or base in COMMAND_PACKAGES
    if allow_optional and cmd in OPTIONAL:
        return
    if not known:
        problem(f"{where}: unknown command {cmd!r} — add it to COMMAND_PACKAGES (or OPTIONAL) in check-launchers.py")
        return
    if provider(cmd) is None:
        pkg = OWN_COMMANDS.get(base) or COMMAND_PACKAGES.get(base)
        problem(f"{where}: {cmd!r} comes from package {pkg!r}, which packages.x86_64 doesn't ship")


# ── 1/2: sway configs ──────────────────────────────────────────────────

skel = SKEL_SWAY.read_text()
for n, cmd in sway_exec_commands(skel):
    for prog in shell_programs(cmd):
        check_command(prog, f"{SKEL_SWAY.relative_to(ROOT)}:{n}")

live = LIVE_SWAY.read_text()
if not live.startswith(skel):
    problem(f"{LIVE_SWAY.relative_to(ROOT)}: must start with the seed config verbatim "
            f"(cat skel config + live block); it has drifted")
for n, cmd in sway_exec_commands(live[len(skel):] if live.startswith(skel) else live):
    for prog in shell_programs(cmd):
        check_command(prog, f"{LIVE_SWAY.relative_to(ROOT)} (live block)")

# ── 3: app spawn sites ─────────────────────────────────────────────────

SPAWN_PATTERNS = [
    re.compile(r'Command::new\(\s*"([^"]+)"'),
    re.compile(r'og_(?:command|bin|bin_shell)\(\s*"([^"]+)"'),
    re.compile(r'Message::Launch\(\s*"([^"]+)"'),
    re.compile(r'command:\s*"([^"]+)"\.to_string\(\)'),
]
RUST_DIRS = [ROOT / "apps", ROOT / "crates"]
for base in RUST_DIRS:
    for f in base.rglob("*.rs"):
        if "target" in f.parts or "node_modules" in f.parts:
            continue
        rel = f.relative_to(ROOT)
        text = f.read_text(errors="replace")
        # test modules spawn fixtures, not desktop programs
        text = strip_test_modules(text)
        for n, line in enumerate(text.splitlines(), 1):
            code = line.split("//")[0]
            for pat in SPAWN_PATTERNS:
                for m in pat.finditer(code):
                    words = shell_programs(m.group(1))
                    for prog in words[:1]:
                        check_command(prog, f"{rel}:{n}", allow_optional=True)

# runtime scripts shipped by packages: the programs they call by name
for name, info in PKGS.items():
    for src in re.findall(r'"\$_ogsrc/(apps/[^"]+/runtime/[^"$]+)"', info["text"]):
        p = ROOT / src
        if not p.is_file():
            problem(f"pkgbuilds/{name}: installs missing file {src}")

# ── 4: hardcoded ~/.local/bin ──────────────────────────────────────────

DEV_ONLY = re.compile(r"apps/[^/]+/scripts/install[^/]*\.sh$")
for f in list(ROOT.joinpath("apps").rglob("*")) + list(ROOT.joinpath("crates").rglob("*")) + \
        list(PKGBUILDS.rglob("*")) + [SKEL_SWAY, LIVE_SWAY]:
    if not f.is_file() or "target" in f.parts or "node_modules" in f.parts or "android" in f.parts:
        continue
    rel = str(f.relative_to(ROOT))
    if DEV_ONLY.search(rel) or rel.endswith((".md", ".png", ".wav", ".ttf", ".otf")):
        continue
    if rel == "crates/og-config/src/bin.rs":
        continue  # the resolver itself
    try:
        text = f.read_text()
    except (UnicodeDecodeError, OSError):
        continue
    if ".local/bin" not in text:
        continue
    is_rust = rel.endswith(".rs")
    if is_rust:
        text = strip_test_modules(text)
    for n, line in enumerate(text.splitlines(), 1):
        if "check-launchers: allow" in line:
            continue  # reviewed: dev-first lookup with a PATH/system fallback
        s = line.strip()
        code = s.split("//")[0] if is_rust else ("" if s.startswith(("#", "//", "*", '"""')) else s)
        if ".local/bin" not in code:
            continue
        if is_rust:
            problem(f"{rel}:{n}: hardcoded ~/.local/bin — use og_config::og_bin/og_command/og_bin_shell")
        elif "/usr/bin/" not in text:
            problem(f"{rel}:{n}: ~/.local/bin with no /usr/bin fallback in this file")

# ── 5: sudoers ─────────────────────────────────────────────────────────

all_installs = {p: n for n, i in PKGS.items() for p in i["installs"]}
sudoers_files = [ROOT / m for i in PKGS.values()
                 for m in re.findall(r'"\$_ogsrc/([^"]+)"\s+"\$pkgdir/etc/sudoers\.d/', i["text"])]
for f in sudoers_files:
    for n, line in enumerate(f.read_text().splitlines(), 1):
        if line.strip().startswith("#") or "NOPASSWD" not in line:
            continue
        for path in re.findall(r"NOPASSWD:\s*(.*)", line)[0].split(","):
            path = path.strip().split()[0]
            where = f"{f.relative_to(ROOT)}:{n}"
            if not path.startswith(("/usr/lib/", "/usr/bin/")):
                problem(f"{where}: NOPASSWD target {path} isn't under /usr/lib or /usr/bin (package-owned)")
            if path not in all_installs:
                problem(f"{where}: NOPASSWD target {path} isn't installed by any PKGBUILD")
            else:
                text = PKGS[all_installs[path]]["text"]
                m = re.search(rf'install -Dm(\d+)(.*)"\$pkgdir{re.escape(path)}"', text)
                if not m or int(m.group(1)[-2], 8) & 2 or int(m.group(1)[-1], 8) & 2 or "-o root" not in m.group(2):
                    problem(f"{where}: {path} must be installed root-owned and not group/world-writable")

# ── 6: packaging consistency ───────────────────────────────────────────

build_repo = (DISTRO / "ci/build-repo.sh").read_text()
default_pkgs = re.search(r'^DEFAULT_PACKAGES="([^"]+)"', build_repo, re.M).group(1).split()
workspace_apps = set(read_list(PKGBUILDS / "workspace-apps"))
for p in SHIPPED_LIST:
    if p in PKGS and p not in default_pkgs:
        problem(f"packages.x86_64 ships {p} but ci/build-repo.sh DEFAULT_PACKAGES doesn't build it")
for name, info in PKGS.items():
    for m in re.finditer(r'release/([\w-]+)"', info["text"]):
        if "mapfile" in info["text"] and m.group(1) not in workspace_apps:
            problem(f"pkgbuilds/{name}: installs release/{m.group(1)} but workspace-apps doesn't build it")
    for src in re.findall(r'"\$_ogsrc/([^"$]+)"', info["text"]):
        if not (ROOT / src).exists():
            problem(f"pkgbuilds/{name}: source file {src} doesn't exist")

desktop_ids = {}
for name, info in PKGS.items():
    for path in info["installs"]:
        if path.startswith("/usr/share/applications/") and path.endswith(".desktop"):
            desktop_ids[path.rsplit("/", 1)[1]] = name
for app in DESKTOP_APPS:
    did = f"{app}.desktop"
    if did not in desktop_ids:
        problem(f"{app}: no PKGBUILD installs /usr/share/applications/{did}")
    elif desktop_ids[did] not in shipped:
        problem(f"{did}: its package {desktop_ids[did]} isn't in packages.x86_64")
for name, info in PKGS.items():
    for src, dest in re.findall(r'"\$_ogsrc/([^"]+\.desktop)"\s+"\$pkgdir(/[^"]+)"', info["text"]):
        text = (ROOT / src).read_text() if (ROOT / src).exists() else ""
        m = re.search(r"^Exec=(.*)$", text, re.M)
        if not m:
            problem(f"{src}: no Exec= line")
            continue
        prog = shlex.split(re.sub(r"%[fFuUick]", "", m.group(1)))[0]
        check_command(prog, f"{src} Exec=")

if not MIMEAPPS.is_file():
    problem(f"{MIMEAPPS.relative_to(ROOT)}: missing — no default file manager/viewers for xdg-open")
for n, line in enumerate(MIMEAPPS.read_text().splitlines() if MIMEAPPS.is_file() else [], 1):
    if "=" in line and not line.startswith("#"):
        for did in line.split("=", 1)[1].split(";"):
            if did and did not in desktop_ids:
                problem(f"{MIMEAPPS.relative_to(ROOT)}:{n}: default {did} isn't installed by any PKGBUILD")
            elif did and desktop_ids[did] not in shipped:
                problem(f"{MIMEAPPS.relative_to(ROOT)}:{n}: {did}'s package isn't shipped")

# ── 7: every workspace binary accounted for ────────────────────────────

for cargo in ROOT.joinpath("apps").glob("*/Cargo.toml"):
    for b in re.findall(r'\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"', cargo.read_text()):
        if b not in OWN_COMMANDS and b not in UNPACKAGED:
            problem(f"{cargo.relative_to(ROOT)}: binary {b} is neither packaged nor listed in UNPACKAGED")

if problems:
    for p in problems:
        print(f"FAIL {p}")
    print(f"\n{len(problems)} problem(s)")
    sys.exit(1)
print(f"OK: seed sway config, live config, app spawn sites, .desktop files, mime defaults "
      f"and sudoers all resolve to shipped packages ({len(shipped)} packages, "
      f"{len(OWN_COMMANDS)} own commands)")
