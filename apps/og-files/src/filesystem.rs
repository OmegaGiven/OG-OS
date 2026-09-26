use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use std::os::unix::fs::MetadataExt;
use chrono::{DateTime, Local, TimeZone};
use mime_guess::from_path;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: DateTime<Local>,
    pub mime_type: String,
    pub icon: &'static str,
    pub previewable: bool,
}

pub fn read_dir_entries(path: &Path, show_hidden: bool) -> Vec<FileEntry> {
    let Ok(dir) = std::fs::read_dir(path) else {
        return Vec::new();
    };

    let mut entries: Vec<FileEntry> = dir
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !show_hidden && name.starts_with('.') {
                return None;
            }
            let meta = e.metadata().ok()?;
            let is_dir = meta.is_dir();
            let size = if is_dir { 0 } else { meta.len() };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| Local.timestamp_opt(d.as_secs() as i64, 0).single())
                .flatten()
                .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());

            let path = e.path();
            let mime_type = if is_dir {
                "inode/directory".to_string()
            } else {
                from_path(&path).first_or_octet_stream().to_string()
            };
            let icon = icon_for(&mime_type, is_dir);
            let previewable = !is_dir && is_really_previewable(&path, &mime_type);

            Some(FileEntry { path, name, is_dir, size, modified, mime_type, icon, previewable })
        })
        .collect();

    entries.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    });

    entries
}

/// Nerd Font glyphs, not emoji — the app's default font has no emoji
/// coverage at all, which was rendering every icon as a "?" tofu box.
/// Image files don't use this: the file list renders a real thumbnail for
/// those instead (see `panes/filelist.rs`).
pub fn icon_for(mime_type: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "\u{f07b}"; // nf-fa-folder
    }
    if mime_type.starts_with("image/") { return "\u{f1c5}"; } // nf-fa-file_image_o
    if mime_type.starts_with("video/") { return "\u{f1c8}"; } // nf-fa-file_video_o
    if mime_type.starts_with("audio/") { return "\u{f1c7}"; } // nf-fa-file_audio_o
    if mime_type.starts_with("text/") { return "\u{f0f6}"; } // nf-fa-file_text_o
    match mime_type {
        "application/pdf" => "\u{f1c1}", // nf-fa-file_pdf_o
        "application/zip"
        | "application/x-tar"
        | "application/gzip"
        | "application/x-bzip2"
        | "application/x-xz"
        | "application/x-7z-compressed"
        | "application/x-rar-compressed" => "\u{f1c6}", // nf-fa-file_archive_o
        "application/x-executable" | "application/x-elf" => "\u{f013}", // nf-fa-cog
        "application/x-desktop" => "\u{f108}", // nf-fa-desktop
        _ => "\u{f016}", // nf-fa-file_o
    }
}

/// Whether this entry should show a real decoded thumbnail instead of an
/// icon glyph — only worth it for formats iced's `image` widget (via the
/// `image` crate) can actually decode.
pub fn is_previewable_image(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/bmp" | "image/webp" | "image/x-tga" | "image/x-portable-pixmap"
    )
}

/// Extension-guessed mime types lie: a renamed file (e.g. a WebP saved as
/// `.jpg`) will pass `is_previewable_image` but fail to decode, and iced's
/// image widget turns that failure into a degenerate (NaN/negative) quad
/// that crashes the whole renderer instead of just not showing a thumbnail.
/// Checking the real magic bytes before trusting the extension avoids that.
fn sniff_matches_mime(path: &Path, mime_type: &str) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 16];
    let Ok(n) = file.read(&mut buf) else { return false };
    let buf = &buf[..n];

    match mime_type {
        "image/png" => buf.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => buf.starts_with(b"\xff\xd8\xff"),
        "image/gif" => buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a"),
        "image/bmp" => buf.starts_with(b"BM"),
        "image/webp" => buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP",
        // No cheap, reliable magic number for these — trust the extension.
        "image/x-tga" | "image/x-portable-pixmap" => true,
        _ => false,
    }
}

