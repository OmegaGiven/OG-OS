//! Bluetooth for og-settings: BlueZ over D-Bus (zbus), a built-in pairing
//! agent, live device list and async pair/connect flows.
//!
//! * [`model`] — plain data + pure logic (parsing, grouping, error text)
//! * [`agent`] — `org.bluez.Agent1` implementation and the prompt broker
//! * [`dbus`]  — the event subscription and every bus call
//!
//! [`BtUi`] is the UI-side state; `App` forwards `Message::Bluetooth`
//! here and renders it via `tabs::bluetooth`.

pub mod agent;
pub mod dbus;
pub mod model;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use iced::Task;

pub use agent::{AgentEvent, AgentPrompt, AgentReply, AgentRequest};
pub use model::{BtAdapter, BtDevice, BtError, BtState};

/// Discovery started from "Add device" stops by itself after this long.
pub const SCAN_DURATION: Duration = Duration::from_secs(120);

#[derive(Debug, Clone)]
pub enum BtMsg {
    // From the event subscription
    State(BtState),
    BusError(String),
    AgentRegistered(Result<(), String>),
    Agent(AgentEvent),

    // Adapter
    SetPowered(bool),
    SetDiscoverable(bool),
    SetPairable(bool),
    AdapterDone(Result<(), BtError>),
    DismissAdapterError,

    // Discovery
    ScanStart,
    ScanStop,
    ScanTick,
    ScanDone(Result<(), BtError>),
    ToggleUnnamed,

    // Per-device actions (keyed by address — the object path disappears
    // while a device is being forgotten and re-discovered)
    Pair(String),
    Repair(String),
    Connect(String),
    Disconnect(String),
    Forget(String),
    OpProgress(String, String),
    OpDone(String, OpOutcome),
    DismissOp(String),

