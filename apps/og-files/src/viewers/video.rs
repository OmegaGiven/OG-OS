//! Video player: mp4, mkv, webm, mov, avi… via the FFmpeg *libraries*
//! (`ffmpeg-next`; no ffmpeg CLI) with audio through `cpal`.
//!
//! One decoder thread per open video (`Player`) owns the demuxer, both
//! decoders, swscale/swresample and the audio output. It keeps a small queue
//! of RGBA frames (≤ `QUEUE`) already scaled to display size and presents
//! them against the master clock: the audio clock (samples actually handed
//! to the device) when audio plays at 1×, otherwise the wall clock. Frames
//! reach iced through an `iced::stream::channel` that only exists while
//! playing; seeks and frame steps reply with a single frame on a oneshot.
//!
//! Edit mode never touches the original: "Export trim" stream-copies the
//! packets between the keyframe at/before In and Out into
//! `<stem>-edit.<ext>`, "Snapshot frame" writes `<stem>-frame-MMmSSs.png`.
//! Both run on worker threads and report through `Event::Status`.

use super::Event;
use crate::theme;
use crate::widgets::icon;
use ff::format::sample::Type as SampleType;
use ff::format::Sample;
use ff::software::{resampling, scaling};
use ff::{codec, decoder, encoder, format, frame, media, ChannelLayout, Packet};
use ffmpeg_next as ff;
use iced::futures::SinkExt;
use iced::widget::{button, column, container, image as img, row, slider, text, Space};
use iced::{Alignment, Element, Length, Task};
use iced_fonts::Bootstrap as B;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, Once};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc as tmpsc, oneshot};

const EXTS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "mov", "avi", "wmv", "flv", "mpg", "mpeg", "ts", "ogv", "3gp",
];
/// Largest frame the player scales to (width and height).
const MAX_DISPLAY: u32 = 1920;
/// Decoded-frame queue length.
const QUEUE: usize = 8;
/// Audio kept buffered ahead of the device (seconds).
const AUDIO_AHEAD: f64 = 0.6;
const SPEEDS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];
const AMBER: iced::Color = iced::Color::from_rgb(1.0, 0.784, 0.42);

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = ff::init();
        ff::util::log::set_level(ff::util::log::Level::Fatal);
    });
}

/// Stream facts shown under the player.
#[derive(Debug, Clone, Default)]
pub struct Info {
    /// seconds
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    /// "AAC 48 kHz 2ch"
    pub audio: Option<String>,
    /// bits/s (container), 0 if unknown
    pub bit_rate: i64,
}

impl Info {
    fn summary(&self) -> String {
        let mut s = format!(
            "{} · {}×{} · {:.2} fps",
            self.video_codec, self.width, self.height, self.fps
        );
        if self.bit_rate > 0 {
            s += &format!(" · {:.1} Mb/s", self.bit_rate as f64 / 1e6);
        }
        match &self.audio {
            Some(a) => s += &format!(" · {a}"),
            None => s += " · no audio",
        }
        s
    }
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    pub info: Info,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    let p = path.clone();
    let info = tokio::task::spawn_blocking(move || Core::open(&p, (1, 1), false).map(|c| c.info))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Opened { path, info })
}

// ════════════════════════════════════════════════════════════════ decoding

fn codec_name(id: codec::Id) -> String {
    decoder::find(id)
        .map(|c| c.name().to_ascii_uppercase())
        .unwrap_or_else(|| format!("{id:?}"))
}

fn secs(ts: Option<i64>, tb: f64, start: f64) -> Option<f64> {
    ts.map(|t| t as f64 * tb - start)
}

/// Fit `w`×`h` inside `max` (never upscales).
fn fit(w: u32, h: u32, max: (u32, u32)) -> (u32, u32) {
    let s = (max.0 as f64 / w.max(1) as f64)
        .min(max.1 as f64 / h.max(1) as f64)
        .min(1.0);
    (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

struct Decoded {
    pts: f64,
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

struct Scaler {
    key: (format::Pixel, u32, u32, u32, u32),
    ctx: scaling::Context,
    out: frame::Video,
}

/// Audio output: the cpal stream plus the sample queue its callback drains.
struct Sink {
    _stream: cpal::Stream,
    buf: Arc<Mutex<AudioBuf>>,
    rate: u32,
    ch: u16,
    layout: ChannelLayout,
}

struct AudioBuf {
    /// interleaved f32 at the device rate/channels
    q: VecDeque<f32>,
    /// presentation time of `q.front()` — the audio clock
    front_pts: f64,
    rate: f64,
    ch: usize,
    running: bool,
}

impl AudioBuf {
    fn buffered(&self) -> f64 {
        self.q.len() as f64 / self.ch as f64 / self.rate
    }
    /// Drop queued samples older than `t`.
    fn trim_to(&mut self, t: f64) {
        if t > self.front_pts {
            let n = (((t - self.front_pts) * self.rate) as usize * self.ch).min(self.q.len());
            self.q.drain(..n);
            self.front_pts += (n / self.ch) as f64 / self.rate;
        }
    }
    fn clear(&mut self, t: f64) {
        self.q.clear();
        self.front_pts = t;
    }
}

/// Volume / mute, set by the UI, read by the audio callback.
struct AudioCtl {
    volume: AtomicU32,
    muted: AtomicBool,
}

impl AudioCtl {
    fn gain(&self) -> f32 {
        if self.muted.load(Ordering::Relaxed) {
            0.0
        } else {
            f32::from_bits(self.volume.load(Ordering::Relaxed))
        }
    }
}

fn open_device(ctl: Arc<AudioCtl>) -> Option<Sink> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat as F;
    let dev = cpal::default_host().default_output_device()?;
    let sup = dev.default_output_config().ok()?;
    let fmt = sup.sample_format();
    let cfg: cpal::StreamConfig = sup.into();
    let (rate, ch) = (cfg.sample_rate, cfg.channels);
    let buf = Arc::new(Mutex::new(AudioBuf {
        q: VecDeque::new(),
        front_pts: 0.0,
        rate: rate as f64,
        ch: ch as usize,
        running: false,
    }));
    let b = buf.clone();
    let stream = match fmt {
        F::F32 => build::<f32>(&dev, cfg, b, ctl),
        F::F64 => build::<f64>(&dev, cfg, b, ctl),
        F::I16 => build::<i16>(&dev, cfg, b, ctl),
        F::I32 => build::<i32>(&dev, cfg, b, ctl),
        F::U16 => build::<u16>(&dev, cfg, b, ctl),
        F::U8 => build::<u8>(&dev, cfg, b, ctl),
        F::I8 => build::<i8>(&dev, cfg, b, ctl),
        F::U32 => build::<u32>(&dev, cfg, b, ctl),
        _ => None,
    }?;
    stream.play().ok()?;
    Some(Sink {
        _stream: stream,
        buf,
        rate,
        ch,
        layout: ChannelLayout::default(ch as i32),
    })
}

fn build<T: cpal::SizedSample + cpal::FromSample<f32>>(
    dev: &cpal::Device,
    cfg: cpal::StreamConfig,
    buf: Arc<Mutex<AudioBuf>>,
    ctl: Arc<AudioCtl>,
) -> Option<cpal::Stream> {
    use cpal::traits::DeviceTrait;
    dev.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            let silence = <T as cpal::Sample>::EQUILIBRIUM;
            let Ok(mut b) = buf.lock() else {
                data.fill(silence);
                return;
            };
            if !b.running {
                data.fill(silence);
                return;
            }
            let g = ctl.gain();
            let mut n = 0;
            for s in data.iter_mut() {
                *s = match b.q.pop_front() {
                    Some(x) => {
                        n += 1;
                        T::from_sample(x * g)
                    }
                    None => silence,
                };
            }
            let adv = (n / b.ch) as f64 / b.rate;
            b.front_pts += adv;
        },
        |_e: cpal::Error| {},
        None,
    )
    .ok()
}

