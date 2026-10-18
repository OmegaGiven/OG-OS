//! Network storage: saved connections, discovery (tailnet peers via the
//! Tailscale LocalAPI, LAN services via Avahi, ~/.ssh/config hosts) and
//! user-level mounts through rclone + FUSE, so remote storage appears as a
//! normal folder (`~/Network/<name>`) that every viewer and job can use.
//!
//! Layout: `url` (parse/print), `store` (connections.json), `sshcfg`,
//! `tailnet`, `lan` (Avahi), `rclone` (mount/test/unmount), `keyring`
//! (Secret Service). Everything that touches the outside world goes through
//! a `Paths` value (and, for processes, a `Runner`) so tests can point it at
//! a scratch directory and capture commands instead of running them.

#[cfg(target_os = "linux")]
mod keyring;
#[cfg(not(target_os = "linux"))]
#[path = "keyring_os.rs"]
mod keyring;
#[cfg(target_os = "linux")]
mod lan;
mod rclone;
mod sshcfg;
mod store;
mod tailnet;
mod url;

use iced::futures::Stream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Proto {
    /// SSH file transfer (any machine you can `ssh` into)
    Sftp,
    /// Windows / NAS shares
    Smb,
    WebDav,
    Ftp,
}

impl Proto {
    pub const ALL: [Proto; 4] = [Proto::Sftp, Proto::Smb, Proto::WebDav, Proto::Ftp];

    pub fn default_port(self) -> u16 {
        match self {
            Proto::Sftp => 22,
            Proto::Smb => 445,
            Proto::WebDav => 443,
            Proto::Ftp => 21,
        }
    }
}

impl std::fmt::Display for Proto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Proto::Sftp => "SSH / SFTP",
            Proto::Smb => "SMB (Windows / NAS share)",
            Proto::WebDav => "WebDAV",
            Proto::Ftp => "FTP",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Auth {
    /// SSH agent and default keys (~/.ssh/id_*) — SFTP only
    Agent,
    KeyFile(PathBuf),
    /// password from the keyring if remembered, else asked at mount time
    Password,
    Anonymous,
}

/// A saved connection (`$XDG_CONFIG_HOME/og-files/connections.json`).
/// Never contains a password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conn {
    /// stable id, also the mount folder name source
    pub id: String,
    /// display name, e.g. "go — media"
    pub name: String,
    pub proto: Proto,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    /// remote folder (SFTP/WebDAV/FTP) or "share/sub/folder" (SMB)
    pub path: String,
    pub auth: Auth,
    /// WebDAV over https
    pub tls: bool,
    /// mount when og-files starts
    pub auto_mount: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// a Tailscale peer: online right now?, OS reported by tailscale
    Tailnet { online: bool, os: String },
    /// advertised on the LAN via mDNS / DNS-SD (Avahi: Linux only)
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Lan,
    /// a `Host` entry in ~/.ssh/config
    SshConfig,
}

/// Something you could connect to (not saved yet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub name: String,
    pub proto: Proto,
    pub host: String,
    pub port: Option<u16>,
    pub user: String,
    pub source: Source,
}

