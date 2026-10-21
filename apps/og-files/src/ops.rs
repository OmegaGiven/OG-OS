//! File operations engine: copy / move / trash / delete / restore as
//! background jobs with progress, conflict prompts, cancel and undo, plus
//! freedesktop.org trash (home trash and per-volume `.Trash-$uid`, so USB
//! drives work) and small synchronous helpers. Public signatures and types
//! are a fixed contract (the UI is built against them).
//!
//! Threading: each job runs on its own OS thread; progress comes back as a
//! `Stream<Item = JobEvent>` the UI runs with `Task::run`. Nothing here may
//! block the caller of `start`.
//!
//! Event protocol of every job: exactly one `Started` first (totals come from
//! a quick scan that does not follow symlinks; a job that fails validation
//! still sends `Started` with zero totals), then any number of `Progress`
//! (throttled to ~10/s, plus one final) and `Conflict`, then exactly one
//! `Finished`. `done_items`/`total_items` count every entry recursively
//! (files, folders, links); `Summary::items_done`/`items_skipped` count the
//! top-level sources. A cancelled job finishes with `Err("Cancelled")` but
//! still carries an `undo` for whatever it completed before the cancel.
//!
//! Undo mapping:
//! * Copy    → `RemoveCopies(created)`        → undone by a Trash job (recoverable).
//! * Move    → `MoveBack((new, original))`    → undone by a Move job, see below.
//! * Trash   → `Untrash(entries)`             → undone by a Restore job.
//! * Restore → `RemoveCopies(restored paths)` → i.e. undo trashes them again.
//! * Delete  → no undo.
//!
//! `MoveBack` and `JobSpec`'s single `dest`: originals may live in different
//! folders (and may have had their names changed by "keep both"), so
//! `undo_spec(MoveBack)` records each (current → original) pair in a private
//! process-wide registry and returns `JobSpec { kind: Move, dest: None, .. }`.
//! `start` treats a Move without a destination as "move each source back to
//! the original recorded for it"; sources with no recorded original fail with
//! a per-item error. Entries are consumed when used.

use iced::futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use iced::futures::Stream;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(target_os = "linux")]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    /// destination folder for Copy/Move; None otherwise. A Move with `None`
    /// moves each source back to its original location recorded by
    /// [`undo_spec`] for a `MoveBack`.
    pub dest: Option<PathBuf>,
}

/// How to resolve "destination already exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Remove the existing item, then copy/move (folders are replaced, not merged).
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
    Started {
        id: JobId,
        label: String,
        total_items: usize,
        total_bytes: u64,
    },
    Progress {
        id: JobId,
        done_items: usize,
        done_bytes: u64,
        current: PathBuf,
    },
    /// the job is paused until `JobControl::resolve` is called
    Conflict {
        id: JobId,
        src: PathBuf,
        dst: PathBuf,
    },
    /// last event of every job; `undo` is None for Delete or when nothing changed
    Finished {
        id: JobId,
        result: Result<Summary, String>,
        undo: Option<UndoOp>,
    },
}

/// Handle to steer a running job. Cheap to clone; safe to drop (the job
/// keeps running; a pending conflict then resolves as Cancel).
#[derive(Debug, Clone)]
pub struct JobControl {
    pub id: JobId,
    cancel: Arc<AtomicBool>,
    answers: mpsc::Sender<(Resolution, bool)>,
}

impl JobControl {
    /// Answer the pending conflict. `apply_to_all` reuses the answer for
    /// later conflicts in this job.
    pub fn resolve(&self, r: Resolution, apply_to_all: bool) {
        if r == Resolution::Cancel {
            self.cancel.store(true, Ordering::SeqCst);
        }
        let _ = self.answers.send((r, apply_to_all));
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        // wakes a job parked on a conflict
        let _ = self.answers.send((Resolution::Cancel, false));
    }
}

/// Start a job on its own thread. Moves within one filesystem are renames
/// (instant); across filesystems copy-then-delete. Copying a folder into
/// itself is refused. Symlinks are copied as links, not followed.
pub fn start(spec: JobSpec) -> (JobControl, impl Stream<Item = JobEvent> + Send + 'static) {
    start_with(spec, Opts::default())
}

/// A job id and control for work that runs outside this engine (archive
/// extract / compress) but shows in the same jobs list. `JobControl::cancel`
/// sets the returned flag; conflicts never happen for these.
pub fn external_control() -> (JobControl, Arc<AtomicBool>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    let (answers, _) = mpsc::channel();
    (
        JobControl {
            id,
            cancel: cancel.clone(),
            answers,
        },
        cancel,
    )
}

/// The job that undoes `op`. For `MoveBack` this also records the original
/// locations so the returned `Move` job (with `dest: None`) knows where each
/// source goes (see the module docs).
pub fn undo_spec(op: &UndoOp) -> JobSpec {
    match op {
        UndoOp::MoveBack(pairs) => {
            let mut reg = move_back_registry();
            for (cur, orig) in pairs {
                reg.insert(cur.clone(), orig.clone());
            }
            JobSpec {
                kind: JobKind::Move,
                sources: pairs.iter().map(|(c, _)| c.clone()).collect(),
                dest: None,
            }
        }
        UndoOp::RemoveCopies(paths) => JobSpec {
            kind: JobKind::Trash,
            sources: paths.clone(),
            dest: None,
        },
        UndoOp::Untrash(entries) => JobSpec {
            kind: JobKind::Restore,
            sources: entries.iter().map(|e| e.info.clone()).collect(),
            dest: None,
        },
    }
}

/// Create `name` in `dir`, adding " (2)" etc. if taken.
pub fn new_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    create_unique(dir, name, |p| DirBuilder::new().create(p))
}

/// Create file `name` in `dir` holding `body` (unique like new_folder).
pub fn new_file_with(dir: &Path, name: &str, body: &str) -> Result<PathBuf, String> {
    create_unique(dir, name, |p| {
        use std::io::Write;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)?
            .write_all(body.as_bytes())
    })
}

/// Rename within the same folder. Refuses empty names, '/' and existing
/// targets.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    validate_name(new_name)?;
    let (Some(parent), Some(cur)) = (path.parent(), path.file_name()) else {
        return Err("Can't rename this item".into());
    };
    let meta = fs::symlink_metadata(path)
        .map_err(|_| format!("“{}” no longer exists", cur.to_string_lossy()))?;
    if cur == OsStr::new(new_name) {
        return Ok(path.to_path_buf());
    }
    let target = parent.join(new_name);
    if let Ok(t) = fs::symlink_metadata(&target) {
        // a case-only rename on a case-insensitive drive sees itself
        if !same_file(path, &meta, &target, &t) {
            return Err(format!("“{new_name}” already exists"));
        }
    }
    fs::rename(path, &target).map_err(|e| io_msg(&e))?;
    Ok(target)
}

/// "name (2).ext" style free name in `dir`.
pub fn unique_name(dir: &Path, name: &str) -> PathBuf {
    unique_in(dir, OsStr::new(name))
}

// ───────────────────────────── job engine ─────────────────────────────

const CHUNK: usize = 1 << 20;
const TICK: Duration = Duration::from_millis(100);
const CANCELLED: &str = "Cancelled";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Internal knobs, for tests.
#[derive(Debug, Clone, Copy, Default)]
struct Opts {
    /// pretend every move crosses filesystems (copy + delete)
    force_copy: bool,
    /// sleep after every copied chunk (makes cancel tests deterministic)
    chunk_delay: Option<Duration>,
}

fn start_with(spec: JobSpec, opts: Opts) -> (JobControl, UnboundedReceiver<JobEvent>) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = unbounded();
    let (atx, arx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let job = Job {
        id,
        tx: tx.clone(),
        cancel: cancel.clone(),
        answers: arx,
        apply_all: None,
        opts,
        done_items: 0,
        done_bytes: 0,
        bytes_work: 0,
        last_tick: Instant::now(),
        current: PathBuf::new(),
        items_done: 0,
        items_skipped: 0,
        errors: Vec::new(),
        created: Vec::new(),
    };
    let spawned = std::thread::Builder::new()
        .name(format!("ogx-job-{id}"))
        .spawn(move || job.run(spec));
    if spawned.is_err() {
        let _ = tx.unbounded_send(JobEvent::Started {
            id,
            label: "File operation".into(),
            total_items: 0,
            total_bytes: 0,
        });
        let _ = tx.unbounded_send(JobEvent::Finished {
            id,
            result: Err("Couldn't start the job".into()),
            undo: None,
        });
    }
    (
        JobControl {
            id,
            cancel,
            answers: atx,
        },
        rx,
    )
}

struct Cancelled;

struct Job {
    id: JobId,
    tx: UnboundedSender<JobEvent>,
    cancel: Arc<AtomicBool>,
    answers: mpsc::Receiver<(Resolution, bool)>,
    apply_all: Option<Resolution>,
    opts: Opts,
    /// progress counters (recursive entries / bytes, including skipped)
    done_items: usize,
    done_bytes: u64,
    /// bytes actually copied/moved/trashed (Summary::bytes_done)
    bytes_work: u64,
    last_tick: Instant,
    current: PathBuf,
    items_done: usize,
    items_skipped: usize,
    errors: Vec<String>,
    /// top-level targets a Copy created (also partial ones, for undo)
    created: Vec<PathBuf>,
}

impl Job {
    fn run(mut self, spec: JobSpec) {
        let id = self.id;
        let tx = self.tx.clone();
        let started = Arc::new(AtomicBool::new(false));
        let started2 = started.clone();
        let outcome = catch_unwind(AssertUnwindSafe(move || {
            let (res, undo) = self.body(&spec, &started2);
            self.tick(true);
            let result = res.map(|()| Summary {
                items_done: self.items_done,
                items_skipped: self.items_skipped,
                bytes_done: self.bytes_work,
                errors: std::mem::take(&mut self.errors),
            });
            JobEvent::Finished { id, result, undo }
        }));
        let finished = outcome.unwrap_or_else(|_| JobEvent::Finished {
            id,
            result: Err("Internal error".into()),
            undo: None,
        });
        if !started.load(Ordering::SeqCst) {
            let _ = tx.unbounded_send(JobEvent::Started {
                id,
                label: "File operation".into(),
                total_items: 0,
                total_bytes: 0,
            });
        }
        let _ = tx.unbounded_send(finished);
    }

