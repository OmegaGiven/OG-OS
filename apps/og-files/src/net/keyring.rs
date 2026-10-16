//! Remembered passwords via the freedesktop Secret Service (KWallet's
//! compat daemon, GNOME Keyring, KeePassXC, …) on the session bus. Items
//! live in the default collection with attributes
//! `{application: "og-files", conn: <id>}`; secrets travel in a "plain"
//! session (the bus is local and per-user).

use std::collections::HashMap;
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Connection;

const SECRETS: &str = "org.freedesktop.secrets";
/// KWallet's Secret Service daemon (ksecretd) claims `SECRETS` once
/// started but isn't always registered as activatable under that name.
const KDE_COMPAT: &str = "org.kde.secretservicecompat";
const ROOT: &str = "/org/freedesktop/secrets";
const IF_SERVICE: &str = "org.freedesktop.Secret.Service";
const IF_ITEM: &str = "org.freedesktop.Secret.Item";
const IF_COLLECTION: &str = "org.freedesktop.Secret.Collection";
const IF_PROMPT: &str = "org.freedesktop.Secret.Prompt";
const IF_SESSION: &str = "org.freedesktop.Secret.Session";
const UNAVAILABLE: &str = "No keyring available (Secret Service not running)";

/// (session, parameters, value, content type)
type Secret = (OwnedObjectPath, Vec<u8>, Vec<u8>, String);

/// What the mount code needs; faked in tests.
pub(crate) trait Keyring {
    async fn lookup(&self, id: &str) -> Result<Option<String>, String>;
    async fn store(&self, id: &str, label: &str, password: &str) -> Result<(), String>;
}

pub(crate) struct SecretService;
/// The platform store (`keyring_os.rs` has the macOS / Windows one).
pub(crate) use SecretService as Store;

impl Keyring for SecretService {
    async fn lookup(&self, id: &str) -> Result<Option<String>, String> {
        lookup(id).await
    }
    async fn store(&self, id: &str, label: &str, password: &str) -> Result<(), String> {
        store(id, label, password).await
    }
}

fn err(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, detail, _) => {
            let name = name.as_str();
            if name.ends_with("ServiceUnknown") || name.ends_with("NameHasNoOwner") {
                UNAVAILABLE.into()
            } else if name.ends_with("IsLocked") {
                "The keyring is locked".into()
            } else {
                detail.unwrap_or_else(|| name.to_string())
            }
        }
        zbus::Error::InputOutput(_) => "Session bus unavailable".into(),
        e => format!("Keyring: {e}"),
    }
}

async fn bus() -> Result<&'static Connection, String> {
    static BUS: tokio::sync::OnceCell<Connection> = tokio::sync::OnceCell::const_new();
    BUS.get_or_try_init(Connection::session)
        .await
        .map_err(|_| "Session bus unavailable".to_string())
}

async fn dbus_call<B, R>(c: &Connection, method: &str, body: &B) -> Result<R, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let m = c
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            method,
            body,
        )
        .await
        .map_err(err)?;
    m.body().deserialize::<R>().map_err(|e| e.to_string())
}

async fn call<B, R>(
    c: &Connection,
    path: &str,
    iface: &str,
    method: &str,
    body: &B,
) -> Result<R, String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
    R: for<'d> zbus::zvariant::DynamicDeserialize<'d>,
{
    let m = c
        .call_method(Some(SECRETS), path, Some(iface), method, body)
        .await
        .map_err(err)?;
    m.body().deserialize::<R>().map_err(|e| e.to_string())
}

/// How the Secret Service is reachable (no side effects).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Availability {
    Running,
    Activatable,
    /// only via KWallet's compat daemon, started on first use
    ViaKdeCompat,
    Missing,
}

pub(crate) async fn availability(c: &Connection) -> Result<Availability, String> {
    if dbus_call::<_, bool>(c, "NameHasOwner", &(SECRETS,)).await? {
        return Ok(Availability::Running);
    }
    let names: Vec<String> = dbus_call(c, "ListActivatableNames", &()).await?;
    Ok(if names.iter().any(|n| n == SECRETS) {
        Availability::Activatable
    } else if names.iter().any(|n| n == KDE_COMPAT) {
        Availability::ViaKdeCompat
    } else {
        Availability::Missing
    })
}

