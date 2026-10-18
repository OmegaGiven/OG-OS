//! Stand-in for the FFmpeg fallback player in builds without the `ffmpeg`
//! feature. Video then plays through libmpv only; when libmpv can't be
//! loaded the tab shows how to install it instead of failing. No grid
//! thumbnails and no trim export (snapshots still work: mpv makes them).

use super::common::{ExportEv, Info};
use crate::theme;
use crate::viewers::Event;
use crate::widgets::icon;
use iced::widget::{center, column, text};
use iced::{Alignment, Element, Task};
use iced_fonts::Bootstrap as B;
use std::path::{Path, PathBuf};

/// This build can't stream-copy trims.
pub(super) const CAN_EXPORT: bool = false;

/// Without FFmpeg nothing is probed up front: libmpv reports the stream
/// facts once the file is loaded.
pub(super) fn probe(path: &Path) -> Result<Info, String> {
    if path.is_file() {
        Ok(Info::default())
    } else {
        Err(format!("{}: not a file", path.display()))
    }
}

/// No video thumbnails without FFmpeg.
pub fn thumbnail(_path: &Path, _max: u32) -> Option<image::RgbaImage> {
    None
}

/// Never called (the button is disabled), but answers sensibly anyway.
pub(super) fn spawn_export(_src: PathBuf, _tin: f64, _tout: f64) -> Task<ExportEv> {
    Task::done(ExportEv::Done(Err(
        "trim export needs a build with the ffmpeg feature".into(),
    )))
}

/// How to get libmpv on this OS.
pub(super) fn install_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "Install mpv with Homebrew (brew install mpv), or bundle libmpv.2.dylib in the app's Contents/Frameworks."
    } else if cfg!(windows) {
        "Put libmpv-2.dll (from an mpv \"dev\" build for Windows) next to og-files.exe."
    } else {
        "Install mpv from your distribution (the package that provides libmpv.so.2)."
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    TogglePlay,
    Step,
    ToggleMute,
}

pub struct State {
    path: PathBuf,
    reason: String,
}

impl State {
    pub fn new(path: PathBuf, _info: Info) -> (Self, Task<Msg>) {
        let reason = match super::mpv::api() {
            Err(e) => format!("libmpv could not be loaded: {e}"),
            Ok(_) if std::env::var_os("OGX_NO_MPV").is_some_and(|v| v != "0") => {
                "libmpv is disabled (OGX_NO_MPV)".into()
            }
            Ok(_) => "libmpv failed to start this video".into(),
        };
        (State { path, reason }, Task::none())
    }

    pub fn update(&mut self, _msg: Msg) -> (Task<Msg>, Option<Event>) {
        (Task::none(), None)
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let name = self
            .path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        center(
            column![
                icon(B::CameraVideoOff).size(56).color(theme::MUTED),
                text("Install mpv to play video").size(18),
                text(install_hint()).color(theme::MUTED),
                text(format!("{name} — {}", self.reason))
                    .size(12)
                    .color(theme::MUTED),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .padding(24)
        .into()
    }

    pub fn is_editing(&self) -> Option<bool> {
        None
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        Task::none()
    }
}
