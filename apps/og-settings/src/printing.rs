//! Printing tab backend: CUPS is unprivileged on this system (verified
//! live — `lpadmin -d`/`-p`/`-x` all succeed as a normal user, no sudo
//! needed), so everything here shells straight to `lp*` commands. The one
//! exception is installing a driver package (AUR/pacman), which spawns a
//! terminal for `sudo`/`yay` the same way the Network tab's Ethernet
//! toggle already does — the user should see that happening, not have it
//! run silently.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct Printer {
    pub name: String,
    pub make_model: String,
    pub device_uri: String,
    pub state: String,
    pub is_default: bool,
    pub accepting: bool,
}

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

pub fn cups_running() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", "cups.service"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn set_cups_enabled(terminal: &str, enabled: bool) {
    let term = if terminal.is_empty() { "alacritty" } else { terminal };
    let action = if enabled { "enable --now" } else { "disable --now" };
    let script = format!("sudo systemctl {action} cups.service; echo; read -rsn1 -p 'Press any key to close...'");
    let _ = Command::new(term).arg("-e").arg("bash").arg("-c").arg(script).spawn();
}

pub fn get_default() -> Option<String> {
    let out = run("lpstat", &["-d"]);
    out.trim().strip_prefix("system default destination: ").map(|s| s.to_string())
}

pub fn list_printers() -> Vec<Printer> {
    let default = get_default();
    let out = run("lpstat", &["-l", "-p"]);
    let mut printers = Vec::new();
    let mut current: Option<Printer> = None;

    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("printer ") {
            if let Some(p) = current.take() { printers.push(p); }
            let name = rest.split_whitespace().next().unwrap_or("").to_string();
            let state = if rest.contains("is idle") { "Idle" }
                else if rest.contains("now printing") { "Printing" }
                else if rest.contains("disabled") { "Disabled" }
                else { "Unknown" }.to_string();
            let is_default = default.as_deref() == Some(name.as_str());
            current = Some(Printer { name, make_model: String::new(), device_uri: String::new(), state, is_default, accepting: true });
        } else if let Some(rest) = line.trim().strip_prefix("Description: ") {
            if let Some(p) = current.as_mut() { p.make_model = rest.to_string(); }
        }
    }
    if let Some(p) = current.take() { printers.push(p); }

    let uris = run("lpstat", &["-v"]);
    for line in uris.lines() {
        if let Some(rest) = line.strip_prefix("device for ") {
            if let Some((name, uri)) = rest.split_once(": ") {
                if let Some(p) = printers.iter_mut().find(|p| p.name == name) {
                    p.device_uri = uri.trim().to_string();
                }
            }
        }
    }
    printers
}

pub fn set_default(name: &str) { let _ = Command::new("lpadmin").args(["-d", name]).output(); }
pub fn remove_printer(name: &str) { let _ = Command::new("lpadmin").args(["-x", name]).output(); }
pub fn enable_printer(name: &str, enabled: bool) {
    let cmd = if enabled { "cupsenable" } else { "cupsdisable" };
    let _ = Command::new(cmd).arg(name).output();
}

const TEST_PAGE: &str = "/usr/share/cups/data/default-testpage.pdf";
pub fn print_test_page(name: &str) { let _ = Command::new("lp").args(["-d", name, TEST_PAGE]).output(); }

// ── Detected-but-unconfigured devices ──────────────────────────────────────

/// Backend/scheme names `lpinfo -v` always lists whether or not a real
/// device is attached — these carry no device to add, so they're not
/// "detected printers."
const NO_DEVICE_BACKENDS: &[&str] = &[
    "network socket", "network http", "network ipp", "network https",
    "network beh", "network ipps", "network lpd", "network smb", "file cups-pdf:/",
];

#[derive(Debug, Clone)]
pub struct DetectedDevice {
    pub uri: String,
    pub info: String,
}

pub fn list_detected_devices(configured: &[Printer]) -> Vec<DetectedDevice> {
    let out = run("lpinfo", &["-v"]);
    let known_uris: Vec<&str> = configured.iter().map(|p| p.device_uri.as_str()).collect();
    out.lines()
        .filter(|l| !NO_DEVICE_BACKENDS.contains(l))
        .filter_map(|l| {
            let (_, uri) = l.split_once(' ')?;
            if known_uris.contains(&uri) {
                return None;
            }
            Some(DetectedDevice { uri: uri.to_string(), info: uri.to_string() })
        })
        .collect()
}

/// Best-effort driverless setup (IPP Everywhere / AirPrint / most
/// USB-print-class devices via ipp-usb) — works for the large majority of
/// printers made in the last ~10 years without needing a vendor driver.
pub fn add_driverless(name: &str, uri: &str) {
    let _ = Command::new("lpadmin").args(["-p", name, "-E", "-v", uri, "-m", "everywhere"]).output();
}

/// Curated vendor keyword → common Linux driver package(s), for printers
/// driverless setup doesn't cover (older USB-only models). Not
/// exhaustive — a hint, not a guarantee.
pub fn driver_package_hint(device_text: &str) -> Option<(&'static str, &'static str)> {
    let lower = device_text.to_lowercase();
    let table: &[(&str, &str, &str)] = &[
        ("brother", "Brother", "check aur for brother-<your-model> (e.g. brother-hll2300d)"),
        ("hewlett", "HP", "hplip"),
        (" hp ", "HP", "hplip"),
        ("epson", "Epson", "epson-inkjet-printer-escpr"),
        ("canon", "Canon", "cnijfilter2 (AUR)"),
        ("samsung", "Samsung", "splix"),
        ("lexmark", "Lexmark", "foomatic-db"),
    ];
    table.iter().find(|(kw, _, _)| lower.contains(kw)).map(|(_, vendor, pkg)| (*vendor, *pkg))
}
