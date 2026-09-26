//! The whole recording -> transcribing -> asking-Claude pipeline, run
//! entirely on background threads so the iced event loop (main thread)
//! never blocks. `App` polls `Shared` on a timer (see `app.rs`'s `Tick`)
//! rather than receiving messages through a channel — a plain
//! `Arc<Mutex<Shared>>` is simpler here than threading a non-`Send`
//! `mpsc::Receiver` through iced's `Task`/subscription machinery for
//! what's fundamentally just "latest known state", not a message log.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{claude, stt, wav};

const SAMPLE_RATE: u32 = 16_000;
const CHANNELS: u16 = 1;
const BITS: u16 = 16;
/// ~0.8s chunks (was 1.5s) — chunked-not-true-streaming still, but each
/// chunk buffers less audio before it's sent, so live transcript text
/// lags speech by less. Lower than this and per-chunk overhead (one
/// HTTP round trip + one VAD pass each) starts eating the latency win.
const CHUNK_BYTES: usize = (SAMPLE_RATE as usize) * (CHANNELS as usize) * (BITS as usize / 8) * 4 / 5;

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// `--interactive` launch only: window is open but nothing is
    /// happening yet — waiting for the user to either type a prompt and
    /// hit Enter, or click the mic button to start recording. The
    /// hotkey-triggered default launch never visits this stage; it goes
    /// straight to `StartingStt`.
    Idle,
    /// Waiting on the STT server to come up (only visible if it wasn't
    /// already running when the hotkey was pressed).
    StartingStt,
    Recording,
    Processing,
    Done,
    Failed(String),
    /// Hotkey released with nothing transcribed — close immediately
    /// rather than showing an error, per the "no text = just close"
    /// rule; distinct from `Failed` (STT/Claude errors), which stays
    /// open so the user can read it.
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Shared {
    pub stage: Stage,
    pub transcript: String,
    pub answer: String,
}

impl Default for Shared {
    fn default() -> Self {
        Self { stage: Stage::StartingStt, transcript: String::new(), answer: String::new() }
    }
}

impl Shared {
    /// Initial state for a `--interactive` launch: idle, nothing
    /// recorded/asked yet. Kept as a named constructor next to
    /// `Default` (which stays `StartingStt` for the hotkey path) rather
    /// than adding a bool param everywhere `Shared::default()` is used.
    pub fn idle() -> Self {
        Self { stage: Stage::Idle, transcript: String::new(), answer: String::new() }
    }
}

pub type SharedState = Arc<Mutex<Shared>>;

/// Spawns the capture -> transcribe loop, then (once `stop_flag` is set)
/// the Claude call, updating `shared` throughout. Returns immediately;
/// everything happens on a detached thread. `system_prompt` (loaded once
/// at startup from `~/.config/sway-power/ai-context.json`, see
/// `ai_context.rs`) is passed straight through to `claude::ask`.
///
/// Shared by both launch modes: the default hotkey-triggered launch
/// calls this once at startup (see `main.rs`), and `--interactive`'s
/// in-window mic-toggle button (see `app.rs`) calls it on click — same
/// underlying recording/STT/Claude flow either way, only the trigger
/// differs. The recording-stop half is likewise shared: both the
/// external `--stop` (SIGUSR1 -> `stop_flag`) and the in-window mic
/// button's second click (which just sets `stop_flag` directly, see
/// `app.rs`) are read by the same `stop_flag.load()` in the capture loop
/// below — the loop doesn't know or care which one flipped it.
#[allow(clippy::too_many_arguments)]
pub fn start(
    stt_url: String,
    ai_cli: String,
    system_prompt: Option<String>,
    allow_execution: bool,
    stop_flag: Arc<AtomicBool>,
    shared: SharedState,
) {
    std::thread::spawn(move || {
        if run_recording(&stt_url, &stop_flag, &shared) {
            finish_with_claude(&shared, &ai_cli, system_prompt.as_deref(), allow_execution);
        }
    });
}

/// Typed-text path for `--interactive`: skips capture/STT entirely and
/// goes straight from the submitted string into the same
/// "final transcript ready -> call Claude" tail (`finish_with_claude`)
/// the voice path ends with, so history logging and answer/error
/// handling behave identically regardless of how the transcript was
/// produced.
pub fn submit_text(text: String, ai_cli: String, system_prompt: Option<String>, allow_execution: bool, shared: SharedState) {
    std::thread::spawn(move || {
        {
            let mut s = shared.lock().unwrap();
            s.transcript = text;
            s.stage = Stage::Processing;
        }
        finish_with_claude(&shared, &ai_cli, system_prompt.as_deref(), allow_execution);
    });
}

