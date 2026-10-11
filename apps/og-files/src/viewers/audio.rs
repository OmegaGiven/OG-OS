//! Audio player/editor: mp3, flac, ogg/vorbis, wav, aac/m4a, alac (symphonia).
//! Opus files open (tags, info) but don't play: there is no pure-Rust Opus
//! decoder for symphonia 0.5.
//!
//! - Waveform overview: min/max peaks computed on a worker while the rest of
//!   the view is already up, drawn on a canvas with the playhead.
//! - Playback: a dedicated thread owns the decoder and the cpal stream (cpal
//!   streams aren't Send). It decodes into a small queue the device callback
//!   drains. The device is opened lazily on the first Play, at the file's
//!   sample rate when the device allows it (pipewire-alsa does, and resamples
//!   well itself); otherwise a linear resampler bridges the rates — cheap, and
//!   fine as a fallback, but it isn't band-limited.
//! - Tags via lofty. Edit mode (Ctrl+E) edits them; Save writes over the
//!   original (copy to a hidden sibling, tag that, rename over).
//! - Edit mode can also drag a selection on the waveform and export it, or a
//!   peak-normalized (-1 dBFS) copy, as a NEW `<stem>-edit.wav` (24-bit).

use super::Event;
use crate::theme;
use crate::widgets::icon;
use iced::widget::canvas::{self, Cache, Canvas, Frame, Geometry, Path as CPath, Stroke};
use iced::widget::{
    button, column, container, image, row, scrollable, slider, text, text_input, Space,
};
use iced::{mouse, Color, Element, Length, Point, Rectangle, Renderer, Size, Task, Theme};
use iced_fonts::Bootstrap;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

const EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "wav", "wave", "aac", "m4a", "m4b", "mp4a", "alac", "opus",
];
/// waveform resolution
const BUCKETS: usize = 2000;
/// max frames per fine peak block; blocks are reduced to BUCKETS at the
/// end, so files of unknown length still need only one pass
const BLOCK: usize = 256;
/// UI refresh while playing (25 Hz)
const TICK: Duration = Duration::from_millis(40);
/// -1 dBFS
const NORM_PEAK: f32 = 0.891_250_9;
const WAVE_H: f32 = 120.0;

/// Does this viewer handle `path` (by extension)?
pub fn supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTS.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Stream properties from the container/codec headers.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: Option<u32>,
    /// None when the container doesn't say (some MP3s); the waveform pass
    /// fills it in
    pub frames: Option<u64>,
}

impl Info {
    #[cfg(test)]
    pub fn duration(&self) -> Option<f64> {
        self.frames
            .map(|f| f as f64 / self.sample_rate.max(1) as f64)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub year: String,
    pub track: String,
    pub genre: String,
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Year,
    Track,
    Genre,
    Comment,
}

const FIELDS: [(Field, &str); 8] = [
    (Field::Title, "Title"),
    (Field::Artist, "Artist"),
    (Field::Album, "Album"),
    (Field::AlbumArtist, "Album artist"),
    (Field::Year, "Year"),
    (Field::Track, "Track"),
    (Field::Genre, "Genre"),
    (Field::Comment, "Comment"),
];

impl Tags {
    fn get(&self, f: Field) -> &str {
        match f {
            Field::Title => &self.title,
            Field::Artist => &self.artist,
            Field::Album => &self.album,
            Field::AlbumArtist => &self.album_artist,
            Field::Year => &self.year,
            Field::Track => &self.track,
            Field::Genre => &self.genre,
            Field::Comment => &self.comment,
        }
    }

    fn get_mut(&mut self, f: Field) -> &mut String {
        match f {
            Field::Title => &mut self.title,
            Field::Artist => &mut self.artist,
            Field::Album => &mut self.album,
            Field::AlbumArtist => &mut self.album_artist,
            Field::Year => &mut self.year,
            Field::Track => &mut self.track,
            Field::Genre => &mut self.genre,
            Field::Comment => &mut self.comment,
        }
    }
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    /// None if symphonia can't read the container at all
    pub info: Option<Info>,
    /// why it can't play (e.g. "opus: not supported"), if it can't
    pub error: Option<String>,
    pub tags: Tags,
    /// cover art thumbnail (≤128 px)
    pub cover: Option<image::Handle>,
    /// tags couldn't be read (lofty error); tag editing is disabled
    pub tag_error: Option<String>,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    tokio::task::spawn_blocking(move || open_blocking(path))
        .await
        .map_err(|e| e.to_string())?
}

fn open_blocking(path: PathBuf) -> Result<Opened, String> {
    let probed = probe_info(&path);
    let (tags, cover, tag_error) = match read_tags(&path) {
        Ok((t, c)) => (t, c.and_then(|b| cover_thumb(&b)), None),
        Err(e) => (Tags::default(), None, Some(e)),
    };
    let (info, error) = match probed {
        Ok((info, unplayable)) => (Some(info), unplayable),
        Err(e) if tag_error.is_some() => return Err(e),
        Err(e) => (None, Some(e)),
    };
    Ok(Opened {
        path,
        info,
        error,
        tags,
        cover,
        tag_error,
    })
}

// ------------------------------------------------------------- decoding ----

fn ts_to_frames(tb: Option<TimeBase>, ts: u64, rate: u32) -> u64 {
    match tb {
        Some(tb) => {
            let t = tb.calc_time(ts);
            ((t.seconds as f64 + t.frac) * rate as f64).round() as u64
        }
        None => ts,
    }
}

fn probe(path: &Path) -> Result<Box<dyn FormatReader>, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(e);
    }
    symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map(|p| p.format)
        .map_err(|e| format!("unrecognized audio: {e}"))
}

/// Header info, plus Some(reason) if no decoder exists for the codec.
fn probe_info(path: &Path) -> Result<(Info, Option<String>), String> {
    let format = probe(path)?;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let p = &track.codec_params;
    let codecs = symphonia::default::get_codecs();
    let codec = match codecs.get_codec(p.codec) {
        Some(d) => d.short_name.to_uppercase(),
        None if p.codec == CODEC_TYPE_OPUS => "OPUS".into(),
        None => "unknown".into(),
    };
    let unplayable = match codecs.make(p, &DecoderOptions::default()) {
        Ok(_) => None,
        Err(_) if p.codec == CODEC_TYPE_OPUS => {
            Some("opus: not supported (no pure-Rust Opus decoder)".to_string())
        }
        Err(e) => Some(format!("{codec}: not supported ({e})")),
    };
    let rate = p.sample_rate.unwrap_or(0);
    let frames = p.n_frames.map(|n| {
        // n_frames is in timebase units for some containers
        if p.time_base
            .is_some_and(|tb| tb.denom != rate || tb.numer != 1)
        {
            ts_to_frames(p.time_base, n, rate)
        } else {
            n
        }
    });
    Ok((
        Info {
            codec,
            sample_rate: rate,
            channels: p.channels.map(|c| c.count() as u16).unwrap_or(0),
            bits: p.bits_per_sample.or(p.bits_per_coded_sample),
            frames,
        },
        unplayable,
    ))
}

/// Interleaved samples, channel count, sample rate.
type Chunk<'a> = (&'a [f32], usize, u32);

/// A demuxer + decoder producing interleaved f32.
struct Dec {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track: u32,
    tb: Option<TimeBase>,
    rate: u32,
    ch: usize,
    sbuf: Option<(SignalSpec, SampleBuffer<f32>)>,
}

impl Dec {
    fn open(path: &Path) -> Result<Dec, String> {
        let format = probe(path)?;
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("no audio track")?;
        let p = &track.codec_params;
        let decoder = symphonia::default::get_codecs()
            .make(p, &DecoderOptions::default())
            .map_err(|e| {
                if p.codec == CODEC_TYPE_OPUS {
                    "opus: not supported".to_string()
                } else {
                    e.to_string()
                }
            })?;
        Ok(Dec {
            track: track.id,
            tb: p.time_base,
            rate: p.sample_rate.unwrap_or(44_100),
            ch: p.channels.map(|c| c.count()).unwrap_or(2).max(1),
            decoder,
            format,
            sbuf: None,
        })
    }

