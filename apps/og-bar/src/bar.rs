use iced::widget::{column, container, mouse_area, row, text};
use iced::{Background, Border, Color, Element, Length, Subscription, Task};

use iced_layershell::actions::{IcedNewMenuSettings, MenuDirection};

use og_config::{BarConfig, BarSection, Config, Edge, ModuleConfig, ModuleKind, SectionAlign};
use og_theme::AppColors;

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use crate::modules::bluetooth::Bluetooth;
use crate::modules::clipboard::Clipboard;
use crate::modules::clock::Clock;
use crate::modules::cpu::Cpu;
use crate::modules::gpu::Gpu;
use crate::modules::launcher::Launcher;
use crate::modules::memory::Memory;
use crate::modules::network::Network;
use crate::modules::notifications::Notifications;
use crate::modules::pulseaudio::Pulseaudio;
use crate::modules::tray::Tray;
use crate::modules::workspaces::Workspaces;
use crate::popup::{PopupKind, PopupState};
use crate::power::PowerButton;
use crate::settings::SettingsButton;

fn build_modules(list: &[ModuleConfig], icon_rewrite: &[og_config::IconRewriteRule]) -> Vec<Box<dyn Module>> {
    list.iter()
        .filter(|m| m.enabled)
        .filter_map(|m| -> Option<Box<dyn Module>> {
            match &m.kind {
                ModuleKind::Workspaces => Some(Box::new(Workspaces::new(icon_rewrite.to_vec()))),
                ModuleKind::Clock { timezone, hour12, show_timezone, show_date } => {
                    Some(Box::new(Clock::new(timezone.clone(), *hour12, *show_timezone, *show_date)))
                }
                ModuleKind::Launcher { icon, command, .. } => {
                    Some(Box::new(Launcher::new(icon.clone(), command.clone())))
                }
                ModuleKind::Power => Some(Box::new(PowerButton)),
                ModuleKind::Settings => Some(Box::new(SettingsButton::new())),
                ModuleKind::Cpu => Some(Box::new(Cpu::new())),
                ModuleKind::Gpu => Some(Box::new(Gpu::new())),
                ModuleKind::Memory => Some(Box::new(Memory::new())),
                ModuleKind::Network => Some(Box::new(Network::new())),
                ModuleKind::Bluetooth => Some(Box::new(Bluetooth::new())),
                ModuleKind::Pulseaudio => Some(Box::new(Pulseaudio::new())),
                ModuleKind::Notifications => Some(Box::new(Notifications::new())),
                ModuleKind::Clipboard => Some(Box::new(Clipboard)),
                ModuleKind::Tray => Some(Box::new(Tray::new())),
            }
        })
        .collect()
}

/// A runtime section pairs the config-driven layout knobs (percent/align)
/// with the actual built module instances — rebuilt wholesale by
/// `rebuild_modules` any time the module list changes, same as the old
/// fixed start/center/end fields were.
struct SectionRuntime {
    percent: u32,
    align: SectionAlign,
    modules: Vec<Box<dyn Module>>,
}

fn build_sections(cfg: &BarConfig) -> Vec<SectionRuntime> {
    cfg.sections
        .iter()
        .map(|s| SectionRuntime {
            percent: s.percent,
            align: s.align,
            modules: build_modules(&s.modules, &cfg.icon_rewrite),
        })
        .collect()
}

pub struct Bar {
    colors: AppColors,
    bar_cfg: BarConfig,
    sections: Vec<SectionRuntime>,
    popup: Option<PopupState>,
    /// Click-hold-drag state for moving an app icon to another
    /// workspace — (con_id, workspace it started on).
    drag_origin: Option<(i64, i32)>,
    hover_ws: Option<i32>,
    /// Icon glyph of the window being dragged and the latest cursor
    /// position, both `None` outside a drag — together they're what
    /// `bar_view` needs to render the og-drag ghost overlay at the
    /// cursor. `drag_cursor` only starts updating once a `CursorMoved`
    /// actually arrives after the press (see `subscription`), rather than
    /// snapping to some stale position from before the drag began.
    drag_icon: Option<String>,
    drag_cursor: Option<iced::Point>,
    /// Output names (e.g. "DP-3") with a fullscreen window on them right
    /// now — takes priority over auto_hide/hover in desired_size().
    fullscreen_outputs: std::collections::HashSet<String>,
    /// Surfaces currently hovered — only meaningful when bar_cfg.auto_hide
    /// is set; drives reveal-on-hover in desired_size().
    hovered: std::collections::HashSet<iced::window::Id>,
    /// Last size applied per surface, so SizeChange only goes out when
    /// the desired size actually changes, not on every event that could
    /// have affected it.
    applied_size: std::collections::HashMap<iced::window::Id, (u32, u32)>,
}

