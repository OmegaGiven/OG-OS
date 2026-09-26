//! VPN tab backend: Tailscale (status/exit-node, via the `tailscale` CLI —
//! works unprivileged once `tailscale set --operator=$USER` has been run
//! once) and a generic WireGuard-based "public VPN" (whole-system or
//! per-app split-tunnel via an isolated network namespace), both driven
//! through the root-only `og-vpn-apply` helper for anything that needs
//! privilege (see scripts/og-vpn-apply + scripts/og-vpn-sudoers).

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

// ── Tailscale ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TailscalePeer {
    pub hostname: String,
    pub ip: String,
    pub online: bool,
    pub exit_node_option: bool,
    pub is_exit_node: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TailscaleStatus {
    pub installed: bool,
    pub running: bool,
    pub self_hostname: String,
    pub self_ip: Option<String>,
    pub peers: Vec<TailscalePeer>,
}

#[derive(Debug, Deserialize)]
struct RawPeer {
    #[serde(rename = "HostName")]
    hostname: String,
    #[serde(rename = "TailscaleIPs")]
    ips: Vec<String>,
    #[serde(rename = "Online")]
    online: bool,
    #[serde(rename = "ExitNodeOption")]
    exit_node_option: bool,
    #[serde(rename = "ExitNode")]
    exit_node: bool,
}

#[derive(Debug, Deserialize)]
struct RawSelf {
    #[serde(rename = "HostName")]
    hostname: String,
}

#[derive(Debug, Deserialize)]
struct RawStatus {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Vec<String>,
    #[serde(rename = "Self")]
    self_: RawSelf,
    #[serde(rename = "Peer")]
    peer: Option<HashMap<String, RawPeer>>,
}

pub fn tailscale_status() -> TailscaleStatus {
    let Ok(out) = Command::new("tailscale").args(["status", "--json"]).output() else {
        return TailscaleStatus::default();
    };
    if !out.status.success() {
        return TailscaleStatus { installed: true, ..Default::default() };
    }
    let Ok(raw) = serde_json::from_slice::<RawStatus>(&out.stdout) else {
        return TailscaleStatus { installed: true, ..Default::default() };
    };

    // All peers, not just exit-node-capable ones — the Network tab's exit
    // node picker filters on `exit_node_option` itself when rendering
    // (see network.rs), but this is also the one place that queries live
    // tailnet peers at all, so the AI Context tab (which wants every
    // machine on the tailnet, not just exit-node candidates) reuses this
    // same function/list instead of re-parsing `tailscale status --json`
    // a second time.
    let mut peers: Vec<TailscalePeer> = raw
        .peer
        .unwrap_or_default()
        .into_values()
        .map(|p| TailscalePeer {
            hostname: p.hostname,
            ip: p.ips.first().cloned().unwrap_or_default(),
            online: p.online,
            exit_node_option: p.exit_node_option,
            is_exit_node: p.exit_node,
        })
        .collect();
    peers.sort_by(|a, b| a.hostname.cmp(&b.hostname));

    TailscaleStatus {
        installed: true,
        running: raw.backend_state == "Running",
        self_hostname: raw.self_.hostname,
        self_ip: raw.tailscale_ips.into_iter().next(),
        peers,
    }
}

/// `None` clears the exit node. Requires `tailscale set --operator=$USER`
/// to have been run once — otherwise this silently fails (tailscale exits
/// non-zero, requiring root); the UI surfaces that via a one-time setup
/// hint rather than trying to sudo this itself.
pub fn tailscale_set_exit_node(hostname: Option<&str>) {
    let _ = Command::new("tailscale")
        .arg("set")
        .arg(format!("--exit-node={}", hostname.unwrap_or("")))
        .output();
}

// ── Public VPN (WireGuard) ────────────────────────────────────────────────

const HELPER: &str = "/usr/local/bin/og-vpn-apply";