    /// Next decoded chunk (interleaved, `self.ch` channels), None at the end.
    fn next(&mut self) -> Result<Option<Chunk<'_>>, String> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None)
                }
                Err(SymError::ResetRequired) => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buf) => {
                    let spec = *buf.spec();
                    let n = buf.frames() * spec.channels.count();
                    if n == 0 {
                        continue;
                    }
                    let fresh = match &self.sbuf {
                        Some((s, sb)) => *s != spec || sb.capacity() < n,
                        None => true,
                    };
                    if fresh {
                        self.sbuf = Some((spec, SampleBuffer::new(buf.capacity() as u64, spec)));
                    }
                    self.rate = spec.rate;
                    self.ch = spec.channels.count();
                    let (_, sb) = self.sbuf.as_mut().expect("just set");
                    sb.copy_interleaved_ref(buf);
                    return Ok(Some((sb.samples(), self.ch, self.rate)));
                }
                // corrupt packet: skip it, like every player does
                Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    /// Seek to `frame`; returns how many decoded frames to drop afterwards to
    /// land on it exactly.
    fn seek(&mut self, frame: u64) -> Result<u64, String> {
        let time = Time::from(frame as f64 / self.rate.max(1) as f64);
        let s = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time,
                    track_id: Some(self.track),
                },
            )
            .map_err(|e| e.to_string())?;
        self.decoder.reset();
        Ok(ts_to_frames(
            self.tb,
            s.required_ts.saturating_sub(s.actual_ts),
            self.rate,
        ))
    }
}

/// Decode every frame, calling `f(chunk, channels)`. Returns (frames, rate,
/// channels). `cancel` stops early with an error.
fn decode_all(
    path: &Path,
    cancel: &AtomicBool,
    mut f: impl FnMut(&[f32], usize),
) -> Result<(u64, u32, usize), String> {
    let mut dec = Dec::open(path)?;
    let mut frames = 0u64;
    while let Some((s, ch, _)) = dec.next()? {
        if cancel.load(Relaxed) {
            return Err("cancelled".into());
        }
        frames += (s.len() / ch) as u64;
        f(s, ch);
    }
    Ok((frames, dec.rate, dec.ch))
}

// ---------------------------------------------------------------- peaks ----

/// Waveform overview: per-bucket (min, max) over all channels.
#[derive(Clone)]
pub struct Peaks {
    pub buckets: Vec<(f32, f32)>,
    pub frames: u64,
    pub rate: u32,
    pub channels: usize,
}

impl std::fmt::Debug for Peaks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Peaks({} buckets, {} frames @ {} Hz, {} ch)",
            self.buckets.len(),
            self.frames,
            self.rate,
            self.channels
        )
    }
}

fn compute_peaks(path: &Path, buckets: usize, cancel: &AtomicBool) -> Result<Peaks, String> {
    // ≥2 blocks per bucket when the length is known (short files)
    let block = match probe_info(path).ok().and_then(|(i, _)| i.frames) {
        Some(f) => (f as usize / (buckets * 2).max(1)).clamp(1, BLOCK),
        None => BLOCK,
    };
    let mut blocks: Vec<(f32, f32)> = Vec::new();
    let (mut lo, mut hi, mut n) = (f32::MAX, f32::MIN, 0usize);
    let (frames, rate, channels) = decode_all(path, cancel, |s, ch| {
        for fr in s.chunks_exact(ch) {
            for &v in fr {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            n += 1;
            if n == block {
                blocks.push((lo, hi));
                (lo, hi, n) = (f32::MAX, f32::MIN, 0);
            }
        }
    })?;
    if n > 0 {
        blocks.push((lo, hi));
    }
    Ok(Peaks {
        buckets: reduce_peaks(&blocks, buckets),
        frames,
        rate,
        channels,
    })
}

/// Merge fine (min, max) blocks down to at most `n` buckets.
fn reduce_peaks(blocks: &[(f32, f32)], n: usize) -> Vec<(f32, f32)> {
    if blocks.len() <= n {
        return blocks.to_vec();
    }
    (0..n)
        .map(|i| {
            let a = i * blocks.len() / n;
            let b = ((i + 1) * blocks.len() / n).max(a + 1);
            blocks[a..b]
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, h), &(x, y)| (l.min(x), h.max(y)))
        })
        .collect()
}

// --------------------------------------------------------------- export ----

#[derive(Debug, Clone, PartialEq)]
struct Exported {
    path: PathBuf,
    frames: u64,
    gain_db: f32,
}

/// Write frames `range` (all if None) of `src` to a new `<stem>-edit.wav`
/// beside it, peak-normalized to -1 dBFS if `normalize`. Never overwrites.
fn export_wav(
    src: &Path,
    range: Option<(u64, u64)>,
    normalize: bool,
    cancel: &AtomicBool,
) -> Result<Exported, String> {
    let (a, b) = range.unwrap_or((0, u64::MAX));
    if b <= a {
        return Err("empty selection".into());
    }
    // frames [a, b) of a chunk starting at absolute frame `at`
    let window = |at: u64, len: u64| -> (usize, usize) {
        let s = a.saturating_sub(at).min(len);
        let e = b.saturating_sub(at).min(len);
        (s as usize, e.max(s) as usize)
    };
    let gain = if normalize {
        let mut peak = 0f32;
        let mut at = 0u64;
        decode_all(src, cancel, |s, ch| {
            let len = (s.len() / ch) as u64;
            let (s0, s1) = window(at, len);
            for v in &s[s0 * ch..s1 * ch] {
                peak = peak.max(v.abs());
            }
            at += len;
        })?;
        if peak <= 0.0 {
            return Err("selection is silent".into());
        }
        NORM_PEAK / peak
    } else {
        1.0
    };

    let dir = src.parent().ok_or("no parent folder")?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("audio");
    let out = crate::ops::unique_name(dir, &format!("{stem}-edit.wav"));
    let mut dec = Dec::open(src)?;
    let spec = hound::WavSpec {
        channels: dec.ch as u16,
        sample_rate: dec.rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut write = || -> Result<u64, String> {
        let mut w: Option<hound::WavWriter<_>> = None;
        let (mut at, mut written) = (0u64, 0u64);
        let max = 8_388_607.0f32;
        while let Some((s, ch, rate)) = dec.next()? {
            if cancel.load(Relaxed) {
                return Err("cancelled".into());
            }
            if w.is_none() {
                // spec from the first real chunk (AAC may only know then)
                let spec = hound::WavSpec {
                    channels: ch as u16,
                    sample_rate: rate,
                    ..spec
                };
                w = Some(hound::WavWriter::create(&out, spec).map_err(|e| e.to_string())?);
            }
            let wr = w.as_mut().expect("just set");
            let len = (s.len() / ch) as u64;
            let (s0, s1) = window(at, len);
            for v in &s[s0 * ch..s1 * ch] {
                let q = (v * gain * max).round().clamp(-max - 1.0, max) as i32;
                wr.write_sample(q).map_err(|e| e.to_string())?;
            }
            written += (s1 - s0) as u64;
            at += len;
            if at >= b {
                break;
            }
        }
        match w {
            Some(w) => w.finalize().map_err(|e| e.to_string())?,
            None => return Err("no audio decoded".into()),
        }
        Ok(written)
    };
    match write() {
        Ok(frames) => Ok(Exported {
            path: out,
            frames,
            gain_db: 20.0 * gain.log10(),
        }),
        Err(e) => {
            let _ = std::fs::remove_file(&out);
            Err(e)
        }
    }
}

// ----------------------------------------------------------------- tags ----

/// Read by content, not extension (the save temp file has none that lofty
/// knows).
fn lofty_read(path: &Path) -> Result<lofty::file::TaggedFile, String> {
    let probe = lofty::probe::Probe::open(path)
        .map_err(|e| e.to_string())?
        .guess_file_type()
        .map_err(|e| e.to_string())?;
    probe.read().map_err(|e| e.to_string())
}

fn read_tags(path: &Path) -> Result<(Tags, Option<Vec<u8>>), String> {
    use lofty::picture::PictureType;
    use lofty::prelude::*;
    let tf = lofty_read(path)?;
    let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) else {
        return Ok((Tags::default(), None));
    };
    let s = |v: Option<std::borrow::Cow<'_, str>>| v.map(|c| c.into_owned()).unwrap_or_default();
    let tags = Tags {
        title: s(tag.title()),
        artist: s(tag.artist()),
        album: s(tag.album()),
        album_artist: tag
            .get_string(&ItemKey::AlbumArtist)
            .unwrap_or_default()
            .to_string(),
        year: tag.year().map(|y| y.to_string()).unwrap_or_default(),
        track: tag.track().map(|t| t.to_string()).unwrap_or_default(),
        genre: s(tag.genre()),
        comment: s(tag.comment()),
    };
    let pics = tag.pictures();
    let cover = pics
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or(pics.first())
        .map(|p| p.data().to_vec());
    Ok((tags, cover))
}

