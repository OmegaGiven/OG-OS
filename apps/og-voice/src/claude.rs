//! Shells out to the configured AI CLI in one-shot, non-interactive mode.
//!
//! Confirmed via `claude --help` on this machine: `-p`/`--print` runs a
//! single turn, prints the response, and exits.
//!
//! Output format, empirically tested (throwaway `claude -p "run \`echo
//! test123\` using the Bash tool and tell me the output"` with each
//! candidate flag, this machine, `claude-code` 2.1.226):
//!
//! - `--output-format json` gives a single JSON object at the end (the
//!   same shape as the final line of `stream-json` below) with a
//!   `"result"` field for the final text — but nothing about what tool
//!   calls happened along the way. No good for capturing actions.
//! - `--output-format stream-json --verbose` (the `--verbose` is
//!   mandatory — omitting it errors with "When using --print,
//!   --output-format=stream-json requires --verbose") streams one JSON
//!   object per line (NDJSON) for every turn/event:
//!     - `{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"echo test123",...}},...]}}`
//!       — one such element per tool call; `name` is the tool
//!       (`"Bash"`/`"Edit"`/`"Write"`/`"Read"`/... — whatever Claude Code
//!       exposes), `input` is the tool's raw arguments (shape varies by
//!       tool).
//!     - `{"type":"user","message":{"content":[{"type":"tool_result",...}]}}`
//!       — the result of that tool call, not needed for the action log.
//!     - `{"type":"result","subtype":"success","is_error":false,"result":"Output: `test123`",...}`
//!       — the final line: `result` is the same trimmed final-answer text
//!       `--output-format json`'s `"result"` field has, `is_error` flags
//!       a failed turn (its `result`, when present, is then an error
//!       description rather than an answer).
//!   This is the format used here, since it's the only one that surfaces
//!   tool-use.
//!
//! Also confirmed via `claude --help`: `--append-system-prompt <prompt>`
//! adds to the CLI's own default system prompt rather than replacing it
//! (that's `--system-prompt`, which would strip Claude Code's own
//! tool-use/behavior instructions — not what's wanted here, this is
//! purely additive context: the user's working preferences and known
//! hosts from `~/.config/sway-power/ai-context.json`, see `ai_context.rs`).
//!
//! Skill auto-discovery, empirically tested (throwaway `.claude/skills/
//! <name>/SKILL.md` dir + `claude -p "trigger phrase" --output-format
//! json`, no interactive session involved): scripted `-p` mode DOES
//! auto-discover and invoke project skills, exactly like interactive
//! mode. It needs the literal `.claude/skills/<slug>/SKILL.md` layout
//! (not a flat custom directory name) and resolves it either relative to
//! the process's cwd, or — confirmed working too, and what's used here
//! since it avoids changing this process's own cwd — via `--add-dir
//! <parent-of-.claude>`. So: skills live in
//! `~/.config/sway-power/.claude/skills/<slug>/SKILL.md` (written by
//! og-settings' `ai_skills.rs`) and `--add-dir ~/.config/sway-power` is
//! passed on every call so Claude Code's own discovery finds them there.

use std::path::PathBuf;
use std::process::Command;

use crate::history::Action;
use crate::session::{self, Continuity};

fn skills_add_dir() -> PathBuf {
    og_config::dirs_home().join(".config/sway-power")
}

/// The final answer text plus every tool call Claude made while
/// producing it (empty if it answered without taking any action).
#[derive(Debug, Clone)]
pub struct AskResult {
    pub answer: String,
    pub actions: Vec<Action>,
}

/// Runs `<cli> -p "<prompt>" --output-format stream-json --verbose`
/// (optionally with `--append-system-prompt <system_prompt>` first, and
/// always with `--add-dir ~/.config/sway-power` so Claude Code's own
/// `.claude/skills/` auto-discovery can find the skills library
/// og-settings writes there — see module docs), parses the NDJSON
/// stream, and returns the final answer text plus the tool-use actions
/// taken along the way. `cli` comes from `og_config::Config::default_ai_cli`,
/// falling back to `"claude"` if that field is empty (matches og-search's
/// own fallback behavior for the same field). `system_prompt` of
/// `None`/empty skips that flag entirely rather than passing it empty.
/// `allow_execution` comes from `VoiceConfig::allow_execution`
/// (`voice-config.json`, toggleable from og-settings' AI Context tab) —
/// headless `-p` mode has no interactive prompt to approve/deny tool use,
/// so without `--dangerously-skip-permissions` every action (Bash/SSH/
/// file edits) is silently denied and Claude can only describe what it
/// would do. Defaults to `false`/off; the flag is only appended when the
/// user has explicitly opted in.
/// Env vars a Claude Code session sets on itself and everything it
/// spawns (`CLAUDECODE`, session id, its IPC socket, ...). If og-voice
/// is ever launched from a terminal that's a *descendant* of a running
/// Claude Code session (common while developing/testing og-voice
/// itself, from an editor's integrated terminal, etc.) — as opposed to
/// its normal launch path, a clean `exec` straight from sway with no
/// such ancestor — these leak into this `claude` subprocess and make it
/// defer permission decisions up through the parent session's socket
/// instead of honoring `--permission-mode bypassPermissions` cleanly,
/// silently turning every tool call into a denial. Stripped
/// unconditionally so `ask()` behaves the same regardless of what
/// launched og-voice.
const CLAUDE_CODE_SESSION_ENV_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
];

