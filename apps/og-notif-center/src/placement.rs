//! Positions the window just off og-bar, on whichever edge/thickness the
//! user currently has configured (read live from `BarConfig::load()`, the
//! same file og-settings writes) — rather than a hardcoded corner, so
//! moving or resizing the bar in og-settings keeps this in sync instead of
//! going stale.

use og_config::{BarConfig, Edge, ModuleKind, SectionAlign};

const MARGIN: f32 = 12.0;

/// Coarse per-module width guess, in multiples of `item_size` — good
/// enough to estimate *roughly* where the Notifications module sits
/// along the bar so the popup opens next to it rather than at a generic
/// corner, not a promise of the module's exact rendered width (which
/// depends on live content — window count, clock text, tray icon count —
/// that a static config read can't know). Icon-only modules are ~1x;
/// modules that render variable text/icon runs get a wider guess.
fn estimate_module_width(kind: &ModuleKind, item_size: f32) -> f32 {
    match kind {
        ModuleKind::Workspaces => item_size * 4.0,
        ModuleKind::Clock { .. } => item_size * 3.5,
        ModuleKind::Tray => item_size * 3.0,
        ModuleKind::Cpu | ModuleKind::Memory | ModuleKind::Gpu => item_size * 2.0,
        ModuleKind::Network | ModuleKind::Bluetooth | ModuleKind::Pulseaudio => item_size * 1.5,
        ModuleKind::Notifications
        | ModuleKind::Clipboard
        | ModuleKind::Settings
        | ModuleKind::Power
        | ModuleKind::Launcher { .. } => item_size * 1.2,
    }
}

/// Estimated center of the Notifications module along the bar's main
/// axis (pixels from the bar's own start, i.e. its top/left edge — not
/// yet offset by the output's own position), replicating og-bar's own
/// section-percent + Start/Middle/End alignment layout closely enough to
/// land "next to the bell", even though it can't know each module's true
/// rendered width (variable content — see `estimate_module_width`).
/// `None` if there's no enabled Notifications module in the bar at all.
fn notifications_offset(bar: &BarConfig, bar_length: f32) -> Option<f32> {
    let item_size = bar.item_size as f32;
    let spacing = bar.spacing as f32;
    let total_percent: f32 = bar.sections.iter().map(|s| s.percent.max(1) as f32).sum();
    if total_percent <= 0.0 {
        return None;
    }

    let mut section_start = 0.0;
    for section in &bar.sections {
        let section_width = (section.percent.max(1) as f32 / total_percent) * bar_length;
        let enabled: Vec<&og_config::ModuleConfig> = section.modules.iter().filter(|m| m.enabled).collect();
        let widths: Vec<f32> = enabled.iter().map(|m| estimate_module_width(&m.kind, item_size)).collect();
        let content_width = widths.iter().sum::<f32>() + spacing * widths.len().saturating_sub(1) as f32;

        let content_start = section_start
            + match section.align {
                SectionAlign::Start => 0.0,
                SectionAlign::Middle => (section_width - content_width) * 0.5,
                SectionAlign::End => section_width - content_width,
            };

        let mut offset = content_start;
        for (module, width) in enabled.iter().zip(&widths) {
            if matches!(module.kind, ModuleKind::Notifications) {
                return Some(offset + width * 0.5);
            }
            offset += width + spacing;
        }

        section_start += section_width;
    }
    None
}

struct OutputRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// `swaymsg ... move position <x> <y>` is documented as relative to the
/// container's *current workspace* origin, not the global/output
/// coordinate space `get_outputs`/`get_tree` otherwise use everywhere
/// else — confirmed empirically too (asking for (100,100) landed at
/// (1742,100) on this system, exactly workspace-origin (1642,0) + the
/// literal request). Without subtracting this before issuing the move,
/// every target lands offset by however far that workspace's origin is
/// from (0,0) globally.
fn focused_workspace_origin() -> Option<(f32, f32)> {
    let out = std::process::Command::new("swaymsg").args(["-t", "get_workspaces"]).output().ok()?;
    let workspaces: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = workspaces.as_array()?;
    let ws = arr.iter().find(|w| w["focused"] == true)?;
    let rect = &ws["rect"];
    Some((rect["x"].as_f64()? as f32, rect["y"].as_f64()? as f32))
}

fn focused_output() -> Option<OutputRect> {
    let out = std::process::Command::new("swaymsg")
        .args(["-t", "get_outputs"])
        .output()
        .ok()?;
    let outputs: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = outputs.as_array()?;
    let output = arr.iter().find(|o| o["focused"] == true).or_else(|| arr.first())?;
    let rect = &output["rect"];
    Some(OutputRect {
        x: rect["x"].as_f64()? as f32,
        y: rect["y"].as_f64()? as f32,
        width: rect["width"].as_f64()? as f32,
        height: rect["height"].as_f64()? as f32,
    })
}

