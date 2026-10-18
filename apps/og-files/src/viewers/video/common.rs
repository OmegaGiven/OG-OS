//! Pieces shared by the libmpv player, the FFmpeg fallback player (`ff.rs`,
//! feature `ffmpeg`) and the no-FFmpeg stand-in (`ff_none.rs`).

#[cfg(feature = "ffmpeg")]
use std::path::Path;
use std::path::PathBuf;

pub(super) const AMBER: iced::Color = iced::Color::from_rgb(1.0, 0.784, 0.42);

/// Stream facts shown under the player.
#[derive(Debug, Clone, Default)]
pub struct Info {
    /// seconds
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub video_codec: String,
    /// "AAC 48 kHz 2ch"
    pub audio: Option<String>,
    /// bits/s (container), 0 if unknown
    pub bit_rate: i64,
}

impl Info {
    pub(super) fn summary(&self) -> String {
        let mut s = format!(
            "{} · {}×{} · {:.2} fps",
            self.video_codec, self.width, self.height, self.fps
        );
        if self.bit_rate > 0 {
            s += &format!(" · {:.1} Mb/s", self.bit_rate as f64 / 1e6);
        }
        match &self.audio {
            Some(a) => s += &format!(" · {a}"),
            None => s += " · no audio",
        }
        s
    }
}

pub(super) fn fmt_time(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub(super) fn fmt_mark(t: f64) -> String {
    format!("{}.{}", fmt_time(t), ((t.max(0.0) * 10.0) as u64) % 10)
}

#[derive(Debug, Clone)]
pub enum ExportEv {
    #[cfg_attr(not(feature = "ffmpeg"), allow(dead_code))]
    Progress(f32),
    Done(Result<PathBuf, String>),
}

/// `<stem>-edit.<ext>` beside `src`, made unique.
#[cfg(feature = "ffmpeg")]
pub(super) fn edit_name(src: &Path) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new("."));
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let name = match src.extension() {
        Some(ext) => format!("{stem}-edit.{}", ext.to_string_lossy()),
        None => format!("{stem}-edit"),
    };
    crate::ops::unique_name(dir, &name)
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::PathBuf;

    /// Scratch dir under ~/.cache (never /tmp; the OS temp dir where there is
    /// no $HOME), removed on drop.
    pub(in crate::viewers::video) struct Tmp(pub PathBuf);
    impl Tmp {
        pub fn new(name: &str) -> Tmp {
            let base = std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .unwrap_or_else(std::env::temp_dir);
            let d = base
                .join("ogx-mpv-tests")
                .join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
