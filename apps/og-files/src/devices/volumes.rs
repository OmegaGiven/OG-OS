//! macOS / Windows drives: the mounted volumes (via `sysinfo`), read-only.
//! No mount UI (the OS mounts what's plugged in); macOS can unmount and
//! eject through `diskutil`, Windows leaves that to "Safely Remove
//! Hardware". The list is refreshed when it changes (checked every few
//! seconds: neither OS has a cheap change signal we can use without more
//! native code).

use super::Drive;
use iced::futures::{SinkExt, Stream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Mounted volumes can be unmounted from the app (`diskutil` on macOS).
pub const CAN_UNMOUNT: bool = cfg!(target_os = "macos");

/// The "Root" entry of the tree: `/` on macOS, the Windows drive on Windows.
pub fn root_path() -> PathBuf {
    if cfg!(windows) {
        let d = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        PathBuf::from(format!("{d}\\"))
    } else {
        PathBuf::from("/")
    }
}

/// One volume as sysinfo reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Vol {
    pub name: String,
    pub mount: PathBuf,
    pub fs: String,
    pub total: u64,
    pub removable: bool,
}

/// Volumes worth showing → drives. macOS: `/` and `/Volumes/*` only (the
/// APFS system / data / VM volumes under /System/Volumes are hidden).
pub fn to_drives(vols: &[Vol], mac: bool, system_root: &Path) -> Vec<Drive> {
    let mut out: Vec<Drive> = vols
        .iter()
        .filter(|v| !mac || v.mount == Path::new("/") || v.mount.starts_with("/Volumes"))
        .map(|v| {
            let system = v.mount == system_root;
            let label = if !v.name.is_empty() && !(mac && v.mount == Path::new("/")) {
                v.name.clone()
            } else if mac && v.mount == Path::new("/") {
                "Macintosh HD".into()
            } else {
                v.mount.display().to_string()
            };
            Drive {
                id: v.mount.to_string_lossy().into_owned(),
                label,
                device: PathBuf::from(&v.name),
                size: v.total,
                fs_type: v.fs.clone(),
                mount_point: Some(v.mount.clone()),
                removable: v.removable,
                ejectable: mac && v.removable && !system,
                encrypted: false,
                system,
            }
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out.dedup_by(|a, b| a.id == b.id);
    out
}

fn volumes() -> Vec<Vol> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .map(|d| Vol {
            name: d.name().to_string_lossy().into_owned(),
            mount: d.mount_point().to_path_buf(),
            fs: d.file_system().to_string_lossy().into_owned(),
            total: d.total_space(),
            removable: d.is_removable(),
        })
        .collect()
}

pub async fn list() -> Result<Vec<Drive>, String> {
    tokio::task::spawn_blocking(|| to_drives(&volumes(), cfg!(target_os = "macos"), &root_path()))
        .await
        .map_err(|e| e.to_string())
}

pub async fn mount(_id: &str) -> Result<PathBuf, String> {
    Err("The system mounts drives here; plug it in and it appears".into())
}

/// `diskutil <verb> <mount point>` (macOS).
#[cfg(target_os = "macos")]
async fn diskutil(verb: &str, id: &str) -> Result<(), String> {
    let out = tokio::process::Command::new("/usr/sbin/diskutil")
        .args([verb, id])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("diskutil: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let e = String::from_utf8_lossy(&out.stderr);
        let o = String::from_utf8_lossy(&out.stdout);
        Err(format!(
            "{} failed: {}",
            verb,
            if e.trim().is_empty() {
                o.trim()
            } else {
                e.trim()
            }
        ))
    }
}

pub async fn unmount(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return diskutil("unmount", id).await;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        Err("Use \"Safely Remove Hardware\" to remove this drive".into())
    }
}

pub async fn eject(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return diskutil("eject", id).await;
    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        Err("Use \"Safely Remove Hardware\" to remove this drive".into())
    }
}

/// Fires whenever the set of mounted volumes changes (checked every 3 s).
pub fn watch_drives() -> impl Stream<Item = ()> + Send + 'static {
    iced::stream::channel(4, |mut out| async move {
        let snapshot = || async {
            tokio::task::spawn_blocking(|| {
                let mut v: Vec<PathBuf> = volumes().into_iter().map(|v| v.mount).collect();
                v.sort();
                v
            })
            .await
            .unwrap_or_default()
        };
        let mut last = snapshot().await;
        loop {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let now = snapshot().await;
            if now != last {
                last = now;
                if out.send(()).await.is_err() {
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(name: &str, mount: &str, removable: bool) -> Vol {
        Vol {
            name: name.into(),
            mount: mount.into(),
            fs: "apfs".into(),
            total: 1 << 30,
            removable,
        }
    }

    #[test]
    fn volumes_to_drives() {
        let mac = [
            vol("Macintosh HD", "/", false),
            vol("Data", "/System/Volumes/Data", false),
            vol("USB", "/Volumes/USB", true),
        ];
        let d = to_drives(&mac, true, Path::new("/"));
        assert_eq!(d.len(), 2);
        assert!(d[0].system && !d[0].ejectable && d[0].label == "Macintosh HD");
        assert!(d[1].removable && d[1].ejectable && !d[1].system);
        assert_eq!(d[1].mount_point.as_deref(), Some(Path::new("/Volumes/USB")));
        let win = [vol("", "C:\\", false), vol("STICK", "E:\\", true)];
        let d = to_drives(&win, false, Path::new("C:\\"));
        assert_eq!(d.len(), 2);
        assert!(d[0].system && d[0].label == "C:\\");
        assert!(!d[1].system && d[1].removable && !d[1].ejectable);
    }
}
