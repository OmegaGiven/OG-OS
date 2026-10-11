//! Drives via UDisks2 over D-Bus (zbus) and live folder watching (notify).
//!
//! Nothing here shells out (og-files used lsblk/udisksctl). All D-Bus calls
//! are async; mount/unmount go through UDisks2, which applies polkit rules
//! (a desktop user can mount removable media without a password).
//!
//! Timers use `tokio::time`, which is compiled in because iced's `tokio`
//! feature enables tokio's `time` driver.
// UI not wired to this module yet.
#![cfg_attr(not(test), allow(dead_code))]

use iced::futures::{SinkExt, Stream, StreamExt};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zbus::fdo::ManagedObjects;
use zbus::zvariant::{OwnedValue, Value};

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

const UDISKS: &str = "org.freedesktop.UDisks2";
const UDISKS_ROOT: &str = "/org/freedesktop/UDisks2";
const IFACE_BLOCK: &str = "org.freedesktop.UDisks2.Block";
const IFACE_FS: &str = "org.freedesktop.UDisks2.Filesystem";
const IFACE_DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const IFACE_ENCRYPTED: &str = "org.freedesktop.UDisks2.Encrypted";
const IFACE_OM: &str = "org.freedesktop.DBus.ObjectManager";
const IFACE_PROPS: &str = "org.freedesktop.DBus.Properties";

/// Mount points that make a volume a "system" volume.
const SYSTEM_MOUNTS: &[&str] = &["/", "/boot", "/boot/efi", "/efi", "/home", "/var", "/usr"];

/// Filesystems the user might care about: removable media, other internal
/// partitions with a mountable filesystem, and mounted system volumes.
/// Swap, EFI-only and empty devices are skipped.
pub async fn list() -> Result<Vec<Drive>, String> {
    let objects = managed_objects().await?;
    Ok(parse_drives(&Snapshot::from_objects(&objects)))
}

/// Mount via UDisks2 (Filesystem.Mount); returns the mount point.
pub async fn mount(id: &str) -> Result<PathBuf, String> {
    let conn = system_bus().await?;
    let reply = conn
        .call_method(
            Some(UDISKS),
            id,
            Some(IFACE_FS),
            "Mount",
            &(mount_options(),),
        )
        .await
        .map_err(map_err)?;
    let path: String = reply.body().deserialize().map_err(map_err)?;
    Ok(PathBuf::from(path))
}

pub async fn unmount(id: &str) -> Result<(), String> {
    let conn = system_bus().await?;
    unmount_on(conn, id).await
}

/// Unmount every filesystem on the drive, then power it off (or eject
/// optical media).
pub async fn eject(id: &str) -> Result<(), String> {
    let conn = system_bus().await?;
    let objects = managed_objects().await?;
    let snap = Snapshot::from_objects(&objects);
    let plan = eject_plan(&snap, id)?;
    for path in &plan.unmount {
        unmount_on(conn, path).await?;
    }
    for path in &plan.lock {
        conn.call_method(
            Some(UDISKS),
            path.as_str(),
            Some(IFACE_ENCRYPTED),
            "Lock",
            &(empty_options(),),
        )
        .await
        .map_err(map_err)?;
    }
    let method = if plan.power_off { "PowerOff" } else { "Eject" };
    conn.call_method(
        Some(UDISKS),
        plan.drive.as_str(),
        Some(IFACE_DRIVE),
        method,
        &(empty_options(),),
    )
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Fires (debounced ~300 ms) whenever drives appear/disappear or mount
/// state changes (UDisks2 InterfacesAdded/Removed + PropertiesChanged).
/// Ends if the system bus is unavailable.
pub fn watch_drives() -> impl Stream<Item = ()> + Send + 'static {
    iced::stream::channel(4, |mut out| async move {
        use zbus::message::Type;
        use zbus::{MatchRule, MessageStream};

        let Ok(conn) = system_bus().await else { return };
        let rules = [
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(UDISKS)
                .and_then(|b| b.interface(IFACE_OM))
                .and_then(|b| b.path(UDISKS_ROOT))
                .map(|b| b.build()),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(UDISKS)
                .and_then(|b| b.interface(IFACE_PROPS))
                .and_then(|b| b.member("PropertiesChanged"))
                .map(|b| b.build()),
            // udisksd restarting
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")
                .and_then(|b| b.member("NameOwnerChanged"))
                .and_then(|b| b.arg(0, UDISKS))
                .map(|b| b.build()),
        ];
        let mut streams = Vec::new();
        for rule in rules.into_iter().flatten() {
            if let Ok(stream) = MessageStream::for_match_rule(rule, conn, Some(64)).await {
                streams.push(stream);
            }
        }
        if streams.is_empty() {
            return;
        }
        let mut events = iced::futures::stream::select_all(streams);
        let quiet = Duration::from_millis(300);
        let max = Duration::from_secs(1);
        loop {
            // wait for the first event of a burst
            if events.next().await.is_none() {
                return;
            }
            let start = tokio::time::Instant::now();
            loop {
                let left = max.saturating_sub(start.elapsed());
                if left.is_zero() {
                    break;
                }
                match tokio::time::timeout(quiet.min(left), events.next()).await {
                    Ok(Some(_)) => continue,
                    Ok(None) => {
                        let _ = out.send(()).await;
                        return;
                    }
                    Err(_) => break,
                }
            }
            if out.send(()).await.is_err() {
                return;
            }
        }
    })
}

