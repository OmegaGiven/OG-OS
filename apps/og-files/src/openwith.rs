//! "Open with": freedesktop desktop entries, MIME detection (shared-mime-info
//! globs + a small magic table), default apps (mimeapps.list chain and
//! mimeinfo.cache), Exec field codes and detached launching.
//!
//! Everything that normally comes from the environment is gathered in
//! [`Env`], so tests pass temp dirs instead of touching env vars or the
//! user's real config. Loading ([`Registry::load`]) and MIME sniffing read
//! the disk: call them off the UI thread.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where to look for desktop files, MIME data and mimeapps.list.
#[derive(Debug, Clone, Default)]
pub struct Env {
    pub data_home: PathBuf,
    /// $XDG_DATA_DIRS, most important first
    pub data_dirs: Vec<PathBuf>,
    pub config_home: PathBuf,
    pub config_dirs: Vec<PathBuf>,
    /// $XDG_CURRENT_DESKTOP, lowercased (e.g. ["sway"], ["gnome"])
    pub desktops: Vec<String>,
    /// flatpak exports (user, system), also used when not in $XDG_DATA_DIRS
    pub flatpak: Vec<PathBuf>,
    /// $PATH, for TryExec
    pub path: Vec<PathBuf>,
}

fn env_dirs(var: &str, default: &str) -> Vec<PathBuf> {
    let v = std::env::var(var).unwrap_or_default();
    let v = if v.trim().is_empty() { default } else { &v };
    v.split(':')
        .filter(|s| s.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

impl Env {
    pub fn from_env() -> Env {
        let home = crate::config::home();
        let abs = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        let data_home = abs("XDG_DATA_HOME", ".local/share");
        Env {
            flatpak: vec![
                data_home.join("flatpak/exports/share"),
                PathBuf::from("/var/lib/flatpak/exports/share"),
            ],
            data_home,
            data_dirs: env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"),
            config_home: abs("XDG_CONFIG_HOME", ".config"),
            config_dirs: env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"),
            desktops: std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect(),
            path: std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).collect())
                .unwrap_or_default(),
        }
    }

    /// Data dirs in priority order: data home, $XDG_DATA_DIRS, then the
    /// flatpak exports if they weren't listed already.
    fn data_roots(&self) -> Vec<PathBuf> {
        let mut v = vec![self.data_home.clone()];
        v.extend(self.data_dirs.iter().cloned());
        for f in &self.flatpak {
            v.push(f.clone());
        }
        let mut seen = HashSet::new();
        v.retain(|p| seen.insert(p.clone()));
        v
    }

    pub fn app_dirs(&self) -> Vec<PathBuf> {
        self.data_roots()
            .into_iter()
            .map(|d| d.join("applications"))
            .collect()
    }

    fn mime_dirs(&self) -> Vec<PathBuf> {
        self.data_roots()
            .into_iter()
            .map(|d| d.join("mime"))
            .collect()
    }

    /// mimeapps.list files in lookup order (spec "Association between MIME
    /// types and applications").
    pub fn mimeapps_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut add = |dir: &Path| {
            for d in &self.desktops {
                out.push(dir.join(format!("{d}-mimeapps.list")));
            }
            out.push(dir.join("mimeapps.list"));
        };
        add(&self.config_home);
        for d in &self.config_dirs {
            add(d);
        }
        add(&self.data_home.join("applications"));
        for d in &self.data_dirs {
            add(&d.join("applications"));
        }
        out
    }

    /// The file "Set as default" writes.
    pub fn user_mimeapps(&self) -> PathBuf {
        self.config_home.join("mimeapps.list")
    }

    /// Is `prog` (a name or absolute path) an executable on $PATH?
    pub fn runnable(&self, prog: &str) -> bool {
        find_in(prog, &self.path).is_some()
    }
}

/// Look `prog` up in `path` (absolute or containing '/' = checked as is).
pub fn find_in(prog: &str, path: &[PathBuf]) -> Option<PathBuf> {
    let exec = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if prog.is_empty() {
        return None;
    }
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return exec(&p).then_some(p);
    }
    path.iter().map(|d| d.join(prog)).find(|p| exec(p))
}

// ------------------------------------------------------------ desktop files

/// One application desktop entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct App {
    /// desktop file id, with ".desktop" (e.g. "org.gnome.Loupe.desktop")
    pub id: String,
    pub file: PathBuf,
    pub name: String,
    pub generic_name: String,
    pub comment: String,
    pub icon: String,
    pub exec: String,
    pub try_exec: String,
    /// Path= : working directory
    pub workdir: String,
    pub terminal: bool,
    pub no_display: bool,
    pub hidden: bool,
    pub only_show_in: Vec<String>,
    pub not_show_in: Vec<String>,
    pub mime_types: Vec<String>,
}

impl App {
    /// "org.gnome.Loupe" (id without ".desktop").
    pub fn short_id(&self) -> &str {
        self.id.strip_suffix(".desktop").unwrap_or(&self.id)
    }

    /// Declares `mime` (exactly or as "major/*").
    pub fn handles(&self, mime: &str) -> bool {
        let major = mime.split('/').next().unwrap_or("");
        self.mime_types.iter().any(|m| {
            m.eq_ignore_ascii_case(mime)
                || m.strip_suffix("/*")
                    .is_some_and(|mj| mj.eq_ignore_ascii_case(major))
        })
    }

    /// OnlyShowIn / NotShowIn against the current desktop (loosely: with
    /// no known desktop, everything shows).
    fn shows_in(&self, desktops: &[String]) -> bool {
        if desktops.is_empty() {
            return true;
        }
        let has = |l: &[String]| l.iter().any(|d| desktops.contains(&d.to_ascii_lowercase()));
        if !self.only_show_in.is_empty() && !has(&self.only_show_in) {
            return false;
        }
        !has(&self.not_show_in)
    }
}

