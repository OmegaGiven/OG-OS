//! BlueZ pairing agent (`org.bluez.Agent1`), registered for as long as
//! og-settings runs. Requests that need the user (confirm a passkey, type
//! a PIN, …) are handed to the UI through the [`Broker`] and the D-Bus
//! method call simply awaits the user's answer — BlueZ is fine with an
//! agent taking its time. "Just Works" pairings (AirPods, most headsets)
//! never reach the agent at all, or only as an authorization request for a
//! pairing we started ourselves, which is auto-accepted.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use zbus::zvariant::OwnedObjectPath;

/// How long a dialog may stay unanswered before the request is canceled.
pub const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// What the UI should show for one agent request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPrompt {
    /// "Does 123456 match on the device?" — Yes / No.
    Confirm { passkey: u32 },
    /// Incoming pairing request — Allow / Deny.
    Authorize,
    /// An unpaired device wants to use a service — Allow / Deny.
    AuthorizeService { uuid: String },
    /// Text input for a legacy PIN (e.g. "0000").
    PinCode,
    /// Numeric input, 0–999999.
    Passkey,
    /// Show this PIN; the user types it on the device.
    DisplayPinCode { pincode: String },
    /// Show this passkey; `entered` digits have been typed so far.
    DisplayPasskey { passkey: u32, entered: u16 },
}

impl AgentPrompt {
    /// Display-only prompts get no reply; the dialog just closes when the
    /// pairing ends.
    pub fn is_display(&self) -> bool {
        matches!(
            self,
            AgentPrompt::DisplayPinCode { .. } | AgentPrompt::DisplayPasskey { .. }
        )
    }

