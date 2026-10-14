//! Archives as folders: zip, tar, tar.gz/tgz, tar.xz/txz, tar.zst, tar.bz2,
//! 7z (read), gz/xz/zst/bz2 single files. Browse the entry tree, open an
//! entry (extracted to the cache on a worker → Event::OpenFile), extract
//! selected / all into a new `<stem>/` folder beside the archive with
//! progress. Extraction refuses zip-slip names (`..`, absolute paths),
//! symlinks pointing outside the folder, and writing through symlinks.

use super::Event;
use crate::fsops;
use crate::theme;
use crate::widgets::{icon, virtual_column, Vp};
use iced::futures::channel::mpsc;
use iced::futures::SinkExt;
use iced::widget::{
    button, checkbox, column, container, mouse_area, progress_bar, row, scrollable, text,
    text_input, Space,
};
use iced::{Element, Length, Task, Theme};
use iced_fonts::Bootstrap;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROW_H: f32 = 24.0;
const TICK: Duration = Duration::from_millis(100);
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
const CANCELLED: &str = "Cancelled";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Comp {
    None,
    Gz,
    Xz,
    Zst,
    Bz2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Zip,
    Tar(Comp),
    SevenZ,
    /// one compressed file, no container
    Single(Comp),
}

/// (suffix, kind), longest first so ".tar.gz" beats ".gz"
const SUFFIXES: &[(&str, Kind)] = &[
    (".tar.gz", Kind::Tar(Comp::Gz)),
    (".tar.xz", Kind::Tar(Comp::Xz)),
    (".tar.zst", Kind::Tar(Comp::Zst)),
    (".tar.bz2", Kind::Tar(Comp::Bz2)),
    (".tgz", Kind::Tar(Comp::Gz)),
    (".txz", Kind::Tar(Comp::Xz)),
    (".tzst", Kind::Tar(Comp::Zst)),
    (".tbz2", Kind::Tar(Comp::Bz2)),
    (".tbz", Kind::Tar(Comp::Bz2)),
    (".tar", Kind::Tar(Comp::None)),
    (".zip", Kind::Zip),
    (".jar", Kind::Zip),
    (".7z", Kind::SevenZ),
    (".gz", Kind::Single(Comp::Gz)),
    (".xz", Kind::Single(Comp::Xz)),
    (".zst", Kind::Single(Comp::Zst)),
    (".bz2", Kind::Single(Comp::Bz2)),
];

fn kind_of(path: &Path) -> Option<(Kind, usize)> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    SUFFIXES
        .iter()
        .find(|(s, _)| name.len() > s.len() && name.ends_with(s))
        .map(|&(s, k)| (k, s.len()))
}

