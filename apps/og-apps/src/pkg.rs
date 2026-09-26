use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Pacman,
    Aur,
    Flatpak,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Pacman => "Pacman",
            Source::Aur => "AUR",
            Source::Flatpak => "Flatpak",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub source: Source,
    /// pacman/AUR package name, or flatpak application id.
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub installed: bool,
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

// ── Search ───────────────────────────────────────────────────────────────

/// `pacman -Ss <query>` prints two lines per match:
///   repo/name version [installed]
///       description
pub fn search_pacman(query: &str) -> Vec<AppEntry> {
    let Ok(out) = Command::new("pacman").args(["-Ss", query]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(header) = lines.next() {
        if header.starts_with(' ') || header.is_empty() {
            continue;
        }
        let installed = header.contains("[installed]");
        let mut parts = header.split_whitespace();
        let Some(repo_name) = parts.next() else { continue };
        let Some(version) = parts.next() else { continue };
        let name = repo_name.split_once('/').map(|(_, n)| n).unwrap_or(repo_name).to_string();
        let description = lines.peek().filter(|l| l.starts_with(' ')).map(|l| l.trim().to_string()).unwrap_or_default();
        if lines.peek().map(|l| l.starts_with(' ')).unwrap_or(false) {
            lines.next();
        }
        entries.push(AppEntry {
            source: Source::Pacman,
            id: name.clone(),
            name,
            description,
            version: version.to_string(),
            installed,
        });
    }
    entries
}

/// `yay -Ss --aur <query>` — same two-line shape as pacman, plus a
/// `(+votes pct%)` block on the header line we just ignore.
pub fn search_aur(query: &str) -> Vec<AppEntry> {
    let Ok(out) = Command::new("yay").args(["-Ss", "--aur", query]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut entries = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(header) = lines.next() {
        if header.starts_with(' ') || header.is_empty() {
            continue;
        }
        let installed = header.contains("[installed]");
        let mut parts = header.split_whitespace();
        let Some(repo_name) = parts.next() else { continue };
        let Some(version) = parts.next() else { continue };
        let name = repo_name.split_once('/').map(|(_, n)| n).unwrap_or(repo_name).to_string();
        let description = lines.peek().filter(|l| l.starts_with(' ')).map(|l| l.trim().to_string()).unwrap_or_default();
        if lines.peek().map(|l| l.starts_with(' ')).unwrap_or(false) {
            lines.next();
        }
        entries.push(AppEntry {
            source: Source::Aur,
            id: name.clone(),
            name,
            description,
            version: version.to_string(),
            installed,
        });
    }
    entries
}

/// `flatpak search <query>` — tab-separated: Name, Description, AppId,
/// Version, Branch, Remote. "No matches found" on a miss.
pub fn search_flatpak(query: &str) -> Vec<AppEntry> {
    let Ok(out) = Command::new("flatpak").args(["search", query]).output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    let installed_ids = installed_flatpak_ids();
    text.lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 4 {
                return None;
            }
            let id = cols[2].to_string();
            Some(AppEntry {
                source: Source::Flatpak,
                installed: installed_ids.contains(&id),
                name: cols[0].to_string(),
                description: cols[1].to_string(),
                id,
                version: cols[3].to_string(),
            })
        })
        .collect()
}

pub fn search_all(query: &str) -> Vec<AppEntry> {
    let mut results = search_pacman(query);
    results.extend(search_aur(query));
    results.extend(search_flatpak(query));
    results
}

// ── Installed ────────────────────────────────────────────────────────────

fn installed_flatpak_ids() -> Vec<String> {
    let Ok(out) = Command::new("flatpak").args(["list", "--app", "--columns=application"]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Explicitly-installed pacman packages (`-Qe`, excludes pulled-in deps),
/// with AUR-origin ones (`-Qm`, foreign/not-in-sync-db) relabeled correctly
/// instead of showing everything as plain pacman.
pub fn list_installed_pacman_and_aur() -> Vec<AppEntry> {
    let explicit = Command::new("pacman").arg("-Qe").output().ok();
    let foreign: std::collections::HashSet<String> = Command::new("pacman")
        .arg("-Qm")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.split_whitespace().next().map(str::to_string)).collect())
        .unwrap_or_default();

    let Some(out) = explicit else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let name = parts.next()?.to_string();
            let version = parts.next().unwrap_or("").to_string();
            let source = if foreign.contains(&name) { Source::Aur } else { Source::Pacman };
            Some(AppEntry {
                source,
                id: name.clone(),
                name,
                description: String::new(),
                version,
                installed: true,
            })
        })
        .collect()
}