fn cover_thumb(bytes: &[u8]) -> Option<image::Handle> {
    let img = ::image::load_from_memory(bytes)
        .ok()?
        .thumbnail(128, 128)
        .to_rgba8();
    let (w, h) = img.dimensions();
    Some(image::Handle::from_rgba(w, h, img.into_raw()))
}

fn parse_num(s: &str, what: &str) -> Result<Option<u32>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    s.parse()
        .map(Some)
        .map_err(|_| format!("{what} must be a number"))
}

/// Write `t` into the file's primary tag: copy to a hidden sibling, tag the
/// copy, rename it over the original (a crash leaves the original intact).
fn write_tags(path: &Path, t: &Tags) -> Result<(), String> {
    use lofty::config::WriteOptions;
    use lofty::prelude::*;
    use lofty::tag::Tag;
    let year = parse_num(&t.year, "Year")?;
    let track = parse_num(&t.track, "Track")?;
    let tmp = path.with_file_name(format!(
        ".{}.og-files.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("audio")
    ));
    std::fs::copy(path, &tmp).map_err(|e| e.to_string())?;
    let tag_it = || -> Result<(), String> {
        let mut tf = lofty_read(&tmp)?;
        if tf.primary_tag_mut().is_none() {
            let tt = tf.primary_tag_type();
            tf.insert_tag(Tag::new(tt));
        }
        let tag = tf.primary_tag_mut().ok_or("can't create a tag")?;
        let text = |v: &str| (!v.trim().is_empty()).then(|| v.trim().to_string());
        match text(&t.title) {
            Some(v) => tag.set_title(v),
            None => tag.remove_title(),
        }
        match text(&t.artist) {
            Some(v) => tag.set_artist(v),
            None => tag.remove_artist(),
        }
        match text(&t.album) {
            Some(v) => tag.set_album(v),
            None => tag.remove_album(),
        }
        match text(&t.album_artist) {
            Some(v) => {
                tag.insert_text(ItemKey::AlbumArtist, v);
            }
            None => tag.remove_key(&ItemKey::AlbumArtist),
        }
        match text(&t.genre) {
            Some(v) => tag.set_genre(v),
            None => tag.remove_genre(),
        }
        match text(&t.comment) {
            Some(v) => tag.set_comment(v),
            None => tag.remove_comment(),
        }
        match year {
            Some(v) => tag.set_year(v),
            None => tag.remove_year(),
        }
        match track {
            Some(v) => tag.set_track(v),
            None => tag.remove_track(),
        }
        tag.save_to_path(&tmp, WriteOptions::default())
            .map_err(|e| e.to_string())
    };
    match tag_it().and_then(|()| std::fs::rename(&tmp, path).map_err(|e| e.to_string())) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

// ------------------------------------------------------------ resampler ----

/// Streaming linear-interpolation resampler for interleaved f32. Cheap and
/// click-free across chunk boundaries; not band-limited (only used when the
/// device can't run at the file's rate).
struct Linear {
    ch: usize,
    /// input frames per output frame
    step: f64,
    /// position of the next output frame, in input frames relative to the
    /// current chunk (-1 = last frame of the previous chunk)
    t: f64,
    last: Vec<f32>,
    primed: bool,
}

impl Linear {
    fn new(from: u32, to: u32, ch: usize) -> Self {
        Self {
            ch: ch.max(1),
            step: from as f64 / to as f64,
            t: 0.0,
            last: vec![0.0; ch.max(1)],
            primed: false,
        }
    }

    fn reset(&mut self) {
        self.t = 0.0;
        self.primed = false;
    }

    fn process(&mut self, inp: &[f32], out: &mut Vec<f32>) {
        let ch = self.ch;
        let n = (inp.len() / ch) as isize;
        if n == 0 {
            return;
        }
        let at = |i: isize, c: usize| {
            if i < 0 {
                self.last[c]
            } else {
                inp[i as usize * ch + c]
            }
        };
        while (self.t.floor() as isize) < n - 1 {
            let i0 = self.t.floor() as isize;
            let fr = (self.t - i0 as f64) as f32;
            for c in 0..ch {
                out.push(at(i0, c) * (1.0 - fr) + at(i0 + 1, c) * fr);
            }
            self.t += self.step;
        }
        self.last
            .copy_from_slice(&inp[(n as usize - 1) * ch..n as usize * ch]);
        self.t -= n as f64;
        self.primed = true;
    }
}

/// Map interleaved `inp` (src channels) onto `dst` channels: equal → copy,
/// to mono → average, mono → duplicate, otherwise wrap (FL/FR first).
fn map_channels(inp: &[f32], src: usize, dst: usize, out: &mut Vec<f32>) {
    out.clear();
    if src == dst {
        out.extend_from_slice(inp);
        return;
    }
    for fr in inp.chunks_exact(src) {
        if dst == 1 {
            out.push(fr.iter().sum::<f32>() / src as f32);
        } else {
            out.extend((0..dst).map(|c| fr[c % src]));
        }
    }
}

// --------------------------------------------------------------- player ----

/// Audio queue shared by the decode thread and the device callback.
struct Buf {
    q: VecDeque<f32>,
    /// device channels
    ch: usize,
    /// source frame the queue started at (after the last seek)
    base: u64,
    /// device frames played since `base`
    played: u64,
    /// source frames per device frame
    ratio: f64,
    /// decoder hit the end; queue holds the tail
    eof: bool,
    /// tail fully played
    ended: bool,
    /// total length, once known (positions wrap at it when looping)
    len: Option<u64>,
}

struct Shared {
    buf: Mutex<Buf>,
    playing: AtomicBool,
    quit: AtomicBool,
    looping: AtomicBool,
    /// f32 bits, 0..1 (slider value; gain is its square)
    volume: AtomicU32,
    /// commands sent but not yet handled
    pending: AtomicU32,
    /// bumps on every Play so only the newest tick stream survives
    tick_gen: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn buf(&self) -> MutexGuard<'_, Buf> {
        self.buf.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn fail(&self, e: String) {
        self.playing.store(false, Relaxed);
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
    }

    fn position(&self) -> u64 {
        let b = self.buf();
        let p = b.base + (b.played as f64 * b.ratio) as u64;
        match b.len {
            Some(l) if l > 0 && p >= l => {
                if b.ended {
                    l
                } else {
                    p % l
                }
            }
            _ => p,
        }
    }

    /// still busy (playing, or a command in flight)
    fn live(&self) -> bool {
        self.playing.load(Relaxed) || self.pending.load(Relaxed) > 0
    }
}

enum Cmd {
    Play,
    Pause,
    Seek(u64),
}

/// Handle to the playback thread. Dropping it stops playback.
struct Player {
    tx: mpsc::Sender<Cmd>,
    sh: Arc<Shared>,
}

impl Player {
    fn spawn(path: PathBuf, volume: f32, looping: bool) -> Player {
        let sh = Arc::new(Shared {
            buf: Mutex::new(Buf {
                q: VecDeque::new(),
                ch: 2,
                base: 0,
                played: 0,
                ratio: 1.0,
                eof: false,
                ended: false,
                len: None,
            }),
            playing: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            looping: AtomicBool::new(looping),
            volume: AtomicU32::new(volume.to_bits()),
            pending: AtomicU32::new(0),
            tick_gen: AtomicU64::new(0),
            error: Mutex::new(None),
        });
        let (tx, rx) = mpsc::channel();
        let sh2 = sh.clone();
        let spawned = std::thread::Builder::new()
            .name("ogx-audio".into())
            .spawn(move || Worker::run(path, sh2, rx));
        if let Err(e) = spawned {
            sh.fail(e.to_string());
        }
        Player { tx, sh }
    }

    fn send(&self, c: Cmd) {
        self.sh.pending.fetch_add(1, Relaxed);
        if self.tx.send(c).is_err() {
            self.sh.pending.fetch_sub(1, Relaxed);
        }
    }

    fn play(&self) {
        self.sh.playing.store(true, Relaxed);
        self.send(Cmd::Play);
    }

    fn pause(&self) {
        self.sh.playing.store(false, Relaxed);
        self.send(Cmd::Pause);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // the callback goes silent at once; the thread exits on its next
        // wake-up (≤10 ms, or immediately via the closed channel) and drops
        // the stream
        self.sh.quit.store(true, Relaxed);
        self.sh.playing.store(false, Relaxed);
    }
}

struct Out {
    stream: cpal::Stream,
    rate: u32,
    ch: usize,
    /// queue target in samples (~250 ms)
    target: usize,
}

fn open_output(src_rate: u32, sh: &Arc<Shared>) -> Result<Out, String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    use cpal::SampleFormat as F;
    let host = cpal::default_host();
    let dev = host
        .default_output_device()
        .ok_or("no audio output device")?;
    let fmt_score = |f: F| match f {
        F::F32 => 3,
        F::I32 | F::I16 => 2,
        F::U16 | F::F64 => 1,
        _ => 0,
    };
    let best = dev
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|r| fmt_score(r.sample_format()) > 0)
        .max_by_key(|r| {
            let rate_ok = (r.min_sample_rate()..=r.max_sample_rate()).contains(&src_rate);
            (rate_ok as u8) * 8 + (r.channels() == 2) as u8 * 4 + fmt_score(r.sample_format())
        });
    let (cfg, format) = match best {
        Some(r) => {
            let (lo, hi) = (r.min_sample_rate(), r.max_sample_rate());
            let rate = if (lo..=hi).contains(&src_rate) {
                src_rate
            } else {
                48_000u32.clamp(lo, hi)
            };
            let f = r.sample_format();
            (r.with_sample_rate(rate).config(), f)
        }
        None => {
            let d = dev.default_output_config().map_err(|e| e.to_string())?;
            (d.config(), d.sample_format())
        }
    };
    let stream = match format {
        F::F32 => build::<f32>(&dev, &cfg, sh),
        F::F64 => build::<f64>(&dev, &cfg, sh),
        F::I32 => build::<i32>(&dev, &cfg, sh),
        F::I16 => build::<i16>(&dev, &cfg, sh),
        F::U16 => build::<u16>(&dev, &cfg, sh),
        f => Err(format!("unsupported device sample format {f}")),
    }?;
    let ch = cfg.channels.max(1) as usize;
    Ok(Out {
        stream,
        rate: cfg.sample_rate,
        ch,
        target: cfg.sample_rate as usize * ch / 4,
    })
}

