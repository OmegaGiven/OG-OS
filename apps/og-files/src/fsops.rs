//! Filesystem access. Rule: nothing in here is ever called from `view()`,
//! and anything that can block (disk, network mounts) runs on the blocking
//! pool via `spawn_blocking` and comes back as a Task.

use crate::filetype::Kind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub hidden: bool,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Folder
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f)
        .await
        .expect("blocking task panicked")
}

pub async fn list_dir(path: PathBuf) -> Result<Vec<Entry>, String> {
    blocking(move || list_dir_sync(&path)).await
}

fn list_dir_sync(path: &Path) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            // metadata() follows symlinks so a link to a folder shows as one
            let meta = std::fs::metadata(&path).ok();
            let is_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            // the hidden attribute (Windows) needs the entry's own metadata
            let own = if cfg!(windows) {
                e.metadata().ok()
            } else {
                None
            };
            let hidden = crate::platform::is_hidden(&name, own.as_ref());
            Entry {
                kind: Kind::of(&path, is_dir),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modified: meta.as_ref().and_then(|m| m.modified().ok()),
                hidden,
                name,
                path,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir()
            .cmp(&a.is_dir())
            .then_with(|| natural_cmp(&a.name.to_lowercase(), &b.name.to_lowercase()))
    });
    Ok(out)
}

/// "file2" < "file10". Port of og-files' natural sort, simplified.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n = String::new();
                    while let Some(c) = it.peek().filter(|c| c.is_ascii_digit()) {
                        n.push(*c);
                        it.next();
                    }
                    n
                };
                let (na, nb) = (take(&mut a), take(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.cmp(y);
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Meta {
    pub path: PathBuf,
    pub kind: Kind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub readonly: bool,
    pub mode: Option<u32>,
    pub child_count: Option<usize>,
}

pub async fn metadata(path: PathBuf) -> Result<Meta, String> {
    blocking(move || {
        let m = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            Some(m.permissions().mode())
        };
        #[cfg(not(unix))]
        let mode = None;
        Ok(Meta {
            kind: Kind::of(&path, m.is_dir()),
            size: m.len(),
            modified: m.modified().ok(),
            readonly: m.permissions().readonly(),
            mode,
            child_count: if m.is_dir() {
                std::fs::read_dir(&path).ok().map(|r| r.count())
            } else {
                None
            },
            path,
        })
    })
    .await
}

/// Mounted removable / network filesystems from /proc/mounts. Placeholder
/// until the UDisks2 (zbus) backend lands — SPEC.md §6.
pub async fn mounts() -> (Vec<PathBuf>, Vec<PathBuf>) {
    blocking(|| {
        let text = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let (mut removable, mut network) = (Vec::new(), Vec::new());
        for line in text.lines() {
            let mut it = line.split_whitespace();
            let (_dev, Some(dir), Some(fstype)) = (it.next(), it.next(), it.next()) else {
                continue;
            };
            let dir = PathBuf::from(dir.replace("\\040", " "));
            if matches!(
                fstype,
                "cifs" | "smb3" | "nfs" | "nfs4" | "fuse.sshfs" | "fuse.rclone"
            ) {
                network.push(dir);
            } else if dir.starts_with("/run/media")
                || dir.starts_with("/media")
                || dir.starts_with("/mnt")
            {
                removable.push(dir);
            }
        }
        (removable, network)
    })
    .await
}

pub async fn read_text(path: PathBuf, max: u64) -> Result<String, String> {
    blocking(move || {
        let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if len > max {
            return Err(format!(
                "file is {} — over the {} text-viewer limit",
                human_size(len),
                human_size(max)
            ));
        }
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        String::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())
    })
    .await
}

pub async fn write_text(path: PathBuf, text: String) -> Result<(), String> {
    write_bytes(path, text.into_bytes()).await
}

/// Atomic write: sibling temp file then rename, so a crash mid-save can't
/// truncate the original.
pub async fn write_bytes(path: PathBuf, bytes: Vec<u8>) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        ".{}.og-files.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    tokio::fs::write(&tmp, bytes)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| e.to_string())
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return "—".into() };
    let secs = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match secs {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} days ago", secs / 86_400),
    }
}
