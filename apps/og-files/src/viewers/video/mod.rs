//! Video player: mp4, mkv, webm, mov, avi… played by an embedded **libmpv**
//! (loaded at runtime, see `mpv.rs`), with hardware decoding
//! (`hwdec=auto-safe`), mpv's A/V sync, scaletempo pitch correction,
//! subtitles and track selection. Frames come from mpv's SW render API and
//! reach the screen through a `shader` widget that owns one texture
//! (`screen.rs`), so playback never touches iced's image cache.
//!
//! Without libmpv (or with `OGX_NO_MPV=1`) the viewer falls back to the
//! FFmpeg-library player in `ff.rs`, which also provides grid thumbnails and
//! the stream-copy trim exporter for both. Builds without the `ffmpeg`
//! feature use `ff_none.rs` instead: no thumbnails, no trim export, and an
//! "Install mpv" placeholder when libmpv can't be loaded — except on macOS,
//! where `av.rs` plays through AVFoundation (the Mac App Store build, which
//! ships neither libmpv nor FFmpeg).
//!
//! Like mpv's watch-later, the position of every file is remembered in
//! `$XDG_STATE_HOME/og-files/video-positions.json` and resumed silently.
//! The other videos in the folder form a playlist (`<` / `>`), switched
//! inside the viewer with `loadfile`.
//!
//! Edit mode never touches the original: "Export trim" stream-copies In..Out
//! into `<stem>-edit.<ext>`, "Snapshot frame" writes
//! `<stem>-frame-MMmSSs.png`; both report through `Event::Status`.

mod common;
#[cfg(feature = "ffmpeg")]
mod ff;
// macOS without FFmpeg (the App Store build): AVFoundation fills the slot
#[cfg(all(not(feature = "ffmpeg"), target_os = "macos"))]
#[path = "av.rs"]
mod ff;
#[cfg(all(not(feature = "ffmpeg"), not(target_os = "macos")))]
#[path = "ff_none.rs"]
mod ff;
mod mpv;
mod screen;
mod seekbar;

use super::Event;
use crate::theme;
use crate::widgets::icon;
use common::{fmt_mark, fmt_time, ExportEv, Info, AMBER};
use iced::futures::SinkExt;
use iced::keyboard::{key::Named, Key, Modifiers};
use iced::widget::{
    button, canvas, column, container, pick_list, row, shader, slider, stack, text, Space,
};
use iced::{Alignment, Element, Length, Task};
use iced_fonts::Bootstrap as B;
use mpv::{Node, Player, UiEv};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "ogv", "3gp",
];
/// Aspect overrides cycled by the aspect button / `A`.
const ASPECTS: [&str; 6] = ["no", "16:9", "4:3", "2.35:1", "1.85:1", "1:1"];
/// Fullscreen controls hide after this long without pointer motion.
const HIDE_AFTER: Duration = Duration::from_millis(2500);
/// mpv audio outputs tried in order.
#[cfg(target_os = "macos")]
const AO: &str = "coreaudio";
#[cfg(windows)]
const AO: &str = "wasapi";
#[cfg(not(any(target_os = "macos", windows)))]
const AO: &str = "pipewire,pulse,alsa";

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

/// First-frame-ish thumbnail (seek ~10% in) scaled to fit `max`×`max`, for
/// the grid view. Blocking; None on any failure.
pub fn thumbnail(path: &Path, max: u32) -> Option<image::RgbaImage> {
    ff::thumbnail(path, max)
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    pub info: Info,
    /// the videos of the folder in display order (contains `path`)
    pub playlist: Vec<PathBuf>,
}

/// Load off the UI thread (probe + folder listing in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    tokio::task::spawn_blocking(move || {
        let info = match ff::probe(&path) {
            Ok(i) => i,
            // FFmpeg can't read it but mpv may still play it
            Err(_) if backend() == Backend::Mpv && path.is_file() => Info::default(),
            Err(e) => return Err(e),
        };
        let playlist = folder_playlist(&path);
        Ok(Opened {
            path,
            info,
            playlist,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ════════════════════════════════════════════════════════════ backend choice

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Mpv,
    Ffmpeg,
}

/// libmpv when it loads and isn't disabled, FFmpeg otherwise.
fn select_backend(lib: Result<(), &str>, disabled: bool) -> Backend {
    match lib {
        Ok(()) if !disabled => Backend::Mpv,
        _ => Backend::Ffmpeg,
    }
}

fn backend() -> Backend {
    select_backend(
        mpv::api().map(|_| ()),
        std::env::var_os("OGX_NO_MPV").is_some_and(|v| v != "0"),
    )
}

/// mpv options for playing `path` (set before `mpv_initialize`).
fn mpv_options(path: &Path, mute: bool) -> Vec<(String, String)> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('%', "%%");
    let mut o: Vec<(String, String)> = [
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("vo", "libmpv"),
        ("hwdec", "auto-safe"),
        ("ao", AO),
        ("audio-client-name", "og-files"),
        ("keep-open", "yes"),
        ("idle", "yes"),
        ("pause", "yes"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("osc", "no"),
        ("terminal", "no"),
        ("sub-auto", "fuzzy"),
        ("audio-file-auto", "fuzzy"),
        ("video-sync", "audio"),
        ("interpolation", "no"),
        ("scale", "spline36"),
        ("cscale", "spline36"),
        ("dscale", "mitchell"),
        ("volume-max", "130"),
        ("audio-pitch-correction", "yes"),
        // mpv's SW renderer aborts on rotated video (mp_image_crop
        // assertion, mpv 0.41): rotation is done by our shader instead
        ("video-rotate", "no"),
        // the render buffer is the fitted video rect: no padding inside it
        ("keepaspect", "yes"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    o.push((
        "screenshot-directory".into(),
        dir.to_string_lossy().into_owned(),
    ));
    o.push((
        "screenshot-template".into(),
        format!("{stem}-frame-%wH%wM%wS"),
    ));
    if mute {
        o.push(("mute".into(), "yes".into()));
    }
    o
}

// ═══════════════════════════════════════════════════════════ property parsing

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub id: i64,
    /// "video" / "audio" / "sub"
    pub kind: String,
    pub title: Option<String>,
    pub lang: Option<String>,
    pub codec: Option<String>,
    pub selected: bool,
    pub external: bool,
    pub channels: Option<i64>,
    pub samplerate: Option<i64>,
    /// embedded cover art (not a real video)
    pub image: bool,
}

impl Track {
    fn label(&self) -> String {
        let mut s = format!("#{}", self.id);
        if let Some(l) = &self.lang {
            s += &format!(" {l}");
        }
        if let Some(t) = &self.title {
            s += &format!(" “{t}”");
        }
        let mut tech = vec![];
        if let Some(c) = &self.codec {
            tech.push(c.clone());
        }
        if let Some(ch) = self.channels {
            tech.push(format!("{ch}ch"));
        }
        if self.external {
            tech.push("external".into());
        }
        if !tech.is_empty() {
            s += &format!(" ({})", tech.join(", "));
        }
        s
    }
}

/// `track-list` node → tracks.
fn parse_tracks(n: &Node) -> Vec<Track> {
    n.array()
        .iter()
        .filter_map(|t| {
            let s = |k: &str| t.get(k).and_then(Node::str).map(str::to_string);
            Some(Track {
                id: t.get("id")?.i64()?,
                kind: s("type")?,
                title: s("title"),
                lang: s("lang"),
                codec: s("codec"),
                selected: t.get("selected").and_then(Node::flag).unwrap_or(false),
                external: t.get("external").and_then(Node::flag).unwrap_or(false),
                channels: t.get("demux-channel-count").and_then(Node::i64),
                samplerate: t.get("demux-samplerate").and_then(Node::i64),
                image: t.get("image").and_then(Node::flag).unwrap_or(false),
            })
        })
        .collect()
}

/// `chapter-list` node → chapter start times (s).
fn parse_chapters(n: &Node) -> Vec<f64> {
    n.array()
        .iter()
        .filter_map(|c| c.get("time").and_then(Node::f64))
        .collect()
}

/// A pick_list entry: a track or "Off".
#[derive(Debug, Clone, PartialEq)]
pub struct TrackChoice {
    pub id: Option<i64>,
    pub label: String,
}

impl std::fmt::Display for TrackChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

/// Choices for one track kind (+ "Off" for subtitles) and the current one.
fn track_choices(tracks: &[Track], kind: &str) -> (Vec<TrackChoice>, Option<TrackChoice>) {
    let mut v = vec![];
    if kind == "sub" {
        v.push(TrackChoice {
            id: None,
            label: "Subtitles off".into(),
        });
    }
    let mut cur = v.first().cloned();
    for t in tracks.iter().filter(|t| t.kind == kind) {
        let c = TrackChoice {
            id: Some(t.id),
            label: t.label(),
        };
        if t.selected {
            cur = Some(c.clone());
        }
        v.push(c);
    }
    (v, cur)
}

// ══════════════════════════════════════════════════════════ resume positions

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
struct Saved {
    pos: f64,
    dur: f64,
    /// unix seconds (oldest entries are dropped first)
    at: u64,
}

/// Entries kept in the store.
const RESUME_MAX: usize = 1000;

fn resume_store() -> Option<PathBuf> {
    let dir = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v).join("og-files"),
        None => crate::config::state_dir(),
    };
    Some(dir.join("video-positions.json"))
}

