# OG-OS base package set (curated, not a clone of this machine)

This machine's `packages.x86_64`/`foreign-packages.txt` (frozen in this
repo) is 139+18 packages — that's *this user's* box, including Steam,
VirtualBox, Android SDK, a dozen personal apps, and a full dev
toolchain. None of that belongs in a default install. This doc is the
actual base set: what makes the OG-suite experience work out of the box
for someone who just installed the ISO, plus genuinely everyday tools.
Everything else becomes an optional group, installed after the fact if
someone wants it — never in the base image.

Checked against the running config, not guessed: `$term` in
`~/.config/sway/config` is `alacritty` (foot is unused), the launcher is
`og-search-launch` (wofi/wmenu are unused — superseded), waybar is fully
replaced by og-bar, clipman by og-clip, pavucontrol is superseded by
og-settings' Audio tab (but see note below).

## Base groups

### Core system (required, no debate)
`base` `base-devel` `linux` `linux-firmware` `amd-ucode` `intel-ucode` `sudo` `efibootmgr` `cryptsetup` `snapper`

`cryptsetup`/`snapper` added 2026-09-08 — the installer now offers real
LUKS2 disk encryption and, on btrfs, real snapper-backed snapshots (auto
pre/post-pacman-transaction via a hand-rolled pacman hook pair, since
snap-pac is AUR-only and not worth pulling into a chroot with no build
user set up). Both packages are base regardless of whether a given
install actually uses either — same "install once, ISO stays
hardware/choice-agnostic" logic as the GPU driver breadth below, not
tied to this machine's own setup.

`intel-ucode` isn't on this AMD box but must ship in a general ISO — same
logic as keeping all GPU drivers (section "Hardware breadth" below).
`base-devel` kept (decided) — needed for `makepkg`/AUR to actually work
via `yay`, which is already in base; without it `yay -S` fails on
anything not pre-built. Real cost is the compiler toolchain it pulls in
(gcc, binutils, make, etc — not tiny), but the point of keeping `yay` in
base at all was AUR-friendliness, and that's dead without this.

### Compositor + session
`sway` `sway-contrib` `swaybg` `swayidle` `swaylock` `lightdm` `lightdm-gtk-greeter` `polkit` `xorg-xwayland` `xdg-desktop-portal-wlr` `xdg-desktop-portal-gtk` `xdg-utils`

### The OG suite itself (own packages, built via PKGBUILD, not from official repos)
og-bar, og-settings, og-search, og-clip, og-note, og-notif-center,
og-notify, og-files, og-links, og-apps — these ship as the local `[ogos]`
repo per OS-PLAN.md 2c, not as generic packages someone else maintains.

Runtime deps these actually need: `ttf-nerd-fonts-symbols` +
`ttf-nerd-fonts-symbols-common` (og-bar/og-search icon glyphs — verified
this is the actual installed font providing them), `wl-clipboard`
(og-clip shells out to it), `grim` `slurp` (screenshot pipeline).

### General text fonts — real gap, found 2026-09-12