/// Desktop-entry string unescaping (\s \n \t \r \\).
fn unescape_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// `a;b\;c;` → ["a", "b;c"].
fn split_list(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut it = v.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => match it.next() {
                Some(';') => cur.push(';'),
                Some(o) => {
                    cur.push('\\');
                    cur.push(o);
                }
                None => {}
            },
            ';' => {
                let s = unescape_value(cur.trim());
                if !s.is_empty() {
                    out.push(s);
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    let s = unescape_value(cur.trim());
    if !s.is_empty() {
        out.push(s);
    }
    out
}

/// Parse the `[Desktop Entry]` group of a desktop file. None unless it is
/// an `Application` with an `Exec` line (or `Hidden`, which masks the id).
pub fn parse_desktop(text: &str, id: &str, file: &Path) -> Option<App> {
    let mut app = App {
        id: id.to_string(),
        file: file.to_path_buf(),
        ..App::default()
    };
    let mut in_main = false;
    let mut kind = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        let b = v.eq_ignore_ascii_case("true");
        match k {
            "Type" => kind = v.to_string(),
            "Name" => app.name = unescape_value(v),
            "GenericName" => app.generic_name = unescape_value(v),
            "Comment" => app.comment = unescape_value(v),
            "Icon" => app.icon = unescape_value(v),
            "Exec" => app.exec = unescape_value(v),
            "TryExec" => app.try_exec = unescape_value(v),
            "Path" => app.workdir = unescape_value(v),
            "Terminal" => app.terminal = b,
            "NoDisplay" => app.no_display = b,
            "Hidden" => app.hidden = b,
            "OnlyShowIn" => app.only_show_in = split_list(v),
            "NotShowIn" => app.not_show_in = split_list(v),
            "MimeType" => app.mime_types = split_list(v),
            _ => {}
        }
    }
    if app.hidden {
        return Some(app);
    }
    if kind != "Application" || app.exec.is_empty() {
        return None;
    }
    if app.name.is_empty() {
        app.name = app.short_id().to_string();
    }
    Some(app)
}

/// Desktop files under `dir` as (id, path); ids of files in subfolders use
/// '-' for '/' (spec).
fn desktop_files(dir: &Path) -> Vec<(String, PathBuf)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>, depth: u8) {
        let Ok(rd) = fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() && depth < 4 {
                walk(&p, &format!("{prefix}{name}-"), out, depth + 1);
            } else if name.ends_with(".desktop") {
                out.push((format!("{prefix}{name}"), p));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, "", &mut out, 0);
    out
}

// ------------------------------------------------------------------- MIME

#[derive(Debug, Clone, Default)]
struct Glob {
    weight: u32,
    mime: String,
    pattern: String,
    case_sensitive: bool,
}

/// shared-mime-info data (globs2, subclasses, aliases); empty when the
/// system has none (then `mime_guess` covers names).
#[derive(Debug, Clone, Default)]
pub struct MimeDb {
    /// "*.ext" patterns by lowercase ext (may contain dots: "tar.gz")
    ext: HashMap<String, (u32, String)>,
    /// literal names and other patterns
    other: Vec<Glob>,
    parents: HashMap<String, Vec<String>>,
    aliases: HashMap<String, String>,
}

/// fnmatch with `*`, `?` and `[...]`.
fn glob_match(pat: &[char], s: &[char]) -> bool {
    match pat.first() {
        None => s.is_empty(),
        Some('*') => (0..=s.len()).any(|i| glob_match(&pat[1..], &s[i..])),
        Some('?') => !s.is_empty() && glob_match(&pat[1..], &s[1..]),
        Some('[') => {
            let Some(end) = pat.iter().position(|&c| c == ']') else {
                return s.first() == Some(&'[') && glob_match(&pat[1..], &s[1..]);
            };
            let Some(&c) = s.first() else {
                return false;
            };
            let set = &pat[1..end];
            let (neg, set) = match set.first() {
                Some('!') | Some('^') => (true, &set[1..]),
                _ => (false, set),
            };
            let mut hit = false;
            let mut i = 0;
            while i < set.len() {
                if i + 2 < set.len() && set[i + 1] == '-' {
                    hit |= set[i] <= c && c <= set[i + 2];
                    i += 3;
                } else {
                    hit |= set[i] == c;
                    i += 1;
                }
            }
            hit != neg && glob_match(&pat[end + 1..], &s[1..])
        }
        Some(&p) => s.first() == Some(&p) && glob_match(&pat[1..], &s[1..]),
    }
}

impl MimeDb {
    pub fn load(dirs: &[PathBuf]) -> MimeDb {
        let mut db = MimeDb::default();
        for d in dirs {
            if let Ok(s) = fs::read_to_string(d.join("globs2")) {
                db.add_globs2(&s);
            }
            if let Ok(s) = fs::read_to_string(d.join("subclasses")) {
                for l in s.lines() {
                    if let Some((c, p)) = l.trim().split_once(' ') {
                        let v = db.parents.entry(c.to_string()).or_default();
                        if !v.iter().any(|x| x == p) {
                            v.push(p.to_string());
                        }
                    }
                }
            }
            if let Ok(s) = fs::read_to_string(d.join("aliases")) {
                for l in s.lines() {
                    if let Some((a, m)) = l.trim().split_once(' ') {
                        db.aliases
                            .entry(a.to_string())
                            .or_insert_with(|| m.to_string());
                    }
                }
            }
        }
        db
    }

