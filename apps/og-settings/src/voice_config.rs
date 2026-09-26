//! Read+write mirror of og-voice's own `~/.config/sway-power/voice-config.json`
//! (schema owned by `og-voice/src/config.rs`'s `VoiceConfig`) — unlike
//! `voice_history.rs` (read-only log viewer), this tab has one genuinely
//! editable setting backed by that file: whether og-voice is allowed to
//! pass `--dangerously-skip-permissions` to the `claude` CLI.
//!
//! Deliberately NOT folded into `ai_context.rs`'s `AiContext` /
//! `ai-context.json` — `allow_execution` isn't descriptive context fed to
//! an agent, it's a setting og-voice's own `claude.rs` reads directly out
//! of `voice-config.json`. Keeping one file per boolean's actual owner
//! avoids two sources of truth for the same flag; sway-control has no
//! existing precedent for writing a sibling app's config file wholesale,
//! so this struct mirrors og-voice's schema field-for-field (same
//! defaults, same `#[serde(default)]` behavior) so a round-trip load/save
//! from this tab never drops or resets fields og-voice's own process
//! wrote (`ai_backend`, `stt_server_url`, `hotkey_hint`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceConfig {
    #[serde(default = "default_ai_backend")]
    pub ai_backend: String,
    #[serde(default = "default_stt_url")]
    pub stt_server_url: String,
    #[serde(default = "default_hotkey_hint")]
    pub hotkey_hint: String,
    /// Gates `--dangerously-skip-permissions` in og-voice's `claude.rs`.
    /// Defaults to `false` — matches og-voice's own safe-by-default
    /// fallback for a missing/pre-existing file.
    #[serde(default)]
    pub allow_execution: bool,
}

fn default_ai_backend() -> String {
    "claude-cli".to_string()
}
fn default_stt_url() -> String {
    "http://127.0.0.1:8765".to_string()
}
fn default_hotkey_hint() -> String {
    "$mod+v".to_string()
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            ai_backend: default_ai_backend(),
            stt_server_url: default_stt_url(),
            hotkey_hint: default_hotkey_hint(),
            allow_execution: false,
        }
    }
}

fn voice_config_path() -> PathBuf {
    let mut p = crate::config::dirs_home();
    p.push(".config/sway-power/voice-config.json");
    p
}

impl VoiceConfig {
    /// Missing or corrupt file loads as defaults (execution off), same
    /// fallback convention as `ai_context.rs`'s `load()` — never errors
    /// out the tab over a file og-voice itself will happily recreate.
    pub fn load() -> Self {
        let path = voice_config_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = voice_config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}
