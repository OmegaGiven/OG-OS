//! Append-only session log: one JSON object per line (JSONL) written to
//! `~/.config/sway-power/voice-history.jsonl`, one record per completed
//! voice session (success, Claude-reported error, or "nothing was
//! transcribed"). Appending (rather than read-modify-write) is the point
//! — this file only ever grows, and appending stays O(1) regardless of
//! how big it gets, unlike `ai-context.json`'s read-whole/write-whole
//! save style which would get slower every session.
//!
//! Read side lives in og-settings (`sway-control/src/voice_history.rs`)
//! for the read-only History viewer in the AI Context tab — this module
//! only ever writes.
//!
//! Best-effort throughout, same convention as `ai_context.rs`/`config.rs`
//! in this crate and `ai_context.rs::save()` in sway-control: a failure
//! to create the directory, serialize, or open/write the file is
//! swallowed rather than surfaced, since losing a history entry is far
//! preferable to crashing or hanging the (already-answered) voice popup
//! over a logging failure.
//!
//! Known limitation (deliberately out of scope for this prototype, per
//! the brief): no rotation/pruning. The file grows forever; the reader
//! caps how much it loads at once, but nothing here trims old entries.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

/// One tool call Claude made while producing a response. `summary` is a
/// short human-readable description derived from the tool's input (the
/// command string for `Bash`, the file path for `Edit`/`Write`/`Read`,
/// etc. — see `summarize_tool_input` in `claude.rs`) rather than the raw
/// JSON input, so it reads cleanly in the History viewer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// RFC3339 UTC, e.g. `2026-08-08T16:10:16Z`.
    pub timestamp: String,
    pub transcript: String,
    pub response: String,
    #[serde(default)]
    pub actions: Vec<Action>,
}

fn history_path() -> PathBuf {
    let mut p = og_config::dirs_home();
    p.push(".config/sway-power/voice-history.jsonl");
    p
}

/// Appends one record. Best-effort — see module docs; never panics, never
/// blocks longer than a single small synchronous file write.
pub fn append(transcript: &str, response: &str, actions: Vec<Action>) {
    let entry = HistoryEntry {
        timestamp: now_rfc3339(),
        transcript: transcript.to_string(),
        response: response.to_string(),
        actions,
    };
    let Ok(line) = serde_json::to_string(&entry) else { return };

    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Hand-rolled UTC RFC3339 formatting (`std::time::SystemTime` only, no
/// `chrono`) — deliberate to avoid adding a dependency og-voice doesn't
/// otherwise need (sway-control already has `chrono`, but og-voice
/// doesn't, and this is the only place that would want it). Civil-date
/// math is Howard Hinnant's well-known `civil_from_days` algorithm.
fn now_rfc3339() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}
