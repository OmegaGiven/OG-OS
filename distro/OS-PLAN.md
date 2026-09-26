# OG-OS — plan to package this machine into an installable ISO

**Scope correction (2026-07-09):** the base image is a *curated*
bare-bones set, not a clone of this machine. `packages.x86_64` /
`foreign-packages.txt` in this repo are a frozen snapshot of this user's
actual box (139+18 packages, including Steam, VirtualBox, a full dev
toolchain, personal apps) — useful as an audit trail, but the real base
package list lives in `BASE-PACKAGES.md`, curated down to what makes the
OG-suite experience work out of the box plus genuinely everyday tools.
Gaming/virtualization/dev-toolchain/personal-app packages become
optional post-install groups, not base image contents.

Goal: package the OG-toolkit sway desktop stack (base Arch + custom
tools/configs/scripts built across the og-bar/og-settings/og-search/etc
sessions) into something installable on a new or repeat
machine — an ISO, not a dotfiles script that half-works depending on
what's already there.

Snapshot of the machine this plan is built from (2026-07-09): Arch Linux,
139 explicit official packages + 17 AUR packages (via `yay`), lightdm as
greeter, sway 1.11 as the compositor. Numbers below will drift as the
system does — this doc describes the *mechanism* for capturing state, not
a frozen package list.

## 1. Two viable approaches — pick one before building anything

| | **archiso respin** | **Custom installer image (calamares/airootfs)** |
|---|---|---|
| What it is | Arch's own `archiso` tool, profile = official releng profile + your packages/configs baked into `airootfs/` | archiso base, but boots into a graphical installer (Calamares) that partitions/installs for the user instead of a manual `arch-chroot` |
| Effort | Low-medium — mostly config, no new UI to build | Medium-high — need to package Calamares + a branding module |
| Result | Boots to a live sway session with everything installed; user partitions manually via a script you provide, `pacstrap`-alike | Boots to a "click next" installer, ends with a fully configured system, reboots into it |
| Best for | You, re-imaging your own boxes | "someone else" installs it unattended |

**Recommendation: start with the archiso respin.** It's the same tool
Arch's own installer ISO is built from, gets you a working artifact in
days not weeks, and the airootfs/package-list/hooks structure it produces
is exactly what a later Calamares module would consume anyway — nothing
built in phase 1 is thrown away if you go further later.

## 2. What actually needs to be captured

Everything below already exists on this machine in some form — the work
is extracting it into a buildable, versioned form instead of "whatever
state this disk happens to be in."

