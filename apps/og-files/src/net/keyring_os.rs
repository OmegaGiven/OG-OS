//! Remembered passwords on macOS (login Keychain) and Windows (Credential
//! Manager) through the `keyring` crate. Service "og-files", account = the
//! connection id. Linux uses the Secret Service code in `keyring.rs`.

const SERVICE: &str = "og-files";

/// What the mount code needs; faked in tests.
pub(crate) trait Keyring {
    async fn lookup(&self, id: &str) -> Result<Option<String>, String>;
    async fn store(&self, id: &str, label: &str, password: &str) -> Result<(), String>;
}

/// The OS credential store.
pub(crate) struct Store;

impl Keyring for Store {
    async fn lookup(&self, id: &str) -> Result<Option<String>, String> {
        lookup(id).await
    }
    async fn store(&self, id: &str, label: &str, password: &str) -> Result<(), String> {
        store(id, label, password).await
    }
}

fn entry(id: &str) -> Result<::keyring::Entry, String> {
    ::keyring::Entry::new(SERVICE, id).map_err(|e| format!("Keyring: {e}"))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

pub(crate) async fn has(id: &str) -> Result<bool, String> {
    Ok(lookup(id).await?.is_some())
}

pub(crate) async fn lookup(id: &str) -> Result<Option<String>, String> {
    let id = id.to_string();
    blocking(move || match entry(&id)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(::keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Keyring: {e}")),
    })
    .await
}

pub(crate) async fn store(id: &str, _label: &str, password: &str) -> Result<(), String> {
    let (id, pw) = (id.to_string(), password.to_string());
    blocking(move || {
        entry(&id)?
            .set_password(&pw)
            .map_err(|e| format!("Keyring: {e}"))
    })
    .await
}

pub(crate) async fn forget(id: &str) -> Result<(), String> {
    let id = id.to_string();
    blocking(move || match entry(&id)?.delete_credential() {
        Ok(()) | Err(::keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Keyring: {e}")),
    })
    .await
}
