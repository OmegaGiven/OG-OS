use iced_layershell::to_layer_message;
use og_config::{Edge, ModuleKind, SectionAlign};

use crate::modules::bluetooth::BluetoothAction;
use crate::modules::tray::{TrayItem, TrayMenuEntry};
use crate::modules::workspaces::WorkspaceInfo;
use crate::power::PowerAction;

#[to_layer_message(multi)]
#[derive(Debug, Clone)]
pub enum Message {
    WorkspacesUpdated(Vec<WorkspaceInfo>),
    // Output names (e.g. "DP-3") that currently have a fullscreen window
    // anywhere on them — bar.rs shrinks that output's surface to nothing
    // (SizeChange, not LayerChange — switching layer dynamically breaks
    // pointer input compositor-wide, a wlroots/sway seat-focus quirk) so
    // it neither covers the fullscreen content nor sits there catching
    // hover/hidden-focus like a full-size invisible surface would.
    FullscreenOutputsUpdated(std::collections::HashSet<String>),
    FocusWorkspace(String),
    TrayUpdated(Vec<TrayItem>),
    TrayActivate(String),
    // Entries flattened (and menu_path carried) directly from the click
    // site (tray.rs's view()) rather than looked back up through the
    // type-erased `Box<dyn Module>` list afterward — same reasoning as
    // WindowDragStart's icon glyph above.
    TrayContextMenu(String, String, Vec<TrayMenuEntry>),
    TrayMenuItemActivate(String, String, i32),
    TrayHover(String, bool),
    /// Result of a fire-and-forget task; carries nothing to update.
    Noop,
    // Per-module readings. Each module's subscription only emits these when
    // the displayed value actually changed (see module::poll_changes), so a
    // quiet system produces no messages -> no rebuild/redraw.
    CpuUsage(u32),
    MemoryUsage(u32),
    GpuUsage(Option<crate::modules::gpu::GpuSample>),
    NotificationCount(u32),
    PulseaudioState(u32, bool),
    BluetoothPowered(bool),
    NetworkLink(crate::modules::network::LinkState),
    ClockMinute,
    /// Periodic liveness ping for systemd's watchdog (see `watchdog.rs`).
    WatchdogPing,
    Launch(String),
    PulseaudioToggleMute,
    PulseaudioHover(bool),
    // Waybar's old right-click menus offered these as the actual quick
    // actions people used (the rest — device list, adapter settings, send
    // files — is one left-click away via og-settings' Network tab, which
    // already has full bluetooth/wifi management). Both toggle current
    // state, same as PulseaudioToggleMute above.
    BluetoothTogglePower,
    WifiTogglePower,
    BluetoothHover(bool),
    NetworkHover(bool),
    OpenBluetoothMenu,
    BluetoothAction(BluetoothAction),
    OpenPowerMenu,
    PowerAction(PowerAction),
    ClosePopup,
    ClickOnWindow(iced::window::Id),
    OpenSettingsPopup,
    SettingsButtonHover(bool),
    SetEdge(Edge),
    SetThickness(u32),
    SetItemSize(u32),
    SetSpacing(u32),
    SetPadding(u32),
    // section index (position in bar_cfg.sections at render time, not
    // BarSection::id — messages round-trip within a single synchronous
    // update() from a just-rendered view, so the list can't shift under
    // them in a way an index would get wrong).
    ToggleModule(usize, usize),
    RemoveModule(usize, usize),
    AddModule(usize, ModuleKind),
    SetClockTimezone(usize, usize, String),
    AddSection,
    RemoveSection(usize),
    SetSectionPercent(usize, u32),
    SetSectionAlign(usize, SectionAlign),
    ApplyRelayout,
    OpenWindowMenu(i64, i32),
    WindowMenuClose(i64),
    WindowMenuMoveToWorkspace(i64, i32),
    // Click-hold-drag an app icon onto another workspace's group to move
    // it there. Start carries the window's *current* workspace so the
    // eventual drop can tell "still over where it started" (no-op) apart
    // from "actually moved somewhere else."
    // Icon glyph carried directly so the drag-ghost overlay (bar.rs's
    // `bar_view`) doesn't need to look the dragged window back up through
    // the type-erased `Box<dyn Module>` list — the click site already has
    // it right there in hand.
    WindowDragStart(i64, i32, String),
    WorkspaceGroupHovered(i32),
    WindowDragCursorMoved(iced::Point),
    WindowDragEnd,
    // Whole-surface hover, only wired up when bar_cfg.auto_hide is on —
    // drives reveal-on-hover (see bar.rs's use of Message::SizeChange).
    BarHoverChanged(iced::window::Id, bool),
}