/// Watch `dirs` (non-recursive) and yield the set of dirs whose contents
/// changed, debounced 250 ms. Restart with a new list when the visible
/// folders change (the UI aborts the old stream).
pub fn watch_dirs(dirs: Vec<PathBuf>) -> impl Stream<Item = Vec<PathBuf>> + Send + 'static {
    iced::stream::channel(8, move |mut out| async move {
        use notify::{RecursiveMode, Watcher};

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Event>();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                let _ = tx.send(ev);
            }
        });
        let Ok(mut watcher) = watcher else { return };

        let mut watched: Vec<PathBuf> = Vec::new();
        for d in dirs {
            if watched.contains(&d) || !d.is_dir() {
                continue;
            }
            if watcher.watch(&d, RecursiveMode::NonRecursive).is_ok() {
                watched.push(d);
            }
        }
        let watched_set: HashSet<PathBuf> = watched.iter().cloned().collect();

        let quiet = Duration::from_millis(250);
        let max = Duration::from_secs(1);
        // `watcher` lives in this future; dropping the stream drops it.
        let _watcher = watcher;
        loop {
            let Some(first) = rx.recv().await else { return };
            let mut changed = HashSet::new();
            collect_changed(&first, &watched_set, &mut changed);
            let start = tokio::time::Instant::now();
            loop {
                let left = max.saturating_sub(start.elapsed());
                if left.is_zero() {
                    break;
                }
                match tokio::time::timeout(quiet.min(left), rx.recv()).await {
                    Ok(Some(ev)) => collect_changed(&ev, &watched_set, &mut changed),
                    Ok(None) | Err(_) => break,
                }
            }
            if changed.is_empty() {
                continue;
            }
            let batch: Vec<PathBuf> = watched
                .iter()
                .filter(|d| changed.contains(*d))
                .cloned()
                .collect();
            if out.send(batch).await.is_err() {
                return;
            }
        }
    })
}

// ---------------------------------------------------------------- watcher

/// Add the watched dirs affected by `ev` to `changed`.
fn collect_changed(ev: &notify::Event, watched: &HashSet<PathBuf>, changed: &mut HashSet<PathBuf>) {
    use notify::EventKind;
    if matches!(ev.kind, EventKind::Access(_)) {
        return;
    }
    for p in &ev.paths {
        if watched.contains(p) {
            changed.insert(p.clone());
        }
        if let Some(parent) = p.parent() {
            if watched.contains(parent) {
                changed.insert(parent.to_path_buf());
            }
        }
    }
}

// ---------------------------------------------------------------- D-Bus

async fn system_bus() -> Result<&'static zbus::Connection, String> {
    static BUS: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();
    BUS.get_or_try_init(zbus::Connection::system)
        .await
        .map_err(|_| "System bus unavailable".to_string())
}

async fn managed_objects() -> Result<ManagedObjects, String> {
    let conn = system_bus().await?;
    let reply = conn
        .call_method(
            Some(UDISKS),
            UDISKS_ROOT,
            Some(IFACE_OM),
            "GetManagedObjects",
            &(),
        )
        .await
        .map_err(map_err)?;
    reply
        .body()
        .deserialize::<ManagedObjects>()
        .map_err(map_err)
}

async fn unmount_on(conn: &zbus::Connection, id: &str) -> Result<(), String> {
    conn.call_method(
        Some(UDISKS),
        id,
        Some(IFACE_FS),
        "Unmount",
        &(empty_options(),),
    )
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Options for Filesystem.Mount: let polkit prompt if authorization is needed.
fn mount_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::from([("auth.no_user_interaction", Value::Bool(false))])
}

fn empty_options() -> HashMap<&'static str, Value<'static>> {
    HashMap::new()
}

fn map_err(e: zbus::Error) -> String {
    match &e {
        zbus::Error::MethodError(name, detail, _) => {
            map_error_name(name.as_str(), detail.as_deref())
        }
        zbus::Error::InputOutput(_) => "System bus connection lost".into(),
        _ => format!("D-Bus error: {e}"),
    }
}