impl Discovered {
    /// A ready-to-save connection prefilled from this discovery.
    pub fn to_conn(&self) -> Conn {
        Conn {
            id: String::new(),
            name: self.name.clone(),
            proto: self.proto,
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            path: String::new(),
            auth: if self.proto == Proto::Sftp {
                Auth::Agent
            } else {
                Auth::Password
            },
            tls: false,
            auto_mount: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub enum LanEvent {
    Found(Discovered),
    Lost { name: String, proto: Proto },
}

/// Where everything lives. `Paths::real()` in the app; tests build one
/// under a scratch dir so nothing touches the user's XDG dirs or ~/Network.
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    /// holds connections.json
    pub config_dir: PathBuf,
    /// holds rclone-<id>.log
    pub state_dir: PathBuf,
    pub mount_root: PathBuf,
    /// ~/.ssh (config, keys)
    pub ssh_dir: PathBuf,
    /// /proc/mounts
    #[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
    pub proc_mounts: PathBuf,
}

impl Paths {
    pub fn real() -> Self {
        let home = crate::config::home();
        let state = crate::config::state_dir();
        Paths {
            config_dir: crate::config::config_dir(),
            state_dir: state,
            mount_root: mount_root(),
            ssh_dir: home.join(".ssh"),
            proc_mounts: "/proc/mounts".into(),
        }
    }
}

/// The local login name (default SFTP user).
pub(crate) fn local_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_default()
}

/// rclone + fusermount3 present? Err explains what to install.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn backend_available() -> Result<(), String> {
    let rclone = rclone::on_path("rclone");
    let fuse = rclone::on_path("fusermount3");
    match (rclone, fuse) {
        (true, true) => Ok(()),
        (false, true) => {
            Err("Install rclone to connect to network storage (pacman -S rclone)".into())
        }
        (true, false) => Err("Install FUSE 3 to mount network storage (pacman -S fuse3)".into()),
        (false, false) => Err(
            "Install rclone and FUSE 3 to connect to network storage (pacman -S rclone fuse3)"
                .into(),
        ),
    }
}

/// rclone + a FUSE layer (macFUSE / FUSE-T on macOS, WinFsp on Windows)
/// present? Err explains what to install.
#[cfg(any(target_os = "macos", windows))]
pub fn backend_available() -> Result<(), String> {
    use std::path::Path;
    let rclone = rclone::on_path("rclone");
    let fuse = if cfg!(target_os = "macos") {
        [
            "/Library/Filesystems/macfuse.fs",
            "/Library/Filesystems/osxfuse.fs",
            "/usr/local/lib/libfuse-t.dylib",
            "/opt/homebrew/lib/libfuse-t.dylib",
        ]
        .iter()
        .any(|p| Path::new(p).exists())
    } else {
        ["ProgramFiles(x86)", "ProgramFiles"]
            .iter()
            .any(|v| std::env::var_os(v).is_some_and(|d| Path::new(&d).join("WinFsp").is_dir()))
    };
    let (get_rclone, fuse_name, get_fuse) = if cfg!(target_os = "macos") {
        (
            "the official rclone binary from rclone.org (Homebrew's can't mount)",
            "macFUSE",
            "macFUSE (macfuse.github.io) or FUSE-T",
        )
    } else {
        (
            "rclone (winget install Rclone.Rclone)",
            "WinFsp",
            "WinFsp (winget install WinFsp.WinFsp)",
        )
    };
    match (rclone, fuse) {
        (true, true) => Ok(()),
        (false, true) => Err(format!(
            "Install {get_rclone} to connect to network storage"
        )),
        (true, false) => Err(format!("Install {get_fuse} to mount network storage")),
        (false, false) => Err(format!(
            "Install {get_rclone} and {fuse_name} to connect to network storage"
        )),
    }
}

/// Folder that holds the mounts (`~/Network`).
pub fn mount_root() -> PathBuf {
    crate::config::home().join("Network")
}

pub fn load_saved() -> Vec<Conn> {
    store::load(&Paths::real().config_dir)
}

/// Write all saved connections (atomic). Assigns ids to new ones.
pub fn save_all(conns: &[Conn]) -> Result<Vec<Conn>, String> {
    store::save(&Paths::real().config_dir, conns)
}

/// `sftp://user@host:port/path`, `ssh://…`, `smb://[user@]host/share/dir`,
/// `dav://` / `davs://` / `webdav://host/path`, `ftp://…`; a bare `host` or
/// `user@host` means SFTP.
pub fn parse_url(s: &str) -> Result<Conn, String> {
    url::parse(s, &local_user())
}

/// The URL form of a connection (for display / copy).
pub fn to_url(c: &Conn) -> String {
    url::print(c)
}

