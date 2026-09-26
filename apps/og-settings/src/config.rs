//! Thin wrapper around the shared `og-config` crate: re-exports its
//! `Config` type (and friends) so the rest of this app's existing
//! `crate::config::Config` references keep working unchanged, and adds
//! the one thing that's genuinely specific to this app — seeding
//! still-empty fields from the *live* sway/waybar config files on first
//! run. No other OG-toolkit app needs this (they only ever read what this
//! one already saved).

#[allow(unused_imports)] // re-exported for API completeness, not all used internally
pub use og_config::{dirs_home, ClockConfig, Config, MonitorConfig, SleepSetting};

/// Loads the shared config, then seeds any still-empty fields (module
/// order, cursor theme, wallpaper) from whatever's live in sway/waybar's
/// own config files — but only until the user's own save fills them in;
/// after that this is a no-op passthrough to `og_config::Config::load()`.
pub fn load_and_seed() -> Config {
    let mut cfg = Config::load();

    if cfg.modules_left.is_empty() && cfg.modules_center.is_empty() && cfg.modules_right.is_empty() {
        if let Some((l, c, r)) = read_waybar_module_lists() {
            cfg.modules_left = l;
            cfg.modules_center = c;
            cfg.modules_right = r;
        }
    } else {
        // The waybar config file is hand-edited too (bespoke `custom/*`
        // modules like the terminal/power/clipboard buttons), so on
        // every load absorb anything present live but missing here —
        // otherwise the next module-order write (drag-arrange, a
        // taskbar toggle) would silently delete it by overwriting the
        // bar's module arrays with our stale list. We only ever add,
        // never remove, so this can't fight the drag-arrange screen.
        if let Some((live_l, live_c, live_r)) = read_waybar_module_lists() {
            for (cfg_list, live_list) in [
                (&mut cfg.modules_left, live_l),
                (&mut cfg.modules_center, live_c),
                (&mut cfg.modules_right, live_r),
            ] {
                for module in live_list {
                    if !cfg_list.contains(&module) {
                        cfg_list.push(module);
                    }
                }
            }
        }
    }
    if cfg.cursor_theme.is_empty() {
        if let Some((theme, size)) = read_sway_cursor_settings() {
            cfg.cursor_theme = theme;
            cfg.cursor_size = size;
        }
        if let Some(accel) = read_sway_pointer_accel() {
            cfg.mouse_sensitivity = accel;
        }
        if let Some(mode) = read_sway_focus_follows_mouse() {
            cfg.focus_follows_mouse = mode;
        }
        if let Some(mode) = read_sway_mouse_warping() {
            cfg.mouse_warping = mode;
        }
        if let Some((rate, delay)) = read_sway_keyboard_repeat() {
            cfg.keyboard_repeat_rate = rate;
            cfg.keyboard_repeat_delay = delay;
        }
    }
    if cfg.wallpaper_path.is_empty() {
        if let Some((value, is_solid_color, fit)) = read_sway_wallpaper() {
            if is_solid_color {
                cfg.wallpaper_mode = "color".into();
                cfg.wallpaper_color = value;
            } else {
                cfg.wallpaper_mode = "image".into();
                cfg.wallpaper_path = value;
                if let Some(fit) = fit {
                    cfg.wallpaper_fit = fit;
                }
            }
        }
    }
    cfg
}

fn default_cursor_size() -> i32 {
    24
}

fn read_sway_cursor_settings() -> Option<(String, i32)> {
    let content = crate::sway::sway_config_text();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("seat seat0 xcursor_theme ") {
            let mut parts = rest.split_whitespace();
            let theme = parts.next()?.to_string();
            let size = parts.next().and_then(|s| s.parse().ok()).unwrap_or(default_cursor_size());
            return Some((theme, size));
        }
    }
    None
}

/// Returns `(value, is_solid_color, fit_mode)` — value is either the quoted
/// image path or the bare color token, read from the sway config's
/// `output * bg` line. `fit_mode` is the trailing word for image mode
/// (fill/stretch/center/...), None for solid color lines.
fn read_sway_wallpaper() -> Option<(String, bool, Option<String>)> {
    let content = crate::sway::sway_config_text();
    for line in content.lines() {
        let t = line.trim();
        if !t.starts_with("output") || !t.contains(" bg ") {
            continue;
        }
        if let Some(start) = t.find('"') {
            let rest = &t[start + 1..];
            let end = rest.find('"')?;
            let image_path = rest[..end].to_string();
            let fit = rest[end + 1..].split_whitespace().next().map(|s| s.to_string());
            return Some((image_path, false, fit));
        }
        // No quotes: `output * bg <color> solid_color`.
        let rest = t.split_once(" bg ")?.1;
        let color = rest.split_whitespace().next()?;
        return Some((color.to_string(), true, None));
    }
    None
}

fn read_sway_pointer_accel() -> Option<f32> {
    let content = crate::sway::sway_config_text();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("input type:pointer pointer_accel ") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn read_sway_focus_follows_mouse() -> Option<og_config::FocusFollowsMouse> {
    let content = crate::sway::sway_config_text();
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("focus_follows_mouse ") {
            return og_config::FocusFollowsMouse::from_sway_value(rest.trim());
        }
    }
    None
}

fn read_sway_mouse_warping() -> Option<og_config::MouseWarping> {
    let content = crate::sway::sway_config_text();
    for line in content.lines() {
        if let Some(rest) = line.trim().strip_prefix("mouse_warping ") {
            return og_config::MouseWarping::from_sway_value(rest.trim());
        }
    }
    None
}

fn read_sway_keyboard_repeat() -> Option<(i32, i32)> {
    let content = crate::sway::sway_config_text();
    let mut rate = None;
    let mut delay = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("input type:keyboard repeat_rate ") {
            rate = rest.trim().parse().ok();
        } else if let Some(rest) = trimmed.strip_prefix("input type:keyboard repeat_delay ") {
            delay = rest.trim().parse().ok();
        }
    }
    Some((rate?, delay?))
}

fn read_waybar_module_lists() -> Option<(Vec<String>, Vec<String>, Vec<String>)> {
    let path = dirs_home().join(".config/waybar/config");
    let content = std::fs::read_to_string(path).ok()?;
    let root: serde_json::Value = serde_json::from_str(&content).ok()?;
    let list = |key: &str| -> Vec<String> {
        root.get(key)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    Some((list("modules-left"), list("modules-center"), list("modules-right")))
}