    fn body(
        &mut self,
        spec: &JobSpec,
        started: &AtomicBool,
    ) -> (Result<(), String>, Option<UndoOp>) {
        // Restore sources may be .trashinfo or trashed-file paths.
        let entries: Vec<Result<TrashEntry, String>> = if spec.kind == JobKind::Restore {
            spec.sources.iter().map(|p| entry_from_path(p)).collect()
        } else {
            Vec::new()
        };
        let sizes: Vec<(usize, u64)> = if spec.kind == JobKind::Restore {
            entries
                .iter()
                .map(|e| {
                    e.as_ref()
                        .map(|e| scan(&e.trashed, &self.cancel))
                        .unwrap_or((0, 0))
                })
                .collect()
        } else {
            spec.sources.iter().map(|p| scan(p, &self.cancel)).collect()
        };
        let total_items = sizes.iter().map(|s| s.0).sum();
        let total_bytes = sizes.iter().map(|s| s.1).sum();
        self.send(JobEvent::Started {
            id: self.id,
            label: label(spec),
            total_items,
            total_bytes,
        });
        started.store(true, Ordering::SeqCst);
        if self.cancelled() {
            return (Err(CANCELLED.into()), None);
        }

        match spec.kind {
            JobKind::Copy => {
                let dest = match check_dest(spec.dest.as_deref()) {
                    Ok(d) => d,
                    Err(e) => return (Err(e), None),
                };
                let mut res = Ok(());
                for (src, size) in spec.sources.iter().zip(&sizes) {
                    if self.copy_item(src, &dest, *size).is_err() {
                        res = Err(CANCELLED.into());
                        break;
                    }
                }
                let undo =
                    (!self.created.is_empty()).then(|| UndoOp::RemoveCopies(self.created.clone()));
                (res, undo)
            }
            JobKind::Move => {
                let dest = match &spec.dest {
                    Some(d) => match check_dest(Some(d)) {
                        Ok(d) => Some(d),
                        Err(e) => return (Err(e), None),
                    },
                    None => None,
                };
                let mut moved = Vec::new();
                let mut res = Ok(());
                for (src, size) in spec.sources.iter().zip(&sizes) {
                    let target = match &dest {
                        Some(d) => match src.file_name() {
                            Some(n) => d.join(n),
                            None => {
                                self.fail(src, "can't move this item");
                                self.advance(size.0, size.1);
                                continue;
                            }
                        },
                        None => match take_original(src) {
                            Some(o) => o,
                            None => {
                                self.fail(src, "original location unknown");
                                self.advance(size.0, size.1);
                                continue;
                            }
                        },
                    };
                    match self.move_item(src, target, *size, dest.is_none()) {
                        Ok(Some(pair)) => moved.push(pair),
                        Ok(None) => {}
                        Err(Cancelled) => {
                            res = Err(CANCELLED.into());
                            break;
                        }
                    }
                }
                (res, (!moved.is_empty()).then_some(UndoOp::MoveBack(moved)))
            }
            JobKind::Trash => {
                let mut trashed = Vec::new();
                let mut res = Ok(());
                for (src, size) in spec.sources.iter().zip(&sizes) {
                    if self.cancelled() {
                        res = Err(CANCELLED.into());
                        break;
                    }
                    self.current = src.clone();
                    match trash(src) {
                        Ok(e) => {
                            trashed.push(e);
                            self.items_done += 1;
                            self.bytes_work += size.1;
                        }
                        Err(m) => self.fail(src, &m),
                    }
                    self.advance(size.0, size.1);
                }
                (
                    res,
                    // macOS: the system trash can't be listed, so no undo
                    (TRASH_UNDO && !trashed.is_empty()).then_some(UndoOp::Untrash(trashed)),
                )
            }
            JobKind::Delete => {
                let mut res = Ok(());
                for (src, size) in spec.sources.iter().zip(&sizes) {
                    if self.cancelled() {
                        res = Err(CANCELLED.into());
                        break;
                    }
                    self.current = src.clone();
                    match remove_any(src) {
                        Ok(()) => {
                            self.items_done += 1;
                            self.bytes_work += size.1;
                        }
                        Err(e) => self.fail(src, &io_msg(&e)),
                    }
                    self.advance(size.0, size.1);
                }
                (res, None)
            }
            JobKind::Restore => {
                let mut restored = Vec::new();
                let mut res = Ok(());
                for ((src, entry), size) in spec.sources.iter().zip(entries).zip(&sizes) {
                    if self.cancelled() {
                        res = Err(CANCELLED.into());
                        break;
                    }
                    self.current = src.clone();
                    match entry.and_then(|e| restore(&e)) {
                        Ok(p) => {
                            restored.push(p);
                            self.items_done += 1;
                            self.bytes_work += size.1;
                        }
                        Err(m) => self.fail(src, &m),
                    }
                    self.advance(size.0, size.1);
                }
                (
                    res,
                    (!restored.is_empty()).then_some(UndoOp::RemoveCopies(restored)),
                )
            }
        }
    }

    // ── plumbing ──

    fn send(&self, ev: JobEvent) {
        let _ = self.tx.unbounded_send(ev);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn check(&self) -> Result<(), Cancelled> {
        if self.cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, items: usize, bytes: u64) {
        self.done_items += items;
        self.done_bytes += bytes;
        self.tick(false);
    }

    fn tick(&mut self, force: bool) {
        if force || self.last_tick.elapsed() >= TICK {
            self.last_tick = Instant::now();
            self.send(JobEvent::Progress {
                id: self.id,
                done_items: self.done_items,
                done_bytes: self.done_bytes,
                current: self.current.clone(),
            });
        }
    }

    fn fail(&mut self, path: &Path, msg: &str) {
        self.errors.push(format!("{}: {}", path.display(), msg));
    }

    /// Ask the UI about an existing destination; blocks until answered.
    fn ask(&mut self, src: &Path, dst: &Path) -> Resolution {
        if let Some(r) = self.apply_all {
            return r;
        }
        while self.answers.try_recv().is_ok() {} // stale answers
        if self.cancelled() {
            return Resolution::Cancel;
        }
        self.tick(true);
        self.send(JobEvent::Conflict {
            id: self.id,
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
        });
        let (r, all) = loop {
            match self.answers.recv_timeout(Duration::from_millis(250)) {
                Ok(a) => break a,
                Err(mpsc::RecvTimeoutError::Timeout) if !self.cancelled() => continue,
                Err(_) => break (Resolution::Cancel, false),
            }
        };
        if r == Resolution::Cancel {
            self.cancel.store(true, Ordering::SeqCst);
        } else if all {
            self.apply_all = Some(r);
        }
        r
    }

    /// Resolve an existing `target`: Ok(Some(path to write)) or Ok(None)
    /// when the item is skipped/failed (already accounted for).
    fn resolve_conflict(
        &mut self,
        src: &Path,
        src_abs: &Path,
        target: PathBuf,
        size: (usize, u64),
    ) -> Result<Option<PathBuf>, Cancelled> {
        match self.ask(src, &target) {
            Resolution::Cancel => Err(Cancelled),
            Resolution::Skip => {
                self.items_skipped += 1;
                self.advance(size.0, size.1);
                Ok(None)
            }
            Resolution::KeepBoth => {
                let dir = target.parent().unwrap_or(Path::new("/"));
                let name = target.file_name().unwrap_or(OsStr::new("item"));
                Ok(Some(unique_in(dir, name)))
            }
            Resolution::Replace => {
                if src_abs.starts_with(&target) {
                    self.fail(src, "can't replace a folder with something inside it");
                    self.advance(size.0, size.1);
                    return Ok(None);
                }
                if let Err(e) = remove_any(&target) {
                    self.fail(&target, &format!("couldn't replace: {}", io_msg(&e)));
                    self.advance(size.0, size.1);
                    return Ok(None);
                }
                Ok(Some(target))
            }
        }
    }

    // ── copy ──

    fn copy_item(&mut self, src: &Path, dest: &Path, size: (usize, u64)) -> Result<(), Cancelled> {
        self.check()?;
        self.current = src.to_path_buf();
        let (Some(name), Some(src_abs)) = (src.file_name(), absolute_nofollow(src)) else {
            self.fail(src, "can't copy this item");
            self.advance(size.0, size.1);
            return Ok(());
        };
        let meta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.fail(src, &io_msg(&e));
                self.advance(size.0, size.1);
                return Ok(());
            }
        };
        if meta.is_dir() && dest.starts_with(&src_abs) {
            self.fail(src, "can't copy a folder into itself");
            self.advance(size.0, size.1);
            return Ok(());
        }
        let mut target = dest.join(name);
        if target == src_abs {
            target = unique_in(dest, name); // duplicate in place
        } else if exists(&target) {
            match self.resolve_conflict(src, &src_abs, target, size)? {
                Some(t) => target = t,
                None => return Ok(()),
            }
        }
        if self.copy_tree(&src_abs, &target, true)? {
            self.items_done += 1;
        }
        Ok(())
    }

    /// Recursive copy; returns whether everything was copied. `top` records
    /// the created target in `self.created`.
    fn copy_tree(&mut self, src: &Path, dst: &Path, top: bool) -> Result<bool, Cancelled> {
        self.check()?;
        self.current = src.to_path_buf();
        let meta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.fail(src, &io_msg(&e));
                return Ok(false);
            }
        };
        let ft = meta.file_type();
        if ft.is_symlink() {
            let r = fs::read_link(src).and_then(|t| crate::platform::symlink(&t, dst));
            self.advance(1, 0);
            return Ok(match r {
                Ok(()) => {
                    if top {
                        self.created.push(dst.to_path_buf());
                    }
                    true
                }
                Err(e) => {
                    self.fail(src, &io_msg(&e));
                    false
                }
            });
        }
        if ft.is_dir() {
            if let Err(e) = private_dir(dst) {
                self.fail(dst, &io_msg(&e));
                return Ok(false);
            }
            if top {
                self.created.push(dst.to_path_buf());
            }
            self.advance(1, 0);
            let mut names: Vec<OsString> = match fs::read_dir(src) {
                Ok(rd) => rd.flatten().map(|e| e.file_name()).collect(),
                Err(e) => {
                    self.fail(src, &io_msg(&e));
                    let _ = fs::set_permissions(dst, meta.permissions());
                    return Ok(false);
                }
            };
            names.sort();
            let mut ok = true;
            for n in names {
                ok &= self.copy_tree(&src.join(&n), &dst.join(&n), false)?;
            }
            let _ = fs::set_permissions(dst, meta.permissions());
            if let (Ok(m), Ok(f)) = (meta.modified(), File::open(dst)) {
                let _ = f.set_modified(m);
            }
            return Ok(ok);
        }
        if ft.is_file() {
            let before = self.done_bytes;
            let before_work = self.bytes_work;
            return match self.copy_file(src, dst, &meta) {
                Ok(()) => {
                    if top {
                        self.created.push(dst.to_path_buf());
                    }
                    self.advance(1, 0);
                    Ok(true)
                }
                Err(err) => {
                    self.done_bytes = before;
                    self.bytes_work = before_work;
                    if err.created {
                        let _ = fs::remove_file(dst);
                    }
                    match err.io {
                        None => Err(Cancelled),
                        Some(e) => {
                            self.fail(src, &io_msg(&e));
                            self.advance(1, 0);
                            Ok(false)
                        }
                    }
                }
            };
        }
        self.fail(src, "special files are skipped");
        self.advance(1, 0);
        Ok(false)
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, meta: &fs::Metadata) -> Result<(), CopyErr> {
        let mut input = File::open(src).map_err(|e| CopyErr::io(e, false))?;
        let mut out = private_new_file(dst).map_err(|e| CopyErr::io(e, false))?;
        let mut buf = vec![0u8; (meta.len() as usize).clamp(8 * 1024, CHUNK)];
        loop {
            if self.cancelled() {
                return Err(CopyErr {
                    io: None,
                    created: true,
                });
            }
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(CopyErr::io(e, true)),
            };
            out.write_all(&buf[..n]).map_err(|e| CopyErr::io(e, true))?;
            self.bytes_work += n as u64;
            self.advance(0, n as u64);
            if let Some(d) = self.opts.chunk_delay {
                std::thread::sleep(d);
            }
        }
        out.flush().map_err(|e| CopyErr::io(e, true))?;
        let _ = out.set_permissions(meta.permissions());
        if let Ok(m) = meta.modified() {
            let _ = out.set_modified(m);
        }
        Ok(())
    }

    // ── move ──

    /// Move `src` to exactly `target`. `back` = undo mode (recreate parents).
    fn move_item(
        &mut self,
        src: &Path,
        target: PathBuf,
        size: (usize, u64),
        back: bool,
    ) -> Result<Option<(PathBuf, PathBuf)>, Cancelled> {
        self.check()?;
        self.current = src.to_path_buf();
        let meta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.fail(src, &io_msg(&e));
                self.advance(size.0, size.1);
                return Ok(None);
            }
        };
        let Some(src_abs) = absolute_nofollow(src) else {
            self.fail(src, "can't move this item");
            self.advance(size.0, size.1);
            return Ok(None);
        };
        if back {
            if let Some(p) = target.parent() {
                if let Err(e) = fs::create_dir_all(p) {
                    self.fail(
                        src,
                        &format!("can't recreate the original folder: {}", io_msg(&e)),
                    );
                    self.advance(size.0, size.1);
                    return Ok(None);
                }
            }
        }
        if target == src_abs {
            self.items_skipped += 1; // already there
            self.advance(size.0, size.1);
            return Ok(None);
        }
        if meta.is_dir() && target.starts_with(&src_abs) {
            self.fail(src, "can't move a folder into itself");
            self.advance(size.0, size.1);
            return Ok(None);
        }
        let mut target = target;
        if exists(&target) {
            match self.resolve_conflict(src, &src_abs, target, size)? {
                Some(t) => target = t,
                None => return Ok(None),
            }
        }
        if !self.opts.force_copy {
            match fs::rename(&src_abs, &target) {
                Ok(()) => {
                    self.items_done += 1;
                    self.bytes_work += size.1;
                    self.advance(size.0, size.1);
                    return Ok(Some((target, src_abs)));
                }
                Err(e) if is_exdev(&e) => {}
                Err(e) => {
                    self.fail(src, &io_msg(&e));
                    self.advance(size.0, size.1);
                    return Ok(None);
                }
            }
        }
        // across filesystems: copy, then delete the source only on full success
        let mark = self.created.len();
        let copied = self.copy_tree(&src_abs, &target, true);
        let made = self.created.len() > mark;
        self.created.truncate(mark);
        match copied {
            Err(Cancelled) => {
                if made {
                    let _ = remove_any(&target);
                }
                Err(Cancelled)
            }
            Ok(false) => {
                if made {
                    let _ = remove_any(&target);
                }
                self.fail(src, "couldn't copy everything; the original was kept");
                Ok(None)
            }
            Ok(true) => {
                if let Err(e) = remove_any(&src_abs) {
                    self.fail(
                        src,
                        &format!("copied, but couldn't remove the original: {}", io_msg(&e)),
                    );
                }
                self.items_done += 1;
                Ok(Some((target, src_abs)))
            }
        }
    }
}

