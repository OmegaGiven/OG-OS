//! Positions the chat bubble in a screen corner — top-right by default.
//! Adapted from og-notif-center's `placement.rs`: Wayland doesn't let a
//! client position its own xdg_toplevel, so the `Position::Specific` hint
//! passed to iced's window settings is a harmless no-op under sway, and
//! the actual fix is an IPC-driven `swaymsg ... move position` issued
//! once the window is mapped (`move_to_corner`, called from `App::new`'s
//! init `Task`). Simplified relative to og-notif-center's version: no
//! bar-module lookup, since this isn't tied to a specific og-bar module —
//! just a fixed margin off a chosen corner, kept easy to change (see
//! `CORNER` below).

use og_config::{BarConfig, Edge};
use std::time::Duration;

const MARGIN: f32 = 16.0;

#[derive(Clone, Copy)]
pub enum Corner {
    TopRight,
    #[allow(dead_code)]
    TopLeft,
    #[allow(dead_code)]
    BottomRight,
    #[allow(dead_code)]
    BottomLeft,
}

/// Change this to relocate the bubble — the rest of the app doesn't care.
pub const CORNER: Corner = Corner::TopRight;

struct OutputRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn focused_output() -> Option<OutputRect> {
    let out = std::process::Command::new("swaymsg").args(["-t", "get_outputs"]).output().ok()?;
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

/// `swaymsg move position` is relative to the focused workspace's own
/// origin, not the global coordinate space `get_outputs` uses — see
/// og-notif-center's `placement.rs` for the empirical confirmation this
/// was copied from.
fn focused_workspace_origin() -> Option<(f32, f32)> {
    let out = std::process::Command::new("swaymsg").args(["-t", "get_workspaces"]).output().ok()?;
    let workspaces: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let arr = workspaces.as_array()?;
    let ws = arr.iter().find(|w| w["focused"] == true)?;
    let rect = &ws["rect"];
    Some((rect["x"].as_f64()? as f32, rect["y"].as_f64()? as f32))
}

fn bar_clearance(edge_needed: Edge) -> f32 {
    let bar = BarConfig::load();
    if bar.position == edge_needed {
        bar.thickness as f32 + MARGIN
    } else {
        MARGIN
    }
}

fn corner_xy(size: (f32, f32)) -> Option<(f32, f32)> {
    let (w, h) = size;
    let o = focused_output()?;
    Some(match CORNER {
        Corner::TopRight => (o.x + o.width - w - MARGIN, o.y + bar_clearance(Edge::Top)),
        Corner::TopLeft => (o.x + MARGIN, o.y + bar_clearance(Edge::Top)),
        Corner::BottomRight => (o.x + o.width - w - MARGIN, o.y + o.height - h - bar_clearance(Edge::Bottom)),
        Corner::BottomLeft => (o.x + MARGIN, o.y + o.height - h - bar_clearance(Edge::Bottom)),
    })
}

/// Best-effort hint passed into `iced::window::Settings` — ignored by
/// sway in practice, kept only as a harmless fallback for other
/// compositors that might honor it.
pub fn hint(size: (f32, f32)) -> iced::window::Position {
    match corner_xy(size) {
        Some((x, y)) => iced::window::Position::Specific(iced::Point::new(x, y)),
        None => iced::window::Position::Specific(iced::Point::new(40.0, 40.0)),
    }
}

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

/// Double-launch handling for `--interactive`: rather than silently
/// doing nothing when an og-voice instance is already running (recording
/// from the hotkey flow, or an already-open idle interactive window),
/// best-effort bring it to focus so the user gets *some* feedback their
/// keypress/click did something. Best-effort/fire-and-forget, same as
/// every other `swaymsg` call in this file — a failure here (window
/// already gone, sway not reachable) just means the user sees nothing
/// happen, no worse than before this existed.
pub fn focus_existing() {
    let _ = std::process::Command::new("swaymsg")
        .arg("[app_id=\"og-voice\"] focus")
        .output();
}

/// The real fix — issues `swaymsg move position` once the window is
/// actually mapped (polls briefly since this races the compositor
/// mapping/floating the surface, same as og-notif-center's version).
pub fn move_to_corner(size: (f32, f32)) {
    let Some((x, y)) = corner_xy(size) else { return };
    for _ in 0..20 {
        if window_exists("og-voice") {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let (ws_x, ws_y) = focused_workspace_origin().unwrap_or((0.0, 0.0));
    let _ = std::process::Command::new("swaymsg")
        .arg(format!("[app_id=\"og-voice\"] move position {} {}", (x - ws_x) as i32, (y - ws_y) as i32))
        .output();
}
