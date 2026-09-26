use serde::Deserialize;

/// Reads the same shared theme og-settings/og-files/og-search
/// already use (`~/.config/sway-power/config.json`) — this app owns none of
/// its own theme settings, it just picks out colors. `#[serde(default)]` on
/// everything means it tolerates the file being missing fields, or missing
/// entirely, without erroring.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_bar_bg")]
    pub bar_bg: String,
    #[serde(default = "default_bar_text")]
    pub bar_text: String,
    #[serde(default = "default_accent")]
    pub accent: String,
    #[serde(default = "default_sec_bg")]
    pub sec_bg: String,
    #[serde(default = "default_urgent")]
    pub urgent_color: String,
}

fn default_bar_bg() -> String { "#1a1a2e".into() }
fn default_bar_text() -> String { "#e0e0e0".into() }
fn default_accent() -> String { "#ff7800".into() }
fn default_sec_bg() -> String { "#2a2535".into() }
fn default_urgent() -> String { "#ff4444".into() }

impl Default for Config {
    fn default() -> Self {
        Self {
            bar_bg: default_bar_bg(),
            bar_text: default_bar_text(),
            accent: default_accent(),
            sec_bg: default_sec_bg(),
            urgent_color: default_urgent(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{home}/.config/sway-power/config.json");
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}
