//! Background file operations with progress, cancellation and conflict
//! prompts — copies of a few GB must never freeze the window, and an
//! existing file must never be silently overwritten.
//!
//! A job runs on its own thread and reports through a channel the app turns
//! into messages. When the worker hits a destination that already exists it
//! parks on a reply channel until the user picks a resolution in the
//! conflict dialog; "apply to all" is remembered for the rest of that job.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind { Copy, Move, Trash, Delete, Restore }

impl JobKind {
    pub fn verb(self) -> &'static str {
        match self {
            JobKind::Copy => "Copying",
            JobKind::Move => "Moving",
            JobKind::Trash => "Moving to trash",
            JobKind::Delete => "Deleting",
            JobKind::Restore => "Restoring",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution { Overwrite, Skip, KeepBoth, Cancel }

#[derive(Debug, Clone)]
pub enum JobEvent {
    Progress {
        id: u64,
        current: String,
        done_bytes: u64,
        total_bytes: u64,
        done_items: u64,
        total_items: u64,
    },
    /// Worker is blocked waiting for `reply_conflict`.
    Conflict { id: u64, src: PathBuf, dst: PathBuf, dst_is_dir: bool },
    Finished { id: u64, kind: JobKind, undo: Option<UndoOp>, error: Option<String>, cancelled: bool, touched_dirs: Vec<PathBuf> },
}

/// What it takes to reverse a finished job (Ctrl+Z).
#[derive(Debug, Clone)]
pub enum UndoOp {
    /// Files that were created by a copy — undo deletes them.
    Copied(Vec<PathBuf>),
    /// (from, to) pairs — undo moves them back.
    Moved(Vec<(PathBuf, PathBuf)>),
    /// (original, trashed) pairs — undo restores from trash.
    Trashed(Vec<(PathBuf, PathBuf)>),
    Renamed { from: PathBuf, to: PathBuf },
    Created(PathBuf),
}

impl UndoOp {
    pub fn describe(&self) -> String {
        match self {
            UndoOp::Copied(v) => format!("Undo copy of {} item(s)", v.len()),
            UndoOp::Moved(v) => format!("Undo move of {} item(s)", v.len()),
            UndoOp::Trashed(v) => format!("Undo trashing {} item(s)", v.len()),
            UndoOp::Renamed { to, .. } => format!("Undo rename of {}", to.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
            UndoOp::Created(p) => format!("Undo creating {}", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct JobHandle {
    pub id: u64,
    pub kind: JobKind,
    pub cancel: Arc<AtomicBool>,
    reply: mpsc::Sender<Resolution>,
    pub apply_to_all: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        // Unblock a worker parked on a conflict so it can notice the flag.
        let _ = self.reply.send(Resolution::Cancel);
    }

    pub fn reply_conflict(&self, res: Resolution, all: bool) {
        self.apply_to_all.store(all, Ordering::Relaxed);
        let _ = self.reply.send(res);
    }
}

pub struct JobSpec {
    pub kind: JobKind,
    pub sources: Vec<PathBuf>,
    /// Destination folder for Copy/Move; ignored otherwise.
    pub dest_dir: Option<PathBuf>,
}

static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One process-wide event bus, same pattern as `watcher`/`thumbs` — the app
/// never touches a per-job channel directly, it just drains this in a
/// subscription and routes events by `id`.
///
/// Spawns the worker. Events land on the shared bus (see `next_event`).
pub fn start(spec: JobSpec) -> JobHandle {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    let apply_to_all = Arc::new(AtomicBool::new(false));
    let (reply_tx, reply_rx) = mpsc::channel();
    let handle = JobHandle { id, kind: spec.kind, cancel: cancel.clone(), reply: reply_tx, apply_to_all: apply_to_all.clone() };
    let events = bus_sender();

    std::thread::Builder::new()
        .name(format!("og-files-job-{id}"))
        .spawn(move || {
            let mut w = Worker { id, spec, events, cancel, reply_rx, apply_to_all, sticky: None, done_bytes: 0, total_bytes: 0, done_items: 0, total_items: 0, last_report: std::time::Instant::now(), touched: Vec::new() };
            w.run();
        })
        .expect("spawn job thread");
    handle
}

struct Bus {
    tx: mpsc::Sender<JobEvent>,
    rx: Mutex<mpsc::Receiver<JobEvent>>,
}

static BUS_CELL: std::sync::OnceLock<Bus> = std::sync::OnceLock::new();

fn bus() -> &'static Bus {
    BUS_CELL.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        Bus { tx, rx: Mutex::new(rx) }
    })
}

fn bus_sender() -> mpsc::Sender<JobEvent> {
    bus().tx.clone()
}

/// Blocking wait for the next job event (for the app's subscription).
pub fn next_event() -> Option<JobEvent> {
    bus().rx.lock().unwrap().recv_timeout(std::time::Duration::from_millis(500)).ok()
}

struct Worker {
    id: u64,
    spec: JobSpec,
    events: mpsc::Sender<JobEvent>,
    cancel: Arc<AtomicBool>,
    reply_rx: mpsc::Receiver<Resolution>,
    apply_to_all: Arc<AtomicBool>,
    sticky: Option<Resolution>,
    done_bytes: u64,
    total_bytes: u64,
    done_items: u64,
    total_items: u64,
    last_report: std::time::Instant,
    touched: Vec<PathBuf>,
}

struct Cancelled;

impl Worker {
    fn run(&mut self) {
        let kind = self.spec.kind;
        let result = match kind {
            JobKind::Copy => self.run_copy_move(false),
            JobKind::Move => self.run_copy_move(true),
            JobKind::Trash => self.run_trash(),
            JobKind::Delete => self.run_delete(),
            JobKind::Restore => self.run_restore(),
        };
        let (undo, error, cancelled) = match result {
            Ok(undo) => (undo, None, false),
            Err(Outcome::Cancelled) => (None, None, true),
            Err(Outcome::Failed(e)) => (None, Some(e), false),
        };
        let mut touched = std::mem::take(&mut self.touched);
        touched.sort();
        touched.dedup();
        let _ = self.events.send(JobEvent::Finished { id: self.id, kind, undo, error, cancelled, touched_dirs: touched });
    }

    fn touch(&mut self, p: &Path) {
        if let Some(parent) = p.parent() {
            self.touched.push(parent.to_path_buf());
        }
    }

    fn check_cancel(&self) -> Result<(), Cancelled> {
        if self.cancel.load(Ordering::Relaxed) { Err(Cancelled) } else { Ok(()) }
    }

    fn report(&mut self, current: &Path, force: bool) {
        if !force && self.last_report.elapsed().as_millis() < 80 {
            return;
        }
        self.last_report = std::time::Instant::now();
        let _ = self.events.send(JobEvent::Progress {
            id: self.id,
            current: current.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            done_bytes: self.done_bytes,
            total_bytes: self.total_bytes,
            done_items: self.done_items,
            total_items: self.total_items,
        });
    }

    /// Asks the UI what to do about an existing destination (or reuses the
    /// sticky "apply to all" answer).
    fn resolve(&mut self, src: &Path, dst: &Path) -> Result<Resolution, Cancelled> {
        if let Some(r) = self.sticky {
            return Ok(r);
        }
        let _ = self.events.send(JobEvent::Conflict { id: self.id, src: src.to_path_buf(), dst: dst.to_path_buf(), dst_is_dir: dst.is_dir() });
        let res = self.reply_rx.recv().unwrap_or(Resolution::Cancel);
        self.check_cancel()?;
        if res == Resolution::Cancel {
            self.cancel.store(true, Ordering::Relaxed);
            return Err(Cancelled);
        }
        if self.apply_to_all.load(Ordering::Relaxed) {
            self.sticky = Some(res);
        }
        Ok(res)
    }

    fn tally(&mut self) {
        let mut bytes = 0u64;
        let mut items = 0u64;
        for s in &self.spec.sources {
            walk_totals(s, &mut bytes, &mut items);
        }
        self.total_bytes = bytes;
        self.total_items = items;
    }

    fn run_copy_move(&mut self, mv: bool) -> Result<Option<UndoOp>, Outcome> {
        let dest_dir = self.spec.dest_dir.clone().ok_or_else(|| Outcome::Failed("no destination".into()))?;
        self.tally();
        let mut created: Vec<PathBuf> = Vec::new();
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let sources = self.spec.sources.clone();
        for src in sources {
            self.check_cancel()?;
            let Some(name) = src.file_name() else { continue };
            if dest_dir.starts_with(&src) {
                return Err(Outcome::Failed(format!("Cannot {} \"{}\" into itself", if mv { "move" } else { "copy" }, name.to_string_lossy())));
            }
            let mut dst = dest_dir.join(name);
            if dst == src {
                if mv {
                    continue;
                }
                dst = keep_both_name(&dst);
            }
            if dst.exists() && dst != src {
                match self.resolve(&src, &dst)? {
                    Resolution::Skip => { self.skip_totals(&src); continue; }
                    Resolution::KeepBoth => dst = keep_both_name(&dst),
                    Resolution::Overwrite => {
                        if dst.is_dir() && !src.is_dir() || !dst.is_dir() && src.is_dir() {
                            remove_any(&dst).map_err(|e| Outcome::Failed(e))?;
                        }
                    }
                    Resolution::Cancel => return Err(Outcome::Cancelled),
                }
            }
            self.touch(&src);
            self.touch(&dst);
            if mv {
                // Fast path: same filesystem rename.
                if std::fs::rename(&src, &dst).is_ok() {
                    moved.push((src.clone(), dst.clone()));
                    self.done_items += 1;
                    self.report(&src, true);
                    continue;
                }
            }
            self.copy_tree(&src, &dst)?;
            if mv {
                remove_any(&src).map_err(|e| Outcome::Failed(e))?;
                moved.push((src.clone(), dst.clone()));
            } else {
                created.push(dst.clone());
            }
        }
        self.report(Path::new(""), true);
        Ok(Some(if mv { UndoOp::Moved(moved) } else { UndoOp::Copied(created) }))
    }

    fn skip_totals(&mut self, src: &Path) {
        let (mut b, mut i) = (0, 0);
        walk_totals(src, &mut b, &mut i);
        self.total_bytes = self.total_bytes.saturating_sub(b);
        self.total_items = self.total_items.saturating_sub(i);
    }

    fn copy_tree(&mut self, src: &Path, dst: &Path) -> Result<(), Outcome> {
        self.check_cancel()?;
        let meta = std::fs::symlink_metadata(src).map_err(|e| Outcome::Failed(format!("{}: {e}", src.display())))?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(src).map_err(|e| Outcome::Failed(e.to_string()))?;
            let _ = std::fs::remove_file(dst);
            std::os::unix::fs::symlink(&target, dst).map_err(|e| Outcome::Failed(format!("{}: {e}", dst.display())))?;
            self.done_items += 1;
            self.report(src, false);
            return Ok(());
        }
        if meta.is_dir() {
            std::fs::create_dir_all(dst).map_err(|e| Outcome::Failed(format!("{}: {e}", dst.display())))?;
            let rd = std::fs::read_dir(src).map_err(|e| Outcome::Failed(format!("{}: {e}", src.display())))?;
            for entry in rd.flatten() {
                let child_src = entry.path();
                let child_dst = dst.join(entry.file_name());
                self.copy_tree(&child_src, &child_dst)?;
            }
            let _ = std::fs::set_permissions(dst, meta.permissions());
            self.done_items += 1;
            return Ok(());
        }
        self.copy_file(src, dst, meta.len())?;
        let _ = std::fs::set_permissions(dst, meta.permissions());
        self.done_items += 1;
        self.report(src, false);
        Ok(())
    }

    fn copy_file(&mut self, src: &Path, dst: &Path, len: u64) -> Result<(), Outcome> {
        use std::io::{Read, Write};
        let mut r = std::fs::File::open(src).map_err(|e| Outcome::Failed(format!("{}: {e}", src.display())))?;
        let mut w = std::fs::File::create(dst).map_err(|e| Outcome::Failed(format!("{}: {e}", dst.display())))?;
        // Bigger buffer than std::fs::copy's default so progress ticks are
        // cheap, still small enough that cancel is responsive.
        let mut buf = vec![0u8; 1 << 20];
        let mut copied = 0u64;
        loop {
            self.check_cancel().map_err(|c| { let _ = std::fs::remove_file(dst); c })?;
            let n = r.read(&mut buf).map_err(|e| Outcome::Failed(e.to_string()))?;
            if n == 0 {
                break;
            }
            w.write_all(&buf[..n]).map_err(|e| Outcome::Failed(format!("{}: {e}", dst.display())))?;
            copied += n as u64;
            self.done_bytes += n as u64;
            if copied % (8 << 20) < (1 << 20) {
                self.report(src, false);
            }
        }
        if copied < len {
            // Source shrank while copying; keep totals honest.
            self.done_bytes += len - copied;
        }
        Ok(())
    }

    fn run_trash(&mut self) -> Result<Option<UndoOp>, Outcome> {
        self.total_items = self.spec.sources.len() as u64;
        let mut pairs = Vec::new();
        let sources = self.spec.sources.clone();
        for src in sources {
            self.check_cancel()?;
            self.touch(&src);
            let trashed = crate::filesystem::trash_one(&src).map_err(Outcome::Failed)?;
            pairs.push((src.clone(), trashed));
            self.done_items += 1;
            self.report(&src, true);
        }
        Ok(Some(UndoOp::Trashed(pairs)))
    }

    fn run_delete(&mut self) -> Result<Option<UndoOp>, Outcome> {
        self.tally();
        let sources = self.spec.sources.clone();
        for src in sources {
            self.check_cancel()?;
            self.touch(&src);
            self.delete_tree(&src)?;
        }
        self.report(Path::new(""), true);
        Ok(None)
    }

    fn delete_tree(&mut self, p: &Path) -> Result<(), Outcome> {
        self.check_cancel()?;
        let meta = std::fs::symlink_metadata(p).map_err(|e| Outcome::Failed(format!("{}: {e}", p.display())))?;
        if meta.is_dir() && !meta.file_type().is_symlink() {
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    self.delete_tree(&e.path())?;
                }
            }
            std::fs::remove_dir(p).map_err(|e| Outcome::Failed(format!("{}: {e}", p.display())))?;
        } else {
            std::fs::remove_file(p).map_err(|e| Outcome::Failed(format!("{}: {e}", p.display())))?;
            self.done_bytes += meta.len();
        }
        self.done_items += 1;
        self.report(p, false);
        Ok(())
    }