/// Above this, a full decode into raw RGBA just to show a 40px thumbnail
/// costs tens of ms and tens of MB per image — with a folder full of
/// screenshots that adds up to a very not-instant first frame. Dimensions
/// are read from the header only (no full decode), so this check is cheap.
const MAX_THUMBNAIL_PIXELS: u64 = 2_000_000; // ~1414x1414

fn is_reasonable_thumbnail_size(path: &Path) -> bool {
    match image::image_dimensions(path) {
        Ok((w, h)) => (w as u64) * (h as u64) <= MAX_THUMBNAIL_PIXELS,
        Err(_) => false,
    }
}

/// Real (magic-byte verified) image, regardless of size — used by the
/// preview panel, which decodes off the UI thread and can afford a big
/// original. The grid's inline thumbnails use `is_really_previewable`
/// instead, which adds a size cap since those decode synchronously.
fn is_genuinely_an_image(path: &Path, mime_type: &str) -> bool {
    is_previewable_image(mime_type) && sniff_matches_mime(path, mime_type)
}

pub fn is_really_previewable(path: &Path, mime_type: &str) -> bool {
    is_genuinely_an_image(path, mime_type) && is_reasonable_thumbnail_size(path)
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "-".to_string();
    }
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut val = bytes as f64;
    let mut i = 0;
    while val >= 1024.0 && i < UNITS.len() - 1 {
        val /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", val, UNITS[i])
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}

pub fn open_default(path: &Path) {
    let _ = std::process::Command::new("xdg-open")
        .arg(path)
        .spawn();
}

pub fn open_with(path: &Path, exec: &str) {
    let path_str = path.to_string_lossy();
    let parts: Vec<String> = exec
        .split_whitespace()
        .map(|tok| match tok {
            "%f" | "%F" | "%u" | "%U" => path_str.to_string(),
            other => other.to_string(),
        })
        .collect();
    if parts.is_empty() {
        return;
    }
    let _ = std::process::Command::new(&parts[0])
        .args(&parts[1..])
        .spawn();
}

pub fn trash_dir() -> PathBuf {
    home_dir().join(".local/share/Trash")
}

/// Moves one file/folder to `~/.local/share/Trash`, writing its `.trashinfo`
/// sidecar (freedesktop trash spec) and returning the trashed path. Used by
/// `jobs::Worker` — one call per item, so cancel/progress stay granular
/// instead of the old all-or-partial `move_to_trash` batch.
pub fn trash_one(path: &Path) -> Result<PathBuf, String> {
    let files_dir = trash_dir().join("files");
    let info_dir = trash_dir().join("info");
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;

    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut dst = files_dir.join(&name);
    let mut info_name = format!("{name}.trashinfo");
    // Two different files both named "notes.txt" from different folders
    // must not collide in the flat trash — same "(2)" scheme as elsewhere.
    let mut n = 2;
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());
    while dst.exists() {
        let candidate_name = match &ext {
            Some(ext) if path.is_file() => format!("{stem} ({n}).{ext}"),
            _ => format!("{name} ({n})"),
        };
        dst = files_dir.join(&candidate_name);
        info_name = format!("{candidate_name}.trashinfo");
        n += 1;
    }

    std::fs::rename(path, &dst).map_err(|e| format!("{}: {}", path.display(), e))?;
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
    let content = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", path.display(), now);
    let _ = std::fs::write(info_dir.join(&info_name), content);
    Ok(dst)
}

/// Reads the `.trashinfo` sidecar for a trashed path to find where it came
/// from originally.
pub fn trash_origin(trashed_path: &Path) -> Option<PathBuf> {
    let name = trashed_path.file_name()?.to_string_lossy().to_string();
    let info_path = trash_dir().join("info").join(format!("{}.trashinfo", name));
    let info = std::fs::read_to_string(&info_path).ok()?;
    let original = info.lines().find_map(|l| l.strip_prefix("Path="))?;
    Some(PathBuf::from(original))
}

