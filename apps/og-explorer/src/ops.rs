//! File operations engine: copy / move / trash / delete / restore as
//! background jobs with progress, conflict prompts, cancel and undo, plus
//! freedesktop.org trash (home trash and per-volume `.Trash-$uid`, so USB
//! drives work) and small synchronous helpers. CONTRACT STUB — bodies to be
//! implemented; public signatures and types are fixed (the UI is built
//! against them).
//!
//! Threading: each job runs on its own OS thread; progress comes back as a
//! `Stream<Item = JobEvent>` the UI runs with `Task::run`. Nothing here may
//! block the caller of `start`.
#![allow(unused_variables, dead_code)]

use iced::futures::Stream;
use std::path::{Path, PathBuf};

const TODO: &str = "not implemented yet";

pub type JobId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Copy,
    Move,
    /// to the freedesktop trash (per-volume aware)
    Trash,
    /// permanent delete
    Delete,
    /// restore trash entries to their original locations
    Restore,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobSpec {
    pub kind: JobKind,
    pub sources: Vec<PathBuf>,
    /// destination folder for Copy/Move; None otherwise
    pub dest: Option<PathBuf>,
}

/// How to resolve "destination already exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    Replace,
    Skip,
    /// copy as "name (2).ext"
    KeepBoth,
    /// stop the whole job
    Cancel,
}

/// One trash item (freedesktop spec).
#[derive(Debug, Clone, PartialEq)]
pub struct TrashEntry {
    /// the file inside `<trash>/files/`
    pub trashed: PathBuf,
    /// its `.trashinfo`
    pub info: PathBuf,
    pub original: PathBuf,
    /// "YYYY-MM-DDThh:mm:ss" from the .trashinfo
    pub deleted_at: String,
    pub size: u64,
}

/// What undo would do for a finished job.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoOp {
    /// move each (current, original) back
    MoveBack(Vec<(PathBuf, PathBuf)>),
    /// trash the copies that were created
    RemoveCopies(Vec<PathBuf>),
    /// restore these trash entries
    Untrash(Vec<TrashEntry>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub items_done: usize,
    pub items_skipped: usize,
    pub bytes_done: u64,
    /// non-fatal per-item errors ("path: reason")
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum JobEvent {
    /// totals known (after a quick scan); `label` e.g. "Copying 3 items to Pictures"
    Started { id: JobId, label: String, total_items: usize, total_bytes: u64 },
    Progress { id: JobId, done_items: usize, done_bytes: u64, current: PathBuf },
    /// the job is paused until `JobControl::resolve` is called
    Conflict { id: JobId, src: PathBuf, dst: PathBuf },
    /// last event of every job; `undo` is None for Delete or when nothing changed
    Finished { id: JobId, result: Result<Summary, String>, undo: Option<UndoOp> },
}

/// Handle to steer a running job. Cheap to clone; safe to drop (the job
/// keeps running; a pending conflict then resolves as Cancel).
#[derive(Debug, Clone)]
pub struct JobControl {
    pub id: JobId,
}

impl JobControl {
    /// Answer the pending conflict. `apply_to_all` reuses the answer for
    /// later conflicts in this job.
    pub fn resolve(&self, r: Resolution, apply_to_all: bool) {}

    pub fn cancel(&self) {}
}

/// Start a job on its own thread. Moves within one filesystem are renames
/// (instant); across filesystems copy-then-delete. Copying a folder into
/// itself is refused. Symlinks are copied as links, not followed.
pub fn start(spec: JobSpec) -> (JobControl, impl Stream<Item = JobEvent> + Send + 'static) {
    let (_, rx) = iced::futures::channel::mpsc::unbounded::<JobEvent>();
    (JobControl { id: 0 }, rx)
}

/// The job that undoes `op`.
pub fn undo_spec(op: &UndoOp) -> JobSpec {
    JobSpec { kind: JobKind::Copy, sources: vec![], dest: None }
}

/// Trash one path right now (used by `start` for JobKind::Trash).
pub fn trash(path: &Path) -> Result<TrashEntry, String> {
    Err(TODO.into())
}

/// Entries in the home trash and every mounted volume's trash.
pub fn list_trash() -> Result<Vec<TrashEntry>, String> {
    Err(TODO.into())
}

/// Restore one entry; fails if the original location is now occupied.
pub fn restore(entry: &TrashEntry) -> Result<PathBuf, String> {
    Err(TODO.into())
}

/// Permanently delete everything in all trashes; returns items removed.
pub fn empty_trash() -> Result<usize, String> {
    Err(TODO.into())
}

/// Create `name` in `dir`, adding " (2)" etc. if taken.
pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    Err(TODO.into())
}

/// Create an empty file `name` in `dir` (unique like new_folder).
pub fn new_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    Err(TODO.into())
}

/// Rename within the same folder. Refuses empty names, '/' and existing
/// targets.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    Err(TODO.into())
}

/// "name (2).ext" style free name in `dir`.
pub fn unique_name(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}
