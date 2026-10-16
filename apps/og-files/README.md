# OG Files

A file manager whose right side is a set of tabbed viewers and editors, so
browsing a folder and working on what's in it happen in one window. Written
in Rust with [iced](https://iced.rs) (wgpu). Runs on Linux, macOS and
Windows.

<!-- screenshots: add docs/screenshot-*.png here -->
_Screenshots coming soon._

## Features

- **Files pane:** tree, list and grid (thumbnails) views, kind filter, live
  background search, focus mode, bookmarks, drag and drop, type-ahead,
  copy / move / rename / trash with progress, conflicts, cancel and undo.
- **Viewers and editors** (all in-process, no helper programs):
  - text and code (syntax colours, edit mode, atomic save)
  - images (fit, pan / zoom)
  - tables: CSV / TSV / XLSX / XLS / ODS (edit cells, save)
  - PDF: fast renderer, page editing, and a 55-tool panel (merge, split,
    watermark, redact, sign, OCR, compress, PDF/A, convert to and from
    Office / HTML / Markdown / EPUB…) — Stirling-PDF's feature set, all
    built in, nothing else to install
  - Word (.docx) editor with lossless round-trip; ODT / RTF / EPUB reader
  - audio (waveform, tags, trim / normalize)
  - video (libmpv: GPU decoding, subtitles, tracks, chapters, A-B loop,
    trim export and frame snapshots)
  - 3D models (glTF / OBJ / STL / PLY), archives as folders
    (zip / tar.* / 7z: browse, extract, compress), hex view, disk usage
    analyzer
- **Open with**, **open terminal here**, **drives** and **network
  storage** (SFTP / SMB / WebDAV / FTP through rclone, Tailscale peers,
  `~/.ssh/config` hosts).

See [SPEC.md](SPEC.md) for the full design and the
[platform support](SPEC.md#11-platform-support) table for what works where.

## Download

Release builds bundle libmpv, so video works out of the box:

| OS | File |
|---|---|
| Linux (any distro) | `OG_Files-<ver>-<arch>.AppImage` |
| Debian / Ubuntu | `og-files_<ver>_<amd64\|arm64>.deb` (uses the system `libmpv2`) |
| macOS (Apple silicon / Intel) | `og-files-<ver>-macos-<arm64\|x86_64>.dmg` |
| Windows | `og-files-<ver>-windows-x86_64.zip` |

## Building from source

Current stable Rust (`rustup`), then the system packages below.

```bash
cargo build --release --no-default-features              # standalone, no FFmpeg
cargo build --release --no-default-features --features ffmpeg
./target/release/og-files ~/Documents
```

Cargo features:

| Feature | What it adds | Default here |
|---|---|---|
| `ffmpeg` | FFmpeg (ffmpeg-next): video player fallback when libmpv is missing, video thumbnails, lossless trim export. Needs the FFmpeg development libraries. | yes |
| `og-suite` | OG-OS desktop integration (suite theme, Wayland drag-and-drop with other apps). Linux only; needs the OG-OS monorepo. | yes (monorepo) |

Outside the OG-OS monorepo always pass `--no-default-features` (and add
`--features ffmpeg` if you want it).

### Linux

Build: a C compiler, `pkg-config`, and the development packages of
`libxkbcommon`, `wayland`, `vulkan` (loader), `alsa` (`libasound2-dev`),
plus FFmpeg's (`libavcodec-dev libavformat-dev libavutil-dev
libswscale-dev libswresample-dev libavdevice-dev libavfilter-dev`,
`clang` for bindgen) when building with `ffmpeg`.

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev \
  libvulkan-dev libasound2-dev
# Arch
sudo pacman -S base-devel libxkbcommon wayland vulkan-icd-loader alsa-lib
```

Runtime (optional):
- `mpv` / `libmpv2` — video playback (`libmpv.so.2`)
- `udisks2` — drives in the sidebar (mount, unmount, eject)
- `rclone` + `fuse3` — network storage
- `tailscale` — tailnet peers; a Secret Service keyring (GNOME Keyring,
  KWallet, KeePassXC) — remembered passwords; Avahi — LAN discovery

### macOS

Xcode command line tools (`xcode-select --install`). Optional:

```bash
brew install mpv        # libmpv for video (release .dmg bundles it)
brew install ffmpeg     # only for --features ffmpeg
```

Network mounts need the official rclone binary from rclone.org (Homebrew's
rclone can't mount) and [macFUSE](https://macfuse.github.io) or FUSE-T.

### Windows

The MSVC toolchain (Visual Studio Build Tools with "Desktop development with
C++") and the `x86_64-pc-windows-msvc` Rust target. Optional at runtime:
- `libmpv-2.dll` next to `og-files.exe` — video (from an mpv "dev" build;
  the release zip bundles it)
- `rclone` (`winget install Rclone.Rclone`) and
  [WinFsp](https://winfsp.dev) — network storage
- Tailscale — tailnet peers

`--features ffmpeg` on Windows needs FFmpeg's development files (e.g. via
vcpkg) and `FFMPEG_DIR`; release builds leave it off.

## Command line

```bash
og-files [--view tree|list|grid] [--search TEXT] [--focus] [--usage DIR] [PATH]
og-files extract ARCHIVE [DEST] [--password PW]
og-files compress OUT.{zip,tar.gz,tar.zst,tar.xz,7z} FILE...
og-files list ARCHIVE [--password PW]
og-files open FILE...
og-files open-with APP FILE...      # desktop id / macOS bundle id / Windows program
og-files apps FILE
og-files --help
```

The subcommands run without a window and print to the terminal on every OS
(on Windows from cmd or PowerShell too).

## Configuration

Plain JSON files, all optional:

| | Linux | macOS | Windows |
|---|---|---|---|
| config (`settings.json`, `bookmarks.json`, `connections.json`) | `~/.config/og-files` | `~/Library/Application Support/og-files` | `%APPDATA%\og-files` |
| state (activity log, video positions, rclone logs) | `~/.local/state/og-files` | `~/Library/Application Support/og-files` | `%LOCALAPPDATA%\og-files` |
| cache (thumbnails) | `~/.cache/og-files` | `~/Library/Caches/og-files` | `%LOCALAPPDATA%\og-files\cache` |

`settings.json`: `{ "terminal": "kitty" }` picks the terminal for "Open
terminal here" (macOS: a command or an app name such as `"iTerm"`; Windows:
`"wt"`, `"powershell"`, `"pwsh"` or `"cmd"`).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