#[derive(Debug, Clone)]
pub struct WgConfig {
    pub name: String,
    pub whole_active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct VpnState {
    pub configs: Vec<WgConfig>,
    pub netns_active: bool,
    pub netns_config: Option<String>,
    pub helper_installed: bool,
}

#[derive(Debug, Deserialize)]
struct RawWgConfig {
    name: String,
    whole_active: bool,
}

#[derive(Debug, Deserialize)]
struct RawVpnList {
    configs: Vec<RawWgConfig>,
    netns_active: bool,
    netns_config: Option<String>,
}

pub fn vpn_state() -> VpnState {
    if !Path::new(HELPER).exists() {
        return VpnState::default();
    }
    let Ok(out) = Command::new("sudo").args(["-n", HELPER, "list"]).output() else {
        return VpnState { helper_installed: true, ..Default::default() };
    };
    let Ok(raw) = serde_json::from_slice::<RawVpnList>(&out.stdout) else {
        return VpnState { helper_installed: true, ..Default::default() };
    };
    VpnState {
        configs: raw.configs.into_iter().map(|c| WgConfig { name: c.name, whole_active: c.whole_active }).collect(),
        netns_active: raw.netns_active,
        netns_config: raw.netns_config,
        helper_installed: true,
    }
}

fn run_helper(args: &[&str]) {
    let _ = Command::new("sudo").arg("-n").arg(HELPER).args(args).output();
}

/// Writes `contents` (a raw wg-quick `.conf`) to `/etc/wireguard/<name>.conf`
/// via the helper's stdin — the helper validates `name` is a bare
/// alnum/dash/underscore token before touching the filesystem.
pub fn add_config(name: &str, contents: &str) {
    use std::io::Write;
    if let Ok(mut child) = Command::new("sudo")
        .args(["-n", HELPER, "add", name])
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(contents.as_bytes());
        }
        let _ = child.wait();
    }
}

pub fn delete_config(name: &str) { run_helper(&["delete", name]); }
pub fn whole_up(name: &str) { run_helper(&["up", name]); }
pub fn whole_down(name: &str) { run_helper(&["down", name]); }
pub fn netns_up(name: &str) { run_helper(&["netns-up", name]); }
pub fn netns_down() { run_helper(&["netns-down"]); }

// ── Per-app split-tunnel routing ──────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SplitApp {
    pub id: String,
    pub name: String,
    pub routed: bool,
}

fn override_path(id: &str) -> PathBuf {
    PathBuf::from(format!("{}/.local/share/applications/{id}.desktop", home()))
}

/// Same `.desktop` scan convention as og-search's app registry, restricted
/// to `/usr/share/applications` since that's the only source the
/// `launch` subcommand of og-vpn-apply re-reads (routing an app whose
/// only `.desktop` lives under a user dir isn't supported — same scope
/// cut as the existing gamescope-Steam override, which only patches the
/// system-wide desktop file too).
pub fn list_split_apps() -> Vec<SplitApp> {
    let dir = Path::new("/usr/share/applications");
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut apps = Vec::new();
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        let mut name = String::new();
        let mut no_display = false;
        let mut in_entry = false;
        for line in content.lines() {
            let line = line.trim();
            if line == "[Desktop Entry]" { in_entry = true; continue; }
            if line.starts_with('[') { in_entry = false; continue; }
            if !in_entry { continue; }
            if let Some(v) = line.strip_prefix("Name=") { if name.is_empty() { name = v.to_string(); } }
            else if let Some(v) = line.strip_prefix("NoDisplay=") { no_display = v.eq_ignore_ascii_case("true"); }
        }
        if no_display || name.is_empty() { continue; }
        apps.push(SplitApp { routed: override_path(id).exists(), id: id.to_string(), name });
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

pub fn set_app_routed(id: &str, name: &str, routed: bool) {
    let path = override_path(id);
    if !routed {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let content = format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec=sudo -n {HELPER} launch {id}\nTerminal=false\nNoDisplay=false\n"
    );
    let _ = std::fs::write(&path, content);
    let _ = Command::new("update-desktop-database")
        .arg(format!("{}/.local/share/applications", home()))
        .output();
}
