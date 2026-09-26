# App Manager

A native GUI app store for Arch Linux — search, install, and remove software across **pacman**, **AUR** (via `yay`), and **Flatpak** from one window, with a two-tier uninstall (app only, or app + its config/data).

Built in Rust with [iced](https://iced.rs), matching the look of the rest of the [sway-settings-manager](https://github.com/OmegaGiven/sway-settings-manager) suite (shares its theme config at `~/.config/sway-power/config.json`).

## Features

- **Browse tab**: search by name/description across pacman repos, the AUR, and Flatpak at once, with per-source filter chips. Install with one click (opens your terminal for the sudo/interactive parts pacman and yay need).
- **Installed tab**: lists everything explicitly installed (not just pulled-in dependencies), correctly labeling AUR-built packages instead of showing everything as plain pacman.
- **Two-tier uninstall**:
  - *Remove app* — package + orphaned dependencies only.
  - *Remove app + config* — also wipes package-owned `/etc` configs (`pacman -Rns`), and for pacman/AUR apps, scans `~/.config` and `~/.local/share` for a name-matching leftover directory and shows you the exact paths before deleting anything (pacman has no concept of a user's runtime config dirs, so this is a best-effort guess, never silent). Flatpak's tier 2 is exact via `--delete-data`.

## Build

```sh
cargo build --release
cp target/release/app-manager ~/.local/bin/
```

## Requirements

`pacman`, `yay`, and `flatpak` on `$PATH`. Any one of them missing just means that source shows no results / "not installed" instead of erroring.