/// Short user-facing message for a D-Bus error name (+ optional detail).
fn map_error_name(name: &str, detail: Option<&str>) -> String {
    let short = name.strip_prefix("org.freedesktop.UDisks2.Error.");
    match short {
        Some("DeviceBusy") => return "Device is busy — close files on it first".into(),
        Some(s) if s.starts_with("NotAuthorized") => return "Not authorized".into(),
        Some("AlreadyMounted") => return "Already mounted".into(),
        Some("NotMounted") => return "Not mounted".into(),
        Some("Cancelled") => return "Cancelled".into(),
        Some("Timedout") => return "Timed out".into(),
        Some("NotSupported") => return "Not supported for this device".into(),
        Some("WouldWakeup") => return "Device is asleep".into(),
        _ => {}
    }
    match name {
        "org.freedesktop.DBus.Error.ServiceUnknown"
        | "org.freedesktop.DBus.Error.NameHasNoOwner" => return "UDisks2 is not running".into(),
        "org.freedesktop.DBus.Error.UnknownObject"
        | "org.freedesktop.DBus.Error.UnknownMethod"
        | "org.freedesktop.DBus.Error.UnknownInterface" => {
            return "Device is no longer available".into()
        }
        "org.freedesktop.DBus.Error.AccessDenied" => return "Not authorized".into(),
        "org.freedesktop.DBus.Error.NoReply" | "org.freedesktop.DBus.Error.Timeout" => {
            return "Timed out".into()
        }
        _ => {}
    }
    // UDisks details often look like "Error mounting /dev/sdb1 at ...: <reason>"
    match detail.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => {
            let line = d.lines().next().unwrap_or(d);
            line.rsplit_once(": ")
                .map_or(line, |(_, r)| r)
                .trim()
                .to_string()
        }
        None => short.unwrap_or(name).to_string(),
    }
}

// ---------------------------------------------------------------- parsing

#[derive(Debug, Default, Clone)]
struct BlockInfo {
    path: String,
    device: PathBuf,
    size: u64,
    id_label: String,
    id_type: String,
    id_usage: String,
    hint_system: bool,
    hint_ignore: bool,
    /// Drive object path, None for "/"
    drive: Option<String>,
    /// for unlocked cleartext devices: the LUKS container's object path
    crypto_backing: Option<String>,
    has_fs: bool,
    mount_points: Vec<PathBuf>,
}

#[derive(Debug, Default, Clone)]
struct DriveInfo {
    removable: bool,
    media_removable: bool,
    ejectable: bool,
    can_power_off: bool,
    model: String,
    vendor: String,
}

/// The parts of GetManagedObjects we use.
#[derive(Debug, Default)]
struct Snapshot {
    blocks: Vec<BlockInfo>,
    drives: HashMap<String, DriveInfo>,
}

type Props = HashMap<String, OwnedValue>;

fn prop<'a>(props: &'a Props, key: &str) -> Option<&'a Value<'static>> {
    props.get(key).map(|v| &**v)
}

fn prop_bool(props: &Props, key: &str) -> bool {
    matches!(prop(props, key), Some(Value::Bool(true)))
}

fn prop_u64(props: &Props, key: &str) -> u64 {
    match prop(props, key) {
        Some(Value::U64(n)) => *n,
        _ => 0,
    }
}

fn prop_str(props: &Props, key: &str) -> String {
    match prop(props, key) {
        Some(Value::Str(s)) => s.as_str().trim().to_string(),
        _ => String::new(),
    }
}

/// Object-path property; "/" (UDisks' "none") maps to None.
fn prop_path(props: &Props, key: &str) -> Option<String> {
    match prop(props, key) {
        Some(Value::ObjectPath(p)) if p.as_str() != "/" => Some(p.as_str().to_string()),
        _ => None,
    }
}

