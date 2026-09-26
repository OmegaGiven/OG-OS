//! Two configs, both load-once at startup like every other app in the
//! suite (see the toolkit's `architecture.md` — no file-watchers, no live
//! reload; restarting the app is how config changes apply):
//!
//! - The shared `og_config::Config` (theme colors, `default_ai_cli`).
//! - og-voice's own small `VoiceConfig` (`voice-config.json`), kept
//!   separate rather than bolted onto the shared schema since og-settings
//!   doesn't have a tab for this app yet (out of scope for this
//!   prototype — see README) and the shared schema is owned/written by
//!   og-settings alone.

pub use og_config::Config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const APP_TINT_SEED: &str = "og-voice";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    /// Only "claude-cli" is implemented right now — the field exists so a
    /// future local-LLM backend can be selected here without another
    /// schema change.
    #[serde(default = "default_ai_backend")]
    pub ai_backend: String,
    #[serde(default = "default_stt_url")]
    pub stt_server_url: String,
    /// Purely informational — shown in the UI/README so the bound key
    /// combo is documented somewhere other than the user's own memory.
    /// Not read by sway; the user (or a future og-settings tab) owns the
    /// actual bindsym lines.
    #[serde(default = "default_hotkey_hint")]
    pub hotkey_hint: String,
    /// Gates `--dangerously-skip-permissions` in `claude.rs`'s `ask()`.
    /// Headless `-p` mode has no interactive prompt to approve/deny tool
    /// use, so this is the only thing standing between a voice/typed
    /// command and Claude actually running Bash/SSH/file-edit tools with
    /// zero confirmation. `#[serde(default)]` (i.e. `false`) is
    /// deliberate: an existing `voice-config.json` from before this field
    /// existed must NOT silently inherit the old hardcoded-always-on
    /// behavior — it should load as execution-disabled until the user
    /// explicitly flips it on (og-settings' AI Context tab, or by hand).
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
    let mut p = og_config::dirs_home();
    p.push(".config/sway-power/voice-config.json");
    p
}

impl VoiceConfig {
    /// Reads `~/.config/sway-power/voice-config.json`. If it doesn't
    /// exist yet, writes the default out (best-effort) so there's a
    /// concrete file for the user to hand-edit — unlike the shared
    /// `config.json`, nothing else in the suite owns writing this one.
    pub fn load() -> Self {
        let path = voice_config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => {
                let cfg = Self::default();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Ok(json) = serde_json::to_string_pretty(&cfg) {
                    let _ = std::fs::write(&path, json);
                }
                cfg
            }
        }
    }
}
