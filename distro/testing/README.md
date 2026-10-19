# og-os click-through testing tool

Automated GUI testing for the og-os live session (Calamares, or any
other graphical app running under sway in the live ISO), built after
several sessions of fighting QEMU-monitor synthetic input (sendkey/
mouse_move over VNC) proved too fragile to trust — coordinate systems,
focus routing, and absolute-vs-relative pointer semantics didn't line
up reliably, and failures were mostly unexplainable from screenshots
alone.

**Status: working.** Clicking through Calamares' Welcome → Location →
Keyboard pages is confirmed reliable and reproducible. The root cause
of the earlier flakiness is understood and fixed (see below).

## What this is

- `boot-vm.sh` — boots an og-os ISO in qemu with standardized VNC,
  monitor, and SSH-forwarding ports for a test run.
- `gui_test.py` — the reusable library: SSH into the guest, install
  `ydotool` there (kernel-level input injection via `/dev/uinput` —
  looks like real hardware to sway, unlike QEMU-monitor synthetic
  input), calibrate its pointer, and drive it. Also wraps
  `swaymsg -t get_tree` for real window geometry/focus (ground truth,
  not a guess from a screenshot) and screenshot capture via the qemu
  monitor for human review.
- `test_calamares_install.py` — a concrete test script that logs in
  and clicks through Calamares' full install flow.
- `check-launchers.py` — static, runs in CI (`ci.yml`): every program
  the seed sway config, the apps' spawn sites (`Command::new`,
  `og_config::og_bin`, og-bar launchers) and the shipped `.desktop` files
  run must come from a package `packages.x86_64` ships; no code may
  hardcode `~/.local/bin` without a `/usr/bin` fallback; NOPASSWD
  sudoers rules may only name package-owned root files; the live-session
  sway config must be the seed config plus its own block.
