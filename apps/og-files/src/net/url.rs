//! Connection URLs: `sftp://user@host:port/path` and friends.
//!
//! Path conventions in `Conn::path`:
//! - SFTP/FTP/WebDAV: as written in the URL, so `/srv/media` is absolute;
//!   `~/media` (URL `sftp://host/~/media`) is relative to the login home;
//!   empty = the login folder.
//! - SMB: `share/sub/folder`, no leading slash.

use super::{Auth, Conn, Proto};

/// Port that `Conn::port == None` stands for.
pub(crate) fn effective_port(c: &Conn) -> u16 {
    c.port.unwrap_or(match c.proto {
        Proto::WebDav if !c.tls => 80,
        p => p.default_port(),
    })
}

fn is_default_port(proto: Proto, tls: bool, port: u16) -> bool {
    match proto {
        Proto::WebDav => port == if tls { 443 } else { 80 },
        p => port == p.default_port(),
    }
}

pub(crate) fn parse(input: &str, local_user: &str) -> Result<Conn, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Enter an address, e.g. sftp://user@host/path".into());
    }
    let (proto, mut tls, rest, bare) = match s.split_once("://") {
        Some((scheme, rest)) => {
            let (proto, tls) = match scheme.to_ascii_lowercase().as_str() {
                "sftp" | "ssh" | "scp" => (Proto::Sftp, false),
                "smb" | "cifs" => (Proto::Smb, false),
                "dav" | "webdav" | "http" => (Proto::WebDav, false),
                "davs" | "webdavs" | "https" => (Proto::WebDav, true),
                "ftp" => (Proto::Ftp, false),
                "ftps" | "ftpes" => (Proto::Ftp, true),
                other => return Err(format!("Unsupported address type \"{other}://\"")),
            };
            (proto, tls, rest, false)
        }
        None => (Proto::Sftp, false, s, true),
    };

    // authority / path
    let (authority, raw_path) = if bare {
        split_bare(rest)
    } else {
        match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        }
    };
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    // a password in the URL is dropped: Conn never stores one
    let user = userinfo
        .map(|u| decode(u.split_once(':').map_or(u, |(u, _)| u)))
        .unwrap_or_default();
    let (host, port) = split_host_port(hostport)?;
    let host = decode(&host);
    if host.is_empty() {
        return Err("The address has no host name".into());
    }
    if host.contains(|c: char| c.is_whitespace() || c == '/') {
        return Err(format!("\"{host}\" is not a valid host name"));
    }
    if proto == Proto::WebDav && !tls && port == Some(443) {
        tls = true;
    }
    let port = port.filter(|&p| !is_default_port(proto, tls, p));

    let mut path = decode(raw_path);
    if path.starts_with("/~") {
        path.remove(0);
    }
    if proto == Proto::Smb {
        path = path.trim_matches('/').to_string();
    } else if path.len() > 1 {
        path = path.trim_end_matches('/').to_string();
    }

    let user = if user.is_empty() && proto == Proto::Sftp {
        local_user.to_string()
    } else {
        user
    };
    let auth = match proto {
        Proto::Sftp => Auth::Agent,
        _ if user.is_empty() => Auth::Anonymous,
        _ => Auth::Password,
    };
    let last = path
        .trim_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && *s != "~");
    let name = match last {
        Some(l) => format!("{host} — {l}"),
        None => host.clone(),
    };
    Ok(Conn {
        id: String::new(),
        name,
        proto,
        host,
        port,
        user,
        path,
        auth,
        tls,
        auto_mount: false,
    })
}

/// `user@host[:port][/path]`, `host:/path` (scp style) or `[v6]:port`.
fn split_bare(s: &str) -> (&str, &str) {
    if let Some(i) = s.find('/') {
        // scp style `host:/path` / `host:~/path`: the colon belongs to the path
        let (a, p) = (&s[..i], &s[i..]);
        if let Some(a2) = a.strip_suffix(':') {
            return (a2, p);
        }
        if let Some(a2) = a.strip_suffix(":~") {
            return (a2, &s[i - 1..]);
        }
        return (a, p);
    }
    (s, "")
}

