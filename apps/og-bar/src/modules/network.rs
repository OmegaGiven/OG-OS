//! Reads link state straight out of /sys/class/net rather than shelling to
//! nmcli/iwctl — those manage connections, this module only needs to know
//! "up, and is it wifi or ethernet", which sysfs already has for free.
//! Re-read only when the kernel reports a link change over rtnetlink (see
//! `network_stream`), so an unchanged network costs nothing.

use iced::widget::{container, mouse_area, text};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

pub fn is_wifi_powered() -> bool {
    let out = std::process::Command::new("rfkill")
        .args(["list", "wifi"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .unwrap_or_default();
    !out.contains("Soft blocked: yes") && !out.is_empty()
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    // No SSID: only the icon is shown, and looking it up meant spawning
    // `iw` on every refresh.
    Wifi,
    Ethernet,
    Disconnected,
}

pub fn detect_link() -> LinkState {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return LinkState::Disconnected;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        // Bridges/veth pairs/tailscale/docker0 are all "up" just as often as
        // the real NIC and sort before it as often as not — the `device`
        // symlink only exists for interfaces backed by real hardware, which
        // is the only thing this module should ever report on.
        if !entry.path().join("device").exists() {
            continue;
        }
        let operstate_path = entry.path().join("operstate");
        let Ok(state) = std::fs::read_to_string(&operstate_path) else { continue };
        if state.trim() != "up" {
            continue;
        }
        if entry.path().join("wireless").exists() {
            return LinkState::Wifi;
        }
        return LinkState::Ethernet;
    }
    LinkState::Disconnected
}

pub struct Network {
    state: LinkState,
    hovered: bool,
}

impl Network {
    pub fn new() -> Self {
        Self { state: LinkState::Disconnected, hovered: false }
    }
}

impl Module for Network {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        let hovered = self.hovered;
        let icon = match &self.state {
            LinkState::Wifi => "\u{f05a9}",
            LinkState::Ethernet => "\u{f0200}",
            LinkState::Disconnected => "\u{f05aa}",
        };
        mouse_area(
            container(
                container(text(icon).size(16).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(fg) }))
                    .width(size as u16)
                    .height(size as u16)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            )
            .style(move |_| container::Style {
                background: Some(Background::Color(if hovered { colors.header_btn_bg } else { Color::TRANSPARENT })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            }),
        )
        .on_press(Message::Launch("og-settings --tab network".to_string()))
        .on_right_press(Message::WifiTogglePower)
        .on_enter(Message::NetworkHover(true))
        .on_exit(Message::NetworkHover(false))
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with_id("network", network_stream())
    }

    fn update(&mut self, message: &Message) {
        if let Message::NetworkLink(state) = message {
            self.state = state.clone();
        }
        if let Message::NetworkHover(v) = message {
            self.hovered = *v;
        }
    }
}

/// A NETLINK_ROUTE socket subscribed to RTMGRP_LINK: the kernel pushes a
/// message whenever any interface is added/removed or changes state
/// (cable, wifi association, rfkill, suspend/resume).
fn link_event_socket() -> Option<std::os::fd::OwnedFd> {
    use std::os::fd::{FromRawFd, OwnedFd};
    // SAFETY: plain socket/bind syscalls; the fd is owned immediately and
    // `addr` is a zeroed sockaddr_nl with only family/groups set.
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            libc::NETLINK_ROUTE,
        );
        if fd < 0 {
            return None;
        }
        let fd = OwnedFd::from_raw_fd(fd);
        let mut addr: libc::sockaddr_nl = std::mem::zeroed();
        addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        addr.nl_groups = libc::RTMGRP_LINK as u32;
        let bound = libc::bind(
            std::os::fd::AsRawFd::as_raw_fd(&fd),
            &addr as *const libc::sockaddr_nl as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        );
        (bound == 0).then_some(fd)
    }
}

/// Discards everything queued on the socket. The messages' contents don't
/// matter — any link event just means "re-read sysfs" — and an overrun
/// (ENOBUFS) means the same thing.
fn drain(fd: &std::os::fd::OwnedFd) {
    let mut buf = [0u8; 8192];
    loop {
        // SAFETY: recv into a local buffer of the stated length.
        let n = unsafe {
            libc::recv(
                std::os::fd::AsRawFd::as_raw_fd(fd),
                buf.as_mut_ptr().cast(),
                buf.len(),
                libc::MSG_DONTWAIT,
            )
        };
        let overrun = n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOBUFS);
        if n <= 0 && !overrun {
            return;
        }
    }
}

fn network_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(1, |mut sender| async move {
        use iced::futures::SinkExt;

        let socket = link_event_socket().and_then(|fd| tokio::io::unix::AsyncFd::new(fd).ok());
        let mut last = None;
        loop {
            if let Ok(state) = tokio::task::spawn_blocking(detect_link).await {
                if last.as_ref() != Some(&state) {
                    last = Some(state.clone());
                    if sender.send(Message::NetworkLink(state)).await.is_err() {
                        return;
                    }
                }
            }
            match &socket {
                Some(socket) => {
                    let Ok(mut guard) = socket.readable().await else { return };
                    // Link changes arrive in bursts (an interface going up
                    // emits several); settle briefly, then take them all.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    drain(socket.get_ref());
                    guard.clear_ready();
                }
                // No netlink (shouldn't happen on Linux): fall back to polling.
                None => tokio::time::sleep(std::time::Duration::from_secs(10)).await,
            }
        }
    })
}