    // Agent dialog
    PromptInput(String),
    PromptAnswer(u64, bool),
    PromptCancelDisplay(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpOutcome {
    Ok,
    /// `paired`: the pairing step succeeded and only connecting failed.
    Failed {
        error: BtError,
        paired: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpState {
    Busy(String),
    Failed { message: String, repair: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceOp {
    pub name: String,
    pub icon: Option<String>,
    pub airpods: bool,
    pub state: OpState,
}

impl DeviceOp {
    pub fn is_busy(&self) -> bool {
        matches!(self.state, OpState::Busy(_))
    }
}

/// Hint shown while waiting for a device to (re)appear in pairing mode.
pub fn pairing_mode_hint(airpods: bool) -> &'static str {
    if airpods {
        "Open the case near this computer and hold the button on the back until the light flashes white."
    } else {
        "Put the device in pairing mode (usually: hold its power or Bluetooth button until a light flashes)."
    }
}

#[derive(Debug, Default)]
pub struct BtUi {
    pub state: BtState,
    /// Set when the system bus or bluetoothd can't be reached.
    pub bus_error: Option<String>,
    /// `None` until the first registration attempt finished.
    pub agent: Option<Result<(), String>>,
    pub adapter_error: Option<String>,
    pub ops: BTreeMap<String, DeviceOp>,
    /// Discovery we started is on until this instant.
    pub scan_deadline: Option<Instant>,
    /// Discovery paused for a pairing; resume when it finishes.
    pub scan_paused: bool,
    pub show_unnamed: bool,
    /// Open agent dialogs; the first one is shown.
    pub prompts: Vec<AgentRequest>,
    pub prompt_input: String,
}

impl BtUi {
    pub fn adapter(&self) -> Option<&BtAdapter> {
        self.state.adapter()
    }

    pub fn scanning(&self) -> bool {
        self.scan_deadline.is_some()
    }

    pub fn scan_remaining(&self) -> Option<Duration> {
        self.scan_deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    fn adapter_path(&self) -> Option<String> {
        self.adapter().map(|a| a.path.clone())
    }

    fn device(&self, address: &str) -> Option<&BtDevice> {
        self.state.device_by_address(address)
    }

    fn device_path_for(&self, address: &str) -> Option<String> {
        self.device(address)
            .map(|d| d.path.clone())
            .or_else(|| self.adapter_path().map(|a| model::device_path(&a, address)))
    }

    fn begin_op(&mut self, address: &str, label: &str) {
        let (name, icon, airpods) = match (self.device(address), self.ops.get(address)) {
            (Some(d), _) => (d.display_name(), d.icon.clone(), d.is_airpods()),
            (None, Some(op)) => (op.name.clone(), op.icon.clone(), op.airpods),
            (None, None) => (address.to_string(), None, false),
        };
        self.ops.insert(
            address.to_string(),
            DeviceOp {
                name,
                icon,
                airpods,
                state: OpState::Busy(label.to_string()),
            },
        );
    }

    fn any_busy(&self) -> bool {
        self.ops.values().any(DeviceOp::is_busy)
    }

    /// The pairing flow stops discovery; remember to restart it.
    fn pause_scan_for_pairing(&mut self) {
        if self.scanning() {
            self.scan_paused = true;
        }
    }

    /// Stop our discovery (tab hidden / timer ran out / Stop pressed).
    pub fn stop_scan(&mut self) -> Task<BtMsg> {
        let was = self.scan_deadline.take().is_some() || self.scan_paused;
        self.scan_paused = false;
        match (was, self.adapter_path()) {
            (true, Some(a)) => Task::perform(dbus::stop_discovery(a), |_| BtMsg::ScanTick),
            _ => Task::none(),
        }
    }

    /// Called when the Network tab (and so the Bluetooth card) is hidden.
    pub fn on_hidden(&mut self) -> Task<BtMsg> {
        self.stop_scan()
    }

    pub fn update(&mut self, msg: BtMsg, visible: bool) -> Task<BtMsg> {
        match msg {
            BtMsg::State(s) => {
                self.state = s;
                self.bus_error = None;
                if !self.adapter().is_some_and(|a| a.powered) {
                    self.scan_deadline = None;
                    self.scan_paused = false;
                }
            }
            BtMsg::BusError(e) => self.bus_error = Some(e),
            BtMsg::AgentRegistered(r) => self.agent = Some(r),
            BtMsg::Agent(ev) => self.on_agent_event(ev),

            BtMsg::SetPowered(on) => return self.set_flag("Powered", on),
            BtMsg::SetDiscoverable(on) => return self.set_flag("Discoverable", on),
            BtMsg::SetPairable(on) => return self.set_flag("Pairable", on),
            BtMsg::AdapterDone(r) => self.adapter_error = r.err().map(|e| e.human()),
            BtMsg::DismissAdapterError => self.adapter_error = None,

            BtMsg::ScanStart => {
                let Some(a) = self.adapter().filter(|a| a.powered).map(|a| a.path.clone()) else {
                    return Task::none();
                };
                self.scan_deadline = Some(Instant::now() + SCAN_DURATION);
                self.adapter_error = None;
                if self.any_busy() {
                    // A pairing is running; it resumes discovery when done.
                    self.scan_paused = true;
                    return Task::none();
                }
                return Task::perform(dbus::start_discovery(a), BtMsg::ScanDone);
            }
            BtMsg::ScanStop => return self.stop_scan(),
            BtMsg::ScanTick => {
                if self.scan_deadline.is_some_and(|d| Instant::now() >= d)
                    || (self.scanning() && !visible)
                {
                    return self.stop_scan();
                }
            }
            BtMsg::ScanDone(r) => {
                if let Err(e) = r {
                    self.scan_deadline = None;
                    self.adapter_error = Some(format!("Couldn't scan: {}", e.human()));
                }
            }
            BtMsg::ToggleUnnamed => self.show_unnamed = !self.show_unnamed,

            BtMsg::Pair(address) => {
                let (Some(adapter), Some(device)) =
                    (self.adapter_path(), self.device(&address).cloned())
                else {
                    return Task::none();
                };
                if self.ops.get(&address).is_some_and(DeviceOp::is_busy) {
                    return Task::none();
                }
                self.pause_scan_for_pairing();
                if device.half_paired() {
                    // Left half-paired by an interrupted attempt: pairing
                    // over the stale entry fails, start from scratch.
                    return self.start_repair(adapter, address, device.path);
                }
                self.begin_op(&address, "Pairing…");
                return op_task(address, move |progress| async move {
                    dbus::pair_and_connect(&adapter, &device.path, progress).await
                });
            }
            BtMsg::Repair(address) => {
                let (Some(adapter), Some(path)) =
                    (self.adapter_path(), self.device_path_for(&address))
                else {
                    return Task::none();
                };
                if self.ops.get(&address).is_some_and(DeviceOp::is_busy) {
                    return Task::none();
                }
                self.pause_scan_for_pairing();
                return self.start_repair(adapter, address, path);
            }
            BtMsg::Connect(address) => {
                let Some(path) = self.device(&address).map(|d| d.path.clone()) else {
                    return Task::none();
                };
                self.begin_op(&address, "Connecting…");
                return Task::perform(dbus::connect(path), move |r| {
                    BtMsg::OpDone(address.clone(), outcome(r))
                });
            }
            BtMsg::Disconnect(address) => {
                let Some(path) = self.device(&address).map(|d| d.path.clone()) else {
                    return Task::none();
                };
                self.begin_op(&address, "Disconnecting…");
                return Task::perform(dbus::disconnect(path), move |r| {
                    BtMsg::OpDone(address.clone(), outcome(r))
                });
            }
            BtMsg::Forget(address) => {
                let (Some(adapter), Some(path)) = (
                    self.adapter_path(),
                    self.device(&address).map(|d| d.path.clone()),
                ) else {
                    return Task::none();
                };
                self.begin_op(&address, "Forgetting…");
                return Task::perform(dbus::forget(adapter, path), move |r| {
                    BtMsg::OpDone(address.clone(), outcome(r))
                });
            }
            BtMsg::OpProgress(address, label) => {
                if let Some(op) = self.ops.get_mut(&address) {
                    op.state = OpState::Busy(label);
                }
            }
            BtMsg::OpDone(address, result) => return self.op_done(address, result, visible),
            BtMsg::DismissOp(address) => {
                if self.ops.get(&address).is_some_and(|op| !op.is_busy()) {
                    self.ops.remove(&address);
                }
            }

            BtMsg::PromptInput(s) => self.prompt_input = s,
            BtMsg::PromptAnswer(id, yes) => {
                if let Some(pos) = self.prompts.iter().position(|p| p.id == id) {
                    let req = self.prompts.remove(pos);
                    let reply = match (yes, req.prompt.wants_text()) {
                        (false, _) => AgentReply::No,
                        (true, true) => AgentReply::Text(std::mem::take(&mut self.prompt_input)),
                        (true, false) => AgentReply::Yes,
                    };
                    agent::respond(id, reply);
                    self.prompt_input.clear();
                }
            }
            BtMsg::PromptCancelDisplay(id) => {
                if let Some(pos) = self.prompts.iter().position(|p| p.id == id) {
                    let req = self.prompts.remove(pos);
                    return Task::perform(dbus::cancel_pairing(req.device), |_| BtMsg::ScanTick);
                }
            }
        }
        Task::none()
    }

    fn set_flag(&mut self, prop: &'static str, on: bool) -> Task<BtMsg> {
        let Some(a) = self.adapter_path() else {
            return Task::none();
        };
        self.adapter_error = None;
        Task::perform(dbus::set_adapter_flag(a, prop, on), BtMsg::AdapterDone)
    }

    fn start_repair(&mut self, adapter: String, address: String, path: String) -> Task<BtMsg> {
        self.begin_op(&address, "Forgetting old pairing…");
        let addr = address.clone();
        op_task(address, move |progress| async move {
            dbus::repair(&adapter, &addr, &path, progress).await
        })
    }

    fn op_done(&mut self, address: String, result: OpOutcome, visible: bool) -> Task<BtMsg> {
        let device = self.device(&address).cloned();
        if let Some(path) = device
            .as_ref()
            .map(|d| d.path.clone())
            .or_else(|| self.device_path_for(&address))
        {
            self.prompts
                .retain(|p| !(p.device == path && p.prompt.is_display()));
        }
        match result {
            OpOutcome::Ok => {
                self.ops.remove(&address);
            }
            OpOutcome::Failed { error, paired } => {
                let human = error.human();
                let message = if paired {
                    format!("Paired, but couldn't connect: {human}")
                } else {
                    human
                };
                let repair =
                    error.suggests_repair() || device.as_ref().is_some_and(BtDevice::half_paired);
                let entry = self.ops.entry(address.clone()).or_insert_with(|| DeviceOp {
                    name: device
                        .as_ref()
                        .map(BtDevice::display_name)
                        .unwrap_or_else(|| address.clone()),
                    icon: device.as_ref().and_then(|d| d.icon.clone()),
                    airpods: device.as_ref().is_some_and(BtDevice::is_airpods),
                    state: OpState::Busy(String::new()),
                });
                entry.state = OpState::Failed { message, repair };
            }
        }
        if self.scan_paused && !self.any_busy() {
            self.scan_paused = false;
            let live = self.scan_deadline.is_some_and(|d| Instant::now() < d);
            if live && visible {
                if let Some(a) = self.adapter().filter(|a| a.powered) {
                    return Task::perform(dbus::start_discovery(a.path.clone()), BtMsg::ScanDone);
                }
            } else {
                self.scan_deadline = None;
            }
        }
        Task::none()
    }

    fn on_agent_event(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::Request(req) => {
                if req.prompt.is_display() {
                    // DisplayPasskey repeats as digits are typed: update in place.
                    self.prompts
                        .retain(|p| !(p.device == req.device && p.prompt.is_display()));
                }
                if self.prompts.iter().all(|p| p.prompt.is_display()) {
                    self.prompt_input.clear();
                }
                self.prompts.push(req);
            }
            AgentEvent::Dismiss(id) => self.prompts.retain(|p| p.id != id),
            AgentEvent::DismissAll => self.prompts.clear(),
        }
    }

    /// Name for a device path in dialogs.
    pub fn name_for_path(&self, path: &str) -> String {
        self.state
            .devices
            .get(path)
            .map(BtDevice::display_name)
            .unwrap_or_else(|| {
                path.rsplit_once("dev_")
                    .map(|(_, a)| a.replace('_', ":"))
                    .unwrap_or_else(|| path.to_string())
            })
    }
}

fn outcome(r: Result<(), BtError>) -> OpOutcome {
    match r {
        Ok(()) => OpOutcome::Ok,
        Err(error) => OpOutcome::Failed {
            error,
            paired: false,
        },
    }
}

/// Run a multi-step device operation, streaming its progress labels.
fn op_task<F, Fut>(address: String, f: F) -> Task<BtMsg>
where
    F: FnOnce(Box<dyn FnMut(String) + Send>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), dbus::PairFailure>> + Send + 'static,
{
    use iced::futures::SinkExt;
    Task::stream(iced::stream::channel(8, move |mut out| async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let fut = f(Box::new(move |label| {
            let _ = tx.send(label);
        }));
        tokio::pin!(fut);
        let result = loop {
            tokio::select! {
                r = &mut fut => break r,
                Some(label) = rx.recv() => {
                    let _ = out.send(BtMsg::OpProgress(address.clone(), label)).await;
                }
            }
        };
        while let Ok(label) = rx.try_recv() {
            let _ = out.send(BtMsg::OpProgress(address.clone(), label)).await;
        }
        let done = match result {
            Ok(()) => OpOutcome::Ok,
            Err(f) => OpOutcome::Failed {
                error: f.error,
                paired: f.paired,
            },
        };
        let _ = out.send(BtMsg::OpDone(address, done)).await;
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::BtDevice;

    const HCI: &str = "/org/bluez/hci0";
    const POD_ADDR: &str = "18:E6:71:0E:1A:9F";

    fn ui_with(devices: Vec<BtDevice>) -> BtUi {
        let mut state = BtState::default();
        state.adapters.insert(
            HCI.into(),
            BtAdapter {
                path: HCI.into(),
                powered: true,
                alias: "STATION".into(),
                ..Default::default()
            },
        );
        for d in devices {
            state.devices.insert(d.path.clone(), d);
        }
        let mut ui = BtUi::default();
        let _ = ui.update(BtMsg::State(state), true);
        ui
    }

    fn airpods(half: bool) -> BtDevice {
        BtDevice {
            path: model::device_path(HCI, POD_ADDR),
            adapter: HCI.into(),
            address: POD_ADDR.into(),
            name: Some("AirPods Pro".into()),
            alias: Some("AirPods Pro".into()),
            icon: Some("audio-headphones".into()),
            trusted: half,
            ..Default::default()
        }
    }

    #[test]
    fn half_paired_device_goes_straight_to_repair() {
        let mut ui = ui_with(vec![airpods(true)]);
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        let op = &ui.ops[POD_ADDR];
        assert_eq!(op.state, OpState::Busy("Forgetting old pairing…".into()));
        assert!(op.airpods);
        // A second click while busy is ignored.
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        assert_eq!(ui.ops.len(), 1);
    }

    #[test]
    fn normal_pair_shows_progress_then_clears() {
        let mut ui = ui_with(vec![airpods(false)]);
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        assert_eq!(ui.ops[POD_ADDR].state, OpState::Busy("Pairing…".into()));
        let _ = ui.update(
            BtMsg::OpProgress(POD_ADDR.into(), "Connecting…".into()),
            true,
        );
        assert_eq!(ui.ops[POD_ADDR].state, OpState::Busy("Connecting…".into()));
        let _ = ui.update(BtMsg::OpDone(POD_ADDR.into(), OpOutcome::Ok), true);
        assert!(ui.ops.is_empty());
    }

    #[test]
    fn failures_offer_repair_and_keep_op_after_device_vanishes() {
        let mut ui = ui_with(vec![airpods(false)]);
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        // Device object removed mid-flow (e.g. forgotten): op stays keyed by address.
        let mut empty = ui.state.clone();
        empty.devices.clear();
        let _ = ui.update(BtMsg::State(empty), true);
        let err = BtError::new(
            "org.bluez.Error.AuthenticationFailed",
            "Authentication Failed",
        );
        let _ = ui.update(
            BtMsg::OpDone(
                POD_ADDR.into(),
                OpOutcome::Failed {
                    error: err,
                    paired: false,
                },
            ),
            true,
        );
        match &ui.ops[POD_ADDR].state {
            OpState::Failed { message, repair } => {
                assert!(message.contains("pairing mode"));
                assert!(*repair);
            }
            s => panic!("{s:?}"),
        }
        assert_eq!(ui.ops[POD_ADDR].name, "AirPods Pro");
        let _ = ui.update(BtMsg::DismissOp(POD_ADDR.into()), true);
        assert!(ui.ops.is_empty());
    }

    #[test]
    fn connect_failure_after_pairing_explains_profile_problem() {
        let mut ui = ui_with(vec![airpods(false)]);
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        let err = BtError::new(
            "org.bluez.Error.Failed",
            "br-connection-profile-unavailable",
        );
        let _ = ui.update(
            BtMsg::OpDone(
                POD_ADDR.into(),
                OpOutcome::Failed {
                    error: err,
                    paired: true,
                },
            ),
            true,
        );
        match &ui.ops[POD_ADDR].state {
            OpState::Failed { message, repair } => {
                assert!(message.starts_with("Paired, but couldn't connect"));
                assert!(message.contains("PipeWire"));
                assert!(!*repair);
            }
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn scan_lifecycle() {
        let mut ui = ui_with(vec![]);
        let _ = ui.update(BtMsg::ScanStart, true);
        assert!(ui.scanning());
        assert!(ui.scan_remaining().unwrap() > Duration::from_secs(100));
        // Pairing pauses, finishing resumes (deadline still in the future).
        ui.state
            .devices
            .insert(airpods(false).path.clone(), airpods(false));
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        assert!(ui.scan_paused);
        let _ = ui.update(BtMsg::OpDone(POD_ADDR.into(), OpOutcome::Ok), true);
        assert!(!ui.scan_paused && ui.scanning());
        // Hidden tab stops it.
        let _ = ui.update(BtMsg::ScanTick, false);
        assert!(!ui.scanning());
        // Expired deadline stops it.
        let _ = ui.update(BtMsg::ScanStart, true);
        ui.scan_deadline = Some(Instant::now() - Duration::from_secs(1));
        let _ = ui.update(BtMsg::ScanTick, true);
        assert!(!ui.scanning());
        // Powering off clears it.
        let _ = ui.update(BtMsg::ScanStart, true);
        let mut off = ui.state.clone();
        off.adapters.get_mut(HCI).unwrap().powered = false;
        let _ = ui.update(BtMsg::State(off), true);
        assert!(!ui.scanning());
        // Can't start while powered off.
        let _ = ui.update(BtMsg::ScanStart, true);
        assert!(!ui.scanning());
    }

    #[test]
    fn display_prompts_replace_and_close_with_pairing() {
        let mut ui = ui_with(vec![airpods(false)]);
        let dev = airpods(false).path;
        let req = |id, entered| AgentRequest {
            id,
            device: dev.clone(),
            prompt: AgentPrompt::DisplayPasskey {
                passkey: 123456,
                entered,
            },
        };
        let _ = ui.update(BtMsg::Agent(AgentEvent::Request(req(1, 0))), true);
        let _ = ui.update(BtMsg::Agent(AgentEvent::Request(req(2, 3))), true);
        assert_eq!(ui.prompts.len(), 1);
        assert_eq!(
            ui.prompts[0].prompt,
            AgentPrompt::DisplayPasskey {
                passkey: 123456,
                entered: 3
            }
        );
        let _ = ui.update(BtMsg::Pair(POD_ADDR.into()), true);
        let _ = ui.update(BtMsg::OpDone(POD_ADDR.into(), OpOutcome::Ok), true);
        assert!(ui.prompts.is_empty());
        assert_eq!(ui.name_for_path(&dev), "AirPods Pro");
        assert_eq!(
            ui.name_for_path("/org/bluez/hci0/dev_00_11_22_33_44_55"),
            "00:11:22:33:44:55"
        );
    }

    #[test]
    fn dismiss_events_close_dialogs() {
        let mut ui = ui_with(vec![]);
        let r = |id| AgentRequest {
            id,
            device: "/x".into(),
            prompt: AgentPrompt::Authorize,
        };
        let _ = ui.update(BtMsg::Agent(AgentEvent::Request(r(1))), true);
        let _ = ui.update(BtMsg::Agent(AgentEvent::Request(r(2))), true);
        let _ = ui.update(BtMsg::Agent(AgentEvent::Dismiss(1)), true);
        assert_eq!(ui.prompts.iter().map(|p| p.id).collect::<Vec<_>>(), [2]);
        let _ = ui.update(BtMsg::Agent(AgentEvent::DismissAll), true);
        assert!(ui.prompts.is_empty());
    }

    /// Agent request → dialog → user answer → D-Bus method result, with
    /// the real broker and channels (no bus).
    #[test]
    fn dialog_answers_reach_the_waiting_agent_call() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let (broker, mut events) = agent::Broker::new();
            agent::set_broker(broker.clone());
            let mut ui = ui_with(vec![airpods(false)]);
            let dev = airpods(false).path;

            // RequestConfirmation → Yes
            let b = broker.clone();
            let d = dev.clone();
            let call = tokio::spawn(async move {
                b.ask(
                    &d,
                    AgentPrompt::Confirm { passkey: 7 },
                    agent::PROMPT_TIMEOUT,
                )
                .await
            });
            let ev = events.recv().await.unwrap();
            let _ = ui.update(BtMsg::Agent(ev), true);
            let id = ui.prompts[0].id;
            let _ = ui.update(BtMsg::PromptAnswer(id, true), true);
            assert_eq!(call.await.unwrap(), Ok(AgentReply::Yes));
            assert!(ui.prompts.is_empty());

            // RequestPasskey → typed text
            let b = broker.clone();
            let d = dev.clone();
            let call = tokio::spawn(async move {
                b.ask(&d, AgentPrompt::Passkey, agent::PROMPT_TIMEOUT).await
            });
            let _ = ui.update(BtMsg::Agent(events.recv().await.unwrap()), true);
            let _ = ui.update(BtMsg::PromptInput("004711".into()), true);
            let id = ui.prompts[0].id;
            let _ = ui.update(BtMsg::PromptAnswer(id, true), true);
            assert_eq!(call.await.unwrap(), Ok(AgentReply::Text("004711".into())));
            assert!(ui.prompt_input.is_empty());

            // RequestAuthorization → No
            let b = broker.clone();
            let call = tokio::spawn(async move {
                b.ask(&dev, AgentPrompt::Authorize, agent::PROMPT_TIMEOUT)
                    .await
            });
            let _ = ui.update(BtMsg::Agent(events.recv().await.unwrap()), true);
            let id = ui.prompts[0].id;
            let _ = ui.update(BtMsg::PromptAnswer(id, false), true);
            assert_eq!(call.await.unwrap(), Ok(AgentReply::No));
        });
    }
}
