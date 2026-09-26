# Suite-wide patched crates

Hand-patched copies of upstream crates, wired in via `[patch.crates-io]`
from every app that uses them (og-bar, og-wallpaper). Keep patches minimal
and documented here; re-apply on any version bump.

## layershellev 0.13.7
- Popup `xdg_positioner` gets `set_gravity`/`set_constraint_adjustment` so
  popups near a screen edge are slid on-screen by the compositor.
- Handles `zwlr_layer_surface_v1.closed`: destroys the unit's surface and
  forwards `DispatchMessage::Closed`. Stock crate ignored it; the next
  present to the dead surface blocked forever in mesa's Vulkan WSI and
  froze the whole process (seen on every DPMS wake — sway re-creates the
  DP-1 output).
- Event loop no longer polls: stock code dispatched calloop with a 1 ms
  timeout to check for messages from its forwarding thread (~1000 wakeups
  a second, forever). The thread now wakes the loop with a calloop `Ping`;
  the dispatch timeout is only a 1 s fallback.

## iced_layershell 0.13.7
- Forwards `LayerEvent::XdgInfoChanged` into `output_registry` so an
  AllScreens app can learn which output each of its surfaces is on.
- Maps `DispatchMessage::Closed` to the existing `WindowRemoved` path via a
  reverse alias lookup (`output_registry::iced_id_for`).
- `multi_window.rs`: removed the redraw feedback loop. Each frame pushed its
  own `RedrawRequested` back into `events`; the next `NormalUpdate` fed it
  through `ui.update()` (always `Updated`) and queued another redraw, so an
  idle bar re-laid-out and redrew nonstop (~3200 ui updates/s, 32k allocs/s,
  ~11% CPU; days of that churn fragmented glibc's heap to 44 GB RSS). Now
  only real input/messages or a widget's own `NextFrame` request redraw,
  like iced_winit. Also dropped a duplicate `ui.draw()` per frame.
- `output_registry::remove` also drops the surface's output name.