/// Runs STT server startup + `parec` capture + per-chunk transcription
/// until `stop_flag` is set, updating `shared.transcript` as chunks come
/// back. Returns `true` if it reached the natural end of the recording
/// (so the caller should proceed to `finish_with_claude`), `false` if it
/// bailed out early with `shared.stage` already set to `Failed` (STT
/// unreachable or `parec` wouldn't start) — in which case there's
/// nothing to send to Claude.
fn run_recording(stt_url: &str, stop_flag: &AtomicBool, shared: &SharedState) -> bool {
    if !stt::ensure_running(stt_url) {
        let mut s = shared.lock().unwrap();
        s.stage = Stage::Failed("STT server unavailable — see stt-server/README or run scripts/install-stt.sh".to_string());
        return false;
    }

    // Defensive reset: on the hotkey path this is always already `false`
    // (a fresh `AtomicBool` per process). On the `--interactive` path the
    // same process/flag can outlive an idle period between recordings —
    // a stray `--stop` SIGUSR1 delivered while idle (e.g. a hotkey
    // release racing an interactive window) shouldn't cause the *next*
    // recording to stop instantly.
    stop_flag.store(false, Ordering::Relaxed);

    {
        let mut s = shared.lock().unwrap();
        s.stage = Stage::Recording;
    }

    let Some(mut child) = spawn_parec() else {
        let mut s = shared.lock().unwrap();
        s.stage = Stage::Failed("failed to start `parec` — is PulseAudio/PipeWire running?".to_string());
        return false;
    };

    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut buf = vec![0u8; 4096];
    let mut chunk: Vec<u8> = Vec::with_capacity(CHUNK_BYTES);

    loop {
        let stop = stop_flag.load(Ordering::Relaxed);

        match stdout.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => chunk.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }

        if chunk.len() >= CHUNK_BYTES || (stop && !chunk.is_empty()) {
            let wav_bytes = wav::pcm_to_wav(&chunk, SAMPLE_RATE, CHANNELS, BITS);
            chunk.clear();
            // Tail of what's been transcribed so far this recording, fed
            // to the STT server as Whisper's `initial_prompt` — gives
            // each otherwise-independent chunk some continuity instead
            // of starting cold. Last ~200 chars is plenty of context
            // without the prompt itself dominating the transcribe call.
            let prior_context = {
                let s = shared.lock().unwrap();
                let t = &s.transcript;
                let char_count = t.chars().count();
                let skip = char_count.saturating_sub(200);
                t.chars().skip(skip).collect::<String>()
            };
            if let Ok(text) = stt::transcribe(stt_url, &wav_bytes, &prior_context) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    let mut s = shared.lock().unwrap();
                    if !s.transcript.is_empty() {
                        s.transcript.push(' ');
                    }
                    s.transcript.push_str(trimmed);
                }
            }
        }

        if stop {
            break;
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    true
}

/// The tail shared by both the voice path (after `run_recording`) and
/// the typed-text path (`submit_text`): moves to `Processing`, calls
/// Claude, logs to history, and lands on `Done`/`Failed`.
fn finish_with_claude(shared: &SharedState, ai_cli: &str, system_prompt: Option<&str>, allow_execution: bool) {
    let transcript = {
        let mut s = shared.lock().unwrap();
        s.stage = Stage::Processing;
        s.transcript.clone()
    };

    if transcript.trim().is_empty() {
        let mut s = shared.lock().unwrap();
        s.stage = Stage::Cancelled;
        return;
    }

    match claude::ask(ai_cli, &transcript, system_prompt, allow_execution) {
        Ok(result) => {
            crate::history::append(&transcript, &result.answer, result.actions);
            let mut s = shared.lock().unwrap();
            s.answer = result.answer;
            s.stage = Stage::Done;
        }
        Err(err) => {
            crate::history::append(&transcript, &format!("Error: {err}"), Vec::new());
            let mut s = shared.lock().unwrap();
            s.stage = Stage::Failed(err);
        }
    }
}

fn spawn_parec() -> Option<Child> {
    // Same invocation shape as sway-control's audio_meter.rs (the
    // suite's other `parec` consumer): `--latency-msec=20` avoids
    // PulseAudio buffering ~2s server-side before delivering anything,
    // which would otherwise make "live" transcription anything but.
    Command::new("parec")
        .args([
            "--raw",
            "--format=s16le",
            &format!("--rate={SAMPLE_RATE}"),
            &format!("--channels={CHANNELS}"),
            "--latency-msec=20",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()
}

/// Small helper so `app.rs` doesn't need to import `Duration` just for
/// the poll interval constant.
pub const POLL_INTERVAL: Duration = Duration::from_millis(150);
