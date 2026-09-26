# og-bar — Design & Build Plan

Replacement for waybar, built on OG-toolkit. Goal: full feature parity plus
two things waybar genuinely can't do — real per-cell centering regardless of
bar edge, and full GUI-driven customization (no config-file editing required
for anything a user would normally want to change).

---

## 1. Design philosophy

Every OG-toolkit app follows the same rule: **the GUI is the source of truth,
the config file is just where it's persisted.** og-settings never asks a user
to hand-edit JSON — color pickers, toggles, sliders. og-bar extends that rule
to the bar itself:

- A user who's comfortable with config files never needs to touch one.
- A user who's not comfortable with config files has two GUI entry points
  that edit the *same* underlying config, live:
  1. **Popout menu** — quick, on-the-spot editing, opened directly from the
     bar (a persistent gear icon that's always present, immune to being
     configured away).
  2. **og-settings "Bar" tab** — the full editor, same depth as the Theme or
     Power tabs today, for people who want to sit down and configure
     everything at once rather than nudge things live.
- Both write to the same `BarConfig` (living in `og-config`, next to the
  existing `Config`). og-bar watches the config file for changes (same
  pattern og-files already uses for `reload_theme_colors`) and re-renders
  live — no restart, no "Apply & Save" required to *see* a change, though
  Apply & Save still exists for anything that needs a full relayout.

## 2. Architecture

| Piece | Choice | Why |
|---|---|---|
| GUI/render | `iced_layershell = "=0.13.7"` | Verified via `cargo tree`: pulls `iced_core 0.13.2` — the exact line og-config/og-theme already build against. No version split, no shim. There's a known working reference (`lala-bar`) proving the approach is sound, not speculative. |
| Config schema | `og-config::BarConfig` | New struct alongside the existing `Config`. Same load/save machinery. |
| Theming | `og-theme::AppColors` | Bar re-themes automatically the moment the user changes system colors, same as every other app — zero bar-specific theme code. |
| Layer-shell bootstrap | Own module in og-bar (or promoted to `og-wayland` once proven) | og-wayland's current `overlay` module is click-through and single-purpose (notification effects) — the bar needs input, multiple persistent surfaces, and per-output placement. Build it in og-bar first; only promote to og-wayland if a second app ever needs it (matches how og-wayland grew organically from og-notify + og-files so far). |
| Tray/SNI | New small module, own dbus SNI-host via `zbus` (already a proven dependency — og-notify uses it) | No existing dependency gives this for free under iced_layershell. Biggest real unknown — gets its own spike phase. |
| Popup surfaces (menus, popout config panel) | One reusable "anchored popup" component, its own layer-shell surface positioned near the triggering module | Same underlying mechanism serves: power menu, network menu, bluetooth menu, tray item menus, *and* the customization popout. Build once. |

## 3. Feature-parity checklist (from the current live waybar config)

- [ ] `sway/workspaces`-equivalent: numbered indicators, per-app-id/class icon
      rewrite table (currently ~30 entries — user-editable list, not hardcoded)
- [ ] Launcher buttons (icon + tooltip + click action), user-addable/removable,
      not a fixed set — today's list (claude, terminal, og-settings, og-apps,
      clipboard, notifications, sysctl, power) becomes a *default* preset, not
      a hardcoded module list
- [ ] CPU / memory: interval-polled, warning/critical color states
- [ ] Tray (StatusNotifierItem)
- [ ] Bluetooth: status icon + right-click menu
- [ ] Network: status icon + right-click menu, left-click opens og-settings'
      Network tab (existing `--tab` CLI arg, already wired)
- [ ] Pulseaudio: icon + volume, click/scroll actions
- [ ] Clock, multiple instances with independent timezones
- [ ] Power menu (lock/suspend/logout/reboot/shutdown)
- [ ] Bar: edge (top/bottom/left/right), thickness — **all four edges, not
      just top/left like today's real config uses**

## 4. Config schema (`og-config::BarConfig`)

```rust
pub struct BarConfig {
    pub position: Edge,              // Top, Bottom, Left, Right
    pub thickness: u32,              // bar's own width/height depending on orientation
    pub item_size: u32,              // every module's cell footprint — one knob,
                                      // not per-module CSS min-width/min-height hunting
    pub spacing: u32,
    pub padding: u32,
    pub modules_start: Vec<ModuleConfig>,
    pub modules_center: Vec<ModuleConfig>,
    pub modules_end: Vec<ModuleConfig>,   // "start/center/end", not "left/center/right" —
                                           // orientation-agnostic naming so the same
                                           // config means the same thing on any edge
}

pub struct ModuleConfig {
    pub kind: ModuleKind,             // Workspaces, Clock{tz}, Cpu, Memory, Tray,
                                       // Bluetooth, Network, Pulseaudio, Launcher{...}, ...
    pub enabled: bool,
    pub size_override: Option<u32>,   // per-module escape hatch from the global item_size
    pub custom: serde_json::Value,    // module-specific fields (launcher icon/command/
                                       // tooltip, clock timezone, icon rewrite rules, ...)
}
```