impl Bar {
    pub fn new() -> (Self, Task<Message>) {
        let bar_cfg = BarConfig::load();
        let config = Config::load();
        let colors = AppColors::from_config(&config, "og-bar");
        let sections = build_sections(&bar_cfg);
        let bar = Self {
            sections, colors, bar_cfg, popup: None,
            drag_origin: None, hover_ws: None, drag_icon: None, drag_cursor: None,
            fullscreen_outputs: std::collections::HashSet::new(),
            hovered: std::collections::HashSet::new(),
            applied_size: std::collections::HashMap::new(),
        };
        (bar, Task::none())
    }

    fn all_modules_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Module>> {
        self.sections.iter_mut().flat_map(|s| s.modules.iter_mut())
    }

    fn all_modules(&self) -> impl Iterator<Item = &Box<dyn Module>> {
        self.sections.iter().flat_map(|s| s.modules.iter())
    }
}

fn rebuild_modules(bar: &mut Bar) {
    bar.sections = build_sections(&bar.bar_cfg);
}

/// Bar reveal strip thickness while auto_hide is on and the surface isn't
/// hovered — thin enough to be unobtrusive, thick enough to reliably
/// catch the pointer at the screen edge.
pub const REVEAL_STRIP: u32 = 4;

/// The surface size a given window should have right now: shrunk to
/// nothing if its output is fullscreen (takes priority — see the
/// fullscreen-focus-loss finding this exists to work around), else the
/// reveal strip if auto_hide is on and it isn't hovered, else full size.
fn desired_size(bar: &Bar, output_name: Option<&str>, hovered: bool) -> (u32, u32) {
    if output_name.is_some_and(|n| bar.fullscreen_outputs.contains(n)) {
        return (1, 1);
    }
    let (main_zero, thickness) = match bar.bar_cfg.position {
        Edge::Top | Edge::Bottom => (true, bar.bar_cfg.thickness),
        Edge::Left | Edge::Right => (false, bar.bar_cfg.thickness),
    };
    let cross = if bar.bar_cfg.auto_hide && !hovered { REVEAL_STRIP } else { thickness };
    if main_zero { (0, cross) } else { (cross, 0) }
}

/// Recomputes every known surface's desired size and emits SizeChange
/// only for the ones that actually changed — called after anything that
/// could affect it (fullscreen state, hover state).
fn sync_sizes(bar: &mut Bar) -> Task<Message> {
    let changes: Vec<Message> = iced_layershell::output_registry::known_ids()
        .into_iter()
        .filter_map(|(id, name)| {
            let hovered = bar.hovered.contains(&id);
            let size = desired_size(bar, name.as_deref(), hovered);
            if bar.applied_size.get(&id) == Some(&size) {
                return None;
            }
            bar.applied_size.insert(id, size);
            Some(Message::SizeChange { id, size })
        })
        .collect();
    if changes.is_empty() {
        Task::none()
    } else {
        Task::batch(changes.into_iter().map(Task::done))
    }
}

pub fn remove_id(bar: &mut Bar, id: iced::window::Id) {
    bar.applied_size.remove(&id);
    bar.hovered.remove(&id);
    if bar.popup.as_ref().is_some_and(|p| p.id == id) {
        bar.popup = None;
    }
}