struct CopyErr {
    /// None = cancelled
    io: Option<io::Error>,
    /// the destination file was created by us (remove it)
    created: bool,
}

impl CopyErr {
    fn io(e: io::Error, created: bool) -> Self {
        CopyErr {
            io: Some(e),
            created,
        }
    }
}

fn check_dest(dest: Option<&Path>) -> Result<PathBuf, String> {
    let d = dest.ok_or("No destination folder")?;
    match fs::metadata(d) {
        Ok(m) if m.is_dir() => Ok(fs::canonicalize(d).unwrap_or_else(|_| d.to_path_buf())),
        Ok(_) => Err("The destination is not a folder".into()),
        Err(_) => Err("The destination folder doesn't exist".into()),
    }
}

/// (entries, file bytes) below `path`, not following symlinks.
fn scan(path: &Path, cancel: &AtomicBool) -> (usize, u64) {
    let (mut items, mut bytes) = (0usize, 0u64);
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(m) = fs::symlink_metadata(&p) else {
            continue;
        };
        items += 1;
        if m.is_dir() {
            if let Ok(rd) = fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        } else if m.is_file() {
            bytes += m.len();
        }
    }
    (items, bytes)
}

fn label(spec: &JobSpec) -> String {
    let n = spec.sources.len();
    let what = if n == 1 {
        format!("“{}”", display_name(&spec.sources[0]))
    } else {
        format!("{n} items")
    };
    let to = spec.dest.as_deref().map(display_name).unwrap_or_default();
    match spec.kind {
        JobKind::Copy => format!("Copying {what} to {to}"),
        JobKind::Move if spec.dest.is_some() => format!("Moving {what} to {to}"),
        JobKind::Move => format!("Moving {what} back"),
        JobKind::Trash => format!("Moving {what} to the Trash"),
        JobKind::Delete => format!("Deleting {what}"),
        JobKind::Restore => format!("Restoring {what}"),
    }
}

fn move_back_registry() -> MutexGuard<'static, HashMap<PathBuf, PathBuf>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, PathBuf>>> = OnceLock::new();
    REG.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn take_original(src: &Path) -> Option<PathBuf> {
    let mut reg = move_back_registry();
    reg.remove(src)
        .or_else(|| absolute_nofollow(src).and_then(|a| reg.remove(&a)))
}

// ───────────────────────────── trash ─────────────────────────────

#[cfg(not(target_os = "linux"))]
use os_trash::entry_from_path;
#[cfg(not(target_os = "linux"))]
pub use os_trash::{empty_trash, list_trash, restore, trash};
#[cfg(target_os = "linux")]
use xdg_trash::entry_from_path;
#[cfg(all(test, target_os = "linux"))]
use xdg_trash::*;
#[cfg(target_os = "linux")]
pub use xdg_trash::{empty_trash, list_trash, restore, trash};

/// The Trash view (list / restore / empty) works on this OS. macOS: no —
/// the `trash` crate can only move items there (use the Finder).
pub const TRASH_VIEW: bool = cfg!(any(target_os = "linux", windows));
/// Moving to the Trash can be undone (needs the view's restore).
pub const TRASH_UNDO: bool = TRASH_VIEW;

/// The freedesktop.org trash (home trash + per-volume `.Trash-$uid`).
#[cfg(target_os = "linux")]
mod xdg_trash {
    use super::*;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    /// Trash one path right now (used by `start` for JobKind::Trash).
    pub fn trash(path: &Path) -> Result<TrashEntry, String> {
        let meta = fs::symlink_metadata(path).map_err(|e| io_msg(&e))?;
        let abs = absolute_nofollow(path).ok_or("Can't move this item to the Trash")?;
        let parent = abs.parent().ok_or("Can't move this item to the Trash")?;
        let pdev = fs::metadata(parent).map_err(|e| io_msg(&e))?.dev();
        let home = trash_home().ok_or("No home folder, so there is no Trash")?;
        if abs.starts_with(&home) {
            return Err("Already in the Trash".into());
        }
        if home.starts_with(&abs) {
            return Err("Can't move a folder that contains the Trash".into());
        }
        let (root, topdir) = if nearest_dev(&home) == Some(pdev) {
            (home, None)
        } else {
            if cfg!(test) {
                // tests must never touch a real volume's trash
                return Err("test build: cross-device trash disabled".into());
            }
            let top = mount_point(parent, pdev);
            (volume_trash_dir(&top, current_uid(), true)?, Some(top))
        };
        trash_into(&abs, &meta, &root, topdir.as_deref())
    }

    /// Entries in the home trash and every mounted volume's trash.
    pub fn list_trash() -> Result<Vec<TrashEntry>, String> {
        let mut out = Vec::new();
        for root in all_trash_roots() {
            out.extend(list_root(&root));
        }
        out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
        Ok(out)
    }

