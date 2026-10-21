//! macOS-only pieces that go through Cocoa instead of child processes, so
//! they also work inside the Mac App Store sandbox:
//!
//! - opening files (default app, or a chosen app) with NSWorkspace instead
//!   of `/usr/bin/open`
//! - folder access under the App Sandbox: the app may only read folders the
//!   user picked in an open panel. OG Files asks once (usually for the home
//!   folder), keeps a security-scoped bookmark per granted folder in its
//!   container and re-opens access to each one at launch.
//!
//! Outside the sandbox (the direct-download build) `sandboxed()` is false
//! and the bookmark code is never used.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{
    NSArray, NSData, NSString, NSURLBookmarkCreationOptions, NSURLBookmarkResolutionOptions, NSURL,
};

/// Running under the App Sandbox (the Mac App Store build).
pub fn sandboxed() -> bool {
    std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some()
}

/// The user's real home folder. Inside the sandbox `$HOME` is the app's
/// container (`~/Library/Containers/<id>/Data`), so ask the password
/// database instead.
pub fn real_home() -> Option<PathBuf> {
    // SAFETY: getpwuid returns a pointer into static storage or null.
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*pw).pw_dir).to_string_lossy().into_owned();
        Some(PathBuf::from(dir))
    }
}

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

fn url_path(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|p| PathBuf::from(p.to_string()))
}

// ── Opening files ────────────────────────────────────────────────────────

/// Opens `files` in `app` (an `.app` bundle) or, without one, each in its
/// default app.
pub fn open(files: &[PathBuf], app: Option<&Path>) -> Result<(), String> {
    let ws = NSWorkspace::sharedWorkspace();
    match app {
        None => {
            for f in files {
                if !ws.openURL(&file_url(f)) {
                    return Err(format!("No app can open {}", f.display()));
                }
            }
        }
        Some(bundle) => {
            if !bundle.exists() {
                return Err(format!("{} not found", bundle.display()));
            }
            let urls: Vec<Retained<NSURL>> = files.iter().map(|f| file_url(f)).collect();
            let urls = NSArray::from_retained_slice(&urls);
            let config = NSWorkspaceOpenConfiguration::configuration();
            ws.openURLs_withApplicationAtURL_configuration_completionHandler(
                &urls,
                &file_url(bundle),
                &config,
                None,
            );
        }
    }
    Ok(())
}

// ── Folder access (App Sandbox) ────────────────────────────────────────

fn store_path() -> PathBuf {
    crate::config::config_dir().join("folder-access.json")
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Store {
    /// Granted folder → security-scoped bookmark bytes.
    bookmarks: Vec<(PathBuf, Vec<u8>)>,
}

fn load_store() -> Store {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_store(store: &Store) {
    let path = store_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(store) {
        let _ = std::fs::write(path, json);
    }
}

fn bookmark_bytes(url: &NSURL) -> Option<Vec<u8>> {
    let data = url
        .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::WithSecurityScope,
            None,
            None,
        )
        .ok()?;
    Some(data.to_vec())
}

/// Re-opens access to every folder granted in earlier sessions. Called once
/// at launch, before the first folder is listed.
pub fn restore_access() {
    if !sandboxed() {
        return;
    }
    let mut store = load_store();
    let mut changed = false;
    let mut kept = Vec::new();
    for (path, bytes) in store.bookmarks.drain(..) {
        let data = NSData::with_bytes(&bytes);
        let mut stale = Bool::NO;
        // SAFETY: plain Foundation call; `stale` outlives it.
        let resolved = unsafe {
            NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
                &data,
                NSURLBookmarkResolutionOptions::WithSecurityScope,
                None,
                &mut stale,
            )
        };
        let Ok(url) = resolved else {
            changed = true; // the folder is gone: forget it
            continue;
        };
        // SAFETY: balanced only by process exit; access stays open all session.
        if !unsafe { url.startAccessingSecurityScopedResource() } {
            changed = true;
            continue;
        }
        let real = url_path(&url).unwrap_or(path);
        // A stale bookmark still works this once; refresh it for next time.
        let bytes = if stale.as_bool() {
            changed = true;
            bookmark_bytes(&url).unwrap_or(bytes)
        } else {
            bytes
        };
        kept.push((real, bytes));
    }
    store.bookmarks = kept;
    if changed {
        save_store(&store);
    }
}

/// Records a folder the user just picked in the open panel (the panel
/// granted access for this session) so it's re-opened at every launch.
pub fn remember(path: &Path) {
    let url = file_url(path);
    let Some(bytes) = bookmark_bytes(&url) else {
        return;
    };
    // SAFETY: keeps access alive for the session even if the panel's own
    // grant lapses.
    unsafe { url.startAccessingSecurityScopedResource() };
    let mut store = load_store();
    store.bookmarks.retain(|(p, _)| p != path);
    store.bookmarks.push((path.to_path_buf(), bytes));
    save_store(&store);
}

/// The folder to suggest in the open panel when `path` can't be read: the
/// home folder for anything inside it, else the volume (or the folder).
pub fn suggested_grant(path: &Path) -> PathBuf {
    let home = real_home().unwrap_or_else(|| PathBuf::from("/"));
    if path.starts_with(&home) {
        home
    } else if let Ok(rest) = path.strip_prefix("/Volumes") {
        rest.components()
            .next()
            .map(|c| Path::new("/Volumes").join(c))
            .unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

/// Shows the open panel at `start`; the folder the user chose, if any.
pub async fn pick_folder(start: PathBuf) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title("Give OG Files access to a folder")
        .set_directory(&start)
        .pick_folder()
        .await
        .map(|h| h.path().to_path_buf())
}
