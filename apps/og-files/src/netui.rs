//! The "Connect to server" form (new or edit) and the password prompt used
//! when mounting a connection whose password isn't in the keyring.

use crate::net::{self, Auth, Conn, Proto};
use crate::theme;
use iced::widget::{button, checkbox, column, pick_list, row, text, text_input, Space};
use iced::{Element, Length};
use std::path::PathBuf;

pub const URL_ID: &str = "conn-url";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthKind {
    Agent,
    KeyFile,
    Password,
    Anonymous,
}

impl AuthKind {
    fn all(proto: Proto) -> Vec<AuthKind> {
        match proto {
            Proto::Sftp => vec![AuthKind::Agent, AuthKind::KeyFile, AuthKind::Password],
            _ => vec![AuthKind::Password, AuthKind::Anonymous],
        }
    }
}

impl std::fmt::Display for AuthKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            AuthKind::Agent => "SSH keys / agent",
            AuthKind::KeyFile => "Key file",
            AuthKind::Password => "Password",
            AuthKind::Anonymous => "Guest / anonymous",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Form {
    /// Some(id) when editing a saved connection
    pub editing: Option<String>,
    pub url: String,
    pub proto: Proto,
    pub name: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub path: String,
    pub auth: AuthKind,
    pub key_file: String,
    pub password: String,
    pub remember: bool,
    pub auto_mount: bool,
    pub tls: bool,
    pub status: Option<Result<String, String>>,
    pub busy: bool,
}

#[derive(Debug, Clone)]
pub enum FormMsg {
    Url(String),
    Proto(Proto),
    Name(String),
    Host(String),
    Port(String),
    User(String),
    Path(String),
    Auth(AuthKind),
    KeyFile(String),
    Password(String),
    Remember(bool),
    AutoMount(bool),
    Tls(bool),
}

pub enum Action {
    Test,
    Save,
    Cancel,
}

impl Form {
    pub fn new() -> Self {
        Self::from_conn(
            &Conn {
                id: String::new(),
                name: String::new(),
                proto: Proto::Sftp,
                host: String::new(),
                port: None,
                user: std::env::var("USER").unwrap_or_default(),
                path: String::new(),
                auth: Auth::Agent,
                tls: false,
                auto_mount: false,
            },
            None,
        )
    }

    pub fn from_conn(c: &Conn, editing: Option<String>) -> Self {
        let (auth, key_file) = match &c.auth {
            Auth::Agent => (AuthKind::Agent, String::new()),
            Auth::KeyFile(p) => (AuthKind::KeyFile, p.display().to_string()),
            Auth::Password => (AuthKind::Password, String::new()),
            Auth::Anonymous => (AuthKind::Anonymous, String::new()),
        };
        Self {
            editing,
            url: if c.host.is_empty() {
                String::new()
            } else {
                net::to_url(c)
            },
            proto: c.proto,
            name: c.name.clone(),
            host: c.host.clone(),
            port: c.port.map(|p| p.to_string()).unwrap_or_default(),
            user: c.user.clone(),
            path: c.path.clone(),
            auth,
            key_file,
            password: String::new(),
            remember: true,
            auto_mount: c.auto_mount,
            tls: c.tls,
            status: None,
            busy: false,
        }
    }

    pub fn update(&mut self, m: FormMsg) {
        self.status = None;
        match m {
            FormMsg::Url(u) => {
                // a pasted URL fills in the fields below
                if let Ok(c) = net::parse_url(&u) {
                    let keep_name = self.name.clone();
                    let editing = self.editing.clone();
                    *self = Self::from_conn(&c, editing);
                    if !keep_name.is_empty() {
                        self.name = keep_name;
                    }
                }
                self.url = u;
            }
            FormMsg::Proto(p) => {
                self.proto = p;
                if !AuthKind::all(p).contains(&self.auth) {
                    self.auth = AuthKind::all(p)[0];
                }
            }
            FormMsg::Name(v) => self.name = v,
            FormMsg::Host(v) => self.host = v,
            FormMsg::Port(v) => self.port = v.chars().filter(|c| c.is_ascii_digit()).collect(),
            FormMsg::User(v) => self.user = v,
            FormMsg::Path(v) => self.path = v,
            FormMsg::Auth(a) => self.auth = a,
            FormMsg::KeyFile(v) => self.key_file = v,
            FormMsg::Password(v) => self.password = v,
            FormMsg::Remember(b) => self.remember = b,
            FormMsg::AutoMount(b) => self.auto_mount = b,
            FormMsg::Tls(b) => self.tls = b,
        }
    }