/// Moves a trashed item back to `dest` and removes its `.trashinfo`.
pub fn restore_to(trashed_path: &Path, dest: &Path) -> Result<(), String> {
    let name = trashed_path.file_name().ok_or("invalid trash entry")?.to_string_lossy().to_string();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(trashed_path, dest).map_err(|e| e.to_string())?;
    let info_path = trash_dir().join("info").join(format!("{}.trashinfo", name));
    let _ = std::fs::remove_file(info_path);
    Ok(())
}

pub fn move_to_trash(paths: &[PathBuf]) -> Result<(), String> {
    let files_dir = trash_dir().join("files");
    let info_dir = trash_dir().join("info");
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;

    for path in paths {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let dst = files_dir.join(name.as_ref());
        std::fs::rename(path, &dst).map_err(|e| format!("{}: {}", path.display(), e))?;

        let info_path = info_dir.join(format!("{}.trashinfo", name));
        let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S");
        let content = format!(
            "[Trash Info]\nPath={}\nDeletionDate={}\n",
            path.display(),
            now
        );
        let _ = std::fs::write(info_path, content);
    }
    Ok(())
}

pub fn copy_file(src: &Path, dst: &Path) -> Result<(), String> {
    if src.is_dir() {
        copy_dir_all(src, dst).map_err(|e| e.to_string())
    } else {
        std::fs::copy(src, dst).map(|_| ()).map_err(|e| e.to_string())
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst.join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}

pub fn move_file(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::rename(src, dst)
        .or_else(|_| {
            copy_file(src, dst)?;
            if src.is_dir() {
                std::fs::remove_dir_all(src).map_err(|e| e.to_string())
            } else {
                std::fs::remove_file(src).map_err(|e| e.to_string())
            }
        })
        .map_err(|e: String| e)
}

pub fn create_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir(path).map_err(|e| e.to_string())
}

