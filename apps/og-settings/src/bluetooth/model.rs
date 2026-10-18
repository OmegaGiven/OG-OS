//! Plain-data model of BlueZ's object tree (adapters, devices, batteries)
//! plus the pure logic built on it: property parsing from D-Bus variant
//! maps, live-update application, list grouping/sorting and error text.
//! Nothing here touches the bus, so all of it is unit-tested.

use std::collections::{BTreeMap, HashMap};

use zbus::zvariant::OwnedValue;

pub const ADAPTER_IFACE: &str = "org.bluez.Adapter1";
pub const DEVICE_IFACE: &str = "org.bluez.Device1";
pub const BATTERY_IFACE: &str = "org.bluez.Battery1";

/// D-Bus property map as BlueZ sends it (`a{sv}`).
pub type Props = HashMap<String, OwnedValue>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtAdapter {
    pub path: String,
    pub address: String,
    pub alias: String,
    pub powered: bool,
    pub discoverable: bool,
    pub pairable: bool,
    pub discovering: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtDevice {
    pub path: String,
    pub adapter: String,
    pub address: String,
    /// The device's own advertised name. `None` for devices that never
    /// sent one — BlueZ then fills `Alias` with the dashed address.
    pub name: Option<String>,
    pub alias: Option<String>,
    pub icon: Option<String>,
    pub paired: bool,
    pub bonded: bool,
    pub trusted: bool,
    pub connected: bool,
    pub blocked: bool,
    pub rssi: Option<i16>,
    pub battery: Option<u8>,
}

impl BtDevice {
    pub fn has_name(&self) -> bool {
        self.name.as_deref().is_some_and(|n| !n.trim().is_empty())
            || self
                .alias
                .as_deref()
                .is_some_and(|a| !a.trim().is_empty() && !looks_like_address(a))
    }

    pub fn display_name(&self) -> String {
        if let Some(a) = self
            .alias
            .as_deref()
            .filter(|a| !a.trim().is_empty() && !looks_like_address(a))
        {
            return a.to_string();
        }
        if let Some(n) = self.name.as_deref().filter(|n| !n.trim().is_empty()) {
            return n.to_string();
        }
        self.address.clone()
    }

    /// Saved on this machine: shows under "My devices".
    pub fn is_known(&self) -> bool {
        self.paired || self.bonded
    }

    /// Trusted but holding no pairing keys — what an interrupted pairing
    /// leaves behind (e.g. `bluetoothctl` quitting mid-`pair`). Pairing
    /// from this state tends to fail until the stale entry is removed.
    pub fn half_paired(&self) -> bool {
        self.trusted && !self.paired && !self.bonded
    }

    pub fn is_airpods(&self) -> bool {
        self.display_name().to_lowercase().contains("airpods")
    }
}

/// `AA-BB-CC-DD-EE-FF` / `AA:BB:…` — BlueZ's placeholder alias for
/// devices without a name.
pub fn looks_like_address(s: &str) -> bool {
    let parts: Vec<&str> = s.split(['-', ':']).collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Object path BlueZ uses for a device: `<adapter>/dev_AA_BB_…`.
pub fn device_path(adapter_path: &str, address: &str) -> String {
    format!(
        "{adapter_path}/dev_{}",
        address.replace(':', "_").to_uppercase()
    )
}

fn prop_bool(props: &Props, key: &str) -> Option<bool> {
    props.get(key).and_then(|v| bool::try_from(v).ok())
}

fn prop_string(props: &Props, key: &str) -> Option<String> {
    props
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_string)
}

fn prop_object_path(props: &Props, key: &str) -> Option<String> {
    props.get(key).and_then(|v| {
        zbus::zvariant::ObjectPath::try_from(v.try_clone().ok()?)
            .ok()
            .map(|p| p.to_string())
    })
}

impl BtAdapter {
    pub fn apply(&mut self, props: &Props) {
        if let Some(v) = prop_string(props, "Address") {
            self.address = v;
        }
        if let Some(v) = prop_string(props, "Alias") {
            self.alias = v;
        }
        if let Some(v) = prop_bool(props, "Powered") {
            self.powered = v;
        }
        if let Some(v) = prop_bool(props, "Discoverable") {
            self.discoverable = v;
        }
        if let Some(v) = prop_bool(props, "Pairable") {
            self.pairable = v;
        }
        if let Some(v) = prop_bool(props, "Discovering") {
            self.discovering = v;
        }
    }
}

impl BtDevice {
    pub fn apply(&mut self, props: &Props) {
        if let Some(v) = prop_object_path(props, "Adapter") {
            self.adapter = v;
        }
        if let Some(v) = prop_string(props, "Address") {
            self.address = v;
        }
        if let Some(v) = prop_string(props, "Name") {
            self.name = Some(v);
        }
        if let Some(v) = prop_string(props, "Alias") {
            self.alias = Some(v);
        }
        if let Some(v) = prop_string(props, "Icon") {
            self.icon = Some(v);
        }
        if let Some(v) = prop_bool(props, "Paired") {
            self.paired = v;
        }
        if let Some(v) = prop_bool(props, "Bonded") {
            self.bonded = v;
        }
        if let Some(v) = prop_bool(props, "Trusted") {
            self.trusted = v;
        }
        if let Some(v) = prop_bool(props, "Connected") {
            self.connected = v;
        }
        if let Some(v) = prop_bool(props, "Blocked") {
            self.blocked = v;
        }
        if let Some(v) = props.get("RSSI").and_then(|v| i16::try_from(v).ok()) {
            self.rssi = Some(v);
        }
    }

    pub fn invalidate(&mut self, names: &[String]) {
        for n in names {
            match n.as_str() {
                "RSSI" => self.rssi = None,
                "Name" => self.name = None,
                "Icon" => self.icon = None,
                _ => {}
            }
        }
    }
}

/// Everything og-settings knows about BlueZ, keyed by object path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BtState {
    pub adapters: BTreeMap<String, BtAdapter>,
    pub devices: BTreeMap<String, BtDevice>,
}