struct AudioTrack {
    idx: usize,
    tb: f64,
    dec: decoder::Audio,
    rs: Option<((Sample, u16, u32), resampling::Context)>,
    sink: Option<Sink>,
    /// samples before this time are dropped (exact seek)
    skip_before: f64,
    /// false while playing at ≠1× (audio muted then)
    enabled: bool,
}

impl AudioTrack {
    fn active(&self) -> bool {
        self.enabled && self.sink.is_some()
    }

    fn push(&mut self, f: &mut frame::Audio, start: f64) {
        let Some(sink) = &self.sink else { return };
        if f.samples() == 0 || f.rate() == 0 {
            return;
        }
        // unspecified / custom channel orders: assume the default layout
        let l = f.channel_layout();
        if l.0.order != ff::ffi::AVChannelOrder::AV_CHANNEL_ORDER_NATIVE
            || l.channels() != f.channels() as i32
        {
            f.set_channel_layout(ChannelLayout::default(f.channels() as i32));
        }
        let key = (f.format(), f.channels(), f.rate());
        if self.rs.as_ref().map(|r| r.0) != Some(key) {
            self.rs = resampling::Context::get(
                f.format(),
                f.channel_layout(),
                f.rate(),
                Sample::F32(SampleType::Packed),
                sink.layout,
                sink.rate,
            )
            .ok()
            .map(|c| (key, c));
        }
        let Some((_, rs)) = &mut self.rs else { return };
        let cap = f.samples() * sink.rate as usize / f.rate() as usize + 256;
        let mut out = frame::Audio::new(Sample::F32(SampleType::Packed), cap, sink.layout);
        if rs.run(f, &mut out).is_err() {
            return;
        }
        let ch = sink.ch as usize;
        let n = out.samples() * ch;
        if n == 0 {
            return;
        }
        // SAFETY: packed f32 frame holding `samples × channels` values
        let mut data =
            unsafe { std::slice::from_raw_parts((*out.as_ptr()).data[0] as *const f32, n) };
        let mut pts = secs(f.timestamp().or(f.pts()), self.tb, start);
        if let Some(p) = pts {
            if p < self.skip_before {
                let drop = ((self.skip_before - p) * sink.rate as f64) as usize;
                if drop >= out.samples() {
                    return;
                }
                data = &data[drop * ch..];
                pts = Some(self.skip_before);
            }
        }
        let Ok(mut b) = sink.buf.lock() else { return };
        if b.q.is_empty() {
            if let Some(p) = pts {
                b.front_pts = p;
            }
        }
        b.q.extend(data.iter().copied());
    }
}

/// Demuxer + video decoder (+ optional audio), shared by the player thread
/// and the one-shot helpers (thumbnail, snapshot).
struct Core {
    info: Info,
    ictx: format::context::Input,
    /// container start time (s); all timestamps are relative to it
    start: f64,
    vidx: usize,
    vtb: f64,
    vdec: decoder::Video,
    fdur: f64,
    sar: f64,
    max: (u32, u32),
    scaler: Option<Scaler>,
    vpkts: VecDeque<Packet>,
    frames: VecDeque<Decoded>,
    /// frames before this time are decoded but not scaled (exact seek)
    skip_before: f64,
    last_raw: Option<(f64, frame::Video)>,
    last_pts: f64,
    demux_eof: bool,
    veof_sent: bool,
    vdrained: bool,
    audio: Option<AudioTrack>,
}

impl Core {
    fn open(path: &Path, max: (u32, u32), want_audio: bool) -> Result<Core, String> {
        init();
        let ictx = format::input(path).map_err(|e| format!("can't open video ({e})"))?;
        // SAFETY: plain field read on a live context
        let st = unsafe { (*ictx.as_ptr()).start_time };
        let start = if st == ff::ffi::AV_NOPTS_VALUE {
            0.0
        } else {
            st as f64 / 1e6
        };
        let vs = ictx
            .streams()
            .best(media::Type::Video)
            .ok_or("no video stream")?;
        let vidx = vs.index();
        let vtb = f64::from(vs.time_base());
        let mut ctx =
            codec::Context::from_parameters(vs.parameters()).map_err(|e| e.to_string())?;
        ctx.set_threading(codec::threading::Config {
            kind: codec::threading::Type::Frame,
            count: 0,
            ..Default::default()
        });
        let video_codec = codec_name(ctx.id());
        let vdec = ctx
            .decoder()
            .video()
            .map_err(|e| format!("no decoder for {video_codec} ({e})"))?;
        let rate =
            |r: ff::Rational| (r.numerator() > 0 && r.denominator() > 0).then(|| f64::from(r));
        // r_frame_rate (the stream's base rate) first: avg_frame_rate is
        // derived from the duration and is off for short or trimmed clips
        let fps = rate(vs.rate())
            .filter(|f| *f <= 240.0)
            .or_else(|| rate(vs.avg_frame_rate()))
            .filter(|f| *f < 1000.0)
            .unwrap_or(25.0);
        let duration = if ictx.duration() > 0 {
            ictx.duration() as f64 / 1e6
        } else {
            (vs.duration().max(0) as f64 * vtb).max(0.0)
        };
        let sar = rate(vdec.aspect_ratio()).unwrap_or(1.0);

        let mut audio = None;
        let mut audio_desc = None;
        if let Some(a) = ictx.streams().best(media::Type::Audio) {
            let ctx = codec::Context::from_parameters(a.parameters()).ok();
            let name = codec_name(a.parameters().id());
            if let Some(dec) = ctx.and_then(|c| c.decoder().audio().ok()) {
                audio_desc = Some(format!(
                    "{name} {} kHz {}ch",
                    (dec.rate() as f64 / 1000.0 * 10.0).round() / 10.0,
                    dec.channels()
                ));
                if want_audio {
                    audio = Some(AudioTrack {
                        idx: a.index(),
                        tb: f64::from(a.time_base()),
                        dec,
                        rs: None,
                        sink: None,
                        skip_before: f64::NEG_INFINITY,
                        enabled: true,
                    });
                }
            } else {
                audio_desc = Some(format!("{name} (unsupported)"));
            }
        }
        let info = Info {
            duration,
            width: vdec.width(),
            height: vdec.height(),
            fps,
            video_codec,
            audio: audio_desc,
            bit_rate: ictx.bit_rate(),
        };
        Ok(Core {
            info,
            ictx,
            start,
            vidx,
            vtb,
            vdec,
            fdur: 1.0 / fps,
            sar,
            max,
            scaler: None,
            vpkts: VecDeque::new(),
            frames: VecDeque::new(),
            skip_before: f64::NEG_INFINITY,
            last_raw: None,
            last_pts: 0.0,
            demux_eof: false,
            veof_sent: false,
            vdrained: false,
            audio,
        })
    }

    fn sink(&self) -> Option<&Sink> {
        self.audio.as_ref().and_then(|a| a.sink.as_ref())
    }

    fn read_packet(&mut self) {
        let mut p = Packet::empty();
        match p.read(&mut self.ictx) {
            Ok(()) => {
                let si = p.stream();
                if si == self.vidx {
                    self.vpkts.push_back(p);
                } else if let Some(a) = self.audio.as_mut().filter(|a| a.idx == si && a.active()) {
                    if a.dec.send_packet(&p).is_ok() {
                        let mut f = frame::Audio::empty();
                        while a.dec.receive_frame(&mut f).is_ok() {
                            a.push(&mut f, self.start);
                        }
                    }
                }
            }
            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => {}
            Err(_) => self.demux_eof = true,
        }
    }

