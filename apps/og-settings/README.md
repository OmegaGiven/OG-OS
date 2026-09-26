<img width="1920" height="1080" alt="2026-07-04T10:25:15,202129311-04:00" src="https://github.com/user-attachments/assets/97678495-2f52-4ba3-bc3f-e05c7eb08eb1" />
<img width="1920" height="1080" alt="2026-07-04T10:14:37,947911263-04:00" src="https://github.com/user-attachments/assets/8d8fbeee-cf76-4e47-b373-c79f7bae5c5d" />


# settings-manager

A native GTK-free settings app for a `sway` + `waybar` desktop, built with
[`iced`](https://iced.rs). One window, no config files to hand-edit for the
common stuff — it reads and writes your actual `sway` and `waybar` config
files directly and applies most changes live.

## What it does

| Tab | Covers |
|---|---|
| **Power** | Monitor/system sleep timers, sway's own startup `exec`/`exec_always` commands (toggle on/off), `systemd --user` services, and system-level (root) services |
| **Display** | Monitor resolution, drag-to-arrange multi-monitor layout |
| **Network** | Wi-Fi (scan/connect/forget), wired interfaces (up/down), Bluetooth (power/discoverable/pairable, pair/connect/remove devices) |
| **Hotkeys** | Edit sway `bindsym` keybindings and `set $var` variables with live key-capture, writes back to `~/.config/sway/config` |
| **Theme** | Color picker (real HSV wheel) for every sway/waybar color, window gaps/borders, waybar position/thickness/module arrangement (drag-and-drop between left/center/right), clock timezones, mouse sensitivity, cursor theme/size, terminal color scheme (see below) |
| **History** | Snapshots of every applied config, one-click restore |
| **System Monitor** | Embedded `btop` |

A header search box does live cross-tab search — type "tailscale" and get
that service's toggle directly, no matter which tab it lives on.

## How it works

There's no daemon and no custom config format for the things sway/waybar
already own. Instead:

- **Sway settings** (bindings, variables, gaps, borders, cursor theme,
  pointer sensitivity, startup commands) are read from and written directly
  to `~/.config/sway/config`, using a few markers
  (`# settings-manager bindings start/end`) to keep the app's managed
  region separate from the rest of your config. Everything outside those
  markers is left untouched.
- **Waybar settings** (colors, module layout, clock format) are written to
  `~/.config/waybar/theme-overrides.css` (imported at the top of your
  `style.css`) and directly into `~/.config/waybar/config`. Waybar gets
  restarted (not just reloaded) when something structural changes, like
  moving the bar to a different screen edge.
- **App-level settings** (which aren't really sway's or waybar's business —
  sleep timers, arrangement of modules, etc.) live in
  `~/.config/sway-power/config.json`.
- Network/Bluetooth/service state is **not** stored anywhere — it's queried
  live from `rfkill`, `bluetoothctl`, `iwctl`/`nmcli`, and `systemctl` each
  time you open that tab.
- Anything requiring root (enabling a system service, bringing an ethernet
  interface up) opens your terminal and runs `sudo` there interactively,
  rather than trying to manage a password inside the GUI.

## Requirements

Runtime, all typically already present on a sway desktop:

- `sway`, `waybar`
- `systemd` (`systemctl`) — Power tab's service lists
- `rfkill`, `bluetoothctl` (bluez) — Network tab
- Wi-Fi: either **iwd** (`iwctl`) or **NetworkManager** (`nmcli`) — the app
  detects whichever is actually running and uses that. Neither installed
  means the Wi-Fi section just stays empty; nothing else breaks.
- `btop` — System Monitor tab (optional; that tab just won't render without it)
- A terminal emulator (auto-detected from `alacritty`, `foot`, `kitty`,
  `wezterm`, `konsole`, `gnome-terminal`, `xfce4-terminal`, `tilix`,
  `terminator`, `urxvt`, `st`, `xterm` — whichever's on `$PATH`) — used for
  sudo prompts and Wi-Fi/Bluetooth pairing that need interactive input

Build-time:

- Rust (stable) + Cargo
- A Wayland-capable GPU stack for `iced`'s `wgpu` renderer (any GPU with
  working Vulkan/GL drivers; falls back to `tiny-skia` software rendering
  otherwise)

## Build

```sh
cd settings-manager
cargo build --release
```

Binary lands at `target/release/settings-manager`.

## Install

```sh
cp target/release/settings-manager ~/.local/bin/
```

Make sure `~/.local/bin` is on your `$PATH`.

### Wire it into sway

Add a binding to launch it (this repo's own sway config uses
`$mod+m` and `ctrl+alt+Delete`):

```
bindsym $mod+m exec swaymsg '[title="Settings Manager"] focus' 2>/dev/null || ~/.local/bin/settings-manager &
```

The `swaymsg focus` half means pressing the key again just refocuses the
existing window instead of spawning a duplicate.

### Wire it into waybar (optional)

A custom module button, e.g.:

```json
"custom/settings": {
  "format": "",
  "tooltip": "Settings Manager",
  "on-click": "swaymsg '[title=\"Settings Manager\"] focus' 2>/dev/null || ~/.local/bin/settings-manager &"
}
```

(the `` glyph needs a Nerd Font — swap for plain text if you don't have one)

### First run

On first launch it reads your *existing* `~/.config/sway/config` and
`~/.config/waybar/config` to seed its own state (current keybindings,
colors it can find, current module layout, current cursor theme/size,
current pointer acceleration). It won't overwrite anything until you
actually change a setting and hit **Apply & Save** — nothing is touched on
launch alone.

## Terminal theming

There's no 17th color picker for this — it reuses the same six colors
already in the Theme tab (background, text, accent, secondary background,
inactive, urgent) and procedurally derives a full 16-color ANSI palette
plus background/foreground/cursor/selection from them, so the terminal
just reads as part of the same theme. Regenerated on every **Apply & Save**
for whichever terminal is set in "Default Terminal":

- **Alacritty, foot, kitty** — written to a small companion file the app
  owns entirely (`settings-manager-colors.{toml,ini,conf}`), wired in via
  an `import`/`include` line added to the main config if it's missing.
- **xfce4-terminal, terminator** — patched in place; their config is a flat
  enough format that specific known keys can be safely updated without
  touching anything else in the file.
- **Konsole** — dropped as a new `.colorscheme` file. Konsole will list it
  as a selectable scheme, but there's no reliable way to find and flip
  "the active profile" automatically, so pick "Settings Manager" once in
  Profile Settings.
- **Everything else is a no-op**: wezterm's config is executable Lua (no
  text-upsert is safe against arbitrary user logic), gnome-terminal/tilix
  keep their profiles in dconf rather than a file, urxvt/xterm use
  Xresources (a different mechanism entirely), and st has no config file
  at all — its colors are compiled in. None of these error, they're just
  silently skipped.

## A note on portability

This was built against one specific machine's stack: Arch Linux, iwd (not
NetworkManager), systemd, PipeWire's PulseAudio shim. It should work
unmodified on **any** sway+waybar desktop with systemd, either iwd or
NetworkManager, and bluez — which covers the large majority of sway users
on any distro. It will *not* gracefully handle a non-systemd init (Void,
Gentoo/OpenRC, Alpine) — the Power tab's service lists simply require
`systemctl` to exist.
