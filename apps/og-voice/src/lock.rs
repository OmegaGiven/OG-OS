//! Single-instance push-to-talk coordination.
//!
//! `og-voice` (press) writes its own PID to a lock file under
//! `$XDG_RUNTIME_DIR` (falling back to `/tmp` if unset). `og-voice --stop`
//! (release) reads that file and sends the already-running instance
//! SIGUSR1 rather than starting a second process — see `main.rs` for why
//! a plain Unix signal was chosen over a socket (simpler to get right in
//! a couple lines, and the kernel already guarantees delivery/atomicity
//! we'd otherwise have to build ourselves with a socket).

use std::path::PathBuf;
use std::process::Command;

fn lock_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    let mut p = PathBuf::from(dir);
    p.push("og-voice.pid");
    p
}

/// Is the PID in the lock file still a live process? Checking `/proc/<pid>`
/// existing is enough on Linux (this suite only ever targets Linux/sway)
/// without needing a signal(0)-style syscall wrapper crate.
fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Returns `Some(pid)` if a live og-voice instance already holds the
/// lock, `None` otherwise (including a stale lock file from a crashed
/// process — treated the same as no lock at all).
pub fn running_pid() -> Option<u32> {
    let text = std::fs::read_to_string(lock_path()).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    if pid_alive(pid) {
        Some(pid)
    } else {
        None
    }
}

/// Claims the lock for the current process. Call only after confirming
/// `running_pid()` is `None` — this always overwrites.
pub fn acquire() {
    let _ = std::fs::write(lock_path(), std::process::id().to_string());
}

/// Removes the lock file, but only if it's still ours — avoids a race
/// where this process exits after a *newer* instance has already
/// re-acquired the lock (shouldn't happen given the guard in `main.rs`,
/// but cheap to be defensive about).
pub fn release() {
    if let Ok(text) = std::fs::read_to_string(lock_path()) {
        if text.trim().parse::<u32>() == Ok(std::process::id()) {
            let _ = std::fs::remove_file(lock_path());
        }
    }
}

/// `--stop` entry point: signal the running instance (if any) to stop
/// recording and move to "processing". Shells out to `kill` rather than
/// pulling in a libc/nix dependency just for one syscall.
pub fn send_stop() -> bool {
    let Some(pid) = running_pid() else { return false };
    Command::new("kill")
        .args(["-USR1", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `--start` entry point: a hotkey press while an instance is already
/// running (i.e. the popup is still open showing a `Done`/`Failed`/
/// `Cancelled` answer from the previous turn) signals it to begin a new
/// recording turn in place, rather than launching a second window. Same
/// SIGUSR1-vs-socket reasoning as `send_stop`, just SIGUSR2 so the two
/// signals don't collide.
pub fn send_start() -> bool {
    let Some(pid) = running_pid() else { return false };
    Command::new("kill")
        .args(["-USR2", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
