//! Reads keybindings straight from the live sway config every time this
//! opens — not a hardcoded list (the old `hotkeys.sh` cheatsheet this
//! replaces was exactly that, and had already drifted out of sync with
//! the actual bindings) — so any custom rebind a user makes (through
//! og-settings' Hotkeys tab, or by hand) shows up automatically, no
//! second place to update.
//!
//! The parser itself (`parse_sway_bindings`) is a deliberate duplicate of
//! sway-control's own `parse_sway_bindings`/`parse_bindsym` in its
//! `app.rs` — same "kept in sync by hand since they're separate
//! binaries" tradeoff already made elsewhere in this codebase (e.g.
//! og-bar/og-settings' `kind_label`), rather than carving out a shared
//! library crate for ~40 lines of parsing that rarely changes.

use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Binding {
    pub keys: String,
    pub command: String,
    pub description: String,
    pub category: &'static str,
}

fn sway_config_path() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/.config/sway/config")
}

fn parse_bindsym(rest: &str) -> Option<(String, String)> {
    let mut rest = rest.trim_start();
    let mut keys_parts: Vec<&str> = Vec::new();
    while rest.starts_with("--") {
        let (flag, r) = rest.split_once(' ')?;
        keys_parts.push(flag);
        rest = r.trim_start();
    }
    let (keys, cmd) = rest.split_once(' ')?;
    keys_parts.push(keys);
    Some((keys_parts.join(" "), cmd.trim().to_string()))
}

/// Only bindings at brace depth 0 are real top-level bindings — bindsym
/// lines inside `mode "..." { }` blocks (e.g. resize mode) are skipped,
/// same reasoning as sway-control's own copy of this function.
fn parse_sway_bindings(content: &str) -> Vec<(String, String)> {
    let mut bindings = Vec::new();
    let mut depth: i32 = 0;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('}') {
            depth -= 1;
        }
        if depth == 0 {
            if let Some(rest) = trimmed.strip_prefix("bindsym ") {
                if let Some(b) = parse_bindsym(rest) {
                    bindings.push(b);
                }
            }
        }
        if trimmed.ends_with('{') {
            depth += 1;
        }
    }
    bindings
}

fn parse_sway_variables(content: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("set ") {
            if let Some((name, value)) = rest.trim_start().split_once(' ') {
                if let Some(name) = name.strip_prefix('$') {
                    vars.insert(name.to_string(), value.trim().to_string());
                }
            }
        }
    }
    vars
}

