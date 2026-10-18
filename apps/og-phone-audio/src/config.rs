//! `~/.config/sway-power/phone-audio.json` — written by og-settings'
//! Audio → Phone subtab (mirrored field-for-field in
//! `og-settings/src/phone_audio.rs`), read once at daemon startup.
//! og-settings restarts the service after every change, same
//! load-once convention as the rest of the suite.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 47800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhoneAudioConfig {
    /// Shared secret the phone app must send in its hello. Anyone on the
    /// LAN can beacon; only a phone that knows this code gets audio.
    #[serde(default)]
    pub pair_code: String,
    /// Packet size in milliseconds (2.5 / 5 / 10). Smaller = lower latency,
    /// more packets per second and less tolerance for Wi-Fi hiccups.
    #[serde(default = "default_frame_ms")]
    pub frame_ms: f32,
    /// Make the phone sink the default output while a phone is connected
    /// (and move what's already playing onto it); restore on disconnect.
    #[serde(default = "default_true")]
    pub auto_default: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Which way audio goes:
    ///   "auto"     — follow the headphones: phone has its own headphones →
    ///                PC audio relayed to the phone over Wi-Fi; otherwise →
    ///                phone audio (calls, notifications, media) to the PC
    ///                over Bluetooth
    ///   "to_phone" — always PC → phone relay
    ///   "to_pc"    — always phone → PC over Bluetooth
    #[serde(default = "default_direction")]
    pub direction: String,
}

fn default_direction() -> String {
    "auto".into()
}

fn default_frame_ms() -> f32 {
    5.0
}
fn default_true() -> bool {
    true
}
fn default_port() -> u16 {
    DEFAULT_PORT
}

impl Default for PhoneAudioConfig {
    fn default() -> Self {
        Self { pair_code: String::new(), frame_ms: default_frame_ms(), auto_default: true, port: DEFAULT_PORT, direction: default_direction() }
    }
}

pub fn path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config/sway-power/phone-audio.json")
}

/// 6 random digits from /dev/urandom.
pub fn new_pair_code() -> String {
    use std::io::Read;
    let mut b = [0u8; 4];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

impl PhoneAudioConfig {
    /// Missing/corrupt file → defaults. A missing pair code is generated
    /// and saved so the phone and og-settings always have one to show.
    pub fn load() -> Self {
        let mut c: Self = std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if c.pair_code.is_empty() {
            c.pair_code = new_pair_code();
            let _ = c.save();
        }
        if ![2.5, 5.0, 10.0].contains(&c.frame_ms) {
            c.frame_ms = default_frame_ms();
        }
        c
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default())
    }
}