    /// Restore one entry; fails if the original location is now occupied.
    pub fn restore(entry: &TrashEntry) -> Result<PathBuf, String> {
        let name = display_name(&entry.original);
        if fs::symlink_metadata(&entry.original).is_ok() {
            return Err(format!("“{name}” already exists in its original folder"));
        }
        if fs::symlink_metadata(&entry.trashed).is_err() {
            return Err(format!("“{name}” is no longer in the Trash"));
        }
        if let Some(parent) = entry.original.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Can't recreate the original folder: {}", io_msg(&e)))?;
        }
        match fs::rename(&entry.trashed, &entry.original) {
            Ok(()) => {}
            Err(e) if is_exdev(&e) => {
                if let Err(e) = copy_plain(&entry.trashed, &entry.original) {
                    let _ = remove_any(&entry.original);
                    return Err(io_msg(&e));
                }
                remove_any(&entry.trashed).map_err(|e| {
                    format!(
                        "Restored a copy, but couldn't remove it from the Trash: {}",
                        io_msg(&e)
                    )
                })?;
            }
            Err(e) => return Err(io_msg(&e)),
        }
        let _ = fs::remove_file(&entry.info);
        Ok(entry.original.clone())
    }

    /// Permanently delete everything in all trashes; returns items removed.
    pub fn empty_trash() -> Result<usize, String> {
        empty_roots(&all_trash_roots())
    }

    pub(super) fn current_uid() -> u32 {
        fs::metadata("/proc/self")
            .map(|m| m.uid())
            .or_else(|_| {
                fs::metadata(std::env::var_os("HOME").unwrap_or_default()).map(|m| m.uid())
            })
            .unwrap_or(u32::MAX)
    }

    /// `$XDG_DATA_HOME/Trash`, default `~/.local/share/Trash`. Test builds
    /// always use a private directory instead.
    pub(super) fn trash_home() -> Option<PathBuf> {
        #[cfg(test)]
        {
            Some(super::tests::home_trash())
        }
        #[cfg(not(test))]
        {
            let xdg = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute());
            let data = match xdg {
                Some(p) => p,
                None => PathBuf::from(std::env::var_os("HOME").filter(|h| !h.is_empty())?)
                    .join(".local/share"),
            };
            Some(data.join("Trash"))
        }
    }

    /// Device of `p` or of its nearest existing ancestor.
    pub(super) fn nearest_dev(p: &Path) -> Option<u64> {
        p.ancestors()
            .find_map(|a| fs::metadata(a).ok())
            .map(|m| m.dev())
    }

    /// Topmost ancestor of `start` still on device `dev`.
    pub(super) fn mount_point(start: &Path, dev: u64) -> PathBuf {
        let mut cur = start;
        while let Some(p) = cur.parent() {
            match fs::metadata(p) {
                Ok(m) if m.dev() == dev => cur = p,
                _ => break,
            }
        }
        cur.to_path_buf()
    }

    /// `$topdir/.Trash` usable as the shared trash: a real dir with sticky bit.
    pub(super) fn shared_trash_ok(top: &Path) -> bool {
        fs::symlink_metadata(top.join(".Trash"))
            .map(|m| m.file_type().is_dir() && m.mode() & 0o1000 != 0)
            .unwrap_or(false)
    }

    /// The trash directory for files on the volume mounted at `top`:
    /// `$top/.Trash/$uid` if `.Trash` passes the spec's checks, else
    /// `$top/.Trash-$uid` (created 0700 when `create`).
    pub(super) fn volume_trash_dir(top: &Path, uid: u32, create: bool) -> Result<PathBuf, String> {
        if shared_trash_ok(top) {
            let d = top.join(".Trash").join(uid.to_string());
            match fs::symlink_metadata(&d) {
                Ok(m) if m.file_type().is_dir() => return Ok(d),
                Err(e) if create && e.kind() == io::ErrorKind::NotFound => {
                    if DirBuilder::new().mode(0o700).create(&d).is_ok() {
                        return Ok(d);
                    }
                }
                _ => {}
            }
        }
        let own = top.join(format!(".Trash-{uid}"));
        match fs::symlink_metadata(&own) {
            Ok(m) if m.file_type().is_dir() && m.uid() == uid => Ok(own),
            Ok(_) => Err("This drive's trash folder is unusable".into()),
            Err(e) if create && e.kind() == io::ErrorKind::NotFound => DirBuilder::new()
                .mode(0o700)
                .create(&own)
                .map(|()| own)
                .map_err(|e| format!("Can't create a trash folder on this drive ({})", io_msg(&e))),
            Err(_) => Err("This drive has no trash folder".into()),
        }
    }

    pub(super) fn ensure_trash_dirs(root: &Path) -> Result<(), String> {
        for sub in ["files", "info"] {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(root.join(sub))
                .map_err(|e| format!("Can't open the Trash: {}", io_msg(&e)))?;
        }
        Ok(())
    }

    /// Move `abs` into the trash at `root`. `topdir` = volume mount point for a
    /// volume trash (Path= is then relative to it), None for the home trash.
    pub(super) fn trash_into(
        abs: &Path,
        meta: &fs::Metadata,
        root: &Path,
        topdir: Option<&Path>,
    ) -> Result<TrashEntry, String> {
        ensure_trash_dirs(root)?;
        let name = abs.file_name().ok_or("Can't move this item to the Trash")?;
        let recorded = match topdir {
            Some(t) => abs.strip_prefix(t).unwrap_or(abs),
            None => abs,
        };
        let deleted_at = format_civil(now_local());
        let content = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            encode_path(recorded),
            deleted_at
        );
        let size = if meta.is_dir() {
            scan(abs, &AtomicBool::new(false)).1
        } else {
            meta.len()
        };
        let (stem, ext) = split_name(name.as_bytes());
        let (base, _) = strip_counter(stem);
        for n in 1..10_000u32 {
            let cand: OsString = if n == 1 {
                name.to_os_string()
            } else {
                numbered(base, ext, n)
            };
            let mut info_name = cand.clone();
            info_name.push(".trashinfo");
            let info = root.join("info").join(&info_name);
            let trashed = root.join("files").join(&cand);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&info)
            {
                Ok(mut f) => {
                    if exists(&trashed) {
                        drop(f);
                        let _ = fs::remove_file(&info);
                        continue;
                    }
                    if let Err(e) = f.write_all(content.as_bytes()).and_then(|()| f.flush()) {
                        drop(f);
                        let _ = fs::remove_file(&info);
                        return Err(io_msg(&e));
                    }
                    drop(f);
                    if let Err(e) = fs::rename(abs, &trashed) {
                        let _ = fs::remove_file(&info);
                        return Err(if is_exdev(&e) {
                            "Can't move this item to the Trash on another drive".into()
                        } else {
                            io_msg(&e)
                        });
                    }
                    return Ok(TrashEntry {
                        trashed,
                        info,
                        original: abs.to_path_buf(),
                        deleted_at,
                        size,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_msg(&e)),
            }
        }
        Err("Too many items with this name in the Trash".into())
    }

    /// Mount points from /proc/mounts (octal escapes decoded), minus pseudo fs.
    pub(super) fn mount_points() -> Vec<PathBuf> {
        const PSEUDO: &[&str] = &[
            "proc",
            "sysfs",
            "devtmpfs",
            "devpts",
            "cgroup",
            "cgroup2",
            "securityfs",
            "pstore",
            "bpf",
            "tracefs",
            "debugfs",
            "configfs",
            "fusectl",
            "mqueue",
            "hugetlbfs",
            "autofs",
            "efivarfs",
            "binfmt_misc",
            "rpc_pipefs",
            "nsfs",
            "ramfs",
            "selinuxfs",
        ];
        let Ok(text) = fs::read_to_string("/proc/mounts") else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                let (_, mp, ty) = (it.next()?, it.next()?, it.next()?);
                (!PSEUDO.contains(&ty))
                    .then(|| PathBuf::from(OsString::from_vec(unescape_mount(mp))))
            })
            .collect()
    }

    pub(super) fn unescape_mount(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\'
                && i + 3 < b.len()
                && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
            {
                let v = (b[i + 1] - b'0') as u32 * 64
                    + (b[i + 2] - b'0') as u32 * 8
                    + (b[i + 3] - b'0') as u32;
                out.push(v as u8);
                i += 4;
            } else {
                out.push(b[i]);
                i += 1;
            }
        }
        out
    }

    /// Home trash plus every readable volume trash of this user.
    pub(super) fn all_trash_roots() -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = trash_home().into_iter().collect();
        if cfg!(test) {
            return roots; // never look at real trashes from tests
        }
        let uid = current_uid();
        let mut seen: Vec<PathBuf> = roots
            .iter()
            .filter_map(|r| fs::canonicalize(r).ok())
            .collect();
        for mp in mount_points() {
            let mut cands = Vec::new();
            if shared_trash_ok(&mp) {
                cands.push(mp.join(".Trash").join(uid.to_string()));
            }
            cands.push(mp.join(format!(".Trash-{uid}")));
            for c in cands {
                let ok = fs::symlink_metadata(&c)
                    .map(|m| m.file_type().is_dir())
                    .unwrap_or(false)
                    && fs::read_dir(c.join("info")).is_ok();
                if !ok {
                    continue;
                }
                let canon = fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
                if !seen.contains(&canon) {
                    seen.push(canon);
                    roots.push(c);
                }
            }
        }
        roots
    }

    pub(super) fn list_root(root: &Path) -> Vec<TrashEntry> {
        let Ok(rd) = fs::read_dir(root.join("info")) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension() != Some(OsStr::new("trashinfo")) {
                    return None;
                }
                entry_from_path(&p).ok()
            })
            .collect()
    }

    /// Build an entry from a `.trashinfo` path or a `<trash>/files/<name>` path.
    pub(super) fn entry_from_path(p: &Path) -> Result<TrashEntry, String> {
        let not_trash = || "Not an item in the Trash".to_string();
        let parent = p.parent().ok_or_else(not_trash)?;
        let pname = parent.file_name().ok_or_else(not_trash)?;
        let root = parent.parent().ok_or_else(not_trash)?;
        let stem: OsString = if pname == "info" && p.extension() == Some(OsStr::new("trashinfo")) {
            let b = p.file_name().ok_or_else(not_trash)?.as_bytes();
            OsString::from_vec(b[..b.len() - ".trashinfo".len()].to_vec())
        } else if pname == "files" {
            p.file_name().ok_or_else(not_trash)?.to_os_string()
        } else {
            return Err(not_trash());
        };
        let mut info_name = stem.clone();
        info_name.push(".trashinfo");
        let info = root.join("info").join(info_name);
        let trashed = root.join("files").join(&stem);
        let (original, deleted_at) = parse_info(&info, root)?;
        let meta = fs::symlink_metadata(&trashed)
            .map_err(|_| "The trashed item is missing".to_string())?;
        let size = if meta.is_dir() {
            scan(&trashed, &AtomicBool::new(false)).1
        } else {
            meta.len()
        };
        Ok(TrashEntry {
            trashed,
            info,
            original,
            deleted_at,
            size,
        })
    }

    /// Volume topdir for a trash root (`$top/.Trash-$uid` or `$top/.Trash/$uid`).
    pub(super) fn topdir_of(root: &Path) -> PathBuf {
        let name = root.file_name().map(|n| n.as_bytes()).unwrap_or_default();
        let parent = root.parent().unwrap_or(Path::new("/"));
        if name.starts_with(b".Trash-") {
            parent.to_path_buf()
        } else if parent.file_name() == Some(OsStr::new(".Trash")) {
            parent.parent().unwrap_or(Path::new("/")).to_path_buf()
        } else {
            parent.to_path_buf()
        }
    }

    pub(super) fn parse_info(info: &Path, root: &Path) -> Result<(PathBuf, String), String> {
        let text = fs::read_to_string(info).map_err(|_| "Can't read the trash info".to_string())?;
        let mut in_group = false;
        let (mut path, mut date) = (None, String::new());
        for line in text.lines() {
            let line = line.trim_end();
            if line.starts_with('[') {
                in_group = line == "[Trash Info]";
            } else if in_group {
                if let Some(v) = line.strip_prefix("Path=") {
                    path = Some(decode_path(v));
                } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                    date = v.to_string();
                }
            }
        }
        let path = path
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("Broken trash info")?;
        if path.components().any(|c| c == Component::ParentDir) {
            return Err("Broken trash info".into());
        }
        let original = if path.is_absolute() {
            path
        } else {
            topdir_of(root).join(path)
        };
        Ok((original, date))
    }

    pub(super) fn empty_roots(roots: &[PathBuf]) -> Result<usize, String> {
        let (mut removed, mut failed) = (0usize, 0usize);
        for root in roots {
            for sub in ["files", "info"] {
                let Ok(rd) = fs::read_dir(root.join(sub)) else {
                    continue;
                };
                for e in rd.flatten() {
                    match remove_any(&e.path()) {
                        Ok(()) if sub == "files" => removed += 1,
                        Ok(()) => {}
                        Err(_) => failed += 1,
                    }
                }
            }
            let _ = fs::remove_file(root.join("directorysizes"));
        }
        if failed > 0 {
            Err(format!(
                "Removed {removed} items; {failed} couldn't be removed"
            ))
        } else {
            Ok(removed)
        }
    }

    /// Percent-encode a path for `Path=` (RFC 3986 unreserved and '/' kept).
    pub(super) fn encode_path(p: &Path) -> String {
        let mut s = String::new();
        for &b in p.as_os_str().as_bytes() {
            if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
        s
    }

    pub(super) fn decode_path(s: &str) -> PathBuf {
        let b = s.as_bytes();
        let mut out = Vec::with_capacity(b.len());
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%' && i + 2 < b.len() {
                let hex = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = hex {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        PathBuf::from(OsString::from_vec(out))
    }
}

/// macOS / Windows: the system trash through the `trash` crate. Windows can
/// list, restore and empty it (`os_limited`); macOS can only move into it.
#[cfg(not(target_os = "linux"))]
mod os_trash {
    use super::*;

    /// macOS: through NSFileManager (the crate's default asks Finder over
    /// Apple Events, which prompts for permission and is blocked in the App
    /// Store sandbox).
    fn delete(path: &Path) -> Result<(), ::trash::Error> {
        #[cfg(target_os = "macos")]
        {
            use ::trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut ctx = ::trash::TrashContext::default();
            ctx.set_delete_method(DeleteMethod::NsFileManager);
            ctx.delete(path)
        }
        #[cfg(not(target_os = "macos"))]
        ::trash::delete(path)
    }

    /// Trash one path right now (used by `start` for JobKind::Trash).
    pub fn trash(path: &Path) -> Result<TrashEntry, String> {
        let meta = fs::symlink_metadata(path).map_err(|e| io_msg(&e))?;
        let abs = absolute_nofollow(path).ok_or("Can't move this item to the Trash")?;
        let size = if meta.is_dir() {
            scan(&abs, &AtomicBool::new(false)).1
        } else {
            meta.len()
        };
        if cfg!(test) {
            return Err("test build: the system trash is never touched".into());
        }
        delete(&abs).map_err(|e| format!("Can't move this item to the Trash: {e}"))?;
        #[cfg(windows)]
        if let Ok(items) = ::trash::os_limited::list() {
            if let Some(it) = items
                .into_iter()
                .filter(|i| i.original_parent.join(&i.name) == abs)
                .max_by_key(|i| i.time_deleted)
            {
                return Ok(entry_of(&it, size));
            }
        }
        Ok(TrashEntry {
            trashed: PathBuf::new(),
            info: PathBuf::new(),
            original: abs,
            deleted_at: format_civil(now_local()),
            size,
        })
    }

    #[cfg(windows)]
    fn entry_of(it: &::trash::TrashItem, size: u64) -> TrashEntry {
        let id = PathBuf::from(&it.id);
        let t = it.time_deleted;
        TrashEntry {
            trashed: id.clone(),
            info: id,
            original: it.original_parent.join(&it.name),
            deleted_at: format_civil(t + local_offset(t)),
            size,
        }
    }

    #[cfg(windows)]
    fn find(id: &Path) -> Result<::trash::TrashItem, String> {
        ::trash::os_limited::list()
            .map_err(|e| format!("Can't read the Trash: {e}"))?
            .into_iter()
            .find(|i| Path::new(&i.id) == id)
            .ok_or_else(|| "Not an item in the Trash".to_string())
    }

    /// Entries in the Recycle Bin (Windows); empty on macOS.
    pub fn list_trash() -> Result<Vec<TrashEntry>, String> {
        #[cfg(windows)]
        {
            let items =
                ::trash::os_limited::list().map_err(|e| format!("Can't read the Trash: {e}"))?;
            let mut out: Vec<TrashEntry> = items
                .iter()
                .map(|it| {
                    let size = match ::trash::os_limited::metadata(it) {
                        Ok(m) => match m.size {
                            ::trash::TrashItemSize::Bytes(b) => b,
                            ::trash::TrashItemSize::Entries(_) => 0,
                        },
                        Err(_) => 0,
                    };
                    entry_of(it, size)
                })
                .collect();
            out.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));
            Ok(out)
        }
        #[cfg(not(windows))]
        Ok(Vec::new())
    }

    /// The entry for a trashed item (Windows: by its Recycle Bin id).
    pub(super) fn entry_from_path(p: &Path) -> Result<TrashEntry, String> {
        #[cfg(windows)]
        return find(p).map(|it| entry_of(&it, 0));
        #[cfg(not(windows))]
        Err(format!(
            "{}: restore it from the Finder's Trash",
            display_name(p)
        ))
    }

    /// Restore one entry; fails if the original location is now occupied.
    pub fn restore(entry: &TrashEntry) -> Result<PathBuf, String> {
        let name = display_name(&entry.original);
        if fs::symlink_metadata(&entry.original).is_ok() {
            return Err(format!("“{name}” already exists in its original folder"));
        }
        #[cfg(windows)]
        {
            let it = find(&entry.trashed)?;
            ::trash::os_limited::restore_all([it])
                .map_err(|e| format!("Can't restore “{name}”: {e}"))?;
            Ok(entry.original.clone())
        }
        #[cfg(not(windows))]
        Err(format!("“{name}”: restore it from the Finder's Trash"))
    }

    /// Permanently delete everything in the trash; returns items removed.
    pub fn empty_trash() -> Result<usize, String> {
        #[cfg(windows)]
        {
            let items =
                ::trash::os_limited::list().map_err(|e| format!("Can't read the Trash: {e}"))?;
            let n = items.len();
            ::trash::os_limited::purge_all(items)
                .map_err(|e| format!("Can't empty the Trash: {e}"))?;
            Ok(n)
        }
        #[cfg(not(windows))]
        Err("Empty the Trash from the Finder".into())
    }
}

