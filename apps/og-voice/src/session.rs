//! Cross-turn conversation continuity for `claude -p`.
//!
//! `claude -p` is stateless per invocation by default — each call starts
//! a brand new conversation. To let consecutive og-voice turns (hotkey
//! press -> answer -> press again, see `app.rs`'s `--start` handling)
//! feel like one ongoing conversation instead of amnesia every time, we
//! pin a session id and pass `--resume <id>` on every call after the
//! first, which used `--session-id <id>` to create it.
//!
//! The session id + its start time are persisted to a small JSON file
//! rather than kept in memory, since og-voice's own process exits
//! between turns whenever the popup fully closes (25s idle timeout /
//! click-away / Esc) — only the `--start` fast-path keeps one process
//! alive across turns, and this needs to survive fresh launches too.
//!
//! Flushed (a new id generated) once the session is more than an hour
//! old, per the "hold context, flush every hour or so" ask — deliberately
//! a fixed wall-clock window from when the session started, not a sliding
//! idle timeout, so a burst of rapid-fire turns doesn't keep resetting it
//! and drift the context forward forever.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const FLUSH_AFTER_SECS: u64 = 3600;

#[derive(Serialize, Deserialize)]
struct SessionState {
    id: String,
    started_at: u64,
}

fn state_path() -> PathBuf {
    og_config::dirs_home().join(".local/share/og-voice/session.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Reads a fresh random UUID straight from the kernel — no `uuid` crate
/// needed for the one thing this app uses it for. Linux-only, but so is
/// the rest of this suite (see `lock.rs`'s `pid_alive` for the same
/// assumption elsewhere).
fn new_uuid() -> Option<String> {
    let text = fs::read_to_string("/proc/sys/kernel/random/uuid").ok()?;
    let id = text.trim().to_string();
    (!id.is_empty()).then_some(id)
}

/// What `claude::ask` should pass for session continuity this turn.
pub enum Continuity {
    /// First call for this session id — pass `--session-id <id>`.
    Fresh(String),
    /// Continuing an existing, still-fresh session — pass `--resume <id>`.
    Resume(String),
    /// Couldn't read/write session state or mint a UUID — fall back to
    /// the old stateless behavior rather than failing the whole request.
    None,
}

/// Loads the current session if it's under an hour old, otherwise mints
/// a new one and persists it. Best-effort throughout: any I/O failure
/// just degrades to `Continuity::None` (today's stateless behavior)
/// instead of blocking the user's request.
pub fn current() -> Continuity {
    let path = state_path();

    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(state) = serde_json::from_str::<SessionState>(&text) {
            if now().saturating_sub(state.started_at) < FLUSH_AFTER_SECS {
                return Continuity::Resume(state.id);
            }
        }
    }

    let Some(id) = new_uuid() else { return Continuity::None };
    let state = SessionState { id: id.clone(), started_at: now() };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(json) = serde_json::to_string(&state) else { return Continuity::None };
    if fs::write(&path, json).is_err() {
        return Continuity::None;
    }
    Continuity::Fresh(id)
}