**Nothing in this list, until now, ever specified a general-purpose UI
font.** `ttf-nerd-fonts-symbols` above is icon glyphs only (a narrow,
specific codepoint range for og-bar/og-search's own UI chrome) — it was
never meant to, and can't, cover ordinary text. Confirmed live on the
dev machine: `fc-match sans-serif` / `monospace` both fell through to
`Nimbus Sans` / `Nimbus Mono PS` — Ghostscript's bundled PostScript-clone
fonts, present only as a side effect of the `cups`/`ghostscript` printing
group below, with Unicode coverage limited to roughly Latin-1. That's
the actual cause of the "random tofu boxes in random apps" report: any
character outside that narrow range — accented letters beyond the
basics, typographic quotes/dashes, most symbols, all emoji — had
literally no matching font anywhere on the system, in *any* app, not
just the OG suite.

`noto-fonts` `noto-fonts-emoji` `ttf-liberation` — Noto for broad
general-script Unicode coverage (the closest thing to a "just works"
default), Noto Color Emoji for actual emoji instead of tofu, Liberation
as a metric-compatible fallback for documents/PDFs expecting
Arial/Times/Courier. `noto-fonts-cjk` deliberately left out of base (it's
large, and CJK text isn't a default-install assumption for this distro)
— call this out as an easy optional group later if it comes up.

### Audio
`pipewire-alsa` `pipewire-jack` `pipewire-pulse` `alsa-utils` `alsa-firmware` `sof-firmware`

`pavucontrol` dropped from base — og-settings' Audio tab is the intended
front-end now. Keep it in an optional "troubleshooting tools" group
since it's a good fallback when something's wrong with our own tab.

### Bluetooth / network
`bluez` `bluez-utils` `iwd` `wireless_tools` `openssh` `tailscale` `wireguard-tools`

`blueman` dropped — og-settings' Network tab drives bluetoothctl
directly. Same superseded logic as pavucontrol; not carrying the GUI
duplicate into base.

### GPU / hardware breadth
`mesa-utils` `vulkan-tools` `vulkan-intel` `vulkan-radeon` `vulkan-nouveau` `libva-intel-driver` `intel-media-driver` `libva-utils` `xf86-video-amdgpu` `xf86-video-ati` `xf86-video-nouveau`

Deliberately broad (installs drivers for hardware this box doesn't have)
— the whole point of a general installer is "new or repeat system,"
per OS-PLAN.md 2e. Don't trim this to what this machine needs.

### Terminal
`alacritty` — gap found while writing `packages.x86_64` for the archiso
build: this doc's own audit note says `$term` in `~/.config/sway/config`
is `alacritty`, but no group above ever actually listed the package.
Without it the default sway config's terminal keybind launches nothing
on a fresh install.

### Everyday utilities (small, genuinely useful for most desktop users)
`git` `nano` `vim` `htop` `btop` (btop is a hard dependency — og-settings'
SysMonitor tab embeds it directly, not optional) `unzip` `wget` `less`
`brightnessctl` `smartmontools` `zram-generator` `flatpak` `mpv`

`vlc` dropped in favor of `mpv` alone — no reason to ship two media
players in a *bare-bones* base; `vlc` is one `pacman -S` away for anyone
who wants it.

### Printing
`cups` `cups-pdf` `ghostscript` `gsfonts` `gutenprint` — backs
og-settings' Printing tab (driverless IPP-Everywhere setup covers most
modern printers out of the box; `gutenprint`/`ghostscript` widen
coverage for older/PostScript-ish models).

### AUR helper
`yay` — kept even though the base image doesn't itself need AUR, since
it's the standard way anyone extends this system afterward.

## Explicitly cut from base (this machine has them, base install won't)

- **Superseded by our own tools**: `waybar`, `wofi`, `wmenu`, `foot`, `clipman`, `pavucontrol`* , `blueman`*, `wireless_tools`** (*kept as optional fallback, not base; **actually kept, see above — listed here only because iwd mostly supersedes it in practice, low cost to keep both)
- **Gaming stack**: `steam` `gamescope` `gamemode` `lib32-gamemode` `mangohud` `lib32-mangohud` `lutris` `heroic-games-launcher-bin` `moonlight-qt` `wine-staging` `winetricks` `alvr-bin` `alvr-bin-debug` — real, well-defined "gaming" optional group for later, not base.
- **Virtualization stack**: `qemu-desktop` `qemu-user-static` `virt-manager` `virt-viewer` `virtualbox` `virtualbox-guest-iso` `libguestfs` `nfs-utils` `multipath-tools` `vde2` `dnsmasq` — optional "virtualization" group.
- **Dev toolchain**: `docker` `docker-compose` `code` `github-cli` `rustup` `npm` `nvm` `wasm-pack` `cmake` `cargo-about` `jdk17-openjdk` `android-sdk-cmdline-tools-latest` `android-tools` `python-protobuf` — optional "development" group.
- **Personal apps (this user's choices, not defaults for anyone)**: `discord` `vesktop` `vesktop-debug` `brave-bin` `min-browser-bin` `gimp` `plasticity-bin` `obs-studio` `godot` `sidequest-bin` `sidequest-bin-debug` `brother-hll2300d` `brother-hll2300d-debug` `usbmuxd` `sshpass` `fish` `yazi` `dolphin`
- **Qt theming** (`qt6-wayland` `qt5ct` `qt6ct` `breeze`): only existed on this machine to make Dolphin look right — checked, nothing else installed actually depends on them (`Required By: None` on all four). Since Dolphin's cut and og-files/file-manager is the intended default, these go too rather than shipping dead weight.

No default browser ships in base. Recommend documenting "install
firefox or your browser of choice" in first-boot rather than picking one
for everyone — `brave-bin` here was this user's personal pick via AUR,
not a default worth baking in.

### Printing — now base (resolved)
og-settings shipped a real Printing tab (detects configured/unconfigured
printers via CUPS, driverless IPP-Everywhere setup, driver-package
hints, default/remove/test-page control — verified live against this
machine's actual Brother printer). That makes printing an OG-suite
feature, not a "maybe someone prints" extra, so `cups` `cups-pdf`
`ghostscript` `gsfonts` `gutenprint` move from "cut" into base.

## Open decisions for you

All resolved. Base package set is settled — see BASE-PACKAGES.md groups
above and the "explicitly cut" list for what's excluded and why. No
default browser ships in base (see note above); first-boot documentation
should point users at installing whichever browser they want.