    /// Feed one packet (or EOF) to the video decoder and collect frames.
    /// false = no packet queued and the demuxer isn't done: read more.
    fn decode_video_step(&mut self) -> bool {
        if let Some(p) = self.vpkts.pop_front() {
            let _ = self.vdec.send_packet(&p); // corrupt packets are skipped
        } else if self.demux_eof {
            if !self.veof_sent {
                let _ = self.vdec.send_eof();
                self.veof_sent = true;
            }
        } else {
            return false;
        }
        loop {
            let mut f = frame::Video::empty();
            if self.vdec.receive_frame(&mut f).is_err() {
                break;
            }
            self.take_frame(f);
        }
        if self.veof_sent {
            self.vdrained = true;
        }
        true
    }

    fn take_frame(&mut self, f: frame::Video) {
        let pts = secs(f.timestamp().or(f.pts()), self.vtb, self.start)
            .unwrap_or(self.last_pts + self.fdur);
        self.last_pts = pts;
        if pts < self.skip_before - self.fdur * 0.5 {
            self.last_raw = Some((pts, f));
            return;
        }
        self.last_raw = None;
        if let Some(d) = self.scale(pts, &f) {
            self.frames.push_back(d);
        }
    }

    fn scale(&mut self, pts: f64, f: &frame::Video) -> Option<Decoded> {
        let (sw, sh) = (f.width(), f.height());
        if sw == 0 || sh == 0 {
            return None;
        }
        let (dw, dh) = fit((sw as f64 * self.sar).round() as u32, sh, self.max);
        let key = (f.format(), sw, sh, dw, dh);
        if self.scaler.as_ref().map(|s| s.key) != Some(key) {
            let ctx = scaling::Context::get(
                f.format(),
                sw,
                sh,
                format::Pixel::RGBA,
                dw,
                dh,
                scaling::Flags::BILINEAR,
            )
            .ok()?;
            self.scaler = Some(Scaler {
                key,
                ctx,
                out: frame::Video::empty(),
            });
        }
        let s = self.scaler.as_mut()?;
        s.ctx.run(f, &mut s.out).ok()?;
        let stride = s.out.stride(0);
        let data = s.out.data(0);
        let row = dw as usize * 4;
        let mut rgba = Vec::with_capacity(row * dh as usize);
        for y in 0..dh as usize {
            rgba.extend_from_slice(&data[y * stride..y * stride + row]);
        }
        Some(Decoded {
            pts,
            w: dw,
            h: dh,
            rgba,
        })
    }

    /// Next decoded frame in presentation order (blocking decode).
    fn next_frame(&mut self) -> Option<Decoded> {
        loop {
            if let Some(d) = self.frames.pop_front() {
                return Some(d);
            }
            if self.vdrained {
                return None;
            }
            if !self.decode_video_step() {
                self.read_packet();
            }
        }
    }

    /// Seek to the keyframe at/before `t`, then decode forward to the frame
    /// shown at `t` (the last frame if `t` is past the end).
    fn seek(&mut self, t: f64) -> Option<Decoded> {
        let t = t.clamp(0.0, self.info.duration.max(0.0));
        let ts = ((t + self.start) * 1e6) as i64;
        if self.ictx.seek(ts, ..ts).is_err() {
            let _ = self.ictx.seek(ts, ..);
        }
        self.vdec.flush();
        self.vpkts.clear();
        self.frames.clear();
        self.last_raw = None;
        self.demux_eof = false;
        self.veof_sent = false;
        self.vdrained = false;
        self.skip_before = t;
        self.last_pts = t;
        if let Some(a) = &mut self.audio {
            a.dec.flush();
            a.rs = None;
            a.skip_before = t;
            if let Some(Ok(mut b)) = a.sink.as_ref().map(|s| s.buf.lock()) {
                b.clear(t);
            }
        }
        let got = loop {
            if let Some(d) = self.frames.pop_front() {
                break Some(d);
            }
            if self.vdrained {
                break self.last_raw.take().and_then(|(p, f)| self.scale(p, &f));
            }
            if !self.decode_video_step() {
                self.read_packet();
            }
        };
        self.skip_before = f64::NEG_INFINITY;
        got
    }

    /// Keep the frame queue and audio buffer topped up (bounded work).
    fn fill(&mut self, budget: usize) {
        for _ in 0..budget {
            let need_v = self.frames.len() < QUEUE && !self.vdrained;
            let need_a = !self.demux_eof
                && self.vpkts.len() < 2048
                && self
                    .audio
                    .as_ref()
                    .filter(|a| a.active())
                    .and_then(|a| a.sink.as_ref())
                    .and_then(|s| s.buf.lock().ok().map(|b| b.buffered() < AUDIO_AHEAD))
                    .unwrap_or(false);
            if need_v {
                if !self.decode_video_step() {
                    self.read_packet();
                }
            } else if need_a {
                self.read_packet();
            } else {
                break;
            }
        }
    }
}

/// Decode the frame shown at `t` (seconds), scaled to fit `max`.
fn frame_at(path: &Path, t: f64, max: (u32, u32)) -> Result<(f64, image::RgbaImage), String> {
    let mut c = Core::open(path, max, false)?;
    let d = c.seek(t).ok_or("no frame decoded")?;
    let im = image::RgbaImage::from_raw(d.w, d.h, d.rgba).ok_or("bad frame")?;
    Ok((d.pts, im))
}

/// First-frame-ish thumbnail (seek ~10% in) scaled to fit `max`×`max`, for
/// the grid view. Blocking; None on any failure.
pub fn thumbnail(path: &Path, max: u32) -> Option<image::RgbaImage> {
    let mut c = Core::open(path, (max.max(1), max.max(1)), false).ok()?;
    let d = c.seek(c.info.duration * 0.1)?;
    image::RgbaImage::from_raw(d.w, d.h, d.rgba)
}