/// Worth resuming: past the first 10 s and not at the end.
fn should_resume(pos: f64, dur: f64) -> bool {
    pos > 10.0 && (dur <= 0.0 || (pos < dur - 10.0 && pos < dur * 0.95))
}

fn resume_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn load_positions(store: &Path) -> HashMap<String, Saved> {
    std::fs::read(store)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Saved position for `path`, if it should be resumed.
fn lookup_position(store: &Path, path: &Path) -> Option<f64> {
    load_positions(store)
        .get(&resume_key(path))
        .filter(|s| should_resume(s.pos, s.dur))
        .map(|s| s.pos)
}

/// Remember (or forget, at the start/end) the position of `path`. Only
/// writes when something changed.
fn record_position(store: &Path, path: &Path, pos: f64, dur: f64) {
    let mut all = load_positions(store);
    let key = resume_key(path);
    if should_resume(pos, dur) {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        all.insert(key, Saved { pos, dur, at });
        while all.len() > RESUME_MAX {
            let Some(old) = all.iter().min_by_key(|(_, s)| s.at).map(|(k, _)| k.clone()) else {
                break;
            };
            all.remove(&old);
        }
    } else if all.remove(&key).is_none() {
        return;
    }
    let Ok(json) = serde_json::to_vec(&all) else {
        return;
    };
    if let Some(dir) = store.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = store.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, store);
    }
}

// ═════════════════════════════════════════════════════════════════ playlist

/// The videos in `path`'s folder in natural name order (always contains
/// `path` itself).
fn folder_playlist(path: &Path) -> Vec<PathBuf> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| supported(p) && p.is_file())
                .collect()
        })
        .unwrap_or_default();
    let name = |p: &PathBuf| {
        p.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    };
    v.sort_by(|a, b| crate::fsops::natural_cmp(&name(a), &name(b)));
    if !v.iter().any(|p| p == path) {
        v = vec![path.to_path_buf()];
    }
    v
}

/// Index after moving `d` entries in a list of `n` (wrapping).
fn step_index(i: usize, d: i64, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    (i as i64 + d).rem_euclid(n as i64) as usize
}

// ═══════════════════════════════════════════════════════════════════ keys

/// What a key does (mpv's default bindings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    TogglePause,
    /// seconds, exact
    Seek(f64, bool),
    SpeedMul(f64),
    SpeedReset,
    Mute,
    Volume(f64),
    Fullscreen,
    LeaveFullscreen,
    FrameStep,
    FrameBack,
    SubToggle,
    SubCycle,
    AudioCycle,
    /// with subtitles?
    Screenshot(bool),
    AbLoop,
    LoopFile,
    Chapter(i64),
    File(i64),
    Stats,
    SubDelay(f64),
    AudioDelay(f64),
    Aspect,
    Zoom(f64),
    Pan(f64, f64),
    ZoomReset,
    Edge,
}

