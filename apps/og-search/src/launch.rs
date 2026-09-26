use std::process::Command;
use crate::config::Config;

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn browser(config: &Config) -> &str {
    if config.default_browser.is_empty() { "xdg-open" } else { &config.default_browser }
}

fn terminal(config: &Config) -> &str {
    if config.terminal.is_empty() { "alacritty" } else { &config.terminal }
}

pub fn web_search(config: &Config, query: &str) {
    let url = format!("https://www.google.com/search?q={}", urlencode(query));
    let _ = Command::new(browser(config)).arg(url).spawn();
}

/// `go/<alias>` is meant as a direct DNS-style lookup (an internal short-link
/// service), not a search — passed as an explicit `http://` URL so the
/// browser never mistakes it for a search query.
pub fn go_alias(config: &Config, alias: &str) {
    let url = format!("http://go/{alias}");
    let _ = Command::new(browser(config)).arg(url).spawn();
}

/// AI CLIs are interactive terminal tools, so this runs in the configured
/// terminal with the query passed as a prompt argument, then waits for a
/// keypress so the response stays on screen instead of vanishing instantly.
pub fn ask_ai(config: &Config, query: &str) {
    if config.default_ai_cli.is_empty() {
        return;
    }
    let script = format!(
        "{} {:?}; echo; read -rsn1 -p 'Press any key to close...'",
        config.default_ai_cli, query,
    );
    let _ = Command::new(terminal(config)).arg("-e").arg("bash").arg("-c").arg(script).spawn();
}

/// Launches an already-cleaned `Exec=` command line. Entries whose desktop
/// file had `Terminal=true` are prefixed with a marker by `apps::parse_desktop_file`
/// so they open in the configured terminal instead of headless (a CLI tool
/// with no terminal would just flash and disappear).
pub fn launch_app(config: &Config, exec: &str) {
    let (in_terminal, cmd) = match exec.strip_prefix("__TERMINAL__") {
        Some(rest) => (true, rest),
        None => (false, exec),
    };
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return;
    }
    if in_terminal {
        let _ = Command::new(terminal(config)).arg("-e").args(&parts).spawn();
    } else {
        let _ = Command::new(parts[0]).args(&parts[1..]).spawn();
    }
}

/// Opens a file-search result with whatever the desktop considers its
/// default handler — a directory opens in the file manager, a document in
/// its associated app, exactly like double-clicking it would.
pub fn open_file(path: &std::path::Path) {
    let _ = Command::new("xdg-open").arg(path).spawn();
}

/// Settings-search result: jump straight to the matching og-settings tab.
/// Absolute path, not bare "og-settings" — sway's own process environment
/// has no ~/.local/bin on PATH (same bug class fixed earlier for
/// og-power-apply), and every og-settings launch elsewhere in this
/// toolkit already uses the full path for exactly that reason.
pub fn open_settings_tab(tab_arg: &str) {
    let home = std::env::var("HOME").unwrap_or_default();
    let _ = Command::new(format!("{home}/.local/bin/og-settings")).arg("--tab").arg(tab_arg).spawn();
}