fn snapshot(path: &Path, t: f64) -> Result<PathBuf, String> {
    let (pts, im) = frame_at(path, t, (u32::MAX, u32::MAX))?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let s = pts.max(0.0) as u64;
    let name = format!("{stem}-frame-{:02}m{:02}s.png", s / 60, s % 60);
    let dst = crate::ops::unique_name(dir, &name);
    im.save(&dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Stream-copy `[tin, tout)` of `src` into `dst`: starts at the video
/// keyframe at/before `tin` (no re-encode), timestamps shifted to 0.
fn export_trim(
    src: &Path,
    dst: &Path,
    tin: f64,
    tout: f64,
    progress: &dyn Fn(f32),
) -> Result<(), String> {
    init();
    let e = |e: ff::Error| e.to_string();
    let mut ictx = format::input(src).map_err(e)?;
    // SAFETY: plain field read on a live context
    let st = unsafe { (*ictx.as_ptr()).start_time };
    let start = if st == ff::ffi::AV_NOPTS_VALUE {
        0.0
    } else {
        st as f64 / 1e6
    };
    let vidx = ictx
        .streams()
        .best(media::Type::Video)
        .ok_or("no video stream")?
        .index();
    let mut octx = format::output(dst).map_err(e)?;
    let n = ictx.nb_streams() as usize;
    let mut map = vec![usize::MAX; n];
    let mut itb = vec![0.0f64; n];
    let mut irat = vec![ff::Rational(0, 1); n];
    let mut next = 0;
    for ist in ictx.streams() {
        let m = ist.parameters().medium();
        if m != media::Type::Video && m != media::Type::Audio {
            continue;
        }
        let mut ost = octx.add_stream(encoder::find(codec::Id::None)).map_err(e)?;
        ost.set_parameters(ist.parameters());
        // SAFETY: let the muxer choose its own tag for the codec
        unsafe { (*ost.parameters().as_mut_ptr()).codec_tag = 0 };
        map[ist.index()] = next;
        itb[ist.index()] = f64::from(ist.time_base());
        irat[ist.index()] = ist.time_base();
        next += 1;
    }
    octx.set_metadata(ictx.metadata().to_owned());
    octx.write_header().map_err(e)?;
    let otb: Vec<ff::Rational> = (0..next)
        .map(|i| {
            octx.stream(i)
                .map(|s| s.time_base())
                .unwrap_or(ff::Rational(1, 1000))
        })
        .collect();

    let ts = ((tin + start) * 1e6) as i64;
    if ictx.seek(ts, ..ts).is_err() {
        let _ = ictx.seek(ts, ..);
    }
    let end = tout + start;
    let mut offset: Option<f64> = None; // absolute seconds of the first keyframe
    let mut video_done = false;
    let mut last_report = Instant::now();
    loop {
        let mut p = Packet::empty();
        match p.read(&mut ictx) {
            Ok(()) => {}
            Err(ff::Error::Other { errno }) if errno == ff::util::error::EAGAIN => continue,
            Err(_) => break,
        }
        let si = p.stream();
        if map[si] == usize::MAX {
            continue;
        }
        let tb = itb[si];
        let t_pts = p.pts().or(p.dts()).map(|x| x as f64 * tb);
        let t_dts = p.dts().or(p.pts()).map(|x| x as f64 * tb);
        let (Some(tp), Some(td)) = (t_pts, t_dts) else {
            continue;
        };
        let off = match offset {
            Some(o) => o,
            None => {
                if si != vidx || !p.is_key() {
                    continue;
                }
                let o = tp.min(td);
                offset = Some(o);
                o
            }
        };
        if si == vidx {
            if td >= end {
                video_done = true;
                continue;
            }
        } else if tp >= end || tp < off {
            continue;
        }
        if video_done && td > end + 1.0 {
            break;
        }
        let shift = (off / tb).round() as i64;
        p.set_pts(p.pts().map(|x| x - shift));
        p.set_dts(p.dts().map(|x| x - shift));
        p.rescale_ts(irat[si], otb[map[si]]);
        p.set_position(-1);
        p.set_stream(map[si]);
        p.write_interleaved(&mut octx).map_err(e)?;
        if last_report.elapsed() > Duration::from_millis(100) {
            last_report = Instant::now();
            let span = (end - off).max(1e-3);
            progress((((td - off) / span) as f32).clamp(0.0, 1.0));
        }
    }
    if offset.is_none() {
        return Err("no keyframe in range".into());
    }
    octx.write_trailer().map_err(e)?;
    progress(1.0);
    Ok(())
}

// ══════════════════════════════════════════════════════════════════ player

/// A frame for the UI.
#[derive(Debug, Clone)]
pub struct Frame {
    serial: u64,
    pts: f64,
    handle: img::Handle,
}

#[derive(Debug, Clone)]
pub enum Out {
    Frame(Frame),
    Ended,
}

enum Cmd {
    Play(tmpsc::Sender<Out>),
    Pause,
    Seek {
        t: f64,
        serial: u64,
        reply: oneshot::Sender<Option<Frame>>,
    },
    Step(oneshot::Sender<Option<Frame>>),
    Speed(f32),
    Stop,
}

/// Handle to the decoder thread; dropping it stops decoding and audio.
struct Player {
    tx: mpsc::Sender<Cmd>,
    ctl: Arc<AudioCtl>,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Stop);
    }
}

impl Player {
    fn spawn(path: PathBuf, want_audio: bool) -> Player {
        let (tx, rx) = mpsc::channel();
        let ctl = Arc::new(AudioCtl {
            volume: AtomicU32::new(1.0f32.to_bits()),
            muted: AtomicBool::new(false),
        });
        let c = ctl.clone();
        let _ = std::thread::Builder::new()
            .name("og-files-video".into())
            .spawn(move || {
                // a broken file: answer requests with "no frame" until closed
                match Core::open(&path, (MAX_DISPLAY, MAX_DISPLAY), want_audio) {
                    Ok(core) => Playback::new(core, c, want_audio).run(rx),
                    Err(_) => {
                        while let Ok(cmd) = rx.recv() {
                            match cmd {
                                Cmd::Seek { reply, .. } | Cmd::Step(reply) => {
                                    let _ = reply.send(None);
                                }
                                Cmd::Stop => return,
                                _ => {}
                            }
                        }
                    }
                }
            });
        Player { tx, ctl }
    }

    fn send(&self, c: Cmd) {
        let _ = self.tx.send(c);
    }

    /// A one-frame request (seek / step) as a future.
    fn request(
        &self,
        make: impl FnOnce(oneshot::Sender<Option<Frame>>) -> Cmd,
    ) -> impl std::future::Future<Output = Option<Frame>> {
        let (reply, rx) = oneshot::channel();
        self.send(make(reply));
        async move { rx.await.ok().flatten() }
    }
}

struct Playback {
    core: Core,
    ctl: Arc<AudioCtl>,
    want_audio: bool,
    audio_tried: bool,
    playing: bool,
    speed: f32,
    /// pts of the frame on screen
    cur: f64,
    base_pts: f64,
    base_at: Instant,
    out: Option<tmpsc::Sender<Out>>,
    serial: u64,
}

