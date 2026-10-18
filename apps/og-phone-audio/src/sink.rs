//! The `og_phone` null sink: apps play into it, `parec` reads its monitor.
//! Same "own the module you loaded into the audio server" shape as
//! og-settings' mic-monitor loopback — always unload it on the way out.

use std::process::Command;

pub const SINK_NAME: &str = "og_phone";

fn pactl(args: &[&str]) -> Option<String> {
    let o = Command::new("pactl").args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn default_sink() -> Option<String> {
    pactl(&["get-default-sink"]).filter(|s| !s.is_empty())
}

fn sink_exists(name: &str) -> bool {
    pactl(&["list", "short", "sinks"])
        .map(|out| out.lines().any(|l| l.split('\t').nth(1) == Some(name)))
        .unwrap_or(false)
}

/// Move every playing stream onto `sink`.
fn move_all_inputs(sink: &str) {
    if let Some(out) = pactl(&["list", "short", "sink-inputs"]) {
        for idx in out.lines().filter_map(|l| l.split('\t').next()) {
            let _ = pactl(&["move-sink-input", idx, sink]);
        }
    }
}

/// Indices of every module-null-sink we loaded, past or present.
fn our_modules() -> Vec<String> {
    pactl(&["list", "short", "modules"])
        .map(|out| {
            out.lines()
                .filter(|l| l.contains("module-null-sink") && l.contains(&format!("sink_name={SINK_NAME}")))
                .filter_map(|l| l.split('\t').next().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Remembers which output was default before we took over, in a file, so a
/// crash mid-session can still be undone by the next start.
fn prev_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(dir).join("og-phone-audio.prev-sink")
}

pub struct PhoneSink {
    module: String,
    prev_default: Option<String>,
}

impl PhoneSink {
    pub fn load(make_default: bool) -> Result<Self, String> {
        let module = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK_NAME}"),
            "rate=48000",
            "channels=2",
            "channel_map=front-left,front-right",
            "sink_properties=device.description=Phone\\ (OG\\ relay) device.icon_name=phone",
        ])
        .ok_or("pactl load-module module-null-sink failed")?;

        let mut prev_default = None;
        if make_default {
            prev_default = default_sink().filter(|s| s != SINK_NAME);
            if let Some(p) = &prev_default {
                let _ = std::fs::write(prev_path(), p);
            }
            let _ = pactl(&["set-default-sink", SINK_NAME]);
            move_all_inputs(SINK_NAME);
        }
        Ok(Self { module, prev_default })
    }

    pub fn unload(self) {
        restore_default(self.prev_default.as_deref());
        let _ = pactl(&["unload-module", &self.module]);
        let _ = std::fs::remove_file(prev_path());
    }
}

/// Point the default (and every stream still on our sink) back at `prev`,
/// but only if the user hasn't picked something else in the meantime.
fn restore_default(prev: Option<&str>) {
    let Some(prev) = prev.filter(|p| sink_exists(p)) else { return };
    if default_sink().as_deref() == Some(SINK_NAME) {
        let _ = pactl(&["set-default-sink", prev]);
        move_all_inputs(prev);
    }
}

pub fn cleanup_stale() {
    let mods = our_modules();
    if mods.is_empty() {
        return;
    }
    let prev = std::fs::read_to_string(prev_path()).ok();
    restore_default(prev.as_deref().map(str::trim));
    for m in mods {
        let _ = pactl(&["unload-module", &m]);
    }
    let _ = std::fs::remove_file(prev_path());
}
