# OG File Manager

A fast, keyboard- and mouse-friendly file manager built with [iced](https://github.com/iced-rs/iced), for Linux.

## Features

- Grid and list views, sortable by name/size/modified/kind, with optional folders-first grouping
- Right-click context menu: cut/copy/paste, rename, duplicate, delete (trash or permanent), compress/extract, open with..., bookmarks
- Sidebar: places, mounted devices with usage bars, network drives, recent locations, custom bookmarks
- Background recursive search (instant results in the current folder, then keeps searching subfolders)
- File preview panel: metadata (size, dates, permissions, owner), EXIF data for photos, and image/video thumbnails generated in the background
- Resizable sidebar and preview panel
- Spawns a terminal (Alacritty or foot) themed to match the app

## Requirements

- Linux (uses `/proc/mounts` and POSIX permission APIs — won't build/run on macOS or Windows as-is)
- Rust (install via [rustup](https://rustup.rs))
- A Wayland or X11 desktop session
- **Symbols Nerd Font** installed system-wide — without it, toolbar/sidebar icons render as tofu boxes. Get it from [Nerd Fonts](https://www.nerdfonts.com/font-downloads) (the "Symbols Nerd Font" / "Symbols Only" variant is enough) and install it to `~/.local/share/fonts` or `/usr/share/fonts`, then `fc-cache -f`.

Optional, for extra features (each degrades gracefully if missing):

| Tool | Used for |
|---|---|
| `ffmpeg` | video thumbnails in the preview panel |
| `zip`, `unzip`, `tar` | compress/extract from the right-click menu |
| `alacritty` or `foot` | "Open Terminal Here" / "Open in Terminal" |
| `xdg-open`, `xdg-mime` | default app / "Open With" / setting a default app |

## Build & Install

```sh
git clone https://github.com/OmegaGiven/OG-OS.git
cd OG-OS
cargo build --release -p og-files
```

Copy the binary somewhere on your `$PATH`, e.g.:

```sh
mkdir -p ~/.local/bin
cp target/release/file-manager ~/.local/bin/file-manager
```

Make sure `~/.local/bin` is on your `PATH` (add `export PATH="$HOME/.local/bin:$PATH"` to your shell rc file if not), then run:

```sh
file-manager
```

### Updating

The binary can be replaced while a previous instance is still running (Linux keeps the old one executing from its inode), but if a straight `cp` gives you `Text file busy`, use a rename instead:

```sh
cargo build --release -p og-files
cp target/release/file-manager ~/.local/bin/file-manager.new
mv ~/.local/bin/file-manager.new ~/.local/bin/file-manager
```

Then fully quit and relaunch the app to pick up the new binary.

## Theming

If `~/.config/sway-power/config.json` exists, the app reads its `bar_bg`, `sec_bg`, `bar_text`, and `accent` keys and uses them for its own color scheme, and for theming spawned terminals (Alacritty/foot). Without that file, it falls back to a built-in dark theme.

## Storage

- Recent locations: `~/.config/file-manager/recent.json`
- Custom sidebar bookmarks: `~/.config/file-manager/bookmarks.json`
- Cached image/video previews: `~/.cache/file-manager/previews/`
- Generated terminal theme (Alacritty): `~/.cache/file-manager/alacritty-theme.toml`