/// The key table. None = not ours (the app handles it).
pub fn key_action(key: &Key, mods: Modifiers) -> Option<Action> {
    use Action as A;
    let (shift, alt) = (mods.shift(), mods.alt());
    if mods.logo() {
        return None;
    }
    if mods.control() {
        // mpv: Ctrl +/- audio delay; every other Ctrl chord is the app's
        return match key {
            Key::Character(c) if !alt => match c.as_str() {
                "+" | "=" => Some(A::AudioDelay(0.1)),
                "-" => Some(A::AudioDelay(-0.1)),
                _ => None,
            },
            _ => None,
        };
    }
    match key {
        Key::Named(n) => Some(match (n, alt) {
            (Named::Space, false) => A::TogglePause,
            (Named::ArrowLeft, false) if shift => A::Seek(-1.0, true),
            (Named::ArrowRight, false) if shift => A::Seek(1.0, true),
            (Named::ArrowUp, false) if shift => A::Seek(5.0, true),
            (Named::ArrowDown, false) if shift => A::Seek(-5.0, true),
            (Named::ArrowLeft, false) => A::Seek(-5.0, false),
            (Named::ArrowRight, false) => A::Seek(5.0, false),
            (Named::ArrowUp, false) => A::Seek(60.0, false),
            (Named::ArrowDown, false) => A::Seek(-60.0, false),
            (Named::ArrowLeft, true) => A::Pan(0.1, 0.0),
            (Named::ArrowRight, true) => A::Pan(-0.1, 0.0),
            (Named::ArrowUp, true) => A::Pan(0.0, 0.1),
            (Named::ArrowDown, true) => A::Pan(0.0, -0.1),
            (Named::Backspace, false) => A::SpeedReset,
            (Named::Backspace, true) => A::ZoomReset,
            (Named::Enter, false) => A::Fullscreen,
            (Named::Escape, _) => A::LeaveFullscreen,
            (Named::PageUp, false) => A::Chapter(1),
            (Named::PageDown, false) => A::Chapter(-1),
            _ => return None,
        }),
        Key::Character(c) => {
            let c = c.as_str();
            if alt {
                return match c {
                    "+" | "=" => Some(A::Zoom(0.1)),
                    "-" => Some(A::Zoom(-0.1)),
                    _ => None,
                };
            }
            // letters arrive lower-case with Shift on some setups
            let up = shift
                && c.len() == 1
                && c.chars()
                    .all(|ch| ch.is_ascii_lowercase() || "3,.".contains(ch));
            Some(match (c, up) {
                ("p", false) | (" ", _) => A::TogglePause,
                ("[", _) => A::SpeedMul(0.9),
                ("]", _) => A::SpeedMul(1.1),
                ("{", _) => A::SpeedMul(0.5),
                ("}", _) => A::SpeedMul(2.0),
                ("m", false) => A::Mute,
                ("9" | "/", false) => A::Volume(-2.0),
                ("0" | "*", false) => A::Volume(2.0),
                ("f", false) => A::Fullscreen,
                (",", false) => A::FrameBack,
                (".", false) => A::FrameStep,
                ("<", _) | (",", true) => A::File(-1),
                (">", _) | (".", true) => A::File(1),
                ("v", false) => A::SubToggle,
                ("j" | "J", _) => A::SubCycle,
                ("#", _) | ("3", true) => A::AudioCycle,
                ("s", false) => A::Screenshot(true),
                ("S", _) | ("s", true) => A::Screenshot(false),
                ("l", false) => A::AbLoop,
                ("L", _) | ("l", true) => A::LoopFile,
                ("i" | "I", _) => A::Stats,
                ("z", false) => A::SubDelay(-0.1),
                ("Z", _) | ("z", true) | ("x", false) => A::SubDelay(0.1),
                ("A", _) | ("a", true) => A::Aspect,
                ("e", false) => A::Edge,
                _ => return None,
            })
        }
        _ => None,
    }
}

impl Action {
    fn msg(self) -> Msg {
        use Action as A;
        match self {
            A::TogglePause => Msg::TogglePlay,
            A::Seek(d, exact) => Msg::SeekBy(d, exact),
            A::SpeedMul(m) => Msg::SpeedMul(m),
            A::SpeedReset => Msg::Speed(1.0),
            A::Mute => Msg::ToggleMute,
            A::Volume(d) => Msg::VolumeBy(d),
            A::Fullscreen | A::LeaveFullscreen => Msg::ToggleFullscreen,
            A::FrameStep => Msg::FrameStep,
            A::FrameBack => Msg::FrameBack,
            A::SubToggle => Msg::ToggleSubs,
            A::SubCycle => Msg::CycleSub,
            A::AudioCycle => Msg::CycleAudio,
            A::Screenshot(subs) => Msg::Screenshot(subs),
            A::AbLoop => Msg::AbLoop,
            A::LoopFile => Msg::LoopFile,
            A::Chapter(d) => Msg::Chapter(d),
            A::File(d) => Msg::File(d),
            A::Stats => Msg::ToggleStats,
            A::SubDelay(d) => Msg::SubDelay(d),
            A::AudioDelay(d) => Msg::AudioDelay(d),
            A::Aspect => Msg::CycleAspect,
            A::Zoom(d) => Msg::Zoom(d),
            A::Pan(x, y) => Msg::PanBy(x, y),
            A::ZoomReset => Msg::ZoomReset,
            A::Edge => Msg::ToggleEdge,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════ state

/// What mpv last told us.
#[derive(Debug, Clone)]
struct Live {
    pos: f64,
    duration: f64,
    paused: bool,
    eof: bool,
    volume: f64,
    muted: bool,
    speed: f64,
    tracks: Vec<Track>,
    chapters: Vec<f64>,
    chapter: i64,
    hwdec: String,
    width: i64,
    height: i64,
    fps: f64,
    est_fps: f64,
    dropped: i64,
    dec_dropped: i64,
    sub_delay: f64,
    audio_delay: f64,
    ab: (Option<f64>, Option<f64>),
    loop_file: bool,
    sub_visible: bool,
    /// user rotation (clockwise degrees, multiple of 90)
    rotate: i64,
    /// rotation from the file's metadata (applied by us, see options)
    meta_rotate: i64,
    zoom: f64,
    cache_end: Option<f64>,
    loaded: bool,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            pos: 0.0,
            duration: 0.0,
            paused: true,
            eof: false,
            volume: 100.0,
            muted: false,
            speed: 1.0,
            tracks: vec![],
            chapters: vec![],
            chapter: -1,
            hwdec: String::new(),
            width: 0,
            height: 0,
            fps: 0.0,
            est_fps: 0.0,
            dropped: 0,
            dec_dropped: 0,
            sub_delay: 0.0,
            audio_delay: 0.0,
            ab: (None, None),
            loop_file: false,
            sub_visible: true,
            rotate: 0,
            meta_rotate: 0,
            zoom: 0.0,
            cache_end: None,
            loaded: false,
        }
    }
}

impl Live {
    fn apply(&mut self, name: &str, v: &Node) {
        match name {
            "time-pos" => self.pos = v.f64().unwrap_or(self.pos),
            "duration" => self.duration = v.f64().unwrap_or(0.0),
            "pause" => self.paused = v.flag().unwrap_or(true),
            "eof-reached" => self.eof = v.flag().unwrap_or(false),
            "volume" => self.volume = v.f64().unwrap_or(self.volume),
            "mute" => self.muted = v.flag().unwrap_or(false),
            "speed" => self.speed = v.f64().unwrap_or(1.0),
            "track-list" => self.tracks = parse_tracks(v),
            "chapter-list" => self.chapters = parse_chapters(v),
            "chapter" => self.chapter = v.i64().unwrap_or(-1),
            "hwdec-current" => self.hwdec = v.str().unwrap_or("").to_string(),
            "video-params" => {
                self.width = v.get("w").and_then(Node::i64).unwrap_or(0);
                self.height = v.get("h").and_then(Node::i64).unwrap_or(0);
            }
            "container-fps" => self.fps = v.f64().unwrap_or(0.0),
            "estimated-vf-fps" => self.est_fps = v.f64().unwrap_or(0.0),
            "frame-drop-count" => self.dropped = v.i64().unwrap_or(0),
            "decoder-frame-drop-count" => self.dec_dropped = v.i64().unwrap_or(0),
            "sub-delay" => self.sub_delay = v.f64().unwrap_or(0.0),
            "audio-delay" => self.audio_delay = v.f64().unwrap_or(0.0),
            // "no" doesn't parse as a number → None
            "ab-loop-a" => self.ab.0 = v.f64(),
            "ab-loop-b" => self.ab.1 = v.f64(),
            "loop-file" => {
                self.loop_file = !matches!(v, Node::None | Node::Flag(false))
                    && v.str() != Some("no")
                    && v.i64() != Some(0)
            }
            "sub-visibility" => self.sub_visible = v.flag().unwrap_or(true),
            "video-dec-params" => {
                self.meta_rotate = v.get("rotate").and_then(Node::i64).unwrap_or(0)
            }
            "video-zoom" => self.zoom = v.f64().unwrap_or(0.0),
            "demuxer-cache-time" => self.cache_end = v.f64(),
            _ => {}
        }
    }

