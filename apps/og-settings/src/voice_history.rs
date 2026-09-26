//! Read-only side of og-voice's session log — writing lives entirely in
//! og-voice (`og-voice/src/history.rs`); this module only ever reads
//! `~/.config/sway-power/voice-history.jsonl` for the "History" card in
//! the AI Context tab (`tabs/ai_context.rs`).
//!
//! JSONL (one JSON object per line, newest appended last). Any line that
//! fails to parse is skipped rather than erroring the whole load — same
//! best-effort convention as `ai_context.rs`'s `load()` for a
//! missing/corrupt file.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceAction {
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VoiceHistoryEntry {
    pub timestamp: String,
    pub transcript: String,
    pub response: String,
    #[serde(default)]
    pub actions: Vec<VoiceAction>,
}

fn history_path() -> PathBuf {
    let mut p = crate::config::dirs_home();
    p.push(".config/sway-power/voice-history.jsonl");
    p
}

/// Loads the most recent `limit` entries, newest first. Missing file is
/// just "no history yet", not an error. Only the last `limit` lines of
/// the file are parsed (rather than the whole file then truncated) so a
/// large log doesn't cost more than it needs to just to show a handful
/// of recent sessions.
pub fn load_recent(limit: usize) -> Vec<VoiceHistoryEntry> {
    let Ok(text) = std::fs::read_to_string(history_path()) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(limit);
    lines[start..]
        .iter()
        .rev()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<VoiceHistoryEntry>(line).ok()
        })
        .collect()
}

/// Deletes the whole history log. Best-effort: a missing file is not an
/// error (nothing to clear), any other I/O error is swallowed the same
/// way `ai_context.rs`'s `save()` treats write failures elsewhere in this
/// tab — the button just won't visibly do anything rather than crashing
/// the settings app over a log file.
pub fn clear_all() {
    match std::fs::remove_file(history_path()) {
        Ok(()) | Err(_) => {}
    }
}

/// Rewrites the log to keep only the most recent `keep` entries (oldest
/// dropped first). No-op (not an error) if the file is missing or already
/// has `keep` or fewer lines.
pub fn keep_recent(keep: usize) {
    let path = history_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= keep {
        return;
    }
    let start = lines.len() - keep;
    let trimmed = lines[start..].join("\n") + "\n";
    let _ = std::fs::write(&path, trimmed);
}
