//! App-owned config under XDG dirs. No dependency on the OG-OS
//! `~/.config/sway-power` layout so the app works on any desktop.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("og-explorer")
}

#[allow(dead_code)] // thumbnail cache lands in phase 1 (SPEC.md §9)
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("og-explorer")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Bookmarks {
    #[serde(default)]
    pub folders: Vec<PathBuf>,
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

impl Bookmarks {
    fn path() -> PathBuf {
        config_dir().join("bookmarks.json")
    }

    /// First run seeds the usual home folders so the sidebar isn't empty.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => {
                let h = home();
                let folders = [
                    "Desktop",
                    "Documents",
                    "Downloads",
                    "Pictures",
                    "Music",
                    "Videos",
                ]
                .iter()
                .map(|d| h.join(d))
                .filter(|p| p.is_dir())
                .collect();
                Self {
                    folders,
                    files: Vec::new(),
                }
            }
        }
    }

    pub async fn save(self) -> Result<(), String> {
        let path = Self::path();
        let json = serde_json::to_string_pretty(&self).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| e.to_string())?;
        }
        tokio::fs::write(path, json)
            .await
            .map_err(|e| e.to_string())
    }
}
