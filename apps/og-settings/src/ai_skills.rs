//! Reusable named "skills" for AI agents (og-voice) — procedures the user
//! wants documented once and reused across voice sessions instead of
//! re-explained every time ("restarting Sonarr", "how I want commit
//! messages written", etc).
//!
//! Stored as real Claude-Code-format skill directories under
//! `~/.config/sway-power/.claude/skills/<slug>/SKILL.md`. That exact
//! `.claude/skills/<slug>/SKILL.md` layout was confirmed empirically
//! (see og-voice's `claude.rs`) to be what Claude Code's CLI
//! auto-discovers in scripted `-p` mode, either relative to its working
//! directory or via `--add-dir <parent-of-.claude>` — og-voice points
//! `--add-dir` at `~/.config/sway-power` so these resolve. This file on
//! disk is the single source of truth; og-settings only stages edits in
//! memory (`App::ai_skills`) and re-syncs the whole directory tree on
//! Apply & Save, same convention as `ai_context.rs`'s `AiContext`.
//!
//! Minimal hand-rolled frontmatter parser/writer rather than pulling in
//! a YAML crate — the format read is exactly the format written
//! (`---\nname: ...\ndescription: ...\n---\n\n<body>`), two flat string
//! fields, so a full YAML parser would be unnecessary weight.

use std::collections::HashSet;
use std::path::PathBuf;

fn skills_dir() -> PathBuf {
    let mut p = crate::config::dirs_home();
    p.push(".config/sway-power/.claude/skills");
    p
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AiSkill {
    /// Display name; also slugified into the directory/frontmatter
    /// `name:` field (see `slug()`).
    pub name: String,
    /// One-line description — this is what the future local-LLM backend
    /// index (name + description for every skill, folded into the system
    /// prompt) shows, and what Claude Code's own skill-matching logic
    /// reads to decide when to pull in the full body.
    pub description: String,
    pub body: String,
}

impl AiSkill {
    pub fn slug(&self) -> String {
        slugify(&self.name)
    }
}

/// Lowercase + hyphens — matches what a Claude Code skill directory name
/// should look like. Strips anything that isn't alphanumeric, collapses
/// runs of other characters into a single hyphen, trims leading/trailing
/// hyphens.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut last_was_hyphen = true; // suppresses a leading hyphen
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            out.push('-');
            last_was_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn render(skill: &AiSkill) -> String {
    let slug = skill.slug();
    // Frontmatter values are single-line scalars in the format this code
    // both writes and reads — flatten any embedded newlines rather than
    // producing invalid frontmatter.
    let description = skill.description.trim().replace('\n', " ");
    format!("---\nname: {slug}\ndescription: {description}\n---\n\n{}\n", skill.body.trim_end())
}

/// Parses the `---`-delimited frontmatter this module writes. Returns
/// `None` for anything that doesn't match the expected shape — the
/// caller skips malformed entries rather than erroring the whole load.
fn parse(text: &str) -> Option<(String, String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text); // tolerate BOM
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.trim_start_matches('\n').to_string();

    let mut name = String::new();
    let mut description = String::new();
    for line in frontmatter.lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    Some((name, description, body))
}

/// Scans `~/.config/sway-power/.claude/skills/*/SKILL.md`. Missing
/// directory or unreadable/malformed entries are skipped, not errors —
/// same fallback-to-empty philosophy as `AiContext::load`.
pub fn load_all() -> Vec<AiSkill> {
    let dir = skills_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut skills: Vec<AiSkill> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
            let (name, description, body) = parse(&text)?;
            let name = if name.is_empty() { e.file_name().to_string_lossy().to_string() } else { name };
            Some(AiSkill { name, description, body })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Re-syncs `~/.config/sway-power/.claude/skills/` to exactly match
/// `skills`: writes/overwrites a `SKILL.md` for each entry, deletes any
/// on-disk skill directory (by slug) not present in `skills`. Best-effort
/// per-directory — one bad entry doesn't block the rest of the save,
/// matching this tab's existing don't-block-the-user error handling.
pub fn save_all(skills: &[AiSkill]) -> Result<(), String> {
    let dir = skills_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let keep: HashSet<String> = skills.iter().map(|s| s.slug()).filter(|s| !s.is_empty()).collect();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            if entry.path().is_dir() {
                let dirname = entry.file_name().to_string_lossy().to_string();
                if !keep.contains(&dirname) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    let mut first_err = None;
    for skill in skills {
        let slug = skill.slug();
        if slug.is_empty() {
            continue;
        }
        let skill_dir = dir.join(&slug);
        if let Err(e) = std::fs::create_dir_all(&skill_dir) {
            first_err.get_or_insert(e.to_string());
            continue;
        }
        if let Err(e) = std::fs::write(skill_dir.join("SKILL.md"), render(skill)) {
            first_err.get_or_insert(e.to_string());
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