    fn add_globs2(&mut self, s: &str) {
        for l in s.lines() {
            if l.starts_with('#') {
                continue;
            }
            let mut f = l.splitn(4, ':');
            let (Some(w), Some(mime), Some(pat)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            if pat == "__NOGLOBS__" {
                continue;
            }
            let weight = w.parse().unwrap_or(50);
            let cs = f.next().is_some_and(|fl| fl.split(',').any(|x| x == "cs"));
            let simple_ext = pat
                .strip_prefix("*.")
                .filter(|e| !e.contains(['*', '?', '[']));
            match simple_ext {
                Some(e) if !cs => {
                    // first file / higher weight wins
                    let e = e.to_ascii_lowercase();
                    let keep = self.ext.get(&e).is_some_and(|(ow, _)| *ow >= weight);
                    if !keep {
                        self.ext.insert(e, (weight, mime.to_string()));
                    }
                }
                _ => self.other.push(Glob {
                    weight,
                    mime: mime.to_string(),
                    pattern: pat.to_string(),
                    case_sensitive: cs,
                }),
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ext.is_empty() && self.other.is_empty()
    }

    pub fn unalias(&self, mime: &str) -> String {
        self.aliases
            .get(mime)
            .cloned()
            .unwrap_or_else(|| mime.to_string())
    }

    /// MIME type from the file name alone.
    pub fn by_name(&self, name: &str) -> Option<String> {
        let lower = name.to_ascii_lowercase();
        // highest weight wins, then the longest pattern
        let consider = |w: u32, len: usize, m: &str, best: &mut Option<(u32, usize, String)>| {
            if best
                .as_ref()
                .is_none_or(|(bw, bl, _)| (w, len) > (*bw, *bl))
            {
                *best = Some((w, len, m.to_string()));
            }
        };
        let mut found: Option<(u32, usize, String)> = None;
        // every dot position: "a.tar.gz" tries "tar.gz" then "gz"
        for (i, _) in lower.match_indices('.') {
            if let Some((w, m)) = self.ext.get(&lower[i + 1..]) {
                consider(*w, lower.len() - i + 1, m, &mut found);
            }
        }
        let chars: Vec<char> = name.chars().collect();
        let lchars: Vec<char> = lower.chars().collect();
        for g in &self.other {
            let p: Vec<char> = if g.case_sensitive {
                g.pattern.chars().collect()
            } else {
                g.pattern.to_ascii_lowercase().chars().collect()
            };
            let s = if g.case_sensitive { &chars } else { &lchars };
            if glob_match(&p, s) {
                consider(g.weight, g.pattern.len(), &g.mime, &mut found);
            }
        }
        found.map(|(_, _, m)| self.unalias(&m)).or_else(|| {
            if self.is_empty() {
                mime_guess::from_path(name).first_raw().map(str::to_string)
            } else {
                None
            }
        })
    }

    /// `mime`, then its ancestors (subclasses, text/* → text/plain),
    /// without application/octet-stream.
    pub fn chain(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.unalias(mime)];
        let mut i = 0;
        while i < out.len() {
            let cur = out[i].clone();
            let mut ps: Vec<String> = self.parents.get(&cur).cloned().unwrap_or_default();
            if cur.starts_with("text/") && cur != "text/plain" {
                ps.push("text/plain".into());
            }
            for p in ps {
                let p = self.unalias(&p);
                if p != "application/octet-stream" && !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        out
    }
}

/// MIME type from the first bytes of a file.
pub fn sniff(head: &[u8]) -> &'static str {
    let starts = |m: &[u8]| head.starts_with(m);
    let at = |o: usize, m: &[u8]| head.len() >= o + m.len() && &head[o..o + m.len()] == m;
    if starts(b"%PDF-") {
        "application/pdf"
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if starts(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        "image/gif"
    } else if starts(b"RIFF") && at(8, b"WEBP") {
        "image/webp"
    } else if starts(b"RIFF") && at(8, b"WAVE") {
        "audio/x-wav"
    } else if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        "application/zip"
    } else if starts(b"\x1f\x8b") {
        "application/gzip"
    } else if starts(b"7z\xbc\xaf\x27\x1c") {
        "application/x-7z-compressed"
    } else if starts(b"\xfd7zXZ\x00") {
        "application/x-xz"
    } else if starts(b"\x28\xb5\x2f\xfd") {
        "application/zstd"
    } else if starts(b"BZh") {
        "application/x-bzip2"
    } else if starts(b"\x7fELF") {
        "application/x-executable"
    } else if starts(b"OggS") {
        "audio/ogg"
    } else if starts(b"fLaC") {
        "audio/flac"
    } else if starts(b"ID3") {
        "audio/mpeg"
    } else if at(4, b"ftyp") {
        "video/mp4"
    } else if starts(b"\x1a\x45\xdf\xa3") {
        "video/x-matroska"
    } else if starts(b"#!") {
        let line = head.split(|&b| b == b'\n').next().unwrap_or(b"");
        let line = String::from_utf8_lossy(line);
        if line.contains("python") {
            "text/x-python3"
        } else if line.contains("sh") {
            "application/x-shellscript"
        } else {
            "text/plain"
        }
    } else if head.contains(&0) || std::str::from_utf8(head).is_err() && !utf8_cut(head) {
        "application/octet-stream"
    } else {
        let s = String::from_utf8_lossy(head).to_ascii_lowercase();
        let s = s.trim_start();
        if s.starts_with("<svg") || (s.starts_with("<?xml") && s.contains("<svg")) {
            "image/svg+xml"
        } else if s.starts_with("<!doctype html") || s.starts_with("<html") {
            "text/html"
        } else if s.starts_with("<?xml") {
            "application/xml"
        } else {
            "text/plain"
        }
    }
}

/// Valid UTF-8 except for a multi-byte char cut off at the end of the buffer.
fn utf8_cut(b: &[u8]) -> bool {
    match std::str::from_utf8(b) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && b.len() - e.valid_up_to() < 4,
    }
}

// ------------------------------------------------------------ associations

#[derive(Debug, Clone, Default)]
struct MimeApps {
    default: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
}

fn parse_mimeapps(text: &str) -> MimeApps {
    let mut m = MimeApps::default();
    let mut sect = "";
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            sect = match line {
                "[Default Applications]" => "d",
                "[Added Associations]" => "a",
                "[Removed Associations]" => "r",
                _ => "",
            };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let target = match sect {
            "d" => &mut m.default,
            "a" => &mut m.added,
            "r" => &mut m.removed,
            _ => continue,
        };
        target
            .entry(k.trim().to_string())
            .or_default()
            .extend(split_list(v));
    }
    m
}

/// The apps that can open a MIME type: `default` (if any) is also first in
/// `apps`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Choices {
    pub mime: String,
    pub default: Option<String>,
    pub apps: Vec<String>,
}

