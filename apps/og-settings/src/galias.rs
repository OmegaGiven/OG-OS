//! Backend for the Network tab's web-shortcut section — talks to the
//! og-links service (built from the separate `galias` repo), which is
//! the real go/<alias> redirect server running on localhost. Listing
//! reads its JSON store directly (same file the server itself persists
//! to, so it's always accurate); add/remove go through the server's own
//! HTTP endpoints instead of writing that file ourselves, so the
//! server's in-memory alias table — not just the file — stays in sync
//! without needing a restart.

use serde::Deserialize;
use std::process::Command;

const BASE_URL: &str = "http://127.0.0.1:8096";

#[derive(Debug, Clone, Deserialize)]
pub struct Alias {
    pub key: String,
    pub url: String,
}

fn store_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(format!("{home}/.config/og-links/aliases.json"))
}

pub fn list_aliases() -> Vec<Alias> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn add_alias(key: &str, url: &str) {
    let _ = Command::new("curl")
        .args([
            "-s", "-X", "POST",
            "--data-urlencode", &format!("key={key}"),
            "--data-urlencode", &format!("url={url}"),
            &format!("{BASE_URL}/add"),
        ])
        .output();
}

pub fn remove_alias(key: &str) {
    let _ = Command::new("curl")
        .args([
            "-s", "-X", "POST",
            "--data-urlencode", &format!("key={key}"),
            &format!("{BASE_URL}/delete"),
        ])
        .output();
}