    /// Total clockwise rotation shown on screen: 0, 90, 180 or 270.
    fn display_rotate(&self) -> u32 {
        quarter_turns(self.meta_rotate + self.rotate) * 90
    }

    fn track(&self, kind: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == kind && t.selected)
    }

    /// "H264 · 1920×1080 · 30 fps · hw: vaapi · AAC 2ch"
    fn summary(&self) -> Option<String> {
        if !self.loaded {
            return None;
        }
        let mut parts = vec![];
        match self.track("video").filter(|t| !t.image) {
            Some(v) => {
                if let Some(c) = &v.codec {
                    parts.push(c.to_ascii_uppercase());
                }
                if self.width > 0 {
                    parts.push(format!("{}×{}", self.width, self.height));
                }
                if self.fps > 0.0 {
                    parts.push(format!("{} fps", trim_num(self.fps)));
                }
                parts.push(match self.hwdec.as_str() {
                    "" | "no" => "hw: no (software decoding)".to_string(),
                    h => format!("hw: {h}"),
                });
            }
            None => parts.push("no video".into()),
        }
        match self.track("audio") {
            Some(a) => {
                let mut s = a.codec.clone().unwrap_or_default().to_ascii_uppercase();
                if let Some(r) = a.samplerate {
                    s += &format!(" {} kHz", trim_num(r as f64 / 1000.0));
                }
                if let Some(c) = a.channels {
                    s += &format!(" {c}ch");
                }
                parts.push(s);
            }
            None => parts.push("no audio".into()),
        }
        Some(parts.join(" · "))
    }

    fn stats(&self, render: (u32, u32)) -> String {
        let mut s = format!(
            "{}×{} → {}×{} · {} fps",
            self.width,
            self.height,
            render.0,
            render.1,
            trim_num(self.fps)
        );
        if self.est_fps > 0.0 {
            s += &format!(" (est {:.2})", self.est_fps);
        }
        if let Some(c) = self.track("video").and_then(|t| t.codec.as_deref()) {
            s += &format!(" · {c}");
        }
        s += &format!(
            " · hwdec {}",
            if self.hwdec.is_empty() {
                "no"
            } else {
                &self.hwdec
            }
        );
        s += &format!(" · dropped {} (decoder {})", self.dropped, self.dec_dropped);
        if let Some(c) = self.cache_end {
            s += &format!(" · cache {:.1} s", (c - self.pos).max(0.0));
        }
        s += &format!(
            " · speed {}× · A {:+.1} s · S {:+.1} s",
            trim_num(self.speed),
            self.audio_delay,
            self.sub_delay
        );
        s
    }
}

/// Degrees (any sign) → quarter turns 0..=3 (nearest).
fn quarter_turns(deg: i64) -> u32 {
    ((deg as f64 / 90.0).round() as i64).rem_euclid(4) as u32
}

/// Rotate a saved screenshot like the picture on screen (mpv saves it
/// unrotated because rotation happens in our shader).
fn rotate_image_file(path: &Path, deg: u32) -> Result<(), String> {
    if deg == 0 {
        return Ok(());
    }
    let im = image::open(path).map_err(|e| e.to_string())?;
    let im = match deg {
        90 => im.rotate90(),
        180 => im.rotate180(),
        _ => im.rotate270(),
    };
    im.save(path).map_err(|e| e.to_string())
}