fn split_host_port(s: &str) -> Result<(String, Option<u16>), String> {
    let bad_port = |p: &str| format!("\"{p}\" is not a valid port");
    if let Some(rest) = s.strip_prefix('[') {
        let Some((h, after)) = rest.split_once(']') else {
            return Err("Missing \"]\" after the IPv6 address".into());
        };
        let port = match after.strip_prefix(':') {
            Some(p) if !p.is_empty() => Some(p.parse::<u16>().map_err(|_| bad_port(p))?),
            Some(_) => None,
            None if after.is_empty() => None,
            None => return Err(format!("Unexpected \"{after}\" after the IPv6 address")),
        };
        return Ok((h.to_string(), port));
    }
    match s.matches(':').count() {
        0 => Ok((s.to_string(), None)),
        1 => {
            let (h, p) = s.split_once(':').unwrap();
            if p.is_empty() {
                return Ok((h.to_string(), None));
            }
            let port = p.parse::<u16>().map_err(|_| bad_port(p))?;
            if port == 0 {
                return Err(bad_port(p));
            }
            Ok((h.to_string(), Some(port)))
        }
        // bare IPv6 without brackets: no port possible
        _ => Ok((s.to_string(), None)),
    }
}

/// Percent-decoding (invalid escapes are kept literally).
pub(crate) fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encode what would break re-parsing; `extra` = more bytes to
/// escape (e.g. `@:` in the user part, `@` in paths is fine).
pub(crate) fn encode(s: &str, extra: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        let c = ch as u32;
        if c < 0x80 {
            let b = c as u8;
            if b <= 0x20 || b == 0x7f || matches!(b, b'%' | b'?' | b'#') || extra.contains(&b) {
                out.push_str(&format!("%{b:02X}"));
                continue;
            }
        }
        out.push(ch);
    }
    out
}

