#!/usr/bin/env python3
"""Export apps/og-files from the OG-OS monorepo into the standalone public
repo (github.com/OmegaGiven/og-files).

    packaging/export-public.py DEST        # DEST = a clone of the public repo

- copies only git-tracked files of apps/og-files (never build output or
  untracked local files), replacing everything in DEST except .git
- rewrites Cargo.toml for a standalone build: drops the OG-OS-only
  `og-suite` feature and its path dependencies (code behind
  cfg(feature = "og-suite") simply compiles out)
- writes a pruned Cargo.lock derived from the monorepo's, so the public
  build uses the exact dependency versions tested here
- refuses to export if a secret-looking string is found
"""
import os, re, shutil, subprocess, sys
from pathlib import Path

APP = Path(__file__).resolve().parent.parent
ROOT = Path(subprocess.check_output(["git", "-C", str(APP), "rev-parse", "--show-toplevel"], text=True).strip())
SELF = "packaging/export-public.py"
SUITE_DEPS = ("og-config", "og-theme", "og-wayland")

# strings that must never be published (real tailnet / credentials / home
# paths). Personal e-mail addresses are caught generically below instead of
# being listed here, so this file itself holds nothing personal.
BLOCK = [
    r"tail[0-9a-f]{6}\.ts\.net",  # real tailnet suffixes (tests use tail1234)
    r"gh[pousr]_[A-Za-z0-9]{20,}", r"sk-[A-Za-z0-9]{20,}",
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
    r"/home/omegagiven",
]
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
EMAIL_OK = re.compile(
    r"(?:^|\.|@)example\.(?:com|org|net)$|\.(?:local|lan|test|invalid|internal)$"
    r"|@users\.noreply\.github\.com$|@anthropic\.com$|^noreply@"
)


def tracked():
    out = subprocess.check_output(["git", "-C", str(APP), "ls-files", "-z", "."], text=True)
    return [p for p in out.split("\0") if p]


def scan(files):
    bad = []
    rx = [re.compile(b) for b in BLOCK]
    for f in files:
        if f == SELF:
            continue  # holds the block list itself; never exported
        try:
            text = (APP / f).read_text(errors="ignore")
        except IsADirectoryError:
            continue
        binary = "\0" in text[:8192]
        # vendored upstream crates carry their authors' published addresses
        vendored = f.startswith("third_party/")
        for m in ([] if binary or vendored else EMAIL.finditer(text)):
            # user@host inside a URL (sftp://u@host) isn't an address
            if text[max(0, m.start() - 3) : m.start()].endswith("//"):
                continue
            if not EMAIL_OK.search(m.group(0)):
                line = text.count("\n", 0, m.start()) + 1
                bad.append(f"{f}:{line}: e-mail address {m.group(0)}")
        for r in rx:
            for m in r.finditer(text):
                if "tail1234" in m.group(0):
                    continue
                line = text.count("\n", 0, m.start()) + 1
                bad.append(f"{f}:{line}: {m.group(0)}")
    return bad


def standalone_toml(src: str) -> str:
    out, skip_feature_block = [], False
    for line in src.splitlines():
        name = line.split("=", 1)[0].strip()
        if name in SUITE_DEPS and "path" in line:
            continue  # OG-OS path dependency
        if line.strip().startswith("og-suite") and "dep:" in line:
            continue
        if line.strip().startswith("default") and "og-suite" in line:
            feats = re.findall(r'"([^"]+)"', line)
            # standalone default is portable: no og-suite, and no build-time
            # ffmpeg (video plays through libmpv when installed)
            feats = [f for f in feats if f not in ("og-suite", "ffmpeg")]
            line = "default = [" + ", ".join(f'"{f}"' for f in feats) + "]"
        out.append(line)
    text = "\n".join(out) + "\n"
    # the monorepo patches winit at the workspace root; standalone, here
    text += (
        "\n[patch.crates-io]\n"
        "# winit without its private macOS window-blur API (Mac App Store)\n"
        'winit = { path = "third_party/winit" }\n'
    )
    # og-suite stays a known cfg so the gated code compiles out cleanly
    text += (
        "\n[lints.rust]\n"
        "unexpected_cfgs = { level = \"warn\", check-cfg = ['cfg(feature, values(\"og-suite\"))'] }\n"
    )
    return text


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    dest = Path(sys.argv[1]).resolve()
    files = tracked()
    bad = scan(files)
    if bad:
        sys.exit("refusing to export, sensitive-looking strings:\n  " + "\n  ".join(bad))
    dest.mkdir(parents=True, exist_ok=True)
    for entry in dest.iterdir():
        if entry.name == ".git":
            continue
        shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()
    for f in files:
        if f == SELF:
            continue  # monorepo tooling
        d = dest / f
        d.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(APP / f, d)
    cargo = dest / "Cargo.toml"
    cargo.write_text(standalone_toml(cargo.read_text()))
    shutil.copy2(ROOT / "Cargo.lock", dest / "Cargo.lock")
    # resolve against the copied lock: drops unused packages,
    # keeps every version that's shared
    subprocess.check_call(["cargo", "metadata", "--format-version", "1",
                           "--manifest-path", str(cargo)], stdout=subprocess.DEVNULL)
    print(f"exported {len(files)} files to {dest}")


if __name__ == "__main__":
    main()