    /// The connection described by the form (validated).
    pub fn conn(&self) -> Result<Conn, String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("enter a server name or address".into());
        }
        let port = match self.port.trim() {
            "" => None,
            p => Some(
                p.parse::<u16>()
                    .map_err(|_| "port must be a number".to_string())?,
            ),
        };
        if self.proto == Proto::Smb && self.path.trim().trim_matches('/').is_empty() {
            return Err("SMB needs a share name in Folder / share (e.g. media)".into());
        }
        let auth = match self.auth {
            AuthKind::Agent => Auth::Agent,
            AuthKind::KeyFile => Auth::KeyFile(crate::config::expand_home(self.key_file.trim())),
            AuthKind::Password => Auth::Password,
            AuthKind::Anonymous => Auth::Anonymous,
        };
        let name = if self.name.trim().is_empty() {
            let tail = self
                .path
                .trim_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("")
                .to_string();
            if tail.is_empty() {
                host.to_string()
            } else {
                format!("{host} — {tail}")
            }
        } else {
            self.name.trim().to_string()
        };
        Ok(Conn {
            id: self.editing.clone().unwrap_or_default(),
            name,
            proto: self.proto,
            host: host.to_string(),
            port,
            user: self.user.trim().to_string(),
            path: self.path.trim().to_string(),
            auth,
            tls: self.tls,
            auto_mount: self.auto_mount,
        })
    }

    pub fn password(&self) -> Option<String> {
        (self.auth == AuthKind::Password && !self.password.is_empty())
            .then(|| self.password.clone())
    }

    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        backend_err: Option<&'a str>,
        on: impl Fn(FormMsg) -> M + Copy + 'a,
        act: impl Fn(Action) -> M + 'a,
    ) -> Element<'a, M> {
        let field = |label: &'static str,
                     value: &'a str,
                     ph: &'static str,
                     f: fn(String) -> FormMsg|
         -> Element<'a, M> {
            column![
                text(label).size(12).color(theme::MUTED),
                text_input(ph, value).on_input(move |v| on(f(v))).size(13),
            ]
            .spacing(2)
            .into()
        };
        let mut c = column![
            text(if self.editing.is_some() {
                "Edit connection"
            } else {
                "Connect to server"
            })
            .size(16),
            column![
                text("Paste an address (optional)")
                    .size(12)
                    .color(theme::MUTED),
                text_input(
                    "sftp://you@station/home/you · smb://nas/media · davs://cloud.example/dav",
                    &self.url
                )
                .id(text_input::Id::new(URL_ID))
                .on_input(move |v| on(FormMsg::Url(v)))
                .size(13),
            ]
            .spacing(2),
            column![
                text("Type").size(12).color(theme::MUTED),
                pick_list(Proto::ALL.to_vec(), Some(self.proto), move |p| on(
                    FormMsg::Proto(p)
                ))
                .text_size(13),
            ]
            .spacing(2),
            row![
                field(
                    "Server",
                    &self.host,
                    "station · 192.168.1.20",
                    FormMsg::Host
                ),
                column![
                    text("Port").size(12).color(theme::MUTED),
                    text_input(
                        match self.proto {
                            Proto::Sftp => "22",
                            Proto::Smb => "445",
                            Proto::WebDav => "443",
                            Proto::Ftp => "21",
                        },
                        &self.port
                    )
                    .on_input(move |v| on(FormMsg::Port(v)))
                    .size(13)
                    .width(80),
                ]
                .spacing(2),
            ]
            .spacing(8),
            field(
                if self.proto == Proto::Smb {
                    "Share / folder"
                } else {
                    "Folder"
                },
                &self.path,
                if self.proto == Proto::Smb {
                    "media/Movies"
                } else {
                    "/home/you (empty = home)"
                },
                FormMsg::Path
            ),
            row![
                field("User", &self.user, "you", FormMsg::User),
                column![
                    text("Sign in with").size(12).color(theme::MUTED),
                    pick_list(AuthKind::all(self.proto), Some(self.auth), move |a| on(
                        FormMsg::Auth(a)
                    ))
                    .text_size(13),
                ]
                .spacing(2),
            ]
            .spacing(8),
        ]
        .spacing(10);
        match self.auth {
            AuthKind::KeyFile => {
                c = c.push(field(
                    "Key file",
                    &self.key_file,
                    "~/.ssh/id_ed25519",
                    FormMsg::KeyFile,
                ))
            }
            AuthKind::Password => {
                c = c.push(
                    column![
                        text("Password (leave empty to use a remembered one)")
                            .size(12)
                            .color(theme::MUTED),
                        text_input("", &self.password)
                            .secure(true)
                            .on_input(move |v| on(FormMsg::Password(v)))
                            .size(13),
                    ]
                    .spacing(2),
                );
                c = c.push(
                    checkbox("Remember password in the keyring", self.remember)
                        .on_toggle(move |b| on(FormMsg::Remember(b)))
                        .size(15)
                        .text_size(13),
                );
            }
            _ => {}
        }
        if self.proto == Proto::WebDav {
            c = c.push(
                checkbox("Use https", self.tls)
                    .on_toggle(move |b| on(FormMsg::Tls(b)))
                    .size(15)
                    .text_size(13),
            );
        }
        c = c.push(field(
            "Name in the sidebar (optional)",
            &self.name,
            "station home",
            FormMsg::Name,
        ));
        c = c.push(
            checkbox("Connect when og-files starts", self.auto_mount)
                .on_toggle(move |b| on(FormMsg::AutoMount(b)))
                .size(15)
                .text_size(13),
        );
        if let Some(e) = backend_err {
            c = c.push(
                text(e)
                    .size(12)
                    .color(iced::Color::from_rgb8(0xff, 0xc8, 0x6b)),
            );
        }
        if let Some(s) = &self.status {
            let (msg, color) = match s {
                Ok(m) => (format!("✓ {m}"), iced::Color::from_rgb8(0x7f, 0xcf, 0x8f)),
                Err(e) => (e.clone(), iced::Color::from_rgb8(0xff, 0x7a, 0x7a)),
            };
            c = c.push(text(msg).size(12).color(color));
        }
        let busy = self.busy;
        c.push(
            row![
                button(text("Cancel"))
                    .on_press(act(Action::Cancel))
                    .style(button::secondary),
                Space::with_width(Length::Fill),
                button(text(if busy { "Working…" } else { "Test" }))
                    .on_press_maybe((!busy).then(|| act(Action::Test)))
                    .style(button::secondary),
                button(text(if self.editing.is_some() {
                    "Save"
                } else {
                    "Connect"
                }))
                .on_press_maybe((!busy).then(|| act(Action::Save))),
            ]
            .spacing(8),
        )
        .into()
    }
}

/// Mount point label for status messages.
pub fn short(p: &PathBuf) -> String {
    p.display()
        .to_string()
        .replace(&crate::config::home().display().to_string(), "~")
}