impl BtState {
    /// Build from a `GetManagedObjects` reply (path → interface → props).
    pub fn from_managed<I, J>(objects: I) -> Self
    where
        I: IntoIterator<Item = (String, J)>,
        J: IntoIterator<Item = (String, Props)>,
    {
        let mut state = BtState::default();
        for (path, ifaces) in objects {
            for (iface, props) in ifaces {
                state.interface_added(&path, &iface, &props);
            }
        }
        state
    }

    /// The adapter everything in the UI acts on — the first one BlueZ
    /// lists (`hci0` on almost every machine), picked dynamically.
    pub fn adapter(&self) -> Option<&BtAdapter> {
        self.adapters.values().next()
    }

    pub fn device_by_address(&self, address: &str) -> Option<&BtDevice> {
        self.devices
            .values()
            .find(|d| d.address.eq_ignore_ascii_case(address))
    }

    pub fn interface_added(&mut self, path: &str, iface: &str, props: &Props) {
        match iface {
            ADAPTER_IFACE => {
                let a = self
                    .adapters
                    .entry(path.to_string())
                    .or_insert_with(|| BtAdapter {
                        path: path.to_string(),
                        ..Default::default()
                    });
                a.apply(props);
            }
            DEVICE_IFACE => {
                let d = self
                    .devices
                    .entry(path.to_string())
                    .or_insert_with(|| BtDevice {
                        path: path.to_string(),
                        adapter: parent_path(path),
                        ..Default::default()
                    });
                d.apply(props);
            }
            BATTERY_IFACE => {
                if let Some(pct) = props.get("Percentage").and_then(|v| u8::try_from(v).ok()) {
                    let d = self
                        .devices
                        .entry(path.to_string())
                        .or_insert_with(|| BtDevice {
                            path: path.to_string(),
                            adapter: parent_path(path),
                            ..Default::default()
                        });
                    d.battery = Some(pct);
                }
            }
            _ => {}
        }
    }

    pub fn interfaces_removed(&mut self, path: &str, ifaces: &[String]) {
        for iface in ifaces {
            match iface.as_str() {
                ADAPTER_IFACE => {
                    self.adapters.remove(path);
                    self.devices.retain(|_, d| d.adapter != path);
                }
                DEVICE_IFACE => {
                    self.devices.remove(path);
                }
                BATTERY_IFACE => {
                    if let Some(d) = self.devices.get_mut(path) {
                        d.battery = None;
                    }
                }
                _ => {}
            }
        }
    }