/// Make sure someone owns `SECRETS`, starting KWallet's daemon if needed.
async fn ensure_service(c: &Connection) -> Result<(), String> {
    match availability(c).await? {
        Availability::Running | Availability::Activatable => Ok(()),
        Availability::Missing => Err(UNAVAILABLE.into()),
        Availability::ViaKdeCompat => {
            let _: u32 = dbus_call(c, "StartServiceByName", &(KDE_COMPAT, 0u32)).await?;
            for _ in 0..30 {
                if dbus_call::<_, bool>(c, "NameHasOwner", &(SECRETS,)).await? {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(UNAVAILABLE.into())
        }
    }
}

async fn connect() -> Result<&'static Connection, String> {
    let c = bus().await?;
    ensure_service(c).await?;
    Ok(c)
}

fn attrs(id: &str) -> HashMap<&str, &str> {
    HashMap::from([("application", "og-files"), ("conn", id)])
}

/// (unlocked, locked) items for this connection.
async fn search(
    c: &Connection,
    id: &str,
) -> Result<(Vec<OwnedObjectPath>, Vec<OwnedObjectPath>), String> {
    call(c, ROOT, IF_SERVICE, "SearchItems", &(attrs(id),)).await
}

async fn open_session(c: &Connection) -> Result<OwnedObjectPath, String> {
    let (_, session): (OwnedValue, OwnedObjectPath) = call(
        c,
        ROOT,
        IF_SERVICE,
        "OpenSession",
        &("plain", Value::from("")),
    )
    .await?;
    Ok(session)
}

async fn close_session(c: &Connection, session: &OwnedObjectPath) {
    let _ = call::<_, ()>(c, session.as_str(), IF_SESSION, "Close", &()).await;
}

/// Run a prompt (unlock / confirm dialog shown by the keyring daemon) and
/// wait for it; Err if dismissed.
async fn prompt(c: &Connection, path: &OwnedObjectPath) -> Result<(), String> {
    if path.as_str() == "/" {
        return Ok(());
    }
    use iced::futures::StreamExt;
    use zbus::message::Type;
    let rule = zbus::MatchRule::builder()
        .msg_type(Type::Signal)
        .interface(IF_PROMPT)
        .and_then(|b| b.member("Completed"))
        .and_then(|b| b.path(path.as_str()))
        .map_err(|e| e.to_string())?
        .build();
    let mut stream = zbus::MessageStream::for_match_rule(rule, c, Some(4))
        .await
        .map_err(err)?;
    call::<_, ()>(c, path.as_str(), IF_PROMPT, "Prompt", &("",)).await?;
    let msg = tokio::time::timeout(Duration::from_secs(300), stream.next())
        .await
        .map_err(|_| "The keyring didn't answer".to_string())?
        .ok_or("The keyring went away")?
        .map_err(err)?;
    let (dismissed, _): (bool, OwnedValue) = msg.body().deserialize().map_err(|e| e.to_string())?;
    if dismissed {
        Err("Keyring unlock was cancelled".into())
    } else {
        Ok(())
    }
}

async fn unlock(c: &Connection, objects: &[OwnedObjectPath]) -> Result<(), String> {
    if objects.is_empty() {
        return Ok(());
    }
    let (_, p): (Vec<OwnedObjectPath>, OwnedObjectPath) =
        call(c, ROOT, IF_SERVICE, "Unlock", &(objects,)).await?;
    prompt(c, &p).await
}

pub(crate) async fn has(id: &str) -> Result<bool, String> {
    let c = connect().await?;
    let (a, b) = search(c, id).await?;
    Ok(!a.is_empty() || !b.is_empty())
}

pub(crate) async fn lookup(id: &str) -> Result<Option<String>, String> {
    let c = connect().await?;
    let (mut unlocked, locked) = search(c, id).await?;
    if unlocked.is_empty() && !locked.is_empty() {
        unlock(c, &locked[..1]).await?;
        unlocked = locked;
    }
    let Some(item) = unlocked.first() else {
        return Ok(None);
    };
    let session = open_session(c).await?;
    let res: Result<Secret, String> =
        call(c, item.as_str(), IF_ITEM, "GetSecret", &(&session,)).await;
    close_session(c, &session).await;
    let (_, _, value, _) = res?;
    String::from_utf8(value)
        .map(Some)
        .map_err(|_| "The remembered password isn't valid text".into())
}

pub(crate) async fn store(id: &str, label: &str, password: &str) -> Result<(), String> {
    let c = connect().await?;
    let coll: OwnedObjectPath = call(c, ROOT, IF_SERVICE, "ReadAlias", &("default",)).await?;
    if coll.as_str() == "/" {
        return Err("The keyring has no default collection".into());
    }
    unlock(c, std::slice::from_ref(&coll)).await?;
    let session = open_session(c).await?;
    let mut props: HashMap<&str, Value> = HashMap::new();
    props.insert("org.freedesktop.Secret.Item.Label", Value::from(label));
    props.insert(
        "org.freedesktop.Secret.Item.Attributes",
        Value::from(attrs(id)),
    );
    let secret: Secret = (
        session.clone(),
        Vec::new(),
        password.as_bytes().to_vec(),
        "text/plain; charset=utf8".into(),
    );
    let res: Result<(OwnedObjectPath, OwnedObjectPath), String> = call(
        c,
        coll.as_str(),
        IF_COLLECTION,
        "CreateItem",
        &(props, secret, true),
    )
    .await;
    close_session(c, &session).await;
    let (_, p) = res?;
    prompt(c, &p).await
}

pub(crate) async fn forget(id: &str) -> Result<(), String> {
    let c = connect().await?;
    let (unlocked, locked) = search(c, id).await?;
    unlock(c, &locked).await?;
    for item in unlocked.iter().chain(&locked) {
        let p: OwnedObjectPath = call(c, item.as_str(), IF_ITEM, "Delete", &()).await?;
        prompt(c, &p).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live, read-only: how is the Secret Service reachable? (Doesn't start
    /// any daemon or touch the keyring.)
    #[tokio::test]
    #[ignore]
    async fn net_keyring_live_availability() {
        match bus().await {
            Ok(c) => println!("LIVE keyring: {:?}", availability(c).await),
            Err(e) => println!("LIVE keyring: {e}"),
        }
    }
}