pub fn update(bar: &mut Bar, message: Message) -> Task<Message> {
    match &message {
        Message::WatchdogPing => {
            crate::watchdog::ping();
            return Task::none();
        }
        Message::FocusWorkspace(name) => {
            let name = name.clone();
            return Task::future(async move {
                let _ = tokio::process::Command::new("swaymsg")
                    .arg(format!("workspace {name}"))
                    .output()
                    .await;
                Message::Noop
            });
        }
        Message::WindowDragStart(con_id, origin_ws, icon) => {
            let con_id = *con_id;
            bar.drag_origin = Some((con_id, *origin_ws));
            bar.hover_ws = None;
            bar.drag_icon = Some(icon.clone());
            bar.drag_cursor = None;
            // Same focus-on-click behavior FocusWindow used to provide —
            // a plain click is just a press+release with no group-hover
            // change in between, so this fires every time regardless of
            // whether a drag follows.
            return Task::future(async move {
                let _ = tokio::process::Command::new("swaymsg")
                    .arg(format!("[con_id={con_id}] focus"))
                    .output()
                    .await;
                Message::Noop
            });
        }
        Message::WorkspaceGroupHovered(ws_num) => {
            bar.hover_ws = Some(*ws_num);
        }
        Message::WindowDragCursorMoved(pos) => {
            if bar.drag_origin.is_some() {
                bar.drag_cursor = Some(*pos);
            }
        }
        Message::WindowDragEnd => {
            bar.drag_icon = None;
            bar.drag_cursor = None;
            if let Some((con_id, origin_ws)) = bar.drag_origin.take() {
                if let Some(target_ws) = bar.hover_ws.take() {
                    if target_ws != origin_ws {
                        return Task::future(async move {
                            let _ = tokio::process::Command::new("swaymsg")
                                .arg(format!("[con_id={con_id}] move to workspace number {target_ws}"))
                                .output()
                                .await;
                            Message::Noop
                        });
                    }
                }
            }
            bar.hover_ws = None;
        }
        Message::TrayActivate(address) => {
            let address = address.clone();
            return Task::future(async move {
                crate::modules::tray::activate(address).await;
                Message::Noop
            });
        }
        Message::TrayContextMenu(address, menu_path, entries) => {
            let id = iced::window::Id::unique();
            bar.popup = Some(PopupState {
                id,
                kind: PopupKind::Tray { address: address.clone(), menu_path: menu_path.clone(), entries: entries.clone() },
            });
            let direction = match bar.bar_cfg.position {
                Edge::Bottom => MenuDirection::Up,
                Edge::Top | Edge::Left | Edge::Right => MenuDirection::Down,
            };
            return Task::done(Message::NewMenu {
                settings: IcedNewMenuSettings { size: (220, 320), direction },
                id,
            });
        }
        Message::TrayMenuItemActivate(address, menu_path, submenu_id) => {
            let (address, menu_path, submenu_id) = (address.clone(), menu_path.clone(), *submenu_id);
            if let Some(popup) = bar.popup.take() {
                let close = Task::done(Message::RemoveWindow(popup.id));
                let fire = Task::future(async move {
                    crate::modules::tray::activate_menu_item(address, menu_path, submenu_id).await;
                    Message::Noop
                });
                return Task::batch([close, fire]);
            }
        }
        Message::Launch(cmd) => {
            let cmd = cmd.clone();
            // og-bar's own systemd unit has a bare PATH (/usr/local/bin:
            // /usr/bin, no ~/.local/bin — same root cause as sway's own
            // exec environment, see known-issues.md) and every launcher
            // here now spawns bare command names (og-settings, og-clip,
            // og-notif-center-launch, ...) rather than full paths. Rather
            // than hunt down every call site, prepend ~/.local/bin once,
            // here, so bare names resolve regardless of the caller.
            let home = std::env::var("HOME").unwrap_or_default();
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("PATH=\"{home}/.local/bin:$PATH\" setsid {cmd} >/dev/null 2>&1 &"))
                .spawn();
        }
        // Toggles run off the UI thread. Pulseaudio and bluetooth need no
        // reply (their event streams see the change); wifi reports the
        // resulting link state straight back.
        Message::PulseaudioToggleMute => {
            return Task::future(async {
                let _ = tokio::process::Command::new("pactl")
                    .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
                    .output()
                    .await;
                Message::Noop
            });
        }
        // Bluetooth needs no reply either: BlueZ's PropertiesChanged
        // signal reaches the module's D-Bus stream.
        Message::BluetoothTogglePower => {
            return Task::future(async {
                crate::modules::bluetooth::toggle_powered().await;
                Message::Noop
            });
        }
        Message::WifiTogglePower => {
            return Task::future(async {
                let link = tokio::task::spawn_blocking(|| {
                    let action = if crate::modules::network::is_wifi_powered() { "block" } else { "unblock" };
                    let _ = std::process::Command::new("rfkill").args([action, "wifi"]).output();
                    crate::modules::network::detect_link()
                })
                .await
                .unwrap_or(crate::modules::network::LinkState::Disconnected);
                Message::NetworkLink(link)
            });
        }
        Message::OpenBluetoothMenu => {
            let id = iced::window::Id::unique();
            bar.popup = Some(PopupState { id, kind: PopupKind::Bluetooth });
            let direction = match bar.bar_cfg.position {
                Edge::Bottom => MenuDirection::Up,
                Edge::Top | Edge::Left | Edge::Right => MenuDirection::Down,
            };
            return Task::done(Message::NewMenu {
                settings: IcedNewMenuSettings { size: (160, 190), direction },
                id,
            });
        }
        Message::BluetoothAction(action) => {
            use crate::modules::bluetooth::BluetoothAction;
            let power = match action {
                BluetoothAction::PowerOn => Some(true),
                BluetoothAction::PowerOff => Some(false),
                _ => None,
            };
            let set_power = match power {
                Some(on) => Task::future(async move {
                    crate::modules::bluetooth::set_powered(on).await;
                    Message::Noop
                }),
                None => {
                    action.run();
                    Task::none()
                }
            };
            if let Some(popup) = bar.popup.take() {
                return Task::batch([set_power, Task::done(Message::RemoveWindow(popup.id))]);
            }
            return set_power;
        }
        Message::OpenPowerMenu => {
            let id = iced::window::Id::unique();
            bar.popup = Some(PopupState { id, kind: PopupKind::Power });
            // Top/vertical bars: menu should open below the click point.
            // Bottom bar: open above it instead, so it doesn't run off-screen.
            let direction = match bar.bar_cfg.position {
                Edge::Bottom => MenuDirection::Up,
                Edge::Top | Edge::Left | Edge::Right => MenuDirection::Down,
            };
            return Task::done(Message::NewMenu {
                settings: IcedNewMenuSettings { size: (140, 190), direction },
                id,
            });
        }
        Message::PowerAction(action) => {
            action.run();
            if let Some(popup) = bar.popup.take() {
                return Task::done(Message::RemoveWindow(popup.id));
            }
        }
        Message::ClosePopup => {
            if let Some(popup) = bar.popup.take() {
                return Task::done(Message::RemoveWindow(popup.id));
            }
        }
        Message::ClickOnWindow(window) => {
            if let Some(popup) = &bar.popup {
                if popup.id != *window {
                    let id = popup.id;
                    bar.popup = None;
                    return Task::done(Message::RemoveWindow(id));
                }
            }
        }
        Message::OpenWindowMenu(con_id, current_ws) => {
            let (con_id, current_ws) = (*con_id, *current_ws);
            let id = iced::window::Id::unique();
            let other_workspaces = fetch_other_workspace_nums(current_ws);
            bar.popup = Some(PopupState { id, kind: PopupKind::WindowMenu { con_id, other_workspaces } });
            let direction = match bar.bar_cfg.position {
                Edge::Bottom => MenuDirection::Up,
                Edge::Top | Edge::Left | Edge::Right => MenuDirection::Down,
            };
            return Task::done(Message::NewMenu {
                settings: IcedNewMenuSettings { size: (160, 260), direction },
                id,
            });
        }
        Message::WindowMenuClose(con_id) => {
            let _ = std::process::Command::new("swaymsg")
                .arg(format!("[con_id={con_id}] kill"))
                .output();
            if let Some(popup) = bar.popup.take() {
                return Task::done(Message::RemoveWindow(popup.id));
            }
        }
        Message::WindowMenuMoveToWorkspace(con_id, ws_num) => {
            let _ = std::process::Command::new("swaymsg")
                .arg(format!("[con_id={con_id}] move to workspace number {ws_num}"))
                .output();
            if let Some(popup) = bar.popup.take() {
                return Task::done(Message::RemoveWindow(popup.id));
            }
        }
        Message::OpenSettingsPopup => {
            let id = iced::window::Id::unique();
            bar.popup = Some(PopupState { id, kind: PopupKind::Settings });
            let direction = match bar.bar_cfg.position {
                Edge::Bottom => MenuDirection::Up,
                Edge::Top | Edge::Left | Edge::Right => MenuDirection::Down,
            };
            return Task::done(Message::NewMenu {
                settings: IcedNewMenuSettings { size: (260, 420), direction },
                id,
            });
        }
        // Edge/thickness need a real wlr layer-shell relayout this process
        // can't do to its own already-mapped surface. Edge is a discrete
        // button click, not a drag — nothing is lost by applying it (i.e.
        // respawning) the instant it's clicked, and requiring a separate
        // "Apply" click after was a real discoverability gap (a user
        // clicking Left/Right/Bottom reasonably expects the bar to just
        // move). Thickness is a slider — respawning on every tick of a
        // drag would spam processes, so that one still stages and needs
        // the explicit Apply button.
        Message::SetEdge(edge) => {
            bar.bar_cfg.position = *edge;
            let _ = bar.bar_cfg.save();
            return Task::done(Message::ApplyRelayout);
        }
        Message::SetThickness(v) => {
            bar.bar_cfg.thickness = *v;
            let _ = bar.bar_cfg.save();
        }
        // item_size/spacing/padding are read straight out of bar_cfg by
        // bar_view() every frame, so these apply live with no extra work.
        Message::SetItemSize(v) => {
            bar.bar_cfg.item_size = *v;
            let _ = bar.bar_cfg.save();
        }
        Message::SetSpacing(v) => {
            bar.bar_cfg.spacing = *v;
            let _ = bar.bar_cfg.save();
        }
        Message::SetPadding(v) => {
            bar.bar_cfg.padding = *v;
            let _ = bar.bar_cfg.save();
        }
        Message::ToggleModule(section, index) => {
            if let Some(m) = bar.bar_cfg.sections.get_mut(*section).and_then(|s| s.modules.get_mut(*index)) {
                m.enabled = !m.enabled;
            }
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::RemoveModule(section, index) => {
            if let Some(list) = bar.bar_cfg.sections.get_mut(*section).map(|s| &mut s.modules) {
                if *index < list.len() {
                    list.remove(*index);
                }
            }
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::AddModule(section, kind) => {
            if let Some(s) = bar.bar_cfg.sections.get_mut(*section) {
                s.modules.push(ModuleConfig { kind: kind.clone(), enabled: true, size_override: None });
            }
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::SetClockTimezone(section, index, tz) => {
            if let Some(m) = bar.bar_cfg.sections.get_mut(*section).and_then(|s| s.modules.get_mut(*index)) {
                if let ModuleKind::Clock { timezone, .. } = &mut m.kind {
                    *timezone = tz.clone();
                }
            }
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::AddSection => {
            let id = bar.bar_cfg.next_section_id;
            bar.bar_cfg.next_section_id += 1;
            bar.bar_cfg.sections.push(BarSection { id, percent: 20, align: SectionAlign::Middle, modules: Vec::new() });
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::RemoveSection(index) => {
            if *index < bar.bar_cfg.sections.len() {
                bar.bar_cfg.sections.remove(*index);
            }
            let _ = bar.bar_cfg.save();
            rebuild_modules(bar);
        }
        Message::SetSectionPercent(index, percent) => {
            if let Some(s) = bar.bar_cfg.sections.get_mut(*index) {
                s.percent = *percent;
            }
            let _ = bar.bar_cfg.save();
        }
        Message::SetSectionAlign(index, align) => {
            if let Some(s) = bar.bar_cfg.sections.get_mut(*index) {
                s.align = *align;
            }
            let _ = bar.bar_cfg.save();
        }
        Message::ApplyRelayout => {
            let _ = bar.bar_cfg.save();
            // ApplyRelayout is only ever triggered from a button *inside*
            // the settings popup, which was therefore still open/mapped
            // — killing the process out from under it via a bare
            // process::exit(0) never gives its Wayland surface a clean
            // destroy, just an abrupt disconnect. That's a real
            // candidate for leaving sway's pointer/seat state wedged
            // for whatever respawns next (matches "popup applies once,
            // then the reopened one is unresponsive"). Close the popup
            // properly first (a real RemoveWindow the runtime processes
            // this frame), and only actually respawn+exit after a short
            // delay so that close has time to land before the process
            // dies.
            let popup_id = bar.popup.take().map(|p| p.id);
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
                std::process::exit(0);
            });
            if let Some(id) = popup_id {
                return Task::done(Message::RemoveWindow(id));
            }
        }
        Message::FullscreenOutputsUpdated(outputs) => {
            bar.fullscreen_outputs = outputs.clone();
            return sync_sizes(bar);
        }
        Message::BarHoverChanged(id, hovered) => {
            if *hovered {
                bar.hovered.insert(*id);
            } else {
                bar.hovered.remove(id);
            }
            return sync_sizes(bar);
        }
        _ => {}
    }

    for module in bar.all_modules_mut() {
        module.update(&message);
    }
    Task::none()
}

/// Synchronous — same convention as other quick shell-outs in this
/// codebase; the window menu is transient so a live-updating list isn't
/// worth the complexity.
fn fetch_other_workspace_nums(current_ws: i32) -> Vec<i32> {
    let out = std::process::Command::new("swaymsg").args(["-t", "get_workspaces"]).output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let Some(arr) = v.as_array() else { return Vec::new() };
    let mut nums: Vec<i32> = arr
        .iter()
        .filter_map(|w| w.get("num").and_then(|n| n.as_i64()).map(|n| n as i32))
        .filter(|n| *n != current_ws)
        .collect();
    nums.sort();
    nums
}

pub fn view(bar: &Bar, id: iced::window::Id) -> Element<'_, Message> {
    if let Some(popup) = &bar.popup {
        if popup.id == id {
            return match &popup.kind {
                PopupKind::Power => crate::power::popup_view(bar.colors),
                PopupKind::Settings => crate::settings::popup_view(bar.colors, &bar.bar_cfg),
                PopupKind::WindowMenu { con_id, other_workspaces } => {
                    crate::window_menu::popup_view(bar.colors, *con_id, other_workspaces)
                }
                PopupKind::Tray { address, menu_path, entries } => {
                    crate::modules::tray::popup_view(bar.colors, address, menu_path, entries)
                }
                PopupKind::Bluetooth => crate::modules::bluetooth::popup_view(bar.colors),
            };
        }
    }
    if bar.applied_size.get(&id) == Some(&(1, 1)) {
        return text("").into();
    }

    let content = bar_view(bar);
    if bar.bar_cfg.auto_hide {
        // mouse_area passes events through to content underneath —
        // Length::Fill so it covers the *whole* surface, however small
        // (the reveal strip), not just bar_view's own rendered footprint.
        return mouse_area(container(content).width(Length::Fill).height(Length::Fill))
            .on_enter(Message::BarHoverChanged(id, true))
            .on_exit(Message::BarHoverChanged(id, false))
            .into();
    }
    content
}

fn bar_view(bar: &Bar) -> Element<'_, Message> {
    let size = bar.bar_cfg.item_size;
    let thickness = bar.bar_cfg.thickness as u16;
    let spacing = bar.bar_cfg.spacing as u16;
    let orientation = Orientation::from(bar.bar_cfg.position);

    // Main axis = the direction the bar runs (width for top/bottom, height
    // for left/right). Cross axis is always the bar's fixed thickness.
    fn cell<'a>(m: &'a Box<dyn Module>, bar: &'a Bar, size: u32, orientation: Orientation, thickness: u16) -> Element<'a, Message> {
        let main_len = m.cell_length(size, orientation);
        let content = m.view(bar.colors, size, orientation);
        let c = container(content).center_x(Length::Fill).center_y(Length::Fill);
        match orientation {
            Orientation::Horizontal => c.width(main_len).height(thickness),
            Orientation::Vertical => c.width(thickness).height(main_len),
        }
        .into()
    }

    fn section<'a>(
        modules: &'a [Box<dyn Module>],
        bar: &'a Bar,
        size: u32,
        orientation: Orientation,
        thickness: u16,
        spacing: u16,
    ) -> Element<'a, Message> {
        let cells = modules.iter().map(|m| cell(m, bar, size, orientation, thickness));
        match orientation {
            Orientation::Horizontal => row(cells).spacing(spacing).into(),
            Orientation::Vertical => column(cells).spacing(spacing).into(),
        }
    }

    // Each user-defined section gets `percent` of the bar's length via
    // FillPortion (a relative share, not a strict 0-100 that must sum to
    // 100 — see BarSection's own doc comment) and packs its modules
    // toward Start/Middle/End of that slice.
    let content: Element<'_, Message> = match orientation {
        Orientation::Horizontal => {
            let cols = bar.sections.iter().map(|s| {
                let align = match s.align {
                    SectionAlign::Start => iced::alignment::Horizontal::Left,
                    SectionAlign::Middle => iced::alignment::Horizontal::Center,
                    SectionAlign::End => iced::alignment::Horizontal::Right,
                };
                container(section(&s.modules, bar, size, orientation, thickness, spacing))
                    .width(Length::FillPortion(s.percent.max(1) as u16))
                    .align_x(align)
                    .into()
            });
            row(cols).padding(bar.bar_cfg.padding as u16).align_y(iced::Alignment::Center).into()
        }
        Orientation::Vertical => {
            let rows = bar.sections.iter().map(|s| {
                let align = match s.align {
                    SectionAlign::Start => iced::alignment::Vertical::Top,
                    SectionAlign::Middle => iced::alignment::Vertical::Center,
                    SectionAlign::End => iced::alignment::Vertical::Bottom,
                };
                container(section(&s.modules, bar, size, orientation, thickness, spacing))
                    .height(Length::FillPortion(s.percent.max(1) as u16))
                    .align_y(align)
                    .into()
            });
            column(rows).padding(bar.bar_cfg.padding as u16).align_x(iced::Alignment::Center).into()
        }
    };

    let bar_surface: Element<'_, Message> = container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(bar.colors.bg_fill),
            ..Default::default()
        })
        .into();

    // Visual confirmation for the workspace app-icon drag (move a window
    // to another workspace group) — a copy of the dragged icon rides the
    // cursor instead of the drag being invisible until release.
    match (&bar.drag_icon, bar.drag_cursor) {
        (Some(icon), Some(cursor)) => {
            let ghost = drag_ghost_icon(bar.colors, icon);
            og_drag::with_drag_ghost(bar_surface, Some(ghost), cursor, iced::Vector::new(14.0, 14.0))
        }
        _ => bar_surface,
    }
}

