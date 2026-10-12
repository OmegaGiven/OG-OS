//! Network storage: saved connections, discovery (tailnet peers via the
//! Tailscale LocalAPI, LAN services via Avahi, ~/.ssh/config hosts) and
//! user-level mounts through rclone + FUSE, so remote storage appears as a
//! normal folder (`~/Network/<name>`) that every viewer and job can use.
//! CONTRACT STUB — implement bodies; keep the pub interface.
#![allow(unused_variables, dead_code)]

use iced::futures::Stream;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const TODO: &str = "not implemented yet";

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
    /// advertised on the LAN via mDNS / DNS-SD
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
            auth: if self.proto == Proto::Sftp { Auth::Agent } else { Auth::Password },
            tls: false,
            auto_mount: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LanEvent {
    Found(Discovered),
    Lost { name: String, proto: Proto },
}

/// rclone + fusermount3 present? Err explains what to install.
pub fn backend_available() -> Result<(), String> {
    Err(TODO.into())
}

/// Folder that holds the mounts (`~/Network`).
pub fn mount_root() -> PathBuf {
    crate::config::home().join("Network")
}

pub fn load_saved() -> Vec<Conn> {
    Vec::new()
}

/// Write all saved connections (atomic). Assigns ids to new ones.
pub fn save_all(conns: &[Conn]) -> Result<Vec<Conn>, String> {
    Err(TODO.into())
}

/// `sftp://user@host:port/path`, `ssh://…`, `smb://[user@]host/share/dir`,
/// `dav://` / `davs://` / `webdav://host/path`, `ftp://…`; a bare `host` or
/// `user@host` means SFTP.
pub fn parse_url(s: &str) -> Result<Conn, String> {
    Err(TODO.into())
}

/// The URL form of a connection (for display / copy).
pub fn to_url(c: &Conn) -> String {
    String::new()
}

/// Tailnet peers (Tailscale LocalAPI over its unix socket), as SFTP
/// candidates. Err if tailscaled isn't running.
pub async fn discover_tailnet() -> Result<Vec<Discovered>, String> {
    Err(TODO.into())
}

/// `Host` entries from ~/.ssh/config (no wildcards).
pub fn ssh_config_hosts() -> Vec<Discovered> {
    Vec::new()
}

/// LAN services via Avahi (D-Bus): _sftp-ssh/_ssh, _smb, _webdav(s), _ftp.
/// Runs until dropped; ends if Avahi isn't available.
pub fn discover_lan() -> impl Stream<Item = LanEvent> + Send + 'static {
    iced::futures::stream::empty()
}

/// Can we log in and list the remote folder? Ok = short description.
pub async fn test(conn: Conn, password: Option<String>) -> Result<String, String> {
    Err(TODO.into())
}

/// Mount under `mount_root()/<name>`; returns the local folder. `password`
/// None = use the keyring (or fail asking for one); `remember` stores it in
/// the keyring. The mount outlives og-files.
pub async fn mount(conn: Conn, password: Option<String>, remember: bool) -> Result<PathBuf, String> {
    Err(TODO.into())
}

pub async fn unmount(conn: Conn) -> Result<(), String> {
    Err(TODO.into())
}

/// Currently mounted connections: (conn id, local folder).
pub fn mounted() -> Vec<(String, PathBuf)> {
    Vec::new()
}

/// Is a password remembered for this connection?
pub async fn has_saved_password(conn: Conn) -> bool {
    false
}

pub async fn forget_password(conn: Conn) -> Result<(), String> {
    Err(TODO.into())
}
