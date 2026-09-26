//! systemd watchdog heartbeat.
//!
//! `Restart=always` only covers a *dead* process. The DPMS-wake bug
//! (layer surface closed → Vulkan present blocked forever) left og-bar
//! alive but frozen for hours, and systemd saw nothing wrong. With
//! `WatchdogSec=` in the unit, systemd expects `WATCHDOG=1` pings on
//! `$NOTIFY_SOCKET`; we send them from inside iced's update loop, so a
//! hung loop means no pings and systemd kills + restarts the bar.
//!
//! Hand-rolled sd_notify (one datagram) — not worth a crate.

use std::os::unix::net::UnixDatagram;
use std::sync::OnceLock;

fn socket() -> Option<&'static (UnixDatagram, String)> {
    static SOCK: OnceLock<Option<(UnixDatagram, String)>> = OnceLock::new();
    SOCK.get_or_init(|| {
        let path = std::env::var("NOTIFY_SOCKET").ok()?;
        let sock = UnixDatagram::unbound().ok()?;
        Some((sock, path))
    })
    .as_ref()
}

fn notify(msg: &str) {
    if let Some((sock, path)) = socket() {
        // Abstract-namespace sockets start with '@' in the env var.
        let target = if let Some(rest) = path.strip_prefix('@') {
            format!("\0{rest}")
        } else {
            path.clone()
        };
        let _ = sock.send_to(msg.as_bytes(), target);
    }
}

pub fn ready() {
    notify("READY=1");
}

pub fn ping() {
    notify("WATCHDOG=1");
}

/// How often to ping. Must be comfortably under the unit's WatchdogSec.
pub const PING_SECS: u64 = 10;
