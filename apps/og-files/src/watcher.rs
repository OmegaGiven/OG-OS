//! Live folder refresh: an inotify watch on every open tab's folder, so a
//! download finishing or a terminal `mv` shows up without pressing F5.
//!
//! One global watcher; the app re-points it whenever the set of open
//! folders changes. Events are debounced per folder (a build touching
//! 2000 files must not trigger 2000 relists).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

struct Shared {
    watcher: Option<notify::RecommendedWatcher>,
    watched: HashSet<PathBuf>,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
static EVENTS: OnceLock<Mutex<Option<mpsc::Receiver<PathBuf>>>> = OnceLock::new();

fn shared() -> &'static Mutex<Shared> {
    SHARED.get_or_init(|| Mutex::new(Shared { watcher: None, watched: HashSet::new() }))
}

fn ensure_started() {
    let mut s = shared().lock().unwrap();
    if s.watcher.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel::<PathBuf>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            // Report the *folder* that changed, whatever happened inside it.
            for p in ev.paths {
                let dir = if p.is_dir() && matches!(ev.kind, notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))) {
                    p.clone()
                } else {
                    p.parent().map(Path::to_path_buf).unwrap_or(p.clone())
                };
                let _ = tx.send(dir);
            }
        }
    });
    match watcher {
        Ok(w) => {
            s.watcher = Some(w);
            *EVENTS.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(rx);
        }
        Err(e) => eprintln!("og-files: inotify unavailable, live refresh disabled: {e}"),
    }
}

/// Replace the watched set with exactly these folders.
pub fn set_watched(dirs: impl IntoIterator<Item = PathBuf>) {
    ensure_started();
    let want: HashSet<PathBuf> = dirs.into_iter().collect();
    let mut s = shared().lock().unwrap();
    let have = s.watched.clone();
    let Some(w) = s.watcher.as_mut() else { return };
    for d in have.difference(&want) {
        let _ = w.unwatch(d);
    }
    for d in want.difference(&have) {
        let _ = w.watch(d, RecursiveMode::NonRecursive);
    }
    s.watched = want;
}

/// Blocking drain used by the app's subscription: waits for at least one
/// event, then keeps collecting for a short quiet period and returns the
/// distinct folders touched.
pub fn next_batch() -> Vec<PathBuf> {
    let rx_slot = EVENTS.get_or_init(|| Mutex::new(None));
    loop {
        let first = {
            let guard = rx_slot.lock().unwrap();
            match guard.as_ref() {
                Some(rx) => rx.recv_timeout(Duration::from_millis(500)),
                None => Err(mpsc::RecvTimeoutError::Timeout),
            }
        };
        let Ok(first) = first else {
            if rx_slot.lock().unwrap().is_none() {
                std::thread::sleep(Duration::from_millis(500));
            }
            continue;
        };
        let mut set = HashSet::new();
        set.insert(first);
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let next = {
                let guard = rx_slot.lock().unwrap();
                match guard.as_ref() {
                    Some(rx) => rx.recv_timeout(deadline - now),
                    None => break,
                }
            };
            match next {
                Ok(p) => { set.insert(p); }
                Err(_) => break,
            }
        }
        return set.into_iter().collect();
    }
}
