//! Persistent context handed to AI agents/tools in the suite (starting
//! with og-voice) — free-form user notes plus a small roster of known
//! hosts (tailnet or otherwise) so an agent can be told "ssh into
//! `download` and check X" and actually know what `download` is.
//!
//! Deliberately kept OUT of the shared `config.json` (`og_config::Config`)
//! — this is free-form user-authored context, not theme/settings, same
//! reasoning that already split `bar-config.json` out of the main
//! config. Own small file, own load/save, same shape as `Config`'s and
//! `BarConfig`'s in `og-config`: read-with-default on load, write on
//! save, no error surfaced to the user on a missing/corrupt file — just
//! falls back to empty context.
//!
//! Not a credentials manager: no SSH keys, no auto-login, nothing beyond
//! descriptive notes an AI agent reads before acting.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

fn ai_context_path() -> PathBuf {
    let mut p = crate::config::dirs_home();
    p.push(".config/sway-power/ai-context.json");
    p
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AiContext {
    /// Free text: "how I like things done", coding/ops preferences, etc.
    #[serde(default)]
    pub general_notes: String,
    #[serde(default)]
    pub hosts: Vec<AiContextHost>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AiContextHost {
    /// Tailnet hostname (e.g. "download", "station", "go") or, for a
    /// manually-added host, whatever the user calls it (SSH config
    /// alias, etc.).
    pub name: String,
    /// Tailnet IP / MagicDNS name, or a manually-entered address.
    #[serde(default)]
    pub address: String,
    /// User-authored: what it is, what's on it, anything an AI agent
    /// should know before sshing in.
    #[serde(default)]
    pub notes: String,
    /// True if this row was auto-discovered from `tailscale status` (see
    /// `crate::vpn::tailscale_status`), false if the user typed it in by
    /// hand via "+ Add host manually".
    #[serde(default)]
    pub from_tailnet: bool,
}

impl AiContext {
    /// Reads `~/.config/sway-power/ai-context.json`. Missing or corrupt
    /// file is treated as empty context, not an error — matches
    /// `og_config::Config::load`'s own fallback-to-default behavior.
    pub fn load() -> Self {
        let path = ai_context_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = ai_context_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}