Both GUI entry points (popout + og-settings tab) read/write this struct
through the same `og_config::BarConfig::load()/save()` — no divergent code
paths, no risk of the two editors disagreeing about what a field means.

## 5. Module system

A small trait, not a hardcoded match statement per feature — adding a new
module later (a to-do widget, a weather module, whatever comes up) means
implementing one trait, not touching bar layout code:

```rust
trait Module {
    fn view(&self, colors: &AppColors, size: u32) -> Element<Message>;
    fn poll(&mut self) -> Option<Task<Message>>;  // for interval-based modules (cpu, clock)
}
```

Every module renders into a fixed `item_size × item_size` (or `item_size ×
thickness` for edge-spanning ones like workspaces) cell, so...

## 6. Centering & sizing — the actual point of this rebuild

Waybar's centering is GTK label alignment inside a variably-sized module box
— it never really centers a multi-line icon+number combo, it approximates.
og-bar's fix: every module is rendered inside
`Container::new(content).width(item_size).height(item_size).center_x().center_y()`.
That's a *real* fixed-size cell with *real* centering, not padding tricks.

- **Horizontal bars** (top/bottom): modules laid out in a `Row`, each cell
  centers its content vertically and horizontally within its own slot.
- **Vertical bars** (left/right): same modules laid out in a `Column`
  instead. Text stays upright and stacked (like the current clock's
  `%Z\n%m|%d\n...` trick), not rotated glyphs — but now genuinely centered
  in both axes per cell, which is the thing that doesn't actually work in
  waybar today.
- One code path handles all four edges — the only thing that changes is
  `Row` vs `Column` and which edge the layer-shell surface anchors to.

## 7. Popout customization menu

- Always-present gear icon on the bar itself (can't be configured away —
  otherwise a user could lock themselves out of customizing further).
- Click opens a small anchored popup (the same reusable popup-surface
  mechanism as the power/network/bluetooth menus) positioned next to the
  bar, near the gear icon.
- Contents: edge picker, thickness slider, item-size slider, spacing/padding,
  drag-reorderable module list per section (start/center/end), per-module
  enable toggle. Changes apply live — the bar re-renders itself in place
  as the user drags a slider, not after closing the popup.
- Same `BarConfig` write path as og-settings' Bar tab — this is genuinely
  the same editor in a smaller frame, not a separate implementation.

## 8. og-settings "Bar" tab

Full-size version of the same editor — better for bigger changes (adding/
removing several modules, picking new launcher icons, editing the app-id
icon-rewrite table) where a small popout is cramped. Same underlying
`BarConfig` widget code, reused between the tab and the popout content
where practical (same iced `view` function, different container size).

## 9. Tray / SNI — spike before committing

Waybar's tray wraps an existing SNI-watcher. Nothing under iced_layershell
gives this for free. Plan: small standalone dbus SNI-host using `zbus`
(already proven in og-notify), spiked in isolation before it's wired into
the bar layout, so a dead end here doesn't block everything else.

## 10. Multi-output

waybar auto-spawns one bar per connected output from a single process.
iced_layershell's multi-surface story runs through iced's `daemon()`
API rather than `application()` (different construction path than every
other OG-toolkit app so far, which are all single-window `application()`
apps) — needs its own small spike to confirm before the real build.

## 11. Migration plan

- og-bar runs *alongside* waybar during development — different `layer`/
  position if needed, or just tested on a spare workspace — waybar keeps
  running the real desktop until og-bar has real parity.
- Cutover: swap sway's waybar autostart line for og-bar's, keep the old
  waybar config/binary around untouched for a fallback window, remove once
  confirmed stable for a while.
- No renaming/deleting waybar's own config during development — it's the
  reference implementation being matched against.

## 12. Build order

1. **Skeleton**: single horizontal top bar, layer-shell surface up, empty.
   Confirms iced_layershell + og-theme integration works at all.
2. **Workspaces + clock + 2-3 launcher buttons**, prove real centering on
   a horizontal bar.
3. **Vertical orientation**: left/right edges, `Row`→`Column` swap, confirm
   centering holds.
4. **Popup-surface mechanism**: build the one reusable anchored-popup
   component. First consumer: the power menu (simplest of the three menus).
5. **Popout customization menu**, using the popup-surface mechanism from
   step 4. This is the core "GUI-first" deliverable — prioritize it early,
   not as a late add-on.
6. **og-settings Bar tab**, reusing the popout's editor widget code.
7. **Remaining pollers**: cpu, memory, network, bluetooth, pulseaudio.
8. **Tray/SNI spike**, wired in once proven.
9. **Multi-output spike**, wired in once proven.
10. **Cutover** per the migration plan above.

Steps 4–5 are pulled forward deliberately — customization-from-the-GUI is
the actual point of this project, not a nice-to-have bolted on at the end.

## 13. Open risks

- Tray/SNI: real unknown, own spike (step 8).
- Multi-output under iced's `daemon()` API: real unknown, own spike (step 9).
- Popup-surface positioning (anchoring a small popup precisely next to a
  bar module, across all four bar edges) is new ground — no existing OG-
  toolkit app has built a secondary anchored surface like this yet.
