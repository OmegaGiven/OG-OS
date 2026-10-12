//! `connections.json`: the saved connections (never passwords).

use super::Conn;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default)]
    connections: Vec<Conn>,
}

fn file(dir: &Path) -> PathBuf {
    dir.join("connections.json")
}

pub(crate) fn load(dir: &Path) -> Vec<Conn> {
    std::fs::read_to_string(file(dir))
        .ok()
        .and_then(|s| serde_json::from_str::<File>(&s).ok())
        .map(|f| f.connections)
        .unwrap_or_default()
}

/// Assign ids, drop duplicates, write atomically (temp + rename). Returns
/// what was written.
pub(crate) fn save(dir: &Path, conns: &[Conn]) -> Result<Vec<Conn>, String> {
    let list = normalize(conns);
    let json = serde_json::to_string_pretty(&File {
        connections: list.clone(),
    })
    .map_err(|e| e.to_string())?;
    write_atomic(&file(dir), json.as_bytes())
        .map_err(|e| format!("Couldn't save connections: {e}"))?;
    Ok(list)
}

pub(crate) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        std::process::id()
    ));
    let res = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Same connection target and settings, ignoring id/name/auto_mount.
fn same_target(a: &Conn, b: &Conn) -> bool {
    a.proto == b.proto
        && a.host.eq_ignore_ascii_case(&b.host)
        && a.port == b.port
        && a.user == b.user
        && a.path == b.path
        && a.auth == b.auth
        && a.tls == b.tls
        && a.name == b.name
}

fn normalize(conns: &[Conn]) -> Vec<Conn> {
    let mut out: Vec<Conn> = Vec::with_capacity(conns.len());
    let mut ids: HashSet<String> = HashSet::new();
    for c in conns {
        if out.iter().any(|o| same_target(o, c)) {
            continue;
        }
        let mut c = c.clone();
        if c.id.is_empty() {
            let base = new_id(&c);
            let mut id = base.clone();
            let mut n = 2;
            while ids.contains(&id) || conns.iter().any(|o| o.id == id) {
                id = format!("{base}-{n}");
                n += 1;
            }
            c.id = id;
        } else if ids.contains(&c.id) {
            continue; // first one with an id wins
        }
        ids.insert(c.id.clone());
        out.push(c);
    }
    out
}

/// `slug(name)-hash6`, e.g. `go-media-3fa2c1`.
pub(crate) fn new_id(c: &Conn) -> String {
    let mut slug = String::new();
    for ch in c.name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 24 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let key = format!(
        "{:?}|{}|{}|{:?}|{}|{}",
        c.proto, c.user, c.host, c.port, c.path, c.name
    );
    let hash = fnv1a(key.as_bytes()) & 0xff_ffff;
    if slug.is_empty() {
        format!("conn-{hash:06x}")
    } else {
        format!("{slug}-{hash:06x}")
    }
}

fn fnv1a(b: &[u8]) -> u32 {
    b.iter().fold(0x811c9dc5u32, |h, &x| {
        (h ^ x as u32).wrapping_mul(0x0100_0193)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testdir::TestDir;
    use crate::net::{Auth, Proto};

    fn conn(name: &str, host: &str) -> Conn {
        Conn {
            id: String::new(),
            name: name.into(),
            proto: Proto::Sftp,
            host: host.into(),
            port: None,
            user: "me".into(),
            path: String::new(),
            auth: Auth::Agent,
            tls: false,
            auto_mount: false,
        }
    }

    #[test]
    fn net_store_round_trip_and_ids() {
        let t = TestDir::new("store");
        let dir = t.path().join("cfg");
        assert!(load(&dir).is_empty());

        let mut b = conn("NAS — Media!", "nas");
        b.proto = Proto::Smb;
        b.auth = Auth::Password;
        b.path = "media".into();
        b.port = Some(4450);
        let mut keep = conn("kept", "k");
        keep.id = "fixed".into();
        let input = vec![
            conn("go — media", "go"),
            conn("go — media", "go"), // exact dup
            b.clone(),
            keep.clone(),
            {
                let mut d = conn("other", "o");
                d.id = "fixed".into(); // dup id
                d
            },
            conn("go — media", "go2"), // same name, other host
        ];
        let saved = save(&dir, &input).unwrap();
        assert_eq!(saved.len(), 4);
        assert!(saved[0].id.starts_with("go-media-"), "{}", saved[0].id);
        assert_eq!(saved[0].id.len(), "go-media-".len() + 6);
        assert!(saved[1].id.starts_with("nas-media-"));
        assert_eq!(saved[2].id, "fixed");
        assert_ne!(saved[3].id, saved[0].id);
        assert!(saved.iter().all(|c| !c.id.is_empty()));

        let loaded = load(&dir);
        assert_eq!(loaded, saved);
        // ids are stable on resave
        assert_eq!(save(&dir, &loaded).unwrap(), saved);
        // no temp files left, file is private
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["connections.json"]);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join("connections.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // no password field anywhere
        let raw = std::fs::read_to_string(dir.join("connections.json")).unwrap();
        assert!(!raw.to_lowercase().contains("pass\""));
    }

    #[test]
    fn net_store_bad_json_is_empty() {
        let t = TestDir::new("store-bad");
        std::fs::write(t.path().join("connections.json"), "{nope").unwrap();
        assert!(load(t.path()).is_empty());
    }

    #[test]
    fn net_store_slug_edge_cases() {
        assert!(new_id(&conn("———", "h")).starts_with("conn-"));
        let long = new_id(&conn(&"x".repeat(100), "h"));
        assert!(long.len() <= 24 + 7);
    }
}