- `headless-launch-test.sh` — runtime, local only (needs sway): private
  headless sway on a fresh HOME with no `~/.local/bin`, fires every
  startup exec and keybinding of the seed config through sway and checks
  each reached its program (stubs stand in for `/usr/bin`; never touches
  the caller's session).

## The bug, and the fix (this took a long time to find)

`ydotoold`'s virtual device, as started normally, has **no absolute
(`EV_ABS`) capability at all** — confirmed via `/proc/bus/input/devices`,
it's `REL`-only, a plain relative mouse. `ydotoold`'s own
`--touch-on`/`-T` flag is documented to add `EV_ABS`, but in this
environment it made the daemon fail to create *any* device at all (no
socket, nothing) — a dead end, not a fix.

So `ydotool mousemove --absolute` was **never operating on a real
absolute device**. Pointer position is actually determined by relative
motion run through **libinput's pointer acceleration curve** — and the
default `adaptive` accel profile distorts movement nonlinearly (small
moves land close to 1:1, larger jumps land well past where you'd
expect, and the distortion ratio isn't constant). This is exactly why
earlier debugging was so confusing: every theory (halved coordinates,
a fixed 2x scale factor, absolute-device output-mapping) looked
plausible for a couple of data points and then fell apart on the next
one — the real distortion is velocity-dependent, not a fixed ratio.

**The fix**, confirmed live and reproducible:

1. `swaymsg -t get_inputs`, find the ydotool device's `identifier`
   (only appears after at least one move/click has happened once).
2. `swaymsg input <identifier> accel_profile flat`
3. `swaymsg input <identifier> pointer_accel 0`
4. Move via a two-step relative sequence: slam far off-screen first
   (e.g. `-5000,-5000`, clamps to the real (0,0) regardless of current
   position), then send one plain relative move of exactly the target
   `(x, y)`. This now lands within a couple of pixels of the true
   target, reliably, across repeated calls.

All of this is wrapped in `gui_test.py`'s `calibrate_pointer()` and
`move()`/`click()` — callers just call `click(x, y)` in real screen
pixels and it works.

**How this was actually confirmed**, in order:
1. Closing a window via its titlebar `×` button worked immediately
   after calibration — proved the click *mechanism* itself is fine
   once positioning is exact (before calibration, this failed too,
   ruling out "Calamares specifically is broken" as a theory).
2. Clicking Calamares' Next button then worked, landing on Location.
3. Repeated successfully for Location → Keyboard.

The reason earlier attempts *looked* like they had correct positioning
(a screenshot showing the cursor seemingly on the button) but still
missed: the acceleration-curve error was small enough to still look
right at screenshot resolution, but large enough to miss a tight Qt
button hit-box while still registering at the coarser "which window
has focus" level (window-level focus-on-click kept working throughout
all the earlier failed attempts, which is what made this so
misleading).

## `calamares_next_button()` — fixed offset, not a percentage

Calamares' Back/Next/Cancel row sits at a **fixed pixel offset** from
the window's bottom-right corner — confirmed by measuring it in both a
640px-wide tiled window and a 1280px-wide full-screen window and
finding the same offset in both cases, not a proportional fraction.
Earlier versions of this code used `width * 0.746`-style percentages,
which is wrong for exactly this reason (a percentage assumes the
button bar scales with window width; it doesn't, it's right-anchored
with a constant margin). If Calamares' UI chrome changes and this
needs recalibrating, measure it with a screenshot crop and pixel-count
the true offset from the edge — don't guess a percentage.

## What's still unverified

- Full Welcome → Finished click-through plus reboot into the installed
  disk and greeter login now runs end to end (2026-10-19, see "Install
  speed" below). The Partitions/Users offsets are fixed pixel offsets
  that worked there; recalibrate the same way as
  `calamares_next_button()` if Calamares' layout changes.
- BIOS (non-UEFI) installs: never exercised (boot-vm.sh is OVMF-only).

## Research done along the way (context for the above)

- **libei / xdg-desktop-portal `RemoteDesktop`** — this is the modern,
  correct way to inject input on Wayland, and `xdg-desktop-portal-wlr`
  is already in our base image, but wlroots doesn't implement the
  libei backend yet
  ([swaywm/wlroots#2378](https://github.com/swaywm/wlroots/issues/2378),
  open). Not usable today; worth revisiting once it lands.
- **ydotool's own issue tracker** confirms unreliability on Wayland is
  a known, common category of problem generally
  ([#231](https://github.com/ReimuNotMoe/ydotool/issues/231),
  [#73](https://github.com/ReimuNotMoe/ydotool/issues/73),
  [RH bug 2250692](https://bugzilla.redhat.com/show_bug.cgi?id=2250692)) —
  though none of those specific reports were this exact acceleration-
  curve issue; that part seems to have been found here first.
- **AT-SPI / dogtail** (accessibility-based automation, the reliable
  approach for GTK) has only unofficial, limited Qt support, and its
  Wayland story (`gnome-ponytail-daemon`) is GNOME-portal-specific —
  doesn't carry over to sway/wlroots as-is.

## Usage

```sh
./boot-vm.sh                                   # boots newest ISO in ../out/
source /tmp/ogos-vm-XXXXXX/env.sh              # get the printed ports
python3 test_calamares_install.py \
    --ssh-port "$OGOS_SSH_PORT" \
    --monitor-port "$OGOS_MONITOR_PORT" \
    --username testuser --password testpass123 --hostname ogos-test
```

Screenshots land in `/tmp/ogos-test-shots/` by default, together with
`calamares-session.log` (Calamares' own log, copied out of the guest).

The install wait no longer sleeps for the whole `--install-timeout`:
the test polls `/root/.cache/calamares/session.log` and stops at the
finished module's `completion: succeeded|failed` line, raises on a
failed install, and prints a per-step timing table. The same table can
be produced from any saved log:

```sh
python3 calamares_timing.py calamares-session.log
```

`full_install_boot_test.py` additionally checks the installed system
for live-medium leftovers (liveuser, NOPASSWD sudo, root autologin),
the on-disk pacman keyring, the `[ogos]` repo, bluetooth enabled and
the single-initramfs preset. (Its `mako process running` check predates
og-notif-center replacing mako in the seed sway config and currently
always reports NO.)

## Install speed

Target: Calamares "Install" click to "Finished" in well under the
"couple of minutes" Omarchy-style installers take on an SSD. Everything
that runs after the copy is plain file edits plus exactly one
`mkinitcpio` run: no pacman transactions, no downloads, no keyring
generation, no second initramfs.

### Measured (2026-10-19)

KVM VM, 4 vCPU / 4 GB, OVMF, virtio disk (qcow2 on host tmpfs), ISO
read from host page cache — so this measures CPU-bound work (squashfs
decompression, mkinitcpio), not USB/SSD speed. Same package set both
columns (the 2026-07-17 ISO; "after" is that image respun rootless with
this tree's `airootfs/` and squashfs options, see below). Per-step
seconds from `calamares_timing.py` (1 s log resolution):

| Calamares exec step | before | after |
|---|---:|---:|
| partition (9 kpmcore jobs: table, ESP, root, flags) | 6 | 3–4 |
| mount | 0 | 0–1 |
| **unpackfs** (rsync out of the loop-mounted squashfs) | **75** | **28** |
| shellprocess@earlycleanup | 0 | 0 |
| machineid, fstab, locale, keyboard, localecfg (`locale-gen`, 1 locale) | 2 | 1 |
| initcpiocfg | 0 | 0 |
| **initcpio** (`mkinitcpio -p linux`) | **18** | **2** |
| removeuser, users | 0 | 0–1 |
| displaymanager, networkcfg, hwclock, services-systemd | 1 | 0–1 |
| **bootloader** (`bootctl install` + `kernel-install add`) | **5** | **2** |
| shellprocess (target-setup final) | 0 | 0 |
| umount | 0 | 1 |
| **total** | **107** | **39–40** |

(Three "after" runs: 52, 40, 39 s; the 52 s one was the first run of a
freshly booted VM, unpackfs 36 s.)

Installed system after the change: boots to `graphical.target` in
8.5 s; first-boot `pacman-init.service` (keyring) takes 5.2 s in the
background, not ordered before the greeter, and never runs again.

Unit benchmark of the squashfs itself (host, `unsquashfs`, 2026-07-17
rootfs, 3.7 GB / 116k inodes):

| compression | sfs size | decode, 1 thread | decode, 4 threads |
|---|---:|---:|---:|
| xz `-Xbcj x86 -b 1M -Xdict-size 1M` (old) | 1387 MB | 32.8 s | 8.2 s |
| **zstd `-Xcompression-level 19 -b 1M` (new)** | **1451 MB** | **6.2 s** | **4.8 s** |
| zstd level 15 | 1525 MB | 11.2 s* | 8.2 s* |
| lz4 `-Xhc` | 1768 MB | 11.0 s | 6.3 s |

\* noisy run; zstd decode speed is essentially independent of level, so
level 19 (smallest) wins. unpackfs reads through a single rsync, i.e.
one decompression stream, so the 1-thread column is the one that maps
to install time.

### What changed and why

| change | where | saving |
|---|---|---|
| squashfs xz → zstd-19 (+64 MB / +4.6 %, ISO ≈ 1.98 GB, still under the 2 GiB release-asset cap) | `profiledef.sh` | unpackfs 75 → 28 s |
| one initramfs: stock `hook.preset` (`PRESETS=('default')`) instead of a hand-written default+fallback preset; the 209 MB fallback image is gone | `target-setup early` | initcpio 18 → 2 s |
| kernel-install `layout=other` when the ESP is `/boot`: the bootloader step no longer builds a 2nd initramfs into `/boot/<machine-id>/`; `target-setup final` writes a standard Arch entry (`/vmlinuz-linux`, `/initramfs-linux.img`, options from Calamares' `/etc/kernel/cmdline`) | `target-setup early/final`, settings.conf (shellprocess moved after bootloader) | bootloader 5 → 2 s |
| kernel copied from `/usr/lib/modules/<kver>/vmlinuz` (already on disk) instead of rsync'ing the ISO boot dir, which also put the 237 MB live archiso initramfs on the ESP | `unpackfs.conf`, `target-setup early` | ~1–2 s, 237 MB less ESP writing |
| `/root/ogos-repo` excluded from the copy instead of copied then deleted | `unpackfs.conf` | small |
| keyring: first-boot oneshot (persistent) instead of tmpfs re-init on every boot | `target-setup final` | 0 at install; −5 s CPU on every boot after the first |

Correctness fixes that came out of the same audit (the old flow
"succeeded" but produced these):

- **Kernel updates broke boot.** The boot entry pointed at
  kernel-install's private copy of kernel+initramfs, which Arch's pacman
  hooks never update; after the first `linux` upgrade it booted the old
  kernel with its modules deleted. The entry now uses the files the
  stock `90-mkinitcpio-install` hook maintains.
- **ESP nearly full**: 268 of 300 MiB used (fallback + duplicate
  initramfs); now 29 MiB.
- Live-only state copied onto every install, now deleted by
  `target-setup final`: root autologin on tty1, sshd
  `PermitRootLogin yes` drop-in, sysusers.d/tmpfiles.d entries that
  recreated `liveuser` at every boot, volatile (RAM-only) journald,
  lid-switch/suspend disabled, livecd units, choose-mirror,
  archiso motd/`.zlogin`/`.automated_script.sh`, leftover `liveuser`
  group.
- No pacman keyring on disk (and a new local master key generated on
  every boot) — see the keyring row above.
- No `[ogos]` repo in the installed `/etc/pacman.conf`, i.e. no update
  channel for the OG suite.
- bluetooth.service was never enabled (`services-systemd.conf`).
- First user got uid 1001 (`users` ran before `removeuser`); now 1000.

### Not done (and why)

- **`unpackfsc` (C++ unpackfs, multi-threaded `unsquashfs`)** would cut
  the copy further (host: 4.8 s vs 6.2 s decode, and no rsync per-file
  overhead), but it can't exclude paths: the mount module has already
  bind-mounted `/dev`, `/proc`, `/sys`, `/run` into the target, and
  unsquashfs would set attributes on those mount points. Worth a
  dedicated test, not needed for the target.
- Removing live-only *packages* (calamares, archiso bits): a pacman
  transaction costs seconds, and dropping `mkinitcpio-archiso` would
  re-trigger mkinitcpio via its pacman hook. They're inert on an
  installed system.
- **BIOS**: unchanged and still untested by this harness (OVMF only).
  Calamares' bootloader module installs GRUB there; `target-setup`
  only touches the systemd-boot entry when `/boot/loader/loader.conf`
  exists and the ESP is `/boot`.
- Real hardware: expect unpackfs to be bounded by the USB stick
  (1.45 GB squashfs: ~10–15 s on USB 3, ~50 s on a USB 2 stick) and
  SSD writes (3.9 GB) rather than by decompression, i.e. roughly 30–70 s
  end to end. Not measured on hardware yet.

### Reproducing the "after" numbers without root

`mkarchiso` needs root; the respin used here doesn't. In a user
namespace with a full subuid map (`unshare --map-auto -r`), unsquashfs
an existing ISO's `airootfs.sfs`, overlay this tree's `airootfs/`, fix
the `profiledef.sh` `file_permissions`, `mksquashfs` with the
`profiledef.sh` options, and swap it into the ISO with
`xorriso -indev old.iso -outdev test.iso -boot_image any replay -update
new.sfs /ogos/x86_64/airootfs.sfs`. The package set is the old ISO's,
so this validates installer config and image compression, not new
packages.