pub fn rename_entry(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

pub fn create_file(path: &Path) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Picks "New Folder", "New Folder (2)", "New Folder (3)", ... whichever doesn't collide.
pub fn unique_name(dir: &Path, base: &str) -> PathBuf {
    let candidate = dir.join(base);
    if !candidate.exists() {
        return candidate;
    }
    let mut n = 2;
    loop {
        let candidate = dir.join(format!("{} ({})", base, n));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

// ── Archives ───────────────────────────────────────────────────────────────────

pub fn is_archive(path: &Path) -> bool {
    let name = path.to_string_lossy().to_lowercase();
    name.ends_with(".zip")
        || name.ends_with(".tar")
        || name.ends_with(".tar.gz")
        || name.ends_with(".tgz")
        || name.ends_with(".tar.bz2")
        || name.ends_with(".tar.xz")
}

pub fn compress_paths(paths: &[PathBuf], dest_dir: &Path) -> Result<(), String> {
    let Some(first) = paths.first() else { return Ok(()) };
    let base_name = first.file_name().unwrap_or_default().to_string_lossy().to_string();
    let archive = unique_name(dest_dir, &format!("{}.tar.gz", base_name));
    let parent = first.parent().unwrap_or(dest_dir);

    let mut cmd = std::process::Command::new("tar");
    cmd.arg("czf").arg(&archive).arg("-C").arg(parent);
    for p in paths {
        cmd.arg(p.file_name().unwrap_or_default());
    }
    cmd.status().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn extract_archive(path: &Path, dest_dir: &Path) -> Result<(), String> {
    let name = path.to_string_lossy().to_lowercase();
    let status = if name.ends_with(".zip") {
        std::process::Command::new("unzip")
            .arg("-o").arg(path).arg("-d").arg(dest_dir)
            .status()
    } else {
        std::process::Command::new("tar")
            .arg("xf").arg(path).arg("-C").arg(dest_dir)
            .status()
    };
    status.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn open_terminal_here(path: &Path) {
    spawn_terminal(path, None);
}

/// Opens a terminal in `dir` with `prefill` sitting ready-to-edit on the
/// command line (via bash's `read -e -i`), rather than actually run it.
pub fn open_terminal_with_prefill(dir: &Path, prefill: &str) {
    let escaped = prefill.replace('\'', "'\\''");
    let inner = format!(
        "read -e -i '{}' -p '$ ' line; history -s \"$line\"; eval \"$line\"; exec bash",
        escaped
    );
    spawn_terminal(dir, Some(&inner));
}

/// Same config file (and same keys) `app::load_theme_colors` reads, so a
/// spawned terminal matches the rest of the UI instead of whatever default
/// theme the terminal emulator ships with.
fn theme_hex_colors() -> (String, String, String, String) {
    let path = home_dir().join(".config/sway-power/config.json");
    let json: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let s = |k: &str, d: &str| json[k].as_str().unwrap_or(d).to_string();
    (s("bar_bg", "#1a1a2e"), s("sec_bg", "#2a2535"), s("bar_text", "#e0e0e0"), s("accent", "#ff7800"))
}

fn theme_cache_dir() -> PathBuf {
    home_dir().join(".cache/og-files")
}

/// Writes (or refreshes, in case the theme changed) a small Alacritty TOML
/// themed to match the app, and returns its path for `--config-file`.
fn ensure_alacritty_theme() -> Option<PathBuf> {
    let (bar_bg, sec_bg, bar_text, accent) = theme_hex_colors();
    let path = theme_cache_dir().join("alacritty-theme.toml");
    std::fs::create_dir_all(path.parent()?).ok()?;
    let toml = format!(
        "[colors.primary]\nbackground = \"{bg}\"\nforeground = \"{fg}\"\n\n\
         [colors.cursor]\ncursor = \"{accent}\"\ntext = \"{bg}\"\n\n\
         [colors.selection]\nbackground = \"{sel}\"\ntext = \"{fg}\"\n",
        bg = bar_bg, fg = bar_text, accent = accent, sel = sec_bg,
    );
    std::fs::write(&path, toml).ok()?;
    Some(path)
}

/// Foot takes theme overrides straight on the command line (`-o key=value`),
/// so unlike Alacritty there's no need to write a file — and no risk of
/// shadowing the user's own `foot.ini`.
fn foot_theme_overrides() -> Vec<String> {
    let (bar_bg, sec_bg, bar_text, accent) = theme_hex_colors();
    let strip = |h: &str| h.trim_start_matches('#').to_string();
    let (bg, fg, sel, cursor) = (strip(&bar_bg), strip(&bar_text), strip(&sec_bg), strip(&accent));
    vec![
        format!("colors.background={bg}"),
        format!("colors.foreground={fg}"),
        format!("colors.selection-background={sel}"),
        format!("colors.selection-foreground={fg}"),
        format!("cursor.color={fg} {cursor}"),
    ]
}

fn preferred_terminal() -> String {
    let path = home_dir().join(".config/sway-power/config.json");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|json| json["terminal"].as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "alacritty".to_string())
}

fn spawn_terminal(dir: &Path, shell_command: Option<&str>) {
    let preferred = preferred_terminal();
    let env_term = std::env::var("TERMINAL").ok();
    let candidates: Vec<&str> = env_term.iter().map(|s| s.as_str())
        .chain(std::iter::once(preferred.as_str()))
        .chain(["alacritty", "foot", "xterm"])
        .collect();

    for candidate in candidates {
        let mut cmd = std::process::Command::new(candidate);
        cmd.current_dir(dir);

        match candidate {
            "alacritty" => {
                if let Some(theme) = ensure_alacritty_theme() {
                    cmd.arg("--config-file").arg(theme);
                }
            }
            "foot" => {
                for o in foot_theme_overrides() {
                    cmd.arg("-o").arg(o);
                }
            }
            _ => {}
        }

        if let Some(shell_command) = shell_command {
            cmd.args(["-e", "bash", "-c", shell_command]);
        }
        if cmd.spawn().is_ok() {
            return;
        }
    }
}

// ── Drives (Devices / Network) ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct DriveInfo {
    pub label: String,
    pub mount_point: PathBuf,
    pub total: u64,
    pub used: u64,
}

impl DriveInfo {
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 { 0.0 } else { self.used as f32 / self.total as f32 }
    }
}

const NETWORK_FSTYPES: &[&str] = &[
    "nfs", "nfs4", "cifs", "smb3", "smbfs", "sshfs", "fuse.sshfs", "davfs", "ftpfs",
];

const IGNORED_FSTYPES: &[&str] = &[
    "proc", "sysfs", "devtmpfs", "devpts", "tmpfs", "cgroup", "cgroup2", "pstore",
    "bpf", "tracefs", "debugfs", "mqueue", "hugetlbfs", "securityfs", "configfs",
    "autofs", "binfmt_misc", "efivarfs", "overlay", "squashfs", "fusectl", "ramfs",
];

fn statvfs_usage(mount_point: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(mount_point.as_os_str().as_bytes()).ok()?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return None;
        }
        let block_size = stat.f_frsize as u64;
        let total = stat.f_blocks as u64 * block_size;
        let free = stat.f_bfree as u64 * block_size;
        Some((total, total.saturating_sub(free)))
    }
}

