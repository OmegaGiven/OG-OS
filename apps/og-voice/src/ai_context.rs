//! Reads the user-authored AI context og-settings' "AI Context" tab
//! writes to `~/.config/sway-power/ai-context.json` (general working
//! preferences + known tailnet/SSH hosts), plus the Skills library it
//! writes to `~/.config/sway-power/.claude/skills/*/SKILL.md`, and turns
//! both into a system prompt string for the `claude` CLI call in
//! `claude.rs`.
//!
//! Load-once at startup, same convention as `VoiceConfig::load` — if the
//! file doesn't exist yet (og-settings' tab was never opened/saved),
//! that's not an error, just no extra context to add.
//!
//! Deliberately a standalone struct here rather than a dependency on
//! og-settings/sway-control (which isn't a library) — this only ever
//! reads the file, never writes it, so a small independent copy of the
//! schema is simpler than introducing a shared crate for one read.
//!
//! Skills index: every backend gets a compact name+description index of
//! all skills folded into the system prompt unconditionally — this is
//! what a future local-LLM backend (which has no skill-invocation
//! mechanism of its own) would rely on entirely. The current Claude CLI
//! backend additionally gets real skill auto-discovery for full-body
//! retrieval on demand: `claude.rs` passes `--add-dir
//! ~/.config/sway-power` so Claude Code's own `.claude/skills/`
//! discovery finds these directories relative to that added dir
//! (confirmed empirically — see `claude.rs`).

use serde::Deserialize;
use std::path::PathBuf;

fn ai_context_path() -> PathBuf {
    let mut p = og_config::dirs_home();
    p.push(".config/sway-power/ai-context.json");
    p
}

fn skills_dir() -> PathBuf {
    let mut p = og_config::dirs_home();
    p.push(".config/sway-power/.claude/skills");
    p
}

/// Name + description of one skill, parsed from a `SKILL.md`'s
/// frontmatter. The body is deliberately not read here — the compact
/// index is all every backend gets unconditionally; the Claude CLI
/// backend fetches full bodies itself via its own skill-invocation
/// mechanism (see module docs above).
struct SkillSummary {
    name: String,
    description: String,
}

/// Parses just the `name:`/`description:` lines out of the
/// `---`-delimited frontmatter og-settings' `ai_skills.rs` writes.
/// Anything that doesn't match is skipped, not an error.
fn parse_frontmatter(text: &str) -> Option<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];

    let mut name = String::new();
    let mut description = String::new();
    for line in frontmatter.lines() {
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("description:") {
            description = v.trim().to_string();
        }
    }
    Some((name, description))
}

/// Scans `~/.config/sway-power/.claude/skills/*/SKILL.md`. Missing
/// directory or unreadable/malformed entries are skipped, not errors.
fn load_skills_index() -> Vec<SkillSummary> {
    let Ok(entries) = std::fs::read_dir(skills_dir()) else {
        return Vec::new();
    };
    let mut skills: Vec<SkillSummary> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
            let (name, description) = parse_frontmatter(&text)?;
            if name.is_empty() {
                return None;
            }
            Some(SkillSummary { name, description })
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AiContext {
    #[serde(default)]
    general_notes: String,
    #[serde(default)]
    hosts: Vec<AiContextHost>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct AiContextHost {
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    notes: String,
}

/// Builds the system-prompt string to pass via `--append-system-prompt`.
/// Returns `None` if there's nothing to add (missing/empty file) so the
/// caller can skip the flag entirely rather than passing an empty prompt.
pub fn system_prompt() -> Option<String> {
    // Fall back to empty context rather than bailing out early — unlike
    // before the Skills index existed, a missing/corrupt ai-context.json
    // shouldn't also suppress the skills index below.
    let ctx: AiContext = std::fs::read_to_string(ai_context_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut parts: Vec<String> = Vec::new();

    let notes = ctx.general_notes.trim();
    if !notes.is_empty() {
        parts.push(notes.to_string());
    }

    if !ctx.hosts.is_empty() {
        let mut lines = vec!["Known tailnet/SSH hosts you may be asked to act on:".to_string()];
        for h in &ctx.hosts {
            if h.name.trim().is_empty() {
                continue;
            }
            let addr = if h.address.trim().is_empty() {
                String::new()
            } else {
                format!(" ({})", h.address.trim())
            };
            let notes = if h.notes.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", h.notes.trim())
            };
            lines.push(format!("- `{}`{}{}", h.name.trim(), addr, notes));
        }
        if lines.len() > 1 {
            parts.push(lines.join("\n"));
        }
    }

    let skills = load_skills_index();
    if !skills.is_empty() {
        // Name + description only — always included for every backend
        // (including a future local-LLM one with no skill-invocation
        // mechanism of its own). The Claude CLI backend additionally
        // gets real full-body auto-discovery via `--add-dir` (see
        // `claude.rs`); this index is what lets it know a skill exists
        // to reach for in the first place, and is the local-LLM
        // backend's only signal.
        let mut lines = vec!["Named skills available (ask the user or use context to decide when to apply one; full instructions are auto-loaded on demand if you have skill access):".to_string()];
        for s in &skills {
            if s.description.trim().is_empty() {
                lines.push(format!("- `{}`", s.name));
            } else {
                lines.push(format!("- `{}` — {}", s.name, s.description.trim()));
            }
        }
        parts.push(lines.join("\n"));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}