pub fn ask(cli: &str, prompt: &str, system_prompt: Option<&str>, allow_execution: bool) -> Result<AskResult, String> {
    let cli = if cli.trim().is_empty() { "claude" } else { cli.trim() };
    let mut command = Command::new(cli);
    for var in CLAUDE_CODE_SESSION_ENV_VARS {
        command.env_remove(var);
    }
    if allow_execution {
        // `--dangerously-skip-permissions` bypassed *everything*,
        // `sudo` included — too broad for a voice popup that fires off
        // whatever it heard. `--permission-mode bypassPermissions` gets
        // the same "don't hang waiting for an approval prompt that can
        // never come" behavior headless `-p` mode needs, while
        // `--disallowedTools` explicitly blocks `sudo` — deny rules take
        // precedence over the bypass mode in Claude Code's permission
        // system, so this is a real block, not just a hint.
        command.arg("--permission-mode").arg("bypassPermissions");
        command.arg("--disallowedTools").arg("Bash(sudo *)");
    }
    command.arg("--add-dir").arg(skills_add_dir());
    if let Some(sp) = system_prompt.filter(|s| !s.trim().is_empty()) {
        command.arg("--append-system-prompt").arg(sp);
    }
    // Cross-turn conversation continuity — see session.rs. `Fresh` starts
    // a session under our own chosen id so a later turn can resume it;
    // `Resume` continues one already started within the last hour;
    // `None` (I/O hiccup minting the id) falls back to the old
    // stateless-every-turn behavior rather than failing the request.
    match session::current() {
        Continuity::Fresh(id) => {
            command.arg("--session-id").arg(id);
        }
        Continuity::Resume(id) => {
            command.arg("--resume").arg(id);
        }
        Continuity::None => {}
    }
    command.arg("--output-format").arg("stream-json").arg("--verbose");
    let output = command
        .arg("-p")
        .arg(prompt)
        .output()
        .map_err(|e| format!("failed to run `{cli}`: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("`{cli}` exited with {}", output.status)
        } else {
            stderr
        });
    }

    parse_stream_json(&String::from_utf8_lossy(&output.stdout), cli)
}

/// Walks the NDJSON stream line by line (unparseable lines — hook
/// chatter, blank lines — are skipped rather than treated as fatal, same
/// "best effort" spirit as the rest of this app's file/process I/O),
/// collecting `tool_use` entries from every `"assistant"` event and
/// pulling the final answer (or error) out of the terminal `"result"`
/// event. See module docs for the exact shapes matched here.
fn parse_stream_json(stdout: &str, cli: &str) -> Result<AskResult, String> {
    let mut actions = Vec::new();
    let mut answer: Option<String> = None;
    let mut error: Option<String> = None;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        match event.get("type").and_then(|v| v.as_str()) {
            Some("assistant") => {
                if let Some(content) = event.pointer("/message/content").and_then(|v| v.as_array()) {
                    for item in content {
                        if item.get("type").and_then(|v| v.as_str()) != Some("tool_use") {
                            continue;
                        }
                        let tool = item.get("name").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
                        let input = item.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        actions.push(Action { tool, summary: summarize_tool_input(&input) });
                    }
                }
            }
            Some("result") => {
                let is_error = event.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                let result_text = event.get("result").and_then(|v| v.as_str()).map(|s| s.trim().to_string());
                if is_error {
                    error = Some(result_text.filter(|s| !s.is_empty()).unwrap_or_else(|| format!("`{cli}` reported an error")));
                } else {
                    answer = result_text;
                }
            }
            _ => {}
        }
    }

    if let Some(err) = error {
        return Err(err);
    }

    match answer {
        Some(text) if !text.is_empty() => Ok(AskResult { answer: text, actions }),
        _ => Err(format!("`{cli}` returned no output")),
    }
}

/// A short, human-readable description of a tool call, derived from its
/// input rather than dumping raw JSON — the first field found among the
/// common ones tools actually use (`command` for Bash, `file_path` for
/// Edit/Write/Read, `pattern` for Grep/Glob, `url` for WebFetch,
/// `description` as a general fallback), falling back to the input's
/// compact JSON form if none of those are present. Truncated so one
/// runaway tool call can't blow up a history entry.
fn summarize_tool_input(input: &serde_json::Value) -> String {
    const MAX_CHARS: usize = 200;

    let raw = ["command", "file_path", "path", "pattern", "url", "description"]
        .iter()
        .find_map(|key| input.get(key).and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| input.to_string());

    if raw.chars().count() > MAX_CHARS {
        let truncated: String = raw.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        raw
    }
}
