//! BlueZ over the system D-Bus: the live event stream (object tree +
//! agent prompts) and every adapter/device action. Replaces the old
//! `bluetoothctl` shell-outs — nothing here blocks the UI thread.

use std::collections::HashMap;
use std::time::Duration;

use iced::futures::{SinkExt, StreamExt};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

use super::agent::{self, Agent, Broker};
use super::model::{BtError, BtState, Props, ADAPTER_IFACE, DEVICE_IFACE};
use super::BtMsg;

#[cfg(test)]
#[path = "live_tests.rs"]
mod live_tests;

pub const BLUEZ: &str = "org.bluez";
const AGENT_PATH: &str = "/org/og_os/settings/bluetooth_agent";
const AGENT_CAPABILITY: &str = "KeyboardDisplay";
const PROPS_IFACE: &str = "org.freedesktop.DBus.Properties";

pub const PAIR_TIMEOUT: Duration = Duration::from_secs(40);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long "Forget & pair again" waits for the device to show up again.
pub const REDISCOVER_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn system_bus() -> Result<&'static Connection, BtError> {
    static BUS: tokio::sync::OnceCell<Connection> = tokio::sync::OnceCell::const_new();
    BUS.get_or_try_init(Connection::system)
        .await
        .map_err(BtError::from)
}

async fn call<B>(
    conn: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> Result<zbus::Message, BtError>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(Some(BLUEZ), path, Some(iface), method, body)
        .await
        .map_err(BtError::from)
}

async fn set_prop(
    conn: &Connection,
    path: &str,
    iface: &str,
    name: &str,
    value: Value<'_>,
) -> Result<(), BtError> {
    call(conn, path, PROPS_IFACE, "Set", &(iface, name, value))
        .await
        .map(|_| ())
}

async fn with_timeout<T>(
    limit: Duration,
    what: &str,
    fut: impl std::future::Future<Output = Result<T, BtError>>,
) -> Result<T, BtError> {
    tokio::time::timeout(limit, fut)
        .await
        .unwrap_or_else(|_| Err(BtError::timeout(what)))
}

pub async fn load_state(conn: &Connection) -> Result<BtState, BtError> {
    let reply = call(
        conn,
        "/",
        "org.freedesktop.DBus.ObjectManager",
        "GetManagedObjects",
        &(),
    )
    .await?;
    let objects: HashMap<OwnedObjectPath, HashMap<String, Props>> = reply
        .body()
        .deserialize()
        .map_err(|e| BtError::from(zbus::Error::from(e)))?;
    Ok(BtState::from_managed(
        objects.into_iter().map(|(p, i)| (p.to_string(), i)),
    ))
}

// ── Adapter ──────────────────────────────────────────────────────────────

pub async fn set_adapter_flag(
    adapter: String,
    prop: &'static str,
    on: bool,
) -> Result<(), BtError> {
    let conn = system_bus().await?;
    set_prop(conn, &adapter, ADAPTER_IFACE, prop, Value::from(on)).await
}

