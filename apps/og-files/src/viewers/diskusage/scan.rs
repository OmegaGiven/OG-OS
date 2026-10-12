//! Background scanner: a parallel directory walk (one coordinator + N
//! worker threads sharing a FIFO work queue) that streams new nodes in
//! batches. Workers only `read_dir` + `lstat`; the coordinator assigns
//! node ids in the order it emits them, so the receiving `Tree` can append
//! batches without any id translation beyond a per-job index map.
//!
//! Rules: never follows symlinks (`DirEntry::metadata` is an lstat), stays
//! on the root's filesystem (other mounts become size-less `OtherFs`
//! leaves), counts hard-linked files once (dev+ino), and records both the
//! allocated (`st_blocks * 512`) and the apparent size.

use crate::filetype::Kind;
use std::collections::{HashSet, VecDeque};
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// "No parent": the job's root node (attached by the receiver).
pub const NONE: u32 = u32::MAX;
/// How often the coordinator flushes a batch to the UI (~5×/s).
const TICK: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Dir,
    File(Kind),
    /// mount point of another filesystem: shown greyed, never entered
    OtherFs,
}

/// One stat'ed directory entry.
#[derive(Debug, Clone)]
pub struct RawEntry {
    pub name: Box<str>,
    pub kind: NodeKind,
    /// apparent size (st_size); 0 for directories
    pub size: u64,
    /// allocated size (st_blocks * 512); 0 for directories
    pub alloc: u64,
    pub mtime: i64,
    dev: u64,
    ino: u64,
    nlink: u64,
}

impl RawEntry {
    pub fn from_meta(name: Box<str>, md: &Metadata, root_dev: u64) -> Self {
        let ft = md.file_type();
        let kind = if ft.is_dir() {
            if md.dev() == root_dev {
                NodeKind::Dir
            } else {
                NodeKind::OtherFs
            }
        } else {
            NodeKind::File(Kind::of(Path::new(&*name), false))
        };
        let file = matches!(kind, NodeKind::File(_));
        Self {
            kind,
            size: if file { md.size() } else { 0 },
            alloc: if file { md.blocks() * 512 } else { 0 },
            mtime: md.mtime(),
            dev: md.dev(),
            ino: md.ino(),
            nlink: md.nlink(),
            name,
        }
    }

    /// (dev, ino) of a file with more than one name.
    pub fn link(&self) -> Option<(u64, u64)> {
        (matches!(self.kind, NodeKind::File(_)) && self.nlink > 1).then_some((self.dev, self.ino))
    }

    /// Second+ appearance of a hard-linked file?
    fn is_dup_link(&self, seen: &mut HashSet<(u64, u64)>) -> bool {
        self.link().is_some_and(|l| !seen.insert(l))
    }
}

/// A node as the receiver should append it.
#[derive(Debug, Clone)]
pub struct NewNode {
    /// index of the parent within this job's emitted nodes, or NONE
    pub parent: u32,
    pub name: Box<str>,
    pub kind: NodeKind,
    pub size: u64,
    pub alloc: u64,
    pub mtime: i64,
    /// hard-linked file identity, for tree-wide dedup
    pub link: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub items: u64,
    /// allocated bytes seen so far
    pub bytes: u64,
    pub current: String,
}

#[derive(Debug, Clone, Default)]
pub struct Batch {
    pub nodes: Vec<NewNode>,
    /// job-local ids of directories that couldn't be read
    pub unreadable: Vec<u32>,
    pub progress: Progress,
}

#[derive(Debug, Clone)]
pub struct ScanEnd {
    pub cancelled: bool,
    pub root_dev: u64,
    pub elapsed: Duration,
    /// (free, total) bytes of the root's filesystem
    pub space: Option<(u64, u64)>,
}

struct Listing {
    id: u32,
    path: PathBuf,
    entries: Vec<RawEntry>,
    ok: bool,
}

#[derive(Default)]
struct Queue {
    items: VecDeque<(u32, PathBuf)>,
    stop: bool,
}

