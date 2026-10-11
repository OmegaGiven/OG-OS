//! App-owned config under XDG dirs. No dependency on the OG-OS
//! `~/.config/sway-power` layout so the app works on any desktop.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

/// `~/x` → `$HOME/x`; anything else unchanged.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(fallback))
}

/// One-time move of data written while the app was called og-explorer
/// (bookmarks, Stirling settings, activity log) to the og-files folders.
/// Never overwrites anything that already exists.
pub fn migrate_old_dirs() {
    let pairs = [
        (
            xdg("XDG_CONFIG_HOME", ".config").join("og-explorer"),
            config_dir(),
        ),
        (
            xdg("XDG_STATE_HOME", ".local/state").join("og-explorer"),
            xdg("XDG_STATE_HOME", ".local/state").join("og-files"),
        ),
    ];
    for (old, new) in pairs {
        let Ok(rd) = std::fs::read_dir(&old) else {
            continue;
        };
        let _ = std::fs::create_dir_all(&new);
        for e in rd.flatten() {
            let to = new.join(e.file_name());
            if !to.exists() {
                let _ = std::fs::rename(e.path(), to);
            }
        }
        let _ = std::fs::remove_dir(&old); // only if now empty
    }
    // the old thumbnail cache is just regenerated
    let _ = std::fs::remove_dir_all(xdg("XDG_CACHE_HOME", ".cache").join("og-explorer"));
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("og-files")
}

#[allow(dead_code)] // thumbnail cache lands in phase 1 (SPEC.md §9)
pub fn cache_dir() -> PathBuf {
    xdg("XDG_CACHE_HOME", ".cache").join("og-files")
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
