//! og-phone-audio — streams this PC's audio to the OG Phone Audio Android
//! app over the LAN, so headphones paired to the phone hear the PC and
//! the phone together (Android's own mixer does the mixing).
//!
//! Flow:
//!   1. The phone app beacons a HELLO (UDP broadcast → :47800) every second
//!      while it's on Wi-Fi, carrying the pair code and its playback stats.
//!   2. On the first HELLO with the right code we load a null sink
//!      (`og_phone`), optionally make it the default output, and start
//!      `parec` on its monitor with a tiny latency target.
//!   3. Raw s16le/48k/stereo PCM goes out in 2.5/5/10 ms UDP packets
//!      straight back to the phone. No codec — 1.5 Mbit/s is nothing on a
//!      LAN, and it costs zero encode/decode latency.
//!   4. After `SILENCE_GATE` of digital silence the pump stops sending, so
//!      the phone can release its AudioTrack/Wi-Fi lock and the headphones
//!      idle; the first non-silent packet resumes it.
//!   Direction: the phone reports whether it has its own headphones. If it
//!   does (or `direction` = "to_phone") we relay PC → phone as above. If it
//!   doesn't, the relay stays off and we bring up the phone's Bluetooth link
//!   to this PC instead, so phone audio — calls included — plays here
//!   (see `bt.rs`). The Wi-Fi link stays up either way as the control channel.
//!   5. No HELLO for `PEER_TIMEOUT` → restore the old default output,
//!      unload the sink, go back to waiting. While waiting, PING the last
//!      phone's :47801 every `PING_EVERY`, which makes an idle phone (one
//!      that gave up searching) look for us again.
//!
//! Wire format (all little-endian):
//!   HELLO  phone→pc  "OGPA" 0x01 <json>
//!   AUDIO  pc→phone  "OGPA" 0x02 ch:u8 frames:u16 seq:u32 rate:u32 <pcm>
//!   ACK    pc→phone  "OGPA" 0x03 <json>       (reply to every HELLO)
//!   BYE    pc→phone  "OGPA" 0x04              (daemon stopping / switching)
//!   PING   pc→phone  "OGPA" 0x05              (to last phone's :47801 while idle)
//!
//! Live status for og-settings goes to `$XDG_RUNTIME_DIR/og-phone-audio.json`.

mod bt;
mod config;
mod sink;

use config::PhoneAudioConfig;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::{SocketAddr, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAGIC: &[u8; 4] = b"OGPA";
const T_HELLO: u8 = 1;
const T_AUDIO: u8 = 2;
const T_ACK: u8 = 3;
const T_BYE: u8 = 4;
const T_PING: u8 = 5;
const PHONE_PORT: u16 = 47801;
const RATE: u32 = 48_000;
const CHANNELS: u8 = 2;
/// phone heartbeats every 4 s while linked-but-silent, every 1 s streaming
const PEER_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(10);
const SILENCE_GATE: Duration = Duration::from_secs(5);
/// |sample| at or below this counts as silence (covers dither)
const SILENCE_LEVEL: i16 = 2;

#[derive(Debug, Deserialize, Default)]
struct Hello {
    #[serde(default)]
    name: String,
    #[serde(default)]
    code: String,
    /// phone-side stats, echoed into the status file
    #[serde(default)]
    buf_ms: f32,
    #[serde(default)]
    underruns: u64,
    #[serde(default)]
    lost: u64,
    #[serde(default)]
    late: u64,
    /// AudioTrack's own output latency estimate incl. Bluetooth, if known
    #[serde(default)]
    out_ms: f32,
    /// phone has its own headphones (Bluetooth other than this PC, wired, USB)
    #[serde(default)]
    headset: bool,
    /// names of those outputs, for the status display
    #[serde(default)]
    outputs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// PC audio → phone over Wi-Fi
    Relay,
    /// phone audio → PC over Bluetooth
    Bluetooth,
    /// neither: no headphones on the phone and it isn't paired with the PC
    Off,
}

impl Route {
    fn as_str(self) -> &'static str {
        match self {
            Route::Relay => "relay",
            Route::Bluetooth => "bluetooth",
            Route::Off => "off",
        }
    }
}