/// 30 → "30", 29.97 → "29.97", 23.976… → "23.976"
fn trim_num(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[derive(Debug, Clone)]
pub enum Msg {
    Ev(UiEv),
    TogglePlay,
    SeekDrag(f64),
    SeekRelease,
    SeekBy(f64, bool),
    FrameStep,
    FrameBack,
    Volume(f64),
    VolumeBy(f64),
    ToggleMute,
    Speed(f64),
    SpeedMul(f64),
    ToggleTimeMode,
    Audio(TrackChoice),
    Sub(TrackChoice),
    ToggleSubs,
    CycleSub,
    CycleAudio,
    SubDelay(f64),
    AudioDelay(f64),
    AbLoop,
    LoopFile,
    CycleAspect,
    Rotate,
    Zoom(f64),
    /// drag on the picture (fractions of the widget)
    Pan(f64, f64),
    PanBy(f64, f64),
    ZoomReset,
    /// wheel lines, Ctrl held
    Wheel(f32, bool),
    Screenshot(bool),
    ScreenshotDone(Result<String, String>),
    Chapter(i64),
    File(i64),
    ToggleStats,
    ToggleEdge,
    ToggleFullscreen,
    Motion,
    ToggleEdit,
    SetIn,
    SetOut,
    ClearMarks,
    Export,
    Exporting(ExportEv),
    Snapshot,
    SnapshotDone(Result<PathBuf, String>),
    /// the FFmpeg fallback player
    Ff(ff::Msg),
}

pub struct State {
    inner: Inner,
    fullscreen: bool,
}

enum Inner {
    Mpv(Box<MpvState>),
    Ff(ff::State),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct MpvState {
    id: u64,
    player: Player,
    path: PathBuf,
    info: Info,
    playlist: Vec<PathBuf>,
    index: usize,
    live: Live,
    drag: Option<f64>,
    remaining: bool,
    stats: bool,
    aspect_ix: usize,
    /// hairline outline at the picture edge (key `e`)
    edge: bool,
    /// "mpv 0.41.0" for the stats line
    mpv_version: String,
    last_motion: Instant,
    error: Option<String>,
    /// `start` was set for a resume; reset it once the file is loaded
    start_pending: bool,
    /// watch-later store (None in tests)
    resume_file: Option<PathBuf>,
    editing: bool,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    export: Option<f32>,
    snapshotting: bool,
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        Self::with_store(o, resume_store())
    }

    fn with_store(o: Opened, store: Option<PathBuf>) -> (Self, Task<Msg>) {
        if backend() == Backend::Mpv {
            match MpvState::new(&o, store) {
                Ok((s, t)) => {
                    return (
                        State {
                            inner: Inner::Mpv(Box::new(s)),
                            fullscreen: false,
                        },
                        t,
                    )
                }
                Err(e) => eprintln!("og-files: libmpv unusable ({e}); using the FFmpeg player"),
            }
        }
        let (s, t) = ff::State::new(o.path, o.info);
        (
            State {
                inner: Inner::Ff(s),
                fullscreen: false,
            },
            t.map(Msg::Ff),
        )
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        if let Msg::ToggleFullscreen = msg {
            self.fullscreen = !self.fullscreen;
            if let Inner::Mpv(m) = &mut self.inner {
                m.last_motion = Instant::now();
            }
            return (Task::none(), None);
        }
        match (&mut self.inner, msg) {
            (Inner::Ff(s), Msg::Ff(m)) => {
                let (t, e) = s.update(m);
                (t.map(Msg::Ff), e)
            }
            (Inner::Mpv(s), m) => s.update(m),
            _ => (Task::none(), None),
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        match &self.inner {
            Inner::Mpv(s) => s.view(self.fullscreen),
            Inner::Ff(s) => s.view().map(Msg::Ff),
        }
    }

    /// Keyboard input while the viewer has focus. Return None if not
    /// consumed (the app handles it), Some(task) if consumed. mpv defaults:
    /// Space/p pause, Left/Right ±5 s, Up/Down ±60 s (Shift: exact ±1 s /
    /// ±5 s), [ ] speed ×0.9/×1.1, Backspace speed 1, m mute, 9/0 volume,
    /// f or Enter fullscreen toggle, Escape leave fullscreen, , . frame
    /// back/step, v sub toggle, j sub track cycle, # audio track cycle,
    /// s screenshot, l A-B loop, L loop file, PgUp/PgDn chapter, < > previous
    /// / next file in folder, i stats.
    pub fn key(&mut self, key: &Key, mods: Modifiers) -> Option<Task<Msg>> {
        let a = key_action(key, mods)?;
        if a == Action::LeaveFullscreen && !self.fullscreen {
            return None;
        }
        if let Inner::Ff(_) = self.inner {
            // the fallback player only knows a few of them
            let m = match a {
                Action::TogglePause => Msg::Ff(ff::Msg::TogglePlay),
                Action::FrameStep => Msg::Ff(ff::Msg::Step),
                Action::Mute => Msg::Ff(ff::Msg::ToggleMute),
                Action::Fullscreen | Action::LeaveFullscreen => Msg::ToggleFullscreen,
                _ => return None,
            };
            return Some(self.update(m).0);
        }
        Some(self.update(a.msg()).0)
    }

    /// Whether the viewer currently wants the window fullscreen (toggled by
    /// f / double-click on the video / a button).
    pub fn fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// The app left fullscreen by itself (e.g. Escape handled globally).
    pub fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        match &self.inner {
            Inner::Mpv(s) => Some(s.editing),
            Inner::Ff(s) => s.is_editing(),
        }
    }

    /// Ctrl+E. Never refused: edits only ever produce new files.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        match &mut self.inner {
            Inner::Mpv(s) => {
                s.editing = !s.editing;
                Task::none()
            }
            Inner::Ff(s) => s.toggle_edit().map(Msg::Ff),
        }
    }

    /// Ctrl+S: nothing to save — video is never written over.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}

/// Options for a real viewer: tests never open an audio device.
fn viewer_options(path: &Path) -> Vec<(String, String)> {
    let mute = std::env::var_os("OGX_MUTE").is_some_and(|v| v != "0");
    let mut o = mpv_options(path, mute);
    // debugging aid: start playing instead of paused
    if std::env::var_os("OGX_AUTOPLAY").is_some_and(|v| v != "0") {
        o.push(("pause".into(), "no".into()));
    }
    if cfg!(test) {
        o.retain(|(k, _)| k != "ao");
        o.push(("ao".into(), "null".into()));
    }
    o
}

