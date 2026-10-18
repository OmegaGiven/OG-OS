//! Minimal ~/.ssh/config reader: `Host` blocks (with first-value-wins
//! semantics, wildcards for lookup), HostName/User/Port/IdentityFile,
//! `Include` (relative to ~/.ssh, globs best-effort). `Match` blocks are
//! skipped.

use super::{Discovered, Proto, Source};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Block {
    patterns: Vec<String>,
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
}

/// What ssh would use for an alias.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SshHost {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<PathBuf>,
}

pub(crate) struct Config {
    blocks: Vec<Block>,
    ssh_dir: PathBuf,
}

impl Config {
    pub fn load(ssh_dir: &Path) -> Self {
        let mut blocks = Vec::new();
        let mut depth = 0;
        read_file(&ssh_dir.join("config"), ssh_dir, &mut blocks, &mut depth);
        Config {
            blocks,
            ssh_dir: ssh_dir.to_path_buf(),
        }
    }

    /// Concrete aliases (no wildcards / negations), in file order.
    pub fn aliases(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for b in &self.blocks {
            for p in &b.patterns {
                if !p.contains(['*', '?', '!']) && !out.contains(p) {
                    out.push(p.clone());
                }
            }
        }
        out
    }

    /// Resolve an alias like ssh does (first obtained value wins, across
    /// every matching block including `Host *`). None if it's no alias.
    pub fn resolve(&self, alias: &str) -> Option<SshHost> {
        if !self.aliases().iter().any(|a| a == alias) {
            return None;
        }
        let mut h = SshHost {
            alias: alias.to_string(),
            ..Default::default()
        };
        for b in self
            .blocks
            .iter()
            .filter(|b| block_matches(&b.patterns, alias))
        {
            if h.hostname.is_none() {
                h.hostname = b.hostname.as_ref().map(|s| s.replace("%h", alias));
            }
            if h.user.is_none() {
                h.user.clone_from(&b.user);
            }
            if h.port.is_none() {
                h.port = b.port;
            }
            if h.identity.is_none() {
                h.identity = b.identity.as_deref().map(|i| expand(i, &self.ssh_dir));
            }
        }
        Some(h)
    }
}

/// `~` → home (the parent of ssh_dir), relative → under ssh_dir.
fn expand(p: &str, ssh_dir: &Path) -> PathBuf {
    let home = ssh_dir.parent().unwrap_or(Path::new("/"));
    if let Some(rest) = p.strip_prefix("~/") {
        home.join(rest)
    } else if p == "~" {
        home.to_path_buf()
    } else if Path::new(p).is_absolute() {
        PathBuf::from(p)
    } else {
        ssh_dir.join(p)
    }
}

fn read_file(path: &Path, ssh_dir: &Path, blocks: &mut Vec<Block>, depth: &mut u32) {
    if *depth > 8 {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    *depth += 1;
    // lines before the first Host apply to everything
    let mut in_match = false;
    for line in text.lines() {
        let Some((key, val)) = split_kv(line) else {
            continue;
        };
        let val = unquote(val);
        match key.to_ascii_lowercase().as_str() {
            "host" => {
                in_match = false;
                blocks.push(Block {
                    patterns: val
                        .split_whitespace()
                        .map(|s| unquote(s).to_string())
                        .collect(),
                    ..Default::default()
                });
            }
            "match" => in_match = true,
            "include" => {
                for pat in val.split_whitespace() {
                    for f in glob(&expand(unquote(pat), ssh_dir)) {
                        read_file(&f, ssh_dir, blocks, depth);
                    }
                }
            }
            _ if in_match => {}
            k => {
                if blocks.is_empty() {
                    blocks.push(Block {
                        patterns: vec!["*".into()],
                        ..Default::default()
                    });
                }
                let b = blocks.last_mut().unwrap();
                match k {
                    "hostname" if b.hostname.is_none() => b.hostname = Some(val.to_string()),
                    "user" if b.user.is_none() => b.user = Some(val.to_string()),
                    "port" if b.port.is_none() => b.port = val.parse().ok(),
                    "identityfile" if b.identity.is_none() => b.identity = Some(val.to_string()),
                    _ => {}
                }
            }
        }
    }
    *depth -= 1;
}

/// `Key value` or `Key=value`; None for blanks/comments.
fn split_kv(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let i = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = &line[..i];
    let rest = line[i..].trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    Some((key, rest))
}

fn unquote(s: &str) -> &str {
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(s)
}

fn block_matches(patterns: &[String], host: &str) -> bool {
    let mut hit = false;
    for p in patterns {
        if let Some(neg) = p.strip_prefix('!') {
            if wildcard(neg, host) {
                return false;
            }
        } else if wildcard(p, host) {
            hit = true;
        }
    }
    hit
}

/// `*` / `?` matching (case-insensitive like ssh host patterns).
pub(crate) fn wildcard(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b) && go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

/// Best-effort glob: wildcards allowed in the last path component only.
fn glob(path: &Path) -> Vec<PathBuf> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !name.contains(['*', '?']) {
        return vec![path.to_path_buf()];
    }
    let dir = path.parent().unwrap_or(Path::new("/"));
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| wildcard(name, n) && !n.starts_with('.'))
        })
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    out.sort();
    out
}

