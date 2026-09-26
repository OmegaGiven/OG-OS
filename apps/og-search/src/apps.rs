use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub name: String,
    pub exec: String,
}

/// Same `.desktop` scan as og-files' "Open With" registry, trimmed to
/// just what a launcher needs (name + exec, no mime types).
pub fn load_app_registry() -> Vec<AppEntry> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dirs = vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from(format!("{home}/.local/share/applications")),
        // Flatpak's own exports dirs — not under either path above, so
        // flatpak-installed apps (system or --user) were invisible here
        // even though they show up fine in every other launcher.
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        PathBuf::from(format!("{home}/.local/share/flatpak/exports/share/applications")),
    ];

    let mut entries = Vec::new();
    for dir in &dirs {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            if let Some(app) = parse_desktop_file(&path) {
                entries.push(app);
            }
        }
    }
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries.dedup_by(|a, b| a.name == b.name);
    entries
}

fn parse_desktop_file(path: &Path) -> Option<AppEntry> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut name = String::new();
    let mut exec = String::new();
    let mut no_display = false;
    let mut terminal = false;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let line = line.trim();
        if line == "[Desktop Entry]" {
            in_desktop_entry = true;
            continue;
        }
        if line.starts_with('[') {
            in_desktop_entry = false;
            continue;
        }
        if !in_desktop_entry {
            continue;
        }
        if let Some(v) = line.strip_prefix("Name=") {
            if name.is_empty() {
                name = v.to_string();
            }
        } else if let Some(v) = line.strip_prefix("Exec=") {
            exec = v.to_string();
        } else if let Some(v) = line.strip_prefix("NoDisplay=") {
            no_display = v.eq_ignore_ascii_case("true");
        } else if let Some(v) = line.strip_prefix("Terminal=") {
            terminal = v.eq_ignore_ascii_case("true");
        }
    }

    if no_display || name.is_empty() || exec.is_empty() {
        return None;
    }

    // Strip desktop-file field codes (%f, %F, %u, %U, %i, %c, %k) — we're
    // launching with no file/icon/etc context to substitute in. Also
    // strips flatpak's own `@@u ... @@` optional-file-args bracketing
    // (e.g. Bambu Studio's `Exec=... @@u %U @@`) — without this, removing
    // just the %U left bare `@@u @@` tokens behind for flatpak's arg
    // parser to choke on.
    let cleaned = exec
        .split_whitespace()
        .filter(|w| !(w.starts_with('%') && w.len() == 2) && *w != "@@" && !w.starts_with("@@"))
        .collect::<Vec<_>>()
        .join(" ");

    let final_exec = if terminal {
        format!("__TERMINAL__{cleaned}")
    } else {
        cleaned
    };

    Some(AppEntry { name, exec: final_exec })
}

/// Substring match against the app name, ranked so the most accurate match
/// comes first: exact name match, then starts-with, then contains-anywhere,
/// each tier alphabetical. Ties in accuracy fall back to name order rather
/// than leaving results in arbitrary registry-scan order.
pub fn filter_apps<'a>(apps: &'a [AppEntry], query: &str) -> Vec<&'a AppEntry> {
    if query.is_empty() {
        return apps.iter().collect();
    }
    let q = query.to_lowercase();
    let mut matches: Vec<&AppEntry> = apps.iter()
        .filter(|a| a.name.to_lowercase().contains(&q))
        .collect();

    let rank = |name: &str| -> u8 {
        let lower = name.to_lowercase();
        if lower == q { 0 } else if lower.starts_with(&q) { 1 } else { 2 }
    };
    matches.sort_by(|a, b| {
        rank(&a.name).cmp(&rank(&b.name)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    matches
}