impl MpvState {
    fn new(o: &Opened, store: Option<PathBuf>) -> Result<(MpvState, Task<Msg>), String> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let player = Player::new(&viewer_options(&o.path), Some(tx))?;
        let index = o.playlist.iter().position(|p| *p == o.path).unwrap_or(0);
        let mut s = MpvState {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            player,
            path: o.path.clone(),
            info: o.info.clone(),
            playlist: if o.playlist.is_empty() {
                vec![o.path.clone()]
            } else {
                o.playlist.clone()
            },
            index,
            live: Live::default(),
            drag: None,
            remaining: false,
            stats: false,
            aspect_ix: 0,
            edge: true,
            mpv_version: String::new(),
            last_motion: Instant::now(),
            error: None,
            start_pending: false,
            resume_file: store,
            editing: false,
            mark_in: None,
            mark_out: None,
            export: None,
            snapshotting: false,
        };
        s.mpv_version = s.player.mpv.get_str("mpv-version").unwrap_or_default();
        s.load(o.path.clone());
        let stream = iced::stream::channel(32, move |mut out| async move {
            while let Some(e) = rx.recv().await {
                if out.send(e).await.is_err() {
                    break;
                }
            }
        });
        Ok((s, Task::run(stream, Msg::Ev)))
    }

    /// Load `path` into mpv (resuming a remembered position).
    fn load(&mut self, path: PathBuf) {
        let m = &self.player.mpv;
        let dir = path.parent().unwrap_or(Path::new("."));
        let _ = m.set_str("screenshot-directory", &dir.to_string_lossy());
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .replace('%', "%%");
        let _ = m.set_str("screenshot-template", &format!("{stem}-frame-%wH%wM%wS"));
        let resume = self
            .resume_file
            .as_deref()
            .and_then(|f| lookup_position(f, &path));
        self.start_pending = resume.is_some();
        if let Some(p) = resume {
            let _ = m.set_str("start", &format!("{p:.3}"));
        }
        self.player
            .cmd(&["loadfile", &path.to_string_lossy(), "replace"]);
        self.path = path;
    }

    fn save_resume(&self) {
        if let Some(f) = &self.resume_file {
            if self.live.loaded {
                // ask mpv: the UI's copy of time-pos is throttled
                let m = &self.player.mpv;
                let pos = m.get_f64("time-pos").unwrap_or(self.live.pos);
                let dur = m.get_f64("duration").unwrap_or(self.live.duration);
                record_position(f, &self.path, pos, dur);
            }
        }
    }

    fn duration(&self) -> f64 {
        if self.live.duration > 0.0 {
            self.live.duration
        } else {
            self.info.duration
        }
    }

    fn set(&self, k: &str, v: impl ToString) {
        self.player.cmd(&["set", k, &v.to_string()]);
    }

    fn switch_file(&mut self, d: i64) -> Option<Event> {
        let n = self.playlist.len();
        if n < 2 {
            return None;
        }
        self.save_resume();
        self.index = step_index(self.index, d, n);
        let next = self.playlist[self.index].clone();
        let keep = Live {
            volume: self.live.volume,
            muted: self.live.muted,
            speed: self.live.speed,
            paused: self.live.paused,
            ..Live::default()
        };
        self.live = keep;
        self.info = Info::default();
        self.mark_in = None;
        self.mark_out = None;
        self.error = None;
        self.drag = None;
        self.load(next);
        let name = self.path.file_name().unwrap_or_default().to_string_lossy();
        Some(Event::Status(format!("{name} ({} / {n})", self.index + 1)))
    }

    fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Ev(ev) => return self.event(ev),
            Msg::TogglePlay => {
                if self.live.eof
                    || (self.duration() > 0.0 && self.live.pos >= self.duration() - 0.05)
                {
                    self.player.cmd(&["seek", "0", "absolute"]);
                    self.set("pause", "no");
                } else {
                    self.player.cmd(&["cycle", "pause"]);
                }
            }
            Msg::SeekDrag(t) => {
                self.drag = Some(t);
                self.live.pos = t;
                self.player
                    .cmd(&["seek", &format!("{t:.3}"), "absolute+keyframes"]);
            }
            Msg::SeekRelease => {
                if let Some(t) = self.drag.take() {
                    self.player
                        .cmd(&["seek", &format!("{t:.3}"), "absolute+exact"]);
                }
            }
            Msg::SeekBy(d, exact) => {
                let flags = if exact { "relative+exact" } else { "relative" };
                self.player.cmd(&["seek", &format!("{d}"), flags]);
            }
            Msg::FrameStep => self.player.cmd(&["frame-step"]),
            Msg::FrameBack => self.player.cmd(&["frame-back-step"]),
            Msg::Volume(v) => {
                self.live.volume = v.clamp(0.0, 130.0);
                self.set("volume", format!("{:.0}", self.live.volume));
            }
            Msg::VolumeBy(d) => return self.update(Msg::Volume(self.live.volume + d)),
            Msg::ToggleMute => self.player.cmd(&["cycle", "mute"]),
            Msg::Speed(v) => {
                self.live.speed = v.clamp(0.25, 4.0);
                self.set("speed", format!("{:.4}", self.live.speed));
            }
            Msg::SpeedMul(m) => return self.update(Msg::Speed(self.live.speed * m)),
            Msg::ToggleTimeMode => self.remaining = !self.remaining,
            Msg::Audio(c) => self.set("aid", c.id.map_or("no".into(), |i| i.to_string())),
            Msg::Sub(c) => self.set("sid", c.id.map_or("no".into(), |i| i.to_string())),
            Msg::ToggleSubs => self.player.cmd(&["cycle", "sub-visibility"]),
            Msg::CycleSub => self.player.cmd(&["cycle", "sub"]),
            Msg::CycleAudio => self.player.cmd(&["cycle", "audio"]),
            Msg::SubDelay(d) => self.player.cmd(&["add", "sub-delay", &format!("{d}")]),
            Msg::AudioDelay(d) => self.player.cmd(&["add", "audio-delay", &format!("{d}")]),
            Msg::AbLoop => self.player.cmd(&["ab-loop"]),
            Msg::LoopFile => self.player.cmd(&["cycle-values", "loop-file", "inf", "no"]),
            Msg::CycleAspect => {
                self.aspect_ix = (self.aspect_ix + 1) % ASPECTS.len();
                self.set("video-aspect-override", ASPECTS[self.aspect_ix]);
            }
            Msg::Rotate => self.live.rotate = (self.live.rotate + 90).rem_euclid(360),
            Msg::Zoom(d) => self.player.cmd(&["add", "video-zoom", &format!("{d}")]),
            Msg::Pan(dx, dy) => {
                // only meaningful when zoomed in; pan is in video widths
                if self.live.zoom > 0.0 {
                    let s = 2f64.powf(self.live.zoom);
                    return self.update(Msg::PanBy(dx / s, dy / s));
                }
            }
            Msg::PanBy(dx, dy) => {
                self.player
                    .cmd(&["add", "video-pan-x", &format!("{dx:.4}")]);
                self.player
                    .cmd(&["add", "video-pan-y", &format!("{dy:.4}")]);
            }
            Msg::ZoomReset => {
                self.set("video-zoom", 0);
                self.set("video-pan-x", 0);
                self.set("video-pan-y", 0);
            }
            Msg::Wheel(y, ctrl) => {
                return if ctrl {
                    self.update(Msg::Zoom(0.1 * y as f64))
                } else {
                    self.update(Msg::VolumeBy(2.0 * y as f64))
                };
            }
            Msg::Screenshot(subs) => {
                let mpv = self.player.mpv.clone();
                let flag = if subs { "subtitles" } else { "video" };
                let rot = self.live.display_rotate();
                let fut = async move {
                    tokio::task::spawn_blocking(move || {
                        let f = mpv.command_ret(&["screenshot", flag]).map(|n| {
                            n.get("filename")
                                .and_then(Node::str)
                                .unwrap_or("")
                                .to_string()
                        })?;
                        if !f.is_empty() {
                            rotate_image_file(Path::new(&f), rot)?;
                        }
                        Ok(f)
                    })
                    .await
                    .map_err(|e| e.to_string())?
                };
                return (Task::perform(fut, Msg::ScreenshotDone), None);
            }
            Msg::ScreenshotDone(r) => {
                let s = match r {
                    Ok(f) if f.is_empty() => "Screenshot saved".to_string(),
                    Ok(f) => format!("Screenshot → {f}"),
                    Err(e) => format!("Screenshot failed: {e}"),
                };
                return (Task::none(), Some(Event::Status(s)));
            }
            Msg::Chapter(d) => {
                if !self.live.chapters.is_empty() {
                    self.player.cmd(&["add", "chapter", &d.to_string()]);
                }
            }
            Msg::File(d) => return (Task::none(), self.switch_file(d)),
            Msg::ToggleStats => self.stats = !self.stats,
            Msg::ToggleEdge => self.edge = !self.edge,
            Msg::ToggleFullscreen => {} // handled by State
            Msg::Motion => self.last_motion = Instant::now(),
            Msg::ToggleEdit => self.editing = !self.editing,
            Msg::SetIn => {
                let p = self.live.pos;
                self.mark_in = Some(p);
                if self.mark_out.is_some_and(|o| o <= p) {
                    self.mark_out = None;
                }
            }
            Msg::SetOut => {
                let p = self.live.pos;
                self.mark_out = Some(p);
                if self.mark_in.is_some_and(|i| i >= p) {
                    self.mark_in = None;
                }
            }
            Msg::ClearMarks => {
                self.mark_in = None;
                self.mark_out = None;
            }
            Msg::Export => {
                if self.export.is_some() {
                    return none;
                }
                let tin = self.mark_in.unwrap_or(0.0);
                let tout = self.mark_out.unwrap_or(self.duration());
                if tout <= tin {
                    return (
                        Task::none(),
                        Some(Event::Status("Trim: Out must be after In".into())),
                    );
                }
                self.export = Some(0.0);
                return (
                    ff::spawn_export(self.path.clone(), tin, tout).map(Msg::Exporting),
                    None,
                );
            }
            Msg::Exporting(ExportEv::Progress(p)) => self.export = Some(p),
            Msg::Exporting(ExportEv::Done(r)) => {
                self.export = None;
                let s = match r {
                    Ok(p) => format!("Exported trim → {}", p.display()),
                    Err(e) => format!("Trim export failed: {e}"),
                };
                return (Task::none(), Some(Event::Status(s)));
            }
            Msg::Snapshot => {
                if self.snapshotting {
                    return none;
                }
                self.snapshotting = true;
                let dir = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let stem = self.path.file_stem().unwrap_or_default().to_string_lossy();
                let s = self.live.pos.max(0.0) as u64;
                let name = format!("{stem}-frame-{:02}m{:02}s.png", s / 60, s % 60);
                let dst = crate::ops::unique_name(&dir, &name);
                let mpv = self.player.mpv.clone();
                let rot = self.live.display_rotate();
                let fut = async move {
                    tokio::task::spawn_blocking(move || {
                        mpv.command(&["screenshot-to-file", &dst.to_string_lossy(), "video"])?;
                        rotate_image_file(&dst, rot)?;
                        Ok(dst)
                    })
                    .await
                    .map_err(|e| e.to_string())?
                };
                return (Task::perform(fut, Msg::SnapshotDone), None);
            }
            Msg::SnapshotDone(r) => {
                self.snapshotting = false;
                let s = match r {
                    Ok(p) => format!("Saved frame → {}", p.display()),
                    Err(e) => format!("Snapshot failed: {e}"),
                };
                return (Task::none(), Some(Event::Status(s)));
            }
            Msg::Ff(_) => {}
        }
        none
    }

    fn event(&mut self, ev: UiEv) -> (Task<Msg>, Option<Event>) {
        match ev {
            UiEv::Prop(name, v) => {
                if name == "time-pos" && self.drag.is_some() {
                    return (Task::none(), None);
                }
                self.live.apply(name, &v);
            }
            UiEv::FileLoaded => {
                self.live.loaded = true;
                self.error = None;
                if self.start_pending {
                    self.start_pending = false;
                    let _ = self.player.mpv.set_str("start", "none");
                }
            }
            UiEv::EndFile(Some(e)) => {
                let msg = format!("Can't play {}: {e}", self.path.display());
                self.error = Some(e);
                return (Task::none(), Some(Event::Status(msg)));
            }
            UiEv::EndFile(None) => {}
            UiEv::Frame => self.player.shared.frame_seen(),
            UiEv::Shutdown => {}
        }
        (Task::none(), None)
    }

    fn view(&self, fullscreen: bool) -> Element<'_, Msg> {
        let l = &self.live;
        let playing = !l.paused && !l.eof;
        let screen = shader(screen::Screen {
            id: self.id,
            shared: self.player.shared.clone(),
            playing,
            rotate: l.display_rotate(),
            edge: self.edge.then(|| theme::MUTED.scale_alpha(0.3)),
            track_motion: fullscreen,
        })
        .width(Length::Fill)
        .height(Length::Fill);
        let overlay: Element<'_, Msg> = match (&self.error, l.loaded) {
            (Some(e), _) => text(format!("Can't play this video: {e}"))
                .size(13)
                .color(theme::MUTED)
                .into(),
            (None, false) => text("Loading…").size(13).color(theme::MUTED).into(),
            _ => Space::new(0, 0).into(),
        };
        // no black box: the app background shows around the picture
        let picture = container(stack![screen, container(overlay).center(Length::Fill)])
            .width(Length::Fill)
            .height(Length::Fill);

        let show_controls = !fullscreen || !playing || self.last_motion.elapsed() < HIDE_AFTER;
        if !show_controls {
            return picture.into();
        }

        let dur = self.duration();
        let pos = self.drag.unwrap_or(l.pos).clamp(0.0, dur.max(0.0));
        let seek = canvas(seekbar::SeekBar {
            duration: dur,
            pos,
            cache_end: l.cache_end,
            chapters: l.chapters.clone(),
            ab: l.ab,
            marks: if self.editing {
                (self.mark_in, self.mark_out)
            } else {
                (None, None)
            },
        })
        .width(Length::Fill)
        .height(18);

        let ibtn = |b: B, msg: Option<Msg>| {
            button(icon(b).size(14))
                .padding([4, 8])
                .on_press_maybe(msg)
                .style(button::secondary)
        };
        let tbtn = |s: String, msg: Msg, on: bool| {
            button(text(s).size(12).wrapping(text::Wrapping::None))
                .padding([4, 8])
                .on_press(msg)
                .style(if on {
                    button::primary
                } else {
                    button::secondary
                })
        };
        let time = if self.remaining {
            format!("-{} / {}", fmt_time(dur - pos), fmt_time(dur))
        } else {
            format!("{} / {}", fmt_time(pos), fmt_time(dur))
        };
        let mut main = row![
            ibtn(
                if playing { B::PauseFill } else { B::PlayFill },
                Some(Msg::TogglePlay)
            ),
            ibtn(B::SkipStartFill, Some(Msg::FrameBack)),
            ibtn(B::SkipEndFill, Some(Msg::FrameStep)),
            button(text(time).size(12))
                .padding([4, 6])
                .on_press(Msg::ToggleTimeMode)
                .style(button::text),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if !l.chapters.is_empty() {
            main = main
                .push(ibtn(B::SkipBackwardFill, Some(Msg::Chapter(-1))))
                .push(
                    text(format!("ch {}/{}", l.chapter + 1, l.chapters.len()))
                        .size(12)
                        .color(theme::MUTED),
                )
                .push(ibtn(B::SkipForwardFill, Some(Msg::Chapter(1))));
        }
        if self.playlist.len() > 1 {
            main = main
                .push(ibtn(B::ChevronBarLeft, Some(Msg::File(-1))))
                .push(
                    text(format!("{} / {}", self.index + 1, self.playlist.len()))
                        .size(12)
                        .color(theme::MUTED),
                )
                .push(ibtn(B::ChevronBarRight, Some(Msg::File(1))));
        }
        main = main
            .push(tbtn("−".into(), Msg::SpeedMul(0.9), false))
            .push(tbtn(
                format!("{}×", trim_num((l.speed * 100.0).round() / 100.0)),
                Msg::Speed(1.0),
                (l.speed - 1.0).abs() > 1e-3,
            ))
            .push(tbtn("+".into(), Msg::SpeedMul(1.1), false))
            .push(ibtn(
                if l.muted || l.volume == 0.0 {
                    B::VolumeMuteFill
                } else {
                    B::VolumeUpFill
                },
                Some(Msg::ToggleMute),
            ))
            .push(
                slider(0.0..=130.0, l.volume, Msg::Volume)
                    .step(1.0)
                    .width(100),
            )
            .push(text(format!("{:.0}", l.volume)).size(12).width(26))
            .push(ibtn(
                if fullscreen {
                    B::FullscreenExit
                } else {
                    B::Fullscreen
                },
                Some(Msg::ToggleFullscreen),
            ))
            .push(if self.editing {
                button(text("Done").size(12))
                    .padding([4, 8])
                    .on_press(Msg::ToggleEdit)
                    .style(button::secondary)
            } else {
                button(text("Edit  Ctrl+E").size(12).wrapping(text::Wrapping::None))
                    .padding([4, 8])
                    .on_press(Msg::ToggleEdit)
            });

        let mut col = column![picture, seek, main.wrap()].spacing(6);
        if fullscreen {
            return container(col).padding([0, 6]).into();
        }

        let (achoices, acur) = track_choices(&l.tracks, "audio");
        let (schoices, scur) = track_choices(&l.tracks, "sub");
        let delay = |label: &'static str, v: f64, f: fn(f64) -> Msg| {
            row![
                text(format!("{label} {v:+.1}s"))
                    .size(12)
                    .color(theme::MUTED),
                button(text("−").size(12))
                    .padding([2, 6])
                    .on_press(f(-0.1))
                    .style(button::secondary),
                button(text("+").size(12))
                    .padding([2, 6])
                    .on_press(f(0.1))
                    .style(button::secondary),
            ]
            .spacing(3)
            .align_y(Alignment::Center)
        };
        let ab = match l.ab {
            (Some(a), Some(b)) => format!("A-B {}–{}", fmt_mark(a), fmt_mark(b)),
            (Some(a), None) => format!("A {} – B?", fmt_mark(a)),
            _ => "A-B loop".into(),
        };
        let mut tools = row![].spacing(6).align_y(Alignment::Center);
        if achoices.len() > 1 {
            tools = tools.push(
                pick_list(achoices, acur, Msg::Audio)
                    .text_size(12)
                    .padding([3, 6]),
            );
        }
        if schoices.len() > 1 {
            tools = tools
                .push(
                    pick_list(schoices, scur, Msg::Sub)
                        .text_size(12)
                        .padding([3, 6]),
                )
                .push(
                    ibtn(B::BadgeCcFill, Some(Msg::ToggleSubs)).style(if l.sub_visible {
                        button::secondary
                    } else {
                        button::text
                    }),
                )
                .push(delay("sub", l.sub_delay, Msg::SubDelay));
        }
        if l.track("audio").is_some() {
            tools = tools.push(delay("audio", l.audio_delay, Msg::AudioDelay));
        }
        tools = tools
            .push(tbtn(ab, Msg::AbLoop, l.ab.0.is_some()))
            .push(
                button(icon(B::Repeat).size(14))
                    .padding([4, 8])
                    .on_press(Msg::LoopFile)
                    .style(if l.loop_file {
                        button::primary
                    } else {
                        button::secondary
                    }),
            )
            .push(tbtn(
                match ASPECTS[self.aspect_ix] {
                    "no" => "Aspect".to_string(),
                    a => a.to_string(),
                },
                Msg::CycleAspect,
                self.aspect_ix != 0,
            ))
            .push(tbtn(
                format!("Rotate {}°", l.rotate),
                Msg::Rotate,
                l.rotate != 0,
            ));
        if l.zoom != 0.0 {
            tools = tools.push(ibtn(B::ZoomOut, Some(Msg::ZoomReset)));
        }
        tools = tools
            .push(ibtn(B::Camera, Some(Msg::Screenshot(true))))
            .push(tbtn("Edge".into(), Msg::ToggleEdge, self.edge))
            .push(tbtn("stats".into(), Msg::ToggleStats, self.stats));

        let summary = l.summary().unwrap_or_else(|| self.info.summary());
        col = col
            .push(tools.wrap())
            .push(text(summary).size(12).color(theme::MUTED));
        if self.stats {
            let render = self
                .player
                .shared
                .frame
                .lock()
                .map(|f| (f.w, f.h))
                .unwrap_or((0, 0));
            col = col.push(
                text(format!("{} · {}", l.stats(render), self.mpv_version))
                    .size(12)
                    .color(theme::MUTED),
            );
        }
        if self.editing {
            let mark = |m: Option<f64>| m.map(fmt_mark).unwrap_or_else(|| "—".into());
            let busy = self.export.is_some();
            let edit = row![
                text("EDITING").size(12).color(AMBER),
                button(text(format!("In {}", mark(self.mark_in))).size(12))
                    .padding([4, 8])
                    .on_press(Msg::SetIn)
                    .style(button::secondary),
                button(text(format!("Out {}", mark(self.mark_out))).size(12))
                    .padding([4, 8])
                    .on_press(Msg::SetOut)
                    .style(button::secondary),
                button(text("Clear").size(12))
                    .padding([4, 8])
                    .on_press_maybe(
                        (self.mark_in.is_some() || self.mark_out.is_some())
                            .then_some(Msg::ClearMarks)
                    )
                    .style(button::secondary),
                Space::with_width(Length::Fill),
                button(row![icon(B::Camera).size(13), text("Snapshot frame").size(12)].spacing(4))
                    .padding([4, 8])
                    .on_press_maybe((!self.snapshotting).then_some(Msg::Snapshot))
                    .style(button::secondary),
                button(
                    row![
                        icon(B::Scissors).size(13),
                        text(match self.export {
                            Some(p) => format!("Exporting {:.0}%", p * 100.0),
                            None => "Export trim".into(),
                        })
                        .size(12)
                    ]
                    .spacing(4)
                )
                .padding([4, 8])
                .on_press_maybe((!busy && ff::CAN_EXPORT).then_some(Msg::Export)),
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            col = col.push(edit).push(
                text(if ff::CAN_EXPORT {
                    "Exports are new files (keyframe-aligned stream copy); the video is never saved over."
                } else {
                    "Snapshots are new files; the video is never saved over. Trim export needs a build with FFmpeg."
                })
                    .size(12)
                    .color(theme::MUTED),
            );
        }
        col.into()
    }
}

impl Drop for MpvState {
    fn drop(&mut self) {
        // remember where we were; the Player's Drop then stops mpv
        self.save_resume();
    }
}

#[cfg(test)]
mod tests;