/// Blocking scan of `root` (its node is named `root_name`). `emit` gets a
/// batch every ~200 ms and once at the end; returning false (receiver gone)
/// cancels the scan, as does setting `cancel`.
pub fn scan(
    root: &Path,
    root_name: Box<str>,
    root_dev: Option<u64>,
    cancel: &AtomicBool,
    mut emit: impl FnMut(Batch) -> bool,
) -> Result<ScanEnd, String> {
    let t0 = Instant::now();
    let md = std::fs::symlink_metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !md.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let dev = root_dev.unwrap_or(md.dev());
    let mut batch = Batch::default();
    batch.nodes.push(NewNode {
        parent: NONE,
        name: root_name,
        kind: NodeKind::Dir,
        size: 0,
        alloc: 0,
        mtime: md.mtime(),
        link: None,
    });
    let mut next_id: u32 = 1;
    let mut seen = HashSet::new();
    let mut progress = Progress::default();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let queue = (Mutex::new(Queue::default()), Condvar::new());
    let (rtx, rrx) = mpsc::channel::<Listing>();

    std::thread::scope(|s| {
        for _ in 0..threads {
            let rtx = rtx.clone();
            let queue = &queue;
            s.spawn(move || worker(queue, rtx, dev, cancel));
        }
        drop(rtx);
        let push = |items: Vec<(u32, PathBuf)>| {
            if items.is_empty() {
                return;
            }
            let mut q = queue.0.lock().unwrap_or_else(|e| e.into_inner());
            q.items.extend(items);
            queue.1.notify_all();
        };
        push(vec![(0, root.to_path_buf())]);
        let mut outstanding = 1usize;
        let mut last = Instant::now();
        while outstanding > 0 && !cancel.load(Ordering::Relaxed) {
            match rrx.recv_timeout(Duration::from_millis(50)) {
                Ok(l) => {
                    outstanding -= 1;
                    let mut dirs = Vec::new();
                    if !l.ok {
                        batch.unreadable.push(l.id);
                    }
                    for e in l.entries {
                        progress.items += 1;
                        if e.is_dup_link(&mut seen) {
                            continue;
                        }
                        let id = next_id;
                        next_id += 1;
                        if e.kind == NodeKind::Dir {
                            dirs.push((id, l.path.join(&*e.name)));
                        }
                        progress.bytes += e.alloc;
                        batch.nodes.push(NewNode {
                            link: e.link(),
                            parent: l.id,
                            name: e.name,
                            kind: e.kind,
                            size: e.size,
                            alloc: e.alloc,
                            mtime: e.mtime,
                        });
                    }
                    outstanding += dirs.len();
                    push(dirs);
                    progress.current = l.path.display().to_string();
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if outstanding > 0 && last.elapsed() >= TICK {
                last = Instant::now();
                batch.progress = progress.clone();
                if !emit(std::mem::take(&mut batch)) {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        let mut q = queue.0.lock().unwrap_or_else(|e| e.into_inner());
        q.stop = true;
        q.items.clear();
        queue.1.notify_all();
    });

    let cancelled = cancel.load(Ordering::Relaxed);
    if !cancelled {
        batch.progress = progress;
        emit(batch);
    }
    Ok(ScanEnd {
        cancelled,
        root_dev: dev,
        elapsed: t0.elapsed(),
        space: if root_dev.is_none() {
            fs_space(root)
        } else {
            None
        },
    })
}

fn worker(
    queue: &(Mutex<Queue>, Condvar),
    out: mpsc::Sender<Listing>,
    dev: u64,
    cancel: &AtomicBool,
) {
    loop {
        let (id, path) = {
            let mut q = queue.0.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if q.stop || cancel.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(job) = q.items.pop_front() {
                    break job;
                }
                // timed wait so a cancel without a notify is still noticed
                q = queue
                    .1
                    .wait_timeout(q, Duration::from_millis(50))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        };
        let (entries, ok) = list_dir(&path, dev, cancel);
        if out
            .send(Listing {
                id,
                path,
                entries,
                ok,
            })
            .is_err()
        {
            return;
        }
    }
}

/// lstat every entry of `dir`. `ok` = false when the directory itself
/// couldn't be opened (permissions etc.).
fn list_dir(dir: &Path, dev: u64, cancel: &AtomicBool) -> (Vec<RawEntry>, bool) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return (Vec::new(), false);
    };
    let mut out = Vec::new();
    for (i, e) in rd.flatten().enumerate() {
        if i % 512 == 511 && cancel.load(Ordering::Relaxed) {
            break;
        }
        // DirEntry::metadata doesn't traverse symlinks (fstatat NOFOLLOW)
        let Ok(md) = e.metadata() else { continue };
        let name = e
            .file_name()
            .to_string_lossy()
            .into_owned()
            .into_boxed_str();
        out.push(RawEntry::from_meta(name, &md, dev));
    }
    (out, true)
}

/// What `fs_changed` found for one path (computed off the UI thread).
#[derive(Debug, Clone)]
pub enum Probe {
    Gone(PathBuf),
    File(PathBuf, RawEntry),
    /// a directory and its current direct entries (shallow)
    Dir(PathBuf, RawEntry, Vec<RawEntry>, bool),
}

pub fn probe(paths: &[PathBuf], root_dev: u64) -> Vec<Probe> {
    let never = AtomicBool::new(false);
    paths
        .iter()
        .map(|p| match std::fs::symlink_metadata(p) {
            Err(_) => Probe::Gone(p.clone()),
            Ok(md) => {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into_boxed_str();
                let e = RawEntry::from_meta(name, &md, root_dev);
                if e.kind == NodeKind::Dir {
                    let (entries, ok) = list_dir(p, root_dev, &never);
                    Probe::Dir(p.clone(), e, entries, ok)
                } else {
                    Probe::File(p.clone(), e)
                }
            }
        })
        .collect()
}

/// (free for unprivileged users, total) bytes of the filesystem holding `p`.
pub fn fs_space(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the zeroed struct we pass; `c` is a
    // valid NUL-terminated path for the duration of the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let f = st.f_frsize as u64;
    Some((st.f_bavail as u64 * f, st.f_blocks as u64 * f))
}