/// Raw pixel coordinates for "just off the bar" — shared by `near_bar`
/// (an iced `Position` hint that Wayland compositors ignore for
/// client-requested toplevel placement, kept as a harmless best-effort)
/// and `move_near_bar` (an actual `swaymsg move position`, which *does*
/// work since IPC-driven moves aren't subject to that restriction).
fn near_bar_xy(size: (f32, f32)) -> Option<(f32, f32)> {
    let bar = BarConfig::load();
    let thickness = bar.thickness as f32;
    let (w, h) = size;

    let output = focused_output()?;
    let vertical_bar = matches!(bar.position, Edge::Left | Edge::Right);
    let bar_length = if vertical_bar { output.height } else { output.width };

    // Falls back to the old flat MARGIN-from-corner placement if there's
    // no enabled Notifications module to locate (shouldn't normally
    // happen — this is the module that launches this popup — but the
    // config could've changed since).
    let module_offset = notifications_offset(&bar, bar_length);
    let popup_size_along_axis = if vertical_bar { h } else { w };
    let along_axis = module_offset
        .map(|center| center - popup_size_along_axis * 0.5)
        .unwrap_or(MARGIN)
        .clamp(MARGIN, (bar_length - popup_size_along_axis - MARGIN).max(MARGIN));

    Some(match bar.position {
        Edge::Top => (output.x + along_axis, output.y + thickness + MARGIN),
        Edge::Bottom => (output.x + along_axis, output.y + output.height - thickness - h - MARGIN),
        Edge::Left => (output.x + thickness + MARGIN, output.y + along_axis),
        Edge::Right => (output.x + output.width - thickness - w - MARGIN, output.y + along_axis),
    })
}

pub fn near_bar(size: (f32, f32)) -> iced::window::Position {
    match near_bar_xy(size) {
        Some((x, y)) => iced::window::Position::Specific(iced::Point::new(x, y)),
        None => iced::window::Position::Centered,
    }
}

/// Does sway's tree currently contain a mapped window with this app_id?
/// `swaymsg [criteria] move ...` silently no-ops (no error, just matches
/// nothing) if the window isn't mapped yet — which it often isn't the
/// instant `App::new`'s init `Task` starts running, since that races the
/// compositor actually mapping/floating the surface.
fn window_exists(app_id: &str) -> bool {
    let Ok(out) = std::process::Command::new("swaymsg").args(["-t", "get_tree"]).output() else {
        return false;
    };
    let Ok(tree) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return false;
    };
    fn walk(node: &serde_json::Value, app_id: &str) -> bool {
        if node.get("app_id").and_then(|v| v.as_str()) == Some(app_id) {
            return true;
        }
        for key in ["nodes", "floating_nodes"] {
            if let Some(children) = node.get(key).and_then(|v| v.as_array()) {
                if children.iter().any(|c| walk(c, app_id)) {
                    return true;
                }
            }
        }
        false
    }
    walk(&tree, app_id)
}

/// Wayland compositors don't let a client position its own xdg_toplevel
/// (unlike X11) — `near_bar`'s `Position::Specific` above is silently
/// ignored, and sway falls back to its own floating-window default
/// (centered), which is why the window used to open away from the bar no
/// matter what that was set to. `swaymsg ... move position` is
/// compositor-side IPC, not a client request, so it isn't subject to that
/// restriction — this is the actual fix, called once from `App::new`'s
/// init `Task`.
///
/// Polls for the window actually existing in sway's tree first — this
/// runs from `App::new`'s init `Task`, which starts essentially
/// concurrently with the compositor mapping/floating the surface, not
/// reliably after it. Without this, `move position` would frequently
/// silently no-op (criteria matches nothing yet) and the window would
/// stay wherever sway's own floating-window default put it (centered).
pub fn move_near_bar(size: (f32, f32)) {
    let Some((x, y)) = near_bar_xy(size) else { return };
    for _ in 0..20 {
        if window_exists("og-notif-center") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    // `move position` wants coordinates relative to the workspace this
    // window landed on (see focused_workspace_origin's doc comment) —
    // `near_bar_xy` computed (x, y) in the same global space as
    // get_outputs/get_tree, so that origin has to be subtracted back out
    // here before handing the numbers to swaymsg.
    let (ws_x, ws_y) = focused_workspace_origin().unwrap_or((0.0, 0.0));
    let _ = std::process::Command::new("swaymsg")
        .arg(format!("[app_id=\"og-notif-center\"] move position {} {}", (x - ws_x) as i32, (y - ws_y) as i32))
        .output();
}