/// The archive's file name without its archive extension(s).
fn stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".into());
    match kind_of(path) {
        Some((_, n)) => name[..name.len() - n].to_string(),
        None => name,
    }
}

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    kind_of(path).is_some()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// normalized: `/`-separated, no leading `/` or `./`, no trailing `/`
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub mtime: Option<SystemTime>,
    pub encrypted: bool,
    pub symlink: bool,
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    pub entries: Arc<Vec<Entry>>,
    kind: Kind,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    tokio::task::spawn_blocking(move || {
        let (kind, _) = kind_of(&path).ok_or("not an archive")?;
        let entries = list(&path, kind)?;
        Ok(Opened {
            path,
            entries: Arc::new(entries),
            kind,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------------ reading

fn decoder<'a>(comp: Comp, r: impl Read + 'a) -> Result<Box<dyn Read + 'a>, String> {
    let r = BufReader::new(r);
    Ok(match comp {
        Comp::None => Box::new(r),
        Comp::Gz => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Comp::Xz => Box::new(xz2::read::XzDecoder::new_multi_decoder(r)),
        Comp::Zst => {
            Box::new(zstd::stream::read::Decoder::with_buffer(r).map_err(|e| e.to_string())?)
        }
        Comp::Bz2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
    })
}

fn norm(name: &str) -> String {
    name.replace('\\', "/")
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn zip_time(d: zip::DateTime) -> Option<SystemTime> {
    let days = days_from_civil(d.year() as i64, d.month() as u32, d.day() as u32);
    let secs = days * 86_400 + d.hour() as i64 * 3600 + d.minute() as i64 * 60 + d.second() as i64;
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

fn zip_err(e: zip::result::ZipError) -> String {
    match e {
        zip::result::ZipError::UnsupportedArchive(m)
            if m == zip::result::ZipError::PASSWORD_REQUIRED =>
        {
            "encrypted entry: enter the password".into()
        }
        zip::result::ZipError::InvalidPassword => "wrong password".into(),
        e => e.to_string(),
    }
}

fn list(path: &Path, kind: Kind) -> Result<Vec<Entry>, String> {
    let file = || File::open(path).map_err(|e| e.to_string());
    let mut out = Vec::new();
    match kind {
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::new(file()?)).map_err(zip_err)?;
            for i in 0..z.len() {
                let f = z.by_index_raw(i).map_err(zip_err)?;
                out.push(Entry {
                    path: norm(f.name()),
                    is_dir: f.is_dir(),
                    size: Some(f.size()),
                    mtime: f.last_modified().and_then(zip_time),
                    encrypted: f.encrypted(),
                    symlink: f.is_symlink(),
                });
            }
        }
        Kind::Tar(comp) => {
            let mut a = tar::Archive::new(decoder(comp, file()?)?);
            for e in a.entries().map_err(|e| e.to_string())? {
                let e = e.map_err(|e| e.to_string())?;
                let h = e.header();
                let t = h.entry_type();
                out.push(Entry {
                    path: norm(&String::from_utf8_lossy(&e.path_bytes())),
                    is_dir: t.is_dir(),
                    size: Some(e.size()),
                    mtime: h.mtime().ok().map(|s| UNIX_EPOCH + Duration::from_secs(s)),
                    encrypted: false,
                    symlink: t.is_symlink(),
                });
            }
        }
        Kind::SevenZ => return list_7z(path, sevenz_rust2::Password::empty()),
        Kind::Single(_) => out.push(Entry {
            path: stem(path),
            is_dir: false,
            size: None,
            mtime: fs::metadata(path).and_then(|m| m.modified()).ok(),
            encrypted: false,
            symlink: false,
        }),
    }
    out.retain(|e| !e.path.is_empty());
    Ok(out)
}

fn list_7z(path: &Path, pw: sevenz_rust2::Password) -> Result<Vec<Entry>, String> {
    let a = sevenz_rust2::Archive::open_with_password(path, &pw).map_err(sz_err)?;
    Ok(a.files
        .iter()
        .map(|f| Entry {
            path: norm(&f.name),
            is_dir: f.is_directory,
            size: (!f.is_directory).then_some(f.size),
            mtime: f
                .has_last_modified_date
                .then(|| SystemTime::from(f.last_modified_date)),
            encrypted: false,
            symlink: false,
        })
        .filter(|e| !e.path.is_empty())
        .collect())
}

// --------------------------------------------------------------- extraction

/// Relative path for an archive member, or None if it would escape the
/// destination (absolute, `..`, drive prefixes) or is empty.
fn safe_rel(raw: &str) -> Option<PathBuf> {
    let raw = raw.replace('\\', "/");
    let p = Path::new(&raw);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Where to write `rel` inside `dest`, refusing to go through (or replace) a
/// symlink created by an earlier member.
fn out_path(dest: &Path, rel: &Path) -> Option<PathBuf> {
    let mut cur = dest.to_path_buf();
    for c in rel.components() {
        cur.push(c);
        if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
            return None;
        }
    }
    Some(cur)
}

/// A symlink at `rel` pointing to `target` stays inside the destination.
fn link_ok(rel: &Path, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || target.contains('\\') {
        return false;
    }
    let mut depth: i64 = rel.components().count() as i64 - 1; // the link's folder
    for c in Path::new(target).components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

#[derive(Debug, Default)]
struct Report {
    files: usize,
    /// members refused as unsafe (or unsupported types)
    skipped: Vec<String>,
}

struct Ctx<'a> {
    dest: &'a Path,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64),
    report: Report,
}

impl Ctx<'_> {
    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    fn target(&mut self, raw: &str) -> Option<(PathBuf, PathBuf)> {
        let r = safe_rel(raw).and_then(|rel| out_path(self.dest, &rel).map(|p| (rel, p)));
        if r.is_none() {
            self.report.skipped.push(raw.to_string());
        }
        r
    }

    fn dir(&mut self, raw: &str) -> Result<(), String> {
        if let Some((_, p)) = self.target(raw) {
            fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        Ok(())
    }

    fn file(&mut self, raw: &str, r: &mut dyn Read, mode: Option<u32>) -> Result<(), String> {
        let Some((_, p)) = self.target(raw) else {
            return Ok(());
        };
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut f = File::create(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            self.check()?;
            let n = r.read(&mut buf).map_err(|e| format!("{raw}: {e}"))?;
            if n == 0 {
                break;
            }
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            (self.progress)(n as u64);
        }
        if let Some(m) = mode {
            use std::os::unix::fs::PermissionsExt;
            let m = m & 0o777;
            if m != 0 {
                let _ = fs::set_permissions(&p, fs::Permissions::from_mode(m | 0o600));
            }
        }
        self.report.files += 1;
        Ok(())
    }

    fn symlink(&mut self, raw: &str, target: &str) -> Result<(), String> {
        let Some((rel, p)) = self.target(raw) else {
            return Ok(());
        };
        if !link_ok(&rel, target) {
            self.report.skipped.push(format!("{raw} -> {target}"));
            return Ok(());
        }
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::os::unix::fs::symlink(target, &p).map_err(|e| format!("{}: {e}", p.display()))?;
        self.report.files += 1;
        Ok(())
    }

    fn hardlink(&mut self, raw: &str, target: &str) -> Result<(), String> {
        let src = safe_rel(target).and_then(|rel| out_path(self.dest, &rel));
        match (src, self.target(raw)) {
            (Some(src), Some((_, p))) if src.is_file() => {
                fs::copy(&src, &p).map_err(|e| e.to_string())?;
                self.report.files += 1;
            }
            (_, Some(_)) => self.report.skipped.push(format!("{raw} (hard link)")),
            _ => {}
        }
        Ok(())
    }
}

/// Extract the members whose normalized path passes `want` into `dest`.
fn extract(
    path: &Path,
    kind: Kind,
    want: &dyn Fn(&str) -> bool,
    dest: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Report, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut cx = Ctx {
        dest,
        cancel,
        progress,
        report: Report::default(),
    };
    let file = File::open(path).map_err(|e| e.to_string())?;
    match kind {
        Kind::Zip => {
            let mut z = zip::ZipArchive::new(BufReader::new(file)).map_err(zip_err)?;
            for i in 0..z.len() {
                cx.check()?;
                let raw = z.name_for_index(i).unwrap_or_default().to_string();
                if !want(&norm(&raw)) {
                    continue;
                }
                let mut f = match password.filter(|p| !p.is_empty()) {
                    Some(pw) => z.by_index_decrypt(i, pw.as_bytes()),
                    None => z.by_index(i),
                }
                .map_err(|e| format!("{raw}: {}", zip_err(e)))?;
                if f.is_dir() {
                    cx.dir(&raw)?;
                } else if f.is_symlink() {
                    let mut t = String::new();
                    f.read_to_string(&mut t).map_err(|e| e.to_string())?;
                    cx.symlink(&raw, &t)?;
                } else {
                    let mode = f.unix_mode();
                    cx.file(&raw, &mut f, mode)?;
                }
            }
        }
        Kind::Tar(comp) => extract_tar(decoder(comp, file)?, want, &mut cx)?,
        Kind::SevenZ => {
            drop(file);
            let pw = match password.filter(|p| !p.is_empty()) {
                Some(p) => sevenz_rust2::Password::from(p),
                None => sevenz_rust2::Password::empty(),
            };
            let mut ar = sevenz_rust2::ArchiveReader::open(path, pw).map_err(sz_err)?;
            let mut err: Option<String> = None;
            ar.for_each_entries(|entry, r| {
                let step = (|| -> Result<(), String> {
                    cx.check()?;
                    if !want(&norm(&entry.name)) {
                        io::copy(r, &mut io::sink()).map_err(|e| e.to_string())?;
                        return Ok(());
                    }
                    if entry.is_directory {
                        cx.dir(&entry.name)
                    } else {
                        cx.file(&entry.name, r, None)
                    }
                })();
                match step {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        err = Some(e);
                        Ok(false)
                    }
                }
            })
            .map_err(sz_err)?;
            if let Some(e) = err {
                return Err(e);
            }
        }
        Kind::Single(_) => return Err("single-file archive: use extract_single".into()),
    }
    Ok(cx.report)
}

/// Unpack a (decompressed) tar stream through `cx`.
fn extract_tar(r: impl Read, want: &dyn Fn(&str) -> bool, cx: &mut Ctx) -> Result<(), String> {
    let mut a = tar::Archive::new(r);
    for e in a.entries().map_err(|e| e.to_string())? {
        cx.check()?;
        let mut e = e.map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(&e.path_bytes()).into_owned();
        if !want(&norm(&raw)) {
            continue;
        }
        let t = e.header().entry_type();
        let link = || -> String {
            e.link_name_bytes()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        if t.is_dir() {
            cx.dir(&raw)?;
        } else if t.is_symlink() {
            let l = link();
            cx.symlink(&raw, &l)?;
        } else if t.is_hard_link() {
            let l = link();
            cx.hardlink(&raw, &l)?;
        } else if t.is_file() || t.is_contiguous() || t.is_gnu_sparse() {
            let mode = e.header().mode().ok();
            cx.file(&raw, &mut e, mode)?;
        } else {
            cx.report.skipped.push(format!("{raw} (special file)"));
        }
    }
    Ok(())
}

/// 7z errors in words (password problems mention "password").
fn sz_err(e: sevenz_rust2::Error) -> String {
    match e {
        sevenz_rust2::Error::PasswordRequired => "encrypted archive: enter the password".into(),
        sevenz_rust2::Error::MaybeBadPassword(_) => "wrong password (or damaged archive)".into(),
        e => e.to_string(),
    }
}

/// Decompress a single-file .gz/.xz/.zst/.bz2 to `out`.
fn extract_single(
    path: &Path,
    comp: Comp,
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let mut r = decoder(comp, File::open(path).map_err(|e| e.to_string())?)?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut f = File::create(out).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(f);
            let _ = fs::remove_file(out);
            return Err(CANCELLED.into());
        }
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress(n as u64);
    }
}

/// Cache folder for entries opened from `archive`.
fn cache_dir_for(archive: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    archive.hash(&mut h);
    if let Ok(m) = fs::metadata(archive) {
        m.len().hash(&mut h);
        m.modified().ok().hash(&mut h);
    }
    crate::config::cache_dir()
        .join("archive")
        .join(format!("{:016x}", h.finish()))
}

// ------------------------------------------------------------------ compress