// ── time (no chrono/libc: UTC civil math + the system TZif file) ──

/// Local wall-clock seconds since the epoch.
fn now_local() -> i64 {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    t + local_offset(t)
}

/// UTC offset at `t` from `$TZ` / `/etc/localtime` (TZif). Uses the
/// transition table only (not the POSIX footer rule), so "slim" zone files
/// past their last transition may be off by the DST hour; 0 (UTC) on failure.
fn local_offset(t: i64) -> i64 {
    let file = match std::env::var("TZ") {
        Ok(tz) => {
            let tz = tz.trim_start_matches(':');
            if tz.is_empty() || tz == "UTC" || tz == "UTC0" {
                return 0;
            } else if tz.starts_with('/') {
                PathBuf::from(tz)
            } else if tz.split('/').any(|c| c == "..") {
                return 0;
            } else {
                Path::new("/usr/share/zoneinfo").join(tz)
            }
        }
        Err(_) => PathBuf::from("/etc/localtime"),
    };
    fs::read(file)
        .ok()
        .and_then(|d| tzif_offset(&d, t))
        .unwrap_or(0)
}

fn tzif_offset(d: &[u8], t: i64) -> Option<i64> {
    let be32 =
        |o: usize| -> Option<u32> { Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?)) };
    let counts = |h: usize| -> Option<[usize; 6]> {
        if d.get(h..h + 4)? != b"TZif" {
            return None;
        }
        let mut c = [0usize; 6];
        for (i, v) in c.iter_mut().enumerate() {
            *v = be32(h + 20 + 4 * i)? as usize;
        }
        Some(c)
    };
    let c1 = counts(0)?;
    let v1len = c1[3] * 5 + c1[4] * 6 + c1[5] + c1[2] * 8 + c1[1] + c1[0];
    let (base, tsz, c) = if *d.get(4)? >= b'2' {
        let h = 44 + v1len;
        (h + 44, 8, counts(h)?)
    } else {
        (44, 4, c1)
    };
    let (timecnt, typecnt) = (c[3], c[4]);
    let idx_off = base + timecnt * tsz;
    let types_off = idx_off + timecnt;
    let mut ti = 0usize;
    for i in 0..timecnt {
        let o = base + i * tsz;
        let tt = if tsz == 8 {
            i64::from_be_bytes(d.get(o..o + 8)?.try_into().ok()?)
        } else {
            be32(o)? as i32 as i64
        };
        if tt > t {
            break;
        }
        ti = *d.get(idx_off + i)? as usize;
    }
    if ti >= typecnt {
        return None;
    }
    let o = types_off + ti * 6;
    Some(be32(o)? as i32 as i64)
}

/// Seconds since the epoch → "YYYY-MM-DDThh:mm:ss".
fn format_civil(t: i64) -> String {
    let (days, sod) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

// ───────────────────────────── helpers ─────────────────────────────

/// A name's bytes: raw on unix, UTF-8 elsewhere (Windows names are UTF-16).
fn os_bytes(s: &OsStr) -> std::borrow::Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::borrow::Cow::Borrowed(s.as_bytes())
    }
    #[cfg(not(unix))]
    match s.to_string_lossy() {
        std::borrow::Cow::Borrowed(b) => std::borrow::Cow::Borrowed(b.as_bytes()),
        std::borrow::Cow::Owned(o) => std::borrow::Cow::Owned(o.into_bytes()),
    }
}

