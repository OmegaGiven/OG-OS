use std::path::{Path, PathBuf};
use mime_guess::from_path;

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
    pub mime_types: Vec<String>,
    pub comment: String,
    pub desktop_id: String,
}

pub fn load_app_registry() -> Vec<AppEntry> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from(format!("{}/.local/share/applications", home)),
    ];

    let mut entries = Vec::new();
    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let desktop_id = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if let Some(app) = parse_desktop_file(&path, desktop_id) {
                entries.push(app);
            }
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

fn parse_desktop_file(path: &Path, desktop_id: String) -> Option<AppEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name = String::new();
    let mut exec = String::new();
    let mut mime_types: Vec<String> = Vec::new();
    let mut comment = String::new();
    let mut no_display = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            if name.is_empty() {
                name = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = v.to_string();
        } else if let Some(v) = line.strip_prefix("MimeType=") {
            mime_types = v
                .split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
        } else if let Some(v) = line.strip_prefix("Comment=") {
            if comment.is_empty() {
                comment = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("NoDisplay=") {
            no_display = v.eq_ignore_ascii_case("true");
        }
    }

    if no_display || name.is_empty() || exec.is_empty() {
        return None;
    }

    Some(AppEntry { name, exec, mime_types, comment, desktop_id })
}

pub fn apps_for_file<'a>(path: &Path, registry: &'a [AppEntry]) -> Vec<&'a AppEntry> {
    let mime = from_path(path).first_or_octet_stream().to_string();
    let top_level = mime.split('/').next().unwrap_or("");
    registry
        .iter()
        .filter(|app| app.mime_types.iter().any(|m| m == &mime || m == top_level))
        .collect()
}

pub fn mime_for_file(path: &Path) -> String {
    from_path(path).first_or_octet_stream().to_string()
}

pub fn default_app_for_file(path: &Path) -> Option<String> {
    let mime = from_path(path).first_or_octet_stream().to_string();
    let output = std::process::Command::new("xdg-mime")
        .args(["query", "default", &mime])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub fn set_default_app(mime: &str, desktop_id: &str) {
    let _ = std::process::Command::new("xdg-mime")
        .args(["default", desktop_id, mime])
        .status();
}
