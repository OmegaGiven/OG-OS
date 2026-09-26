//! Removable / external drives: what's plugged in (mounted or not), and
//! mount / unmount / eject through udisks2 — no root needed for the
//! interactive user, same as Dolphin.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub dev: String,
    pub label: String,
    pub size: u64,
    pub mount_point: Option<PathBuf>,
    pub removable: bool,
    pub fstype: String,
}

impl Device {
    pub fn is_mounted(&self) -> bool {
        self.mount_point.is_some()
    }
}

/// Partitions that carry a filesystem, minus the system's own root/boot/swap.
pub fn list() -> Vec<Device> {
    let out = std::process::Command::new("lsblk")
        .args(["-J", "-b", "-o", "NAME,PATH,LABEL,SIZE,MOUNTPOINTS,RM,HOTPLUG,TYPE,FSTYPE,MODEL,TRAN"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    let mut devices = Vec::new();
    let Some(blocks) = json["blockdevices"].as_array() else { return devices };

    fn walk(node: &serde_json::Value, parent_rm: bool, parent_model: &str, parent_tran: &str, out: &mut Vec<Device>) {
        let rm = node["rm"].as_bool().unwrap_or(false) || node["hotplug"].as_bool().unwrap_or(false) || parent_rm;
        let model = node["model"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| parent_model.to_string());
        let tran = node["tran"].as_str().unwrap_or(parent_tran).to_string();
        let ty = node["type"].as_str().unwrap_or("");
        let fstype = node["fstype"].as_str().unwrap_or("").to_string();
        if (ty == "part" || ty == "disk" || ty == "crypt") && !fstype.is_empty() && !matches!(fstype.as_str(), "swap" | "crypto_LUKS" | "LVM2_member" | "linux_raid_member") {
            let mounts: Vec<PathBuf> = node["mountpoints"].as_array().map(|a| a.iter().filter_map(|m| m.as_str().map(PathBuf::from)).collect()).unwrap_or_default();
            let mount_point = mounts.into_iter().find(|m| !m.starts_with("/boot") && m != std::path::Path::new("/"));
            let is_system = node["mountpoints"].as_array().is_some_and(|a| a.iter().any(|m| matches!(m.as_str(), Some("/") | Some("/boot") | Some("/boot/efi") | Some("/home") | Some("/var"))));
            if !is_system && (rm || tran == "usb" || mount_point.as_ref().is_some_and(|m| m.starts_with("/run/media") || m.starts_with("/media"))) {
                let label = node["label"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
                    .unwrap_or_else(|| if model.is_empty() { node["name"].as_str().unwrap_or("Drive").to_string() } else { model.clone() });
                out.push(Device {
                    dev: node["path"].as_str().unwrap_or("").to_string(),
                    label,
                    size: node["size"].as_u64().unwrap_or(0),
                    mount_point,
                    removable: rm,
                    fstype,
                });
            }
        }
        if let Some(children) = node["children"].as_array() {
            for c in children {
                walk(c, rm, &model, &tran, out);
            }
        }
    }
    for b in blocks {
        walk(b, false, "", "", &mut devices);
    }
    devices.sort_by(|a, b| a.dev.cmp(&b.dev));
    devices
}

fn udisks(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("udisksctl").args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// Mounts and returns the mount point.
pub fn mount(dev: &str) -> Result<PathBuf, String> {
    let msg = udisks(&["mount", "-b", dev, "--no-user-interaction"])?;
    // "Mounted /dev/sdb1 at /run/media/user/LABEL"
    let mp = msg.split(" at ").nth(1).map(|s| s.trim_end_matches('.').to_string());
    mp.map(PathBuf::from).ok_or_else(|| msg)
}

pub fn unmount(dev: &str) -> Result<(), String> {
    udisks(&["unmount", "-b", dev, "--no-user-interaction"]).map(|_| ())
}

/// Unmount every partition of the parent disk and power it off (safe to
/// unplug). Falls back to just unmounting where power-off is refused.
pub fn eject(dev: &str) -> Result<(), String> {
    let _ = unmount(dev);
    let disk = parent_disk(dev);
    match udisks(&["power-off", "-b", &disk, "--no-user-interaction"]) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("not supported") || e.contains("NotSupported") => Ok(()),
        Err(e) => Err(e),
    }
}

fn parent_disk(dev: &str) -> String {
    // /dev/sdb1 → /dev/sdb ; /dev/nvme0n1p2 → /dev/nvme0n1 ; /dev/mmcblk0p1 → /dev/mmcblk0
    let name = dev.trim_start_matches("/dev/");
    let trimmed = if name.starts_with("nvme") || name.starts_with("mmcblk") {
        name.rsplit_once('p').map(|(a, b)| if b.chars().all(|c| c.is_ascii_digit()) { a.to_string() } else { name.to_string() }).unwrap_or(name.to_string())
    } else {
        name.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
    };
    format!("/dev/{trimmed}")
}