    pub fn properties_changed(
        &mut self,
        path: &str,
        iface: &str,
        changed: &Props,
        invalidated: &[String],
    ) {
        match iface {
            ADAPTER_IFACE => {
                if let Some(a) = self.adapters.get_mut(path) {
                    a.apply(changed);
                }
            }
            DEVICE_IFACE => {
                if let Some(d) = self.devices.get_mut(path) {
                    d.apply(changed);
                    d.invalidate(invalidated);
                }
            }
            BATTERY_IFACE => {
                if let Some(d) = self.devices.get_mut(path) {
                    if let Some(pct) = changed.get("Percentage").and_then(|v| u8::try_from(v).ok())
                    {
                        d.battery = Some(pct);
                    }
                }
            }
            _ => {}
        }
    }

    /// Devices of the active adapter split into the three UI lists.
    pub fn grouped(&self) -> DeviceLists<'_> {
        let adapter = self.adapter().map(|a| a.path.as_str());
        let devices = self
            .devices
            .values()
            .filter(|d| Some(d.adapter.as_str()) == adapter && !d.address.is_empty());
        group_devices(devices)
    }
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(p, _)| p.to_string())
        .unwrap_or_default()
}

#[derive(Debug, Default)]
pub struct DeviceLists<'a> {
    /// Paired/bonded: connected first, then by name.
    pub mine: Vec<&'a BtDevice>,
    /// Not paired, with a name: strongest signal first, out-of-range last.
    pub available: Vec<&'a BtDevice>,
    /// Not paired and nameless (mostly random BLE beacons).
    pub unnamed: Vec<&'a BtDevice>,
}

fn by_signal(a: &BtDevice, b: &BtDevice) -> std::cmp::Ordering {
    // `Some` (in range) before `None`, then stronger (less negative) first.
    b.rssi
        .is_some()
        .cmp(&a.rssi.is_some())
        .then(b.rssi.unwrap_or(i16::MIN).cmp(&a.rssi.unwrap_or(i16::MIN)))
        .then_with(|| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
        })
}

pub fn group_devices<'a>(devices: impl IntoIterator<Item = &'a BtDevice>) -> DeviceLists<'a> {
    let mut lists = DeviceLists::default();
    for d in devices {
        if d.is_known() {
            lists.mine.push(d);
        } else if d.has_name() {
            lists.available.push(d);
        } else {
            lists.unnamed.push(d);
        }
    }
    lists.mine.sort_by(|a, b| {
        b.connected.cmp(&a.connected).then_with(|| {
            a.display_name()
                .to_lowercase()
                .cmp(&b.display_name().to_lowercase())
        })
    });
    lists.available.sort_by(|a, b| by_signal(a, b));
    lists.unnamed.sort_by(|a, b| by_signal(a, b));
    lists
}

/// Nerd-font glyph for BlueZ's freedesktop icon name.
pub fn icon_glyph(icon: Option<&str>) -> &'static str {
    match icon.unwrap_or("") {
        "audio-headphones" => "\u{f02cb}",
        "audio-headset" => "\u{f02ce}",
        "audio-card" | "audio-speakers" => "\u{f04c3}",
        "input-keyboard" => "\u{f030c}",
        "input-mouse" => "\u{f037d}",
        "input-gaming" => "\u{f0297}",
        "input-tablet" => "\u{f04f6}",
        "phone" => "\u{f011c}",
        "computer" => "\u{f0322}",
        "video-display" => "\u{f0379}",
        "camera-photo" | "camera-video" => "\u{f0100}",
        "printer" => "\u{f042a}",
        "modem" | "network-wireless" => "\u{f05a9}",
        _ => "\u{f00af}",
    }
}

/// Signal bars 0–4 from RSSI (dBm).
pub fn signal_bars(rssi: Option<i16>) -> u8 {
    match rssi {
        None => 0,
        Some(r) if r >= -55 => 4,
        Some(r) if r >= -67 => 3,
        Some(r) if r >= -78 => 2,
        Some(_) => 1,
    }
}

// ── Errors ────────────────────────────────────────────────────────────────

