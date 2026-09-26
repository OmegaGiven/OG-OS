use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    pub key: String,
    pub url: String,
}

fn store_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(format!("{home}/.config/og-links/aliases.json"))
}

pub fn load() -> Vec<Alias> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(aliases: &[Alias]) {
    let path = store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(aliases) {
        let _ = std::fs::write(path, json);
    }
}

/// Normalizes a raw key the same way both add and lookup do, so `G/Radarr`
/// and `g/radarr` are the same shortcut.
pub fn normalize_key(key: &str) -> String {
    key.trim().trim_matches('/').to_lowercase()
}

/// Adds a scheme if the destination doesn't already look like a full URL —
/// lets people type `radarr.local:7878` instead of always needing `http://`.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim();
    if url.contains("://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}
