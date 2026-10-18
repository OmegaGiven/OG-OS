//! Tiny sway IPC client: tells the procedural scenes which outputs actually show
//! the desktop, event-driven (no polling). A background thread subscribes to
//! workspace/window/output events, and after each burst asks for the tree and
//! reports the outputs whose visible workspace has no tiled windows.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const MAGIC: &[u8; 6] = b"i3-ipc";
const SUBSCRIBE: u32 = 2;
const GET_TREE: u32 = 4;

fn socket_path() -> Option<String> {
    if let Ok(p) = std::env::var("SWAYSOCK") {
        return Some(p);
    }
    // Full path: sway's exec environment may not have /usr/bin on PATH.
    let out = std::process::Command::new("/usr/bin/sway").arg("--get-socketpath").output().ok()?;
    let p = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!p.is_empty()).then_some(p)
}

fn send(s: &mut UnixStream, ty: u32, payload: &[u8]) -> io::Result<()> {
    let mut msg = Vec::with_capacity(14 + payload.len());
    msg.extend_from_slice(MAGIC);
    msg.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    msg.extend_from_slice(&ty.to_ne_bytes());
    msg.extend_from_slice(payload);
    s.write_all(&msg)
}

fn recv(s: &mut UnixStream) -> io::Result<(u32, Vec<u8>)> {
    let mut head = [0u8; 14];
    s.read_exact(&mut head)?;
    if &head[..6] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "bad sway ipc magic"));
    }
    let len = u32::from_ne_bytes(head[6..10].try_into().unwrap()) as usize;
    let ty = u32::from_ne_bytes(head[10..14].try_into().unwrap());
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    Ok((ty, body))
}

/// Outputs whose visible workspace has no tiled windows (floating ones don't
/// count — they leave most of the desktop showing).
pub fn exposed_outputs(tree: &serde_json::Value) -> Vec<String> {
    let mut exposed = Vec::new();
    for output in tree["nodes"].as_array().into_iter().flatten() {
        let (Some(name), Some(current)) = (output["name"].as_str(), output["current_workspace"].as_str()) else {
            continue;
        };
        let covered = output["nodes"].as_array().into_iter().flatten().any(|ws| {
            ws["name"].as_str() == Some(current) && ws["nodes"].as_array().is_some_and(|n| !n.is_empty())
        });
        if !covered {
            exposed.push(name.to_string());
        }
    }
    exposed
}

fn query(path: &str) -> io::Result<Vec<String>> {
    let mut s = UnixStream::connect(path)?;
    send(&mut s, GET_TREE, b"")?;
    let (_, body) = recv(&mut s)?;
    let tree: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok(exposed_outputs(&tree))
}

/// Whether an event can change which outputs are covered. Title, mark and
/// urgency changes are by far the most frequent window events and never do.
fn relevant(ty: u32, body: &[u8]) -> bool {
    if ty & 0x7fff_ffff != 3 {
        return true; // workspace/output events
    }
    let change = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["change"].as_str().map(str::to_owned))
        .unwrap_or_default();
    !matches!(change.as_str(), "title" | "mark" | "urgent")
}

fn run(path: &str, report: &mut impl FnMut(Option<Vec<String>>) -> bool) -> io::Result<()> {
    let mut sub = UnixStream::connect(path)?;
    send(&mut sub, SUBSCRIBE, br#"["workspace","window","output"]"#)?;
    recv(&mut sub)?; // {"success": true}
    let mut last = query(path)?;
    if !report(Some(last.clone())) {
        return Ok(());
    }
    loop {
        let (ty, body) = recv(&mut sub)?;
        if !relevant(ty, &body) {
            continue;
        }
        // Coalesce the burst a single action produces (e.g. a workspace
        // switch fires several events) into one tree query.
        sub.set_read_timeout(Some(Duration::from_millis(40)))?;
        loop {
            match recv(&mut sub) {
                Ok(_) => continue,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                Err(e) => return Err(e),
            }
        }
        sub.set_read_timeout(None)?;
        let now = query(path)?;
        if now != last {
            if !report(Some(now.clone())) {
                return Ok(());
            }
            last = now;
        }
    }
}

/// Blocks until `send` returns false; reconnects if sway restarts. Sends
/// `None` ("unknown: animate everything") while sway can't be reached.
pub fn watch(mut report: impl FnMut(Option<Vec<String>>) -> bool) {
    loop {
        let err = match socket_path() {
            Some(path) => match run(&path, &mut report) {
                Ok(()) => return,
                Err(e) => e,
            },
            None => io::Error::new(io::ErrorKind::NotFound, "no SWAYSOCK"),
        };
        eprintln!("og-wallpaper: sway ipc: {err}; animating all outputs, retrying in 5 s");
        if !report(None) {
            return;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_from_tree() {
        let tree = serde_json::json!({"nodes": [
            {"name": "__i3", "nodes": []},
            {"name": "DP-2", "current_workspace": "1", "nodes": [
                {"name": "1", "nodes": [], "floating_nodes": [{"id": 9}]},
                {"name": "4", "nodes": [{"id": 1}]}
            ]},
            {"name": "DP-3", "current_workspace": "2", "nodes": [
                {"name": "2", "nodes": [{"id": 2}]}
            ]}
        ]});
        assert_eq!(exposed_outputs(&tree), vec!["DP-2".to_string()]);
    }

    #[test]
    fn ignores_title_events() {
        assert!(!relevant(0x8000_0003, br#"{"change":"title"}"#));
        assert!(relevant(0x8000_0003, br#"{"change":"new"}"#));
        assert!(relevant(0x8000_0000, br#"{"change":"focus"}"#));
    }
}