/// Installed apps + MIME data + associations, loaded once (off the UI
/// thread) and queried without touching the disk (except `mime_of`).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    /// every usable app, by display name
    pub apps: Vec<App>,
    by_id: HashMap<String, usize>,
    pub mime: MimeDb,
    lists: Vec<MimeApps>,
    cache: HashMap<String, Vec<String>>,
    desktops: Vec<String>,
    /// file "Set as default" writes
    pub user_list: PathBuf,
}

impl Registry {
    pub fn load(env: &Env) -> Registry {
        let mut apps: Vec<App> = Vec::new();
        let mut seen = HashSet::new();
        let mut cache: HashMap<String, Vec<String>> = HashMap::new();
        for dir in env.app_dirs() {
            for (id, p) in desktop_files(&dir) {
                // the first dir with an id wins (also when it says Hidden)
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(text) = fs::read_to_string(&p) else {
                    continue;
                };
                if let Some(a) = parse_desktop(&text, &id, &p) {
                    if a.hidden || (!a.try_exec.is_empty() && !env.runnable(&a.try_exec)) {
                        continue;
                    }
                    apps.push(a);
                }
            }
            if let Ok(s) = fs::read_to_string(dir.join("mimeinfo.cache")) {
                for (m, ids) in
                    parse_mimeapps(&s.replace("[MIME Cache]", "[Added Associations]")).added
                {
                    let v = cache.entry(m).or_default();
                    for id in ids {
                        if !v.contains(&id) {
                            v.push(id);
                        }
                    }
                }
            }
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        let lists = env
            .mimeapps_files()
            .iter()
            .filter_map(|f| fs::read_to_string(f).ok())
            .map(|s| parse_mimeapps(&s))
            .collect();
        let by_id = apps
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id.clone(), i))
            .collect();
        Registry {
            apps,
            by_id,
            mime: MimeDb::load(&env.mime_dirs()),
            lists,
            cache,
            desktops: env.desktops.clone(),
            user_list: env.user_mimeapps(),
        }
    }

    /// By desktop id, with or without ".desktop".
    pub fn get(&self, id: &str) -> Option<&App> {
        let key = if id.ends_with(".desktop") {
            id.to_string()
        } else {
            format!("{id}.desktop")
        };
        self.by_id.get(&key).map(|&i| &self.apps[i])
    }

    /// Apps for the "all apps" list: not NoDisplay, shown on this desktop.
    pub fn listed(&self) -> impl Iterator<Item = &App> {
        self.apps
            .iter()
            .filter(|a| !a.no_display && a.shows_in(&self.desktops))
    }

    /// MIME type of a path: folders, then name globs, then magic bytes.
    /// Reads up to 512 bytes when the name doesn't decide.
    pub fn mime_of(&self, path: &Path) -> String {
        if path.is_dir() {
            return "inode/directory".into();
        }
        if let Some(m) = self.mime_by_name(path) {
            return m;
        }
        let mut head = Vec::with_capacity(512);
        if let Ok(f) = fs::File::open(path) {
            let _ = f.take(512).read_to_end(&mut head);
        }
        self.mime.unalias(sniff(&head))
    }

    /// From the name alone (no disk access).
    pub fn mime_by_name(&self, path: &Path) -> Option<String> {
        let name = path.file_name()?.to_string_lossy();
        self.mime.by_name(&name)
    }

    fn removed(&self, mime: &str, id: &str) -> bool {
        self.lists.iter().any(|l| {
            l.removed
                .get(mime)
                .is_some_and(|v| v.iter().any(|x| x == id))
        })
    }

    /// Default + recommended apps for `mime`, walking parent types.
    pub fn choices(&self, mime: &str) -> Choices {
        let chain = self.mime.chain(mime);
        let mut default: Option<String> = None;
        let mut apps: Vec<String> = Vec::new();
        let ok = |id: &str| self.by_id.contains_key(id);
        for m in &chain {
            if default.is_none() {
                'files: for l in &self.lists {
                    for id in l.default.get(m).into_iter().flatten() {
                        if ok(id) {
                            default = Some(id.clone());
                            break 'files;
                        }
                    }
                }
            }
            let mut push = |id: &String| {
                if ok(id) && !self.removed(m, id) && !apps.contains(id) {
                    apps.push(id.clone());
                }
            };
            for l in &self.lists {
                l.added.get(m).into_iter().flatten().for_each(&mut push);
            }
            self.cache.get(m).into_iter().flatten().for_each(&mut push);
            for a in self.apps.iter().filter(|a| a.handles(m)) {
                push(&a.id);
            }
        }
        if default.is_none() {
            default = apps.first().cloned();
        }
        if let Some(d) = &default {
            apps.retain(|x| x != d);
            apps.insert(0, d.clone());
        }
        Choices {
            mime: self.mime.unalias(mime),
            default,
            apps,
        }
    }

    pub fn default_for(&self, mime: &str) -> Option<&App> {
        self.choices(mime).default.and_then(|id| self.get(&id))
    }
}

