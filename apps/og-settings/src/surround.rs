//! Experimental "Spatial" audio lab (Audio tab → Spatial subtab).
//!
//! Three output modes, all built from a single PipeWire `filter-chain`
//! drop-in plus the per-user `filter-chain.service` unit:
//!
//!   * **Stereo**  — no effect sink, the real device is the default (this
//!                   is "off": conf removed, unit stopped).
//!   * **Mono**    — a tiny filter-chain that sums L+R into both ears.
//!   * **Surround** — the HeSuVi HRIR convolver graph Arch ships at
//!     `/usr/share/pipewire/filter-chain/sink-virtual-surround-7.1-hesuvi.conf`,
//!     with `channelmix.upmix = true` so plain stereo is upmixed to 7.1
//!     before convolution and real 5.1/7.1 (a movie) passes straight in.
//!
//! Enable = write the drop-in, `systemctl --user enable --now
//! filter-chain.service`, wait for the effect sink to appear, make it the
//! default and move existing streams onto it. If the sink never appears we
//! roll all of that back and report why — that's the "revert to stereo if
//! the system can't upscale" behaviour.
//!
//! Same "own the thing you loaded into the audio server" shape as the
//! mic-monitor loopback in `audio.rs`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// `node.name` of the effect sink apps play into. Stable across presets so
/// `target.object` overrides and `pactl` moves keep working.
pub const SINK_NAME: &str = "effect_input.og-spatial";
const SERVICE: &str = "filter-chain.service";
/// First line we write into the drop-in — lets `current()` recover the
/// active mode without parsing the whole graph.
const MARKER: &str = "# og-spatial-mode:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    Atmos,
    DtsHeadphoneX,
    SennheiserGsx,
    WindowsSonic,
    Cmss3d,
    DolbyHeadphone,
    Dht,
    Ooyh,
    StudioRoom,
    DolbyVirtualSpeaker,
    WavesNx,
}

impl Preset {
    pub const ALL: [Preset; 11] = [
        Preset::Atmos,
        Preset::DtsHeadphoneX,
        Preset::SennheiserGsx,
        Preset::WindowsSonic,
        Preset::Cmss3d,
        Preset::DolbyHeadphone,
        Preset::Dht,
        Preset::Ooyh,
        Preset::StudioRoom,
        Preset::DolbyVirtualSpeaker,
        Preset::WavesNx,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::Atmos => "Dolby Atmos for Headphones",
            Preset::DtsHeadphoneX => "DTS Headphone:X",
            Preset::SennheiserGsx => "Sennheiser GSX",
            Preset::WindowsSonic => "Windows Sonic",
            Preset::Cmss3d => "Creative CMSS-3D (gaming positional)",
            Preset::DolbyHeadphone => "Dolby Headphone (strong rear)",
            Preset::Dht => "Dolby Headphone Theater (wide)",
            Preset::Ooyh => "Out Of Your Head (max externalization)",
            Preset::StudioRoom => "Studio room — reverby, best front/back",
            Preset::DolbyVirtualSpeaker => "Dolby Virtual Speaker",
            Preset::WavesNx => "Waves Nx",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Preset::Atmos => "atmos",
            Preset::DtsHeadphoneX => "dtshx",
            Preset::SennheiserGsx => "gsx",
            Preset::WindowsSonic => "sonic",
            Preset::Cmss3d => "cmss_game",
            Preset::DolbyHeadphone => "dh+",
            Preset::Dht => "dht",
            Preset::Ooyh => "ooyh1",
            Preset::StudioRoom => "ssc_syd",
            Preset::DolbyVirtualSpeaker => "dvs",
            Preset::WavesNx => "waves",
        }
    }

    fn wav_bytes(self) -> &'static [u8] {
        match self {
            Preset::Atmos => include_bytes!("assets/hrir/atmos.wav"),
            Preset::DtsHeadphoneX => include_bytes!("assets/hrir/dtshx.wav"),
            Preset::SennheiserGsx => include_bytes!("assets/hrir/gsx.wav"),
            Preset::WindowsSonic => include_bytes!("assets/hrir/sonic.wav"),
            Preset::Cmss3d => include_bytes!("assets/hrir/cmss_game.wav"),
            Preset::DolbyHeadphone => include_bytes!("assets/hrir/dh+.wav"),
            Preset::Dht => include_bytes!("assets/hrir/dht.wav"),
            Preset::Ooyh => include_bytes!("assets/hrir/ooyh1.wav"),
            Preset::StudioRoom => include_bytes!("assets/hrir/ssc_syd.wav"),
            Preset::DolbyVirtualSpeaker => include_bytes!("assets/hrir/dvs.wav"),
            Preset::WavesNx => include_bytes!("assets/hrir/waves.wav"),
        }
    }

    fn from_slug(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.slug() == s)
    }
}

