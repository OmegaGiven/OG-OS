//! Thumbnails for the grid view, generated in-process (no ffmpeg /
//! pdftoppm subprocesses) and cached as PNG under
//! `$XDG_CACHE_HOME/og-explorer/thumbs/`, keyed by path + size + mtime so an
//! edited file gets a fresh thumbnail.
//!
//! Images, and video frames via the video viewer's FFmpeg decoder.

use crate::config;
use crate::filetype::Kind;
use crate::fsops::Entry;
use iced::futures::{SinkExt, Stream};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

pub const SIZE: u32 = 256;
// don't decode giant files just for a thumbnail
const MAX_SOURCE_BYTES: u64 = 80 * 1024 * 1024;

pub fn wants(e: &Entry) -> bool {
    match e.kind {
        Kind::Image => e.size <= MAX_SOURCE_BYTES && !e.name.ends_with(".svg"),
        // a single decoded frame, any size file
        Kind::Video => crate::viewers::video::supported(&e.path),
        _ => false,
    }
}

fn cache_path(e: &Entry) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    e.path.hash(&mut h);
    e.size.hash(&mut h);
    e.modified.hash(&mut h);
    config::cache_dir()
        .join("thumbs")
        .join(format!("{:016x}.png", h.finish()))
}

fn make(e: &Entry) -> Option<PathBuf> {
    let out = cache_path(e);
    if out.exists() {
        return Some(out);
    }
    std::fs::create_dir_all(out.parent()?).ok()?;
    if e.kind == Kind::Video {
        crate::viewers::video::thumbnail(&e.path, SIZE)?
            .save(&out)
            .ok()?;
    } else {
        image::open(&e.path)
            .ok()?
            .thumbnail(SIZE, SIZE)
            .save(&out)
            .ok()?;
    }
    Some(out)
}

/// One worker, in order: the first entries (top of the grid) come back
/// first, and a huge folder can't flood the blocking pool.
pub fn stream(entries: Vec<Entry>) -> impl Stream<Item = (PathBuf, PathBuf)> {
    iced::stream::channel(8, move |mut out| async move {
        for e in entries {
            let src = e.path.clone();
            let thumb = tokio::task::spawn_blocking(move || make(&e))
                .await
                .ok()
                .flatten();
            if let Some(t) = thumb {
                if out.send((src, t)).await.is_err() {
                    return;
                }
            }
        }
    })
}