/// Reads /proc/mounts and splits real mounts into local devices vs network shares,
/// skipping virtual/pseudo filesystems that clutter every Linux mount table.
pub fn list_drives() -> (Vec<DriveInfo>, Vec<DriveInfo>) {
    let Ok(content) = std::fs::read_to_string("/proc/mounts") else {
        return (Vec::new(), Vec::new());
    };

    let mut devices = Vec::new();
    let mut network = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for line in content.lines() {
        let mut fields = line.split_whitespace();
        let Some(source) = fields.next() else { continue };
        let Some(mount_point) = fields.next() else { continue };
        let Some(fstype) = fields.next() else { continue };

        if IGNORED_FSTYPES.contains(&fstype) {
            continue;
        }
        let is_network = NETWORK_FSTYPES.contains(&fstype) || source.contains(':');
        if !is_network && !source.starts_with("/dev/") {
            continue;
        }
        if !seen.insert(mount_point.to_string()) {
            continue;
        }

        let mount_path = PathBuf::from(
            mount_point
                .replace("\\040", " ")
                .replace("\\011", "\t"),
        );
        let Some((total, used)) = statvfs_usage(&mount_path) else { continue };
        if total == 0 {
            continue;
        }

        let label = if mount_path == PathBuf::from("/") {
            "System".to_string()
        } else if is_network {
            source.to_string()
        } else {
            mount_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| mount_point.to_string())
        };

        let info = DriveInfo { label, mount_point: mount_path, total, used };
        if is_network {
            network.push(info);
        } else {
            devices.push(info);
        }
    }

    devices.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    network.sort_by(|a, b| a.label.cmp(&b.label));
    (devices, network)
}

// ── Recent places ────────────────────────────────────────────────────────────────

fn recent_file() -> PathBuf {
    home_dir().join(".config/og-files/recent.json")
}

