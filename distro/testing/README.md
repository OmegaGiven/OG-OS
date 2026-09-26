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

- The Partitions page's "Erase disk" choice and the Users page's field
  positions in `test_calamares_install.py` use percentage-of-window
  guesses, not measured pixel offsets like the Next button. They
  haven't been click-tested yet. If a run doesn't behave as expected
  on those pages, that's the likely reason — recalibrate them the same
  way `calamares_next_button()` was derived.
- A full run through Partitions/Users/Summary/Install/Finish hasn't
  been done end-to-end yet — only Welcome/Location/Keyboard are
  confirmed.

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

Screenshots land in `/tmp/ogos-test-shots/` by default — review them
for the pages not yet confirmed (Partitions onward).