fn build<T>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    sh: &Arc<Shared>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    use cpal::traits::DeviceTrait;
    let sh = sh.clone();
    let err_sh = sh.clone();
    dev.build_output_stream(
        cfg.clone(),
        move |data: &mut [T], _| fill(data, &sh),
        move |e| err_sh.fail(format!("audio device: {e}")),
        None,
    )
    .map_err(|e| e.to_string())
}

/// Device callback: drain the queue with volume applied, silence otherwise.
fn fill<T: cpal::Sample + cpal::FromSample<f32>>(data: &mut [T], sh: &Shared) {
    let silent = |d: &mut [T]| d.iter_mut().for_each(|s| *s = T::EQUILIBRIUM);
    if sh.quit.load(Relaxed) || !sh.playing.load(Relaxed) {
        return silent(data);
    }
    let v = f32::from_bits(sh.volume.load(Relaxed));
    let gain = v * v;
    let mut b = sh.buf();
    let ch = b.ch.max(1);
    let frames = data.len() / ch;
    let n = frames.min(b.q.len() / ch);
    for (s, v) in data[..n * ch].iter_mut().zip(b.q.drain(..n * ch)) {
        *s = T::from_sample(v * gain);
    }
    silent(&mut data[n * ch..]);
    b.played += n as u64;
    if n < frames && b.eof && b.q.is_empty() {
        b.ended = true;
    }
}

/// The playback thread: owns the decoder and the (non-Send) cpal stream.
struct Worker {
    path: PathBuf,
    sh: Arc<Shared>,
    dec: Dec,
    out: Option<Out>,
    rs: Option<Linear>,
    /// frames still to drop after an accurate seek
    skip: u64,
    /// source position of the decoder (frames)
    at: u64,
    eof: bool,
    mapped: Vec<f32>,
    res: Vec<f32>,
}

