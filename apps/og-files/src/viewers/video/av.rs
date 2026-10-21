//! AVFoundation player: the fallback player of macOS builds without the
//! `ffmpeg` feature, i.e. the Mac App Store build (which can't ship the GPL
//! libmpv / FFmpeg). Same face as the FFmpeg player in `ff.rs`: play/pause,
//! seek, frame step, speed, volume, plus the edit mode (In/Out trim export
//! and frame snapshots). Plays what macOS plays natively: mp4, m4v, mov,
//! H.264 / HEVC / ProRes, AAC / ALAC; not mkv / webm.
//!
//! `AVPlayer` is main-thread-only, and so is iced's `update`, so the player
//! lives right in [`State`]. Frames come from an `AVPlayerItemVideoOutput`
//! (BGRA pixel buffers) polled on a short self-rescheduling tick and shown
//! as an image, like `ff.rs` does. Thumbnails, snapshots and trim export run
//! on worker threads through `AVAssetReader` / `AVAssetExportSession`.

use super::common::{edit_name, fmt_mark, fmt_time, ExportEv, Info, AMBER};
use crate::theme;
use crate::viewers::Event;
use crate::widgets::icon;
use block2::RcBlock;
use iced::futures::SinkExt;
use iced::widget::{button, column, container, image as img, row, slider, text, Space};
use iced::{Alignment, Element, Length, Task};
use iced_fonts::Bootstrap as B;
use image::RgbaImage;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::MainThreadMarker;
use objc2_av_foundation::{
    AVAsset, AVAssetExportPresetPassthrough, AVAssetExportSession, AVAssetExportSessionStatus,
    AVAssetReader, AVAssetReaderOutput, AVAssetReaderTrackOutput, AVAssetTrack,
    AVFileTypeMPEG4, AVFileTypeQuickTimeMovie, AVMediaTypeAudio, AVMediaTypeVideo, AVPlayer,
    AVPlayerItem, AVPlayerItemStatus, AVPlayerItemVideoOutput, AVURLAsset,
};
use objc2_core_media::{kCMTimePositiveInfinity, kCMTimeZero, CMTime, CMTimeRange};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBuffer,
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Trims are written by AVAssetExportSession (passthrough: no re-encode).
pub(super) const CAN_EXPORT: bool = true;
const SPEEDS: [f32; 4] = [0.5, 1.0, 1.5, 2.0];
/// Frame poll interval while playing / while paused (a paused player still
/// delivers the frame a seek or step lands on).
const TICK_PLAYING: Duration = Duration::from_millis(12);
const TICK_PAUSED: Duration = Duration::from_millis(100);

// ═══════════════════════════════════════════════════════════════ helpers

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn asset(path: &Path) -> Retained<AVURLAsset> {
    // SAFETY: plain constructor, no options
    unsafe { AVURLAsset::URLAssetWithURL_options(&file_url(path), None) }
}

fn secs(t: CMTime) -> f64 {
    // SAFETY: pure function on a value
    let s = unsafe { t.seconds() };
    if s.is_finite() {
        s
    } else {
        0.0
    }
}

fn cmtime(s: f64) -> CMTime {
    // SAFETY: pure function on values
    unsafe { CMTime::with_seconds(s.max(0.0), 600) }
}

/// `{kCVPixelBufferPixelFormatTypeKey: 32BGRA}` for video outputs.
fn bgra_attrs() -> Retained<NSDictionary<NSString, AnyObject>> {
    // SAFETY: CFString and NSString are toll-free bridged
    let key: &NSString = unsafe {
        &*(kCVPixelBufferPixelFormatTypeKey as *const objc2_core_foundation::CFString
            as *const NSString)
    };
    let fmt = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
    let value: &AnyObject = &fmt;
    NSDictionary::from_slices(&[key], &[value])
}

