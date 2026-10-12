//! Tailnet peers from the Tailscale LocalAPI (`GET /localapi/v0/status`
//! over tailscaled's unix socket). Read-only.

use super::{Discovered, Proto, Source};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const SOCKET: &str = "/run/tailscale/tailscaled.sock";

pub(crate) async fn discover(socket: PathBuf, user: String) -> Result<Vec<Discovered>, String> {
    let body = tokio::task::spawn_blocking(move || fetch(&socket, "/localapi/v0/status"))
        .await
        .map_err(|e| e.to_string())??;
    let json: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("Unexpected answer from Tailscale: {e}"))?;
    Ok(parse_status(&json, &user))
}

/// HTTP/1.1 GET over a unix socket; returns the (de-chunked) body.
fn fetch(socket: &std::path::Path, path: &str) -> Result<Vec<u8>, String> {
    let mut s = std::os::unix::net::UnixStream::connect(socket).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
            "Tailscale isn't running".to_string()
        }
        std::io::ErrorKind::PermissionDenied => "No permission to talk to Tailscale".to_string(),
        _ => format!("Can't reach Tailscale: {e}"),
    })?;
    let timeout = Some(Duration::from_secs(5));
    let _ = s.set_read_timeout(timeout);
    let _ = s.set_write_timeout(timeout);
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: local-tailscaled.sock\r\nUser-Agent: og-files\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    s.write_all(req.as_bytes())
        .map_err(|e| format!("Tailscale: {e}"))?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)
        .map_err(|e| format!("Tailscale: {e}"))?;
    parse_http_response(&raw)
}

/// Split status line + headers from the body; handle chunked and
/// Content-Length bodies. Err on non-2xx.
pub(crate) fn parse_http_response(raw: &[u8]) -> Result<Vec<u8>, String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("Truncated answer from Tailscale")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or_default();
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("Bad HTTP status line \"{status}\""))?;
    let mut chunked = false;
    let mut length: Option<usize> = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                length = v.parse().ok();
            }
        }
    }
    let body = if chunked {
        dechunk(body)?
    } else if let Some(n) = length {
        body.get(..n)
            .ok_or("Truncated answer from Tailscale")?
            .to_vec()
    } else {
        body.to_vec()
    };
    if !(200..300).contains(&code) {
        let msg = String::from_utf8_lossy(&body);
        return Err(format!("Tailscale answered {code}: {}", msg.trim()));
    }
    Ok(body)
}

pub(crate) fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let eol = b
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("Bad chunked encoding")?;
        let size_line = std::str::from_utf8(&b[..eol]).map_err(|_| "Bad chunked encoding")?;
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| "Bad chunk size")?;
        b = &b[eol + 2..];
        if size == 0 {
            return Ok(out); // trailers ignored
        }
        let chunk = b.get(..size).ok_or("Truncated chunk")?;
        out.extend_from_slice(chunk);
        b = &b[size..];
        b = b.strip_prefix(b"\r\n").ok_or("Bad chunk terminator")?;
    }
}