/// Tailnet peers (Tailscale LocalAPI over its unix socket; macOS /
/// Windows: `tailscale status --json`), as SFTP candidates. Err if
/// tailscaled isn't running.
pub async fn discover_tailnet() -> Result<Vec<Discovered>, String> {
    #[cfg(target_os = "linux")]
    return tailnet::discover(tailnet::SOCKET.into(), local_user()).await;
    #[cfg(not(target_os = "linux"))]
    return tailnet::discover_cli(local_user()).await;
}

/// `Host` entries from ~/.ssh/config (no wildcards).
pub fn ssh_config_hosts() -> Vec<Discovered> {
    sshcfg::discovered(&Paths::real().ssh_dir, &local_user())
}

/// LAN services via Avahi (D-Bus): _sftp-ssh/_ssh, _smb, _webdav(s), _ftp.
/// Runs until dropped; ends if Avahi isn't available (always, on macOS and
/// Windows: no Avahi there).
pub fn discover_lan() -> impl Stream<Item = LanEvent> + Send + 'static {
    #[cfg(target_os = "linux")]
    return lan::browse();
    #[cfg(not(target_os = "linux"))]
    return iced::futures::stream::empty();
}

/// Can we log in and list the remote folder? Ok = short description.
pub async fn test(conn: Conn, password: Option<String>) -> Result<String, String> {
    backend_available()?;
    rclone::test(
        &Paths::real(),
        &rclone::System,
        &keyring::Store,
        conn,
        password,
    )
    .await
}

/// Mount under `mount_root()/<name>`; returns the local folder. `password`
/// None = use the keyring (or fail asking for one); `remember` stores it in
/// the keyring. The mount outlives og-files.
pub async fn mount(
    conn: Conn,
    password: Option<String>,
    remember: bool,
) -> Result<PathBuf, String> {
    backend_available()?;
    rclone::mount(
        &Paths::real(),
        &rclone::System,
        &keyring::Store,
        conn,
        password,
        remember,
    )
    .await
}

pub async fn unmount(conn: Conn) -> Result<(), String> {
    rclone::unmount(&Paths::real(), &rclone::System, &conn).await
}

/// Currently mounted connections: (conn id, local folder).
pub fn mounted() -> Vec<(String, PathBuf)> {
    let paths = Paths::real();
    let saved = store::load(&paths.config_dir);
    rclone::mounted(&paths, &saved)
}

/// Is a password remembered for this connection?
pub async fn has_saved_password(conn: Conn) -> bool {
    keyring::has(&conn.id).await.unwrap_or(false)
}

pub async fn forget_password(conn: Conn) -> Result<(), String> {
    keyring::forget(&conn.id).await
}

#[cfg(test)]
pub(crate) mod testdir {
    //! Scratch dirs under `$HOME/.cache/ogx-net-tests` (never /tmp, never
    //! the real XDG dirs); removed on drop.
    use super::Paths;
    use std::path::{Path, PathBuf};

    pub struct TestDir(pub PathBuf);

    impl TestDir {
        pub fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("ogx-net-tests")
                .join(format!(
                    "{tag}-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
            let _ = std::fs::remove_dir_all(&dir);
            // Windows can briefly deny access (antivirus scans, pending
            // deletes) — retry before failing the test
            let mut tries = 0;
            while let Err(e) = std::fs::create_dir_all(&dir) {
                tries += 1;
                assert!(tries < 20, "create {}: {e}", dir.display());
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            TestDir(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }

        pub fn paths(&self) -> Paths {
            let d = &self.0;
            for sub in ["config", "state", "Network", "ssh"] {
                std::fs::create_dir_all(d.join(sub)).unwrap();
            }
            std::fs::write(d.join("mounts"), "").unwrap();
            Paths {
                config_dir: d.join("config"),
                state_dir: d.join("state"),
                mount_root: d.join("Network"),
                ssh_dir: d.join("ssh"),
                proc_mounts: d.join("mounts"),
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
            // drop the parent too once the last test is done with it
            if let Some(parent) = self.0.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}