pub fn list_installed_flatpak() -> Vec<AppEntry> {
    let Ok(out) = Command::new("flatpak").args(["list", "--app", "--columns=name,application,version"]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() < 2 {
                return None;
            }
            Some(AppEntry {
                source: Source::Flatpak,
                name: cols[0].to_string(),
                id: cols[1].to_string(),
                version: cols.get(2).unwrap_or(&"").to_string(),
                description: String::new(),
                installed: true,
            })
        })
        .collect()
}

pub fn list_installed_all() -> Vec<AppEntry> {
    let mut entries = list_installed_pacman_and_aur();
    entries.extend(list_installed_flatpak());
    entries
}

// ── Install ──────────────────────────────────────────────────────────────

/// Interactive installs need a real terminal (sudo password, pacman/yay
/// conflict prompts) — same spawn pattern used throughout og-apps
/// (e.g. its `update_pacman`/`wifi_connect`): open the user's terminal,
/// run the command, pause so errors are visible, fire-and-forget from here.
fn spawn_terminal(terminal: &str, script: &str) {
    let term = if terminal.is_empty() { "alacritty" } else { terminal };
    let _ = Command::new(term).arg("-e").arg("bash").arg("-c").arg(script).spawn();
}

pub fn install_pacman(terminal: &str, pkg: &str) {
    spawn_terminal(terminal, &format!("sudo pacman -S {pkg}; echo; read -rsn1 -p 'Press any key to close...'"));
}

pub fn install_aur(terminal: &str, pkg: &str) {
    spawn_terminal(terminal, &format!("yay -S {pkg}; echo; read -rsn1 -p 'Press any key to close...'"));
}

pub fn install_flatpak(terminal: &str, id: &str) {
    spawn_terminal(terminal, &format!("flatpak install -y flathub {id}; echo; read -rsn1 -p 'Press any key to close...'"));
}

// ── Uninstall ────────────────────────────────────────────────────────────

/// Best-effort guess at leftover user config/data dirs pacman doesn't know
/// about (it only tracks package-owned files, mostly under /etc — never a
/// user's `~/.config/<app>` created at runtime). Name-matched, case
///-insensitive, shown to the user before anything is deleted.
pub fn find_leftover_config_dirs(pkg: &str) -> Vec<String> {
    let candidates = [format!("{}/.config", home()), format!("{}/.local/share", home())];
    let pkg_lower = pkg.to_lowercase();
    let mut found = Vec::new();
    for dir in candidates {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(file_name) = entry.file_name().into_string() else { continue };
            if file_name.to_lowercase().contains(&pkg_lower) {
                if let Some(p) = entry.path().to_str() {
                    found.push(p.to_string());
                }
            }
        }
    }
    found
}

pub fn uninstall_pacman(terminal: &str, pkg: &str, wipe_config: bool) {
    let flag = if wipe_config { "-Rns" } else { "-Rs" };
    spawn_terminal(terminal, &format!("sudo pacman {flag} {pkg}; echo; read -rsn1 -p 'Press any key to close...'"));
}

pub fn uninstall_flatpak(terminal: &str, id: &str, wipe_config: bool) {
    let extra = if wipe_config { " --delete-data" } else { "" };
    spawn_terminal(terminal, &format!("flatpak uninstall{extra} -y {id}; echo; read -rsn1 -p 'Press any key to close...'"));
}

/// Deletes the specific leftover dirs a user has confirmed (from
/// `find_leftover_config_dirs`) — direct removal, not spawned, since this
/// only touches the user's own home dir and needs no sudo.
pub fn delete_leftover_dirs(paths: &[String]) {
    for p in paths {
        let _ = std::fs::remove_dir_all(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacman_search_finds_known_package() {
        let results = search_pacman("htop");
        assert!(results.iter().any(|e| e.id == "htop"), "expected htop in {results:?}");
        let htop = results.iter().find(|e| e.id == "htop").unwrap();
        assert!(!htop.description.is_empty());
        assert!(!htop.version.is_empty());
    }

    #[test]
    #[ignore = "needs flatpak with the flathub remote configured; run with `cargo test -- --ignored` on a dev machine"]
    fn flatpak_search_finds_known_app() {
        let results = search_flatpak("gimp");
        assert!(results.iter().any(|e| e.id == "org.gimp.GIMP"), "expected org.gimp.GIMP in {results:?}");
    }

    #[test]
    fn installed_pacman_list_nonempty_and_relabels_aur() {
        let installed = list_installed_pacman_and_aur();
        assert!(!installed.is_empty());
        // yay itself is AUR-built on this machine.
        if let Some(yay) = installed.iter().find(|e| e.id == "yay") {
            assert_eq!(yay.source, Source::Aur);
        }
    }
}