pub fn load_recent() -> Vec<PathBuf> {
    std::fs::read_to_string(recent_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_recent(recent: &[PathBuf]) {
    if let Some(dir) = recent_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(recent) {
        let _ = std::fs::write(recent_file(), json);
    }
}

// ── Detailed file info / preview panel ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ExifTag {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone)]
pub struct FileDetails {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<PathBuf>,
    pub size: u64,
    pub item_count: Option<usize>,
    pub mime_type: String,
    pub created: Option<DateTime<Local>>,
    pub modified: Option<DateTime<Local>>,
    pub accessed: Option<DateTime<Local>>,
    pub permissions: String,
    pub mode_octal: String,
    pub owner: String,
    pub group: String,
    pub dimensions: Option<(u32, u32)>,
    pub exif: Vec<ExifTag>,
}

fn system_time_to_local(t: std::io::Result<std::time::SystemTime>) -> Option<DateTime<Local>> {
    let t = t.ok()?;
    let dur = t.duration_since(UNIX_EPOCH).ok()?;
    Local.timestamp_opt(dur.as_secs() as i64, dur.subsec_nanos()).single()
}

fn user_name(uid: u32) -> String {
    unsafe {
        let pw = libc::getpwuid(uid);
        if pw.is_null() {
            return uid.to_string();
        }
        std::ffi::CStr::from_ptr((*pw).pw_name).to_string_lossy().to_string()
    }
}

fn group_name(gid: u32) -> String {
    unsafe {
        let gr = libc::getgrgid(gid);
        if gr.is_null() {
            return gid.to_string();
        }
        std::ffi::CStr::from_ptr((*gr).gr_name).to_string_lossy().to_string()
    }
}

fn permissions_string(mode: u32) -> String {
    let file_type = mode & libc::S_IFMT;
    let type_char = if file_type == libc::S_IFDIR {
        'd'
    } else if file_type == libc::S_IFLNK {
        'l'
    } else {
        '-'
    };
    let bits: [(u32, char); 9] = [
        (libc::S_IRUSR, 'r'), (libc::S_IWUSR, 'w'), (libc::S_IXUSR, 'x'),
        (libc::S_IRGRP, 'r'), (libc::S_IWGRP, 'w'), (libc::S_IXGRP, 'x'),
        (libc::S_IROTH, 'r'), (libc::S_IWOTH, 'w'), (libc::S_IXOTH, 'x'),
    ];
    let mut s = String::new();
    s.push(type_char);
    for (bit, c) in bits {
        s.push(if mode & bit != 0 { c } else { '-' });
    }
    s
}

fn exif_for_file(path: &Path) -> Vec<ExifTag> {
    let Ok(file) = std::fs::File::open(path) else { return Vec::new() };
    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else { return Vec::new() };

    const WANTED: &[exif::Tag] = &[
        exif::Tag::Make,
        exif::Tag::Model,
        exif::Tag::DateTimeOriginal,
        exif::Tag::ExposureTime,
        exif::Tag::FNumber,
        exif::Tag::PhotographicSensitivity,
        exif::Tag::FocalLength,
        exif::Tag::Orientation,
    ];

    WANTED
        .iter()
        .filter_map(|tag| {
            let field = exif.get_field(*tag, exif::In::PRIMARY)?;
            Some(ExifTag {
                label: format!("{}", tag),
                value: field.display_value().with_unit(&exif).to_string(),
            })
        })
        .collect()
}

pub fn get_file_details(path: &Path) -> Option<FileDetails> {
    let link_meta = std::fs::symlink_metadata(path).ok()?;
    let is_symlink = link_meta.file_type().is_symlink();
    let target_meta = if is_symlink { std::fs::metadata(path).ok() } else { None };
    let stat_meta = target_meta.as_ref().unwrap_or(&link_meta);
    let is_dir = stat_meta.is_dir();

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let mime_type = if is_dir {
        "inode/directory".to_string()
    } else {
        from_path(path).first_or_octet_stream().to_string()
    };

    let size = stat_meta.len();
    let item_count = if is_dir {
        std::fs::read_dir(path).ok().map(|rd| rd.filter_map(|e| e.ok()).count())
    } else {
        None
    };

    let permissions = permissions_string(stat_meta.mode());
    let mode_octal = format!("{:o}", stat_meta.mode() & 0o7777);
    let owner = user_name(stat_meta.uid());
    let group = group_name(stat_meta.gid());

    let created = system_time_to_local(stat_meta.created());
    let modified = system_time_to_local(stat_meta.modified());
    let accessed = system_time_to_local(stat_meta.accessed());

    let dimensions = if !is_dir && is_really_previewable(path, &mime_type) {
        image::image_dimensions(path).ok()
    } else {
        None
    };

    let exif = if !is_dir && (mime_type == "image/jpeg" || mime_type == "image/tiff") {
        exif_for_file(path)
    } else {
        Vec::new()
    };

    Some(FileDetails {
        name,
        path: path.to_path_buf(),
        is_dir,
        is_symlink,
        symlink_target: if is_symlink { std::fs::read_link(path).ok() } else { None },
        size,
        item_count,
        mime_type,
        created,
        modified,
        accessed,
        permissions,
        mode_octal,
        owner,
        group,
        dimensions,
        exif,
    })
}

// ── Recursive background search ─────────────────────────────────────────────────

pub fn matches_query(name: &str, query: &str) -> bool {
    name.to_lowercase().contains(&query.to_lowercase())
}

/// One step of a background BFS search: reads a single directory, returns the
/// entries in it that match `query` plus the subdirectories to queue up next.
/// Meant to run inside `spawn_blocking` — a single dir read is cheap, but a
/// whole tree can be large, so callers drive this one directory at a time
/// rather than recursing here, so a search can be cancelled between steps.
pub fn scan_dir_search(dir: &Path, query: &str, show_hidden: bool) -> (Vec<FileEntry>, Vec<PathBuf>) {
    let mut matches = Vec::new();
    let mut subdirs = Vec::new();

    let Ok(rd) = std::fs::read_dir(dir) else {
        return (matches, subdirs);
    };

    for entry in rd.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let is_dir = meta.is_dir();
        let path = entry.path();

        if is_dir {
            subdirs.push(path.clone());
        }

        if matches_query(&name, query) {
            let size = if is_dir { 0 } else { meta.len() };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| Local.timestamp_opt(d.as_secs() as i64, 0).single())
                .flatten()
                .unwrap_or_else(|| Local.timestamp_opt(0, 0).single().unwrap());
            let mime_type = if is_dir {
                "inode/directory".to_string()
            } else {
                from_path(&path).first_or_octet_stream().to_string()
            };
            let icon = icon_for(&mime_type, is_dir);
            let previewable = !is_dir && is_really_previewable(&path, &mime_type);
            matches.push(FileEntry { path, name, is_dir, size, modified, mime_type, icon, previewable });
        }
    }

    (matches, subdirs)
}

