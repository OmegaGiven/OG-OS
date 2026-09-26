//! Plain-readable connected-hardware overview: USB, input peripherals,
//! Bluetooth, monitors, PCI. "Status" here is honestly limited to what's
//! readable without root — this system has kernel.dmesg_restrict=1
//! (confirmed live: unprivileged `dmesg` fails with "Operation not
//! permitted"), so there's no kernel-log-based error detection. What IS
//! available without root: whether a driver is actually bound in sysfs,
//! which is a real (if narrower) signal — "no driver bound" reliably
//! means something didn't claim the device, even if it can't explain why.

use std::process::Command;

#[derive(Debug, Clone)]
pub struct UsbDevice {
    pub bus: String,
    pub device_num: String,
    pub vendor_id: String,
    pub product_id: String,
    pub description: String,
    pub driver_bound: bool,
}

pub fn list_usb() -> Vec<UsbDevice> {
    let Ok(out) = Command::new("lsusb").output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().filter_map(parse_lsusb_line).collect()
}

fn parse_lsusb_line(line: &str) -> Option<UsbDevice> {
    // "Bus 001 Device 002: ID 0bda:5411 Realtek Semiconductor Corp. RTS5411 Hub"
    let rest = line.strip_prefix("Bus ")?;
    let (bus, rest) = rest.split_once(' ')?;
    let rest = rest.strip_prefix("Device ")?;
    let (device_num, rest) = rest.split_once(':')?;
    let rest = rest.trim().strip_prefix("ID ")?;
    let (ids, description) = rest.split_once(' ')?;
    let (vendor_id, product_id) = ids.split_once(':')?;
    Some(UsbDevice {
        bus: bus.to_string(),
        device_num: device_num.to_string(),
        vendor_id: vendor_id.to_string(),
        product_id: product_id.to_string(),
        description: description.trim().to_string(),
        driver_bound: usb_driver_bound(bus, device_num),
    })
}

/// Cross-references lsusb's bus/device numbers against sysfs
/// (/sys/bus/usb/devices/*/busnum + devnum) to find the matching device
/// node, then checks whether any of its interfaces has a driver bound.
fn usb_driver_bound(bus: &str, device_num: &str) -> bool {
    let bus_n: u32 = bus.parse().unwrap_or(0);
    let dev_n: u32 = device_num.parse().unwrap_or(0);
    let Ok(entries) = std::fs::read_dir("/sys/bus/usb/devices") else { return true };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Skip interface nodes ("1-1:1.0") here — only match top-level
        // device nodes ("1-1"), then check ITS interfaces below.
        if name.contains(':') {
            continue;
        }
        let busnum = std::fs::read_to_string(path.join("busnum")).ok().and_then(|s| s.trim().parse::<u32>().ok());
        let devnum = std::fs::read_to_string(path.join("devnum")).ok().and_then(|s| s.trim().parse::<u32>().ok());
        if busnum != Some(bus_n) || devnum != Some(dev_n) {
            continue;
        }
        // Found the matching device node — does any interface under it
        // (or the node itself, for simple single-interface devices) have
        // a driver bound?
        if path.join("driver").exists() {
            return true;
        }
        if let Ok(children) = std::fs::read_dir(&path) {
            for child in children.flatten() {
                if child.path().join("driver").exists() {
                    return true;
                }
            }
        }
        return false;
    }
    // Not found in sysfs at all is unusual but not itself evidence of a
    // problem — don't flag it as broken on a lookup miss.
    true
}

#[derive(Debug, Clone)]
pub struct InputDevice {
    pub name: String,
    pub has_event_handler: bool,
}

pub fn list_input() -> Vec<InputDevice> {
    let Ok(text) = std::fs::read_to_string("/proc/bus/input/devices") else { return Vec::new() };
    let mut devices = Vec::new();
    let mut current_name: Option<String> = None;
    let mut has_event = false;
    let mut is_real_peripheral = false;

    let flush = |name: Option<String>, has_event: bool, is_real: bool, out: &mut Vec<InputDevice>| {
        if let (Some(name), true) = (name, is_real) {
            out.push(InputDevice { name, has_event_handler: has_event });
        }
    };

    for line in text.lines() {
        if line.is_empty() {
            flush(current_name.take(), has_event, is_real_peripheral, &mut devices);
            has_event = false;
            is_real_peripheral = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("N: Name=") {
            current_name = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("H: Handlers=") {
            let handlers: Vec<&str> = rest.split_whitespace().collect();
            has_event = handlers.iter().any(|h| h.starts_with("event"));
            // Audio codecs register jack-sense pseudo "input devices" for
            // every mic/line/HDMI port (found live: 11 of these on this
            // machine's actual hardware) — their only handler is a bare
            // "event", never kbd/mouse/js. Filtering those out is what
            // keeps this "plain readable" instead of drowning the real
            // keyboards/mice/etc in audio-jack noise.
            // mouse/js handlers are numbered ("mouse0", "js0"), kbd isn't —
            // prefix match catches both instead of an exact match silently
            // dropping every real mouse (caught live: "BY Tech Gaming
            // Keyboard Mouse" with Handlers="event6 mouse0" was wrongly
            // filtered out before this fix).
            is_real_peripheral = handlers.iter().any(|h| *h == "kbd" || h.starts_with("mouse") || h.starts_with("js"));
        }
    }
    flush(current_name.take(), has_event, is_real_peripheral, &mut devices);
    devices
}

#[derive(Debug, Clone)]
pub struct PciDevice {
    pub slot: String,
    pub description: String,
    pub driver_bound: bool,
}

/// Host bridges, IOMMU, and "dummy"/"non-essential instrumentation"
/// functions never have a driver bound on this (and most) systems — that's
/// completely normal, not a problem. Found this live: without filtering
/// these out, roughly half of every PCI device on this actual machine
/// showed `driver_bound: false`, which would've made "no driver" status
/// meaningless noise instead of an actionable signal for the endpoint
/// devices (GPU, audio, network, storage, USB) where it actually matters.
const PCI_NOISE_PREFIXES: &[&str] = &["Host bridge:", "PCI bridge:", "ISA bridge:", "IOMMU:", "Non-Essential Instrumentation"];

pub fn list_pci() -> Vec<PciDevice> {
    let Ok(out) = Command::new("lspci").output() else { return Vec::new() };
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            let (slot, description) = line.split_once(' ')?;
            let description = description.trim();
            if PCI_NOISE_PREFIXES.iter().any(|p| description.starts_with(p)) {
                return None;
            }
            Some(PciDevice {
                slot: slot.to_string(),
                description: description.to_string(),
                driver_bound: pci_driver_bound(slot),
            })
        })
        .collect()
}

fn pci_driver_bound(slot: &str) -> bool {
    // lspci's slot is short form ("00:00.0"); sysfs wants the full
    // "0000:00:00.0" domain-qualified form.
    let full_slot = if slot.matches(':').count() == 1 { format!("0000:{slot}") } else { slot.to_string() };
    std::path::Path::new("/sys/bus/pci/devices").join(&full_slot).join("driver").exists()
}