impl Playback {
    fn new(core: Core, ctl: Arc<AudioCtl>, want_audio: bool) -> Self {
        Playback {
            core,
            ctl,
            want_audio,
            audio_tried: false,
            playing: false,
            speed: 1.0,
            cur: 0.0,
            base_pts: 0.0,
            base_at: Instant::now(),
            out: None,
            serial: 0,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Cmd>) {
        loop {
            let cmd = if self.playing {
                match rx.recv_timeout(self.wait()) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                self.core.fill(QUEUE * 4); // prefill so Play starts at once
                match rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };
            if let Some(c) = cmd {
                if !self.handle(c) {
                    return;
                }
            }
            if self.playing {
                self.core.fill(32);
                self.present();
            }
        }
    }

    fn frame(&self, d: Decoded) -> Frame {
        Frame {
            serial: self.serial,
            pts: d.pts,
            handle: img::Handle::from_rgba(d.w, d.h, d.rgba),
        }
    }

    fn set_running(&self, on: bool) {
        if let Some(Ok(mut b)) = self.core.sink().map(|s| s.buf.lock()) {
            b.running = on;
        }
    }

    fn anchor(&mut self, t: f64) {
        self.base_pts = t;
        self.base_at = Instant::now();
    }

    /// Master clock: audio when it's playing, wall clock otherwise.
    fn clock(&mut self) -> f64 {
        if !self.playing {
            return self.cur;
        }
        if self.speed == 1.0 {
            let a = self.core.sink().and_then(|s| {
                s.buf
                    .lock()
                    .ok()
                    .filter(|b| !b.q.is_empty())
                    .map(|b| b.front_pts)
            });
            if let Some(t) = a {
                self.anchor(t);
                return t;
            }
        }
        self.base_pts + self.base_at.elapsed().as_secs_f64() * self.speed as f64
    }

    fn wait(&mut self) -> Duration {
        let now = self.clock();
        let dt = match self.core.frames.front() {
            Some(f) => (f.pts - now) / self.speed as f64,
            None => 0.005,
        };
        Duration::from_secs_f64(dt.clamp(0.001, 0.02))
    }

    /// Re-decode from `t` (keeps the shown frame; refills audio).
    fn reseek(&mut self, t: f64) {
        if let Some(d) = self.core.seek(t) {
            self.core.frames.push_front(d);
        }
        self.anchor(t);
    }

    fn handle(&mut self, c: Cmd) -> bool {
        match c {
            Cmd::Stop => return false,
            Cmd::Play(tx) => {
                if self.want_audio && !self.audio_tried {
                    self.audio_tried = true;
                    if let Some(sink) = open_device(self.ctl.clone()) {
                        if let Some(a) = &mut self.core.audio {
                            a.sink = Some(sink);
                            let t = self.cur;
                            self.reseek(t);
                        }
                    }
                }
                self.out = Some(tx);
                self.playing = true;
                self.anchor(self.cur);
                if let Some(Ok(mut b)) = self.core.sink().map(|s| s.buf.lock()) {
                    b.trim_to(self.cur);
                    b.running = true;
                }
            }
            Cmd::Pause => {
                self.playing = false;
                self.out = None;
                self.set_running(false);
            }
            Cmd::Seek { t, serial, reply } => {
                self.serial = serial;
                let d = self.core.seek(t);
                if let Some(d) = &d {
                    self.cur = d.pts;
                }
                self.anchor(self.cur);
                let _ = reply.send(d.map(|d| self.frame(d)));
            }
            Cmd::Step(reply) => {
                let d = if self.playing {
                    None
                } else {
                    self.core.next_frame()
                };
                if let Some(d) = &d {
                    self.cur = d.pts;
                    if let Some(Ok(mut b)) = self.core.sink().map(|s| s.buf.lock()) {
                        b.trim_to(d.pts);
                    }
                }
                let _ = reply.send(d.map(|d| self.frame(d)));
            }
            Cmd::Speed(s) => {
                let now = self.clock();
                let was_normal = self.speed == 1.0;
                self.speed = s;
                if let Some(a) = &mut self.core.audio {
                    a.enabled = s == 1.0;
                }
                if was_normal != (s == 1.0) {
                    if let Some(Ok(mut b)) = self.core.sink().map(|s| s.buf.lock()) {
                        b.clear(now);
                    }
                    if s == 1.0 && self.core.sink().is_some() {
                        // back to 1×: realign audio with the picture
                        let t = self.cur;
                        self.reseek(t);
                        return true;
                    }
                }
                self.anchor(now);
            }
        }
        true
    }

    fn present(&mut self) {
        let now = self.clock();
        let mut show = None;
        while self.core.frames.front().is_some_and(|f| f.pts <= now) {
            show = self.core.frames.pop_front(); // late frames are dropped
        }
        if let Some(d) = show {
            self.cur = d.pts;
            let f = self.frame(d);
            if let Some(tx) = &self.out {
                let _ = tx.try_send(Out::Frame(f)); // UI behind: skip
            }
        }
        if self.core.frames.is_empty() && self.core.vdrained {
            self.playing = false;
            self.set_running(false);
            if let Some(tx) = self.out.take() {
                let _ = tx.blocking_send(Out::Ended);
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════════════ UI

fn fmt_time(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_mark(t: f64) -> String {
    format!("{}.{}", fmt_time(t), ((t.max(0.0) * 10.0) as u64) % 10)
}

pub struct State {
    path: PathBuf,
    info: Info,
    player: Player,
    frame: Option<img::Handle>,
    failed: bool,
    pos: f64,
    playing: bool,
    serial: u64,
    drag: Option<f64>,
    seeking: bool,
    pending_seek: Option<f64>,
    volume: f32,
    muted: bool,
    speed_ix: usize,
    editing: bool,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    export: Option<f32>,
    snapshotting: bool,
}

#[derive(Debug, Clone)]
pub enum ExportEv {
    Progress(f32),
    Done(Result<PathBuf, String>),
}

#[derive(Debug, Clone)]
pub enum Msg {
    TogglePlay,
    Out(Out),
    SeekDrag(f64),
    SeekRelease,
    Seeked(Option<Frame>),
    Step,
    Stepped(Option<Frame>),
    Volume(f32),
    ToggleMute,
    CycleSpeed,
    ToggleEdit,
    SetIn,
    SetOut,
    ClearMarks,
    Export,
    Exporting(ExportEv),
    Snapshot,
    SnapshotDone(Result<PathBuf, String>),
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        Self::with_audio(o, true)
    }

    fn with_audio(o: Opened, audio: bool) -> (Self, Task<Msg>) {
        let player = Player::spawn(o.path.clone(), audio);
        let mut s = Self {
            path: o.path,
            info: o.info,
            player,
            frame: None,
            failed: false,
            pos: 0.0,
            playing: false,
            serial: 0,
            drag: None,
            seeking: false,
            pending_seek: None,
            volume: 1.0,
            muted: false,
            speed_ix: 1,
            editing: false,
            mark_in: None,
            mark_out: None,
            export: None,
            snapshotting: false,
        };
        let t = s.request_seek(0.0);
        (s, t)
    }

    fn request_seek(&mut self, t: f64) -> Task<Msg> {
        if self.seeking {
            self.pending_seek = Some(t);
            return Task::none();
        }
        self.seeking = true;
        self.serial += 1;
        let serial = self.serial;
        Task::perform(
            self.player
                .request(move |reply| Cmd::Seek { t, serial, reply }),
            Msg::Seeked,
        )
    }

    fn show(&mut self, f: Frame) {
        if f.serial == self.serial {
            if self.drag.is_none() {
                self.pos = f.pts;
            }
            self.frame = Some(f.handle);
        }
    }

    fn play_task(&mut self) -> Task<Msg> {
        let (tx, mut rx) = tmpsc::channel::<Out>(2);
        self.player.send(Cmd::Play(tx));
        self.playing = true;
        let stream = iced::stream::channel(2, move |mut out| async move {
            while let Some(o) = rx.recv().await {
                if out.send(o).await.is_err() {
                    break;
                }
            }
        });
        Task::run(stream, Msg::Out)
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::TogglePlay => {
                if self.playing {
                    self.player.send(Cmd::Pause);
                    self.playing = false;
                    return (Task::none(), None);
                }
                // at the end: start over
                let mut first = Task::none();
                if self.pos >= self.info.duration - 1.5 / self.info.fps.max(1.0) {
                    first = self.request_seek(0.0);
                }
                let play = self.play_task();
                (Task::batch([first, play]), None)
            }
            Msg::Out(Out::Frame(f)) => {
                self.show(f);
                (Task::none(), None)
            }
            Msg::Out(Out::Ended) => {
                self.playing = false;
                (Task::none(), None)
            }
            Msg::SeekDrag(t) => {
                self.drag = Some(t);
                self.pos = t;
                (self.request_seek(t), None)
            }
            Msg::SeekRelease => {
                let t = self.drag.take();
                match t {
                    Some(t) => (self.request_seek(t), None),
                    None => (Task::none(), None),
                }
            }
            Msg::Seeked(f) => {
                self.seeking = false;
                match f {
                    Some(f) => self.show(f),
                    None if self.frame.is_none() => self.failed = true,
                    None => {}
                }
                match self.pending_seek.take() {
                    Some(t) => (self.request_seek(t), None),
                    None => (Task::none(), None),
                }
            }
            Msg::Step => {
                if self.playing {
                    return (Task::none(), None);
                }
                (
                    Task::perform(self.player.request(Cmd::Step), Msg::Stepped),
                    None,
                )
            }
            Msg::Stepped(f) => {
                if let Some(f) = f {
                    self.show(f);
                }
                (Task::none(), None)
            }
            Msg::Volume(v) => {
                self.volume = v;
                self.player.ctl.volume.store(v.to_bits(), Ordering::Relaxed);
                (Task::none(), None)
            }
            Msg::ToggleMute => {
                self.muted = !self.muted;
                self.player.ctl.muted.store(self.muted, Ordering::Relaxed);
                (Task::none(), None)
            }
            Msg::CycleSpeed => {
                self.speed_ix = (self.speed_ix + 1) % SPEEDS.len();
                self.player.send(Cmd::Speed(SPEEDS[self.speed_ix]));
                (Task::none(), None)
            }
            Msg::ToggleEdit => (self.toggle_edit(), None),
            Msg::SetIn => {
                self.mark_in = Some(self.pos);
                if self.mark_out.is_some_and(|o| o <= self.pos) {
                    self.mark_out = None;
                }
                (Task::none(), None)
            }
            Msg::SetOut => {
                self.mark_out = Some(self.pos);
                if self.mark_in.is_some_and(|i| i >= self.pos) {
                    self.mark_in = None;
                }
                (Task::none(), None)
            }
            Msg::ClearMarks => {
                self.mark_in = None;
                self.mark_out = None;
                (Task::none(), None)
            }
            Msg::Export => {
                if self.export.is_some() {
                    return (Task::none(), None);
                }
                let tin = self.mark_in.unwrap_or(0.0);
                let tout = self.mark_out.unwrap_or(self.info.duration);
                if tout <= tin {
                    return (
                        Task::none(),
                        Some(Event::Status("Trim: Out must be after In".into())),
                    );
                }
                self.export = Some(0.0);
                (spawn_export(self.path.clone(), tin, tout), None)
            }
            Msg::Exporting(ExportEv::Progress(p)) => {
                self.export = Some(p);
                (Task::none(), None)
            }
            Msg::Exporting(ExportEv::Done(r)) => {
                self.export = None;
                let s = match r {
                    Ok(p) => format!("Exported trim → {}", p.display()),
                    Err(e) => format!("Trim export failed: {e}"),
                };
                (Task::none(), Some(Event::Status(s)))
            }
            Msg::Snapshot => {
                if self.snapshotting {
                    return (Task::none(), None);
                }
                self.snapshotting = true;
                let (path, t) = (self.path.clone(), self.pos);
                let fut = async move {
                    tokio::task::spawn_blocking(move || snapshot(&path, t))
                        .await
                        .map_err(|e| e.to_string())?
                };
                (Task::perform(fut, Msg::SnapshotDone), None)
            }
            Msg::SnapshotDone(r) => {
                self.snapshotting = false;
                let s = match r {
                    Ok(p) => format!("Saved frame → {}", p.display()),
                    Err(e) => format!("Snapshot failed: {e}"),
                };
                (Task::none(), Some(Event::Status(s)))
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let picture: Element<'_, Msg> = match &self.frame {
            Some(h) => img(h.clone())
                .content_fit(iced::ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => text(if self.failed {
                "Can't decode this video"
            } else {
                "Loading…"
            })
            .size(13)
            .color(theme::MUTED)
            .into(),
        };
        let screen = container(picture)
            .center(Length::Fill)
            .style(|_| container::Style {
                background: Some(iced::Color::BLACK.into()),
                border: iced::Border {
                    radius: 6.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });

        let dur = self.info.duration.max(0.01);
        let pos = self.drag.unwrap_or(self.pos).clamp(0.0, dur);
        let seek = slider(0.0..=dur, pos, Msg::SeekDrag)
            .on_release(Msg::SeekRelease)
            .step(0.01);

        let ibtn = |b: B, msg: Option<Msg>| {
            button(icon(b).size(14))
                .padding([4, 8])
                .on_press_maybe(msg)
                .style(button::secondary)
        };
        let controls = row![
            ibtn(
                if self.playing {
                    B::PauseFill
                } else {
                    B::PlayFill
                },
                Some(Msg::TogglePlay)
            ),
            ibtn(B::SkipEndFill, (!self.playing).then_some(Msg::Step)),
            text(format!(
                "{} / {}",
                fmt_time(pos),
                fmt_time(self.info.duration)
            ))
            .size(12),
            Space::with_width(Length::Fill),
            button(text(format!("{}×", SPEEDS[self.speed_ix])).size(12))
                .padding([4, 8])
                .on_press(Msg::CycleSpeed)
                .style(button::secondary),
            ibtn(
                if self.muted || self.volume == 0.0 {
                    B::VolumeMuteFill
                } else {
                    B::VolumeUpFill
                },
                Some(Msg::ToggleMute)
            ),
            slider(0.0f32..=1.0, self.volume, Msg::Volume)
                .step(0.01f32)
                .width(90),
            if self.editing {
                button(text("Done").size(12))
                    .padding([4, 8])
                    .on_press(Msg::ToggleEdit)
                    .style(button::secondary)
            } else {
                button(text("Edit  Ctrl+E").size(12))
                    .padding([4, 8])
                    .on_press(Msg::ToggleEdit)
            },
        ]
        .spacing(6)
        .align_y(Alignment::Center);

        let mut speed_note = String::new();
        if SPEEDS[self.speed_ix] != 1.0 && self.info.audio.is_some() {
            speed_note = "  ·  audio muted at this speed".into();
        }
        let info = text(self.info.summary() + &speed_note)
            .size(12)
            .color(theme::MUTED);

        let mut col = column![screen, seek, controls, info].spacing(6);
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
                .on_press_maybe((!busy).then_some(Msg::Export)),
            ]
            .spacing(6)
            .align_y(Alignment::Center);
            col = col.push(edit).push(
                text("Exports are new files (keyframe-aligned stream copy); the video is never saved over.")
                    .size(12)
                    .color(theme::MUTED),
            );
        }
        col.into()
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        Some(self.editing)
    }

    /// Ctrl+E. Never refused: edits only ever produce new files.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        self.editing = !self.editing;
        Task::none()
    }

    /// Ctrl+S: nothing to save — video is never written over.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}

/// `<stem>-edit.<ext>` beside `src`, made unique.
fn edit_name(src: &Path) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let name = match src.extension() {
        Some(ext) => format!("{stem}-edit.{}", ext.to_string_lossy()),
        None => format!("{stem}-edit"),
    };
    crate::ops::unique_name(dir, &name)
}

fn spawn_export(src: PathBuf, tin: f64, tout: f64) -> Task<Msg> {
    let (tx, mut rx) = tmpsc::unbounded_channel::<ExportEv>();
    std::thread::spawn(move || {
        let dst = edit_name(&src);
        let p = tx.clone();
        let r = export_trim(&src, &dst, tin, tout, &move |f| {
            let _ = p.send(ExportEv::Progress(f));
        });
        if r.is_err() {
            let _ = std::fs::remove_file(&dst);
        }
        let _ = tx.send(ExportEv::Done(r.map(|()| dst)));
    });
    let stream = iced::stream::channel(4, move |mut out| async move {
        while let Some(ev) = rx.recv().await {
            if out.send(ev).await.is_err() {
                break;
            }
        }
    });
    Task::run(stream, Msg::Exporting)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scratch dir under ~/.cache (never /tmp), removed on drop.
    struct Tmp(PathBuf);
    impl Tmp {
        fn new(name: &str) -> Tmp {
            let home = std::env::var_os("HOME").expect("HOME");
            let d = PathBuf::from(home)
                .join(".cache/ogx-p2-video-tests")
                .join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const W: u32 = 160;
    const H: u32 = 120;
    const FPS: i32 = 25;
    const GOP: u32 = 10; // keyframe every 0.4 s

    fn drain(
        enc: &mut encoder::Encoder,
        octx: &mut format::context::Output,
        idx: usize,
        dur: i64,
        from: ff::Rational,
        to: ff::Rational,
    ) {
        let mut p = Packet::empty();
        while enc.receive_packet(&mut p).is_ok() {
            p.set_stream(idx);
            if p.duration() <= 0 {
                p.set_duration(dur);
            }
            p.rescale_ts(from, to);
            p.write_interleaved(octx).unwrap();
        }
    }

    /// 2 s test clip written with the libavcodec *encoders*: MPEG-4 part 2
    /// video (160×120, 25 fps, GOP 10) + AAC 440 Hz sine, in MP4.
    fn make_video(path: &Path) {
        init();
        let mut octx = format::output(path).unwrap();
        let global = octx.format().flags().contains(format::Flags::GLOBAL_HEADER);

        let vcodec = encoder::find(codec::Id::MPEG4).expect("mpeg4 encoder");
        let mut v = codec::Context::new_with_codec(vcodec)
            .encoder()
            .video()
            .unwrap();
        v.set_width(W);
        v.set_height(H);
        v.set_format(format::Pixel::YUV420P);
        v.set_time_base((1, FPS));
        v.set_frame_rate(Some((FPS, 1)));
        v.set_gop(GOP);
        v.set_max_b_frames(0);
        v.set_bit_rate(400_000);
        if global {
            v.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let mut venc = v.open_as(vcodec).unwrap();

        let acodec = encoder::find(codec::Id::AAC).expect("aac encoder");
        let mut a = codec::Context::new_with_codec(acodec)
            .encoder()
            .audio()
            .unwrap();
        a.set_rate(44_100);
        a.set_format(Sample::F32(SampleType::Planar));
        a.set_channel_layout(ChannelLayout::MONO);
        a.set_time_base((1, 44_100));
        a.set_bit_rate(64_000);
        if global {
            a.set_flags(codec::Flags::GLOBAL_HEADER);
        }
        let mut aenc = a.open_as(acodec).unwrap();

        {
            let mut s = octx.add_stream(vcodec).unwrap();
            s.set_parameters(&venc);
            s.set_time_base((1, FPS));
        }
        {
            let mut s = octx.add_stream(acodec).unwrap();
            s.set_parameters(&aenc);
            s.set_time_base((1, 44_100));
        }
        octx.write_header().unwrap();
        let vtb = octx.stream(0).unwrap().time_base();
        let atb = octx.stream(1).unwrap().time_base();

        for i in 0..(2 * FPS) as i64 {
            let mut f = frame::Video::new(format::Pixel::YUV420P, W, H);
            let stride = f.stride(0);
            let y = f.data_mut(0);
            for row in 0..H as usize {
                for col in 0..W as usize {
                    y[row * stride + col] = ((col + row) as i64 + i * 4) as u8;
                }
            }
            for pl in 1..3 {
                f.data_mut(pl).fill(128);
            }
            f.set_pts(Some(i));
            venc.send_frame(&f).unwrap();
            drain(&mut venc, &mut octx, 0, 1, (1, FPS).into(), vtb);
        }
        venc.send_eof().unwrap();
        drain(&mut venc, &mut octx, 0, 1, (1, FPS).into(), vtb);

        let n = aenc.frame_size() as usize;
        let total = 2 * 44_100;
        let mut done = 0;
        while done < total {
            let mut f = frame::Audio::new(Sample::F32(SampleType::Planar), n, ChannelLayout::MONO);
            f.set_rate(44_100);
            for (k, s) in f.plane_mut::<f32>(0).iter_mut().enumerate() {
                let t = (done + k) as f32 / 44_100.0;
                *s = 0.3 * (t * 440.0 * std::f32::consts::TAU).sin();
            }
            f.set_pts(Some(done as i64));
            aenc.send_frame(&f).unwrap();
            drain(&mut aenc, &mut octx, 1, n as i64, (1, 44_100).into(), atb);
            done += n;
        }
        aenc.send_eof().unwrap();
        drain(&mut aenc, &mut octx, 1, n as i64, (1, 44_100).into(), atb);
        octx.write_trailer().unwrap();
    }

    fn clip(t: &Tmp) -> PathBuf {
        let p = t.0.join("clip.mp4");
        make_video(&p);
        p
    }

    #[test]
    fn video_supported_extensions() {
        for n in [
            "a.mp4", "b.MKV", "c.webm", "d.mov", "e.mpeg", "f.ts", "g.3gp", "h.ogv",
        ] {
            assert!(supported(Path::new(n)), "{n}");
        }
        for n in ["a.mp3", "b.png", "c", "d.txt"] {
            assert!(!supported(Path::new(n)), "{n}");
        }
    }

    #[tokio::test]
    async fn video_open_probes_metadata() {
        let t = Tmp::new("probe");
        let p = clip(&t);
        let o = open(p.clone()).await.unwrap();
        let i = &o.info;
        assert_eq!((i.width, i.height), (W, H));
        assert!((i.fps - 25.0).abs() < 0.01, "fps {}", i.fps);
        assert!((i.duration - 2.0).abs() < 0.1, "duration {}", i.duration);
        assert_eq!(i.video_codec, "MPEG4");
        let a = i.audio.as_deref().unwrap();
        assert!(a.starts_with("AAC 44.1 kHz 1ch"), "{a}");
        assert!(i.bit_rate > 0);
        assert!(i.summary().contains("160×120"));
        assert!(open(t.0.join("missing.mp4")).await.is_err());
    }

    #[test]
    fn video_first_frame_and_thumbnail() {
        let t = Tmp::new("thumb");
        let p = clip(&t);
        let (pts, im) = frame_at(&p, 0.0, (u32::MAX, u32::MAX)).unwrap();
        assert_eq!(im.dimensions(), (W, H));
        assert!(pts.abs() < 0.001, "pts {pts}");
        let th = thumbnail(&p, 64).unwrap();
        assert_eq!(th.dimensions(), (64, 48));
        assert!(thumbnail(&t.0.join("nope.mp4"), 64).is_none());
    }

    #[test]
    fn video_seek_is_frame_accurate() {
        let t = Tmp::new("seek");
        let p = clip(&t);
        let fd = 1.0 / FPS as f64;
        let mut c = Core::open(&p, (W, H), false).unwrap();
        // out of order, mid-GOP and on keyframes
        for target in [1.37, 0.5, 0.93, 0.0, 1.9, 0.61, 0.8] {
            let d = c.seek(target).unwrap();
            assert!(
                (d.pts - target).abs() <= fd * 0.5 + 1e-6,
                "seek {target} -> {}",
                d.pts
            );
            // and it's the right picture: luma ramp offset = 4 × frame index
            let i = (d.pts * FPS as f64).round() as i64;
            let px = d.rgba[(10 * W as usize + 10) * 4] as i64; // grey: R ≈ Y
            let want = ((20 + i * 4) % 256) as f64;
            let want_rgb = ((want - 16.0) * 255.0 / 219.0).clamp(0.0, 255.0) as i64;
            assert!(
                (px - want_rgb).abs() < 20,
                "frame {i}: R={px} want≈{want_rgb}"
            );
        }
        let mut n = 1;
        c.seek(0.0).unwrap();
        while c.next_frame().is_some() {
            n += 1;
        }
        assert_eq!(n, 2 * FPS);
        let last = c.seek(5.0).unwrap();
        assert!((last.pts - (2.0 - fd)).abs() < 1e-6, "end -> {}", last.pts);
    }

    #[test]
    fn video_player_seek_step_play() {
        let t = Tmp::new("player");
        let p = clip(&t);
        let fd = 1.0 / FPS as f64;
        let pl = Player::spawn(p, false); // no audio device in tests
        let seek = |t: f64| {
            let (reply, rx) = oneshot::channel();
            pl.send(Cmd::Seek {
                t,
                serial: 7,
                reply,
            });
            rx.blocking_recv().unwrap().unwrap()
        };
        let f = seek(1.0);
        assert!((f.pts - 1.0).abs() <= fd * 0.5 + 1e-6, "{}", f.pts);
        assert_eq!(f.serial, 7);
        let (reply, rx) = oneshot::channel();
        pl.send(Cmd::Step(reply));
        let s = rx.blocking_recv().unwrap().unwrap();
        assert!(
            (s.pts - f.pts - fd).abs() < 1e-6,
            "step {} -> {}",
            f.pts,
            s.pts
        );

        // play the last 0.3 s at 2×: frames arrive in order, then Ended
        seek(1.7);
        pl.send(Cmd::Speed(2.0));
        let (tx, mut rx) = tmpsc::channel(2);
        pl.send(Cmd::Play(tx));
        let start = Instant::now();
        let mut last = 1.7 - fd;
        let mut frames = 0;
        let ended = loop {
            match rx.blocking_recv() {
                Some(Out::Frame(f)) => {
                    assert!(f.pts > last, "{} after {last}", f.pts);
                    last = f.pts;
                    frames += 1;
                }
                Some(Out::Ended) => break true,
                None => break false,
            }
        };
        assert!(ended && frames > 0, "ended={ended} frames={frames}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        drop(pl); // Drop stops the worker
    }

    #[test]
    fn video_trim_export_stream_copy() {
        let t = Tmp::new("trim");
        let p = clip(&t);
        let dst = edit_name(&p);
        assert_eq!(dst, t.0.join("clip-edit.mp4"));
        let seen = std::cell::Cell::new(0.0f32);
        export_trim(&p, &dst, 0.5, 1.5, &|f| seen.set(f)).unwrap();
        assert_eq!(seen.get(), 1.0);
        let o = Core::open(&dst, (W, H), false).unwrap().info;
        let kf = GOP as f64 / FPS as f64;
        // starts at the keyframe at/before In (0.4 s): ≈1.1 s long
        assert!(
            o.duration >= 1.0 - 0.05 && o.duration <= 1.0 + kf + 0.1,
            "trimmed duration {}",
            o.duration
        );
        assert_eq!((o.width, o.height), (W, H));
        assert!(o.audio.is_some());
        // playable: decodes from the start and near the end
        let (pts, im) = frame_at(&dst, 0.0, (u32::MAX, u32::MAX)).unwrap();
        assert!(pts.abs() < 0.05, "first pts {pts}");
        assert_eq!(im.dimensions(), (W, H));
        assert!(frame_at(&dst, o.duration - 0.1, (W, H)).is_ok());
        // a second export gets a fresh name
        assert_ne!(edit_name(&p), dst);
    }

    fn status(e: Option<Event>) -> String {
        match e {
            Some(Event::Status(s)) => s,
            _ => panic!("expected a status"),
        }
    }

    /// UI state machine without a window (and without ever pressing Play,
    /// so no audio device is opened).
    #[tokio::test]
    async fn video_state_controls_and_edit_mode() {
        let t = Tmp::new("state");
        let p = clip(&t);
        let o = open(p.clone()).await.unwrap();
        let (mut s, _first) = State::new(o);
        assert!(!s.is_dirty());
        assert_eq!(s.is_editing(), Some(false));
        let _ = s.view();

        // a seek reply from the worker shows that frame
        s.seeking = false;
        let _ = s.request_seek(1.0);
        let f = s
            .player
            .request({
                let serial = s.serial;
                move |reply| Cmd::Seek {
                    t: 1.0,
                    serial,
                    reply,
                }
            })
            .await;
        let (_, e) = s.update(Msg::Seeked(f));
        assert!(e.is_none());
        assert!(s.frame.is_some());
        assert!((s.pos - 1.0).abs() < 0.03, "pos {}", s.pos);
        // stale frames (older serial) are ignored
        let old = Frame {
            serial: 0,
            pts: 0.2,
            handle: img::Handle::from_rgba(1, 1, vec![0; 4]),
        };
        let _ = s.update(Msg::Out(Out::Frame(old)));
        assert!((s.pos - 1.0).abs() < 0.03);

        let _ = s.update(Msg::Volume(0.5));
        assert_eq!(
            f32::from_bits(s.player.ctl.volume.load(Ordering::Relaxed)),
            0.5
        );
        let _ = s.update(Msg::ToggleMute);
        assert_eq!(s.player.ctl.gain(), 0.0);
        let _ = s.update(Msg::CycleSpeed);
        assert_eq!(SPEEDS[s.speed_ix], 1.5);
        s.playing = true; // as if playing: the toggle pauses
        let _ = s.update(Msg::TogglePlay);
        assert!(!s.playing);
        let _ = s.update(Msg::Out(Out::Ended));
        assert_eq!(fmt_time(3725.0), "1:02:05");
        assert_eq!(fmt_mark(61.25), "1:01.2");

        // edit mode: marks, export (new file), snapshot status; never dirty
        let _ = s.toggle_edit();
        assert_eq!(s.is_editing(), Some(true));
        s.pos = 1.5;
        let _ = s.update(Msg::SetOut);
        s.pos = 0.5;
        let _ = s.update(Msg::SetIn);
        assert_eq!((s.mark_in, s.mark_out), (Some(0.5), Some(1.5)));
        let _ = s.view();
        let _ = s.update(Msg::Export);
        assert!(s.export.is_some());
        let dst = t.0.join("clip-edit.mp4");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Core::open(&dst, (8, 8), false)
            .map(|c| c.info.duration)
            .unwrap_or(0.0)
            <= 0.0
        {
            assert!(Instant::now() < deadline, "export never finished");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = s.update(Msg::Exporting(ExportEv::Progress(0.5)));
        let (_, e) = s.update(Msg::Exporting(ExportEv::Done(Ok(dst.clone()))));
        assert!(status(e).contains("clip-edit.mp4"));
        assert!(s.export.is_none());
        let _ = s.update(Msg::Snapshot);
        let (_, e) = s.update(Msg::SnapshotDone(Err("x".into())));
        assert!(status(e).starts_with("Snapshot failed"));
        let _ = s.update(Msg::ClearMarks);
        s.pos = 0.0;
        let _ = s.update(Msg::SetOut);
        let (_, e) = s.update(Msg::Export); // empty range → refused
        assert!(status(e).contains("Out must be after In"));
        assert!(!s.is_dirty());
        let _ = s.save();
        let _ = s.update(Msg::ToggleEdit);
        assert_eq!(s.is_editing(), Some(false));
        let _ = s.update(Msg::SeekDrag(0.3));
        let _ = s.update(Msg::SeekRelease);
        let _ = s.update(Msg::Step);
        let _ = s.update(Msg::Stepped(None));
        drop(s); // stops the worker
    }

    #[test]
    fn video_snapshot_png() {
        let t = Tmp::new("snap");
        let p = clip(&t);
        let out = snapshot(&p, 1.0).unwrap();
        assert_eq!(out, t.0.join("clip-frame-00m01s.png"));
        let im = image::open(&out).unwrap();
        assert_eq!((im.width(), im.height()), (W, H));
        assert_ne!(snapshot(&p, 1.0).unwrap(), out); // unique name
    }
}
