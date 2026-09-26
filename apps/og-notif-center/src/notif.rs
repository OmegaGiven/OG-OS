use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// mako (checked via `makoctl help`) has no per-entry delete and no
/// clear-history command at all — only dismiss/restore/list/history/
/// reload/mode. So "delete" here means: keep our own hide-list of raw
/// history entries to filter out of view (mako still holds the real
/// entry until its own `max-history` ages it out); "clear all" means
/// dismissing everything currently visible and restarting the mako
/// daemon itself, which wipes its in-memory history buffer.
fn hidden_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(format!("{home}/.local/share/notification-hidden.json"))
}

pub struct NotifItem {
    /// Canonical JSON text of the raw mako history entry — doubles as
    /// the hide-list key, since mako gives no stable per-entry id.
    pub raw: String,
    pub label: String,
}

fn load_hidden() -> Vec<String> {
    let raw = std::fs::read_to_string(hidden_path()).unwrap_or_else(|_| "[]".into());
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn load_visible() -> Vec<NotifItem> {
    let hidden = load_hidden();
    let Ok(out) = Command::new("makoctl").args(["history", "-j"]).output() else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    let Some(arr) = json.as_array() else { return Vec::new() };

    arr.iter()
        .rev()
        .filter_map(|item| {
            let raw = item.to_string();
            if hidden.contains(&raw) {
                return None;
            }
            let app_name = item.get("app_name").and_then(|v| v.as_str()).unwrap_or("?");
            let summary = item.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            let body = item.get("body").and_then(|v| v.as_str()).unwrap_or("");
            let label = if body.is_empty() {
                format!("{app_name}: {summary}")
            } else {
                format!("{app_name}: {summary} — {body}")
            };
            Some(NotifItem { raw, label })
        })
        .collect()
}

pub fn hide(raw: &str) {
    let mut hidden = load_hidden();
    hidden.push(raw.to_string());
    if let Some(parent) = hidden_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&hidden) {
        let _ = std::fs::write(hidden_path(), json);
    }
}

pub fn clear_all() {
    let _ = Command::new("makoctl").args(["dismiss", "-a"]).status();
    let _ = Command::new("pkill").args(["-x", "mako"]).status();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let _ = Command::new("setsid").arg("mako").stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    let _ = std::fs::write(hidden_path(), "[]");
}

/// true = notifications currently shown as they arrive (not in
/// do-not-disturb).
pub fn preview_on() -> bool {
    let Ok(out) = Command::new("makoctl").arg("mode").output() else { return true };
    let s = String::from_utf8_lossy(&out.stdout);
    !s.lines().any(|l| l.trim() == "do-not-disturb")
}

/// Same mode og-settings' "Enable notifications" master toggle uses —
/// `invisible=1` under this section is what actually suppresses the popup
/// while do-not-disturb is active. Without the section, `makoctl mode -a`
/// would just track the mode name with no effect.
pub fn ensure_dnd_mode_configured() {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = format!("{home}/.config/mako/config");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if !content.contains("[mode=do-not-disturb]") {
        let appended = format!("{content}\n[mode=do-not-disturb]\ninvisible=1\n");
        let _ = std::fs::write(&path, appended);
        let _ = Command::new("makoctl").arg("reload").status();
    }
}

pub fn set_preview(enable: bool) {
    if enable {
        let _ = Command::new("makoctl").args(["mode", "-r", "do-not-disturb"]).status();
    } else {
        let _ = Command::new("makoctl").args(["mode", "-a", "do-not-disturb"]).status();
    }
}

pub fn copy_to_clipboard(text: &str) {
    if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
    }
}
