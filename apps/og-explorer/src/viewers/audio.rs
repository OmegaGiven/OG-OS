//! Audio player/editor: mp3, flac, ogg/vorbis, wav, aac/m4a, alac, opus.
//! Waveform overview, play/pause/seek/volume, tags (view; edit in edit mode,
//! Save writes over), trim/normalize exported as a NEW file.
//! CONTRACT STUB — implement bodies; keep the pub interface.
#![allow(unused_variables, dead_code)]

use super::Event;
use iced::{Element, Task};
use std::path::{Path, PathBuf};

const TODO: &str = "not implemented yet";

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    false
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    Err(TODO.into())
}

pub struct State {
    path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Msg {}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        (Self { path: o.path }, Task::none())
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {}
    }

    pub fn view(&self) -> Element<'_, Msg> {
        iced::widget::text(TODO).into()
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        None
    }

    /// Ctrl+E. Leaving edit mode with unsaved changes must be refused (return
    /// an Event::Status explaining) — see text.rs.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        Task::none()
    }

    /// Ctrl+S: write over the original (atomically). No-op if not dirty.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}