    fn run_restore(&mut self) -> Result<Option<UndoOp>, Outcome> {
        self.total_items = self.spec.sources.len() as u64;
        let mut pairs = Vec::new();
        let sources = self.spec.sources.clone();
        for trashed in sources {
            self.check_cancel()?;
            let Some(original) = crate::filesystem::trash_origin(&trashed) else {
                return Err(Outcome::Failed(format!("No trash info for {}", trashed.display())));
            };
            if original.exists() {
                match self.resolve(&trashed, &original)? {
                    Resolution::Skip => { continue; }
                    Resolution::KeepBoth => {
                        let alt = keep_both_name(&original);
                        crate::filesystem::restore_to(&trashed, &alt).map_err(Outcome::Failed)?;
                        self.touch(&alt);
                        pairs.push((alt, trashed.clone()));
                        self.done_items += 1;
                        continue;
                    }
                    Resolution::Overwrite => { remove_any(&original).map_err(Outcome::Failed)?; }
                    Resolution::Cancel => return Err(Outcome::Cancelled),
                }
            }
            crate::filesystem::restore_to(&trashed, &original).map_err(Outcome::Failed)?;
            self.touch(&original);
            self.touch(&trashed);
            pairs.push((original, trashed.clone()));
            self.done_items += 1;
            self.report(&trashed, true);
        }
        let _ = pairs;
        // Restoring is intentionally not undoable from here (undo would mean
        // re-trashing, which needs the same conflict machinery all over
        // again) — the Trash itself is the undo path for a restore mistake.
        Ok(None)
    }
}