fn drag_ghost_icon<'a>(colors: AppColors, icon: &str) -> Element<'a, Message> {
    container(
        text(icon.to_string())
            .size(20)
            .font(icon_font::font_for(icon))
            .style(move |_| text::Style { color: Some(colors.text) }),
    )
    .padding(6)
    .style(move |_| container::Style {
        background: Some(Background::Color(Color { a: 0.85, ..colors.accent })),
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    })
    .into()
}

pub fn subscription(bar: &Bar) -> Subscription<Message> {
    let escape_closes_popup = iced::event::listen_with(|event, _status, _id| {
        if let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            ..
        }) = event
        {
            Some(Message::ClosePopup)
        } else {
            None
        }
    });

    // A real "click anywhere on the desktop closes the popup" needs the
    // compositor to grab pointer input for the popup surface (xdg_popup's
    // own `.grab()`) and deliver a `popup_done` event on outside click —
    // iced_layershell 0.13.7 never calls `.grab()` and its xdg_popup
    // dispatch doesn't even handle `PopupDone` if the compositor sent one
    // anyway. That's an upstream gap, not fixable from here. What this
    // *can* do: notice a click landing back on the bar's own surface
    // (a different window from the popup) while a popup is open, and
    // treat that as "dismiss" — covers the common case of clicking the
    // bar again, though not clicking some unrelated app window elsewhere.
    // `listen_with` only takes a non-capturing fn pointer, so this always
    // reports the click; `update()` (which has `bar.popup`) decides
    // whether it actually means "close".
    // Status::Ignored means no widget (button, etc) claimed the click —
    // critically, this excludes the click that just opened a popup via
    // its own on_press, which iced reports as Captured. Without this
    // check, clicking the gear/power icon would open a popup and this
    // same listener would immediately close it again in the same frame.
    let click_reports_window = iced::event::listen_with(|event, status, event_window| {
        if status != iced::event::Status::Ignored {
            return None;
        }
        if let iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) = event {
            Some(Message::ClickOnWindow(event_window))
        } else {
            None
        }
    });

    // Neither xdg_popup grab nor layer-shell has a "click anywhere else
    // closes this" primitive layershellev implements (see the long
    // comment above) — so instead of protocol-level dismissal, treat any
    // sway window/workspace-focus change as "the user started doing
    // something else" and close whatever popup is open. This is what
    // actually fires when you click another app, alt-tab, or switch
    // workspace while a popup is open — the two listeners above only
    // ever caught clicks landing back on the bar's own surface.
    // Gated on `bar.popup.is_some()` via `Subscription::none()` so no
    // `swaymsg subscribe` process runs at all while nothing is open.
    let popup_dismiss_on_activity = if bar.popup.is_some() {
        Subscription::run_with_id("popup-dismiss", popup_dismiss_stream())
    } else {
        Subscription::none()
    };

    // Always-on (not gated on drag_origin) — cheap plain event listener,
    // no process spawned, and it needs to see the release regardless of
    // which widget it lands on/is captured by (unlike click_reports_window
    // above, which specifically wants uncaptured clicks only). A no-op
    // when no drag is in progress.
    let window_drag_release = iced::event::listen_with(|event, _status, _id| {
        if let iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) = event {
            Some(Message::WindowDragEnd)
        } else {
            None
        }
    });

    // Gated on drag_origin (unlike window_drag_release above, which needs
    // to fire regardless) — cursor position is only meaningful, and this
    // listener only worth running, while a drag is actually in progress.
    let window_drag_cursor = if bar.drag_origin.is_some() {
        iced::event::listen_with(|event, _status, _id| {
            if let iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }) = event {
                Some(Message::WindowDragCursorMoved(position))
            } else {
                None
            }
        })
    } else {
        Subscription::none()
    };

    // Liveness heartbeat for systemd's WatchdogSec — a subscription so the
    // ping only happens if the update loop is actually turning over.
    let watchdog_ping = iced::time::every(std::time::Duration::from_secs(crate::watchdog::PING_SECS))
        .map(|_| Message::WatchdogPing);

    Subscription::batch(
        bar.all_modules()
            .map(|m| m.subscription())
            .chain([escape_closes_popup, click_reports_window, popup_dismiss_on_activity, window_drag_release, window_drag_cursor, watchdog_ping]),
    )
}

fn popup_dismiss_stream() -> impl iced::futures::Stream<Item = Message> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    iced::stream::channel(4, |mut sender| async move {
        use iced::futures::SinkExt;

        let mut command = Command::new("swaymsg");
        command
            .args(["-t", "subscribe", "-m", r#"["window","workspace"]"#])
            .stdout(Stdio::piped())
            .kill_on_drop(true);
        unsafe {
            command.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let Ok(mut child) = command.spawn() else { return };
        let Some(stdout) = child.stdout.take() else { return };
        let mut lines = BufReader::new(stdout).lines();

        while let Ok(Some(_line)) = lines.next_line().await {
            let _ = sender.send(Message::ClosePopup).await;
        }
    })
}

pub fn style(bar: &Bar, _theme: &iced::Theme) -> iced_layershell::Appearance {
    iced_layershell::Appearance {
        background_color: iced::Color::TRANSPARENT,
        text_color: bar.colors.text,
    }
}