/// `$mod+d` -> `Super+D`: substitutes sway variables (`$mod`, `$left`,
/// ...) with their resolved value, then re-cases known modifier names
/// into the form a non-power-user actually recognizes. Values are
/// substituted once (sway's own variables here don't nest), then each
/// `+`-separated token is title-cased unless it's a bare letter key.
fn humanize_keys(raw: &str, vars: &HashMap<String, String>) -> String {
    let substituted = raw
        .split('+')
        .map(|token| {
            if let Some(name) = token.strip_prefix('$') {
                vars.get(name).cloned().unwrap_or_else(|| token.to_string())
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("+");

    substituted
        .split('+')
        .map(|token| match token.to_lowercase().as_str() {
            "mod4" | "super" => "Super".to_string(),
            "mod1" | "alt" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "control" | "ctrl" => "Ctrl".to_string(),
            "--locked" => "".to_string(),
            single if single.chars().count() == 1 => single.to_uppercase(),
            other => {
                // Leave multi-char special key names (Return, Left,
                // XF86AudioMute, ...) as sway itself spells them — those
                // are already readable, just capitalize the first letter
                // for consistency with the modifier names above.
                let mut c = other.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Best-effort human description from the raw command text — matched by
/// substring against patterns actually seen in this system's sway config
/// (see this module's doc comment), falling back to a generic guess for
/// anything unrecognized (a custom user command) rather than showing
/// nothing. Order matters: more specific patterns are checked first.
fn describe(cmd: &str) -> (&'static str, String) {
    let c = cmd.to_lowercase();

    let table: &[(&str, &str, &str)] = &[
        // (substring to match, category, description)
        ("og-search", "Toolkit", "Open app launcher / search"),
        ("og-files", "Toolkit", "Open file manager"),
        ("og-clip", "Toolkit", "Open clipboard history"),
        ("og-settings", "Toolkit", "Open OG Settings"),
        ("og-notif-center", "Toolkit", "Open notification history"),
        ("swaynag", "Session", "Confirm and exit sway"),
        ("swaymsg exit", "Session", "Exit sway"),
        ("pavucontrol", "Apps", "Open audio mixer"),
        ("claude-float", "Apps", "Open Claude (floating terminal)"),
        ("grimshot copy area", "Screenshot", "Screenshot a selected area (copies to clipboard)"),
        ("grimshot save active", "Screenshot", "Screenshot the active window"),
        ("grimshot save screen", "Screenshot", "Screenshot the whole screen"),
        ("grimshot save output", "Screenshot", "Screenshot the current monitor"),
        ("grim", "Screenshot", "Take a screenshot"),
        ("set-sink-mute", "Media", "Mute/unmute audio"),
        ("set-sink-volume", "Media", if c.contains("-5%") { "Volume down" } else { "Volume up" }),
        ("set-source-mute", "Media", "Mute/unmute microphone"),
        ("brightnessctl set 5%-", "Media", "Decrease screen brightness"),
        ("brightnessctl set 5%+", "Media", "Increase screen brightness"),
        ("kill", "Windows", "Close the focused window"),
        ("reload", "System", "Reload the sway config"),
        ("floating toggle", "Windows", "Toggle floating for the focused window"),
        ("fullscreen", "Windows", "Toggle fullscreen for the focused window"),
        ("focus left", "Focus", "Focus the window to the left"),
        ("focus right", "Focus", "Focus the window to the right"),
        ("focus up", "Focus", "Focus the window above"),
        ("focus down", "Focus", "Focus the window below"),
        ("move left", "Windows", "Move the focused window left"),
        ("move right", "Windows", "Move the focused window right"),
        ("move up", "Windows", "Move the focused window up"),
        ("move down", "Windows", "Move the focused window down"),
        ("move container to workspace", "Workspaces", "Move the focused window to a workspace"),
        ("workspace number", "Workspaces", "Switch to a workspace"),
        ("splith", "Layout", "Split the container horizontally"),
        ("splitv", "Layout", "Split the container vertically"),
        ("layout stacking", "Layout", "Switch to stacking layout"),
        ("layout tabbed", "Layout", "Switch to tabbed layout"),
        ("layout toggle split", "Layout", "Toggle split layout orientation"),
        ("move scratchpad", "Windows", "Send the focused window to the scratchpad"),
        ("scratchpad show", "Windows", "Show the next scratchpad window"),
    ];

    for (needle, category, description) in table {
        if c.contains(needle) {
            return (category, description.to_string());
        }
    }

    if let Some(rest) = cmd.strip_prefix("exec ") {
        let program = rest.split_whitespace().next().unwrap_or(rest);
        let name = program.rsplit('/').next().unwrap_or(program);
        return ("Apps", format!("Launch {name}"));
    }

    ("Other", cmd.to_string())
}

/// Reads and parses the live sway config into displayable bindings —
/// call fresh each time the window opens rather than caching, since the
/// whole point is reflecting whatever's actually bound *right now*.
/// Substitutes every `$name` token anywhere in `text` (not just at `+`
/// boundaries like `humanize_keys` does for key combos) — `exec $menu`
/// needs this to resolve to the real command before `describe()` has any
/// chance of recognizing it.
fn resolve_vars(text: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch != '$' {
            out.push(ch);
            continue;
        }
        let start = i + 1;
        let end = text[start..]
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|off| start + off)
            .unwrap_or(text.len());
        let name = &text[start..end];
        match vars.get(name) {
            Some(value) => out.push_str(value),
            None => {
                out.push('$');
                out.push_str(name);
            }
        }
        for _ in 0..name.len() {
            chars.next();
        }
    }
    out
}

pub fn load() -> Vec<Binding> {
    let content = std::fs::read_to_string(sway_config_path()).unwrap_or_default();
    let vars = parse_sway_variables(&content);
    let raw_bindings = parse_sway_bindings(&content);

    raw_bindings
        .into_iter()
        .map(|(keys, command)| {
            let command = resolve_vars(&command, &vars);
            let (category, description) = describe(&command);
            Binding {
                keys: humanize_keys(&keys, &vars),
                command,
                description,
                category,
            }
        })
        .collect()
}