impl Worker {
    fn run(path: PathBuf, sh: Arc<Shared>, rx: mpsc::Receiver<Cmd>) {
        let dec = match Dec::open(&path) {
            Ok(d) => d,
            Err(e) => {
                sh.pending.store(0, Relaxed);
                return sh.fail(e);
            }
        };
        let mut w = Worker {
            path,
            sh,
            dec,
            out: None,
            rs: None,
            skip: 0,
            at: 0,
            eof: false,
            mapped: Vec::new(),
            res: Vec::new(),
        };
        let mut running = false;
        loop {
            // handle every queued command first
            loop {
                match rx.try_recv() {
                    Ok(c) => {
                        w.handle(c);
                        w.sh.pending.fetch_sub(1, Relaxed);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            if w.sh.quit.load(Relaxed) {
                return;
            }
            let ended = w.sh.buf().ended;
            if ended && w.sh.pending.load(Relaxed) == 0 {
                w.sh.playing.store(false, Relaxed);
            }
            let playing = w.sh.playing.load(Relaxed);
            if let Some(o) = &w.out {
                use cpal::traits::StreamTrait;
                if playing && !running {
                    if let Err(e) = o.stream.play() {
                        w.sh.fail(e.to_string());
                    }
                    running = true;
                } else if !playing && running {
                    let _ = o.stream.pause();
                    running = false;
                }
            }
            let hungry = playing
                && !w.eof
                && w.out
                    .as_ref()
                    .is_some_and(|o| w.sh.buf().q.len() < o.target);
            if hungry {
                w.decode_step();
                continue;
            }
            // nothing to do: wait for a command (bounded while playing so
            // the queue gets topped up; unbounded while paused — no polling)
            let got = if playing {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(c) => Some(c),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(c) => Some(c),
                    Err(_) => return,
                }
            };
            if let Some(c) = got {
                w.handle(c);
                w.sh.pending.fetch_sub(1, Relaxed);
            }
        }
    }

    fn handle(&mut self, c: Cmd) {
        match c {
            Cmd::Play => {
                if self.out.is_none() {
                    match open_output(self.dec.rate, &self.sh) {
                        Ok(o) => {
                            self.rs = (o.rate != self.dec.rate)
                                .then(|| Linear::new(self.dec.rate, o.rate, o.ch));
                            let mut b = self.sh.buf();
                            b.ch = o.ch;
                            b.ratio = self.dec.rate as f64 / o.rate as f64;
                            drop(b);
                            self.out = Some(o);
                        }
                        Err(e) => return self.sh.fail(e),
                    }
                }
                if self.sh.buf().ended {
                    self.seek(0);
                }
                self.sh.playing.store(true, Relaxed);
            }
            Cmd::Pause => self.sh.playing.store(false, Relaxed),
            Cmd::Seek(f) => self.seek(f),
        }
    }

    fn seek(&mut self, frame: u64) {
        let skip = match self.dec.seek(frame) {
            Ok(s) => s,
            // some streams can't seek back to the start: reopen instead
            Err(_) if frame == 0 => match Dec::open(&self.path) {
                Ok(d) => {
                    self.dec = d;
                    0
                }
                Err(e) => return self.sh.fail(e),
            },
            Err(e) => return self.sh.fail(format!("seek failed: {e}")),
        };
        self.skip = skip;
        self.at = frame;
        self.eof = false;
        if let Some(r) = &mut self.rs {
            r.reset();
        }
        let mut b = self.sh.buf();
        b.q.clear();
        b.base = frame;
        b.played = 0;
        b.eof = false;
        b.ended = false;
    }

    fn decode_step(&mut self) {
        let Some(out_ch) = self.out.as_ref().map(|o| o.ch) else {
            return;
        };
        match self.dec.next() {
            Ok(Some((s, ch, _))) => {
                let frames = (s.len() / ch) as u64;
                let drop_n = self.skip.min(frames);
                self.skip -= drop_n;
                self.at += frames - drop_n;
                let s = &s[drop_n as usize * ch..];
                if s.is_empty() {
                    return;
                }
                map_channels(s, ch, out_ch, &mut self.mapped);
                let data = match &mut self.rs {
                    Some(r) => {
                        self.res.clear();
                        r.process(&self.mapped, &mut self.res);
                        &self.res
                    }
                    None => &self.mapped,
                };
                self.sh.buf().q.extend(data.iter().copied());
            }
            Ok(None) => {
                let len = self.at;
                self.sh.buf().len.get_or_insert(len);
                if self.sh.looping.load(Relaxed) && len > 0 {
                    // keep the queued tail; continue from the top (the
                    // position wraps at `len`)
                    if self.dec.seek(0).is_err() {
                        match Dec::open(&self.path) {
                            Ok(d) => self.dec = d,
                            Err(e) => return self.sh.fail(e),
                        }
                    }
                    self.skip = 0;
                    self.at = 0;
                } else {
                    self.eof = true;
                    self.sh.buf().eof = true;
                }
            }
            Err(e) => {
                self.eof = true;
                self.sh.buf().eof = true;
                self.sh.fail(e);
            }
        }
    }
}

/// Emits Tick at 25 Hz while this generation is playing, then one last Tick.
fn tick_task(sh: Arc<Shared>, gen: u64) -> Task<Msg> {
    let stream = iced::stream::channel(1, move |mut out| async move {
        use iced::futures::SinkExt;
        loop {
            tokio::time::sleep(TICK).await;
            let live = sh.live() && sh.tick_gen.load(Relaxed) == gen && !sh.quit.load(Relaxed);
            if out.send(Msg::Tick).await.is_err() || !live {
                return;
            }
        }
    });
    Task::run(stream, |m| m)
}

// ------------------------------------------------------------------- ui ----

pub struct State {
    path: PathBuf,
    info: Option<Info>,
    error: Option<String>,
    tags: Tags,
    orig: Tags,
    cover: Option<image::Handle>,
    tag_error: Option<String>,
    peaks: Option<Arc<Peaks>>,
    peaks_err: Option<String>,
    wave: Cache,
    player: Option<Player>,
    playing: bool,
    /// playhead, source frames
    pos: u64,
    /// playhead preview while dragging on the waveform (0..1)
    scrub: Option<f32>,
    volume: f32,
    looping: bool,
    editing: bool,
    saving: bool,
    busy: bool,
    /// edit-mode selection, fractions of the length (ordered)
    sel: Option<(f32, f32)>,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Peaks(Result<Arc<Peaks>, String>),
    PlayPause,
    Tick,
    Scrub(f32),
    Seek(f32),
    Select(f32, f32),
    ClearSelection,
    Volume(f32),
    ToggleLoop,
    ToggleEdit,
    Field(Field, String),
    Save,
    Saved(Result<Tags, String>),
    Discard,
    Export(bool),
    Exported(Result<String, String>),
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let task = if o.error.is_none() && o.info.is_some() {
            let (p, c) = (o.path.clone(), cancel.clone());
            Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || compute_peaks(&p, BUCKETS, &c))
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r.map(Arc::new))
                },
                Msg::Peaks,
            )
        } else {
            Task::none()
        };
        (
            Self {
                path: o.path,
                info: o.info,
                error: o.error,
                orig: o.tags.clone(),
                tags: o.tags,
                cover: o.cover,
                tag_error: o.tag_error,
                peaks: None,
                peaks_err: None,
                wave: Cache::new(),
                player: None,
                playing: false,
                pos: 0,
                scrub: None,
                volume: 0.8,
                looping: false,
                editing: false,
                saving: false,
                busy: false,
                sel: None,
                cancel,
            },
            task,
        )
    }

    fn rate(&self) -> u32 {
        self.peaks
            .as_ref()
            .map(|p| p.rate)
            .or(self.info.as_ref().map(|i| i.sample_rate))
            .unwrap_or(0)
            .max(1)
    }

    fn total(&self) -> Option<u64> {
        self.peaks
            .as_ref()
            .map(|p| p.frames)
            .or(self.info.as_ref().and_then(|i| i.frames))
            .filter(|&f| f > 0)
    }

    fn playable(&self) -> bool {
        self.error.is_none() && self.info.is_some()
    }

    fn frac(&self) -> f32 {
        if let Some(s) = self.scrub {
            return s;
        }
        match self.total() {
            Some(t) => (self.pos as f64 / t as f64).clamp(0.0, 1.0) as f32,
            None => 0.0,
        }
    }

    fn sel_frames(&self) -> Option<(u64, u64)> {
        let t = self.total()? as f64;
        self.sel
            .map(|(a, b)| ((a as f64 * t) as u64, (b as f64 * t) as u64))
            .filter(|(a, b)| b > a)
    }

    fn stop(&mut self) {
        self.cancel.store(true, Relaxed);
        self.player = None;
        self.playing = false;
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Peaks(r) => {
                match r {
                    Ok(p) => self.peaks = Some(p),
                    Err(e) => self.peaks_err = Some(e),
                }
                self.wave.clear();
                none
            }
            Msg::PlayPause => {
                if !self.playable() {
                    return none;
                }
                if self.playing {
                    if let Some(p) = &self.player {
                        p.pause();
                    }
                    self.playing = false;
                    return none;
                }
                let p = self.player.get_or_insert_with(|| {
                    let p = Player::spawn(self.path.clone(), self.volume, self.looping);
                    if self.pos > 0 {
                        p.send(Cmd::Seek(self.pos));
                    }
                    p
                });
                p.play();
                let gen = p.sh.tick_gen.fetch_add(1, Relaxed) + 1;
                self.playing = true;
                (tick_task(p.sh.clone(), gen), None)
            }
            Msg::Tick => {
                let Some(p) = &self.player else {
                    return none;
                };
                if self.scrub.is_none() {
                    self.pos = p.sh.position();
                }
                if !p.sh.live() {
                    self.playing = false;
                }
                let err = p.sh.error.lock().ok().and_then(|mut e| e.take());
                (
                    Task::none(),
                    err.map(|e| Event::Status(format!("Playback: {e}"))),
                )
            }
            Msg::Scrub(f) => {
                self.scrub = Some(f.clamp(0.0, 1.0));
                none
            }
            Msg::Seek(f) => {
                self.scrub = None;
                if self.editing {
                    self.sel = None;
                }
                let Some(t) = self.total() else {
                    return none;
                };
                self.pos = (f.clamp(0.0, 1.0) as f64 * t as f64) as u64;
                if let Some(p) = &self.player {
                    p.send(Cmd::Seek(self.pos));
                }
                none
            }
            Msg::Select(a, b) => {
                let (a, b) = (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
                self.sel = Some((a.min(b), a.max(b)));
                none
            }
            Msg::ClearSelection => {
                self.sel = None;
                none
            }
            Msg::Volume(v) => {
                self.volume = v;
                if let Some(p) = &self.player {
                    p.sh.volume.store(v.to_bits(), Relaxed);
                }
                none
            }
            Msg::ToggleLoop => {
                self.looping = !self.looping;
                if let Some(p) = &self.player {
                    p.sh.looping.store(self.looping, Relaxed);
                }
                none
            }
            Msg::ToggleEdit => {
                if self.editing && self.is_dirty() {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Unsaved tag changes: Save or Discard first".into(),
                        )),
                    );
                }
                self.editing = !self.editing;
                if !self.editing {
                    self.sel = None;
                }
                none
            }
            Msg::Field(f, v) => {
                if self.editing {
                    *self.tags.get_mut(f) = v;
                }
                none
            }
            Msg::Save => (self.save(), None),
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(t) => {
                        self.orig = t;
                        (
                            Task::none(),
                            Some(Event::Status(format!(
                                "Saved tags to {}",
                                self.path.display()
                            ))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Save failed: {e}"))),
                    ),
                }
            }
            Msg::Discard => {
                self.tags = self.orig.clone();
                (
                    Task::none(),
                    Some(Event::Status("Changes discarded".into())),
                )
            }
            Msg::Export(normalize) => {
                if self.busy || !self.playable() {
                    return none;
                }
                let range = self.sel_frames();
                if !normalize && range.is_none() {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Drag on the waveform to select a range first".into(),
                        )),
                    );
                }
                self.busy = true;
                let (src, c) = (self.path.clone(), self.cancel.clone());
                let rate = self.rate() as f64;
                let task = Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || export_wav(&src, range, normalize, &c))
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r)
                            .map(|x| {
                                let name = x
                                    .path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                let secs = x.frames as f64 / rate;
                                if normalize {
                                    format!(
                                        "Normalized ({:+.1} dB, {}) → {name}",
                                        x.gain_db,
                                        fmt_time(secs, true)
                                    )
                                } else {
                                    format!("Exported {} → {name}", fmt_time(secs, true))
                                }
                            })
                    },
                    Msg::Exported,
                );
                (
                    task,
                    Some(Event::Status(if normalize {
                        "Normalizing…".into()
                    } else {
                        "Exporting selection…".into()
                    })),
                )
            }
            Msg::Exported(r) => {
                self.busy = false;
                (
                    Task::none(),
                    Some(Event::Status(match r {
                        Ok(s) => s,
                        Err(e) => format!("Export failed: {e}"),
                    })),
                )
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let small = |s: String| text(s).size(12).color(theme::MUTED);

        // header: cover, title, artist — album, stream info
        let title = if self.tags.title.trim().is_empty() {
            self.path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            self.tags.title.clone()
        };
        let sub = [self.tags.artist.trim(), self.tags.album.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" — ");
        let mut info_line = Vec::new();
        if let Some(i) = &self.info {
            info_line.push(i.codec.clone());
            if i.sample_rate > 0 {
                info_line.push(format!("{:.1} kHz", i.sample_rate as f64 / 1000.0));
            }
            if let Some(b) = i.bits {
                info_line.push(format!("{b}-bit"));
            }
            match i.channels {
                0 => {}
                1 => info_line.push("mono".into()),
                2 => info_line.push("stereo".into()),
                n => info_line.push(format!("{n} ch")),
            }
        }
        if let Some(t) = self.total() {
            info_line.push(fmt_time(t as f64 / self.rate() as f64, false));
        }
        let mut head = column![text(title).size(16)].spacing(2);
        if !sub.is_empty() {
            head = head.push(text(sub).size(13));
        }
        head = head.push(small(info_line.join(" · ")));
        let cover: Element<'_, Msg> = match &self.cover {
            Some(h) => image(h.clone()).width(72).height(72).into(),
            None => container(
                icon(Bootstrap::MusicNoteBeamed)
                    .size(32)
                    .color(theme::MUTED),
            )
            .center(72)
            .style(theme::panel)
            .into(),
        };
        let header = row![cover, head]
            .spacing(12)
            .align_y(iced::Alignment::Center);

        // waveform + transport (or why it can't play)
        let player: Element<'_, Msg> = if let Some(e) = &self.error {
            container(text(e.clone()).size(13).color(theme::MUTED))
                .padding(12)
                .width(Length::Fill)
                .style(theme::panel)
                .into()
        } else {
            let wave = Canvas::new(Wave { st: self })
                .width(Length::Fill)
                .height(WAVE_H);
            let rate = self.rate() as f64;
            let shown = match (self.scrub, self.total()) {
                (Some(f), Some(t)) => (f as f64 * t as f64) as u64,
                _ => self.pos,
            };
            let time = format!(
                "{} / {}",
                fmt_time(shown as f64 / rate, false),
                self.total()
                    .map(|t| fmt_time(t as f64 / rate, false))
                    .unwrap_or_else(|| "--:--".into())
            );
            let play = button(
                icon(if self.playing {
                    Bootstrap::PauseFill
                } else {
                    Bootstrap::PlayFill
                })
                .size(16),
            )
            .padding([5, 10])
            .on_press(Msg::PlayPause);
            let lp = button(icon(Bootstrap::Repeat).size(14))
                .padding([5, 8])
                .style(if self.looping {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Msg::ToggleLoop);
            let vol_icon = if self.volume <= 0.0 {
                Bootstrap::VolumeMuteFill
            } else {
                Bootstrap::VolumeUpFill
            };
            let status = if self.peaks.is_none() {
                match &self.peaks_err {
                    Some(e) => format!("waveform: {e}"),
                    None => "computing waveform…".into(),
                }
            } else {
                String::new()
            };
            let transport = row![
                play,
                lp,
                text(time).size(13).font(iced::Font::MONOSPACE),
                small(status),
                Space::with_width(Length::Fill),
                icon(vol_icon).size(14).color(theme::MUTED),
                slider(0.0..=1.0, self.volume, Msg::Volume)
                    .step(0.01f32)
                    .width(110.0),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center);
            column![container(wave).style(theme::panel).padding(4), transport]
                .spacing(8)
                .into()
        };

        // edit bar, as in the text viewer
        let dirty = self.is_dirty();
        let can_tag = self.tag_error.is_none();
        let bar: Element<'_, Msg> = if self.editing {
            row![
                text(if dirty {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(Color::from_rgb8(0xff, 0xc8, 0x6b)),
                Space::with_width(Length::Fill),
                button(text("Discard").size(12))
                    .on_press_maybe(dirty.then_some(Msg::Discard))
                    .style(button::secondary),
                button(text("Save  Ctrl+S").size(12))
                    .on_press_maybe((dirty && !self.saving).then_some(Msg::Save)),
                button(text("Done").size(12))
                    .on_press_maybe((!dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                small("Read-only".into()),
                Space::with_width(Length::Fill),
                button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit),
            ]
            .align_y(iced::Alignment::Center)
            .into()
        };

        let mut body = column![header, player, bar].spacing(10);

        if self.editing && self.playable() {
            let rate = self.rate() as f64;
            let sel_txt = match self.sel_frames() {
                Some((a, b)) => format!(
                    "Selection {} – {}  ({})",
                    fmt_time(a as f64 / rate, true),
                    fmt_time(b as f64 / rate, true),
                    fmt_time((b - a) as f64 / rate, true)
                ),
                None => "Drag on the waveform to select".into(),
            };
            let has_sel = self.sel_frames().is_some();
            let idle = !self.busy;
            body = body.push(
                row![
                    icon(Bootstrap::Scissors).size(13).color(theme::MUTED),
                    small(sel_txt),
                    Space::with_width(Length::Fill),
                    button(text("Clear").size(12))
                        .style(button::secondary)
                        .on_press_maybe(has_sel.then_some(Msg::ClearSelection)),
                    button(text("Export selection as WAV").size(12))
                        .on_press_maybe((has_sel && idle).then_some(Msg::Export(false))),
                    button(
                        text(if has_sel {
                            "Normalize selection → WAV"
                        } else {
                            "Normalize → WAV"
                        })
                        .size(12)
                    )
                    .on_press_maybe(idle.then_some(Msg::Export(true))),
                ]
                .spacing(6)
                .align_y(iced::Alignment::Center),
            );
        }

        // tags
        let tags: Element<'_, Msg> = if let Some(e) = &self.tag_error {
            small(format!("Tags unavailable: {e}")).into()
        } else {
            let rows = FIELDS.iter().filter_map(|&(f, label)| {
                let v = self.tags.get(f);
                let value: Element<'_, Msg> = if self.editing && can_tag {
                    text_input(label, v)
                        .on_input(move |s| Msg::Field(f, s))
                        .size(13)
                        .padding(4)
                        .into()
                } else if v.is_empty() {
                    return None;
                } else {
                    text(v.to_string()).size(13).into()
                };
                Some(
                    row![container(small(label.to_string())).width(96), value]
                        .spacing(8)
                        .align_y(iced::Alignment::Center)
                        .into(),
                )
            });
            let col = column(rows).spacing(4);
            if !self.editing && self.tags == Tags::default() {
                small("No tags".into()).into()
            } else {
                col.into()
            }
        };
        body = body.push(
            container(tags)
                .padding(10)
                .width(Length::Fill)
                .style(theme::panel),
        );

        scrollable(body.padding(4)).height(Length::Fill).into()
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        self.tags != self.orig
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        Some(self.editing)
    }

    /// Ctrl+E. Leaving edit mode with unsaved changes must be refused (return
    /// an Event::Status explaining) — see text.rs.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        let (t, _) = self.update(Msg::ToggleEdit);
        t
    }

    /// Ctrl+S: write over the original (atomically). No-op if not dirty.
    pub fn save(&mut self) -> Task<Msg> {
        if self.saving || !self.is_dirty() || self.tag_error.is_some() {
            return Task::none();
        }
        self.saving = true;
        let (path, tags) = (self.path.clone(), self.tags.clone());
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || write_tags(&path, &tags).map(|()| tags))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            },
            Msg::Saved,
        )
    }
}