#[derive(Debug, Serialize, Default)]
struct Status {
    pid: u32,
    connected: bool,
    phone_name: String,
    phone_addr: String,
    since: u64,
    frame_ms: f32,
    sent: u64,
    phone_buf_ms: f32,
    phone_out_ms: f32,
    underruns: u64,
    lost: u64,
    late: u64,
    /// last HELLO from a phone with the wrong pair code, so the UI can say
    /// "Pixel 8 tried to connect — check the code"
    rejected: String,
    /// "relay" / "bluetooth" / "off"
    route: String,
    phone_headset: bool,
    phone_outputs: Vec<String>,
    bt_paired: bool,
    bt_connected: bool,
    updated: u64,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn status_path() -> std::path::PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    std::path::PathBuf::from(dir).join("og-phone-audio.json")
}

fn write_status(s: &Status) {
    let p = status_path();
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_vec(s).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, p);
    }
}

fn last_phone_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"));
    base.join("og-phone-audio/last-phone")
}

fn load_last_phone() -> Option<std::net::IpAddr> {
    std::fs::read_to_string(last_phone_path()).ok()?.trim().parse().ok()
}

fn save_last_phone(ip: std::net::IpAddr) {
    let p = last_phone_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, ip.to_string());
}

fn is_silent(pcm: &[u8]) -> bool {
    pcm.chunks_exact(2).all(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs() <= SILENCE_LEVEL as u16)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "PC".into())
}

/// PC → phone relay: the sink we loaded + the parec→UDP pump.
struct Session {
    peer: Arc<Mutex<SocketAddr>>,
    stop: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    parec: Child,
    pump: Option<std::thread::JoinHandle<()>>,
    sink: sink::PhoneSink,
}

impl Session {
    fn start(sock: &UdpSocket, peer: SocketAddr, cfg: &PhoneAudioConfig) -> Result<Self, String> {
        let sink = sink::PhoneSink::load(cfg.auto_default)?;
        let frame_ms = cfg.frame_ms;
        let frames = (RATE as f32 * frame_ms / 1000.0) as usize;

        // --latency-msec asks PipeWire for a small fragment, which also pulls
        // the graph quantum down from the 1024-sample default (~21 ms).
        let mut parec = match Command::new("parec")
            .args([
                "-d",
                &format!("{}.monitor", sink::SINK_NAME),
                "--format=s16le",
                &format!("--rate={RATE}"),
                &format!("--channels={CHANNELS}"),
                &format!("--latency-msec={}", frame_ms.max(2.5)),
                "--raw",
                "--client-name=og-phone-audio",
                "--stream-name=Phone relay",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                sink.unload();
                return Err(format!("parec: {e}"));
            }
        };
        let mut out = parec.stdout.take().ok_or("parec: no stdout")?;

        let peer = Arc::new(Mutex::new(peer));
        let stop = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicU64::new(0));
        let tx = sock.try_clone().map_err(|e| e.to_string())?;
        let (p2, s2, n2) = (peer.clone(), stop.clone(), sent.clone());

        let pump = std::thread::spawn(move || {
            let bytes = frames * CHANNELS as usize * 2;
            let mut pkt = vec![0u8; 16 + bytes];
            pkt[..4].copy_from_slice(MAGIC);
            pkt[4] = T_AUDIO;
            pkt[5] = CHANNELS;
            pkt[6..8].copy_from_slice(&(frames as u16).to_le_bytes());
            pkt[12..16].copy_from_slice(&RATE.to_le_bytes());
            let mut seq: u32 = 0;
            let gate_after = (SILENCE_GATE.as_secs_f32() * 1000.0 / frame_ms) as u32;
            let mut silent_run: u32 = 0;
            while !s2.load(Ordering::Relaxed) {
                if out.read_exact(&mut pkt[16..]).is_err() {
                    break;
                }
                // Keep reading parec either way (so its buffer never builds
                // up latency), but stop transmitting during long silence.
                if is_silent(&pkt[16..]) {
                    silent_run = silent_run.saturating_add(1);
                    if silent_run > gate_after {
                        continue;
                    }
                } else {
                    silent_run = 0;
                }
                pkt[8..12].copy_from_slice(&seq.to_le_bytes());
                seq = seq.wrapping_add(1);
                let to = *p2.lock().unwrap();
                let _ = tx.send_to(&pkt, to);
                n2.fetch_add(1, Ordering::Relaxed);
            }
        });