/// Inverse of `os_bytes`.
fn os_from_bytes(v: Vec<u8>) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(v)
    }
    #[cfg(not(unix))]
    OsString::from(String::from_utf8_lossy(&v).into_owned())
}

/// Are `a` and `b` the same file (e.g. a case-only rename target)?
fn same_file(a: &Path, am: &fs::Metadata, b: &Path, bm: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let _ = (a, b);
        am.dev() == bm.dev() && am.ino() == bm.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (am, bm);
        // canonicalize resolves to the on-disk spelling of the name
        matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
    }
}

/// New folder only we can enter while a copy fills it (unix 0700; the
/// source's permissions are applied afterwards).
fn private_dir(p: &Path) -> io::Result<()> {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut b = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(p)
}

/// New file (fails if it exists), unix mode 0600 until the copy is done.
fn private_new_file(p: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p)
}

fn exists(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok()
}

fn is_exdev(e: &io::Error) -> bool {
    e.raw_os_error() == Some(18) || e.kind() == io::ErrorKind::CrossesDevices
}

fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Absolute path without resolving the last component (keeps symlinks).
fn absolute_nofollow(p: &Path) -> Option<PathBuf> {
    let name = p.file_name()?;
    let parent = match p.parent() {
        Some(par) if !par.as_os_str().is_empty() => par,
        _ => Path::new("."),
    };
    fs::canonicalize(parent).ok().map(|c| c.join(name))
}