// --------------------------------------------------------- set as default

/// `text` (a mimeapps.list) with `mime=id;` in [Default Applications];
/// every other line is kept as it was.
pub fn set_default_in(text: &str, mime: &str, id: &str) -> String {
    let entry = format!("{mime}={id};");
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let header = lines
        .iter()
        .position(|l| l.trim() == "[Default Applications]");
    match header {
        Some(h) => {
            let end = lines[h + 1..]
                .iter()
                .position(|l| l.trim_start().starts_with('['))
                .map(|i| h + 1 + i)
                .unwrap_or(lines.len());
            let existing = (h + 1..end).find(|&i| {
                lines[i]
                    .split_once('=')
                    .is_some_and(|(k, _)| k.trim() == mime)
            });
            match existing {
                Some(i) => lines[i] = entry,
                None => {
                    // after the section's last non-blank line
                    let mut at = end;
                    while at > h + 1 && lines[at - 1].trim().is_empty() {
                        at -= 1;
                    }
                    lines.insert(at, entry);
                }
            }
        }
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push(String::new());
            }
            lines.push("[Default Applications]".into());
            lines.push(entry);
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// Make `id` the default for `mime` in `file` (normally
/// $XDG_CONFIG_HOME/mimeapps.list), atomically; a symlinked file is
/// updated where it points.
pub fn set_default(file: &Path, mime: &str, id: &str) -> Result<(), String> {
    let target = fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let old = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", target.display())),
    };
    let new = set_default_in(&old, mime, id);
    let dir = target.parent().ok_or("bad mimeapps.list path")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".mimeapps.list.og-files-{}", std::process::id()));
    fs::write(&tmp, new).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

// ------------------------------------------------------------ Exec lines

/// Split an Exec value into arguments (spec quoting: double quotes, with
/// `\"` `\`` `\$` `\\` escapes inside). `bool` = the argument was quoted.
pub fn split_exec(exec: &str) -> Result<Vec<(String, bool)>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut in_arg = false;
    let mut it = exec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                in_arg = true;
                quoted = true;
                loop {
                    match it.next() {
                        None => return Err("unterminated quote in Exec".into()),
                        Some('"') => break,
                        Some('\\') => match it.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => cur.push(e),
                            Some(o) => {
                                cur.push('\\');
                                cur.push(o);
                            }
                            None => return Err("unterminated quote in Exec".into()),
                        },
                        Some(o) => cur.push(o),
                    }
                }
            }
            ' ' | '\t' | '\n' => {
                if in_arg {
                    out.push((std::mem::take(&mut cur), quoted));
                    in_arg = false;
                    quoted = false;
                }
            }
            '\\' => {
                in_arg = true;
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            c => {
                in_arg = true;
                cur.push(c);
            }
        }
    }
    if in_arg {
        out.push((cur, quoted));
    }
    if out.is_empty() {
        return Err("empty Exec".into());
    }
    Ok(out)
}