// ── Duplicate / permanent delete / trash restore / zip / bookmarks ─────────────

/// Picks "name copy.ext", then "name copy 2.ext", "name copy 3.ext", ...
pub fn duplicate_name(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string());

    let make = |suffix: &str| -> PathBuf {
        match &ext {
            Some(ext) => parent.join(format!("{} {}.{}", stem, suffix, ext)),
            None => parent.join(format!("{} {}", stem, suffix)),
        }
    };

    let first = make("copy");
    if !first.exists() {
        return first;
    }
    let mut n = 2;
    loop {
        let candidate = make(&format!("copy {}", n));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

pub fn delete_permanently(paths: &[PathBuf]) -> Result<(), String> {
    for path in paths {
        let result = if path.is_dir() && !path.is_symlink() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        result.map_err(|e| format!("{}: {}", path.display(), e))?;
    }
    Ok(())
}

pub fn empty_trash() -> Result<(), String> {
    let files_dir = trash_dir().join("files");
    let info_dir = trash_dir().join("info");
    let _ = std::fs::remove_dir_all(&files_dir);
    let _ = std::fs::remove_dir_all(&info_dir);
    std::fs::create_dir_all(&files_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&info_dir).map_err(|e| e.to_string())?;
    Ok(())
}

/// Reads the matching `.trashinfo` file to find where a trashed item came
/// from, moves it back there, and removes the trashinfo sidecar.
pub fn restore_from_trash(trashed_path: &Path) -> Result<PathBuf, String> {
    let name = trashed_path.file_name().ok_or("invalid trash entry")?.to_string_lossy().to_string();
    let info_path = trash_dir().join("info").join(format!("{}.trashinfo", name));
    let info = std::fs::read_to_string(&info_path).map_err(|e| e.to_string())?;

    let original = info
        .lines()
        .find_map(|l| l.strip_prefix("Path="))
        .ok_or("no Path= entry in trashinfo")?;
    let original = PathBuf::from(original);

    if let Some(parent) = original.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::rename(trashed_path, &original).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&info_path);
    Ok(original)
}