fn remove_any(p: &Path) -> io::Result<()> {
    if fs::symlink_metadata(p)?.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

/// Plain recursive copy (restore across devices), links kept as links.
#[cfg(target_os = "linux")]
fn copy_plain(src: &Path, dst: &Path) -> io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if m.file_type().is_symlink() {
        crate::platform::symlink(&fs::read_link(src)?, dst)
    } else if m.is_dir() {
        private_dir(dst)?;
        for e in fs::read_dir(src)? {
            let n = e?.file_name();
            copy_plain(&src.join(&n), &dst.join(&n))?;
        }
        fs::set_permissions(dst, m.permissions())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// Short user-facing text for an I/O error.
fn io_msg(e: &io::Error) -> String {
    use io::ErrorKind::*;
    match e.kind() {
        NotFound => "not found".into(),
        PermissionDenied => "permission denied".into(),
        AlreadyExists => "already exists".into(),
        StorageFull => "not enough space".into(),
        ReadOnlyFilesystem => "the drive is read-only".into(),
        DirectoryNotEmpty => "folder is not empty".into(),
        FileTooLarge => "file too large for this drive".into(),
        InvalidFilename => "invalid or too long name".into(),
        CrossesDevices => "can't do this across drives".into(),
        _ => {
            let s = e.to_string();
            match s.find(" (os error") {
                Some(i) => s[..i].to_string(),
                None => s,
            }
        }
    }
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Name can't be empty".into());
    }
    if name.contains('/') {
        return Err("Name can't contain “/”".into());
    }
    if cfg!(windows) {
        if let Some(c) = name.chars().find(|c| "\\:*?\"<>|".contains(*c)) {
            return Err(format!("Name can't contain “{c}”"));
        }
    }
    if name.contains('\0') {
        return Err("Name contains an invalid character".into());
    }
    if name == "." || name == ".." {
        return Err("That name is reserved".into());
    }
    if name.len() > 255 {
        return Err("Name is too long".into());
    }
    Ok(())
}

fn create_unique(
    dir: &Path,
    name: &str,
    make: impl Fn(&Path) -> io::Result<()>,
) -> Result<PathBuf, String> {
    validate_name(name)?;
    if !fs::metadata(dir).map(|m| m.is_dir()).unwrap_or(false) {
        return Err("Folder not found".into());
    }
    for _ in 0..8 {
        let p = unique_name(dir, name);
        match make(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue, // raced
            Err(e) => return Err(io_msg(&e)),
        }
    }
    Err("Couldn't find a free name".into())
}

/// (stem, extension incl. dot). Dotfiles without another dot have no
/// extension; common `.tar.*` double extensions stay together.
fn split_name(name: &[u8]) -> (&[u8], &[u8]) {
    let from = usize::from(name.starts_with(b"."));
    for de in [
        &b".tar.gz"[..],
        b".tar.xz",
        b".tar.bz2",
        b".tar.zst",
        b".tar.lz",
        b".tar.lzma",
    ] {
        if name.len() > de.len() + from && name.ends_with(de) {
            let i = name.len() - de.len();
            return (&name[..i], &name[i..]);
        }
    }
    match name[from..].iter().rposition(|&b| b == b'.') {
        Some(i) if from + i + 1 < name.len() => (&name[..from + i], &name[from + i..]),
        _ => (name, b""),
    }
}

/// "name (3)" → ("name", 4); anything else → (stem, 2).
fn strip_counter(stem: &[u8]) -> (&[u8], u32) {
    if let Some(inner) = stem.strip_suffix(b")") {
        if let Some(open) = inner.windows(2).rposition(|w| w == b" (") {
            let digits = &inner[open + 2..];
            if !digits.is_empty() && digits.len() < 7 && digits.iter().all(u8::is_ascii_digit) {
                let n: u32 = std::str::from_utf8(digits)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if n >= 2 && open > 0 {
                    return (&stem[..open], n + 1);
                }
            }
        }
    }
    (stem, 2)
}

fn numbered(base: &[u8], ext: &[u8], n: u32) -> OsString {
    let mut v = base.to_vec();
    v.extend_from_slice(format!(" ({n})").as_bytes());
    v.extend_from_slice(ext);
    os_from_bytes(v)
}

fn unique_in(dir: &Path, name: &OsStr) -> PathBuf {
    let first = dir.join(name);
    if !exists(&first) {
        return first;
    }
    let bytes = os_bytes(name);
    let (stem, ext) = split_name(&bytes);
    let (base, start) = strip_counter(stem);
    for n in start..start.saturating_add(100_000) {
        let p = dir.join(numbered(base, ext, n));
        if !exists(&p) {
            return p;
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    dir.join(numbered(base, ext, nanos))
}

// ───────────────────────────── tests ─────────────────────────────

// unix permissions, symlinks and the freedesktop trash: Linux behaviour
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use iced::futures::executor::block_on;
    use iced::futures::StreamExt;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;

    fn base() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").expect("HOME")).join(".cache/ogx-agent-ops-tests")
    }

    /// Private home trash for the whole test process (never the real one).
    pub(super) fn home_trash() -> PathBuf {
        base()
            .join(format!("home-trash-{}", std::process::id()))
            .join("Trash")
    }

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = base().join(format!(
                "{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(fs::canonicalize(&p).unwrap())
        }
        fn p(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, s: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }

    fn collect(rx: UnboundedReceiver<JobEvent>) -> Vec<JobEvent> {
        block_on(rx.collect::<Vec<_>>())
    }

    fn run(spec: JobSpec) -> Vec<JobEvent> {
        let (ctl, rx) = start(spec);
        let evs = block_on(rx.collect::<Vec<_>>());
        assert!(evs.iter().all(|e| match e {
            JobEvent::Started { id, .. }
            | JobEvent::Progress { id, .. }
            | JobEvent::Conflict { id, .. }
            | JobEvent::Finished { id, .. } => *id == ctl.id,
        }));
        evs
    }

    /// Serialises tests that use the (private) home trash.
    fn trash_lock() -> MutexGuard<'static, ()> {
        static L: Mutex<()> = Mutex::new(());
        L.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn finished(evs: &[JobEvent]) -> (Result<Summary, String>, Option<UndoOp>) {
        match evs.last() {
            Some(JobEvent::Finished { result, undo, .. }) => (result.clone(), undo.clone()),
            other => panic!("last event not Finished: {other:?}"),
        }
    }

    fn check_protocol(evs: &[JobEvent]) {
        assert!(
            matches!(evs.first(), Some(JobEvent::Started { .. })),
            "{evs:?}"
        );
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, JobEvent::Started { .. }))
                .count(),
            1
        );
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, JobEvent::Finished { .. }))
                .count(),
            1
        );
        let _ = finished(evs);
    }

    fn spec(kind: JobKind, sources: Vec<PathBuf>, dest: Option<PathBuf>) -> JobSpec {
        JobSpec {
            kind,
            sources,
            dest,
        }
    }

    #[test]
    fn copy_tree_with_progress() {
        let t = Tmp::new("copy");
        write(&t.p("src/a.txt"), "hello");
        write(&t.p("src/sub/b.txt"), "world!");
        fs::set_permissions(t.p("src/a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(t.p("src/a.txt"))
            .unwrap()
            .set_modified(old)
            .unwrap();
        fs::create_dir(t.p("dst")).unwrap();

        let evs = run(spec(JobKind::Copy, vec![t.p("src")], Some(t.p("dst"))));
        check_protocol(&evs);
        let JobEvent::Started {
            total_items,
            total_bytes,
            ref label,
            ..
        } = evs[0]
        else {
            unreachable!()
        };
        assert_eq!((total_items, total_bytes), (4, 11));
        assert!(label.contains("“src”"), "{label}");
        let last_prog = evs.iter().rev().find_map(|e| match e {
            JobEvent::Progress {
                done_items,
                done_bytes,
                ..
            } => Some((*done_items, *done_bytes)),
            _ => None,
        });
        assert_eq!(last_prog, Some((4, 11)));
        let (res, undo) = finished(&evs);
        let s = res.unwrap();
        assert_eq!((s.items_done, s.bytes_done, s.errors.len()), (1, 11, 0));
        assert_eq!(undo, Some(UndoOp::RemoveCopies(vec![t.p("dst/src")])));
        assert_eq!(
            fs::read_to_string(t.p("dst/src/sub/b.txt")).unwrap(),
            "world!"
        );
        let m = fs::metadata(t.p("dst/src/a.txt")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o640);
        assert_eq!(m.modified().unwrap(), old);
    }

    #[test]
    fn copy_onto_itself_keeps_both() {
        let t = Tmp::new("dup");
        write(&t.p("a.txt"), "x");
        let evs = run(spec(JobKind::Copy, vec![t.p("a.txt")], Some(t.0.clone())));
        assert!(finished(&evs).0.unwrap().errors.is_empty());
        assert_eq!(fs::read_to_string(t.p("a (2).txt")).unwrap(), "x");
    }

    fn conflict_setup(t: &Tmp) {
        write(&t.p("src/a.txt"), "new-a");
        write(&t.p("src/b.txt"), "new-b");
        write(&t.p("dst/a.txt"), "old-a");
        write(&t.p("dst/b.txt"), "old-b");
    }

    /// Run a copy answering every conflict with `r`; returns events.
    fn copy_answering(t: &Tmp, r: Resolution, all: bool) -> Vec<JobEvent> {
        let (ctl, mut rx) = start_with(
            spec(
                JobKind::Copy,
                vec![t.p("src/a.txt"), t.p("src/b.txt")],
                Some(t.p("dst")),
            ),
            Opts::default(),
        );
        let mut evs = Vec::new();
        while let Some(ev) = block_on(rx.next()) {
            if matches!(ev, JobEvent::Conflict { .. }) {
                ctl.resolve(r, all);
            }
            evs.push(ev);
        }
        evs
    }

    fn conflicts(evs: &[JobEvent]) -> usize {
        evs.iter()
            .filter(|e| matches!(e, JobEvent::Conflict { .. }))
            .count()
    }

    #[test]
    fn conflict_skip_all() {
        let t = Tmp::new("skip");
        conflict_setup(&t);
        let evs = copy_answering(&t, Resolution::Skip, true);
        check_protocol(&evs);
        assert_eq!(conflicts(&evs), 1);
        let (res, undo) = finished(&evs);
        assert_eq!(res.unwrap().items_skipped, 2);
        assert_eq!(undo, None);
        assert_eq!(fs::read_to_string(t.p("dst/a.txt")).unwrap(), "old-a");
    }

    #[test]
    fn conflict_replace_each() {
        let t = Tmp::new("replace");
        conflict_setup(&t);
        let evs = copy_answering(&t, Resolution::Replace, false);
        assert_eq!(conflicts(&evs), 2);
        assert_eq!(finished(&evs).0.unwrap().items_done, 2);
        assert_eq!(fs::read_to_string(t.p("dst/a.txt")).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(t.p("dst/b.txt")).unwrap(), "new-b");
    }

    #[test]
    fn conflict_keep_both_all() {
        let t = Tmp::new("keepboth");
        conflict_setup(&t);
        let evs = copy_answering(&t, Resolution::KeepBoth, true);
        assert_eq!(conflicts(&evs), 1);
        assert_eq!(fs::read_to_string(t.p("dst/a (2).txt")).unwrap(), "new-a");
        assert_eq!(fs::read_to_string(t.p("dst/b (2).txt")).unwrap(), "new-b");
        assert_eq!(fs::read_to_string(t.p("dst/a.txt")).unwrap(), "old-a");
    }

    #[test]
    fn conflict_cancel_and_dropped_control() {
        let t = Tmp::new("ccancel");
        conflict_setup(&t);
        let evs = copy_answering(&t, Resolution::Cancel, false);
        assert_eq!(finished(&evs).0, Err(CANCELLED.into()));
        // dropping the control resolves a pending conflict as Cancel
        let (ctl, rx) = start_with(
            spec(JobKind::Copy, vec![t.p("src/a.txt")], Some(t.p("dst"))),
            Opts::default(),
        );
        drop(ctl);
        let evs = collect(rx);
        assert_eq!(finished(&evs).0, Err(CANCELLED.into()));
    }

    #[test]
    fn cancel_mid_copy_removes_partial() {
        let t = Tmp::new("cancel");
        fs::write(t.p("big.bin"), vec![7u8; 8 << 20]).unwrap();
        fs::create_dir(t.p("dst")).unwrap();
        let opts = Opts {
            chunk_delay: Some(Duration::from_millis(30)),
            ..Opts::default()
        };
        let (ctl, mut rx) = start_with(
            spec(JobKind::Copy, vec![t.p("big.bin")], Some(t.p("dst"))),
            opts,
        );
        let mut evs = Vec::new();
        while let Some(ev) = block_on(rx.next()) {
            if matches!(ev, JobEvent::Progress { done_bytes, .. } if done_bytes > 0) {
                ctl.cancel();
            }
            evs.push(ev);
        }
        check_protocol(&evs);
        assert_eq!(finished(&evs), (Err(CANCELLED.into()), None));
        assert!(!exists(&t.p("dst/big.bin")));
    }

    #[test]
    fn copy_and_move_into_self_refused() {
        let t = Tmp::new("self");
        write(&t.p("d/sub/x"), "x");
        for kind in [JobKind::Copy, JobKind::Move] {
            let evs = run(spec(kind, vec![t.p("d")], Some(t.p("d/sub"))));
            let s = finished(&evs).0.unwrap();
            assert_eq!(s.items_done, 0);
            assert!(s.errors[0].contains("into itself"), "{:?}", s.errors);
            assert!(!exists(&t.p("d/sub/d")));
        }
        assert!(exists(&t.p("d/sub/x")));
    }

    #[test]
    fn move_same_fs_and_undo() {
        let t = Tmp::new("move");
        write(&t.p("a/f.txt"), "f");
        write(&t.p("b/g.txt"), "g");
        fs::create_dir(t.p("dst")).unwrap();
        let evs = run(spec(
            JobKind::Move,
            vec![t.p("a/f.txt"), t.p("b/g.txt")],
            Some(t.p("dst")),
        ));
        check_protocol(&evs);
        let (res, undo) = finished(&evs);
        assert_eq!(res.unwrap().items_done, 2);
        assert!(exists(&t.p("dst/f.txt")) && !exists(&t.p("a/f.txt")));
        let undo = undo.unwrap();
        assert_eq!(
            undo,
            UndoOp::MoveBack(vec![
                (t.p("dst/f.txt"), t.p("a/f.txt")),
                (t.p("dst/g.txt"), t.p("b/g.txt"))
            ])
        );
        // originals in different folders; also one parent was removed meanwhile
        fs::remove_dir(t.p("b")).unwrap();
        let back = undo_spec(&undo);
        assert_eq!((back.kind, back.dest.clone()), (JobKind::Move, None));
        let evs = run(back);
        assert!(finished(&evs).0.unwrap().errors.is_empty());
        assert_eq!(fs::read_to_string(t.p("a/f.txt")).unwrap(), "f");
        assert_eq!(fs::read_to_string(t.p("b/g.txt")).unwrap(), "g");
        assert!(!exists(&t.p("dst/f.txt")));
    }

    #[test]
    fn move_cross_fs_simulated() {
        let t = Tmp::new("xmove");
        write(&t.p("src/d/one"), "1");
        write(&t.p("src/d/deep/two"), "22");
        std::os::unix::fs::symlink("one", t.p("src/d/link")).unwrap();
        fs::create_dir(t.p("dst")).unwrap();
        let opts = Opts {
            force_copy: true,
            ..Opts::default()
        };
        let (_c, rx) = start_with(
            spec(JobKind::Move, vec![t.p("src/d")], Some(t.p("dst"))),
            opts,
        );
        let evs = collect(rx);
        let (res, undo) = finished(&evs);
        let s = res.unwrap();
        assert_eq!((s.items_done, s.bytes_done), (1, 3));
        assert!(!exists(&t.p("src/d")));
        assert_eq!(fs::read_to_string(t.p("dst/d/deep/two")).unwrap(), "22");
        assert_eq!(fs::read_link(t.p("dst/d/link")).unwrap(), Path::new("one"));
        assert_eq!(
            undo,
            Some(UndoOp::MoveBack(vec![(t.p("dst/d"), t.p("src/d"))]))
        );
    }

    #[test]
    fn symlink_copied_as_link() {
        let t = Tmp::new("link");
        std::os::unix::fs::symlink("/nonexistent/target", t.p("l")).unwrap();
        fs::create_dir(t.p("dst")).unwrap();
        let evs = run(spec(JobKind::Copy, vec![t.p("l")], Some(t.p("dst"))));
        assert_eq!(finished(&evs).0.unwrap().items_done, 1);
        let m = fs::symlink_metadata(t.p("dst/l")).unwrap();
        assert!(m.file_type().is_symlink());
        assert_eq!(
            fs::read_link(t.p("dst/l")).unwrap(),
            Path::new("/nonexistent/target")
        );
    }

    #[test]
    fn delete_job() {
        let t = Tmp::new("delete");
        write(&t.p("d/x/y"), "y");
        write(&t.p("f"), "f");
        let evs = run(spec(
            JobKind::Delete,
            vec![t.p("d"), t.p("f"), t.p("missing")],
            None,
        ));
        let (res, undo) = finished(&evs);
        let s = res.unwrap();
        assert_eq!((s.items_done, s.errors.len()), (2, 1));
        assert_eq!(undo, None);
        assert!(!exists(&t.p("d")) && !exists(&t.p("f")));
    }

    #[test]
    fn trash_list_restore_round_trip() {
        let _g = trash_lock();
        let t = Tmp::new("trash");
        write(&t.p("my file %.txt"), "data");
        let e = trash(&t.p("my file %.txt")).unwrap();
        assert!(e.trashed.starts_with(home_trash()));
        assert!(!exists(&t.p("my file %.txt")));
        assert_eq!(e.size, 4);
        let info = fs::read_to_string(&e.info).unwrap();
        assert!(info.starts_with("[Trash Info]\nPath=/"), "{info}");
        assert!(info.contains("my%20file%20%25.txt"), "{info}");
        assert!(e.deleted_at.len() == 19 && e.deleted_at.as_bytes()[10] == b'T');
        let listed = list_trash().unwrap();
        assert!(listed.contains(&e));
        // resolve from either path
        assert_eq!(entry_from_path(&e.trashed).unwrap(), e);
        // same name trashed again gets a suffix
        write(&t.p("my file %.txt"), "data2");
        let e2 = trash(&t.p("my file %.txt")).unwrap();
        assert_ne!(e2.trashed, e.trashed);
        // restore refuses while occupied
        write(&t.p("my file %.txt"), "blocker");
        assert!(restore(&e).is_err());
        fs::remove_file(t.p("my file %.txt")).unwrap();
        assert_eq!(restore(&e).unwrap(), t.p("my file %.txt"));
        assert_eq!(fs::read_to_string(t.p("my file %.txt")).unwrap(), "data");
        assert!(!exists(&e.info) && !exists(&e.trashed));
        // restore recreates parents
        let nested = t.p("gone/dir/n.txt");
        write(&nested, "n");
        let en = trash(&nested).unwrap();
        fs::remove_dir_all(t.p("gone")).unwrap();
        assert_eq!(restore(&en).unwrap(), nested);
        let _ = remove_any(&e2.trashed);
        let _ = fs::remove_file(&e2.info);
    }

    #[test]
    fn trash_job_and_undo() {
        let _g = trash_lock();
        let t = Tmp::new("trashjob");
        write(&t.p("a"), "a");
        write(&t.p("d/b"), "bb");
        let evs = run(spec(JobKind::Trash, vec![t.p("a"), t.p("d")], None));
        check_protocol(&evs);
        let (res, undo) = finished(&evs);
        assert_eq!(res.unwrap().items_done, 2);
        let undo = undo.unwrap();
        let UndoOp::Untrash(ref entries) = undo else {
            panic!()
        };
        assert_eq!(entries.len(), 2);
        assert!(!exists(&t.p("a")));
        let back = undo_spec(&undo);
        assert_eq!(back.kind, JobKind::Restore);
        let evs = run(back);
        let (res, undo2) = finished(&evs);
        assert_eq!(res.unwrap().items_done, 2);
        assert_eq!(fs::read_to_string(t.p("d/b")).unwrap(), "bb");
        // undoing the restore trashes again
        assert_eq!(undo2, Some(UndoOp::RemoveCopies(vec![t.p("a"), t.p("d")])));
        let evs = run(undo_spec(&undo2.unwrap()));
        let (_, undo3) = finished(&evs);
        assert!(!exists(&t.p("a")));
        let _ = run(undo_spec(&undo3.unwrap())); // put back, leave trash clean
        assert!(exists(&t.p("a")));
    }

    #[test]
    fn undo_copy_trashes_copies() {
        let _g = trash_lock();
        let t = Tmp::new("undocopy");
        write(&t.p("src/f"), "f");
        fs::create_dir(t.p("dst")).unwrap();
        let (_, undo) = finished(&run(spec(
            JobKind::Copy,
            vec![t.p("src/f")],
            Some(t.p("dst")),
        )));
        let back = undo_spec(&undo.unwrap());
        assert_eq!(back, spec(JobKind::Trash, vec![t.p("dst/f")], None));
        let (res, undo) = finished(&run(back));
        assert_eq!(res.unwrap().items_done, 1);
        assert!(!exists(&t.p("dst/f")) && exists(&t.p("src/f")));
        let UndoOp::Untrash(e) = undo.unwrap() else {
            panic!()
        };
        let _ = remove_any(&e[0].trashed);
        let _ = fs::remove_file(&e[0].info);
    }

    #[test]
    fn empty_home_trash() {
        let _g = trash_lock();
        let t = Tmp::new("empty");
        write(&t.p("e1"), "1");
        write(&t.p("e2/x"), "2");
        trash(&t.p("e1")).unwrap();
        trash(&t.p("e2")).unwrap();
        assert!(empty_trash().unwrap() >= 2);
        assert!(list_trash().unwrap().is_empty());
        assert!(fs::read_dir(home_trash().join("info"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn volume_trash_dir_logic() {
        let t = Tmp::new("vol");
        let uid = current_uid();
        // no .Trash → .Trash-$uid, created 0700
        let d = volume_trash_dir(&t.0, uid, true).unwrap();
        assert_eq!(d, t.p(&format!(".Trash-{uid}")));
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(&d).unwrap();
        assert!(volume_trash_dir(&t.0, uid, false).is_err());
        // .Trash without sticky bit is ignored
        fs::create_dir(t.p(".Trash")).unwrap();
        assert_eq!(
            volume_trash_dir(&t.0, uid, true).unwrap(),
            t.p(&format!(".Trash-{uid}"))
        );
        fs::remove_dir(t.p(&format!(".Trash-{uid}"))).unwrap();
        // sticky .Trash → .Trash/$uid
        fs::set_permissions(t.p(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        let d = volume_trash_dir(&t.0, uid, true).unwrap();
        assert_eq!(d, t.p(&format!(".Trash/{uid}")));
        assert_eq!(topdir_of(&d), t.0);
        assert_eq!(topdir_of(&t.p(".Trash-5")), t.0);
        // symlinked .Trash is ignored
        fs::remove_dir_all(t.p(".Trash")).unwrap();
        fs::create_dir(t.p("real")).unwrap();
        fs::set_permissions(t.p("real"), fs::Permissions::from_mode(0o1777)).unwrap();
        std::os::unix::fs::symlink("real", t.p(".Trash")).unwrap();
        assert_eq!(
            volume_trash_dir(&t.0, uid, true).unwrap(),
            t.p(&format!(".Trash-{uid}"))
        );
        // trashing into a volume trash records a relative Path and resolves back
        let root = t.p(&format!(".Trash-{uid}"));
        write(&t.p("media/pic one.jpg"), "jpg");
        let src = t.p("media/pic one.jpg");
        let meta = fs::symlink_metadata(&src).unwrap();
        let e = trash_into(&src, &meta, &root, Some(&t.0)).unwrap();
        let info = fs::read_to_string(&e.info).unwrap();
        assert!(info.contains("\nPath=media/pic%20one.jpg\n"), "{info}");
        assert_eq!(entry_from_path(&e.info).unwrap().original, src);
        assert_eq!(list_root(&root), vec![e.clone()]);
        assert_eq!(restore(&e).unwrap(), src);
        // empty a fake trash
        for n in ["x", "y"] {
            write(&t.p(n), n);
            let m = fs::symlink_metadata(t.p(n)).unwrap();
            trash_into(&t.p(n), &m, &root, Some(&t.0)).unwrap();
        }
        assert_eq!(empty_roots(&[root.clone()]), Ok(2));
        assert!(list_root(&root).is_empty());
    }

    #[test]
    fn trashinfo_encoding_round_trip() {
        let raw = OsString::from_vec(b"/tmp/a b%c/\xff\xfe/\xc3\xa9t\xc3\xa9 [1].txt".to_vec());
        let p = PathBuf::from(raw);
        let enc = encode_path(&p);
        assert!(enc.is_ascii() && !enc.contains(' '));
        assert!(enc.starts_with("/tmp/a%20b%25c/%FF%FE/"), "{enc}");
        assert_eq!(decode_path(&enc), p);
        assert_eq!(decode_path("bad%zz%4"), PathBuf::from("bad%zz%4"));
        // parse_info rejects escapes outside the volume
        let t = Tmp::new("info");
        let root = t.p(".Trash-1");
        write(
            &root.join("info/x.trashinfo"),
            "[Trash Info]\nPath=../../etc/passwd\nDeletionDate=2020-01-01T00:00:00\n",
        );
        assert!(parse_info(&root.join("info/x.trashinfo"), &root).is_err());
    }

    #[test]
    fn civil_dates() {
        assert_eq!(format_civil(0), "1970-01-01T00:00:00");
        assert_eq!(format_civil(951_782_400), "2000-02-29T00:00:00");
        assert_eq!(format_civil(1_700_000_000), "2023-11-14T22:13:20");
        assert_eq!(format_civil(-1), "1969-12-31T23:59:59");
        assert_eq!(tzif_offset(b"garbage", 0), None);
        assert_eq!(tzif_offset(b"TZif2\0\0\0", 0), None);
        let off = local_offset(1_700_000_000);
        assert!(off.abs() <= 14 * 3600);
    }

    #[test]
    fn unique_name_cases() {
        let t = Tmp::new("uniq");
        assert_eq!(unique_name(&t.0, "a.txt"), t.p("a.txt"));
        write(&t.p("a.txt"), "");
        assert_eq!(unique_name(&t.0, "a.txt"), t.p("a (2).txt"));
        write(&t.p("a (2).txt"), "");
        assert_eq!(unique_name(&t.0, "a.txt"), t.p("a (3).txt"));
        assert_eq!(unique_name(&t.0, "a (2).txt"), t.p("a (3).txt"));
        write(&t.p("noext"), "");
        assert_eq!(unique_name(&t.0, "noext"), t.p("noext (2)"));
        write(&t.p(".bashrc"), "");
        assert_eq!(unique_name(&t.0, ".bashrc"), t.p(".bashrc (2)"));
        write(&t.p(".config.json"), "");
        assert_eq!(unique_name(&t.0, ".config.json"), t.p(".config (2).json"));
        write(&t.p("x.tar.gz"), "");
        assert_eq!(unique_name(&t.0, "x.tar.gz"), t.p("x (2).tar.gz"));
        write(&t.p("dot."), "");
        assert_eq!(unique_name(&t.0, "dot."), t.p("dot. (2)"));
        write(&t.p("(2)"), "");
        assert_eq!(unique_name(&t.0, "(2)"), t.p("(2) (2)"));
        std::os::unix::fs::symlink("/nonexistent", t.p("dangling")).unwrap();
        assert_eq!(unique_name(&t.0, "dangling"), t.p("dangling (2)"));
    }

    #[test]
    fn new_folder_and_file() {
        let t = Tmp::new("new");
        assert_eq!(new_folder(&t.0, "New Folder").unwrap(), t.p("New Folder"));
        assert_eq!(
            new_folder(&t.0, "New Folder").unwrap(),
            t.p("New Folder (2)")
        );
        assert_eq!(new_file_with(&t.0, "n.txt", "").unwrap(), t.p("n.txt"));
        assert_eq!(new_file_with(&t.0, "n.txt", "").unwrap(), t.p("n (2).txt"));
        assert!(t.p("New Folder (2)").is_dir() && t.p("n (2).txt").is_file());
        assert!(new_folder(&t.0, "a/b").is_err());
        assert!(new_file_with(&t.p("missing"), "x", "").is_err());
        let md = new_file_with(&t.0, "a.md", "# a\n\n").unwrap();
        assert_eq!(std::fs::read_to_string(md).unwrap(), "# a\n\n");
    }

    #[test]
    fn rename_validation() {
        let t = Tmp::new("rename");
        write(&t.p("a"), "a");
        write(&t.p("b"), "b");
        for bad in ["", "  ", "x/y", ".", ".."] {
            assert!(rename(&t.p("a"), bad).is_err(), "{bad:?}");
        }
        assert_eq!(rename(&t.p("a"), "b"), Err("“b” already exists".into()));
        assert_eq!(rename(&t.p("a"), "a").unwrap(), t.p("a"));
        assert_eq!(rename(&t.p("a"), "c.txt").unwrap(), t.p("c.txt"));
        assert_eq!(fs::read_to_string(t.p("c.txt")).unwrap(), "a");
        assert!(rename(&t.p("a"), "z").is_err()); // gone
        assert!(rename(Path::new("/"), "z").is_err());
    }
}
