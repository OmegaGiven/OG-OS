//! Phone ↔ PC Bluetooth link for the reverse direction: the PC acts as a
//! Bluetooth headset (A2DP sink + hands-free) for the phone, so calls,
//! notifications and media from the phone play on the PC natively — no
//! capture APIs, no consent prompts. We only decide *when* that link should
//! be up: the phone can send audio to one Bluetooth device at a time, so it
//! must be down whenever the phone has its own headphones.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn btctl(args: &[&str]) -> String {
    Command::new("bluetoothctl")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Address of a paired device whose name is exactly `name` (Android uses
/// the same device name for Bluetooth and for our HELLO).
pub fn find_paired(name: &str) -> Option<String> {
    btctl(&["devices", "Paired"]).lines().find_map(|l| {
        let rest = l.strip_prefix("Device ")?;
        let (addr, n) = rest.split_once(' ')?;
        (n.trim() == name).then(|| addr.to_string())
    })
}

/// The name the phone sees for this PC (so the phone app can ignore it when
/// listing "its own" headphones).
pub fn alias() -> String {
    btctl(&["show"])
        .lines()
        .find_map(|l| l.trim().strip_prefix("Alias: ").map(str::to_string))
        .unwrap_or_default()
}

fn connected(addr: &str) -> bool {
    btctl(&["info", addr]).lines().any(|l| l.trim() == "Connected: yes")
}

/// Drives connect/disconnect for one phone off the main loop (bluetoothctl
/// connect can block for several seconds), rate-limited so a phone that
/// refuses doesn't get hammered.
pub struct PhoneLink {
    pub addr: String,
    busy: Arc<AtomicBool>,
    state: Arc<AtomicBool>,
    checked: Instant,
    /// last attempt: (direction, when) — retry limit is per direction so a
    /// flip (connect → disconnect) is never delayed
    attempted: Option<(bool, Instant)>,
}

const RECHECK: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(20);

impl PhoneLink {
    pub fn new(addr: String) -> Self {
        let up = connected(&addr);
        Self { addr, busy: Arc::default(), state: Arc::new(AtomicBool::new(up)), checked: Instant::now(), attempted: None }
    }

    pub fn is_connected(&mut self) -> bool {
        if self.checked.elapsed() >= RECHECK && !self.busy.load(Ordering::Relaxed) {
            self.state.store(connected(&self.addr), Ordering::Relaxed);
            self.checked = Instant::now();
        }
        self.state.load(Ordering::Relaxed)
    }

    /// Bring the link up (`true`) or down (`false`) if it isn't already.
    pub fn ensure(&mut self, up: bool) {
        let recent = matches!(self.attempted, Some((dir, at)) if dir == up && at.elapsed() < RETRY);
        if self.is_connected() == up || self.busy.load(Ordering::Relaxed) || recent {
            return;
        }
        self.attempted = Some((up, Instant::now()));
        self.busy.store(true, Ordering::Relaxed);
        let (addr, busy, state) = (self.addr.clone(), self.busy.clone(), self.state.clone());
        eprintln!("og-phone-audio: bluetooth {} {addr}", if up { "connect" } else { "disconnect" });
        std::thread::spawn(move || {
            btctl(&[if up { "connect" } else { "disconnect" }, &addr]);
            state.store(connected(&addr), Ordering::Relaxed);
            busy.store(false, Ordering::Relaxed);
        });
    }
}
