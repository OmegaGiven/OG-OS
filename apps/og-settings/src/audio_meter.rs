//! Live input/output level metering for the Audio tab.
//!
//! `pactl` only exposes static volume/mute — no running peak — so this
//! shells out to `parec` (already on this box alongside `pactl`, same
//! pulseaudio/pipewire-pulse CLI stack) to stream raw PCM from a source
//! and computes a rolling peak in a background thread. That's one less
//! dependency than pulling in `cpal` for the same job, and `parec` works
//! identically against any pulse-compatible source name — a real input
//! device or a sink's `<name>.monitor` for output metering.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;
use std::thread;

pub struct Meter {
    // Tenths of a percent (0-1000) — a plain 0-100 u8 quantized the smoothed
    // level into visible integer-percent steps ("notches") when rendered;
    // this gives the bar sub-percent precision to move continuously.
    level: Arc<AtomicU16>,
    child: Child,
}

impl Meter {
    /// Latest peak amplitude, 0.0-100.0.
    pub fn level_pct(&self) -> f32 {
        self.level.load(Ordering::Relaxed) as f32 / 10.0
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `parec` against `source_name` and starts a thread computing its
/// rolling peak. Returns `None` if `parec` isn't available or the source
/// can't be opened (device just unplugged, etc).
pub fn spawn(source_name: &str) -> Option<Meter> {
    // `--latency-msec=20` matters more than it looks: without it PulseAudio
    // queues ~2s of audio server-side before parec sees any of it, which is
    // exactly why the meter felt laggy compared to pavucontrol (which asks
    // for low-latency delivery through the native stream API, not a CLI
    // default). This makes parec ask for the same thing.
    let mut child = Command::new("parec")
        .args(["--raw", "--format=s16le", "--rate=16000", "--channels=1", "--latency-msec=20", "-d", source_name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let level = Arc::new(AtomicU16::new(0));
    let level_writer = level.clone();

    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        // Plain max-sample-per-chunk (what we had) reacts to every single
        // spike, which reads as jittery/"messy" — a real meter's needle
        // has mass. Standard VU ballistics: fast-ish attack (~30% toward
        // the new peak per update) so it doesn't feel laggy, slow release
        // (~6% per update) so it settles instead of chattering. `smoothed`
        // is carried across loop iterations, not stored raw.
        let mut smoothed: f32 = 0.0;
        loop {
            match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let peak = buf[..n]
                        .chunks_exact(2)
                        .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
                        .max()
                        .unwrap_or(0);
                    let mut instant = peak as f32 / i16::MAX as f32;
                    // Noise floor — near-silence shouldn't flicker.
                    if instant < 0.02 {
                        instant = 0.0;
                    }

                    let alpha = if instant > smoothed { 0.3 } else { 0.06 };
                    smoothed += alpha * (instant - smoothed);

                    let tenths = (smoothed * 1000.0).clamp(0.0, 1000.0) as u16;
                    level_writer.store(tenths, Ordering::Relaxed);
                }
            }
        }
    });

    Some(Meter { level, child })
}