/// Copies a BGRA pixel buffer into an RGBA image.
fn to_rgba(pb: &CVPixelBuffer) -> Option<RgbaImage> {
    // SAFETY: locked/unlocked in pairs around the read below
    if unsafe { CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
    let stride = CVPixelBufferGetBytesPerRow(pb);
    let base = CVPixelBufferGetBaseAddress(pb) as *const u8;
    let out = (!base.is_null() && w > 0 && h > 0).then(|| {
        let mut buf = vec![0u8; w * h * 4];
        for y in 0..h {
            // SAFETY: the locked buffer holds h rows of `stride` bytes
            let src = unsafe { std::slice::from_raw_parts(base.add(y * stride), w * 4) };
            let dst = &mut buf[y * w * 4..(y + 1) * w * 4];
            for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        RgbaImage::from_raw(w as u32, h as u32, buf)
    });
    // SAFETY: paired with the lock above
    unsafe { CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) };
    out.flatten()
}

fn first_track(asset: &AVAsset, video: bool) -> Option<Retained<AVAssetTrack>> {
    // SAFETY: framework constants, read once
    let kind = unsafe { if video { AVMediaTypeVideo } else { AVMediaTypeAudio } }?;
    // SAFETY: synchronous track query on a local asset. Deprecated since
    // macOS 13 in favour of the async loader, but still supported and the
    // right shape here: callers already run off the UI thread.
    #[allow(deprecated)]
    unsafe { asset.tracksWithMediaType(kind) }.firstObject()
}

/// Clockwise rotation (0/90/180/270) phones record in the track transform.
fn rotation(track: &AVAssetTrack) -> u32 {
    // SAFETY: property read
    let t = unsafe { track.preferredTransform() };
    let deg = t.b.atan2(t.a).to_degrees().round() as i32;
    (deg.rem_euclid(360) as u32 + 45) / 90 * 90 % 360
}

fn rotate(im: RgbaImage, deg: u32) -> RgbaImage {
    match deg {
        90 => image::imageops::rotate90(&im),
        180 => image::imageops::rotate180(&im),
        270 => image::imageops::rotate270(&im),
        _ => im,
    }
}

// ════════════════════════════════════════════════ probe / thumbnail / export

pub(super) fn probe(path: &Path) -> Result<Info, String> {
    if !path.is_file() {
        return Err(format!("{}: not a file", path.display()));
    }
    let asset = asset(path);
    // SAFETY: synchronous property reads on a local asset
    let duration = secs(unsafe { asset.duration() });
    let video = first_track(&asset, true).ok_or("Can't read this video (no video track macOS can open)")?;
    // SAFETY: property reads
    let (size, fps, rate) = unsafe { (video.naturalSize(), video.nominalFrameRate(), video.estimatedDataRate()) };
    let (mut w, mut h) = (size.width.round() as u32, size.height.round() as u32);
    if rotation(&video) % 180 == 90 {
        std::mem::swap(&mut w, &mut h);
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
    Ok(Info {
        duration,
        width: w,
        height: h,
        fps: fps as f64,
        video_codec: ext,
        audio: first_track(&asset, false).map(|_| "audio".into()),
        bit_rate: rate as i64,
    })
}

/// The first frame at or after `t` (seconds), upright. Blocking.
fn frame_at(path: &Path, t: f64) -> Result<(f64, RgbaImage), String> {
    let asset = asset(path);
    let track = first_track(&asset, true).ok_or("no video track")?;
    let err = |e: Retained<objc2_foundation::NSError>| e.localizedDescription().to_string();
    // SAFETY: reader set up and used on this thread only
    unsafe {
        let reader = AVAssetReader::assetReaderWithAsset_error(&asset).map_err(err)?;
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &track,
            Some(&bgra_attrs()),
        );
        output.setAlwaysCopiesSampleData(false);
        reader.setTimeRange(CMTimeRange { start: cmtime(t), duration: kCMTimePositiveInfinity });
        let out: &AVAssetReaderOutput = &output;
        if !reader.canAddOutput(out) {
            return Err("can't decode this video".into());
        }
        reader.addOutput(out);
        if !reader.startReading() {
            return Err(reader.error().map(err).unwrap_or_else(|| "can't decode this video".into()));
        }
        let sample = output.copyNextSampleBuffer().ok_or("no frame there")?;
        reader.cancelReading();
        let pts = secs(sample.presentation_time_stamp());
        let pb = sample.image_buffer().ok_or("no picture in that frame")?;
        let im = to_rgba(&pb).ok_or("can't read the frame")?;
        Ok((pts, rotate(im, rotation(&track))))
    }
}

/// Grid thumbnail: a frame ~10% in, scaled to fit `max`×`max`.
pub fn thumbnail(path: &Path, max: u32) -> Option<RgbaImage> {
    // SAFETY: synchronous property read on a local asset
    let dur = secs(unsafe { asset(path).duration() });
    let (_, im) = frame_at(path, dur * 0.1).ok()?;
    let (w, h) = im.dimensions();
    let k = (max as f32 / w.max(h).max(1) as f32).min(1.0);
    let fit = |v: u32| ((v as f32 * k).round() as u32).max(1);
    Some(image::imageops::thumbnail(&im, fit(w), fit(h)))
}

fn snapshot(path: &Path, t: f64) -> Result<PathBuf, String> {
    let (pts, im) = frame_at(path, t)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let s = pts.max(0.0) as u64;
    let name = format!("{stem}-frame-{:02}m{:02}s.png", s / 60, s % 60);
    let dst = crate::ops::unique_name(dir, &name);
    im.save(&dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Passthrough-export `[tin, tout)` of `src` into `<stem>-edit.<ext>`
/// (`.mov` unless the source is mp4/m4v), streaming progress.
pub(super) fn spawn_export(src: PathBuf, tin: f64, tout: f64) -> Task<ExportEv> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ExportEv>();
    std::thread::spawn(move || {
        let r = export_trim(&src, tin, tout, &|p| {
            let _ = tx.send(ExportEv::Progress(p));
        });
        let _ = tx.send(ExportEv::Done(r));
    });
    let stream = iced::stream::channel(4, move |mut out| async move {
        while let Some(ev) = rx.recv().await {
            if out.send(ev).await.is_err() {
                break;
            }
        }
    });
    Task::run(stream, |e| e)
}

fn export_trim(src: &Path, tin: f64, tout: f64, progress: &dyn Fn(f32)) -> Result<PathBuf, String> {
    let mp4 = src
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("m4v"));
    let mut dst = edit_name(src);
    if !mp4 && !dst.extension().is_some_and(|e| e.eq_ignore_ascii_case("mov")) {
        dst = crate::ops::unique_name(
            dst.parent().unwrap_or(Path::new(".")),
            &dst.with_extension("mov").file_name().unwrap_or_default().to_string_lossy(),
        );
    }
    let asset = asset(src);
    let done = Arc::new(AtomicBool::new(false));
    // SAFETY: the session is created, driven and polled on this thread; the
    // completion block only flips an atomic
    unsafe {
        let session = AVAssetExportSession::exportSessionWithAsset_presetName(&asset, AVAssetExportPresetPassthrough)
            .ok_or("macOS can't export this video")?;
        let kind = if mp4 { AVFileTypeMPEG4 } else { AVFileTypeQuickTimeMovie };
        session.setOutputURL(Some(&file_url(&dst)));
        session.setOutputFileType(kind);
        session.setTimeRange(CMTimeRange { start: cmtime(tin), duration: cmtime(tout - tin) });
        let flag = done.clone();
        let block = RcBlock::new(move || flag.store(true, Ordering::Release));
        session.exportAsynchronouslyWithCompletionHandler(&block);
        while !done.load(Ordering::Acquire) {
            progress(session.progress());
            std::thread::sleep(Duration::from_millis(100));
        }
        if session.status() == AVAssetExportSessionStatus::Completed {
            Ok(dst)
        } else {
            let _ = std::fs::remove_file(&dst);
            Err(session
                .error()
                .map(|e| e.localizedDescription().to_string())
                .unwrap_or_else(|| "export failed".into()))
        }
    }
}

// ══════════════════════════════════════════════════════════════════════ UI

#[derive(Debug, Clone)]
pub enum Msg {
    TogglePlay,
    Tick,
    SeekDrag(f64),
    SeekRelease,
    Step,
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

struct Av {
    player: Retained<AVPlayer>,
    item: Retained<AVPlayerItem>,
    output: Retained<AVPlayerItemVideoOutput>,
    rotation: u32,
}

impl Drop for Av {
    fn drop(&mut self) {
        // SAFETY: main thread (State lives in the app); stops audio at once
        unsafe {
            self.player.pause();
            self.player.replaceCurrentItemWithPlayerItem(None);
        }
    }
}

pub struct State {
    path: PathBuf,
    info: Info,
    av: Option<Av>,
    frame: Option<img::Handle>,
    failed: Option<String>,
    pos: f64,
    playing: bool,
    drag: Option<f64>,
    volume: f32,
    muted: bool,
    speed_ix: usize,
    editing: bool,
    mark_in: Option<f64>,
    mark_out: Option<f64>,
    export: Option<f32>,
    snapshotting: bool,
}

fn tick(after: Duration) -> Task<Msg> {
    Task::perform(tokio::time::sleep(after), |_| Msg::Tick)
}

impl State {
    pub fn new(path: PathBuf, info: Info) -> (Self, Task<Msg>) {
        let mut s = Self {
            path,
            info,
            av: None,
            frame: None,
            failed: None,
            pos: 0.0,
            playing: false,
            drag: None,
            volume: 1.0,
            muted: false,
            speed_ix: 1,
            editing: false,
            mark_in: None,
            mark_out: None,
            export: None,
            snapshotting: false,
        };
        match MainThreadMarker::new() {
            Some(mtm) => {
                let asset = asset(&s.path);
                let rotation = first_track(&asset, true).map(|t| rotation(&t)).unwrap_or(0);
                // SAFETY: AVFoundation objects created and used on the main thread
                let av = unsafe {
                    let item = AVPlayerItem::playerItemWithAsset(&asset, mtm);
                    let output = AVPlayerItemVideoOutput::initWithPixelBufferAttributes(
                        mtm.alloc(),
                        Some(&bgra_attrs()),
                    );
                    item.addOutput(&output);
                    let player = AVPlayer::playerWithPlayerItem(Some(&item), mtm);
                    Av { player, item, output, rotation }
                };
                s.av = Some(av);
            }
            None => s.failed = Some("the video player must start on the main thread".into()),
        }
        (s, tick(Duration::ZERO))
    }

    /// Pulls a new frame (if any), the position and the end/error state.
    fn poll(&mut self) {
        let Some(av) = &self.av else { return };
        // SAFETY: main thread; property reads and a buffer copy
        unsafe {
            if av.item.status() == AVPlayerItemStatus::Failed {
                self.failed = Some(
                    av.item
                        .error()
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "macOS can't play this video".into()),
                );
                self.playing = false;
                return;
            }
            let now = av.item.currentTime();
            if av.output.hasNewPixelBufferForItemTime(now) {
                if let Some(pb) = av
                    .output
                    .copyPixelBufferForItemTime_itemTimeForDisplay(now, std::ptr::null_mut())
                {
                    if let Some(im) = to_rgba(&pb) {
                        let im = rotate(im, av.rotation);
                        let (w, h) = im.dimensions();
                        self.frame = Some(img::Handle::from_rgba(w, h, im.into_raw()));
                    }
                }
            }
            if self.drag.is_none() {
                self.pos = secs(av.player.currentTime());
            }
            if self.playing && av.player.rate() == 0.0 && self.pos >= self.info.duration - 0.1 {
                self.playing = false; // reached the end
            }
        }
    }

    fn seek(&self, t: f64, exact: bool) {
        if let Some(av) = &self.av {
            // SAFETY: main thread
            unsafe {
                let tol = if exact { kCMTimeZero } else { kCMTimePositiveInfinity };
                av.player.seekToTime_toleranceBefore_toleranceAfter(cmtime(t), tol, tol);
            }
        }
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Tick => {
                self.poll();
                let next = if self.playing { TICK_PLAYING } else { TICK_PAUSED };
                return (tick(next), None);
            }
            Msg::TogglePlay => {
                let Some(av) = &self.av else { return (Task::none(), None) };
                // SAFETY: main thread
                unsafe {
                    if self.playing {
                        av.player.pause();
                        self.playing = false;
                    } else {
                        if self.pos >= self.info.duration - 0.1 {
                            self.seek(0.0, true);
                        }
                        av.player.setRate(SPEEDS[self.speed_ix]);
                        self.playing = true;
                    }
                }
            }
            Msg::SeekDrag(t) => {
                self.drag = Some(t);
                self.pos = t;
                self.seek(t, false);
            }
            Msg::SeekRelease => {
                if let Some(t) = self.drag.take() {
                    self.seek(t, true);
                }
            }
            Msg::Step => {
                if let (Some(av), false) = (&self.av, self.playing) {
                    // SAFETY: main thread
                    unsafe { av.item.stepByCount(1) };
                }
            }
            Msg::Volume(v) => {
                self.volume = v;
                if let Some(av) = &self.av {
                    // SAFETY: main thread
                    unsafe { av.player.setVolume(v) };
                }
            }
            Msg::ToggleMute => {
                self.muted = !self.muted;
                if let Some(av) = &self.av {
                    // SAFETY: main thread
                    unsafe { av.player.setMuted(self.muted) };
                }
            }
            Msg::CycleSpeed => {
                self.speed_ix = (self.speed_ix + 1) % SPEEDS.len();
                if let (Some(av), true) = (&self.av, self.playing) {
                    // SAFETY: main thread
                    unsafe { av.player.setRate(SPEEDS[self.speed_ix]) };
                }
            }
            Msg::ToggleEdit => return (self.toggle_edit(), None),
            Msg::SetIn => {
                self.mark_in = Some(self.pos);
                if self.mark_out.is_some_and(|o| o <= self.pos) {
                    self.mark_out = None;
                }
            }
            Msg::SetOut => {
                self.mark_out = Some(self.pos);
                if self.mark_in.is_some_and(|i| i >= self.pos) {
                    self.mark_in = None;
                }
            }
            Msg::ClearMarks => {
                self.mark_in = None;
                self.mark_out = None;
            }
            Msg::Export => {
                if self.export.is_some() {
                    return (Task::none(), None);
                }
                let tin = self.mark_in.unwrap_or(0.0);
                let tout = self.mark_out.unwrap_or(self.info.duration);
                if tout <= tin {
                    return (Task::none(), Some(Event::Status("Trim: Out must be after In".into())));
                }
                self.export = Some(0.0);
                return (spawn_export(self.path.clone(), tin, tout).map(Msg::Exporting), None);
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
                    return (Task::none(), None);
                }
                self.snapshotting = true;
                let (path, t) = (self.path.clone(), self.pos);
                let fut = async move {
                    tokio::task::spawn_blocking(move || snapshot(&path, t))
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
        }
        (Task::none(), None)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let picture: Element<'_, Msg> = match (&self.frame, &self.failed) {
            (_, Some(e)) => column![
                icon(B::CameraVideoOff).size(48).color(theme::MUTED),
                text("Can't play this video").size(16),
                text(e.clone()).size(12).color(theme::MUTED),
                text("macOS plays mp4, m4v and mov (H.264, HEVC, ProRes); not mkv or webm.")
                    .size(12)
                    .color(theme::MUTED),
            ]
            .spacing(6)
            .align_x(Alignment::Center)
            .into(),
            (Some(h), None) => img(h.clone())
                .content_fit(iced::ContentFit::Contain)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            (None, None) => text("Loading…").size(13).color(theme::MUTED).into(),
        };
        let screen = container(picture).center(Length::Fill);

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
                if self.playing { B::PauseFill } else { B::PlayFill },
                Some(Msg::TogglePlay)
            ),
            ibtn(B::SkipEndFill, (!self.playing).then_some(Msg::Step)),
            text(format!("{} / {}", fmt_time(pos), fmt_time(self.info.duration))).size(12),
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

        let info = text(self.info.summary()).size(12).color(theme::MUTED);
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
                        (self.mark_in.is_some() || self.mark_out.is_some()).then_some(Msg::ClearMarks)
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
                text("Exports are new files (keyframe-aligned, no re-encode); the video is never saved over.")
                    .size(12)
                    .color(theme::MUTED),
            );
        }
        col.into()
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
}