### 2a. Package manifest
- `packages.x86_64` / `foreign-packages.txt` in this repo = this machine's full snapshot (audit trail only, per the scope correction above).
- **`BASE-PACKAGES.md` is the actual base list** — curated down to core system + compositor/session + the OG suite + audio/bluetooth/network + GPU driver breadth + Qt theming + a short everyday-utilities list + yay. Gaming/virtualization/dev-toolchain/personal-app packages from this machine's snapshot are explicitly excluded, listed there as optional post-install groups instead.
- The only foreign/AUR package base actually needs is `yay` itself (the AUR helper) — everything else foreign on this machine is either superseded by an OG-suite tool or a personal app, both excluded from base. So archiso's AUR-at-build-time problem barely applies to the *base* image; it only resurfaces if/when an optional group (gaming, etc.) is built out later, at which point these still apply:
  - (a) vendor a local pacman repo of prebuilt AUR packages baked into the ISO (`repo-add`, then a local `[ogos]` repo entry in the image's pacman.conf), or
  - (b) ship `yay` + a first-boot/on-demand script that pulls AUR packages when a group is installed (needs network at that point — acceptable, matches how any optional group installs).

### 2b. Dotfiles / configs
Already tracked ad hoc under `~/.config/` — needs a single "skeleton"
source of truth copied into new-user homes at install time (`/etc/skel`
equivalent) or an actual dotfiles repo checked out post-install:
- `~/.config/sway/config` (+ `hotkeys.sh`)
- `~/.config/mako/`, `~/.config/alacritty/`, `~/.config/waybar/` (legacy — superseded by og-bar, keep only if still referenced by anything, otherwise drop)
- `~/.config/sway-power/` (og-power-apply's own config)
- `~/.config/og-files/`, `~/.config/og-links/`
- og-bar's `bar-config.json`, og-settings' `config.json` (both via `og-config` — ship sane defaults, not this machine's personal tuning)

### 2c. Your own repos (the actual product)
All of these are separate git repos today under `~/.local/src/`:
`OG-toolkit`, `og-bar`, `sway-control` (og-settings), `omegagiven-search`
(og-search), `og-clip`, `og-note`, `og-notif-center`, `og-notify`,
`app-store`, `file-manager`, `galias`, `OmegaGiven-profile`.

For an ISO these need to stop being "clone and cargo build on first
boot" and become **actual built binaries baked into the image** (or into
the local pacman repo from 2a as real packages with PKGBUILDs) —
otherwise the ISO needs a Rust toolchain and a live internet connection
just to reach a usable desktop, which defeats "install and go."
Concretely: write a `PKGBUILD` per repo (they're just `cargo build
--release` + install steps, straightforward), build once, drop `.pkg.tar.zst`
into the local `[ogos]` repo alongside the vendored AUR packages.

### 2d. System-level config outside `/home`
- greeter: lightdm + lightdm-gtk-greeter, greeter background sync (already handled by `sway::sync_greeter_background`) — needs to run once at install time, not rely on a live user session.
- systemd units: user-level (`og-links.service`, pipewire wants, timers) — ship as part of a `/etc/skel`-style user unit seed, enabled via `systemctl --user preset` on first login. System-level custom units found today: `gpu-performance.service` — audit what it does and ship it as an actual package-installed unit, not a hand-edited file.
- `/etc/wolf/cfg/config.toml` patch (the uinput/cgroup fix from the Remote Play session) — if Wolf/gamescope-hosting is meant to ship by default, this needs to become a config *template* applied by an install hook, not a manual edit repeated per machine.
- sudoers: **resolved 2026-09-07.** `og-settings`' PKGBUILD now installs `og-vpn-apply` to `/usr/local/bin` (root:root, mode 750 — not `~/.local/bin`, which a normal user can edit) and the matching `%wheel ALL=(root) NOPASSWD: /usr/local/bin/og-vpn-apply` rule to `/etc/sudoers.d/og-vpn` (mode 440) as part of the package itself, not a manual step. The rule also stopped being hardcoded to one username (`omegagiven` → `%wheel`) — it now works for whoever installs OG-OS, not just this dev machine.

### 2e. Hardware breadth
This machine has amdgpu (`xf86-video-amdgpu`) but the pacman list also
includes nouveau/radeon/intel-media drivers — that's fine for a
general-purpose ISO (install everything, let the running kernel pick),
just confirm the package list intentionally stays hardware-agnostic
rather than trimmed to "what this box needs," since the whole point is
running on a "new or repeat system."

**Gap found and closed:** the base list's "let the kernel pick" logic
works fine for AMD/Intel (mesa's open stack *is* the driver, nothing to
choose), but leaves Nvidia hardware on `nouveau` only — a genuinely worse
experience there (no power management, no CUDA/NVENC) than the
proprietary driver most Nvidia users actually want, and picking the right
one (`nvidia-open-dkms` vs `nvidia-dkms` vs an AUR legacy package) by
generation is exactly the kind of thing that trips up first-time Linux
users. `scripts/detect-gpu-driver.sh` reads the GPU's PCI ID + chip
codename (from `lspci -nn`) and recommends/installs the right package:
AMD/Intel confirmed covered by base, Nvidia mapped by generation (Turing+
→ `nvidia-open-dkms`, Maxwell/Pascal/Volta → `nvidia-dkms`, Kepler →
AUR `nvidia-470xx-dkms`, Fermi → AUR `nvidia-390xx-dkms`, older →
nouveau-only, no proprietary option exists). Tested standalone against
this machine's AMD card and against synthetic Nvidia codenames spanning
all five generations. Belongs in phase 4's install script (section 5) as
a first-boot step, but runs fine standalone today on any Arch box.

## 3. Proposed repo layout

New repo, e.g. `~/.local/src/og-os/`:

```
og-os/
├── profiledef.sh              # archiso profile metadata
├── packages.x86_64            # official repo package list (from 2a)
├── pacman.conf                 # base + local [ogos] repo entry
├── local-repo/                 # built .pkg.tar.zst for AUR + your own tools
│   └── (repo-add generated .db/.files)
├── pkgbuilds/                  # one dir per your-own-tool repo
│   ├── og-bar/PKGBUILD
│   ├── og-settings/PKGBUILD    # builds sway-control → og-settings
│   ├── og-search/PKGBUILD
│   └── ...
├── airootfs/                   # files copied verbatim onto the live/installed system
│   ├── etc/skel/.config/sway/config
│   ├── etc/skel/.config/...
│   ├── etc/systemd/system/gpu-performance.service
│   ├── etc/sudoers.d/og-vpn    # scoped NOPASSWD entry
│   └── root/install.sh         # or a customize_airootfs.sh archiso hook
└── OS-PLAN.md                  # this doc, or move it here once the repo exists
```

## 4. Build pipeline (once repo exists)

1. `mkarchiso -v -o out/ og-os/` — standard archiso build, pulls
   `packages.x86_64` from the official repos + `[ogos]` local repo for
   everything in `local-repo/`.
2. Result: a `.iso` in `out/` — `dd` to USB or boot in a VM to test.
3. First real install target: a VM (qemu, already have `qemu-desktop`
   installed) — validate partitioning script + first-boot config before
   ever touching real hardware.

## 5. Suggested phase order

1. **Inventory freeze** — done. `pacman -Qqe`/`pacman -Qqm` snapshot committed to the new repo, so there's a concrete starting package list instead of "whatever's on this disk right now."
2. **PKGBUILDs for the 12 own-repos** — done for 9 (`pkgbuilds/`, see its README for the git+file:// sibling-checkout pattern, build order, and known gaps). `og-note` (Tauri, different toolchain) and `og-hotkeys`/`og-wallpaper`/`og-wallpaper-studio` (not in the original base list) deferred.
3. **archiso skeleton** — in progress. `profiledef.sh`/`pacman.conf`/`packages.x86_64`/`airootfs/` now exist (copied from archiso's own `releng` profile, customized). `pacman.conf` wires up the local `[ogos]` repo (`local-repo/`, unsigned/TrustAll, built from `pkgbuilds/`). Live boot currently inherits releng's default (root autologin to a zsh shell on tty1) — does NOT yet boot straight into a graphical sway session; that needs a liveuser account + lightdm autologin + a seed `~/.config/sway/config` in `airootfs/etc/skel`, deliberately deferred rather than rushed since it's really first-boot/install-script territory (phase 4) bleeding into phase 3.

   First `mkarchiso` build validated end-to-end via qemu (`-enable-kvm`, OVMF UEFI, headless + monitor screendump) — reproduced the exact "mount: wrong fs type, bad option, bad superblock... mounting '' on real root" failure the user hit in UTM, proving it was a real ISO bug, not a VM drive-config issue. Root cause: `packages.x86_64` was built purely from `BASE-PACKAGES.md`'s curated desktop groups and never cross-referenced against archiso's own required infrastructure packages. Two gaps found and fixed: (1) `mkinitcpio`/`mkinitcpio-archiso` missing — without them, none of `HOOKS=(... archiso archiso_loop_mnt ...)` in `airootfs/etc/mkinitcpio.conf.d/archiso.conf` actually exist, so the initramfs has no way to locate the boot medium at all; (2) `zsh` missing — `airootfs/etc/passwd` (copied verbatim from releng) hardcodes root's shell as `/usr/bin/zsh`, which wasn't installed.

   **Known remaining gap, deferred to phase 4**: `packages.x86_64` still lacks real partitioning/install tooling (`parted`, `gptfdisk`, `arch-install-scripts`, `e2fsprogs`, `cryptsetup`, `lvm2`, `man-db`) that releng ships and any actual installer will need — intentionally not added yet since phase 3's scope is "boots to a live session," not installing. The rest of releng's package-list diff (testdisk/clonezilla/cloud-init/virtualbox-guest-utils/PXE-netboot tooling/etc.) is deliberately excluded — rescue-disk/VM-guest bloat that doesn't belong in a bare-bones curated base, per `BASE-PACKAGES.md`.
4. **Install script** — `airootfs/root/install-ogos.sh`, run manually from the live session (liveuser has NOPASSWD sudo). Interactive prompts for disk/hostname/username/password/keyboard/encryption, or flags (`--disk --hostname --user --password --fs --keymap --encrypt --luks-password --yes`) for scripted/repeat runs. GPT partitioning (512MiB ESP + rest ext4 or btrfs via `--fs`). `pacstrap` pulls the same package set as `packages.x86_64`, including the OG-suite from `[ogos]` — pointed at `/root/ogos-repo`, a copy of `local-repo/` **baked directly into the ISO's airootfs**, not the build machine's `file:///home/.../og-os/local-repo` path from `pacman.conf`, which only resolves on this dev machine and would silently fail on any other install target.

   **Added 2026-09-08 — encryption, keyboard, snapshots.**
   - **LUKS2 encryption** (`--encrypt`/`--luks-password`, or prompted): root partition only — ESP can't be encrypted and still boot. `cryptsetup luksFormat`/`open` before any `mkfs`, so the chosen filesystem (ext4 or btrfs, subvolumes included) lands on the mapped `/dev/mapper/cryptroot`, never the raw partition. mkinitcpio's `HOOKS=` gets `encrypt` inserted right after `block` (sed on the stock default line — a hand-edited/reordered HOOKS line on the live medium's base install would break this, not expected here), and the systemd-boot entry's kernel cmdline gets `cryptdevice=UUID=...:cryptroot` prepended with `root=/dev/mapper/cryptroot` instead of a PARTUUID.
   - **Keyboard layout** (`--keymap`, prompted first, before any password entry): applied to the live session immediately via `loadkeys` (so it actually helps typing the *rest* of the prompts, passphrases included), baked into the target's `/etc/vconsole.conf` + initramfs (`keymap` hook, already a stock default hook) so it's active at the LUKS unlock prompt too, and best-effort carried into the seeded sway session as `xkb_layout`. That last part assumes the console keymap name and XKB layout code agree, true for most single-word ones (`de`, `fr`, `se`, ...) but not compound names like `uk`→`gb` — a known, bounded gap, not a silent one.
   - **btrfs snapshots**: when `--fs btrfs`, real subvolumes now exist (`@` for root, `@home` — previously a single flat subvolume, which defeated the actual point of choosing btrfs at all). `snapper -c root create-config /` sets up `.snapshots` (nested under `@`, not a separate top-level `@snapshots` — survives normal rollback/restore, not a full `btrfs subvolume delete @`; documented tradeoff), `snapper-timeline.timer`/`snapper-cleanup.timer` enabled for periodic auto-snapshots + retention, and a hand-rolled pacman hook pair (`/etc/pacman.d/hooks/50-snapper-{pre,post}.hook`) snapshots before/after every pacman transaction — the actual valuable part (a rollback point right before the update that breaks something), done without needing AUR's `snap-pac` (no build user set up inside the chroot to make that practical).
   - **Not yet validated in a real qemu boot** — syntax-checked, the mkinitcpio HOOKS sed and boot cmdline string composition verified in isolation, and each piece follows documented Arch-wiki-standard command sequences, but no interactive root session was available to run the actual partition/LUKS/mkfs/subvolume commands live during this pass. Run `testing/full_install_boot_test.py`-style validation (or a manual qemu install) before trusting this on real hardware.

   **Resolved 2026-09-07 — updates on an installed system.** The throwaway `pacman-install.conf` above only drives `pacstrap` itself; it used to be true that the installed system's real `/etc/pacman.conf` ended up as pacman's own stock default afterward, with nothing pointing at `[ogos]` at all — meaning an installed system had **no update path whatsoever** for its own desktop suite, permanently frozen at whatever shipped on the ISO. Fixed: the script now also appends a real, permanent `[ogos]` entry to the target's `/etc/pacman.conf` inside the `arch-chroot` block, pointed at `https://omegagiven.github.io/OG-os-repo/x86_64` — a real hosted repo (GitHub Pages, same pattern as OG-DB), not a path that only exists on this dev machine or this ISO. `pkgbuilds/publish.sh` builds all ten PKGBUILDs, `repo-add`s them, and pushes to both places from one run — the ISO-baked offline copy and the permanent hosted one never drift out of sync since they come from the same build. og-settings' Updates tab (`checkupdates` + `yay -Sua`) now actually surfaces new og-suite versions on an installed system, not just official-repo/AUR packages.

   **Still open**: no GPG package signing (`SigLevel = Optional TrustAll` — acceptable for a single-maintainer repo today, a real gap before anyone else installs this and trusts it blindly), and publishing is a manual step (`publish.sh` run by hand after committing changes worth shipping), not CI-triggered.

   Target config done inside `arch-chroot`: locale/hostname/fstab, `mkinitcpio -P` (target's own default hooks, not the ISO's `archiso` ones), first user via `useradd -m -G wheel` (picks up the same seed sway config through `/etc/skel`, one source shared with the live session's `liveuser`), root password locked (`passwd -l root`) in favor of `%wheel ALL=(ALL:ALL) ALL` (password required — NOPASSWD is a live-medium-only convenience, not carried into installs), lightdm/iwd/bluetooth/systemd-networkd/systemd-resolved enabled, `systemd-boot` installed via `bootctl` with a loader entry built from the root partition's real `PARTUUID`.

   Added to `packages.x86_64` for this: `arch-install-scripts` (pacstrap/genfstab/arch-chroot), `parted`, `gptfdisk` (sgdisk), `e2fsprogs`.

   **Validated end-to-end in qemu** against a throwaway 16GB scratch disk (`qemu-img create -f qcow2`), unattended via `--disk /dev/vda --hostname ogos-test --user testuser --password ... --yes`: partitioning, pacstrap (all 541 packages resolved cleanly including every `[ogos]` package from `/root/ogos-repo`), `arch-chroot` config, `bootctl` install all completed without error. Rebooted the scratch disk **standalone, no installer ISO attached** — systemd-boot loaded the kernel and lightdm rendered a real graphical login screen with `testuser` pre-filled and the right hostname in the bar. This also strongly suggests the phase-3 live-ISO sway hang (`session_real_start: assertion 'priv->pid == 0' failed`, still unresolved) is specific to the `autologin-session=sway` path, not a general QEMU/GPU limitation — lightdm clearly renders fine here.

   **Bug found and fixed via that same test**: logged into the installed system as `testuser` and found `~/.config/sway` didn't exist at all — sway had silently fallen back to its own stock default session (visible: the plain blue "sway" logo wallpaper, no og-bar). Root cause: `pacstrap` only pulls package files into `/mnt`; it does **not** carry over anything from the live ISO's own `airootfs` overlay, including `/etc/skel`. `install-ogos.sh` now explicitly copies `/etc/skel/.config/sway/config` into `/mnt/etc/skel/` before the chroot's `useradd -m` step.

   **Re-validated after the fix, full loop confirmed working**: rebuilt the ISO, re-ran the same unattended install against a fresh scratch disk, rebooted standalone. Logged in as `testuser` through lightdm's graphical greeter, confirmed via `pgrep -a og-bar` (PID present) and visually via `og-notif-center`/`og-clip` both auto-launching and tiling per the seed config's `exec` lines. Partition → pacstrap → user creation → systemd-boot → graphical login → seeded sway session all work end to end on a completely fresh install.

   Also confirmed live: og-bar/og-settings/sudo all present at `/usr/bin` on the installed system, matching the packages actually requested.
5. **(Optional, later)** Calamares module if unattended installs for other people become the actual goal.

## 6. Open decisions — resolved 2026-07-15

- Legacy waybar/mako/alacritty configs: **drop**, not carried into `airootfs/etc/skel`.
- Personal vs. distributable config split: **seed defaults + import step** — ISO ships sane defaults, og-settings onboarding handles pulling in a personal `bar-config.json`/hotkeys afterward, not hardcoded into the image.
- AUR vendoring scope: **none beyond `yay` itself** in base — matches `BASE-PACKAGES.md`'s existing curation; mangohud/gamemode stay in the future optional gaming group.