pub(crate) fn parse_status(json: &Value, user: &str) -> Vec<Discovered> {
    let magic = json
        .pointer("/CurrentTailnet/MagicDNSEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let self_key = json.pointer("/Self/PublicKey").and_then(Value::as_str);
    let self_id = json.pointer("/Self/ID").and_then(Value::as_str);
    let Some(peers) = json.get("Peer").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<Discovered> = peers
        .iter()
        .filter_map(|(key, p)| {
            let id = p.get("ID").and_then(Value::as_str);
            if Some(key.as_str()) == self_key || (id.is_some() && id == self_id) {
                return None;
            }
            // Mullvad exit nodes etc. aren't machines you log into
            if p.get("Location").is_some_and(|l| !l.is_null()) {
                return None;
            }
            let ips: Vec<&str> = p
                .get("TailscaleIPs")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let dns = p.get("DNSName").and_then(Value::as_str).unwrap_or("");
            let short = dns.trim_end_matches('.').split('.').next().unwrap_or("");
            let v4 = ips.iter().find(|ip| !ip.contains(':')).or(ips.first());
            let host = if magic && !short.is_empty() {
                short.to_string()
            } else {
                v4?.to_string()
            };
            let hostname = p.get("HostName").and_then(Value::as_str).unwrap_or("");
            let name = if hostname.is_empty() || hostname == "localhost" {
                host.clone()
            } else {
                hostname.to_string()
            };
            Some(Discovered {
                name,
                proto: Proto::Sftp,
                host,
                port: None,
                user: user.to_string(),
                source: Source::Tailnet {
                    online: p.get("Online").and_then(Value::as_bool).unwrap_or(false),
                    os: p
                        .get("OS")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                },
            })
        })
        .collect();
    let online = |d: &Discovered| matches!(d.source, Source::Tailnet { online: true, .. });
    out.sort_by(|a, b| {
        online(b)
            .cmp(&online(a))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
      "Version": "1.90.0",
      "BackendState": "Running",
      "Self": {"ID": "n1", "PublicKey": "nodekey:self", "HostName": "station",
               "DNSName": "station.tail1234.ts.net.", "TailscaleIPs": ["100.64.0.1"], "Online": true, "OS": "linux"},
      "CurrentTailnet": {"Name": "x", "MagicDNSSuffix": "tail1234.ts.net", "MagicDNSEnabled": true},
      "Peer": {
        "nodekey:a": {"ID": "n2", "HostName": "go", "DNSName": "go.tail1234.ts.net.",
                      "TailscaleIPs": ["fd7a:115c:a1e0::2", "100.64.0.2"], "Online": false, "OS": "linux"},
        "nodekey:b": {"ID": "n3", "HostName": "Pixel 9", "DNSName": "pixel-9.tail1234.ts.net.",
                      "TailscaleIPs": ["100.64.0.3"], "Online": true, "OS": "android"},
        "nodekey:self": {"ID": "n1", "HostName": "station", "DNSName": "station.tail1234.ts.net.",
                      "TailscaleIPs": ["100.64.0.1"], "Online": true, "OS": "linux"},
        "nodekey:c": {"ID": "n4", "HostName": "us-nyc-wg-301.mullvad.ts.net", "DNSName": "x.mullvad.ts.net.",
                      "TailscaleIPs": ["100.64.0.9"], "Online": true, "OS": "linux",
                      "Location": {"Country": "USA"}},
        "nodekey:d": {"ID": "n5", "HostName": "localhost", "DNSName": "",
                      "TailscaleIPs": ["fd7a::5", "100.64.0.5"], "Online": true, "OS": "windows"}
      }
    }"#;

    #[test]
    fn net_tailnet_parse_status() {
        let v: Value = serde_json::from_str(FIXTURE).unwrap();
        let d = parse_status(&v, "me");
        let names: Vec<_> = d.iter().map(|d| d.name.as_str()).collect();
        // online first, self and mullvad skipped
        assert_eq!(names, vec!["100.64.0.5", "Pixel 9", "go"]);
        assert_eq!(d[1].host, "pixel-9");
        assert_eq!(d[2].host, "go");
        assert_eq!(d[0].host, "100.64.0.5");
        assert!(d.iter().all(|d| d.proto == Proto::Sftp && d.user == "me"));
        assert_eq!(
            d[2].source,
            Source::Tailnet {
                online: false,
                os: "linux".into()
            }
        );

        // MagicDNS off → IPv4 addresses
        let mut v2 = v.clone();
        v2["CurrentTailnet"]["MagicDNSEnabled"] = Value::Bool(false);
        let d = parse_status(&v2, "me");
        assert_eq!(
            d.iter().find(|d| d.name == "go").unwrap().host,
            "100.64.0.2"
        );
        assert!(parse_status(&serde_json::json!({}), "me").is_empty());
    }

    #[test]
    fn net_tailnet_http_chunked() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n6;ext=1\r\n: 12}\n\r\n0\r\nX-Trailer: y\r\n\r\n";
        assert_eq!(parse_http_response(raw).unwrap(), b"{\"a\": 12}\n");
        assert!(dechunk(b"5\r\nab").is_err());
        assert!(dechunk(b"zz\r\n").is_err());
        assert!(dechunk(b"2\r\nabXX0\r\n\r\n").is_err());
    }

    #[test]
    fn net_tailnet_http_plain_and_errors() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}garbage";
        assert_eq!(parse_http_response(raw).unwrap(), b"{}");
        let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"x\":1}";
        assert_eq!(parse_http_response(raw).unwrap(), b"{\"x\":1}");
        let raw = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\n\r\ndenied";
        assert_eq!(
            parse_http_response(raw).unwrap_err(),
            "Tailscale answered 403: denied"
        );
        assert!(parse_http_response(b"HTTP/1.1 200").is_err());
    }

    #[tokio::test]
    async fn net_tailnet_missing_socket() {
        let e = discover("/nonexistent/tailscaled.sock".into(), "me".into())
            .await
            .unwrap_err();
        assert_eq!(e, "Tailscale isn't running");
    }

    /// Live, read-only: GET status from the real LocalAPI. Prints counts only.
    #[tokio::test]
    #[ignore]
    async fn net_tailnet_live() {
        let peers = discover(SOCKET.into(), "me".into())
            .await
            .expect("tailscale status");
        let online = peers
            .iter()
            .filter(|d| matches!(d.source, Source::Tailnet { online: true, .. }))
            .count();
        println!("LIVE tailnet: {} peers, {} online", peers.len(), online);
    }
}