    pub fn wants_text(&self) -> bool {
        matches!(self, AgentPrompt::PinCode | AgentPrompt::Passkey)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    pub id: u64,
    /// Device object path.
    pub device: String,
    pub prompt: AgentPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentReply {
    Yes,
    No,
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Request(AgentRequest),
    /// Request `id` ended without the UI answering (timeout).
    Dismiss(u64),
    /// BlueZ called `Cancel` (or `Release`): close every open dialog.
    DismissAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskError {
    Canceled,
    Timeout,
}

/// Matches agent requests with UI answers via one-shot channels.
pub struct Broker {
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<AgentReply>>>,
    events: mpsc::UnboundedSender<AgentEvent>,
}

impl Broker {
    pub fn new() -> (Arc<Self>, mpsc::UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Arc::new(Self {
                next_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
                events: tx,
            }),
            rx,
        )
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Show a prompt and wait for the user's answer.
    pub async fn ask(
        &self,
        device: &str,
        prompt: AgentPrompt,
        timeout: Duration,
    ) -> Result<AgentReply, AskError> {
        let id = self.id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _ = self.events.send(AgentEvent::Request(AgentRequest {
            id,
            device: device.to_string(),
            prompt,
        }));
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(AskError::Canceled),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                let _ = self.events.send(AgentEvent::Dismiss(id));
                Err(AskError::Timeout)
            }
        }
    }

    /// Show an informational prompt (no answer expected).
    pub fn show(&self, device: &str, prompt: AgentPrompt) -> u64 {
        let id = self.id();
        let _ = self.events.send(AgentEvent::Request(AgentRequest {
            id,
            device: device.to_string(),
            prompt,
        }));
        id
    }

    /// Deliver the user's answer. False if the request already ended.
    pub fn respond(&self, id: u64, reply: AgentReply) -> bool {
        match self.pending.lock().unwrap().remove(&id) {
            Some(tx) => tx.send(reply).is_ok(),
            None => false,
        }
    }

    /// BlueZ canceled the current request: fail every waiting `ask`.
    pub fn cancel_all(&self) {
        self.pending.lock().unwrap().clear();
        let _ = self.events.send(AgentEvent::DismissAll);
    }
}

// ── Shared state between the UI side and the agent ──────────────────────

static BROKER: OnceLock<Mutex<Option<Arc<Broker>>>> = OnceLock::new();
static PAIRING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub fn set_broker(b: Arc<Broker>) {
    *BROKER.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(b);
}

/// Answer an open prompt from the UI thread.
pub fn respond(id: u64, reply: AgentReply) -> bool {
    BROKER
        .get()
        .and_then(|m| m.lock().unwrap().clone())
        .is_some_and(|b| b.respond(id, reply))
}

/// Mark a device path as "we started pairing it", so the agent can
/// auto-accept the authorization request a Just Works pairing may raise.
pub fn begin_pairing(path: &str) {
    PAIRING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(path.to_string());
}

pub fn end_pairing(path: &str) {
    PAIRING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(path);
}

pub fn is_pairing(path: &str) -> bool {
    PAIRING
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .contains(path)
}

// ── Pure decision helpers (unit-tested) ──────────────────────────────────

/// Parse what the user typed for `RequestPasskey`.
pub fn parse_passkey(input: &str) -> Option<u32> {
    let t = input.trim();
    if t.is_empty() || t.len() > 6 || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// Validate what the user typed for `RequestPinCode` (1–16 chars).
pub fn parse_pin(input: &str) -> Option<String> {
    let t = input.trim();
    (!t.is_empty() && t.len() <= 16 && t.is_ascii()).then(|| t.to_string())
}

/// Six-digit, zero-padded passkey as shown on devices ("012345").
pub fn format_passkey(passkey: u32) -> String {
    format!("{passkey:06}")
}

// ── The D-Bus object ─────────────────────────────────────────────────────

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

fn ask_error(e: AskError) -> AgentError {
    match e {
        AskError::Canceled => AgentError::Canceled("Canceled".into()),
        AskError::Timeout => AgentError::Canceled("No answer".into()),
    }
}

fn yes_or_reject(r: Result<AgentReply, AskError>) -> Result<(), AgentError> {
    match r {
        Ok(AgentReply::Yes) => Ok(()),
        Ok(_) => Err(AgentError::Rejected("Rejected by user".into())),
        Err(e) => Err(ask_error(e)),
    }
}

pub struct Agent {
    pub broker: Arc<Broker>,
    pub conn: zbus::Connection,
}

impl Agent {
    /// Paired or trusted devices may use services without asking.
    async fn device_is_known(&self, device: &str) -> bool {
        let get = |prop: &'static str| async move {
            let reply = self
                .conn
                .call_method(
                    Some(super::dbus::BLUEZ),
                    device,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(super::model::DEVICE_IFACE, prop),
                )
                .await
                .ok()?;
            let v: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
            bool::try_from(&v).ok()
        };
        get("Paired").await == Some(true)
            || get("Bonded").await == Some(true)
            || get("Trusted").await == Some(true)
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {
        self.broker.cancel_all();
    }

    async fn request_pin_code(&self, device: OwnedObjectPath) -> Result<String, AgentError> {
        match self
            .broker
            .ask(device.as_str(), AgentPrompt::PinCode, PROMPT_TIMEOUT)
            .await
        {
            Ok(AgentReply::Text(t)) => {
                parse_pin(&t).ok_or_else(|| AgentError::Rejected("Invalid PIN".into()))
            }
            Ok(_) => Err(AgentError::Rejected("Rejected by user".into())),
            Err(e) => Err(ask_error(e)),
        }
    }

    async fn display_pin_code(&self, device: OwnedObjectPath, pincode: String) {
        self.broker
            .show(device.as_str(), AgentPrompt::DisplayPinCode { pincode });
    }

    async fn request_passkey(&self, device: OwnedObjectPath) -> Result<u32, AgentError> {
        match self
            .broker
            .ask(device.as_str(), AgentPrompt::Passkey, PROMPT_TIMEOUT)
            .await
        {
            Ok(AgentReply::Text(t)) => {
                parse_passkey(&t).ok_or_else(|| AgentError::Rejected("Invalid passkey".into()))
            }
            Ok(_) => Err(AgentError::Rejected("Rejected by user".into())),
            Err(e) => Err(ask_error(e)),
        }
    }

    async fn display_passkey(&self, device: OwnedObjectPath, passkey: u32, entered: u16) {
        self.broker.show(
            device.as_str(),
            AgentPrompt::DisplayPasskey { passkey, entered },
        );
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
    ) -> Result<(), AgentError> {
        yes_or_reject(
            self.broker
                .ask(
                    device.as_str(),
                    AgentPrompt::Confirm { passkey },
                    PROMPT_TIMEOUT,
                )
                .await,
        )
    }

    async fn request_authorization(&self, device: OwnedObjectPath) -> Result<(), AgentError> {
        if is_pairing(device.as_str()) {
            return Ok(());
        }
        yes_or_reject(
            self.broker
                .ask(device.as_str(), AgentPrompt::Authorize, PROMPT_TIMEOUT)
                .await,
        )
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
    ) -> Result<(), AgentError> {
        if is_pairing(device.as_str()) || self.device_is_known(device.as_str()).await {
            return Ok(());
        }
        yes_or_reject(
            self.broker
                .ask(
                    device.as_str(),
                    AgentPrompt::AuthorizeService { uuid },
                    PROMPT_TIMEOUT,
                )
                .await,
        )
    }

    async fn cancel(&self) {
        self.broker.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "/org/bluez/hci0/dev_00_11_22_33_44_55";

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn ask_then_respond_yes() {
        runtime().block_on(async {
            let (b, mut rx) = Broker::new();
            let b2 = b.clone();
            let task = tokio::spawn(async move {
                b2.ask(
                    DEV,
                    AgentPrompt::Confirm { passkey: 123456 },
                    PROMPT_TIMEOUT,
                )
                .await
            });
            let AgentEvent::Request(req) = rx.recv().await.unwrap() else {
                panic!("expected request")
            };
            assert_eq!(req.device, DEV);
            assert_eq!(req.prompt, AgentPrompt::Confirm { passkey: 123456 });
            assert!(b.respond(req.id, AgentReply::Yes));
            assert_eq!(task.await.unwrap(), Ok(AgentReply::Yes));
            // Second answer to the same id is a no-op.
            assert!(!b.respond(req.id, AgentReply::No));
        });
    }

    #[test]
    fn no_maps_to_rejected_and_cancel_to_canceled() {
        assert!(matches!(
            yes_or_reject(Ok(AgentReply::No)),
            Err(AgentError::Rejected(_))
        ));
        assert!(matches!(
            yes_or_reject(Err(AskError::Canceled)),
            Err(AgentError::Canceled(_))
        ));
        assert!(matches!(
            yes_or_reject(Err(AskError::Timeout)),
            Err(AgentError::Canceled(_))
        ));
        assert!(yes_or_reject(Ok(AgentReply::Yes)).is_ok());
        let name = zbus::DBusError::name(&AgentError::Rejected("x".into())).to_string();
        assert_eq!(name, "org.bluez.Error.Rejected");
        let name = zbus::DBusError::name(&AgentError::Canceled("x".into())).to_string();
        assert_eq!(name, "org.bluez.Error.Canceled");
    }

    #[test]
    fn timeout_dismisses_dialog() {
        runtime().block_on(async {
            let (b, mut rx) = Broker::new();
            let r = b
                .ask(DEV, AgentPrompt::Authorize, Duration::from_millis(20))
                .await;
            assert_eq!(r, Err(AskError::Timeout));
            let AgentEvent::Request(req) = rx.recv().await.unwrap() else {
                panic!()
            };
            assert_eq!(rx.recv().await.unwrap(), AgentEvent::Dismiss(req.id));
            assert!(!b.respond(req.id, AgentReply::Yes));
        });
    }

    #[test]
    fn cancel_all_fails_pending_asks() {
        runtime().block_on(async {
            let (b, mut rx) = Broker::new();
            let b2 = b.clone();
            let task =
                tokio::spawn(
                    async move { b2.ask(DEV, AgentPrompt::Passkey, PROMPT_TIMEOUT).await },
                );
            let AgentEvent::Request(_) = rx.recv().await.unwrap() else {
                panic!()
            };
            b.cancel_all();
            assert_eq!(task.await.unwrap(), Err(AskError::Canceled));
            assert_eq!(rx.recv().await.unwrap(), AgentEvent::DismissAll);
        });
    }

    #[test]
    fn display_prompts_need_no_reply() {
        let (b, mut rx) = Broker::new();
        let id = b.show(
            DEV,
            AgentPrompt::DisplayPasskey {
                passkey: 42,
                entered: 0,
            },
        );
        match rx.try_recv().unwrap() {
            AgentEvent::Request(r) => {
                assert_eq!(r.id, id);
                assert!(r.prompt.is_display());
            }
            other => panic!("{other:?}"),
        }
        assert!(!b.respond(id, AgentReply::Yes));
    }

    #[test]
    fn input_parsing() {
        assert_eq!(parse_passkey(" 012345 "), Some(12345));
        assert_eq!(parse_passkey("999999"), Some(999999));
        assert_eq!(parse_passkey("1234567"), None);
        assert_eq!(parse_passkey("12a4"), None);
        assert_eq!(parse_passkey(""), None);
        assert_eq!(parse_pin("0000"), Some("0000".into()));
        assert_eq!(parse_pin(""), None);
        assert_eq!(parse_pin("12345678901234567"), None);
        assert_eq!(format_passkey(42), "000042");
    }

    #[test]
    fn pairing_set_tracks_initiated_devices() {
        let p = "/org/bluez/hci0/dev_TEST_PAIRING_SET";
        assert!(!is_pairing(p));
        begin_pairing(p);
        assert!(is_pairing(p));
        end_pairing(p);
        assert!(!is_pairing(p));
    }
}