/// Formats "Compress" (context menu and `og-files compress`) can write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
    TarZst,
    TarXz,
    SevenZ,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Zip,
        Format::TarGz,
        Format::TarZst,
        Format::TarXz,
        Format::SevenZ,
    ];

    /// File-name suffix, with the dot.
    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarZst => ".tar.zst",
            Format::TarXz => ".tar.xz",
            Format::SevenZ => ".7z",
        }
    }

    /// From an output file name (`.tgz` / `.tzst` / `.txz` count too).
    pub fn from_name(path: &Path) -> Option<Format> {
        match kind_of(path)?.0 {
            Kind::Zip => Some(Format::Zip),
            Kind::Tar(Comp::Gz) => Some(Format::TarGz),
            Kind::Tar(Comp::Zst) => Some(Format::TarZst),
            Kind::Tar(Comp::Xz) => Some(Format::TarXz),
            Kind::SevenZ => Some(Format::SevenZ),
            _ => None,
        }
    }

    /// `name` without this format's suffix (case-insensitive), if it has it.
    pub fn strip(self, name: &str) -> Option<String> {
        let ext = self.ext();
        (name.len() > ext.len() && name.to_ascii_lowercase().ends_with(ext))
            .then(|| name[..name.len() - ext.len()].to_string())
    }
}

/// Create a zip (or .tar.gz…, by `dest`'s name) without progress.
#[cfg(test)]
pub fn compress(sources: &[PathBuf], dest: &Path) -> Result<(), String> {
    let f = Format::from_name(dest).unwrap_or(Format::Zip);
    compress_with(sources, dest, f, &AtomicBool::new(false), &mut |_| {})
}

/// Bytes of the regular files under `sources` (symlinks not followed):
/// the progress total of [`compress_with`].
pub fn source_bytes(sources: &[PathBuf], cancel: &AtomicBool) -> u64 {
    fn walk(p: &Path, cancel: &AtomicBool) -> u64 {
        if cancel.load(Ordering::Relaxed) {
            return 0;
        }
        match fs::symlink_metadata(p) {
            Ok(m) if m.is_dir() => fs::read_dir(p)
                .map(|rd| rd.flatten().map(|e| walk(&e.path(), cancel)).sum())
                .unwrap_or(0),
            Ok(m) if m.is_file() => m.len(),
            _ => 0,
        }
    }
    sources.iter().map(|s| walk(s, cancel)).sum()
}

/// Counts bytes read through it and stops with `Cancelled` when asked.
struct Counting<'a, R> {
    inner: R,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64),
}

impl<R: Read> Read for Counting<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::other(CANCELLED));
        }
        let n = self.inner.read(buf)?;
        (self.progress)(n as u64);
        Ok(n)
    }
}

/// Progress + cancel shared by the compressors.
struct Pack<'a> {
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64),
    /// the archive being written (never packed into itself)
    skip: PathBuf,
}

impl Pack<'_> {
    fn check(&self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(CANCELLED.into())
        } else {
            Ok(())
        }
    }

    fn reader<R: Read>(&mut self, inner: R) -> Counting<'_, R> {
        Counting {
            inner,
            cancel: self.cancel,
            progress: &mut *self.progress,
        }
    }

    fn children(&self, dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
        let mut v = sorted_children(dir)?;
        v.retain(|(_, c)| !fs::canonicalize(c).is_ok_and(|c| c == self.skip));
        Ok(v)
    }
}

/// Pack files and folders (recursive) into `dest` as `format`, reporting
/// bytes read through `progress`. Paths inside the archive are relative to
/// each source's parent folder. A failed or cancelled run removes `dest`.
/// Blocking. 7z stores regular files and folders only (symlinks and
/// special files are left out).
pub fn compress_with(
    sources: &[PathBuf],
    dest: &Path,
    format: Format,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    if sources.is_empty() {
        return Err("nothing to compress".into());
    }
    let r = (|| {
        let f = File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        let mut pk = Pack {
            cancel,
            progress,
            skip: fs::canonicalize(dest).unwrap_or_else(|_| dest.to_path_buf()),
        };
        match format {
            Format::Zip => compress_zip(f, sources, &mut pk),
            Format::TarGz => {
                let gz = flate2::write::GzEncoder::new(
                    io::BufWriter::new(f),
                    flate2::Compression::default(),
                );
                let gz = write_tar(gz, sources, &mut pk)?;
                gz.finish()
                    .and_then(|mut w| w.flush())
                    .map_err(|e| e.to_string())
            }
            Format::TarZst => {
                let z = zstd::stream::write::Encoder::new(io::BufWriter::new(f), 0)
                    .map_err(|e| e.to_string())?;
                let z = write_tar(z, sources, &mut pk)?;
                z.finish()
                    .and_then(|mut w| w.flush())
                    .map_err(|e| e.to_string())
            }
            Format::TarXz => {
                let x = xz2::write::XzEncoder::new(io::BufWriter::new(f), 6);
                let x = write_tar(x, sources, &mut pk)?;
                x.finish()
                    .and_then(|mut w| w.flush())
                    .map_err(|e| e.to_string())
            }
            Format::SevenZ => compress_7z(f, sources, &mut pk),
        }
    })();
    let r = r.map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            CANCELLED.to_string()
        } else {
            e
        }
    });
    if r.is_err() {
        let _ = fs::remove_file(dest);
    }
    r
}

fn src_name(src: &Path) -> Result<String, String> {
    src.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| format!("{}: no file name", src.display()))
}

fn write_tar<W: Write>(w: W, sources: &[PathBuf], pk: &mut Pack) -> Result<W, String> {
    let mut b = tar::Builder::new(w);
    b.follow_symlinks(false);
    for src in sources {
        let name = src_name(src)?;
        add_tar(&mut b, src, Path::new(&name), pk)?;
    }
    b.into_inner().map_err(|e| e.to_string())
}

fn add_tar<W: Write>(
    b: &mut tar::Builder<W>,
    abs: &Path,
    rel: &Path,
    pk: &mut Pack,
) -> Result<(), String> {
    pk.check()?;
    let err = |e: io::Error| format!("{}: {e}", abs.display());
    let m = fs::symlink_metadata(abs).map_err(err)?;
    if m.is_dir() {
        b.append_dir(rel, abs).map_err(err)?;
        for (name, child) in pk.children(abs)? {
            add_tar(b, &child, &rel.join(name), pk)?;
        }
        Ok(())
    } else if m.is_file() {
        let mut h = tar::Header::new_gnu();
        h.set_metadata(&m);
        let f = File::open(abs).map_err(err)?;
        b.append_data(&mut h, rel, pk.reader(f)).map_err(err)
    } else {
        b.append_path_with_name(abs, rel).map_err(err)
    }
}

fn sorted_children(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut v: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    v.sort();
    Ok(v)
}

fn compress_zip(f: File, sources: &[PathBuf], pk: &mut Pack) -> Result<(), String> {
    let mut z = zip::ZipWriter::new(io::BufWriter::new(f));
    for src in sources {
        let name = src_name(src)?;
        add_zip(&mut z, src, &name, pk)?;
    }
    z.finish()
        .map_err(|e| e.to_string())?
        .flush()
        .map_err(|e| e.to_string())
}

fn add_zip<W: Write + io::Seek>(
    z: &mut zip::ZipWriter<W>,
    abs: &Path,
    rel: &str,
    pk: &mut Pack,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    pk.check()?;
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", abs.display());
    let m = fs::symlink_metadata(abs).map_err(|e| err(&e))?;
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(m.permissions().mode() & 0o7777)
        .large_file(m.len() >= u32::MAX as u64);
    if m.file_type().is_symlink() {
        let t = fs::read_link(abs).map_err(|e| err(&e))?;
        z.add_symlink(rel, t.to_string_lossy(), opts)
            .map_err(|e| err(&e))
    } else if m.is_dir() {
        z.add_directory(format!("{rel}/"), opts)
            .map_err(|e| err(&e))?;
        for (name, child) in pk.children(abs)? {
            add_zip(z, &child, &format!("{rel}/{name}"), pk)?;
        }
        Ok(())
    } else {
        z.start_file(rel, opts).map_err(|e| err(&e))?;
        let f = File::open(abs).map_err(|e| err(&e))?;
        io::copy(&mut pk.reader(f), z).map_err(|e| err(&e))?;
        Ok(())
    }
}