/// `file:///a%20b` for a local path.
pub fn file_uri(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Command lines (argv) to open `files` with `app`: one when the Exec line
/// takes a list (%F/%U), one per file for %f/%u, one without files when it
/// takes none. `terminal` (argv prefix such as ["alacritty", "-e"]) wraps
/// Terminal=true apps.
pub fn command_lines(
    app: &App,
    files: &[PathBuf],
    terminal: Option<&[String]>,
) -> Result<Vec<Vec<String>>, String> {
    let args = split_exec(&app.exec)?;
    let has = |codes: &[&str]| {
        args.iter()
            .any(|(a, _)| codes.iter().any(|c| a.contains(c)))
    };
    let groups: Vec<Vec<PathBuf>> = if has(&["%F", "%U"]) || files.is_empty() {
        vec![files.to_vec()]
    } else if has(&["%f", "%u"]) {
        files.iter().map(|f| vec![f.clone()]).collect()
    } else {
        vec![Vec::new()]
    };
    let mut out = Vec::new();
    for g in groups {
        let mut argv: Vec<String> = Vec::new();
        for (a, _quoted) in &args {
            match a.as_str() {
                "%F" => argv.extend(g.iter().map(|p| p.to_string_lossy().into_owned())),
                "%U" => argv.extend(g.iter().map(|p| file_uri(p))),
                "%i" => {
                    if !app.icon.is_empty() {
                        argv.push("--icon".into());
                        argv.push(app.icon.clone());
                    }
                }
                _ => {
                    let mut s = String::new();
                    let mut only_codes = true;
                    let mut ch = a.chars();
                    while let Some(c) = ch.next() {
                        if c != '%' {
                            only_codes = false;
                            s.push(c);
                            continue;
                        }
                        match ch.next() {
                            Some('f' | 'F') => {
                                if let Some(p) = g.first() {
                                    s.push_str(&p.to_string_lossy());
                                }
                            }
                            Some('u' | 'U') => {
                                if let Some(p) = g.first() {
                                    s.push_str(&file_uri(p));
                                }
                            }
                            Some('c') => s.push_str(&app.name),
                            Some('k') => s.push_str(&app.file.to_string_lossy()),
                            Some('i') => s.push_str(&app.icon),
                            Some('%') => {
                                only_codes = false;
                                s.push('%');
                            }
                            // deprecated / unknown codes are dropped
                            _ => {}
                        }
                    }
                    if !(s.is_empty() && only_codes) {
                        argv.push(s);
                    }
                }
            }
        }
        if argv.is_empty() {
            return Err("empty Exec".into());
        }
        if app.terminal {
            if let Some(t) = terminal {
                let mut full = t.to_vec();
                full.extend(argv);
                argv = full;
            }
        }
        out.push(argv);
    }
    Ok(out)
}

/// Start `argv` fully detached (own session, no stdio) so it outlives
/// og-files; a thread reaps it so no zombie is left. Returns the pid.
pub fn spawn_detached(argv: &[String], cwd: Option<&Path>) -> Result<u32, String> {
    let (prog, rest) = argv.split_first().ok_or("nothing to run")?;
    let mut cmd = Command::new(prog);
    cmd.args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("DESKTOP_STARTUP_ID");
    if let Some(d) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork
    // and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("{prog}: {e}"))?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// Open `files` with `app` (Path= or the first file's folder as cwd).
pub fn launch(app: &App, files: &[PathBuf]) -> Result<Vec<u32>, String> {
    let term = crate::terminal::resolve_env().map(|t| crate::terminal::exec_prefix(&t));
    let lines = command_lines(app, files, term.as_deref())?;
    let cwd: Option<PathBuf> = if !app.workdir.is_empty() {
        Some(PathBuf::from(&app.workdir))
    } else {
        files
            .first()
            .and_then(|f| f.parent())
            .map(Path::to_path_buf)
    };
    lines
        .iter()
        .map(|argv| spawn_detached(argv, cwd.as_deref()))
        .collect()
}

/// Open each file in its default app (files sharing an app go together).
/// Ok = "Opened x in App" lines; files without an app are reported.
pub fn open_default(reg: &Registry, files: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut missing = Vec::new();
    for f in files {
        let mime = reg.mime_of(f);
        match reg.default_for(&mime) {
            Some(a) => match groups.iter_mut().find(|(id, _)| *id == a.id) {
                Some((_, v)) => v.push(f.clone()),
                None => groups.push((a.id.clone(), vec![f.clone()])),
            },
            None => missing.push(format!("{} ({mime})", f.display())),
        }
    }
    let mut done = Vec::new();
    for (id, fs_) in groups {
        let app = reg.get(&id).ok_or("app vanished")?;
        launch(app, &fs_)?;
        done.push(format!("Opened {} in {}", describe(&fs_), app.name));
    }
    if !missing.is_empty() {
        return Err(format!("No app found for {}", missing.join(", ")));
    }
    Ok(done)
}

/// "notes.md" or "3 files".
pub fn describe(files: &[PathBuf]) -> String {
    match files {
        [one] => one
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| one.display().to_string()),
        v => format!("{} files", v.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            let p = std::env::temp_dir().join(format!("ogx-openwith-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn write(&self, rel: &str, s: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, s).unwrap();
            p
        }
        fn env(&self) -> Env {
            Env {
                data_home: self.0.join("home/share"),
                data_dirs: vec![self.0.join("sys/share")],
                config_home: self.0.join("home/config"),
                config_dirs: vec![self.0.join("etc/xdg")],
                desktops: vec!["sway".into()],
                flatpak: vec![self.0.join("flatpak/exports/share")],
                path: vec![self.0.join("bin")],
            }
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn desk(name: &str, exec: &str, mime: &str, extra: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nMimeType={mime}\n{extra}\n[Desktop Action new]\nName=Other\nExec=wrong\n"
        )
    }

    #[test]
    fn parses_desktop_entries() {
        let text = "# c\n[Desktop Entry]\nType=Application\nName=Image Viewer\nName[de]=Bildbetrachter\nExec=loupe %U\nTryExec=loupe\nIcon=org.gnome.Loupe\nMimeType=image/png;image/jpeg;x\\;y;\nNoDisplay=false\nTerminal=true\nOnlyShowIn=GNOME;sway;\nComment=Line\\sone\\nTwo\n[Desktop Action x]\nExec=nope\n";
        let a = parse_desktop(text, "org.gnome.Loupe.desktop", Path::new("/a")).unwrap();
        assert_eq!(a.name, "Image Viewer");
        assert_eq!(a.exec, "loupe %U");
        assert_eq!(a.mime_types, ["image/png", "image/jpeg", "x;y"]);
        assert!(a.terminal && !a.no_display);
        assert_eq!(a.comment, "Line one\nTwo");
        assert_eq!(a.short_id(), "org.gnome.Loupe");
        assert!(a.handles("image/png") && !a.handles("image/gif"));
        assert!(a.shows_in(&["sway".into()]) && !a.shows_in(&["kde".into()]));
        assert!(a.shows_in(&[]));
        // links / missing Exec aren't apps; Hidden masks
        assert!(
            parse_desktop("[Desktop Entry]\nType=Link\nURL=x\n", "l", Path::new("/")).is_none()
        );
        assert!(
            parse_desktop("[Desktop Entry]\nType=Application\n", "e", Path::new("/")).is_none()
        );
        assert!(
            parse_desktop("[Desktop Entry]\nHidden=true\n", "h", Path::new("/"))
                .unwrap()
                .hidden
        );
        let w = parse_desktop(
            &desk("Pics", "p", "image/*;", ""),
            "p.desktop",
            Path::new("/"),
        )
        .unwrap();
        assert!(w.handles("image/webp") && !w.handles("text/plain"));
    }

    #[test]
    fn exec_quoting_and_field_codes() {
        assert_eq!(
            split_exec(r#"sh -c "echo \"hi\" \$HOME \\x" 'a' b\ c"#).unwrap(),
            vec![
                ("sh".into(), false),
                ("-c".into(), false),
                (r#"echo "hi" $HOME \x"#.into(), true),
                ("'a'".into(), false),
                ("b c".into(), false),
            ]
        );
        assert!(split_exec(r#"x "open"#).is_err());
        assert!(split_exec("   ").is_err());

        let app = App {
            id: "v.desktop".into(),
            file: "/apps/v.desktop".into(),
            name: "Viewer".into(),
            icon: "vicon".into(),
            exec: "viewer %i --title=%c --from %k %d %F 100%%".into(),
            ..App::default()
        };
        let files = vec![PathBuf::from("/t/a b.png"), PathBuf::from("/t/c.png")];
        let l = command_lines(&app, &files, None).unwrap();
        assert_eq!(
            l,
            vec![vec![
                "viewer",
                "--icon",
                "vicon",
                "--title=Viewer",
                "--from",
                "/apps/v.desktop",
                "/t/a b.png",
                "/t/c.png",
                "100%"
            ]]
        );
        // %u: one launch per file, as URIs
        let app2 = App {
            exec: "web --new %u".into(),
            terminal: true,
            ..app.clone()
        };
        let l = command_lines(&app2, &files, Some(&["foot".to_string()])).unwrap();
        assert_eq!(
            l,
            vec![
                vec!["foot", "web", "--new", "file:///t/a%20b.png"],
                vec!["foot", "web", "--new", "file:///t/c.png"],
            ]
        );
        // %U list, no icon → %i vanishes; no files → code dropped
        let app3 = App {
            exec: "x %i %U".into(),
            icon: String::new(),
            ..app.clone()
        };
        assert_eq!(
            command_lines(&app3, &files[..1], None).unwrap(),
            vec![vec!["x", "file:///t/a%20b.png"]]
        );
        assert_eq!(command_lines(&app3, &[], None).unwrap(), vec![vec!["x"]]);
        // no field code: launched once without files
        let app4 = App {
            exec: "calc".into(),
            ..app.clone()
        };
        assert_eq!(
            command_lines(&app4, &files, None).unwrap(),
            vec![vec!["calc"]]
        );
    }

    #[test]
    fn mime_db_globs_parents_and_magic() {
        let t = Tmp::new("mime");
        t.write(
            "sys/share/mime/globs2",
            "# x\n50:text/x-csrc:*.c\n50:application/gzip:*.gz\n50:application/x-compressed-tar:*.tar.gz\n10:text/x-makefile:makefile\n50:text/x-readme:README*:cs\n0:model/x:__NOGLOBS__\n50:image/png:*.png\n",
        );
        t.write(
            "sys/share/mime/subclasses",
            "text/x-csrc text/plain\napplication/x-compressed-tar application/gzip\n",
        );
        t.write(
            "sys/share/mime/aliases",
            "application/x-gzip application/gzip\n",
        );
        let db = MimeDb::load(&t.env().mime_dirs());
        assert_eq!(db.by_name("a.C").as_deref(), Some("text/x-csrc"));
        assert_eq!(
            db.by_name("x.tar.gz").as_deref(),
            Some("application/x-compressed-tar")
        );
        assert_eq!(db.by_name("x.gz").as_deref(), Some("application/gzip"));
        assert_eq!(db.by_name("Makefile").as_deref(), Some("text/x-makefile"));
        assert_eq!(db.by_name("README.md").as_deref(), Some("text/x-readme"));
        assert_eq!(db.by_name("readme.md"), None);
        assert_eq!(db.by_name("noext"), None);
        assert_eq!(
            db.chain("application/x-compressed-tar"),
            ["application/x-compressed-tar", "application/gzip"]
        );
        assert_eq!(db.chain("text/x-csrc"), ["text/x-csrc", "text/plain"]);
        assert_eq!(db.chain("text/x-other"), ["text/x-other", "text/plain"]);
        assert_eq!(db.unalias("application/x-gzip"), "application/gzip");
        // no DB at all: mime_guess fallback
        assert_eq!(
            MimeDb::default().by_name("a.png").as_deref(),
            Some("image/png")
        );

        assert_eq!(sniff(b"%PDF-1.7"), "application/pdf");
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxx"), "image/png");
        assert_eq!(sniff(b"#!/bin/bash\necho"), "application/x-shellscript");
        assert_eq!(sniff(b"hello world"), "text/plain");
        assert_eq!(sniff(b""), "text/plain");
        assert_eq!(sniff(b"<?xml version='1.0'?><svg>"), "image/svg+xml");
        assert_eq!(sniff(b"ab\0cd"), "application/octet-stream");
        assert_eq!(sniff("h\u{e9}".as_bytes()), "text/plain");
        assert_eq!(sniff(&"\u{e9}".as_bytes()[..1]), "text/plain"); // cut char
        assert_eq!(sniff(b"\xff\xfe\xfa"), "application/octet-stream");
        assert!(glob_match(
            &"*.[0-9]".chars().collect::<Vec<_>>(),
            &"ls.1".chars().collect::<Vec<_>>()
        ));
        assert!(!glob_match(
            &"*.[!0-9]".chars().collect::<Vec<_>>(),
            &"ls.1".chars().collect::<Vec<_>>()
        ));
    }

    #[test]
    fn defaults_follow_mimeapps_chain_and_cache() {
        let t = Tmp::new("assoc");
        let e = t.env();
        t.write("sys/share/mime/subclasses", "text/markdown text/plain\n");
        t.write(
            "sys/share/mime/globs2",
            "50:text/markdown:*.md\n50:image/png:*.png\n",
        );
        let sys = "sys/share/applications";
        t.write(
            &format!("{sys}/gedit.desktop"),
            &desk("Gedit", "gedit %U", "text/plain;", ""),
        );
        t.write(
            &format!("{sys}/mark.desktop"),
            &desk("Mark", "mark %f", "text/markdown;", ""),
        );
        t.write(
            &format!("{sys}/kde/okular.desktop"),
            &desk("Okular", "okular %U", "image/png;", ""),
        );
        t.write(
            &format!("{sys}/ghost.desktop"),
            &desk("Ghost", "ghost", "image/png;", "TryExec=no-such-bin-xyz"),
        );
        t.write(
            &format!("{sys}/hidden.desktop"),
            &desk("Hidden", "h", "image/png;", "NoDisplay=true"),
        );
        t.write(
            &format!("{sys}/masked.desktop"),
            &desk("Masked", "m", "image/png;", ""),
        );
        // user copy with Hidden=true masks the system one
        t.write(
            "home/share/applications/masked.desktop",
            "[Desktop Entry]\nHidden=true\n",
        );
        t.write(
            &format!("{sys}/mimeinfo.cache"),
            "[MIME Cache]\nimage/png=hidden.desktop;kde-okular.desktop;\n",
        );
        // system default for markdown; user default for png (desktop-specific
        // list wins over the generic one); an uninstalled default is skipped
        t.write(
            &format!("{sys}/mimeapps.list"),
            "[Default Applications]\ntext/markdown=mark.desktop\ntext/plain=gedit.desktop\n",
        );
        t.write(
            "home/config/sway-mimeapps.list",
            "[Default Applications]\nimage/png=gone.desktop;kde-okular.desktop;\n",
        );
        t.write(
            "home/config/mimeapps.list",
            "[Default Applications]\nimage/png=hidden.desktop\n[Added Associations]\ntext/markdown=gedit.desktop;\n[Removed Associations]\nimage/png=hidden.desktop;\n",
        );
        let r = Registry::load(&e);
        let ids: Vec<&str> = r.apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "gedit.desktop",
                "hidden.desktop",
                "mark.desktop",
                "kde-okular.desktop"
            ]
        );
        let listed: Vec<&str> = r.listed().map(|a| a.name.as_str()).collect();
        assert_eq!(listed, ["Gedit", "Mark", "Okular"]);
        assert_eq!(r.get("kde-okular").unwrap().name, "Okular");

        let c = r.choices("image/png");
        assert_eq!(c.default.as_deref(), Some("kde-okular.desktop"));
        assert_eq!(c.apps, ["kde-okular.desktop"]); // hidden.desktop removed
        let c = r.choices("text/markdown");
        assert_eq!(c.default.as_deref(), Some("mark.desktop"));
        // added association, then the parent type's apps
        assert_eq!(c.apps, ["mark.desktop", "gedit.desktop"]);
        // unknown type: no apps
        assert_eq!(r.choices("application/x-unknown").default, None);

        let md = t.write("files/notes.md", "# hi");
        assert_eq!(r.mime_of(&md), "text/markdown");
        let noext = t.write("files/script", "#!/bin/sh\n");
        assert_eq!(r.mime_of(&noext), "application/x-shellscript");
        assert_eq!(r.mime_of(&t.0.join("files")), "inode/directory");
    }

    #[test]
    fn set_default_keeps_the_rest() {
        let text = "# my comments\n[Added Associations]\nimage/png=a.desktop;\n\n[Default Applications]\ntext/plain=old.desktop;\nimage/png=b.desktop;\n\n[Removed Associations]\nx/y=z.desktop;\n";
        let out = set_default_in(text, "text/plain", "new.desktop");
        assert_eq!(
            out,
            text.replace("text/plain=old.desktop;", "text/plain=new.desktop;")
        );
        let out = set_default_in(text, "application/pdf", "pdf.desktop");
        assert!(out.contains("image/png=b.desktop;\napplication/pdf=pdf.desktop;\n\n[Removed"));
        assert!(out.starts_with("# my comments\n[Added Associations]\nimage/png=a.desktop;\n"));
        let out = set_default_in("[Added Associations]\na/b=c;", "a/b", "d.desktop");
        assert_eq!(
            out,
            "[Added Associations]\na/b=c;\n\n[Default Applications]\na/b=d.desktop;\n"
        );
        assert_eq!(
            set_default_in("", "a/b", "d"),
            "[Default Applications]\na/b=d;\n"
        );

        let t = Tmp::new("setdef");
        let f = t.0.join("cfg/mimeapps.list");
        set_default(&f, "text/plain", "x.desktop").unwrap();
        set_default(&f, "image/png", "y.desktop").unwrap();
        assert_eq!(
            fs::read_to_string(&f).unwrap(),
            "[Default Applications]\ntext/plain=x.desktop;\nimage/png=y.desktop;\n"
        );
        // through a symlink: the target is updated, the link kept
        let link = t.0.join("link.list");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        set_default(&link, "text/plain", "z.desktop").unwrap();
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::read_to_string(&f)
            .unwrap()
            .contains("text/plain=z.desktop;"));
    }

    #[test]
    fn launches_detached_and_reaps() {
        // a harmless sandboxed "app": writes its args into a marker file
        let t = Tmp::new("launch");
        let marker = t.0.join("marker");
        let app = App {
            id: "m.desktop".into(),
            name: "Marker".into(),
            exec: format!(
                "/bin/sh -c \"printf '%%s|' \\\"\\$@\\\" > {}\" sh %F",
                marker.display()
            ),
            ..App::default()
        };
        let files = vec![t.0.join("a b.txt"), t.0.join("c.txt")];
        let pids = launch(&app, &files).unwrap();
        assert_eq!(pids.len(), 1);
        for _ in 0..200 {
            if fs::read_to_string(&marker).is_ok_and(|s| s.ends_with("c.txt|")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            format!("{}|{}|", files[0].display(), files[1].display())
        );
        assert!(spawn_detached(&["/no/such/prog".into()], None).is_err());
        assert_eq!(describe(&files), "2 files");
    }
}