impl std::fmt::Display for Preset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A real output device the binaural chain can be pointed at (name +
/// friendly label), for the "send output to" picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutDev {
    pub name: String,
    pub label: String,
}

impl std::fmt::Display for OutDev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Stereo,
    Mono,
    Surround(Preset),
}

impl Default for Mode {
    fn default() -> Self {
        Mode::Stereo
    }
}

/// How the `spatial` helper + its autoroute daemon are set to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouteMode {
    #[default]
    Off,
    /// 5.1/7.1 streams -> HRTF, stereo left on the real device
    Auto,
    /// every stream -> HRTF
    Always,
}

impl RouteMode {
    pub const ALL: [RouteMode; 3] = [RouteMode::Off, RouteMode::Auto, RouteMode::Always];
    pub fn label(self) -> &'static str {
        match self {
            RouteMode::Off => "Off",
            RouteMode::Auto => "Auto (5.1/7.1 only)",
            RouteMode::Always => "Always on",
        }
    }
    fn arg(self) -> &'static str {
        match self {
            RouteMode::Off => "off",
            RouteMode::Auto => "auto",
            RouteMode::Always => "always",
        }
    }
}

/// Current mode from `~/.local/state/spatial-mode` (default Off).
pub fn route_mode() -> RouteMode {
    match std::fs::read_to_string(home().join(".local/state/spatial-mode"))
        .map(|s| s.trim().to_string())
        .as_deref()
    {
        Ok("auto") => RouteMode::Auto,
        Ok("always") => RouteMode::Always,
        _ => RouteMode::Off,
    }
}

/// The preset the helper will use, from `~/.local/state/spatial-preset`.
pub fn saved_preset() -> Preset {
    std::fs::read_to_string(home().join(".local/state/spatial-preset"))
        .ok()
        .and_then(|s| Preset::from_slug(s.trim()))
        .unwrap_or(Preset::Atmos)
}