/// A BlueZ/D-Bus failure reduced to its error name + message, so it can
/// be cloned into iced messages and mapped to friendly text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtError {
    pub name: String,
    pub message: String,
}

impl BtError {
    pub fn new(name: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            message: message.into(),
        }
    }

    pub fn timeout(what: &str) -> Self {
        Self::new("og.Timeout", what)
    }

    pub fn human(&self) -> String {
        humanize_error(&self.name, &self.message)
    }

    /// Whether removing the device and pairing from scratch is likely to
    /// fix this — stale keys, half-finished pairings, auth failures.
    pub fn suggests_repair(&self) -> bool {
        suggests_repair(&self.name, &self.message)
    }
}

impl From<zbus::Error> for BtError {
    fn from(e: zbus::Error) -> Self {
        match e {
            zbus::Error::MethodError(name, detail, _) => {
                BtError::new(name.as_str(), detail.unwrap_or_default())
            }
            zbus::Error::FDO(fdo) => {
                let s = fdo.to_string();
                let (name, msg) = s.split_once(": ").unwrap_or((s.as_str(), ""));
                BtError::new(name.trim(), msg.trim())
            }
            zbus::Error::InputOutput(_) => BtError::new("og.Io", "System bus connection lost"),
            other => BtError::new("og.Other", other.to_string()),
        }
    }
}

pub fn humanize_error(name: &str, message: &str) -> String {
    let short = name.rsplit('.').next().unwrap_or(name);
    let msg = message.to_lowercase();
    let profile_unavailable = msg.contains("profile-unavailable")
        || msg.contains("profile unavailable")
        || msg.contains("exhausted the list of br/edr profiles");
    if profile_unavailable || name.ends_with("BREDR.ProfileUnavailable") {
        return "No audio profile — is PipeWire running?".into();
    }
    if msg.contains("key-missing") {
        return "The device forgot this computer — use “Forget & pair again”.".into();
    }
    if msg.contains("page-timeout")
        || msg.contains("page timeout")
        || msg.contains("host is down")
        || msg.contains("connection-timeout")
    {
        return "The device didn't answer — is it switched on, nearby and in pairing mode?".into();
    }
    if msg.contains("connection-refused") {
        return "The device refused the connection.".into();
    }
    if msg.contains("not-powered") {
        return "Bluetooth is turned off.".into();
    }
    if msg.contains("canceled")
        || msg.contains("aborted-by-local")
        || msg.contains("abort-by-local")
    {
        return "Canceled.".into();
    }
    match (name, short) {
        (_, "AuthenticationFailed") => {
            "Pairing failed — put the device in pairing mode and try again.".into()
        }
        (_, "AuthenticationCanceled") => "Pairing was canceled.".into(),
        (_, "AuthenticationRejected") => "The device rejected the pairing request.".into(),
        (_, "AuthenticationTimeout") => {
            "The device didn't respond in time — put it in pairing mode and try again.".into()
        }
        (_, "ConnectionAttemptFailed") => {
            "Couldn't reach the device — is it nearby, switched on and in pairing mode?".into()
        }
        (_, "AlreadyExists") => "Already paired.".into(),
        (_, "AlreadyConnected") => "Already connected.".into(),
        (_, "InProgress") => "Already in progress — give it a moment.".into(),
        (_, "Busy") => "Bluetooth is busy — try again in a moment.".into(),
        (_, "DoesNotExist") => "The device is gone — scan again.".into(),
        (_, "NotReady") => "The Bluetooth adapter isn't ready — is it turned on?".into(),
        (_, "NotConnected") => "Not connected.".into(),
        (_, "Blocked") => "This device is blocked.".into(),
        (_, "NotAvailable") | (_, "NotSupported") | (_, "Unsupported") => {
            "Not supported by this device.".into()
        }
        (_, "NotAuthorized") | (_, "NotPermitted") => "Not allowed.".into(),
        (_, "Rejected") => "Rejected.".into(),
        (_, "Canceled") => "Canceled.".into(),
        ("org.freedesktop.DBus.Error.ServiceUnknown", _)
        | ("org.freedesktop.DBus.Error.NameHasNoOwner", _) => {
            "The Bluetooth service (bluetoothd) isn't running.".into()
        }
        ("org.freedesktop.DBus.Error.AccessDenied", _) => {
            "Permission denied by the system bus.".into()
        }
        ("org.freedesktop.DBus.Error.NoReply", _) | ("org.freedesktop.DBus.Error.Timeout", _) => {
            "bluetoothd didn't answer in time.".into()
        }
        ("og.Timeout", _) => format!("Timed out {message}."),
        ("og.Io", _) => "Lost the connection to the system bus.".into(),
        (_, "Failed") if !message.is_empty() => format!("Failed: {message}"),
        _ if !message.is_empty() => message.to_string(),
        _ => format!("Failed ({short})."),
    }
}

