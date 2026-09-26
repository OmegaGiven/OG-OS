//! PulseAudio/PipeWire-pulse control — the pavucontrol-equivalent backend
//! for the Audio tab. All reads go through `pactl -f json ...`, which this
//! system's pactl (17.0) supports and gives real structured data instead
//! of regex-parsing plain text — verified directly against this machine's
//! actual sinks/sources/cards before writing the parsers below.

use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct RawVolumeChannel {
    value_percent: String,
}

#[derive(Debug, Deserialize)]
struct RawDevice {
    index: u32,
    name: String,
    description: String,
    mute: bool,
    volume: HashMap<String, RawVolumeChannel>,
}

#[derive(Debug, Clone)]
pub struct AudioDevice {
    pub index: u32,
    pub name: String,
    pub description: String,
    /// Average across channels — pavucontrol links channels by default too
    /// unless you dig into per-channel balance, which this doesn't expose.
    pub volume_pct: u32,
    pub mute: bool,
    pub is_default: bool,
}

fn parse_devices(json: &[u8], default_name: &str) -> Vec<AudioDevice> {
    let raw: Vec<RawDevice> = serde_json::from_slice(json).unwrap_or_default();
    raw.into_iter()
        .map(|d| {
            let pcts: Vec<u32> = d
                .volume
                .values()
                .filter_map(|c| c.value_percent.trim_end_matches('%').parse().ok())
                .collect();
            let volume_pct = if pcts.is_empty() { 0 } else { pcts.iter().sum::<u32>() / pcts.len() as u32 };
            AudioDevice {
                is_default: d.name == default_name,
                index: d.index,
                name: d.name,
                description: d.description,
                volume_pct,
                mute: d.mute,
            }
        })
        .collect()
}

pub fn get_default_sink() -> String {
    run(&["get-default-sink"]).trim().to_string()
}

pub fn get_default_source() -> String {
    run(&["get-default-source"]).trim().to_string()
}

pub fn list_sinks() -> Vec<AudioDevice> {
    let default = get_default_sink();
    parse_devices(&output(&["-f", "json", "list", "sinks"]), &default)
}

pub fn list_sources() -> Vec<AudioDevice> {
    let default = get_default_source();
    parse_devices(&output(&["-f", "json", "list", "sources"]), &default)
}

#[derive(Debug, Deserialize)]
struct RawStream {
    index: u32,
    mute: bool,
    volume: HashMap<String, RawVolumeChannel>,
    properties: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub index: u32,
    pub app_name: String,
    pub volume_pct: u32,
    pub mute: bool,
}

fn parse_streams(json: &[u8]) -> Vec<AudioStream> {
    let raw: Vec<RawStream> = serde_json::from_slice(json).unwrap_or_default();
    raw.into_iter()
        .map(|s| {
            let pcts: Vec<u32> = s
                .volume
                .values()
                .filter_map(|c| c.value_percent.trim_end_matches('%').parse().ok())
                .collect();
            let volume_pct = if pcts.is_empty() { 0 } else { pcts.iter().sum::<u32>() / pcts.len() as u32 };
            let app_name = s
                .properties
                .get("application.name")
                .or_else(|| s.properties.get("node.name"))
                .or_else(|| s.properties.get("media.name"))
                .cloned()
                .unwrap_or_else(|| format!("Stream #{}", s.index));
            AudioStream { index: s.index, app_name, volume_pct, mute: s.mute }
        })
        .collect()
}

pub fn list_sink_inputs() -> Vec<AudioStream> {
    parse_streams(&output(&["-f", "json", "list", "sink-inputs"]))
}

pub fn list_source_outputs() -> Vec<AudioStream> {
    parse_streams(&output(&["-f", "json", "list", "source-outputs"]))
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    description: String,
    available: bool,
}

