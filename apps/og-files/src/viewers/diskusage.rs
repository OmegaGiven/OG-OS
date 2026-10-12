//! Disk usage analyzer (WinDirStat-style) for a folder or drive: background
//! scan, squarified treemap coloured by file type, sortable size tree,
//! file-type breakdown, largest files, and clean-up actions that go through
//! the app's normal trash/delete jobs.
//! CONTRACT STUB — implement bodies; keep the pub interface.
#![allow(unused_variables, dead_code)]

use super::Event;
use iced::{Element, Task};
use std::path::PathBuf;

/// What to analyze. No async load: the State scans in the background and
/// renders partial results while it runs.
#[derive(Debug, Clone)]
pub struct Opened {
    pub root: PathBuf,
}

pub fn open(root: PathBuf) -> Opened {
    Opened { root }
}

pub struct State {
    root: PathBuf,
}

#[derive(Debug, Clone)]
pub enum Msg {}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        (Self { root: o.root }, Task::none())
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {}
    }

    pub fn view(&self) -> Element<'_, Msg> {
        iced::widget::text("not implemented yet").into()
    }

    /// Paths were trashed/deleted/moved by a finished job (or changed on
    /// disk): drop them from the tree / re-scan their parents so the sizes
    /// update without a full rescan.
    pub fn fs_changed(&mut self, paths: &[PathBuf]) -> Task<Msg> {
        Task::none()
    }

    /// Keys while the viewer has focus (None = not consumed).
    pub fn key(
        &mut self,
        key: &iced::keyboard::Key,
        mods: iced::keyboard::Modifiers,
    ) -> Option<Task<Msg>> {
        None
    }
}