pub(crate) fn discovered(ssh_dir: &Path, local_user: &str) -> Vec<Discovered> {
    let cfg = Config::load(ssh_dir);
    cfg.aliases()
        .into_iter()
        .filter_map(|a| cfg.resolve(&a))
        .map(|h| Discovered {
            name: h.alias.clone(),
            proto: Proto::Sftp,
            // keep the alias: mounting resolves it again, like ssh would
            host: h.alias,
            port: h.port.filter(|&p| p != 22),
            user: h.user.unwrap_or_else(|| local_user.to_string()),
            source: Source::SshConfig,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testdir::TestDir;

    fn fixture() -> TestDir {
        let t = TestDir::new("sshcfg");
        let ssh = t.path().join(".ssh");
        std::fs::create_dir_all(ssh.join("config.d")).unwrap();
        std::fs::write(
            ssh.join("config"),
            "# top\n\
             Include config.d/*.conf\n\
             Host go gopher\n\
             \tHostName 100.64.0.7\n\
             \tUser alice\n\
             \tPort 2222\n\
             \tIdentityFile ~/.ssh/id_go\n\
             \n\
             Host *.lan !bad.lan\n\
             \tUser lanuser\n\
             Host=nas\n\
             \tHostName = %h.lan\n\
             Match host x\n\
             \tUser matched\n\
             Host \"quoted\"\n\
             \tPort 22\n\
             Host *\n\
             \tUser fallback\n\
             \tIdentityFile id_default\n\
             \tPort 22\n",
        )
        .unwrap();
        std::fs::write(
            ssh.join("config.d/work.conf"),
            "Host work\n  HostName work.example.com\n  User bob\n",
        )
        .unwrap();
        std::fs::write(ssh.join("config.d/ignored.txt"), "Host nope\n").unwrap();
        t
    }

    #[test]
    fn net_sshcfg_aliases_and_include() {
        let t = fixture();
        let ssh = t.path().join(".ssh");
        let cfg = Config::load(&ssh);
        assert_eq!(cfg.aliases(), vec!["work", "go", "gopher", "nas", "quoted"]);

        let go = cfg.resolve("go").unwrap();
        assert_eq!(go.hostname.as_deref(), Some("100.64.0.7"));
        assert_eq!(go.user.as_deref(), Some("alice"));
        assert_eq!(go.port, Some(2222));
        assert_eq!(go.identity, Some(t.path().join(".ssh/id_go")));

        let nas = cfg.resolve("nas").unwrap();
        assert_eq!(nas.hostname.as_deref(), Some("nas.lan"));
        assert_eq!(nas.user.as_deref(), Some("fallback"));
        assert_eq!(nas.identity, Some(ssh.join("id_default")));

        let work = cfg.resolve("work").unwrap();
        assert_eq!(work.hostname.as_deref(), Some("work.example.com"));
        assert_eq!(work.user.as_deref(), Some("bob"));
        assert!(cfg.resolve("unknown").is_none());

        let d = discovered(&ssh, "me");
        assert_eq!(d.len(), 5);
        let go = d.iter().find(|d| d.name == "go").unwrap();
        assert_eq!(
            (go.host.as_str(), go.user.as_str(), go.port),
            ("go", "alice", Some(2222))
        );
        assert_eq!(go.source, Source::SshConfig);
        let q = d.iter().find(|d| d.name == "quoted").unwrap();
        assert_eq!((q.port, q.user.as_str()), (None, "fallback"));
    }

    #[test]
    fn net_sshcfg_patterns() {
        assert!(wildcard("*.lan", "nas.lan"));
        assert!(wildcard("h?st", "HOST"));
        assert!(!wildcard("*.lan", "nas.lan.x"));
        assert!(block_matches(
            &["*.lan".into(), "!bad.lan".into()],
            "ok.lan"
        ));
        assert!(!block_matches(
            &["*.lan".into(), "!bad.lan".into()],
            "bad.lan"
        ));
    }

    #[test]
    fn net_sshcfg_missing_is_empty() {
        let t = TestDir::new("sshcfg-none");
        assert!(discovered(&t.path().join(".ssh"), "me").is_empty());
    }
}