enum Outcome { Cancelled, Failed(String) }
impl From<Cancelled> for Outcome {
    fn from(_: Cancelled) -> Self { Outcome::Cancelled }
}

fn walk_totals(p: &Path, bytes: &mut u64, items: &mut u64) {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return };
    *items += 1;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                walk_totals(&e.path(), bytes, items);
            }
        }
    } else {
        *bytes += meta.len();
    }
}

pub fn remove_any(p: &Path) -> Result<(), String> {
    let meta = std::fs::symlink_metadata(p).map_err(|e| e.to_string())?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
    } else {
        std::fs::remove_file(p).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// "photo.jpg" → "photo (copy).jpg", then "photo (copy 2).jpg", ...
pub fn keep_both_name(p: &Path) -> PathBuf {
    let parent = p.parent().unwrap_or(Path::new("."));
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| e.to_string_lossy().to_string());
    let make = |suffix: &str| match &ext {
        Some(ext) if !p.is_dir() => parent.join(format!("{stem} ({suffix}).{ext}")),
        _ => parent.join(format!("{} ({suffix})", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())),
    };
    let first = make("copy");
    if !first.exists() {
        return first;
    }
    let mut n = 2;
    loop {
        let c = make(&format!("copy {n}"));
        if !c.exists() {
            return c;
        }
        n += 1;
    }
}