pub async fn start_discovery(adapter: String) -> Result<(), BtError> {
    let conn = system_bus().await?;
    let mut filter: HashMap<&str, Value> = HashMap::new();
    filter.insert("Transport", Value::from("auto"));
    filter.insert("DuplicateData", Value::from(false));
    // A rejected filter only costs us duplicate-data suppression.
    let _ = call(
        conn,
        &adapter,
        ADAPTER_IFACE,
        "SetDiscoveryFilter",
        &(filter,),
    )
    .await;
    match call(conn, &adapter, ADAPTER_IFACE, "StartDiscovery", &()).await {
        Ok(_) => Ok(()),
        Err(e) if e.name.ends_with(".InProgress") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Stops *our* discovery session (BlueZ tracks discovery per client).
pub async fn stop_discovery(adapter: String) -> Result<(), BtError> {
    let conn = system_bus().await?;
    match call(conn, &adapter, ADAPTER_IFACE, "StopDiscovery", &()).await {
        Ok(_) => Ok(()),
        // "No discovery started" / not ready: nothing to stop.
        Err(e)
            if e.name.ends_with(".Failed")
                || e.name.ends_with(".NotReady")
                || e.name.ends_with(".NotAuthorized") =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

// ── Devices ──────────────────────────────────────────────────────────────

pub async fn connect(device: String) -> Result<(), BtError> {
    let conn = system_bus().await?;
    with_timeout(CONNECT_TIMEOUT, "while connecting", async {
        match call(conn, &device, DEVICE_IFACE, "Connect", &()).await {
            Ok(_) => Ok(()),
            Err(e) if e.name.ends_with(".AlreadyConnected") => Ok(()),
            Err(e) => Err(e),
        }
    })
    .await
}

pub async fn disconnect(device: String) -> Result<(), BtError> {
    let conn = system_bus().await?;
    with_timeout(
        CONNECT_TIMEOUT,
        "while disconnecting",
        call(conn, &device, DEVICE_IFACE, "Disconnect", &()),
    )
    .await
    .map(|_| ())
}

pub async fn forget(adapter: String, device: String) -> Result<(), BtError> {
    let conn = system_bus().await?;
    let path = ObjectPath::try_from(device.as_str())
        .map_err(|e| BtError::new("og.Other", e.to_string()))?;
    call(conn, &adapter, ADAPTER_IFACE, "RemoveDevice", &(path,))
        .await
        .map(|_| ())
}

pub async fn cancel_pairing(device: String) {
    if let Ok(conn) = system_bus().await {
        let _ = call(conn, &device, DEVICE_IFACE, "CancelPairing", &()).await;
    }
}

/// The full pairing sequence for an unpaired device. Progress labels go
/// out through `progress`; the result is the final outcome.
pub async fn pair_and_connect(
    adapter: &str,
    device: &str,
    mut progress: impl FnMut(String),
) -> Result<(), PairFailure> {
    let conn = system_bus().await.map_err(PairFailure::pairing)?;
    progress("Pairing…".into());
    // Pairable=false makes BlueZ refuse the bonding; discovery running in
    // parallel makes BR/EDR paging flaky — pair with the radio otherwise idle.
    let _ = set_prop(conn, adapter, ADAPTER_IFACE, "Pairable", Value::from(true)).await;
    let _ = stop_discovery(adapter.to_string()).await;

    agent::begin_pairing(device);
    let paired = with_timeout(
        PAIR_TIMEOUT,
        "while pairing",
        call(conn, device, DEVICE_IFACE, "Pair", &()),
    )
    .await;
    agent::end_pairing(device);
    match paired {
        Ok(_) => {}
        Err(e) if e.name.ends_with(".AlreadyExists") => {}
        Err(e) => {
            if e.name == "og.Timeout" {
                let _ = call(conn, device, DEVICE_IFACE, "CancelPairing", &()).await;
            }
            return Err(PairFailure::pairing(e));
        }
    }

    // Trusted: reconnects (and service authorizations) happen without us.
    let _ = set_prop(conn, device, DEVICE_IFACE, "Trusted", Value::from(true)).await;

    progress("Connecting…".into());
    connect(device.to_string()).await.map_err(|e| PairFailure {
        error: e,
        paired: true,
    })
}

/// "Forget & pair again": drop the stale entry, wait for the device to
/// advertise again (it must be in pairing mode), then pair from scratch.
pub async fn repair(
    adapter: &str,
    address: &str,
    device: &str,
    mut progress: impl FnMut(String),
) -> Result<(), PairFailure> {
    let conn = system_bus().await.map_err(PairFailure::pairing)?;
    progress("Forgetting old pairing…".into());
    if let Err(e) = forget(adapter.to_string(), device.to_string()).await {
        if !e.name.ends_with(".DoesNotExist") {
            return Err(PairFailure::pairing(e));
        }
    }
    progress("Waiting for the device — put it in pairing mode…".into());
    let _ = set_prop(conn, adapter, ADAPTER_IFACE, "Pairable", Value::from(true)).await;
    start_discovery(adapter.to_string())
        .await
        .map_err(PairFailure::pairing)?;
    let deadline = tokio::time::Instant::now() + REDISCOVER_TIMEOUT;
    let found = loop {
        if let Ok(state) = load_state(conn).await {
            if let Some(d) = state.device_by_address(address) {
                // Seen *now* (has RSSI), not just re-created from cache.
                if d.rssi.is_some() || d.connected {
                    break Some(d.path.clone());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            break None;
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
    };
    let Some(path) = found else {
        let _ = stop_discovery(adapter.to_string()).await;
        return Err(PairFailure::pairing(BtError::new(
            "og.NotFound",
            "The device didn't show up — is it in pairing mode and close by?",
        )));
    };
    pair_and_connect(adapter, &path, progress).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairFailure {
    pub error: BtError,
    /// Pairing itself succeeded; only the connect step failed.
    pub paired: bool,
}

impl PairFailure {
    fn pairing(error: BtError) -> Self {
        Self {
            error,
            paired: false,
        }
    }
}

// ── Live event stream ────────────────────────────────────────────────────

/// Set `OG_SETTINGS_BT_NO_AGENT=1` to run without registering the pairing
/// agent (used when testing a second instance next to a real session).
fn agent_disabled() -> bool {
    std::env::var_os("OG_SETTINGS_BT_NO_AGENT").is_some_and(|v| v != "0" && !v.is_empty())
}

async fn register_agent(conn: &Connection, broker: &std::sync::Arc<Broker>) -> Result<(), BtError> {
    // `at` returns false (no error) if the object is already served.
    conn.object_server()
        .at(
            AGENT_PATH,
            Agent {
                broker: broker.clone(),
                conn: conn.clone(),
            },
        )
        .await
        .map_err(BtError::from)?;
    let path = ObjectPath::try_from(AGENT_PATH).expect("static path");
    match call(
        conn,
        "/org/bluez",
        "org.bluez.AgentManager1",
        "RegisterAgent",
        &(&path, AGENT_CAPABILITY),
    )
    .await
    {
        Ok(_) => {}
        Err(e) if e.name.ends_with(".AlreadyExists") => {}
        Err(e) => return Err(e),
    }
    call(
        conn,
        "/org/bluez",
        "org.bluez.AgentManager1",
        "RequestDefaultAgent",
        &(&path,),
    )
    .await
    .map(|_| ())
}

enum Signal {
    Added(String, HashMap<String, Props>),
    Removed(String, Vec<String>),
    Changed(String, String, Props, Vec<String>),
    /// bluetoothd (re)started or stopped.
    Owner,
}

fn parse_signal(msg: &zbus::Message) -> Option<Signal> {
    let header = msg.header();
    let member = header.member()?.as_str().to_string();
    let body = msg.body();
    match member.as_str() {
        "InterfacesAdded" => {
            let (path, ifaces): (OwnedObjectPath, HashMap<String, Props>) =
                body.deserialize().ok()?;
            Some(Signal::Added(path.to_string(), ifaces))
        }
        "InterfacesRemoved" => {
            let (path, ifaces): (OwnedObjectPath, Vec<String>) = body.deserialize().ok()?;
            Some(Signal::Removed(path.to_string(), ifaces))
        }
        "PropertiesChanged" => {
            let path = header.path()?.to_string();
            let (iface, changed, invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
                body.deserialize().ok()?;
            Some(Signal::Changed(path, iface, changed, invalidated))
        }
        "NameOwnerChanged" => Some(Signal::Owner),
        _ => None,
    }
}

fn apply(state: &mut BtState, sig: Signal) {
    match sig {
        Signal::Added(path, ifaces) => {
            for (iface, props) in ifaces {
                state.interface_added(&path, &iface, &props);
            }
        }
        Signal::Removed(path, ifaces) => state.interfaces_removed(&path, &ifaces),
        Signal::Changed(path, iface, changed, invalidated) => {
            state.properties_changed(&path, &iface, &changed, &invalidated)
        }
        Signal::Owner => {}
    }
}

/// Long-lived subscription: registers the agent, sends the initial object
/// tree, then a fresh `BtState` after every batch of BlueZ signals, and
/// forwards agent prompts.
pub fn events() -> impl iced::futures::Stream<Item = BtMsg> {
    iced::stream::channel(64, |mut out| async move {
        use zbus::message::Type;
        use zbus::{MatchRule, MessageStream};

        let conn = match system_bus().await {
            Ok(c) => c,
            Err(e) => {
                let _ = out.send(BtMsg::BusError(e.human())).await;
                return;
            }
        };

        let rules = [
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(BLUEZ)
                .and_then(|b| b.interface("org.freedesktop.DBus.ObjectManager"))
                .map(|b| b.build()),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(BLUEZ)
                .and_then(|b| b.interface(PROPS_IFACE))
                .and_then(|b| b.member("PropertiesChanged"))
                .and_then(|b| b.path_namespace("/org/bluez"))
                .map(|b| b.build()),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender("org.freedesktop.DBus")
                .and_then(|b| b.member("NameOwnerChanged"))
                .and_then(|b| b.arg(0, BLUEZ))
                .map(|b| b.build()),
        ];
        let mut streams = Vec::new();
        for rule in rules.into_iter().flatten() {
            if let Ok(s) = MessageStream::for_match_rule(rule, conn, Some(256)).await {
                streams.push(s);
            }
        }
        let mut signals = iced::futures::stream::select_all(streams);

        let (broker, mut agent_events) = Broker::new();
        agent::set_broker(broker.clone());
        let use_agent = !agent_disabled();
        let reg = |conn: &'static Connection, broker: std::sync::Arc<Broker>| async move {
            BtMsg::AgentRegistered(register_agent(conn, &broker).await.map_err(|e| e.human()))
        };

        let mut state = match load_state(conn).await {
            Ok(s) => s,
            Err(e) => {
                let _ = out.send(BtMsg::BusError(e.human())).await;
                BtState::default()
            }
        };
        // Registered once an adapter exists (also covers hot-plugged
        // dongles and bluetoothd restarts, which reset `agent_done`).
        let mut agent_done = false;
        if use_agent && !state.adapters.is_empty() {
            agent_done = true;
            let _ = out.send(reg(conn, broker.clone()).await).await;
        }
        if out.send(BtMsg::State(state.clone())).await.is_err() {
            return;
        }

        loop {
            tokio::select! {
                msg = signals.next() => {
                    let Some(msg) = msg else { return };
                    let mut reload = false;
                    let mut handle = |m: zbus::Result<zbus::Message>, state: &mut BtState| {
                        if let Ok(m) = m {
                            match parse_signal(&m) {
                                Some(Signal::Owner) => reload = true,
                                Some(sig) => apply(state, sig),
                                None => {}
                            }
                        }
                    };
                    handle(msg, &mut state);
                    // Coalesce bursts (RSSI updates while scanning) into one UI update.
                    while let Some(Some(m)) = iced::futures::FutureExt::now_or_never(signals.next()) {
                        handle(m, &mut state);
                    }
                    if reload {
                        // bluetoothd restarted: our agent registration and
                        // every object path are gone. Give it a moment.
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        state = load_state(conn).await.unwrap_or_default();
                        agent_done = false;
                    }
                    if use_agent && !agent_done && !state.adapters.is_empty() {
                        agent_done = true;
                        let _ = out.send(reg(conn, broker.clone()).await).await;
                    }
                    if out.send(BtMsg::State(state.clone())).await.is_err() {
                        return;
                    }
                }
                ev = agent_events.recv() => {
                    let Some(ev) = ev else { return };
                    if out.send(BtMsg::Agent(ev)).await.is_err() {
                        return;
                    }
                }
            }
        }
    })
}