        Ok(Self { peer, stop, sent, parec, pump: Some(pump), sink })
    }

    fn end(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.parec.kill();
        let _ = self.parec.wait();
        if let Some(h) = self.pump.take() {
            let _ = h.join();
        }
        self.sink.unload();
    }
}

/// The phone we're currently talking to over Wi-Fi (control channel),
/// whether or not PC audio is being relayed to it.
struct Link {
    name: String,
    addr: SocketAddr,
    last: Instant,
    since: u64,
    /// Bluetooth side, if this phone is paired with the PC
    bt: Option<bt::PhoneLink>,
    bt_lookup: Instant,
}

fn decide(direction: &str, headset: bool, paired: bool) -> Route {
    match direction {
        "to_phone" => Route::Relay,
        "to_pc" if paired => Route::Bluetooth,
        "to_pc" => Route::Off,
        _ if headset => Route::Relay,
        _ if paired => Route::Bluetooth,
        _ => Route::Off,
    }
}

fn send_ctl(sock: &UdpSocket, t: u8, to: SocketAddr) {
    let _ = sock.send_to(&[b'O', b'G', b'P', b'A', t], to);
}

fn main() {
    let cfg = PhoneAudioConfig::load();
    let term = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT, signal_hook::consts::SIGHUP] {
        let _ = signal_hook::flag::register(sig, term.clone());
    }

    // A crash last time may have left our sink loaded / the default pointed
    // at it — clear that before anything else.
    sink::cleanup_stale();

    let sock = match UdpSocket::bind(("0.0.0.0", cfg.port)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("og-phone-audio: bind :{}: {e}", cfg.port);
            std::process::exit(1);
        }
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(250)));
    let host = hostname();
    let bt_alias = bt::alias();
    eprintln!("og-phone-audio: listening on :{} ({} ms packets, direction {})", cfg.port, cfg.frame_ms, cfg.direction);

    let mut link: Option<Link> = None;
    let mut relay: Option<Session> = None;
    let mut status = Status { pid: std::process::id(), frame_ms: cfg.frame_ms, ..Default::default() };
    write_status(&status);
    let mut buf = [0u8; 2048];
    let mut last_phone = load_last_phone();
    let mut last_ping = Instant::now() - PING_EVERY;

    while !term.load(Ordering::Relaxed) {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if n > 5 && &buf[..4] == MAGIC && buf[4] == T_HELLO => {
                let hello: Hello = serde_json::from_slice(&buf[5..n]).unwrap_or_default();
                if hello.code != cfg.pair_code {
                    status.rejected = format!("{} ({})", hello.name, from.ip());
                    write_status(&status);
                    continue;
                }

                // One phone at a time: a different phone only takes over once
                // the current one has gone quiet.
                if let Some(l) = &link {
                    if l.name != hello.name && l.last.elapsed() < PEER_TIMEOUT {
                        continue;
                    }
                }
                if link.as_ref().map(|l| l.name != hello.name).unwrap_or(true) {
                    if let Some(old) = relay.take() {
                        send_ctl(&sock, T_BYE, *old.peer.lock().unwrap());
                        old.end();
                    }
                    let bt = bt::find_paired(&hello.name).map(bt::PhoneLink::new);
                    eprintln!(
                        "og-phone-audio: linked to {} at {from} (bluetooth {})",
                        hello.name,
                        bt.as_ref().map(|b| b.addr.as_str()).unwrap_or("not paired")
                    );
                    link = Some(Link { name: hello.name.clone(), addr: from, last: Instant::now(), since: now_secs(), bt, bt_lookup: Instant::now() });
                    if last_phone != Some(from.ip()) && !from.ip().is_loopback() {
                        last_phone = Some(from.ip());
                        save_last_phone(from.ip());
                    }
                }
                let l = link.as_mut().unwrap();
                l.addr = from;
                l.last = Instant::now();
                // Paired after we linked? Pick it up (cheap: at most every 10 s).
                if l.bt.is_none() && l.bt_lookup.elapsed() >= Duration::from_secs(10) {
                    l.bt_lookup = Instant::now();
                    l.bt = bt::find_paired(&l.name).map(bt::PhoneLink::new);
                    if let Some(b) = &l.bt {
                        eprintln!("og-phone-audio: {} is now paired over bluetooth ({})", l.name, b.addr);
                    }
                }

                // ── route ──
                let route = decide(&cfg.direction, hello.headset, l.bt.is_some());
                match route {
                    Route::Relay => {
                        if let Some(b) = l.bt.as_mut() {
                            b.ensure(false);
                        }
                        match &relay {
                            Some(r) => *r.peer.lock().unwrap() = from,
                            None => match Session::start(&sock, from, &cfg) {
                                Ok(r) => {
                                    eprintln!("og-phone-audio: relaying PC audio to {}", hello.name);
                                    relay = Some(r);
                                }
                                Err(e) => eprintln!("og-phone-audio: relay start failed: {e}"),
                            },
                        }
                    }
                    Route::Bluetooth | Route::Off => {
                        // No BYE here — the Wi-Fi link stays up as the control
                        // channel; the phone just stops getting audio.
                        if let Some(r) = relay.take() {
                            eprintln!("og-phone-audio: relay off ({})", route.as_str());
                            r.end();
                        }
                        if route == Route::Bluetooth {
                            l.bt.as_mut().unwrap().ensure(true);
                        }
                    }
                }

                let ack = serde_json::json!({
                    "host": host,
                    "frame_ms": cfg.frame_ms,
                    "route": route.as_str(),
                    "bt_name": bt_alias,
                });
                let mut pkt = Vec::with_capacity(96);
                pkt.extend_from_slice(MAGIC);
                pkt.push(T_ACK);
                pkt.extend_from_slice(ack.to_string().as_bytes());
                let _ = sock.send_to(&pkt, from);

                status.connected = true;
                status.phone_name = hello.name;
                status.phone_addr = from.ip().to_string();
                status.since = l.since;
                status.sent = relay.as_ref().map(|r| r.sent.load(Ordering::Relaxed)).unwrap_or(0);
                status.phone_buf_ms = hello.buf_ms;
                status.phone_out_ms = hello.out_ms;
                status.underruns = hello.underruns;
                status.lost = hello.lost;
                status.late = hello.late;
                status.route = route.as_str().into();
                status.phone_headset = hello.headset;
                status.phone_outputs = hello.outputs;
                status.bt_paired = l.bt.is_some();
                status.bt_connected = l.bt.as_mut().map(|b| b.is_connected()).unwrap_or(false);
                status.rejected.clear();
                status.updated = now_secs();
                write_status(&status);
            }
            _ => {}
        }

        if link.is_none() && last_ping.elapsed() >= PING_EVERY {
            if let Some(ip) = last_phone {
                send_ctl(&sock, T_PING, SocketAddr::new(ip, PHONE_PORT));
            }
            last_ping = Instant::now();
        }

        if link.as_ref().is_some_and(|l| l.last.elapsed() > PEER_TIMEOUT) {
            let l = link.take().unwrap();
            eprintln!("og-phone-audio: {} went away", l.name);
            if let Some(r) = relay.take() {
                r.end();
            }
            // Bluetooth is left alone: if the phone walked off it drops by
            // itself, and if only Wi-Fi blinked the call/audio keeps going.
            status = Status { pid: status.pid, frame_ms: cfg.frame_ms, updated: now_secs(), ..Default::default() };
            write_status(&status);
        }
    }

    if let Some(r) = relay.take() {
        send_ctl(&sock, T_BYE, *r.peer.lock().unwrap());
        r.end();
    } else if let Some(l) = &link {
        send_ctl(&sock, T_BYE, l.addr);
    }
    let _ = std::fs::remove_file(status_path());
}