/// Drive the `spatial` helper: `spatial <mode> <preset> <device>`.
pub fn set_route(m: RouteMode, preset: Preset, device: &str) -> Result<(), String> {
    let mut args = vec![m.arg().to_string()];
    if m != RouteMode::Off {
        args.push(preset.slug().to_string());
        if !device.is_empty() && device != SINK_NAME {
            args.push(device.to_string());
        }
    }
    let out = Command::new("spatial")
        .args(&args)
        .output()
        .map_err(|e| format!("couldn't run `spatial`: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

// ── paths ──────────────────────────────────────────────────────────────

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn conf_path() -> PathBuf {
    home().join(".config/pipewire/filter-chain.conf.d/og-spatial.conf")
}

/// HRIR files are extracted here from the binary the first time a preset is
/// used, so the drop-in can point the convolver at a real absolute path.
fn hrir_dir() -> PathBuf {
    home().join(".local/share/og-settings/hrir")
}

fn hrir_path(p: Preset) -> PathBuf {
    hrir_dir().join(format!("{}.wav", p.slug()))
}

fn ensure_hrir(p: Preset) -> std::io::Result<PathBuf> {
    let path = hrir_path(p);
    let bytes = p.wav_bytes();
    let fresh = match std::fs::metadata(&path) {
        Ok(m) => m.len() != bytes.len() as u64,
        Err(_) => true,
    };
    if fresh {
        std::fs::create_dir_all(hrir_dir())?;
        std::fs::write(&path, bytes)?;
    }
    Ok(path)
}

// ── graph templates ────────────────────────────────────────────────────

/// The HeSuVi convolver graph. Prefer the distro copy (kept current by
/// pipewire updates); fall back to the snapshot bundled in the binary.
fn hesuvi_graph() -> String {
    std::fs::read_to_string("/usr/share/pipewire/filter-chain/sink-virtual-surround-7.1-hesuvi.conf")
        .unwrap_or_else(|_| include_str!("assets/hrir/hesuvi-graph.conf").to_string())
}

fn surround_conf(p: Preset, wav: &str, hw: &str) -> String {
    let mut g = hesuvi_graph();
    // point every convolver at our extracted file
    g = g.replace("\"hrir_hesuvi/hrir.wav\"", &format!("{wav:?}"));
    // stable node identity (no upmix / no forced latency — both caused
    // static on marginal USB DACs; 5.1/7.1 maps straight in, stereo stays
    // stereo through the front-L/R paths)
    // Plain ASCII description — an em-dash or the preset's own punctuation
    // (e.g. "DTS Headphone:X") in here gets the value rejected and the sink
    // ends up nameless ("(null)") in every device list.
    g = g.replace(
        "node.name      = \"effect_input.virtual-surround-7.1-hesuvi\"",
        &format!("node.name      = \"{SINK_NAME}\"\n                node.description = \"OG Spatial (Virtual Surround)\""),
    );
    g = g.replace(
        "node.name      = \"effect_output.virtual-surround-7.1-hesuvi\"",
        "node.name      = \"effect_output.og-spatial\"",
    );
    // Pin the real output device. `node.passive = true` lets WirePlumber
    // ignore target.object and re-home the output to whatever it thinks is
    // default; a non-passive output + dont-reconnect makes it a normal
    // stream that connects once to target.object and stays put.
    g = g.replace(
        "node.passive   = true",
        &format!(
            "node.passive   = false\n                target.object  = {hw:?}\n                node.dont-reconnect = true"
        ),
    );
    format!("{MARKER} surround:{}\n{g}", p.slug())
}

/// Minimal L+R -> mono filter-chain sink. Both mixers get both inputs, so
/// each ear hears the full mono sum.
fn mono_conf(hw: &str) -> String {
    format!(
        r#"{MARKER} mono
context.modules = [
    {{ name = libpipewire-module-filter-chain
        flags = [ nofail ]
        args = {{
            node.description = "OG Spatial (Mono)"
            media.name       = "OG Spatial Mono"
            filter.graph = {{
                nodes = [
                    {{ type = builtin label = mixer name = mixL }}
                    {{ type = builtin label = mixer name = mixR }}
                ]
                links = []
                inputs  = [ "mixL:In 1" "mixR:In 1" "mixL:In 2" "mixR:In 2" ]
                outputs = [ "mixL:Out" "mixR:Out" ]
            }}
            capture.props = {{
                node.name      = "{SINK_NAME}"
                node.description = "OG Spatial (Mono)"
                media.class    = Audio/Sink
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
            playback.props = {{
                node.name      = "effect_output.og-spatial"
                node.passive   = false
                target.object  = {hw:?}
                node.dont-reconnect = true
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
        }}
    }}
]
"#
    )
}

// ── pactl / systemctl helpers ──────────────────────────────────────────

fn pactl(args: &[&str]) -> String {
    Command::new("pactl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
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

fn sink_present() -> bool {
    pactl(&["list", "short", "sinks"]).contains(SINK_NAME)
}

/// Force BOTH the live and the *configured* (sticky) default sink to
/// `name`. `pactl set-default-sink` only updates the configured value, and
/// if the previous configured value was our effect sink WirePlumber can
/// snap back to it the moment it reappears — so also poke the metadata
/// directly.
fn force_default(name: &str) {
    if name.is_empty() {
        return;
    }
    pactl(&["set-default-sink", name]);
    let json = format!("{{ \"name\": \"{name}\" }}");
    for key in ["default.configured.audio.sink", "default.audio.sink"] {
        let _ = Command::new("pw-metadata")
            .args(["0", key, &json])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// WirePlumber ignores `target.object` on the passive effect-output node
/// and keeps re-linking it to whatever it thinks the default device is.
/// Force `effect_output.og-spatial` onto `hw` by hand and drop any link to
/// another real sink.
fn relink_output(hw: &str) {
    for (op, ip) in [("output_FL", "playback_FL"), ("output_FR", "playback_FR")] {
        let _ = Command::new("pw-link")
            .args([&format!("effect_output.og-spatial:{op}"), &format!("{hw}:{ip}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    for line in pactl(&["list", "short", "sinks"]).lines() {
        let name = line.split('\t').nth(1).unwrap_or("");
        if name.is_empty() || name == hw || name == SINK_NAME {
            continue;
        }
        for (op, ip) in [("output_FL", "playback_FL"), ("output_FR", "playback_FR")] {
            let _ = Command::new("pw-link")
                .args(["-d", &format!("effect_output.og-spatial:{op}"), &format!("{name}:{ip}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Is the HRTF sink actually the current default output? (Surround can be
/// *configured* — service up, conf present — while the user has switched
/// the default away, which silently bypasses the whole chain.)
pub fn is_active() -> bool {
    pactl(&["get-default-sink"]).trim() == SINK_NAME && sink_present()
}

/// Move every real playback stream onto `sink` (skips ones already there
/// and our own effect-chain internals).
fn move_streams_to(sink: &str) {
    for line in pactl(&["list", "short", "sink-inputs"]).lines() {
        if let Some(idx) = line.split('\t').next() {
            if !idx.trim().is_empty() {
                let _ = Command::new("pactl")
                    .args(["move-sink-input", idx.trim(), sink])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

// ── public API ─────────────────────────────────────────────────────────

/// The mode encoded in the current drop-in (Stereo if there is none).
pub fn current() -> Mode {
    let Ok(txt) = std::fs::read_to_string(conf_path()) else {
        return Mode::Stereo;
    };
    let first = txt.lines().next().unwrap_or("");
    let Some(rest) = first.strip_prefix(MARKER) else {
        return Mode::Stereo;
    };
    match rest.trim() {
        "mono" => Mode::Mono,
        s => s
            .strip_prefix("surround:")
            .and_then(Preset::from_slug)
            .map(Mode::Surround)
            .unwrap_or(Mode::Stereo),
    }
}

/// The real output device the *current* drop-in is pinned to (its
/// `target.object`), if surround/mono is already configured. Survives a
/// preset switch where the live default has become the effect sink.
pub fn configured_hw() -> Option<String> {
    let txt = std::fs::read_to_string(conf_path()).ok()?;
    let line = txt.lines().find(|l| l.contains("target.object"))?;
    let start = line.find('"')? + 1;
    let end = line[start..].find('"')? + start;
    Some(line[start..end].to_string())
}

/// The real hardware sink to fall back to — the current default unless it's
/// already our effect sink, in which case the first non-effect sink.
pub fn hardware_default() -> String {
    let def = pactl(&["get-default-sink"]).trim().to_string();
    if !def.is_empty() && def != SINK_NAME {
        return def;
    }
    pactl(&["list", "short", "sinks"])
        .lines()
        .filter_map(|l| l.split('\t').nth(1))
        .find(|n| !n.starts_with("effect_input.") && !n.starts_with("effect_output."))
        .unwrap_or("")
        .to_string()
}

/// Apply a mode. `hw` is the real sink to route through / fall back to
/// (pass [`hardware_default`]). Returns the mode actually in effect —
/// which is `Stereo` if a Surround/Mono sink failed to load.
pub fn apply(mode: Mode, hw: &str) -> Result<Mode, String> {
    match mode {
        Mode::Stereo => {
            // Figure out the real device to hand playback back to *before*
            // tearing the chain down.
            let back: String = if !hw.is_empty() && hw != SINK_NAME {
                hw.to_string()
            } else {
                configured_hw()
                    .filter(|s| !s.is_empty() && s != SINK_NAME)
                    .unwrap_or_else(hardware_default)
            };
            force_default(&back);
            systemctl(&["disable", "--now", SERVICE]);
            let _ = std::fs::remove_file(conf_path());
            force_default(&back);
            if !back.is_empty() {
                move_streams_to(&back);
            }
            Ok(Mode::Stereo)
        }
        Mode::Mono | Mode::Surround(_) => {
            // Resolve the real output device. `hw` from the caller can be
            // empty or (mid preset-switch) the effect sink itself — neither
            // is usable. Fall back to what the existing drop-in was pinned
            // to, then to the live hardware default.
            let hw_owned: String = if !hw.is_empty() && hw != SINK_NAME {
                hw.to_string()
            } else {
                configured_hw()
                    .filter(|s| !s.is_empty() && s != SINK_NAME)
                    .unwrap_or_else(hardware_default)
            };
            let hw = hw_owned.as_str();

            let conf = match mode {
                Mode::Mono => mono_conf(hw),
                Mode::Surround(p) => {
                    let wav = ensure_hrir(p).map_err(|e| format!("Couldn't unpack HRIR file: {e}"))?;
                    surround_conf(p, &wav.to_string_lossy(), hw)
                }
                Mode::Stereo => unreachable!(),
            };
            let path = conf_path();
            std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|_| {
                    let mut f = std::fs::File::create(&path)?;
                    f.write_all(conf.as_bytes())
                })
                .map_err(|e| format!("Couldn't write the PipeWire drop-in: {e}"))?;

            // Point the default at the real device BEFORE the restart drops
            // the effect sink — otherwise the default briefly references a
            // dead node and PipeWire spawns an auto-null in its place.
            force_default(hw);

            // restart, not start — picks up a preset change on an already
            // running unit too
            systemctl(&["enable", SERVICE]);
            if !systemctl(&["restart", SERVICE]) {
                let _ = std::fs::remove_file(&path);
                systemctl(&["disable", "--now", SERVICE]);
                return Err("filter-chain.service failed to start — check: journalctl --user -u filter-chain".into());
            }

            // wait for the effect sink to register (~3 s ceiling)
            let ok = (0..15).any(|_| {
                if sink_present() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
                false
            });
            if !ok {
                let _ = std::fs::remove_file(&path);
                systemctl(&["disable", "--now", SERVICE]);
                force_default(hw);
                return Err("The effect sink never appeared — your setup can't build this chain. Reverted to Stereo.".into());
            }

            std::thread::sleep(std::time::Duration::from_millis(400));
            relink_output(hw);
            if !hw.is_empty() {
                pactl(&["set-sink-volume", hw, "100%"]);
            }
            force_default(SINK_NAME);
            move_streams_to(SINK_NAME);
            Ok(mode)
        }
    }
}

// ── "what's actually playing" inspector ────────────────────────────────

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub app: String,
    /// e.g. "float32le 6ch 48000Hz"
    pub spec: String,
    pub channels: u8,
    /// friendly layout name: "mono" / "stereo" / "quad" / "5.1" / "7.1" / "6ch"
    pub layout: String,
    pub corked: bool,
    /// the device this stream ends up on (friendly label)
    pub dest: String,
    /// true if it is currently going through the HRTF sink
    pub via_hrtf: bool,
}

/// Read the live playback streams straight from `pactl -f json`, keeping
/// the channel/format detail the shared `AudioStream` type drops. This is
/// what answers "is this Jellyfin movie actually surround?".
pub fn active_streams() -> Vec<StreamInfo> {
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        sample_specification: String,
        #[serde(default)]
        channel_map: String,
        #[serde(default)]
        corked: bool,
        #[serde(default)]
        sink: i64,
        #[serde(default)]
        properties: std::collections::HashMap<String, String>,
    }
    #[derive(serde::Deserialize)]
    struct RawSink {
        index: i64,
        name: String,
        #[serde(default)]
        description: String,
    }
    let sink_out = Command::new("pactl")
        .args(["-f", "json", "list", "sinks"])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let sinks: Vec<RawSink> = serde_json::from_slice(&sink_out).unwrap_or_default();
    let sink_name = |id: i64| sinks.iter().find(|s| s.index == id);
    // the real device the HRTF chain feeds, for the "-> device" label
    let hrtf_out = configured_hw()
        .and_then(|n| sinks.iter().find(|s| s.name == n).map(|s| s.description.clone()))
        .unwrap_or_default();

    let out = Command::new("pactl")
        .args(["-f", "json", "list", "sink-inputs"])
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let raws: Vec<Raw> = serde_json::from_slice(&out).unwrap_or_default();
    raws.into_iter()
        .map(|r| {
            let cur = sink_name(r.sink);
            let via_hrtf = cur.map(|s| s.name == SINK_NAME).unwrap_or(false);
            let dest = if via_hrtf {
                if hrtf_out.is_empty() { "headphones".to_string() } else { hrtf_out.clone() }
            } else {
                cur.map(|s| s.description.clone()).unwrap_or_else(|| "?".to_string())
            };
            let channels = r.channel_map.split(',').filter(|s| !s.is_empty()).count() as u8;
            let layout = match channels {
                1 => "mono".to_string(),
                2 => "stereo".to_string(),
                4 => "quad".to_string(),
                6 => "5.1".to_string(),
                8 => "7.1".to_string(),
                0 => "?".to_string(),
                n => format!("{n}ch"),
            };
            let app = r
                .properties
                .get("application.name")
                .or_else(|| r.properties.get("media.name"))
                .or_else(|| r.properties.get("node.name"))
                .cloned()
                .unwrap_or_else(|| "Stream".to_string());
            StreamInfo {
                app,
                spec: r.sample_specification,
                channels,
                layout,
                corked: r.corked,
                dest,
                via_hrtf,
            }
        })
        .collect()
}

// ── test signals ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestSignal {
    /// 0-based index into FL FR FC LFE RL RR SL SR
    Channel(u8),
    /// walk every 7.1 channel in turn, spoken names
    AllChannels,
    LeftOnly,
    RightOnly,
    Sweep,
}

pub const CHANNEL_LABELS: [&str; 8] =
    ["Front L", "Front R", "Center", "LFE", "Rear L", "Rear R", "Side L", "Side R"];

fn sweep_wav() -> std::io::Result<PathBuf> {
    let path = home().join(".local/share/og-settings/sweep_20-20k.wav");
    if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(path);
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    // 20 Hz -> 20 kHz exponential sweep, 12 s, stereo, ~-8 dBFS
    let expr = "0.4*sin(6.28318530718*20*12/log(1000)*(exp(t/12*log(1000))-1))";
    let ok = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "lavfi", "-i",
            &format!("aevalsrc={expr}:d=12:s=48000:c=stereo"),
            "-af", "afade=t=in:d=0.05,afade=t=out:st=11.6:d=0.4",
            "-c:a", "pcm_s16le",
        ])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(std::io::Error::new(std::io::ErrorKind::Other, "ffmpeg failed to render the sweep"));
    }
    Ok(path)
}

/// Fire a test tone at the current default sink (so with a mode active you
/// hear it through the effect chain). Returns the child's PID so the UI
/// can stop it; the process is short-lived and detached otherwise.
pub fn play(sig: TestSignal) -> Option<u32> {
    let mut cmd = match sig {
        TestSignal::Channel(n) => {
            // speaker-test plays its Nth *native* channel; native 8ch order
            // is FL FR RL RR FC LFE SL SR, so remap our slot -> its number.
            // slots: 0 FL 1 FR 2 FC 3 LFE 4 RL 5 RR 6 SL 7 SR
            const SPK: [u8; 8] = [1, 3, 2, 8, 6, 5, 7, 4]; // slot(FL FR FC LFE RL RR SL SR) -> speaker-test -s (its 8ch order: FL FC FR SR RR RL SL LFE)
            let s = SPK.get(n as usize).copied().unwrap_or(1);
            let mut c = Command::new("speaker-test");
            c.args(["-D", "pipewire", "-c", "8", "-t", "wav", "-l", "1", "-s", &s.to_string()]);
            c
        }
        TestSignal::AllChannels => {
            // no -s: speaker-test walks every channel in turn, spoken names
            // from /usr/share/sounds/alsa/*.wav
            let mut c = Command::new("speaker-test");
            c.args(["-D", "pipewire", "-c", "8", "-t", "wav", "-l", "1"]);
            c
        }
        TestSignal::LeftOnly | TestSignal::RightOnly => {
            let s = if matches!(sig, TestSignal::LeftOnly) { "1" } else { "2" };
            let mut c = Command::new("speaker-test");
            c.args(["-D", "pipewire", "-c", "2", "-t", "pink", "-l", "1", "-s", s]);
            c
        }
        TestSignal::Sweep => {
            let wav = sweep_wav().ok()?;
            let mut c = Command::new("pw-play");
            c.arg(wav);
            c
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().ok().map(|child| child.id())
}

/// 8-channel WAV with a short broadband noise-burst train in exactly one
/// channel (`slot` 0-7 = FL FR FC LFE RL RR SL SR). Sharp repeated onsets +
/// HF energy localise far better than continuous pink noise. Cached.
fn stage_wav(slot: u8) -> Option<PathBuf> {
    let path = home().join(format!(".local/share/og-settings/stage_ch{slot}.wav"));
    if std::fs::metadata(&path).map(|m| m.len() > 0).unwrap_or(false) {
        return Some(path);
    }
    std::fs::create_dir_all(path.parent()?).ok()?;
    // one clean 523 Hz (C5) tone, ~1.1 s, soft fades. panned straight from
    // the mono source into channel `slot` (an upmix before pan leaves c0
    // empty). Pure tones localise worse than noise, but the user asked for
    // a tone.
    let ok = Command::new("ffmpeg")
        .args([
            "-y", "-v", "error", "-f", "lavfi", "-i",
            "sine=frequency=440:duration=1.1:sample_rate=48000",
            "-af",
            // ffmpeg's `sine` is only ~-18 dBFS, so lift it ~+14 dB first
            &format!("volume=5,afade=t=in:d=0.02,afade=t=out:st=0.95:d=0.15,pan=7.1|c{slot}=c0"),
            "-c:a", "pcm_s16le",
        ])
        .arg(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(path)
}

/// Fire one channel with an unnamed burst train — for the soundstage /
/// localization game where announcing the channel would give it away.
pub fn play_channel_blind(n: u8) -> Option<u32> {
    let wav = stage_wav(n)?;
    let mut c = Command::new("pw-play");
    if sink_present() {
        c.args(["--target", SINK_NAME]);
    }
    c.arg(wav)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()
        .map(|ch| ch.id())
}

pub fn stop(pid: u32) {
    let _ = Command::new("kill").arg(pid.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surround_conf_is_wired_for_our_sink() {
        let c = surround_conf(Preset::Atmos, "/home/x/.local/share/og-settings/hrir/atmos.wav", "alsa_output.test");
        assert!(c.starts_with("# og-spatial-mode: surround:atmos\n"));
        // convolver repointed at our file, none left on the template path
        assert!(c.contains("\"/home/x/.local/share/og-settings/hrir/atmos.wav\""));
        assert!(!c.contains("hrir_hesuvi/hrir.wav"));
        // our stable node identity + pinned output device
        assert!(c.contains("node.name      = \"effect_input.og-spatial\""));
        assert!(c.contains("node.name      = \"effect_output.og-spatial\""));
        assert!(c.contains("target.object  = \"alsa_output.test\""));
        assert!(!c.contains("channelmix.upmix = true"));
        assert!(!c.contains("virtual-surround-7.1-hesuvi"));
    }

    #[test]
    fn mode_round_trips_through_the_marker() {
        for (line, want) in [
            ("# og-spatial-mode: mono", Mode::Mono),
            ("# og-spatial-mode: surround:gsx", Mode::Surround(Preset::SennheiserGsx)),
            ("# og-spatial-mode: surround:bogus", Mode::Stereo),
        ] {
            let rest = line.strip_prefix(MARKER).unwrap().trim();
            let got = match rest {
                "mono" => Mode::Mono,
                s => s
                    .strip_prefix("surround:")
                    .and_then(Preset::from_slug)
                    .map(Mode::Surround)
                    .unwrap_or(Mode::Stereo),
            };
            assert_eq!(got, want);
        }
    }
}