fn value_bytes(v: &Value<'_>) -> Option<Vec<u8>> {
    match v {
        Value::Array(a) => a
            .iter()
            .map(|b| match b {
                Value::U8(b) => Some(*b),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// D-Bus byte strings (`ay`) carry a trailing NUL.
fn bytes_to_path(mut bytes: Vec<u8>) -> PathBuf {
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    PathBuf::from(OsStr::from_bytes(&bytes))
}

fn prop_bytepath(props: &Props, key: &str) -> PathBuf {
    prop(props, key)
        .and_then(value_bytes)
        .map(bytes_to_path)
        .unwrap_or_default()
}

fn prop_bytepaths(props: &Props, key: &str) -> Vec<PathBuf> {
    match prop(props, key) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(value_bytes)
            .map(bytes_to_path)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

impl Snapshot {
    fn from_objects(objects: &ManagedObjects) -> Self {
        let mut snap = Snapshot::default();
        for (path, ifaces) in objects {
            let iface = |name: &str| {
                ifaces
                    .iter()
                    .find(|(n, _)| n.as_str() == name)
                    .map(|(_, p)| p)
            };
            if let Some(d) = iface(IFACE_DRIVE) {
                snap.drives.insert(
                    path.as_str().to_string(),
                    DriveInfo {
                        removable: prop_bool(d, "Removable"),
                        media_removable: prop_bool(d, "MediaRemovable"),
                        ejectable: prop_bool(d, "Ejectable"),
                        can_power_off: prop_bool(d, "CanPowerOff"),
                        model: prop_str(d, "Model"),
                        vendor: prop_str(d, "Vendor"),
                    },
                );
            }
            let Some(b) = iface(IFACE_BLOCK) else {
                continue;
            };
            let fs = iface(IFACE_FS);
            snap.blocks.push(BlockInfo {
                path: path.as_str().to_string(),
                device: prop_bytepath(b, "Device"),
                size: prop_u64(b, "Size"),
                id_label: prop_str(b, "IdLabel"),
                id_type: prop_str(b, "IdType"),
                id_usage: prop_str(b, "IdUsage"),
                hint_system: prop_bool(b, "HintSystem"),
                hint_ignore: prop_bool(b, "HintIgnore"),
                drive: prop_path(b, "Drive"),
                crypto_backing: prop_path(b, "CryptoBackingDevice"),
                has_fs: fs.is_some(),
                mount_points: fs
                    .map(|f| prop_bytepaths(f, "MountPoints"))
                    .unwrap_or_default(),
            });
        }
        snap.blocks.sort_by(|a, b| a.device.cmp(&b.device));
        snap
    }

    fn block(&self, path: &str) -> Option<&BlockInfo> {
        self.blocks.iter().find(|b| b.path == path)
    }

    /// The Drive a block lives on, following cleartext → LUKS container.
    fn drive_of(&self, b: &BlockInfo) -> Option<String> {
        let mut cur = b;
        for _ in 0..8 {
            if let Some(d) = &cur.drive {
                return Some(d.clone());
            }
            cur = self.block(cur.crypto_backing.as_deref()?)?;
        }
        None
    }
}

fn is_system_mount(p: &Path) -> bool {
    SYSTEM_MOUNTS.iter().any(|m| p == Path::new(m))
}

fn parse_drives(snap: &Snapshot) -> Vec<Drive> {
    // LUKS containers that are unlocked: their cleartext device is listed instead
    let unlocked: HashSet<&str> = snap
        .blocks
        .iter()
        .filter_map(|b| b.crypto_backing.as_deref())
        .collect();
    let mut out = Vec::new();
    for b in &snap.blocks {
        if b.hint_ignore {
            continue;
        }
        let encrypted = match b.id_usage.as_str() {
            "filesystem" if b.has_fs => false,
            "crypto" if !unlocked.contains(b.path.as_str()) => true,
            _ => continue, // swap, partition tables, empty, raid members...
        };
        let mount_point = b.mount_points.first().cloned();
        let name = b
            .device
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.starts_with("loop")
            && !mount_point
                .as_ref()
                .is_some_and(|m| m.starts_with("/run/media"))
        {
            continue;
        }
        let drive = snap.drive_of(b).and_then(|d| snap.drives.get(&d));
        let removable = drive.is_some_and(|d| d.removable || d.media_removable);
        let system =
            b.mount_points.iter().any(|m| is_system_mount(m)) || (b.hint_system && !removable);
        let ejectable =
            !system && removable && drive.is_some_and(|d| d.ejectable || d.can_power_off);
        let label = if !b.id_label.is_empty() {
            b.id_label.clone()
        } else {
            let vm = drive
                .map(|d| format!("{} {}", d.vendor, d.model).trim().to_string())
                .unwrap_or_default();
            if !vm.is_empty() {
                vm
            } else {
                name
            }
        };
        out.push(Drive {
            id: b.path.clone(),
            label,
            device: b.device.clone(),
            size: b.size,
            fs_type: b.id_type.clone(),
            mount_point,
            removable,
            ejectable,
            encrypted,
            system,
        });
    }
    out
}

#[derive(Debug, PartialEq)]
struct EjectPlan {
    drive: String,
    /// filesystem objects to unmount (cleartext devices before others)
    unmount: Vec<String>,
    /// unlocked LUKS containers to lock after unmounting
    lock: Vec<String>,
    power_off: bool,
}

fn eject_plan(snap: &Snapshot, id: &str) -> Result<EjectPlan, String> {
    let block = snap.block(id).ok_or("Device is no longer available")?;
    let drive = snap.drive_of(block).ok_or("Not a removable drive")?;
    let info = snap
        .drives
        .get(&drive)
        .ok_or("Device is no longer available")?;
    let on_drive: Vec<&BlockInfo> = snap
        .blocks
        .iter()
        .filter(|b| snap.drive_of(b).as_deref() == Some(drive.as_str()))
        .collect();
    if on_drive
        .iter()
        .any(|b| b.mount_points.iter().any(|m| is_system_mount(m)))
    {
        return Err("Can't eject the system drive".into());
    }
    let mut unmount: Vec<&BlockInfo> = on_drive
        .iter()
        .copied()
        .filter(|b| !b.mount_points.is_empty())
        .collect();
    // cleartext (crypto-backed) filesystems first
    unmount.sort_by_key(|b| b.crypto_backing.is_none());
    let lock = on_drive
        .iter()
        .filter_map(|b| b.crypto_backing.clone())
        .filter(|p| snap.block(p).is_some())
        .collect();
    Ok(EjectPlan {
        drive,
        unmount: unmount.into_iter().map(|b| b.path.clone()).collect(),
        lock,
        power_off: info.can_power_off,
    })
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::names::OwnedInterfaceName;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath};

    fn ov(v: Value<'_>) -> OwnedValue {
        OwnedValue::try_from(v).unwrap()
    }

    fn bstr(s: &str) -> Value<'static> {
        let mut b = s.as_bytes().to_vec();
        b.push(0);
        Value::from(b)
    }

    fn opath(p: &str) -> Value<'static> {
        Value::from(ObjectPath::try_from(p.to_string()).unwrap())
    }

    struct Fx(ManagedObjects);

    impl Fx {
        fn new() -> Self {
            Fx(HashMap::new())
        }
        fn add(&mut self, path: &str, iface: &str, props: Vec<(&str, Value<'static>)>) {
            let obj = self
                .0
                .entry(OwnedObjectPath::try_from(path.to_string()).unwrap())
                .or_default();
            obj.insert(
                OwnedInterfaceName::try_from(iface.to_string()).unwrap(),
                props
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), ov(v)))
                    .collect(),
            );
        }
        #[allow(clippy::too_many_arguments)]
        fn block(
            &mut self,
            name: &str,
            label: &str,
            ty: &str,
            usage: &str,
            drive: &str,
            sys: bool,
            mounts: Option<&[&str]>,
        ) {
            let path = format!("{UDISKS_ROOT}/block_devices/{name}");
            self.add(
                &path,
                IFACE_BLOCK,
                vec![
                    ("Device", bstr(&format!("/dev/{name}"))),
                    ("Size", Value::U64(1_000_000)),
                    ("IdLabel", Value::from(label.to_string())),
                    ("IdType", Value::from(ty.to_string())),
                    ("IdUsage", Value::from(usage.to_string())),
                    ("HintSystem", Value::Bool(sys)),
                    ("HintIgnore", Value::Bool(false)),
                    ("Drive", opath(drive)),
                    ("CryptoBackingDevice", opath("/")),
                ],
            );
            if let Some(m) = mounts {
                let mp: Vec<Vec<u8>> = m
                    .iter()
                    .map(|s| {
                        let mut b = s.as_bytes().to_vec();
                        b.push(0);
                        b
                    })
                    .collect();
                self.add(&path, IFACE_FS, vec![("MountPoints", Value::from(mp))]);
            }
        }
        fn drive(
            &mut self,
            name: &str,
            removable: bool,
            power_off: bool,
            vendor: &str,
            model: &str,
        ) -> String {
            let path = format!("{UDISKS_ROOT}/drives/{name}");
            self.add(
                &path,
                IFACE_DRIVE,
                vec![
                    ("Removable", Value::Bool(removable)),
                    ("MediaRemovable", Value::Bool(false)),
                    ("Ejectable", Value::Bool(removable)),
                    ("CanPowerOff", Value::Bool(power_off)),
                    ("Model", Value::from(model.to_string())),
                    ("Vendor", Value::from(vendor.to_string())),
                    (
                        "ConnectionBus",
                        Value::from(if removable { "usb" } else { "" }.to_string()),
                    ),
                ],
            );
            path
        }
    }

    fn fixture() -> Fx {
        let mut fx = Fx::new();
        let nvme = fx.drive("nvme", false, false, "", "Samsung SSD 980");
        let usb = fx.drive("usb", true, true, "SanDisk", "Cruzer");
        fx.block("nvme0n1", "", "", "", &nvme, true, None); // partition table
        fx.block(
            "nvme0n1p1",
            "",
            "vfat",
            "filesystem",
            &nvme,
            true,
            Some(&["/boot"]),
        );
        fx.block(
            "nvme0n1p2",
            "root",
            "ext4",
            "filesystem",
            &nvme,
            true,
            Some(&["/"]),
        );
        fx.block("nvme0n1p3", "", "swap", "other", &nvme, true, None);
        fx.block(
            "nvme0n1p4",
            "data",
            "ext4",
            "filesystem",
            &nvme,
            false,
            Some(&[]),
        );
        fx.block("nvme0n1p5", "", "crypto_LUKS", "crypto", &nvme, false, None);
        fx.block("sdb", "", "", "", &usb, false, None);
        fx.block(
            "sdb1",
            "",
            "exfat",
            "filesystem",
            &usb,
            false,
            Some(&["/run/media/me/STICK"]),
        );
        fx.block(
            "loop0",
            "",
            "squashfs",
            "filesystem",
            "/",
            false,
            Some(&["/var/lib/snapd/x"]),
        );
        fx.block(
            "loop1",
            "img",
            "iso9660",
            "filesystem",
            "/",
            false,
            Some(&["/run/media/me/img"]),
        );
        fx
    }

    fn find<'a>(v: &'a [Drive], dev: &str) -> Option<&'a Drive> {
        v.iter().find(|d| d.device == Path::new(dev))
    }

    #[test]
    fn parse_classifies_devices() {
        let drives = parse_drives(&Snapshot::from_objects(&fixture().0));
        let devs: Vec<_> = drives
            .iter()
            .map(|d| d.device.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            devs,
            [
                "/dev/loop1",
                "/dev/nvme0n1p1",
                "/dev/nvme0n1p2",
                "/dev/nvme0n1p4",
                "/dev/nvme0n1p5",
                "/dev/sdb1"
            ]
        );

        let usb = find(&drives, "/dev/sdb1").unwrap();
        assert_eq!(
            usb.mount_point.as_deref(),
            Some(Path::new("/run/media/me/STICK"))
        );
        assert!(usb.removable && usb.ejectable && !usb.system && !usb.encrypted);
        assert_eq!(usb.label, "SanDisk Cruzer");
        assert_eq!(usb.fs_type, "exfat");
        assert_eq!(usb.id, "/org/freedesktop/UDisks2/block_devices/sdb1");

        let root = find(&drives, "/dev/nvme0n1p2").unwrap();
        assert!(root.system && !root.removable && !root.ejectable);
        assert_eq!(root.label, "root");
        assert!(find(&drives, "/dev/nvme0n1p1").unwrap().system);

        let data = find(&drives, "/dev/nvme0n1p4").unwrap();
        assert_eq!(data.mount_point, None);
        assert!(!data.system && !data.removable && !data.ejectable);
        assert_eq!(data.size, 1_000_000);

        let luks = find(&drives, "/dev/nvme0n1p5").unwrap();
        assert!(luks.encrypted && !luks.system);
        assert_eq!(luks.label, "Samsung SSD 980");

        let lp = find(&drives, "/dev/loop1").unwrap();
        assert_eq!(lp.label, "img");
    }

    #[test]
    fn nul_stripping_and_fallback_label() {
        assert_eq!(
            bytes_to_path(b"/dev/sda1\0".to_vec()),
            PathBuf::from("/dev/sda1")
        );
        assert_eq!(bytes_to_path(b"/x\0\0".to_vec()), PathBuf::from("/x"));
        assert_eq!(bytes_to_path(Vec::new()), PathBuf::new());
        let mut fx = Fx::new();
        fx.block("sdc1", "", "ext4", "filesystem", "/", false, Some(&[]));
        let d = parse_drives(&Snapshot::from_objects(&fx.0));
        assert_eq!(d[0].label, "sdc1");
        assert_eq!(d[0].device, PathBuf::from("/dev/sdc1"));
    }

    #[test]
    fn hint_ignore_and_unlocked_luks() {
        let mut fx = Fx::new();
        let usb = fx.drive("usb", true, true, "", "Stick");
        fx.block("sdb1", "", "crypto_LUKS", "crypto", &usb, false, None);
        fx.block(
            "dm_2d0",
            "secret",
            "ext4",
            "filesystem",
            "/",
            false,
            Some(&["/run/media/me/secret"]),
        );
        fx.add(
            &format!("{UDISKS_ROOT}/block_devices/dm_2d0"),
            IFACE_BLOCK,
            vec![
                ("Device", bstr("/dev/dm-0")),
                ("IdLabel", Value::from("secret")),
                ("IdType", Value::from("ext4")),
                ("IdUsage", Value::from("filesystem")),
                ("Drive", opath("/")),
                (
                    "CryptoBackingDevice",
                    opath(&format!("{UDISKS_ROOT}/block_devices/sdb1")),
                ),
            ],
        );
        fx.block("sdb2", "", "vfat", "filesystem", &usb, false, Some(&[]));
        fx.add(
            &format!("{UDISKS_ROOT}/block_devices/sdb2"),
            IFACE_BLOCK,
            vec![
                ("Device", bstr("/dev/sdb2")),
                ("IdUsage", Value::from("filesystem")),
                ("HintIgnore", Value::Bool(true)),
            ],
        );
        let snap = Snapshot::from_objects(&fx.0);
        let d = parse_drives(&snap);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].device, PathBuf::from("/dev/dm-0"));
        assert!(d[0].removable && d[0].ejectable && !d[0].encrypted);

        let plan = eject_plan(&snap, &format!("{UDISKS_ROOT}/block_devices/dm_2d0")).unwrap();
        assert_eq!(plan.drive, usb);
        assert_eq!(
            plan.unmount,
            [format!("{UDISKS_ROOT}/block_devices/dm_2d0")]
        );
        assert_eq!(plan.lock, [format!("{UDISKS_ROOT}/block_devices/sdb1")]);
        assert!(plan.power_off);
    }

    #[test]
    fn eject_plan_unmounts_all_on_drive() {
        let mut fx = fixture();
        let usb = format!("{UDISKS_ROOT}/drives/usb");
        fx.block(
            "sdb2",
            "B",
            "ext4",
            "filesystem",
            &usb,
            false,
            Some(&["/run/media/me/B"]),
        );
        fx.block("sdb3", "C", "ext4", "filesystem", &usb, false, Some(&[]));
        let snap = Snapshot::from_objects(&fx.0);
        let plan = eject_plan(&snap, &format!("{UDISKS_ROOT}/block_devices/sdb1")).unwrap();
        assert_eq!(
            plan.unmount,
            [
                format!("{UDISKS_ROOT}/block_devices/sdb1"),
                format!("{UDISKS_ROOT}/block_devices/sdb2")
            ]
        );
        assert!(plan.lock.is_empty());
        assert!(plan.power_off);
        assert!(eject_plan(&snap, &format!("{UDISKS_ROOT}/block_devices/nvme0n1p4")).is_err());
        assert!(eject_plan(&snap, "/nope").is_err());
    }

    #[test]
    fn mount_options_allow_interaction() {
        let o = mount_options();
        assert_eq!(o.len(), 1);
        assert_eq!(o.get("auth.no_user_interaction"), Some(&Value::Bool(false)));
        // serializes as a{sv}
        let v = Value::from(o);
        assert_eq!(v.value_signature().to_string(), "a{sv}");
        assert!(empty_options().is_empty());
    }

    #[test]
    fn error_mapping() {
        let e = |n: &str, d: Option<&str>| map_error_name(n, d);
        assert_eq!(
            e(
                "org.freedesktop.UDisks2.Error.DeviceBusy",
                Some("target is busy")
            ),
            "Device is busy — close files on it first"
        );
        assert_eq!(
            e("org.freedesktop.UDisks2.Error.NotAuthorized", None),
            "Not authorized"
        );
        assert_eq!(
            e("org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain", None),
            "Not authorized"
        );
        assert_eq!(
            e("org.freedesktop.UDisks2.Error.NotAuthorizedDismissed", None),
            "Not authorized"
        );
        assert_eq!(
            e("org.freedesktop.UDisks2.Error.AlreadyMounted", None),
            "Already mounted"
        );
        assert_eq!(
            e("org.freedesktop.UDisks2.Error.NotMounted", None),
            "Not mounted"
        );
        assert_eq!(
            e("org.freedesktop.DBus.Error.ServiceUnknown", None),
            "UDisks2 is not running"
        );
        assert_eq!(
            e("org.freedesktop.DBus.Error.UnknownObject", None),
            "Device is no longer available"
        );
        assert_eq!(
            e(
                "org.freedesktop.UDisks2.Error.Failed",
                Some("Error mounting /dev/sdb1 at /run/media/x: wrong fs type\nmore")
            ),
            "wrong fs type"
        );
        assert_eq!(e("org.freedesktop.UDisks2.Error.Failed", None), "Failed");
        assert_eq!(e("com.example.Weird", Some("  ")), "com.example.Weird");
    }

    #[test]
    fn collect_changed_maps_to_parent() {
        use notify::event::{AccessKind, CreateKind, EventKind};
        let a = PathBuf::from("/w/a");
        let b = PathBuf::from("/w/a/b");
        let watched: HashSet<PathBuf> = [a.clone(), b.clone()].into();
        let mut changed = HashSet::new();
        let ev = |k, p: &str| notify::Event::new(k).add_path(PathBuf::from(p));
        collect_changed(
            &ev(EventKind::Access(AccessKind::Any), "/w/a/f"),
            &watched,
            &mut changed,
        );
        assert!(changed.is_empty());
        collect_changed(
            &ev(EventKind::Create(CreateKind::File), "/w/other/f"),
            &watched,
            &mut changed,
        );
        assert!(changed.is_empty());
        collect_changed(
            &ev(EventKind::Create(CreateKind::Folder), "/w/a/b"),
            &watched,
            &mut changed,
        );
        assert_eq!(changed, [a.clone(), b.clone()].into());
    }

    // ------------------------------------------------ watch_dirs (real fs)

    fn test_dir(tag: &str) -> PathBuf {
        let base = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap()
            .join(".cache/ogx-agent-dev-tests");
        let d = base.join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    async fn next_batch<S: Stream<Item = Vec<PathBuf>> + Unpin>(
        s: &mut S,
        wait_ms: u64,
    ) -> Option<Vec<PathBuf>> {
        tokio::time::timeout(Duration::from_millis(wait_ms), s.next())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_dirs_reports_and_debounces() {
        let root = test_dir("watch");
        let a = root.join("a");
        let b = root.join("b");
        let unwatched = root.join("c");
        for d in [&a, &b, &unwatched] {
            std::fs::create_dir_all(d).unwrap();
        }
        let missing = root.join("missing");
        let mut s = Box::pin(watch_dirs(vec![a.clone(), missing, b.clone(), a.clone()]));
        // let the watcher register (the stream starts on first poll)
        assert!(next_batch(&mut s, 200).await.is_none());

        // a burst of creates/renames/deletes in `a` coalesces into one batch
        for i in 0..20 {
            std::fs::write(a.join(format!("f{i}")), b"x").unwrap();
        }
        std::fs::rename(a.join("f0"), a.join("g0")).unwrap();
        std::fs::remove_file(a.join("f1")).unwrap();
        std::fs::write(unwatched.join("z"), b"z").unwrap();
        let batch = next_batch(&mut s, 2000).await.expect("batch for a");
        assert_eq!(batch, vec![a.clone()]);
        assert!(
            next_batch(&mut s, 600).await.is_none(),
            "burst not coalesced"
        );

        // changes in both dirs → both reported, deduped, in input order
        std::fs::write(b.join("x"), b"x").unwrap();
        std::fs::remove_file(a.join("g0")).unwrap();
        let batch = next_batch(&mut s, 2000).await.expect("batch for a+b");
        assert_eq!(batch, vec![a.clone(), b.clone()]);

        // unwatched dir only → nothing
        std::fs::write(unwatched.join("y"), b"y").unwrap();
        assert!(next_batch(&mut s, 600).await.is_none());

        drop(s);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_dirs_max_latency() {
        let root = test_dir("latency");
        let mut s = Box::pin(watch_dirs(vec![root.clone()]));
        assert!(next_batch(&mut s, 200).await.is_none());
        // continuous writes every 100 ms never go quiet; the 1 s cap still flushes
        let r2 = root.clone();
        let writer = tokio::spawn(async move {
            for i in 0..25 {
                std::fs::write(r2.join(format!("w{i}")), b"w").unwrap();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
        let t = tokio::time::Instant::now();
        let batch = next_batch(&mut s, 2500).await.expect("flushed by cap");
        assert_eq!(batch, vec![root.clone()]);
        assert!(
            t.elapsed() < Duration::from_millis(1800),
            "{:?}",
            t.elapsed()
        );
        writer.await.unwrap();
        drop(s);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn watch_dirs_all_missing_is_quiet() {
        let mut s = Box::pin(watch_dirs(vec![PathBuf::from("/nonexistent/ogx-test-dir")]));
        assert!(next_batch(&mut s, 300).await.is_none());
    }

    /// The state-changing calls are never invoked in tests (they would act
    /// on the real desktop); this pins their signatures and keeps the test
    /// build warning-free while the UI isn't wired yet.
    #[test]
    fn api_is_referenced_not_called() {
        let _mount = mount;
        let _unmount = unmount;
        let _eject = eject;
        let _watch = watch_drives;
    }

    /// Read-only: GetManagedObjects on the real system bus.
    #[tokio::test]
    #[ignore]
    async fn live_list() {
        let drives = list().await.expect("list");
        for d in &drives {
            println!(
                "{:<14} {:<20} {:<8} {:>14} mnt={:?} rem={} ej={} enc={} sys={}",
                d.device.display(),
                d.label,
                d.fs_type,
                d.size,
                d.mount_point,
                d.removable,
                d.ejectable,
                d.encrypted,
                d.system
            );
        }
    }
}