pub fn compress_paths_zip(paths: &[PathBuf], dest_dir: &Path) -> Result<(), String> {
    let Some(first) = paths.first() else { return Ok(()) };
    let base_name = first.file_name().unwrap_or_default().to_string_lossy().to_string();
    let archive = unique_name(dest_dir, &format!("{}.zip", base_name));
    let parent = first.parent().unwrap_or(dest_dir);

    let mut cmd = std::process::Command::new("zip");
    cmd.arg("-r").arg(&archive).arg("-@").current_dir(parent);
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;

    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        for p in paths {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let _ = writeln!(stdin, "{}", name);
        }
    }
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

fn bookmarks_file() -> PathBuf {
    home_dir().join(".config/og-files/bookmarks.json")
}

pub fn load_bookmarks() -> Vec<PathBuf> {
    std::fs::read_to_string(bookmarks_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<PathBuf>>(&s).ok())
        .unwrap_or_default()
}

pub fn save_bookmarks(bookmarks: &[PathBuf]) {
    if let Some(dir) = bookmarks_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(bookmarks) {
        let _ = std::fs::write(bookmarks_file(), json);
    }
}

// ── Preview panel: image/video thumbnail generation (background, cached) ───────

pub fn is_video(mime_type: &str) -> bool {
    matches!(
        mime_type,
        "video/mp4" | "video/webm" | "video/x-matroska" | "video/quicktime"
            | "video/x-msvideo" | "video/mpeg" | "video/x-flv" | "video/3gpp" | "video/ogg"
    )
}

fn preview_cache_dir() -> PathBuf {
    home_dir().join(".cache/og-files/previews")
}

/// Cache key includes mtime+size so an edited/replaced file gets a fresh
/// thumbnail instead of showing stale cached pixels forever.
fn preview_cache_path(source: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(source).ok()?;
    let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    modified.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    let key = hasher.finish();

    Some(preview_cache_dir().join(format!("{:016x}.png", key)))
}

const PREVIEW_MAX_DIMENSION: u32 = 320;

/// Runs on a background thread (see `app::update_preview`): decodes/extracts
/// a small preview image and caches it, so re-selecting the same file later
/// is instant instead of redoing the work.
pub fn generate_preview_thumbnail(source: &Path, mime_type: &str) -> Result<PathBuf, String> {
    let cache_path = preview_cache_path(source).ok_or("could not stat source file")?;
    if cache_path.exists() {
        return Ok(cache_path);
    }
    if let Some(dir) = cache_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }

    if is_video(mime_type) {
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "quiet", "-i"])
            .arg(source)
            .args([
                "-ss", "00:00:01", "-frames:v", "1",
                "-vf", &format!("scale={0}:{0}:force_original_aspect_ratio=decrease", PREVIEW_MAX_DIMENSION),
            ])
            .arg(&cache_path)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() || !cache_path.exists() {
            return Err("ffmpeg could not extract a frame".to_string());
        }
        return Ok(cache_path);
    }

    if is_genuinely_an_image(source, mime_type) {
        // Sane ceiling so a decompression-bomb-style image can't hang the
        // background thread or blow up memory just to make a 320px preview.
        const MAX_SOURCE_PIXELS: u64 = 100_000_000;
        if let Ok((w, h)) = image::image_dimensions(source) {
            if (w as u64) * (h as u64) > MAX_SOURCE_PIXELS {
                return Err("image too large to preview".to_string());
            }
        }
        let img = image::open(source).map_err(|e| e.to_string())?;
        let thumb = img.thumbnail(PREVIEW_MAX_DIMENSION, PREVIEW_MAX_DIMENSION);
        thumb.save(&cache_path).map_err(|e| e.to_string())?;
        return Ok(cache_path);
    }

    Err("no preview available for this file".to_string())
}
