//! Audio → Phone subtab backend: drives the `og-phone-audio` relay daemon
//! (PC audio → phone over Wi-Fi → phone's Bluetooth headphones).
//!
//! `PhoneAudioConfig` mirrors `og-phone-audio/src/config.rs` field-for-field
//! (same defaults, same `#[serde(default)]`) — same "mirror the owner's
//! schema" approach as `voice_config.rs`. Like the Spatial subtab this is
//! live, not staged: every change is saved immediately and the service is
//! restarted so the load-once daemon picks it up.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SERVICE: &str = "og-phone-audio.service";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhoneAudioConfig {
    #[serde(default)]
    pub pair_code: String,
    #[serde(default = "default_frame_ms")]
    pub frame_ms: f32,
    #[serde(default = "default_true")]
    pub auto_default: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    /// "auto" | "to_phone" | "to_pc" — see og-phone-audio's config.rs
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
    47800
}

impl Default for PhoneAudioConfig {
    fn default() -> Self {
        Self {
            pair_code: String::new(),
            frame_ms: default_frame_ms(),
            auto_default: true,
            port: default_port(),
            direction: default_direction(),
        }
    }
}

fn config_path() -> PathBuf {
    let mut p = crate::config::dirs_home();
    p.push(".config/sway-power/phone-audio.json");
    p
}

pub fn new_pair_code() -> String {
    use std::io::Read;
    let mut b = [0u8; 4];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    format!("{:06}", u32::from_le_bytes(b) % 1_000_000)
}

impl PhoneAudioConfig {
    /// Missing/corrupt → defaults; a missing pair code is generated and
    /// saved right away so the phone and the daemon agree on it.
    pub fn load() -> Self {
        let mut c: Self = std::fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if c.pair_code.is_empty() {
            c.pair_code = new_pair_code();
            let _ = c.save();
        }
        c
    }

    pub fn save(&self) -> Result<(), String> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())
    }
}

/// Live state the daemon writes to `$XDG_RUNTIME_DIR/og-phone-audio.json`.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct RelayStatus {
    pub connected: bool,
    pub phone_name: String,
    pub phone_addr: String,
    pub since: u64,
    pub frame_ms: f32,
    pub sent: u64,
    pub phone_buf_ms: f32,
    pub phone_out_ms: f32,
    pub underruns: u64,
    pub lost: u64,
    pub late: u64,
    pub rejected: String,
    /// "relay" (PC → phone) / "bluetooth" (phone → PC) / "off"
    pub route: String,
    pub phone_headset: bool,
    pub phone_outputs: Vec<String>,
    pub bt_paired: bool,
    pub bt_connected: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhoneState {
    pub installed: bool,
    pub running: bool,
    pub enabled: bool,
    pub status: RelayStatus,
    pub apk: Option<PathBuf>,
}

fn status_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(dir).join("og-phone-audio.json")
}

pub fn apk_path() -> Option<PathBuf> {
    let p = crate::config::dirs_home().join(".local/share/og-phone-audio/og-phone-audio.apk");
    p.exists().then_some(p)
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn state() -> PhoneState {
    let running = systemctl(&["is-active", "--quiet", SERVICE]);
    let status = if running {
        std::fs::read_to_string(status_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    } else {
        RelayStatus::default()
    };
    PhoneState {
        installed: crate::config::dirs_home().join(".local/bin/og-phone-audio").exists(),
        running,
        enabled: systemctl(&["is-enabled", "--quiet", SERVICE]),
        status,
        apk: apk_path(),
    }
}

/// Turn the relay on (enable at login + start now) or off.
pub fn set_enabled(on: bool) -> Result<(), String> {
    let ok = if on { systemctl(&["enable", "--now", SERVICE]) } else { systemctl(&["disable", "--now", SERVICE]) };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "systemctl --user {} {SERVICE} failed — is og-phone-audio installed (apps/og-phone-audio/scripts/install.sh)?",
            if on { "enable --now" } else { "disable --now" }
        ))
    }
}

/// Save + bounce the daemon if it's running so it re-reads the config.
pub fn apply(cfg: &PhoneAudioConfig) -> Result<(), String> {
    cfg.save()?;
    if systemctl(&["is-active", "--quiet", SERVICE]) && !systemctl(&["restart", SERVICE]) {
        return Err(format!("couldn't restart {SERVICE}"));
    }
    Ok(())
}

/// `adb install -r` the bundled APK onto a USB-connected phone.
pub fn adb_install() -> Result<String, String> {
    let apk = apk_path().ok_or("APK not built yet — run apps/og-phone-audio/scripts/install.sh")?;
    let devices = Command::new("adb").arg("devices").output().map_err(|e| format!("adb: {e}"))?;
    let listed = String::from_utf8_lossy(&devices.stdout);
    let lines: Vec<&str> = listed.lines().skip(1).filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return Err("No phone found. Plug it in over USB with USB debugging on.".into());
    }
    if lines.iter().all(|l| l.ends_with("unauthorized")) {
        return Err("Phone found but not authorized — accept the USB debugging prompt on the phone.".into());
    }
    let out = Command::new("adb").arg("install").arg("-r").arg(&apk).output().map_err(|e| format!("adb: {e}"))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        // Open it so the user can type the pair code straight away.
        let _ = Command::new("adb")
            .args(["shell", "am", "start", "-n", "com.omegagiven.ogphoneaudio/.MainActivity"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Ok("Installed on the phone and opened it — enter the pair code there.".into())
    } else {
        Err(text.trim().lines().last().unwrap_or("adb install failed").to_string())
    }
}
