//! Grid/list thumbnails, generated off the UI thread and cached on disk.
//!
//! The old grid decoded every image synchronously in `view()` — a folder of
//! screenshots took seconds to paint. Now the view asks `get()`; a miss
//! enqueues the file for a worker and paints the type icon until the small
//! cached PNG exists. Cache keys include mtime+size so edits refresh.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Condvar, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

const THUMB_PX: u32 = 160;
const WORKERS: usize = 2;

#[derive(Default)]
struct Queue {
    pending: VecDeque<(PathBuf, String)>,
    queued: HashSet<PathBuf>,
}

struct Pool {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// path → Some(thumb) when ready, None when generation failed.
    done: Mutex<HashMap<PathBuf, Option<PathBuf>>>,
    ready_tx: Mutex<Option<mpsc::Sender<PathBuf>>>,
}

static POOL: OnceLock<Pool> = OnceLock::new();
static READY_RX: OnceLock<Mutex<Option<mpsc::Receiver<PathBuf>>>> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        *READY_RX.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(rx);
        let p = Pool { queue: Mutex::new(Queue::default()), wake: Condvar::new(), done: Mutex::new(HashMap::new()), ready_tx: Mutex::new(Some(tx)) };
        p
    })
}

fn start_workers() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        for i in 0..WORKERS {
            std::thread::Builder::new()
                .name(format!("og-files-thumb-{i}"))
                .spawn(worker)
                .expect("thumb worker");
        }
    });
}

fn worker() {
    let p = pool();
    loop {
        let job = {
            let mut q = p.queue.lock().unwrap();
            while q.pending.is_empty() {
                q = p.wake.wait(q).unwrap();
            }
            q.pending.pop_front()
        };
        let Some((path, mime)) = job else { continue };
        let result = generate(&path, &mime).ok();
        {
            let mut q = p.queue.lock().unwrap();
            q.queued.remove(&path);
        }
        p.done.lock().unwrap().insert(path.clone(), result);
        if let Some(tx) = p.ready_tx.lock().unwrap().as_ref() {
            let _ = tx.send(path);
        }
    }
}

/// Cached thumbnail for `path`, or `None` (and a queued generation) if not
/// ready yet. `Some(None)` means "tried, can't" — caller shows the icon.
pub fn get(path: &Path, mime: &str) -> Option<Option<PathBuf>> {
    let p = pool();
    if let Some(v) = p.done.lock().unwrap().get(path) {
        return Some(v.clone());
    }
    // Disk cache hit without going through a worker: cheap stat + exists.
    if let Some(cp) = cache_path(path) {
        if cp.exists() {
            p.done.lock().unwrap().insert(path.to_path_buf(), Some(cp.clone()));
            return Some(Some(cp));
        }
    }
    start_workers();
    let mut q = p.queue.lock().unwrap();
    if q.queued.insert(path.to_path_buf()) {
        // Newest requests first: the user is looking at what just scrolled in.
        q.pending.push_front((path.to_path_buf(), mime.to_string()));
        p.wake.notify_one();
    }
    None
}

/// Forget cached state for a file that changed or vanished.
pub fn invalidate(path: &Path) {
    pool().done.lock().unwrap().remove(path);
}

/// Blocking wait for the next finished thumbnail (for the subscription).
pub fn next_ready() -> Option<PathBuf> {
    pool();
    let slot = READY_RX.get_or_init(|| Mutex::new(None));
    let guard = slot.lock().unwrap();
    guard.as_ref().and_then(|rx| rx.recv_timeout(std::time::Duration::from_millis(500)).ok())
}

fn cache_dir() -> PathBuf {
    crate::filesystem::home_dir().join(".cache/og-files/thumbs")
}

fn cache_path(source: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(source).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    modified.hash(&mut h);
    meta.len().hash(&mut h);
    Some(cache_dir().join(format!("{:016x}.png", h.finish())))
}

pub fn wants_thumbnail(mime: &str) -> bool {
    crate::filesystem::is_previewable_image(mime)
        || crate::filesystem::is_video(mime)
        || mime == "image/svg+xml"
        || mime == "application/pdf"
}

fn generate(source: &Path, mime: &str) -> Result<PathBuf, String> {
    let cp = cache_path(source).ok_or("stat failed")?;
    if cp.exists() {
        return Ok(cp);
    }
    std::fs::create_dir_all(cp.parent().unwrap()).map_err(|e| e.to_string())?;

    if crate::filesystem::is_video(mime) {
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "quiet", "-i"]).arg(source)
            .args(["-ss", "00:00:01", "-frames:v", "1", "-vf", &format!("scale={0}:{0}:force_original_aspect_ratio=decrease", THUMB_PX)])
            .arg(&cp).status().map(|s| s.success()).unwrap_or(false);
        return if ok && cp.exists() { Ok(cp) } else { Err("ffmpeg".into()) };
    }
    if mime == "application/pdf" {
        // pdftoppm writes "<prefix>-1.png"; ask for one page at thumb size.
        let prefix = cp.with_extension("");
        let ok = std::process::Command::new("pdftoppm")
            .args(["-png", "-f", "1", "-l", "1", "-scale-to", &THUMB_PX.to_string(), "-singlefile"])
            .arg(source).arg(&prefix).status().map(|s| s.success()).unwrap_or(false);
        let produced = prefix.with_extension("png");
        if ok && produced.exists() {
            if produced != cp {
                let _ = std::fs::rename(&produced, &cp);
            }
            return Ok(cp);
        }
        return Err("pdftoppm".into());
    }
    if mime == "image/svg+xml" {
        let ok = std::process::Command::new("rsvg-convert")
            .args(["-w", &THUMB_PX.to_string(), "-h", &THUMB_PX.to_string(), "--keep-aspect-ratio", "-o"])
            .arg(&cp).arg(source).status().map(|s| s.success()).unwrap_or(false);
        return if ok && cp.exists() { Ok(cp) } else { Err("rsvg-convert".into()) };
    }

    if let Ok((w, h)) = image::image_dimensions(source) {
        if (w as u64) * (h as u64) > 80_000_000 {
            return Err("too large".into());
        }
    }
    let img = image::open(source).map_err(|e| e.to_string())?;
    let thumb = img.thumbnail(THUMB_PX, THUMB_PX);
    thumb.save(&cp).map_err(|e| e.to_string())?;
    Ok(cp)
}