#[derive(Debug, Deserialize)]
struct RawCard {
    index: u32,
    name: String,
    active_profile: Option<String>,
    profiles: HashMap<String, RawProfile>,
    properties: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct AudioProfile {
    pub id: String,
    pub description: String,
    pub available: bool,
}

#[derive(Debug, Clone)]
pub struct AudioCard {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub active_profile: String,
    pub profiles: Vec<AudioProfile>,
}

pub fn list_cards() -> Vec<AudioCard> {
    let raw: Vec<RawCard> = serde_json::from_slice(&output(&["-f", "json", "list", "cards"])).unwrap_or_default();
    raw.into_iter()
        .map(|c| {
            let description = c
                .properties
                .get("device.description")
                .or_else(|| c.properties.get("device.nick"))
                .cloned()
                .unwrap_or_else(|| c.name.clone());
            let mut profiles: Vec<AudioProfile> = c
                .profiles
                .into_iter()
                .map(|(id, p)| AudioProfile { id, description: p.description, available: p.available })
                .collect();
            profiles.sort_by(|a, b| a.id.cmp(&b.id));
            AudioCard { index: c.index, name: c.name, description, active_profile: c.active_profile.unwrap_or_default(), profiles }
        })
        .collect()
}

fn output(args: &[&str]) -> Vec<u8> {
    Command::new("pactl").args(args).output().map(|o| o.stdout).unwrap_or_default()
}

fn run(args: &[&str]) -> String {
    String::from_utf8(output(args)).unwrap_or_default()
}

pub fn set_sink_volume(name: &str, pct: u32) {
    let _ = Command::new("pactl").args(["set-sink-volume", name, &format!("{pct}%")]).status();
}

pub fn set_source_volume(name: &str, pct: u32) {
    let _ = Command::new("pactl").args(["set-source-volume", name, &format!("{pct}%")]).status();
}

pub fn set_sink_input_volume(index: u32, pct: u32) {
    let _ = Command::new("pactl").args(["set-sink-input-volume", &index.to_string(), &format!("{pct}%")]).status();
}

pub fn set_source_output_volume(index: u32, pct: u32) {
    let _ = Command::new("pactl").args(["set-source-output-volume", &index.to_string(), &format!("{pct}%")]).status();
}

pub fn set_sink_mute(name: &str, mute: bool) {
    let _ = Command::new("pactl").args(["set-sink-mute", name, if mute { "1" } else { "0" }]).status();
}

pub fn set_source_mute(name: &str, mute: bool) {
    let _ = Command::new("pactl").args(["set-source-mute", name, if mute { "1" } else { "0" }]).status();
}

pub fn set_sink_input_mute(index: u32, mute: bool) {
    let _ = Command::new("pactl").args(["set-sink-input-mute", &index.to_string(), if mute { "1" } else { "0" }]).status();
}

pub fn set_source_output_mute(index: u32, mute: bool) {
    let _ = Command::new("pactl").args(["set-source-output-mute", &index.to_string(), if mute { "1" } else { "0" }]).status();
}

pub fn set_default_sink(name: &str) {
    let _ = Command::new("pactl").args(["set-default-sink", name]).status();
}

pub fn set_default_source(name: &str) {
    let _ = Command::new("pactl").args(["set-default-source", name]).status();
}

/// "Hear yourself" — loops `source_name` straight into the current default
/// sink via `module-loopback`, so debugging a mic (clarity, background
/// noise, whether it sounds muffled/clipped) doesn't require alt-tabbing
/// into a Discord call or the target game just to hear it. `latency_msec=1`
/// keeps the round-trip short enough that it reads as "live" rather than a
/// delayed echo. Returns the loaded module's index so it can be unloaded
/// later — pactl doesn't give loopbacks a stable name, only a numeric index.
pub fn start_mic_monitor(source_name: &str) -> Option<u32> {
    let out = Command::new("pactl")
        .args(["load-module", "module-loopback", &format!("source={source_name}"), "latency_msec=1"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

pub fn stop_mic_monitor(module_index: u32) {
    let _ = Command::new("pactl").args(["unload-module", &module_index.to_string()]).status();
}

pub fn set_card_profile(card_name: &str, profile_id: &str) {
    let _ = Command::new("pactl").args(["set-card-profile", card_name, profile_id]).status();
}

/// Identifies which pactl object a volume/mute/default action applies to —
/// sinks/sources are addressed by name (pactl accepts either, name is more
/// stable across a device unplug/replug than an index would be),
/// sink-inputs/source-outputs only ever have an index (they're transient
/// per-stream, no stable name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioTarget {
    Sink(String),
    Source(String),
    SinkInput(u32),
    SourceOutput(u32),
}

pub fn set_volume(target: &AudioTarget, pct: u32) {
    match target {
        AudioTarget::Sink(name) => set_sink_volume(name, pct),
        AudioTarget::Source(name) => set_source_volume(name, pct),
        AudioTarget::SinkInput(i) => set_sink_input_volume(*i, pct),
        AudioTarget::SourceOutput(i) => set_source_output_volume(*i, pct),
    }
}

pub fn set_mute(target: &AudioTarget, mute: bool) {
    match target {
        AudioTarget::Sink(name) => set_sink_mute(name, mute),
        AudioTarget::Source(name) => set_source_mute(name, mute),
        AudioTarget::SinkInput(i) => set_sink_input_mute(*i, mute),
        AudioTarget::SourceOutput(i) => set_source_output_mute(*i, mute),
    }
}

#[derive(Debug, Clone, Default)]
pub struct AudioSnapshot {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub sink_inputs: Vec<AudioStream>,
    pub source_outputs: Vec<AudioStream>,
    pub cards: Vec<AudioCard>,
}

pub fn snapshot() -> AudioSnapshot {
    AudioSnapshot {
        sinks: list_sinks(),
        sources: list_sources(),
        sink_inputs: list_sink_inputs(),
        source_outputs: list_source_outputs(),
        cards: list_cards(),
    }
}