fn compress_7z(f: File, sources: &[PathBuf], pk: &mut Pack) -> Result<(), String> {
    let mut w = sevenz_rust2::ArchiveWriter::new(f).map_err(|e| e.to_string())?;
    for src in sources {
        let name = src_name(src)?;
        add_7z(&mut w, src, &name, pk)?;
    }
    w.finish().map(|_| ()).map_err(|e| e.to_string())
}

fn add_7z(
    w: &mut sevenz_rust2::ArchiveWriter<File>,
    abs: &Path,
    rel: &str,
    pk: &mut Pack,
) -> Result<(), String> {
    use sevenz_rust2::ArchiveEntry;
    pk.check()?;
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", abs.display());
    let m = fs::symlink_metadata(abs).map_err(|e| err(&e))?;
    if m.is_dir() {
        let mut e = ArchiveEntry::from_path(abs, rel.to_string());
        e.is_directory = true;
        w.push_archive_entry::<File>(e, None).map_err(|e| err(&e))?;
        for (name, child) in pk.children(abs)? {
            add_7z(w, &child, &format!("{rel}/{name}"), pk)?;
        }
    } else if m.is_file() {
        let f = File::open(abs).map_err(|e| err(&e))?;
        let e = ArchiveEntry::from_path(abs, rel.to_string());
        w.push_archive_entry(e, Some(pk.reader(f)))
            .map_err(|e| err(&e))?;
    }
    Ok(())
}

// ------------------------------------------------- extract here / extract to

/// Result of [`extract_here`] / [`extract_to`].
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    /// the new top-level item (folder, or the lone file)
    pub out: PathBuf,
    pub files: usize,
    /// members refused as unsafe or unsupported
    pub skipped: Vec<String>,
}

/// Does this error mean "ask for a (different) password"?
pub fn is_password_error(e: &str) -> bool {
    e.to_ascii_lowercase().contains("password")
}

/// The archive's file name without its archive extension(s).
pub fn base_name(path: &Path) -> String {
    stem(path)
}

/// Entries of `path` (a password opens 7z archives with encrypted headers).
pub fn list_entries(path: &Path, password: Option<&str>) -> Result<Vec<Entry>, String> {
    let (kind, _) = kind_of(path).ok_or("not a supported archive")?;
    match (kind, password.filter(|p| !p.is_empty())) {
        (Kind::SevenZ, Some(pw)) => list_7z(path, sevenz_rust2::Password::from(pw)),
        _ => list(path, kind),
    }
}

/// Progress total for [`extract_here`] / [`extract_to`]: uncompressed bytes
/// for zip/7z (cheap to list), the archive's own size for tar and single
/// files (whose progress counts compressed bytes read).
pub fn extract_total(path: &Path, password: Option<&str>) -> u64 {
    match kind_of(path).map(|k| k.0) {
        Some(Kind::Zip | Kind::SevenZ) => list_entries(path, password)
            .map(|es| es.iter().filter_map(|e| e.size).sum())
            .unwrap_or(0),
        Some(_) => fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        None => 0,
    }
}

fn run_extract(
    archive: &Path,
    kind: Kind,
    dest: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Report, String> {
    match kind {
        // count compressed bytes: the uncompressed total isn't known
        // without a second full decompression pass
        Kind::Tar(comp) => {
            let f = File::open(archive).map_err(|e| e.to_string())?;
            let counted = Counting {
                inner: f,
                cancel,
                progress,
            };
            fs::create_dir_all(dest).map_err(|e| e.to_string())?;
            let mut cx = Ctx {
                dest,
                cancel,
                progress: &mut |_| {},
                report: Report::default(),
            };
            extract_tar(decoder(comp, counted)?, &|_| true, &mut cx).map(|()| cx.report)
        }
        _ => extract(archive, kind, &|_| true, dest, password, cancel, progress),
    }
    .map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            CANCELLED.to_string()
        } else {
            e
        }
    })
}

/// "Extract here" into folder `parent`: an archive holding one top-level
/// item (the usual `project-1.2/`) puts that item straight into `parent`;
/// anything else goes into a new folder named after the archive. Taken
/// names get " (2)" etc. Works in a hidden staging folder inside `parent`
/// that is removed on failure or cancel, so nothing half-done is left.
pub fn extract_here(
    archive: &Path,
    parent: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Extracted, String> {
    let (kind, _) = kind_of(archive).ok_or("not a supported archive")?;
    let name = stem(archive);
    if let Kind::Single(c) = kind {
        let out = crate::ops::unique_name(parent, &name);
        single_counted(archive, c, &out, cancel, progress)?;
        return Ok(Extracted {
            out,
            files: 1,
            skipped: Vec::new(),
        });
    }
    let stage = crate::ops::unique_name(parent, &format!(".{name}.og-extract"));
    let rep = match run_extract(archive, kind, &stage, password, cancel, progress) {
        Ok(r) => r,
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
    };
    let finish = || -> Result<PathBuf, String> {
        let mut tops: Vec<PathBuf> = fs::read_dir(&stage)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .collect();
        if tops.len() == 1 {
            let item = tops.remove(0);
            let n = item.file_name().unwrap_or_default().to_string_lossy();
            let out = crate::ops::unique_name(parent, &n);
            fs::rename(&item, &out).map_err(|e| e.to_string())?;
            let _ = fs::remove_dir(&stage);
            Ok(out)
        } else {
            let out = crate::ops::unique_name(parent, &name);
            fs::rename(&stage, &out).map_err(|e| e.to_string())?;
            Ok(out)
        }
    };
    match finish() {
        Ok(out) => Ok(Extracted {
            out,
            files: rep.files,
            skipped: rep.skipped,
        }),
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            Err(e)
        }
    }
}

/// Extract everything into folder `dest` (created; removed again on
/// failure if this call created it).
pub fn extract_to(
    archive: &Path,
    dest: &Path,
    password: Option<&str>,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Extracted, String> {
    let (kind, _) = kind_of(archive).ok_or("not a supported archive")?;
    let existed = dest.exists();
    let r = match kind {
        Kind::Single(c) => {
            let out = dest.join(stem(archive));
            single_counted(archive, c, &out, cancel, progress).map(|()| Report {
                files: 1,
                skipped: Vec::new(),
            })
        }
        _ => run_extract(archive, kind, dest, password, cancel, progress),
    };
    match r {
        Ok(rep) => Ok(Extracted {
            out: dest.to_path_buf(),
            files: rep.files,
            skipped: rep.skipped,
        }),
        Err(e) => {
            if !existed {
                let _ = fs::remove_dir_all(dest);
            }
            Err(e)
        }
    }
}

/// `extract_single`, but progress counts compressed bytes read.
fn single_counted(
    archive: &Path,
    comp: Comp,
    out: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), String> {
    let f = File::open(archive).map_err(|e| e.to_string())?;
    let mut r = decoder(
        comp,
        Counting {
            inner: f,
            cancel,
            progress,
        },
    )?;
    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut o = File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    if let Err(e) = io::copy(&mut r, &mut o) {
        drop(o);
        let _ = fs::remove_file(out);
        return Err(if cancel.load(Ordering::Relaxed) {
            CANCELLED.into()
        } else {
            e.to_string()
        });
    }
    Ok(())
}

// --------------------------------------------------------------------- tree

#[derive(Debug, Clone)]
struct Node {
    name: String,
    path: String,
    dir: bool,
    size: Option<u64>,
    mtime: Option<SystemTime>,
    encrypted: bool,
    children: Vec<usize>,
}

/// Node 0 is the root; folders implied by member paths are synthesized.
fn build_tree(entries: &[Entry]) -> Vec<Node> {
    let mut nodes = vec![Node {
        name: String::new(),
        path: String::new(),
        dir: true,
        size: None,
        mtime: None,
        encrypted: false,
        children: Vec::new(),
    }];
    let mut index: HashMap<String, usize> = HashMap::new();
    for e in entries {
        let mut parent = 0;
        let mut path = String::new();
        let parts: Vec<&str> = e.path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            let last = i + 1 == parts.len();
            let id = match index.get(&path) {
                Some(&id) => id,
                None => {
                    let id = nodes.len();
                    nodes.push(Node {
                        name: part.to_string(),
                        path: path.clone(),
                        dir: !last || e.is_dir,
                        size: None,
                        mtime: None,
                        encrypted: false,
                        children: Vec::new(),
                    });
                    nodes[parent].children.push(id);
                    index.insert(path.clone(), id);
                    id
                }
            };
            if last {
                let n = &mut nodes[id];
                n.dir |= e.is_dir;
                n.mtime = e.mtime.or(n.mtime);
                n.encrypted = e.encrypted;
                if !e.is_dir {
                    n.size = e.size;
                }
            }
            parent = id;
        }
    }
    // folders first, natural order; folder sizes = sum of contents
    fn finish(nodes: &mut Vec<Node>, id: usize) -> Option<u64> {
        let mut kids = std::mem::take(&mut nodes[id].children);
        kids.sort_by(|&a, &b| {
            nodes[b]
                .dir
                .cmp(&nodes[a].dir)
                .then_with(|| fsops::natural_cmp(&nodes[a].name, &nodes[b].name))
        });
        if nodes[id].dir {
            let mut total = Some(0u64);
            for &k in &kids {
                let s = finish(nodes, k);
                total = match (total, s) {
                    (Some(t), Some(s)) => Some(t + s),
                    _ => None,
                };
            }
            nodes[id].size = total;
        }
        nodes[id].children = kids;
        nodes[id].size
    }
    finish(&mut nodes, 0);
    nodes
}

