#!/usr/bin/env python3
"""Draws the OG Files icon and writes every size the platforms use.

The icon: a folder in off-white outline on near-black, in the same style as
the OG TestDesk and OG Paper icons.

Needs rsvg-convert (librsvg) and ImageMagick. Run from apps/og-files:
    python3 packaging/make-icons.py

Writes:
  packaging/icons/og-files.svg     rounded tile (Linux, window icon source)
  packaging/icons/og-files.png     512 px tile (Linux, AppImage, window icon)
  packaging/icons/og-files.ico     16-256 px tiles (Windows exe + installer)
  packaging/icons/og-files.icns    macOS plate, 16-1024 px (direct-download .app)
  packaging/macos/icon-mac-1024.png  macOS plate for the App Store asset catalog
  packaging/macos/icon-1024.png    square, no transparency (App Store listing)
"""
import os
import struct
import subprocess
import tempfile

BG, FG = "#16181d", "#eef0e8"
W = 60  # line width on the 1024 grid


def art():
    """The icon's lines on a 1024 grid (no background)."""
    # Folder: a tab on the top-left, the body below, and the front edge.
    x0, x1 = 186, 838
    top, body, bottom = 252, 338, 790
    tab_end, slope = 432, 486
    r = 52
    outline = (
        f"M {x0} {top + r} Q {x0} {top} {x0 + r} {top} "
        f"L {tab_end} {top} L {slope} {body} "
        f"L {x1 - r} {body} Q {x1} {body} {x1} {body + r} "
        f"L {x1} {bottom - r} Q {x1} {bottom} {x1 - r} {bottom} "
        f"L {x0 + r} {bottom} Q {x0} {bottom} {x0} {bottom - r} Z"
    )
    front = 440
    return (
        f'<path d="{outline}" fill="none" stroke="{FG}" stroke-width="{W}" stroke-linejoin="round"/>'
        f'<path d="M {x0} {front} L {x1} {front}" stroke="{FG}" stroke-width="{W}" stroke-linecap="round"/>'
    )


def svg(kind):
    """full: square, edge to edge (stores round it themselves).
    tile: rounded square with transparent corners (Linux, Windows).
    mac: the macOS plate (824 in a 1024 frame, with a soft shadow)."""
    head = '<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">'
    if kind == "full":
        return f'{head}<rect width="1024" height="1024" fill="{BG}"/>{art()}</svg>'
    if kind == "tile":
        return f'{head}<rect width="1024" height="1024" rx="225" fill="{BG}"/>{art()}</svg>'
    k = 824 / 1024
    return (f'{head}<defs><filter id="s" x="-10%" y="-10%" width="120%" height="125%">'
            f'<feDropShadow dx="0" dy="10" stdDeviation="12" flood-opacity="0.35"/></filter></defs>'
            f'<rect x="100" y="100" width="824" height="824" rx="185" fill="{BG}" filter="url(#s)"/>'
            f'<g transform="translate(100 100) scale({k})">{art()}</g></svg>')


def render(kind, size, out, flatten=False):
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as f:
        f.write(svg(kind))
    subprocess.run(["rsvg-convert", "-w", str(size), "-h", str(size), f.name, "-o", out], check=True)
    os.unlink(f.name)
    if flatten:
        # App Store icons must have no transparency.
        subprocess.run(["magick", out, "-background", BG, "-alpha", "remove", "-alpha", "off", f"PNG24:{out}"], check=True)


def write_icns(out, tmp):
    """An .icns is a header plus one PNG per size, tagged by OSType."""
    types = [("icp4", 16), ("icp5", 32), ("ic07", 128), ("ic08", 256), ("ic09", 512),
             ("ic10", 1024), ("ic11", 32), ("ic12", 64), ("ic13", 256), ("ic14", 512)]
    chunks = b""
    for ostype, size in types:
        png = f"{tmp}/mac-{size}.png"
        if not os.path.exists(png):
            render("mac", size, png)
        data = open(png, "rb").read()
        chunks += ostype.encode() + struct.pack(">I", len(data) + 8) + data
    open(out, "wb").write(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)


if __name__ == "__main__":
    open("packaging/icons/og-files.svg", "w").write(svg("tile") + "\n")
    render("tile", 512, "packaging/icons/og-files.png")
    render("mac", 1024, "packaging/macos/icon-mac-1024.png")
    render("full", 1024, "packaging/macos/icon-1024.png", flatten=True)
    with tempfile.TemporaryDirectory() as tmp:
        sizes = (16, 24, 32, 48, 64, 128, 256)
        for s in sizes:
            render("tile", s, f"{tmp}/{s}.png")
        subprocess.run(["magick", *[f"{tmp}/{s}.png" for s in sizes], "packaging/icons/og-files.ico"], check=True)
        write_icns("packaging/icons/og-files.icns", tmp)