pub(crate) fn print(c: &Conn) -> String {
    let scheme = match (c.proto, c.tls) {
        (Proto::Sftp, _) => "sftp",
        (Proto::Smb, _) => "smb",
        (Proto::WebDav, false) => "dav",
        (Proto::WebDav, true) => "davs",
        (Proto::Ftp, false) => "ftp",
        (Proto::Ftp, true) => "ftps",
    };
    let mut url = format!("{scheme}://");
    if !c.user.is_empty() {
        url.push_str(&encode(&c.user, b"@:/"));
        url.push('@');
    }
    if c.host.contains(':') {
        url.push_str(&format!("[{}]", c.host));
    } else {
        url.push_str(&encode(&c.host, b"@/"));
    }
    if let Some(p) = c.port.filter(|&p| !is_default_port(c.proto, c.tls, p)) {
        url.push_str(&format!(":{p}"));
    }
    if !c.path.is_empty() {
        if !c.path.starts_with('/') {
            url.push('/');
        }
        url.push_str(&encode(&c.path, b""));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Conn {
        parse(s, "me").unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn net_url_sftp_full() {
        let c = p("sftp://alice@go:2222/srv/media");
        assert_eq!(c.proto, Proto::Sftp);
        assert_eq!((c.user.as_str(), c.host.as_str()), ("alice", "go"));
        assert_eq!(c.port, Some(2222));
        assert_eq!(c.path, "/srv/media");
        assert_eq!(c.auth, Auth::Agent);
        assert_eq!(c.name, "go — media");
        assert_eq!(print(&c), "sftp://alice@go:2222/srv/media");
    }

    #[test]
    fn net_url_bare_hosts_are_sftp() {
        let c = p("go");
        assert_eq!(
            (c.proto, c.user.as_str(), c.host.as_str()),
            (Proto::Sftp, "me", "go")
        );
        assert_eq!(c.port, None);
        assert_eq!(c.path, "");
        let c = p("bob@nas.local");
        assert_eq!((c.user.as_str(), c.host.as_str()), ("bob", "nas.local"));
        let c = p("bob@nas:2200");
        assert_eq!(c.port, Some(2200));
        let c = p("bob@nas:/data/x");
        assert_eq!((c.host.as_str(), c.path.as_str()), ("nas", "/data/x"));
        let c = p("nas:~/x");
        assert_eq!((c.host.as_str(), c.path.as_str()), ("nas", "~/x"));
        let c = p("  ssh://box  ");
        assert_eq!((c.host.as_str(), c.user.as_str()), ("box", "me"));
    }

    #[test]
    fn net_url_ipv6() {
        let c = p("sftp://[::1]:2022/tmp");
        assert_eq!((c.host.as_str(), c.port), ("::1", Some(2022)));
        assert_eq!(print(&c), "sftp://me@[::1]:2022/tmp");
        let c = p("[fe80::1]");
        assert_eq!((c.host.as_str(), c.port), ("fe80::1", None));
        let c = p("smb://[fd7a:115c::5]/share");
        assert_eq!(c.host, "fd7a:115c::5");
        assert!(parse("sftp://[::1", "me").is_err());
        assert!(parse("sftp://[::1]x", "me").is_err());
        // unbracketed v6 = host only
        assert_eq!(p("::1").host, "::1");
    }

    #[test]
    fn net_url_default_ports_dropped() {
        assert_eq!(p("sftp://h:22").port, None);
        assert_eq!(p("smb://h:445/s").port, None);
        assert_eq!(p("ftp://h:21").port, None);
        assert_eq!(p("davs://h:443").port, None);
        assert_eq!(p("dav://h:80").port, None);
        assert_eq!(p("dav://h:8080").port, Some(8080));
        assert!(parse("sftp://h:99999", "me").is_err());
        assert!(parse("sftp://h:0", "me").is_err());
        assert!(parse("sftp://h:abc", "me").is_err());
    }

    #[test]
    fn net_url_smb() {
        let c = p("smb://bob@nas/media/films/2024/");
        assert_eq!(c.proto, Proto::Smb);
        assert_eq!(c.path, "media/films/2024");
        assert_eq!(c.auth, Auth::Password);
        assert_eq!(print(&c), "smb://bob@nas/media/films/2024");
        let c = p("smb://nas/public");
        assert_eq!((c.user.as_str(), &c.auth), ("", &Auth::Anonymous));
        assert_eq!(c.name, "nas — public");
    }

    #[test]
    fn net_url_webdav_tls() {
        let c = p("davs://u@cloud.example.com/remote.php/dav/files/u");
        assert!(c.tls);
        assert_eq!(c.proto, Proto::WebDav);
        assert_eq!(c.path, "/remote.php/dav/files/u");
        assert_eq!(effective_port(&c), 443);
        assert_eq!(
            print(&c),
            "davs://u@cloud.example.com/remote.php/dav/files/u"
        );
        let c = p("webdav://h:443/x");
        assert!(c.tls);
        assert_eq!(c.port, None);
        let c = p("webdav://h/x");
        assert!(!c.tls);
        assert_eq!(effective_port(&c), 80);
        assert_eq!(print(&c), "dav://h/x");
        let c = p("https://h:8443/");
        assert!(c.tls);
        assert_eq!(c.port, Some(8443));
    }

    #[test]
    fn net_url_ftp() {
        let c = p("ftp://h/pub");
        assert_eq!(
            (c.proto, &c.auth, c.tls),
            (Proto::Ftp, &Auth::Anonymous, false)
        );
        let c = p("ftps://u@h/pub");
        assert!(c.tls);
        assert_eq!(print(&c), "ftps://u@h/pub");
    }

    #[test]
    fn net_url_percent_and_password() {
        let c = p("sftp://j%40corp:secret@h/My%20Files/%C3%A9t%C3%A9");
        assert_eq!(c.user, "j@corp");
        assert_eq!(c.path, "/My Files/été");
        let url = print(&c);
        assert!(!url.contains("secret"));
        assert_eq!(url, "sftp://j%40corp@h/My%20Files/été");
        assert_eq!(p(&url), c);
        // broken escapes stay literal
        assert_eq!(decode("100%"), "100%");
        assert_eq!(decode("%zz%4"), "%zz%4");
        assert_eq!(decode("a%2Fb"), "a/b");
    }

    #[test]
    fn net_url_home_relative() {
        let c = p("sftp://h/~/media");
        assert_eq!(c.path, "~/media");
        assert_eq!(print(&c), "sftp://me@h/~/media");
        assert_eq!(p("sftp://h/~").path, "~");
        assert_eq!(p("sftp://h/").path, "/");
    }

    #[test]
    fn net_url_errors() {
        assert!(parse("", "me").is_err());
        assert!(parse("gopher://x", "me").is_err());
        assert!(parse("sftp://", "me").is_err());
        assert!(parse("sftp://user@/x", "me").is_err());
    }

    #[test]
    fn net_url_round_trips() {
        for s in [
            "sftp://me@go",
            "sftp://me@go/",
            "sftp://a@b:2222/x/y z",
            "smb://u@nas/share/a b",
            "smb://nas",
            "dav://h:8080/dav",
            "davs://u@h/remote.php/dav",
            "ftp://h/pub",
            "ftps://u@h:990",
            "sftp://me@[2001:db8::2]:22022/srv",
        ] {
            let c = p(s);
            let printed = print(&c);
            assert_eq!(p(&printed), c, "{s} -> {printed}");
        }
    }
}
