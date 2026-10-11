//! Drives via UDisks2 over D-Bus (zbus) and live folder watching (notify).
//! CONTRACT STUB — bodies to be implemented; public signatures and types are
//! fixed (the UI is built against them).
//!
//! Nothing here shells out (og-files used lsblk/udisksctl). All D-Bus calls
//! are async; mount/unmount go through UDisks2, which applies polkit rules
//! (a desktop user can mount removable media without a password).
#![allow(unused_variables, dead_code)]

use iced::futures::Stream;
use std::path::PathBuf;

const TODO: &str = "not implemented yet";

#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    /// UDisks2 object path of the block device (stable handle for calls)
    pub id: String,
    /// filesystem label, else drive model, else device name
    pub label: String,
    /// e.g. /dev/sdb1
    pub device: PathBuf,
    pub size: u64,
    pub fs_type: String,
    pub mount_point: Option<PathBuf>,
    /// USB sticks, SD cards, optical
    pub removable: bool,
    /// the drive can be powered off / ejected
    pub ejectable: bool,
    /// LUKS container (unlock not supported yet; listed so the user sees it)
    pub encrypted: bool,
    /// the system root, /boot, /home etc. — shown, never offered for unmount
    pub system: bool,
}

/// Filesystems the user might care about: removable media, other internal
/// partitions with a mountable filesystem, and mounted system volumes.
/// Swap, EFI-only and empty devices are skipped.
pub async fn list() -> Result<Vec<Drive>, String> {
    Err(TODO.into())
}

/// Mount via UDisks2 (Filesystem.Mount); returns the mount point.
pub async fn mount(id: &str) -> Result<PathBuf, String> {
    Err(TODO.into())
}

pub async fn unmount(id: &str) -> Result<(), String> {
    Err(TODO.into())
}

/// Unmount every filesystem on the drive, then power it off (or eject
/// optical media).
pub async fn eject(id: &str) -> Result<(), String> {
    Err(TODO.into())
}

/// Fires (debounced ~300 ms) whenever drives appear/disappear or mount
/// state changes (UDisks2 InterfacesAdded/Removed + PropertiesChanged).
/// Ends if the system bus is unavailable.
pub fn watch_drives() -> impl Stream<Item = ()> + Send + 'static {
    iced::futures::stream::empty()
}

/// Watch `dirs` (non-recursive) and yield the set of dirs whose contents
/// changed, debounced 250 ms. Restart with a new list when the visible
/// folders change (the UI aborts the old stream).
pub fn watch_dirs(dirs: Vec<PathBuf>) -> impl Stream<Item = Vec<PathBuf>> + Send + 'static {
    iced::futures::stream::empty()
}
