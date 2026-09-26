use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

/// Own storage, replacing the clipman-file-reader this used to be —
/// clipman only ever watched `--type text` (see sway config), so images
/// never had a path into history at all; this owns both, with images
/// cached as real PNG files (not inlined into the JSON) so they can
/// actually be thumbnailed instead of just showing as an opaque blob.
fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(format!("{home}/.local/share/og-clip"))
}

fn history_path() -> PathBuf {
    data_dir().join("history.json")
}

fn images_dir() -> PathBuf {
    data_dir().join("images")
}

const MAX_HISTORY: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind")]
pub enum ClipEntry {
    Text { value: String },
    /// Absolute path into `images_dir()` — the JSON array only ever holds
    /// this pointer, never the raw bytes.
    Image { path: String },
}

fn load_raw() -> Vec<ClipEntry> {
    let raw = std::fs::read_to_string(history_path()).unwrap_or_else(|_| "[]".into());
    serde_json::from_str(&raw).unwrap_or_default()
}

fn save_raw(items: &[ClipEntry]) {
    let _ = std::fs::create_dir_all(data_dir());
    if let Ok(json) = serde_json::to_string(items) {
        let _ = std::fs::write(history_path(), json);
    }
}

fn evict_oldest_if_over_cap(items: &mut Vec<ClipEntry>) {
    while items.len() > MAX_HISTORY {
        if let ClipEntry::Image { path } = items.remove(0) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Appended oldest-first on disk; newest-first is the useful order to
/// browse in, same convention the old clipman-backed version used.
pub fn load() -> Vec<ClipEntry> {
    let mut items = load_raw();
    items.reverse();
    items
}

pub fn delete(entry: &ClipEntry) {
    let mut items = load_raw();
    if let Some(pos) = items.iter().position(|i| i == entry) {
        if let ClipEntry::Image { path } = items.remove(pos) {
            let _ = std::fs::remove_file(path);
        }
    }
    save_raw(&items);
}

pub fn clear_all() {
    for item in load_raw() {
        if let ClipEntry::Image { path } = item {
            let _ = std::fs::remove_file(path);
        }
    }
    save_raw(&[]);
}

/// Collapses newlines/whitespace to a single line and caps length, for
/// showing a multi-line clipboard entry as one readable row.
pub fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 100;
    if collapsed.chars().count() > MAX {
        format!("{}…", collapsed.chars().take(MAX).collect::<String>())
    } else {
        collapsed
    }
}

pub fn copy_text_to_clipboard(text: &str) {
    if let Ok(mut child) = Command::new("wl-copy").stdin(Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(text.as_bytes());
        }
    }
}

pub fn copy_image_to_clipboard(path: &str) {
    let Ok(bytes) = std::fs::read(path) else { return };
    if let Ok(mut child) = Command::new("wl-copy").args(["--type", "image/png"]).stdin(Stdio::piped()).spawn() {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(&bytes);
        }
    }
}

/// FNV-1a — good enough to dedupe identical images without pulling in a
/// crypto-hash dependency for what's just a cache-key.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Entry point for `og-clip --store-text`, run as the command
/// `wl-paste --type text --watch` invokes on every clipboard change —
/// reads the new clipboard text from stdin and appends it.
pub fn store_text_from_stdin() {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
        return;
    }
    let mut items = load_raw();
    // `wl-paste --watch` can refire on a no-op selection change (e.g.
    // re-focusing the app that owns the current selection) — skip if
    // it's an exact repeat of the most recent entry.
    if let Some(ClipEntry::Text { value }) = items.last() {
        if *value == buf {
            return;
        }
    }
    items.push(ClipEntry::Text { value: buf });
    evict_oldest_if_over_cap(&mut items);
    save_raw(&items);
}

/// Entry point for `og-clip --store-image`, run as the command
/// `wl-paste --type image/png --watch` invokes — reads raw PNG bytes
/// from stdin, caches them by content hash (so re-copying the same
/// image doesn't pile up duplicate files), and appends a pointer entry.
pub fn store_image_from_stdin() {
    let mut buf = Vec::new();
    if std::io::stdin().read_to_end(&mut buf).is_err() || buf.is_empty() {
        return;
    }

    let _ = std::fs::create_dir_all(images_dir());
    let path = images_dir().join(format!("{:016x}.png", fnv1a(&buf)));
    if !path.exists() {
        let _ = std::fs::write(&path, &buf);
    }
    let path_str = path.to_string_lossy().to_string();

    let mut items = load_raw();
    if let Some(ClipEntry::Image { path: last }) = items.last() {
        if *last == path_str {
            return;
        }
    }
    items.push(ClipEntry::Image { path: path_str });
    evict_oldest_if_over_cap(&mut items);
    save_raw(&items);
}