pub fn suggests_repair(name: &str, message: &str) -> bool {
    let short = name.rsplit('.').next().unwrap_or(name);
    let msg = message.to_lowercase();
    matches!(
        short,
        "AuthenticationFailed"
            | "AuthenticationRejected"
            | "AuthenticationTimeout"
            | "ConnectionAttemptFailed"
            | "AlreadyExists"
    ) || name == "og.Timeout"
        || msg.contains("key-missing")
        || (short == "Failed" && !msg.contains("profile"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{ObjectPath, Value};

    fn ov<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
        v.into().try_into().unwrap()
    }

    fn props(pairs: Vec<(&str, OwnedValue)>) -> Props {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    const HCI: &str = "/org/bluez/hci0";
    const POD: &str = "/org/bluez/hci0/dev_18_E6_71_0E_1A_9F";

    fn airpods_props() -> Props {
        props(vec![
            ("Address", ov("18:E6:71:0E:1A:9F")),
            ("Name", ov("AirPods Pro")),
            ("Alias", ov("AirPods Pro")),
            ("Icon", ov("audio-headphones")),
            ("Adapter", ov(ObjectPath::try_from(HCI).unwrap())),
            ("Paired", ov(false)),
            ("Bonded", ov(false)),
            ("Trusted", ov(true)),
            ("Connected", ov(false)),
            ("Blocked", ov(false)),
        ])
    }

    fn adapter_props() -> Props {
        props(vec![
            ("Address", ov("64:BC:58:B9:1C:8D")),
            ("Alias", ov("STATION-SWAY")),
            ("Powered", ov(true)),
            ("Discoverable", ov(false)),
            ("Pairable", ov(false)),
            ("Discovering", ov(false)),
        ])
    }

    fn base_state() -> BtState {
        let mut s = BtState::default();
        s.interface_added(HCI, ADAPTER_IFACE, &adapter_props());
        s.interface_added(POD, DEVICE_IFACE, &airpods_props());
        s
    }

    #[test]
    fn parses_adapter_and_device_from_variant_maps() {
        let s = base_state();
        let a = s.adapter().unwrap();
        assert_eq!(a.alias, "STATION-SWAY");
        assert!(a.powered && !a.pairable && !a.discovering);
        let d = &s.devices[POD];
        assert_eq!(d.address, "18:E6:71:0E:1A:9F");
        assert_eq!(d.adapter, HCI);
        assert_eq!(d.display_name(), "AirPods Pro");
        assert_eq!(d.icon.as_deref(), Some("audio-headphones"));
        assert!(d.trusted && !d.paired && !d.bonded);
        assert!(d.half_paired());
        assert!(d.is_airpods());
        assert!(!d.is_known());
    }

    #[test]
    fn from_managed_builds_same_state() {
        let objs = vec![
            (
                HCI.to_string(),
                vec![(ADAPTER_IFACE.to_string(), adapter_props())],
            ),
            (
                POD.to_string(),
                vec![
                    (DEVICE_IFACE.to_string(), airpods_props()),
                    (
                        BATTERY_IFACE.to_string(),
                        props(vec![("Percentage", ov(80u8))]),
                    ),
                    (
                        "org.freedesktop.DBus.Introspectable".to_string(),
                        Props::new(),
                    ),
                ],
            ),
        ];
        let s = BtState::from_managed(objs);
        assert_eq!(s.adapters.len(), 1);
        assert_eq!(s.devices[POD].battery, Some(80));
    }

    #[test]
    fn properties_changed_and_invalidated() {
        let mut s = base_state();
        s.properties_changed(
            POD,
            DEVICE_IFACE,
            &props(vec![("RSSI", ov(-60i16)), ("Paired", ov(true))]),
            &[],
        );
        assert_eq!(s.devices[POD].rssi, Some(-60));
        assert!(s.devices[POD].paired && s.devices[POD].is_known());
        s.properties_changed(POD, DEVICE_IFACE, &Props::new(), &["RSSI".to_string()]);
        assert_eq!(s.devices[POD].rssi, None);
        s.properties_changed(
            HCI,
            ADAPTER_IFACE,
            &props(vec![("Discovering", ov(true))]),
            &[],
        );
        assert!(s.adapter().unwrap().discovering);
        // Unknown paths are ignored rather than creating ghost entries.
        s.properties_changed(
            "/org/bluez/hci0/dev_00",
            DEVICE_IFACE,
            &props(vec![("RSSI", ov(-1i16))]),
            &[],
        );
        assert_eq!(s.devices.len(), 1);
    }

    #[test]
    fn battery_and_removal() {
        let mut s = base_state();
        s.interface_added(POD, BATTERY_IFACE, &props(vec![("Percentage", ov(55u8))]));
        assert_eq!(s.devices[POD].battery, Some(55));
        s.properties_changed(
            POD,
            BATTERY_IFACE,
            &props(vec![("Percentage", ov(54u8))]),
            &[],
        );
        assert_eq!(s.devices[POD].battery, Some(54));
        s.interfaces_removed(POD, &[BATTERY_IFACE.to_string()]);
        assert_eq!(s.devices[POD].battery, None);
        s.interfaces_removed(POD, &[DEVICE_IFACE.to_string()]);
        assert!(s.devices.is_empty());
        s.interface_added(POD, DEVICE_IFACE, &airpods_props());
        s.interfaces_removed(HCI, &[ADAPTER_IFACE.to_string()]);
        assert!(s.adapter().is_none() && s.devices.is_empty());
    }

    #[test]
    fn unnamed_devices_detected_via_dashed_alias() {
        let mut d = BtDevice {
            address: "7C:1E:52:00:11:22".into(),
            alias: Some("7C-1E-52-00-11-22".into()),
            ..Default::default()
        };
        assert!(!d.has_name());
        assert_eq!(d.display_name(), "7C:1E:52:00:11:22");
        d.name = Some("Keyboard K380".into());
        assert!(d.has_name());
        assert_eq!(d.display_name(), "Keyboard K380");
        assert!(looks_like_address("aa:bb:cc:dd:ee:ff"));
        assert!(!looks_like_address("AirPods"));
    }

    fn dev(
        addr: &str,
        name: Option<&str>,
        rssi: Option<i16>,
        paired: bool,
        connected: bool,
    ) -> BtDevice {
        BtDevice {
            path: device_path(HCI, addr),
            adapter: HCI.into(),
            address: addr.into(),
            name: name.map(Into::into),
            alias: Some(
                name.map(Into::into)
                    .unwrap_or_else(|| addr.replace(':', "-")),
            ),
            paired,
            connected,
            rssi,
            ..Default::default()
        }
    }

    #[test]
    fn grouping_and_sorting() {
        let devs = vec![
            dev(
                "00:00:00:00:00:01",
                Some("Far speaker"),
                Some(-90),
                false,
                false,
            ),
            dev(
                "00:00:00:00:00:02",
                Some("Near phone"),
                Some(-40),
                false,
                false,
            ),
            dev(
                "00:00:00:00:00:03",
                Some("Stale headset"),
                None,
                false,
                false,
            ),
            dev("00:00:00:00:00:04", None, Some(-30), false, false),
            dev("00:00:00:00:00:05", None, Some(-70), false, false),
            dev("00:00:00:00:00:06", Some("zz Mouse"), None, true, false),
            dev("00:00:00:00:00:07", Some("Keyboard"), None, true, false),
            dev("00:00:00:00:00:08", Some("Xbox"), None, true, true),
        ];
        let l = group_devices(devs.iter());
        let names = |v: &Vec<&BtDevice>| v.iter().map(|d| d.display_name()).collect::<Vec<_>>();
        assert_eq!(names(&l.mine), ["Xbox", "Keyboard", "zz Mouse"]);
        assert_eq!(
            names(&l.available),
            ["Near phone", "Far speaker", "Stale headset"]
        );
        assert_eq!(
            names(&l.unnamed),
            ["00:00:00:00:00:04", "00:00:00:00:00:05"]
        );
    }

    #[test]
    fn grouped_only_uses_first_adapter() {
        let mut s = base_state();
        let other = "/org/bluez/hci1/dev_00_00_00_00_00_09";
        s.interface_added("/org/bluez/hci1", ADAPTER_IFACE, &adapter_props());
        let mut p = airpods_props();
        p.insert(
            "Adapter".into(),
            ov(ObjectPath::try_from("/org/bluez/hci1").unwrap()),
        );
        p.insert("Address".into(), ov("00:00:00:00:00:09"));
        s.interface_added(other, DEVICE_IFACE, &p);
        let l = s.grouped();
        assert_eq!(l.available.len(), 1);
        assert_eq!(l.available[0].path, POD);
    }

    #[test]
    fn device_path_format() {
        assert_eq!(device_path(HCI, "18:e6:71:0e:1a:9f"), POD);
    }

    #[test]
    fn signal_bar_thresholds() {
        assert_eq!(signal_bars(None), 0);
        assert_eq!(signal_bars(Some(-40)), 4);
        assert_eq!(signal_bars(Some(-60)), 3);
        assert_eq!(signal_bars(Some(-75)), 2);
        assert_eq!(signal_bars(Some(-95)), 1);
    }

    #[test]
    fn error_mapping() {
        let h = humanize_error;
        assert!(h(
            "org.bluez.Error.Failed",
            "br-connection-profile-unavailable"
        )
        .contains("PipeWire"));
        assert!(h(
            "org.bluez.Error.BREDR.ProfileUnavailable",
            "Exhausted the list of BR/EDR profiles to connect to"
        )
        .contains("PipeWire"));
        assert!(h(
            "org.bluez.Error.AuthenticationFailed",
            "Authentication Failed"
        )
        .contains("pairing mode"));
        assert_eq!(
            h("org.bluez.Error.AuthenticationCanceled", ""),
            "Pairing was canceled."
        );
        assert!(
            h("org.bluez.Error.ConnectionAttemptFailed", "Page Timeout").contains("didn't answer")
        );
        assert!(h("org.bluez.Error.ConnectionAttemptFailed", "").contains("Couldn't reach"));
        assert_eq!(
            h("org.bluez.Error.AlreadyExists", "Already Exists"),
            "Already paired."
        );
        assert!(h("org.bluez.Error.InProgress", "In Progress").contains("in progress"));
        assert!(h("org.bluez.Error.Failed", "br-connection-key-missing")
            .contains("Forget & pair again"));
        assert!(h("org.freedesktop.DBus.Error.ServiceUnknown", "x").contains("bluetoothd"));
        assert_eq!(
            h("org.bluez.Error.Failed", "weird thing"),
            "Failed: weird thing"
        );
        assert_eq!(h("og.Timeout", "while pairing"), "Timed out while pairing.");
    }

    #[test]
    fn repair_suggestions() {
        assert!(suggests_repair("org.bluez.Error.AuthenticationFailed", ""));
        assert!(suggests_repair("org.bluez.Error.AlreadyExists", ""));
        assert!(suggests_repair(
            "org.bluez.Error.Failed",
            "br-connection-key-missing"
        ));
        assert!(suggests_repair("og.Timeout", "while pairing"));
        assert!(!suggests_repair(
            "org.bluez.Error.Failed",
            "br-connection-profile-unavailable"
        ));
        assert!(!suggests_repair(
            "org.bluez.Error.AuthenticationCanceled",
            ""
        ));
        assert!(!suggests_repair("org.bluez.Error.InProgress", ""));
    }

    #[test]
    fn zbus_method_error_converts() {
        let e = BtError::from(zbus::Error::Failure("boom".into()));
        assert_eq!(e.name, "og.Other");
        let fdo = BtError::from(zbus::Error::FDO(Box::new(zbus::fdo::Error::AccessDenied(
            "nope".into(),
        ))));
        assert_eq!(fdo.name, "org.freedesktop.DBus.Error.AccessDenied");
        assert!(fdo.human().contains("Permission denied"));
    }
}