// -------------------------------------------------------------------- state

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JobKind {
    Open,
    Extract,
}

struct Job {
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    total: u64,
    done: u64,
}

pub struct State {
    path: PathBuf,
    kind: Kind,
    entries: Arc<Vec<Entry>>,
    nodes: Vec<Node>,
    expanded: HashSet<usize>,
    /// (node, depth) rows currently shown
    visible: Vec<(usize, u16)>,
    selected: BTreeSet<usize>,
    cursor: Option<usize>,
    last_click: Option<(usize, Instant)>,
    password: String,
    job: Option<Job>,
    vp: Vp,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Scrolled(scrollable::Viewport),
    Click(usize),
    Check(usize, bool),
    Toggle(usize),
    OpenEntry,
    ExtractSelected,
    ExtractAll,
    Password(String),
    Progress(u64),
    JobDone(JobKind, Result<(PathBuf, String), String>),
    Cancel,
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let nodes = build_tree(&o.entries);
        let mut s = Self {
            path: o.path,
            kind: o.kind,
            entries: o.entries,
            nodes,
            expanded: HashSet::new(),
            visible: Vec::new(),
            selected: BTreeSet::new(),
            cursor: None,
            last_click: None,
            password: String::new(),
            job: None,
            vp: Vp::default(),
        };
        // a lone top folder (the usual "project-1.2/") opens expanded
        if let [only] = s.nodes[0].children[..] {
            if s.nodes[only].dir {
                s.expanded.insert(only);
            }
        }
        s.relayout();
        (s, Task::none())
    }

    fn relayout(&mut self) {
        fn walk(s: &State, id: usize, depth: u16, out: &mut Vec<(usize, u16)>) {
            for &k in &s.nodes[id].children {
                out.push((k, depth));
                if s.expanded.contains(&k) {
                    walk(s, k, depth + 1, out);
                }
            }
        }
        let mut v = Vec::new();
        walk(self, 0, 0, &mut v);
        self.visible = v;
    }

    fn has_encrypted(&self) -> bool {
        self.entries.iter().any(|e| e.encrypted)
    }

    fn password(&self) -> Option<String> {
        (!self.password.is_empty()).then(|| self.password.clone())
    }

    fn total_of(&self, want: &dyn Fn(&str) -> bool) -> u64 {
        self.entries
            .iter()
            .filter(|e| !e.is_dir && want(&e.path))
            .filter_map(|e| e.size)
            .sum()
    }

    /// Run `work` on a blocking worker, streaming progress back.
    fn run(
        &mut self,
        kind: JobKind,
        total: u64,
        work: impl FnOnce(&AtomicBool, &mut dyn FnMut(u64)) -> Result<(PathBuf, String), String>
            + Send
            + 'static,
    ) -> Task<Msg> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(Job {
            kind,
            cancel: cancel.clone(),
            total,
            done: 0,
        });
        let stream = iced::stream::channel(32, move |mut tx: mpsc::Sender<Msg>| async move {
            let mut ptx = tx.clone();
            let r = tokio::task::spawn_blocking(move || {
                let (mut done, mut last) = (0u64, Instant::now());
                let mut prog = |n: u64| {
                    done += n;
                    if last.elapsed() >= TICK {
                        last = Instant::now();
                        let _ = ptx.try_send(Msg::Progress(done));
                    }
                };
                work(&cancel, &mut prog)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            let _ = tx.send(Msg::JobDone(kind, r)).await;
        });
        Task::run(stream, |m| m)
    }

    fn open_node(&mut self, id: usize) -> Task<Msg> {
        if self.job.is_some() {
            return Task::none();
        }
        let n = &self.nodes[id];
        if n.dir {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
            self.relayout();
            return Task::none();
        }
        let (archive, kind, member, pw) = (
            self.path.clone(),
            self.kind,
            n.path.clone(),
            self.password(),
        );
        let total = n.size.unwrap_or(0);
        self.run(JobKind::Open, total, move |cancel, prog| {
            let dir = cache_dir_for(&archive);
            let rel = safe_rel(&member).ok_or("unsafe entry name")?;
            let out = dir.join(&rel);
            if !out.is_file() {
                match kind {
                    Kind::Single(c) => extract_single(&archive, c, &out, cancel, prog)?,
                    _ => {
                        let want = |p: &str| p == member;
                        let r = extract(&archive, kind, &want, &dir, pw.as_deref(), cancel, prog)?;
                        if r.files == 0 {
                            return Err(match r.skipped.first() {
                                Some(s) => format!("refused unsafe entry {s}"),
                                None => "entry not found".into(),
                            });
                        }
                    }
                }
            }
            Ok((out, String::new()))
        })
    }

    fn extract_to_folder(&mut self, only: Option<Vec<String>>) -> Task<Msg> {
        if self.job.is_some() {
            return Task::none();
        }
        let want = move |p: &str| match &only {
            None => true,
            Some(sel) => sel.iter().any(|s| {
                p == s
                    || (p.len() > s.len()
                        && p.starts_with(s.as_str())
                        && p.as_bytes()[s.len()] == b'/')
            }),
        };
        let total = self.total_of(&want);
        let (archive, kind, pw) = (self.path.clone(), self.kind, self.password());
        let parent = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let name = stem(&self.path);
        self.run(JobKind::Extract, total, move |cancel, prog| {
            if let Kind::Single(c) = kind {
                let out = crate::ops::unique_name(&parent, &name);
                extract_single(&archive, c, &out, cancel, prog)?;
                return Ok((out, String::new()));
            }
            let dest = crate::ops::unique_name(&parent, &name);
            let r = extract(&archive, kind, &want, &dest, pw.as_deref(), cancel, prog);
            match r {
                Ok(r) => {
                    let note = if r.skipped.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " — skipped {} unsafe/unsupported: {}",
                            r.skipped.len(),
                            r.skipped
                                .iter()
                                .take(3)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    Ok((dest, format!("{} files{note}", r.files)))
                }
                Err(e) => {
                    if e == CANCELLED {
                        let _ = fs::remove_dir_all(&dest);
                    }
                    Err(e)
                }
            }
        })
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Scrolled(v) => {
                self.vp = Vp::from(v);
                none
            }
            Msg::Click(id) => {
                let now = Instant::now();
                let double = self
                    .last_click
                    .is_some_and(|(n, t)| n == id && now.duration_since(t) < DOUBLE_CLICK);
                self.last_click = Some((id, now));
                self.cursor = Some(id);
                self.selected.clear();
                self.selected.insert(id);
                if double {
                    self.last_click = None;
                    return (self.open_node(id), None);
                }
                none
            }
            Msg::Check(id, on) => {
                if on {
                    self.selected.insert(id);
                } else {
                    self.selected.remove(&id);
                }
                none
            }
            Msg::Toggle(id) => (self.open_node(id), None),
            Msg::OpenEntry => match self.cursor {
                Some(id) => (self.open_node(id), None),
                None => none,
            },
            Msg::ExtractSelected => {
                if self.selected.is_empty() {
                    return none;
                }
                let sel = self
                    .selected
                    .iter()
                    .map(|&i| self.nodes[i].path.clone())
                    .collect();
                (
                    self.extract_to_folder(Some(sel)),
                    Some(Event::Status("Extracting…".into())),
                )
            }
            Msg::ExtractAll => (
                self.extract_to_folder(None),
                Some(Event::Status("Extracting…".into())),
            ),
            Msg::Password(p) => {
                self.password = p;
                none
            }
            Msg::Progress(d) => {
                if let Some(j) = &mut self.job {
                    j.done = d;
                }
                none
            }
            Msg::JobDone(kind, r) => {
                self.job = None;
                match (kind, r) {
                    (JobKind::Open, Ok((p, _))) => (Task::none(), Some(Event::OpenFile(p))),
                    (JobKind::Extract, Ok((p, note))) => (
                        Task::none(),
                        Some(Event::Status(format!(
                            "Extracted to {}{}",
                            p.display(),
                            if note.is_empty() {
                                String::new()
                            } else {
                                format!(" ({note})")
                            }
                        ))),
                    ),
                    (_, Err(e)) => (
                        Task::none(),
                        Some(Event::Status(if e == CANCELLED {
                            "Cancelled".into()
                        } else {
                            format!("Archive: {e}")
                        })),
                    ),
                }
            }
            Msg::Cancel => {
                if let Some(j) = &self.job {
                    j.cancel.store(true, Ordering::Relaxed);
                }
                none
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let files = self.entries.iter().filter(|e| !e.is_dir).count();
        let size: u64 = self.entries.iter().filter_map(|e| e.size).sum();
        let busy = self.job.is_some();
        let can_open = self.cursor.is_some_and(|c| !self.nodes[c].dir);
        let mut bar = row![
            text(format!(
                "{files} files · {}",
                if matches!(self.kind, Kind::Single(_)) {
                    "compressed file".to_string()
                } else {
                    fsops::human_size(size)
                }
            ))
            .size(12)
            .color(theme::MUTED),
            Space::with_width(Length::Fill),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        if self.has_encrypted() || self.kind == Kind::SevenZ {
            bar = bar.push(
                text_input(
                    if self.has_encrypted() {
                        "Password (encrypted entries)"
                    } else {
                        "Password (if encrypted)"
                    },
                    &self.password,
                )
                .secure(true)
                .size(12)
                .width(200)
                .on_input(Msg::Password),
            );
        }
        bar = bar
            .push(
                button(text("Open").size(12))
                    .on_press_maybe((can_open && !busy).then_some(Msg::OpenEntry)),
            )
            .push(
                button(text(format!("Extract selected ({})", self.selected.len())).size(12))
                    .style(button::secondary)
                    .on_press_maybe(
                        (!self.selected.is_empty() && !busy).then_some(Msg::ExtractSelected),
                    ),
            )
            .push(
                button(text("Extract all").size(12))
                    .style(button::secondary)
                    .on_press_maybe((!busy).then_some(Msg::ExtractAll)),
            );

        let mut col = column![bar].spacing(6);
        if let Some(j) = &self.job {
            let frac = if j.total > 0 {
                (j.done as f32 / j.total as f32).min(1.0)
            } else {
                0.0
            };
            col = col.push(
                row![
                    text(match j.kind {
                        JobKind::Open => "Opening entry…",
                        JobKind::Extract => "Extracting…",
                    })
                    .size(12),
                    progress_bar(0.0..=1.0, frac).height(8).width(Length::Fill),
                    text(if j.total > 0 {
                        format!(
                            "{} / {}",
                            fsops::human_size(j.done),
                            fsops::human_size(j.total)
                        )
                    } else {
                        fsops::human_size(j.done)
                    })
                    .size(12)
                    .color(theme::MUTED),
                    button(text("Cancel").size(12))
                        .style(button::danger)
                        .on_press(Msg::Cancel),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        let head = row![
            Space::with_width(26),
            text("Name")
                .size(12)
                .color(theme::MUTED)
                .width(Length::Fill),
            text("Size").size(12).color(theme::MUTED).width(90),
            text("Modified").size(12).color(theme::MUTED).width(140),
        ]
        .padding([0, 6]);
        let list = virtual_column(self.visible.len(), ROW_H, self.vp, |i| {
            let (id, depth) = self.visible[i];
            self.row_el(id, depth)
        });
        let list = scrollable(list)
            .on_scroll(Msg::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);
        col.push(head).push(list).into()
    }

    fn row_el(&self, id: usize, depth: u16) -> Element<'_, Msg> {
        let n = &self.nodes[id];
        let chevron: Element<'_, Msg> = if n.dir {
            mouse_area(
                icon(if self.expanded.contains(&id) {
                    Bootstrap::ChevronDown
                } else {
                    Bootstrap::ChevronRight
                })
                .size(11),
            )
            .on_press(Msg::Toggle(id))
            .into()
        } else {
            Space::with_width(11).into()
        };
        let glyph = if n.dir {
            icon(Bootstrap::FolderFill).color(iced::color!(0xe0b85c))
        } else {
            icon(Bootstrap::FileEarmark).color(theme::MUTED)
        };
        let name = if n.encrypted {
            format!("{}  (encrypted)", n.name)
        } else {
            n.name.clone()
        };
        let selected = self.selected.contains(&id);
        let focused = self.cursor == Some(id);
        let content = row![
            Space::with_width(depth as f32 * 16.0),
            chevron,
            glyph.size(13),
            text(name).size(13).width(Length::Fill),
            text(n.size.map(fsops::human_size).unwrap_or_else(|| "—".into()))
                .size(12)
                .color(theme::MUTED)
                .width(90),
            text(fmt_time(n.mtime))
                .size(12)
                .color(theme::MUTED)
                .width(140),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center);
        let body = container(content)
            .padding([2, 6])
            .width(Length::Fill)
            .style(move |t: &Theme| row_style(t, selected, focused));
        row![
            checkbox("", selected)
                .size(14)
                .on_toggle(move |on| Msg::Check(id, on)),
            mouse_area(body).on_press(Msg::Click(id)),
        ]
        .align_y(iced::Alignment::Center)
        .into()
    }

    /// Unsaved edits exist (tab close asks first). Archives are read-only.
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        None
    }

    /// Ctrl+E: no edit mode.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        Task::none()
    }

    /// Ctrl+S: nothing to save.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}

fn row_style(t: &Theme, selected: bool, focused: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: selected.then(|| p.primary.weak.color.scale_alpha(0.45).into()),
        border: iced::Border {
            radius: 4.0.into(),
            width: if focused { 1.0 } else { 0.0 },
            color: p.primary.base.color.scale_alpha(0.7),
        },
        ..Default::default()
    }
}

fn fmt_time(t: Option<SystemTime>) -> String {
    let Some(secs) = t
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
    else {
        return "—".into();
    };
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", s / 3600, s / 60 % 60)
}

/// (year, month, day) → days since 1970-01-01 (Howard Hinnant).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = crate::config::home()
                .join(".cache/ogx-p2-sheet-tests")
                .join(format!(
                    "archive-{tag}-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::SeqCst)
                ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
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

    fn tree(t: &Tmp) -> Vec<PathBuf> {
        write(&t.p("src/proj/a.txt"), "alpha");
        write(&t.p("src/proj/sub/b.txt"), "bravo");
        write(&t.p("src/proj/sub/deep/c.txt"), "charlie");
        fs::create_dir_all(t.p("src/proj/empty")).unwrap();
        write(&t.p("src/loose.md"), "loose");
        vec![t.p("src/proj"), t.p("src/loose.md")]
    }

    fn all(_: &str) -> bool {
        true
    }

    fn extract_all(path: &Path, dest: &Path) -> Report {
        let (kind, _) = kind_of(path).unwrap();
        let no = AtomicBool::new(false);
        extract(path, kind, &all, dest, None, &no, &mut |_| {}).unwrap()
    }

    fn check_round_trip(archive: &Path, t: &Tmp) {
        let (kind, _) = kind_of(archive).unwrap();
        let entries = list(archive, kind).unwrap();
        let paths: BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        for p in [
            "proj",
            "proj/a.txt",
            "proj/sub/b.txt",
            "proj/sub/deep/c.txt",
            "proj/empty",
            "loose.md",
        ] {
            assert!(paths.contains(p), "{p} missing from {paths:?}");
        }
        let c = entries
            .iter()
            .find(|e| e.path == "proj/sub/deep/c.txt")
            .unwrap();
        assert_eq!(c.size, Some(7));
        assert!(
            entries
                .iter()
                .find(|e| e.path == "proj/empty")
                .unwrap()
                .is_dir
        );

        let out = t.p("out");
        let r = extract_all(archive, &out);
        assert_eq!(r.files, 4);
        assert!(r.skipped.is_empty(), "{:?}", r.skipped);
        assert_eq!(
            fs::read_to_string(out.join("proj/sub/deep/c.txt")).unwrap(),
            "charlie"
        );
        assert_eq!(fs::read_to_string(out.join("loose.md")).unwrap(), "loose");
        assert!(out.join("proj/empty").is_dir());

        // selective extraction: just one folder
        let out2 = t.p("out2");
        let no = AtomicBool::new(false);
        let want = |p: &str| p == "proj/sub" || p.starts_with("proj/sub/");
        let r = extract(archive, kind, &want, &out2, None, &no, &mut |_| {}).unwrap();
        assert_eq!(r.files, 2);
        assert!(!out2.join("proj/a.txt").exists());
        assert!(out2.join("proj/sub/b.txt").is_file());
    }

    #[test]
    fn zip_compress_list_extract() {
        let t = Tmp::new("zip");
        let src = tree(&t);
        let z = t.p("bundle.zip");
        compress(&src, &z).unwrap();
        check_round_trip(&z, &t);
        let nodes = build_tree(&list(&z, Kind::Zip).unwrap());
        // root: folder "proj" first, then "loose.md"
        let top: Vec<&str> = nodes[0]
            .children
            .iter()
            .map(|&i| nodes[i].name.as_str())
            .collect();
        assert_eq!(top, ["proj", "loose.md"]);
        let proj = nodes[0].children[0];
        assert_eq!(nodes[proj].size, Some(5 + 5 + 7));
    }

    #[test]
    fn targz_compress_list_extract() {
        let t = Tmp::new("tgz");
        let src = tree(&t);
        let z = t.p("bundle.tar.gz");
        compress(&src, &z).unwrap();
        assert_eq!(kind_of(&z).unwrap().0, Kind::Tar(Comp::Gz));
        check_round_trip(&z, &t);
    }

    #[test]
    fn zip_slip_rejected() {
        let t = Tmp::new("slip");
        let z = t.p("evil.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&z).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            for name in [
                "../evil.txt",
                "/abs.txt",
                "ok/../../evil2.txt",
                "ok/fine.txt",
            ] {
                w.start_file(name, o).unwrap();
                w.write_all(b"x").unwrap();
            }
            w.add_symlink("ok/link", "../../outside", o).unwrap();
            w.finish().unwrap();
        }
        let dest = t.p("box/evil");
        let r = extract_all(&z, &dest);
        assert_eq!(r.files, 1);
        assert_eq!(r.skipped.len(), 4, "{:?}", r.skipped);
        assert!(dest.join("ok/fine.txt").is_file());
        assert!(!t.p("box/evil.txt").exists() && !t.p("evil.txt").exists());
        assert!(!t.p("box/evil2.txt").exists());
        assert!(fs::symlink_metadata(dest.join("ok/link")).is_err());
    }

    #[test]
    fn tar_symlink_escape_rejected() {
        let t = Tmp::new("tarslip");
        let p = t.p("evil.tar");
        {
            let mut b = tar::Builder::new(File::create(&p).unwrap());
            let mut add_link = |name: &str, target: &str| {
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                h.set_mode(0o777);
                b.append_link(&mut h, name, target).unwrap();
            };
            // lexically inside, but then used as a stepping stone
            add_link("a/b/s", "../..");
            add_link("esc", "../../etc");
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            b.append_data(&mut h, "a/b/s/u.txt", &b"x"[..]).unwrap();
            // raw "../" name the builder would refuse
            let mut h = tar::Header::new_gnu();
            h.as_gnu_mut().unwrap().name[..9].copy_from_slice(b"../up.txt");
            h.set_size(1);
            h.set_mode(0o644);
            h.set_cksum();
            b.append(&h, &b"x"[..]).unwrap();
            b.finish().unwrap();
        }
        let dest = t.p("x/out");
        let r = extract_all(&p, &dest);
        assert!(fs::symlink_metadata(dest.join("a/b/s"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::symlink_metadata(dest.join("esc")).is_err());
        assert!(!dest.join("u.txt").exists() && !t.p("x/up.txt").exists());
        assert_eq!(r.files, 1, "{:?}", r.skipped);
        assert_eq!(r.skipped.len(), 3, "{:?}", r.skipped);
    }

    #[test]
    fn single_file_gz() {
        let t = Tmp::new("gz");
        let p = t.p("notes.txt.gz");
        {
            let mut e = flate2::write::GzEncoder::new(
                File::create(&p).unwrap(),
                flate2::Compression::default(),
            );
            e.write_all(b"hello gzip").unwrap();
            e.finish().unwrap();
        }
        assert_eq!(kind_of(&p).unwrap().0, Kind::Single(Comp::Gz));
        let entries = list(&p, Kind::Single(Comp::Gz)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "notes.txt");
        let out = t.p("o/notes.txt");
        let no = AtomicBool::new(false);
        let mut n = 0;
        extract_single(&p, Comp::Gz, &out, &no, &mut |k| n += k).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "hello gzip");
        assert_eq!(n, 10);
    }

    #[tokio::test]
    async fn open_and_view() {
        let t = Tmp::new("open");
        let src = tree(&t);
        let z = t.p("b.zip");
        compress(&src, &z).unwrap();
        let o = open(z).await.unwrap();
        assert_eq!(o.entries.len(), 8);
        let (mut s, _) = State::new(o);
        assert_eq!(s.visible.len(), 2);
        let proj = s.visible[0].0;
        let _ = s.update(Msg::Toggle(proj));
        assert_eq!(s.visible.len(), 2 + 3); // empty, sub, a.txt
        let _ = s.update(Msg::Check(proj, true));
        let _ = s.update(Msg::Click(s.visible[4].0));
        let _ = s.update(Msg::Password("pw".into()));
        let _ = s.update(Msg::Progress(3));
        let _ = s.update(Msg::Cancel);
        let _ = s.view();
        let (_, ev) = s.update(Msg::JobDone(
            JobKind::Open,
            Ok((PathBuf::from("/x"), String::new())),
        ));
        assert!(matches!(ev, Some(Event::OpenFile(_))));
        let _ = s.update(Msg::ExtractSelected);
        assert!(s.job.is_some());
        let _ = s.update(Msg::OpenEntry);
        let _ = s.update(Msg::ExtractAll);
        assert_eq!((s.is_dirty(), s.is_editing()), (false, None));
        let _ = s.toggle_edit();
        let _ = s.save();
    }

    /// temp dir (not under $HOME) for the tools tests
    fn tmp2(tag: &str) -> Tmp {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "ogx-archive-tools-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[test]
    fn every_format_round_trips() {
        for f in Format::ALL {
            let t = tmp2("fmt");
            let src = tree(&t);
            let out = t.p(&format!("bundle{}", f.ext()));
            let total = source_bytes(&src, &no_cancel());
            assert_eq!(total, 5 + 5 + 7 + 5);
            let mut seen = 0;
            compress_with(&src, &out, f, &no_cancel(), &mut |n| seen += n).unwrap();
            assert_eq!(seen, total, "{f:?} progress");
            assert_eq!(Format::from_name(&out), Some(f));
            check_round_trip(&out, &t);
            assert_eq!(
                extract_total(&out, None),
                match f {
                    Format::Zip | Format::SevenZ => total,
                    _ => fs::metadata(&out).unwrap().len(),
                }
            );
        }
    }

    #[test]
    fn extract_here_is_smart_about_names() {
        let t = tmp2("here");
        let src = tree(&t);
        let dest = t.p("dest");
        fs::create_dir_all(&dest).unwrap();
        // one top-level folder: lands straight in dest, then "proj (2)"
        let one = t.p("one.tar.xz");
        compress_with(&src[..1], &one, Format::TarXz, &no_cancel(), &mut |_| {}).unwrap();
        let mut done = 0;
        let a = extract_here(&one, &dest, None, &no_cancel(), &mut |n| done += n).unwrap();
        assert_eq!(a.out, dest.join("proj"));
        assert_eq!(a.files, 3);
        assert_eq!(done, fs::metadata(&one).unwrap().len());
        let b = extract_here(&one, &dest, None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(b.out, dest.join("proj (2)"));
        assert!(dest.join("proj (2)/sub/deep/c.txt").is_file());
        // several top-level items: wrapped in a folder named after the archive
        let many = t.p("bundle.zip");
        compress_with(&src, &many, Format::Zip, &no_cancel(), &mut |_| {}).unwrap();
        let c = extract_here(&many, &dest, None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(c.out, dest.join("bundle"));
        assert!(dest.join("bundle/loose.md").is_file() && dest.join("bundle/proj/a.txt").is_file());
        let d = extract_here(&many, &dest, None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(d.out, dest.join("bundle (2)"));
        // a lone file in the archive also goes straight in
        let lone = t.p("lone.7z");
        compress_with(&src[1..], &lone, Format::SevenZ, &no_cancel(), &mut |_| {}).unwrap();
        let e = extract_here(&lone, &dest, None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(e.out, dest.join("loose.md"));
        // single compressed file
        let gz = t.p("notes.txt.gz");
        {
            let mut w = flate2::write::GzEncoder::new(
                File::create(&gz).unwrap(),
                flate2::Compression::default(),
            );
            w.write_all(b"hi").unwrap();
            w.finish().unwrap();
        }
        let g = extract_here(&gz, &dest, None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(g.out, dest.join("notes.txt"));
        let g2 = extract_to(&gz, &t.p("gzout"), None, &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(fs::read_to_string(g2.out.join("notes.txt")).unwrap(), "hi");
        // no staging folders left behind
        let hidden: Vec<_> = fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(hidden.is_empty(), "{hidden:?}");
    }

    #[test]
    fn cancel_leaves_nothing() {
        let t = tmp2("cancel");
        let src = tree(&t);
        let yes = AtomicBool::new(true);
        let out = t.p("c.tar.gz");
        assert_eq!(
            compress_with(&src, &out, Format::TarGz, &yes, &mut |_| {}),
            Err(CANCELLED.to_string())
        );
        assert!(!out.exists());
        compress_with(&src, &out, Format::TarGz, &no_cancel(), &mut |_| {}).unwrap();
        let dest = t.p("d");
        fs::create_dir_all(&dest).unwrap();
        assert_eq!(
            extract_here(&out, &dest, None, &yes, &mut |_| {}),
            Err(CANCELLED.to_string())
        );
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
        assert!(extract_to(&out, &t.p("new"), None, &yes, &mut |_| {}).is_err());
        assert!(!t.p("new").exists());
        assert!(compress_with(&[], &out, Format::Zip, &no_cancel(), &mut |_| {}).is_err());
    }

    #[test]
    fn encrypted_zip_needs_the_password() {
        let t = tmp2("aes");
        let z = t.p("secret.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&z).unwrap());
            let o = zip::write::SimpleFileOptions::default()
                .with_aes_encryption(zip::AesMode::Aes256, "hunter2");
            w.start_file("s/a.txt", o).unwrap();
            w.write_all(b"top secret").unwrap();
            w.finish().unwrap();
        }
        let dest = t.p("d");
        fs::create_dir_all(&dest).unwrap();
        let e = extract_here(&z, &dest, None, &no_cancel(), &mut |_| {}).unwrap_err();
        assert!(is_password_error(&e), "{e}");
        let e = extract_here(&z, &dest, Some("nope"), &no_cancel(), &mut |_| {}).unwrap_err();
        assert!(is_password_error(&e), "{e}");
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
        let ok = extract_here(&z, &dest, Some("hunter2"), &no_cancel(), &mut |_| {}).unwrap();
        assert_eq!(ok.out, dest.join("s"));
        assert_eq!(
            fs::read_to_string(dest.join("s/a.txt")).unwrap(),
            "top secret"
        );
        assert!(list_entries(&z, None).unwrap()[0].encrypted);
    }

    #[test]
    fn format_names() {
        assert_eq!(Format::from_name(Path::new("a.TGZ")), Some(Format::TarGz));
        assert_eq!(Format::from_name(Path::new("a.tar.bz2")), None);
        assert_eq!(Format::from_name(Path::new("a.gz")), None);
        assert_eq!(Format::TarGz.strip("x.TAR.GZ").as_deref(), Some("x"));
        assert_eq!(Format::Zip.strip("x.tar"), None);
        assert_eq!(base_name(Path::new("/a/b.tar.zst")), "b");
        assert!(is_password_error("wrong password") && !is_password_error("crc"));
    }

    #[test]
    fn names_and_safety_helpers() {
        assert_eq!(stem(Path::new("/a/x.tar.gz")), "x");
        assert_eq!(stem(Path::new("/a/X.TGZ")), "X");
        assert_eq!(stem(Path::new("notes.txt.zst")), "notes.txt");
        assert!(supported(Path::new("a.7z")) && supported(Path::new("a.tar.bz2")));
        assert!(!supported(Path::new("a.txt")) && !supported(Path::new(".gz")));
        assert_eq!(safe_rel("a/./b"), Some(PathBuf::from("a/b")));
        assert_eq!(safe_rel("a/../b"), None);
        assert_eq!(safe_rel("/etc/passwd"), None);
        assert_eq!(safe_rel("..\\x"), None);
        assert!(link_ok(Path::new("a/b/l"), "../c"));
        assert!(link_ok(Path::new("a/l"), ".."));
        assert!(!link_ok(Path::new("a/l"), "../.."));
        assert!(!link_ok(Path::new("l"), "/etc"));
        assert_eq!(norm("./a//b/"), "a/b");
        assert_eq!(
            fmt_time(Some(UNIX_EPOCH + Duration::from_secs(86_400 + 3_660))),
            "1970-01-02 01:01"
        );
        assert_eq!(days_from_civil(2023, 3, 15), 19_431);
    }
}