impl Drop for State {
    /// Tab closed: silence playback, stop the thread and any peak pass.
    fn drop(&mut self) {
        self.stop();
    }
}

fn fmt_time(secs: f64, tenths: bool) -> String {
    let secs = secs.max(0.0);
    let whole = secs as u64;
    let (h, m, s) = (whole / 3600, whole / 60 % 60, whole % 60);
    let base = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    if tenths {
        format!("{base}.{}", ((secs.fract() * 10.0) as u64).min(9))
    } else {
        base
    }
}

// ------------------------------------------------------------- waveform ----

struct Wave<'a> {
    st: &'a State,
}

#[derive(Default)]
struct Drag {
    start: Option<f32>,
    moved: bool,
}

impl canvas::Program<Msg> for Wave<'_> {
    type State = Drag;

    fn update(
        &self,
        drag: &mut Drag,
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Msg>) {
        use canvas::event::Status;
        let x = |p: Point| ((p.x - bounds.x) / bounds.width.max(1.0)).clamp(0.0, 1.0);
        let editing = self.st.editing;
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_over(bounds) {
                    let f = x(p);
                    *drag = Drag {
                        start: Some(f),
                        moved: false,
                    };
                    let m = (!editing).then_some(Msg::Scrub(f));
                    return (Status::Captured, m);
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) => {
                if let Some(s) = drag.start {
                    let f = x(position);
                    drag.moved |= (f - s).abs() * bounds.width > 3.0;
                    let m = if editing {
                        drag.moved.then_some(Msg::Select(s, f))
                    } else {
                        Some(Msg::Scrub(f))
                    };
                    return (Status::Captured, m);
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(s) = drag.start.take() {
                    let f = cursor.position().map(x).unwrap_or(s);
                    let m = if editing && drag.moved {
                        Msg::Select(s, f)
                    } else {
                        Msg::Seek(f)
                    };
                    return (Status::Captured, Some(m));
                }
            }
            _ => {}
        }
        (Status::Ignored, None)
    }

    fn draw(
        &self,
        _drag: &Drag,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let pal = theme.extended_palette();
        let (w, h) = (bounds.width, bounds.height);
        let mid = h / 2.0;
        let wave = self.st.wave.draw(renderer, bounds.size(), |f| {
            let color = pal.background.base.text.scale_alpha(0.55);
            let path = CPath::new(|b| match &self.st.peaks {
                Some(p) if !p.buckets.is_empty() => {
                    let n = p.buckets.len();
                    let cols = w.max(1.0) as usize;
                    for x in 0..cols {
                        let a = x * n / cols;
                        let e = ((x + 1) * n / cols).max(a + 1).min(n);
                        if a >= n {
                            break;
                        }
                        let (lo, hi) = p.buckets[a..e]
                            .iter()
                            .fold((f32::MAX, f32::MIN), |(l, h), &(x, y)| (l.min(x), h.max(y)));
                        let top = mid - hi.clamp(-1.0, 1.0) * mid;
                        let bot = (mid - lo.clamp(-1.0, 1.0) * mid).max(top + 1.0);
                        b.move_to(Point::new(x as f32 + 0.5, top));
                        b.line_to(Point::new(x as f32 + 0.5, bot));
                    }
                }
                _ => {
                    b.move_to(Point::new(0.0, mid));
                    b.line_to(Point::new(w, mid));
                }
            });
            f.stroke(&path, Stroke::default().with_width(1.0).with_color(color));
        });

        let mut f = Frame::new(renderer, bounds.size());
        let accent = pal.primary.base.color;
        let px = self.st.frac() * w;
        if let Some((a, b)) = self.st.sel.filter(|_| self.st.editing) {
            let (xa, xb) = (a * w, b * w);
            f.fill_rectangle(
                Point::new(xa, 0.0),
                Size::new((xb - xa).max(1.0), h),
                accent.scale_alpha(0.22),
            );
            for x in [xa, xb] {
                f.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, h), accent);
            }
        } else {
            f.fill_rectangle(Point::ORIGIN, Size::new(px, h), accent.scale_alpha(0.10));
        }
        f.fill_rectangle(
            Point::new((px - 1.0).max(0.0), 0.0),
            Size::new(2.0, h),
            accent,
        );
        vec![wave, f.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        drag: &Drag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if drag.start.is_some() || cursor.is_over(bounds) {
            if self.st.editing {
                mouse::Interaction::Crosshair
            } else {
                mouse::Interaction::Pointer
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

// ---------------------------------------------------------------- tests ----

#[cfg(test)]
mod tests {
    use super::*;

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(name: &str) -> Self {
            let home = std::env::var_os("HOME").expect("HOME");
            let p = PathBuf::from(home).join(format!(
                ".cache/ogx-p2-audio-tests/{name}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 440 Hz sine, 16-bit, `secs` long, amplitude `amp`.
    fn sine(path: &Path, rate: u32, ch: u16, secs: f64, amp: f32) -> u64 {
        let spec = hound::WavSpec {
            channels: ch,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        let n = (rate as f64 * secs) as u64;
        for i in 0..n {
            let v = (i as f32 / rate as f32 * 440.0 * std::f32::consts::TAU).sin() * amp;
            for _ in 0..ch {
                w.write_sample((v * 32767.0) as i16).unwrap();
            }
        }
        w.finalize().unwrap();
        n
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn audio_supported_by_extension() {
        for f in [
            "a.mp3", "b.FLAC", "c.ogg", "d.wav", "e.m4a", "f.aac", "g.opus", "h.alac",
        ] {
            assert!(supported(Path::new(f)), "{f}");
        }
        for f in ["a.txt", "b.mp4", "noext"] {
            assert!(!supported(Path::new(f)), "{f}");
        }
    }

    #[tokio::test]
    async fn audio_open_reads_metadata() {
        let t = TmpDir::new("open");
        let p = t.0.join("tone.wav");
        let n = sine(&p, 44_100, 2, 1.5, 0.5);
        let o = open(p.clone()).await.unwrap();
        assert!(o.error.is_none(), "{:?}", o.error);
        let i = o.info.unwrap();
        assert_eq!(i.sample_rate, 44_100);
        assert_eq!(i.channels, 2);
        assert_eq!(i.bits, Some(16));
        assert_eq!(i.frames, Some(n));
        assert!((i.duration().unwrap() - 1.5).abs() < 1e-3);
        assert_eq!(o.tags, Tags::default());

        // garbage isn't audio
        let bad = t.0.join("bad.mp3");
        std::fs::write(&bad, b"definitely not audio").unwrap();
        assert!(open(bad).await.is_err());
    }

    #[test]
    fn audio_peaks() {
        let t = TmpDir::new("peaks");
        let p = t.0.join("tone.wav");
        let n = sine(&p, 48_000, 1, 2.0, 0.5);
        let pk = compute_peaks(&p, BUCKETS, &no_cancel()).unwrap();
        assert_eq!(pk.frames, n);
        assert_eq!(pk.rate, 48_000);
        assert_eq!(pk.channels, 1);
        assert_eq!(pk.buckets.len(), BUCKETS);
        // buckets are ~0.4 of a 440 Hz period here, so check the envelope,
        // then coarse buckets spanning many periods
        let hi = pk.buckets.iter().map(|b| b.1).fold(f32::MIN, f32::max);
        let lo = pk.buckets.iter().map(|b| b.0).fold(f32::MAX, f32::min);
        assert!((hi - 0.5).abs() < 0.01, "{hi}");
        assert!((lo + 0.5).abs() < 0.01, "{lo}");
        let coarse = reduce_peaks(&pk.buckets, 10);
        assert_eq!(coarse.len(), 10);
        for (l, h) in coarse {
            assert!((h - 0.5).abs() < 0.01 && (l + 0.5).abs() < 0.01);
        }
        // short input: fewer buckets than asked
        assert_eq!(reduce_peaks(&[(0.0, 1.0); 5], 2000).len(), 5);
        // cancellation
        assert!(compute_peaks(&p, BUCKETS, &AtomicBool::new(true)).is_err());
    }

    fn wav_stats(p: &Path) -> (hound::WavSpec, u64, f32) {
        let mut r = hound::WavReader::open(p).unwrap();
        let spec = r.spec();
        let frames = r.duration() as u64;
        let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
        let peak = r
            .samples::<i32>()
            .map(|s| (s.unwrap() as f32 / max).abs())
            .fold(0.0, f32::max);
        (spec, frames, peak)
    }

    #[test]
    fn audio_export_trim_and_normalize() {
        let t = TmpDir::new("export");
        let p = t.0.join("tone.wav");
        let n = sine(&p, 44_100, 2, 2.0, 0.25);

        let x = export_wav(&p, Some((22_050, 66_150)), false, &no_cancel()).unwrap();
        assert_eq!(x.path, t.0.join("tone-edit.wav"));
        assert_eq!(x.frames, 44_100);
        let (spec, frames, peak) = wav_stats(&x.path);
        assert_eq!(
            (spec.channels, spec.sample_rate, spec.bits_per_sample),
            (2, 44_100, 24)
        );
        assert_eq!(frames, 44_100);
        assert!((peak - 0.25).abs() < 0.01, "{peak}");

        // never overwrites: second export gets a unique name
        let y = export_wav(&p, None, true, &no_cancel()).unwrap();
        assert_eq!(y.path, t.0.join("tone-edit (2).wav"));
        let (_, frames, peak) = wav_stats(&y.path);
        assert_eq!(frames, n);
        assert!((peak - NORM_PEAK).abs() < 1e-3, "{peak}");
        assert!((y.gain_db - 20.0 * (NORM_PEAK / 0.25f32).log10()).abs() < 0.05);

        // normalize a selection only
        let z = export_wav(&p, Some((0, 1000)), true, &no_cancel()).unwrap();
        let (_, frames, peak) = wav_stats(&z.path);
        assert_eq!(frames, 1000);
        assert!((peak - NORM_PEAK).abs() < 1e-3);

        assert!(export_wav(&p, Some((10, 10)), false, &no_cancel()).is_err());
        // the original is untouched
        assert_eq!(probe_info(&p).unwrap().0.frames, Some(n));
    }

    #[test]
    fn audio_tag_round_trip_wav() {
        let t = TmpDir::new("tags");
        let p = t.0.join("tone.wav");
        let n = sine(&p, 22_050, 1, 0.5, 0.5);
        let tags = Tags {
            title: "Sine Song".into(),
            artist: "Oscillator".into(),
            album: "Test Tones".into(),
            album_artist: "Various".into(),
            year: "2026".into(),
            track: "7".into(),
            genre: "Electronic".into(),
            comment: "made by a test".into(),
        };
        write_tags(&p, &tags).unwrap();
        let (back, cover) = read_tags(&p).unwrap();
        assert_eq!(back, tags);
        assert!(cover.is_none());
        // audio intact, no temp file left behind
        assert_eq!(probe_info(&p).unwrap().0.frames, Some(n));
        let left: Vec<_> = std::fs::read_dir(&t.0).unwrap().collect();
        assert_eq!(left.len(), 1);

        // clearing a field removes it; bad numbers are refused untouched
        let mut less = tags.clone();
        less.comment.clear();
        write_tags(&p, &less).unwrap();
        assert_eq!(read_tags(&p).unwrap().0, less);
        let mut bad = less.clone();
        bad.year = "soon".into();
        assert!(write_tags(&p, &bad).is_err());
        assert_eq!(read_tags(&p).unwrap().0, less);
    }

    #[test]
    fn audio_state_dirty_and_edit_mode() {
        let o = Opened {
            path: PathBuf::from("/nonexistent/x.wav"),
            info: None,
            error: Some("x".into()),
            tags: Tags::default(),
            cover: None,
            tag_error: None,
        };
        let (mut s, _) = State::new(o);
        assert_eq!(s.is_editing(), Some(false));
        let _ = s.view();
        // read-only: field edits ignored
        let _ = s.update(Msg::Field(Field::Title, "a".into()));
        assert!(!s.is_dirty());
        let _ = s.toggle_edit();
        let _ = s.update(Msg::Field(Field::Title, "a".into()));
        assert!(s.is_dirty());
        let _ = s.view();
        // can't leave edit mode while dirty
        let (_, ev) = s.update(Msg::ToggleEdit);
        assert!(matches!(ev, Some(Event::Status(_))));
        assert_eq!(s.is_editing(), Some(true));
        let _ = s.update(Msg::Discard);
        assert!(!s.is_dirty());
        let _ = s.toggle_edit();
        assert_eq!(s.is_editing(), Some(false));
    }

    #[test]
    fn audio_resampler_length_and_continuity() {
        for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100), (22_050, 48_000)] {
            let mut r = Linear::new(from, to, 2);
            let mut out = Vec::new();
            let n = from as usize; // 1 s
                                   // DC on L, ramp on R, fed in odd-sized chunks
            let inp: Vec<f32> = (0..n).flat_map(|i| [0.5, i as f32 / n as f32]).collect();
            for c in inp.chunks(2 * 997) {
                r.process(c, &mut out);
            }
            let got = out.len() / 2;
            let want = to as usize;
            assert!(got.abs_diff(want) <= 2, "{from}->{to}: {got} vs {want}");
            assert!(out.chunks(2).all(|f| (f[0] - 0.5).abs() < 1e-6));
            // the ramp stays monotonic across chunk boundaries
            assert!(out
                .chunks(2)
                .zip(out.chunks(2).skip(1))
                .all(|(a, b)| b[1] >= a[1]));
        }
    }

    #[test]
    fn audio_channel_mapping() {
        let mut o = Vec::new();
        map_channels(&[0.2, 0.4], 1, 2, &mut o);
        assert_eq!(o, [0.2, 0.2, 0.4, 0.4]);
        map_channels(&[0.2, 0.4], 2, 1, &mut o);
        assert!((o[0] - 0.3).abs() < 1e-6 && o.len() == 1);
        map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, 2, &mut o);
        assert_eq!(o, [1.0, 2.0]);
    }

    #[test]
    fn audio_decoder_seek_lands_exactly() {
        let t = TmpDir::new("seek");
        let p = t.0.join("tone.wav");
        sine(&p, 8_000, 1, 1.0, 0.5);
        let mut d = Dec::open(&p).unwrap();
        let skip = d.seek(4_000).unwrap();
        // count the frames remaining after the skip
        let mut rest = 0u64;
        let mut skip_left = skip;
        while let Some((s, _, _)) = d.next().unwrap() {
            let n = s.len() as u64;
            let k = skip_left.min(n);
            skip_left -= k;
            rest += n - k;
        }
        assert_eq!(rest, 4_000);
        assert_eq!(fmt_time(3725.46, true), "1:02:05.4");
        assert_eq!(fmt_time(65.0, false), "1:05");
    }
}
