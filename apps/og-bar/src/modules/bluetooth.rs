use iced::widget::{button, column, container, mouse_area, text};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

/// Right-click menu actions — same set waybar's old `bluetooth-menu.xml`
/// offered. "Devices…"/"Adapter settings…"/"Send files…" shell out to
/// blueman rather than og-settings' own (fuller) bluetooth UI, since
/// that's literally what the old menu ran and this is a like-for-like
/// replacement, not a redesign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothAction {
    PowerOn,
    PowerOff,
    Devices,
    Adapters,
    SendFiles,
}

impl BluetoothAction {
    pub fn label(&self) -> &'static str {
        match self {
            BluetoothAction::PowerOn => "Turn adapter on",
            BluetoothAction::PowerOff => "Turn adapter off",
            BluetoothAction::Devices => "Devices…",
            BluetoothAction::Adapters => "Adapter settings…",
            BluetoothAction::SendFiles => "Send files…",
        }
    }

    /// Power on/off go over D-Bus asynchronously (bar.rs turns them into
    /// `set_powered` tasks); only the blueman launchers run here.
    pub fn run(&self) {
        match self {
            BluetoothAction::PowerOn | BluetoothAction::PowerOff => {}
            BluetoothAction::Devices => {
                let _ = std::process::Command::new("blueman-manager").spawn();
            }
            BluetoothAction::Adapters => {
                let _ = std::process::Command::new("blueman-adapters").spawn();
            }
            BluetoothAction::SendFiles => {
                let _ = std::process::Command::new("blueman-sendto").spawn();
            }
        }
    }
}

const BLUEZ: &str = "org.bluez";
const ADAPTER_IFACE: &str = "org.bluez.Adapter1";

/// One system-bus connection shared by the event stream and the toggles.
async fn system_bus() -> Option<&'static zbus::Connection> {
    static BUS: tokio::sync::OnceCell<zbus::Connection> = tokio::sync::OnceCell::const_new();
    BUS.get_or_try_init(zbus::Connection::system).await.ok()
}

/// Object path and `Powered` state of the first BlueZ adapter, from a
/// single ObjectManager call — no `bluetoothctl` process.
async fn adapter(conn: &zbus::Connection) -> Option<(zbus::zvariant::OwnedObjectPath, bool)> {
    let manager = zbus::fdo::ObjectManagerProxy::builder(conn)
        .destination(BLUEZ)
        .ok()?
        .path("/")
        .ok()?
        .build()
        .await
        .ok()?;
    let objects = manager.get_managed_objects().await.ok()?;
    objects.into_iter().find_map(|(path, interfaces)| {
        let (_, props) = interfaces.into_iter().find(|(name, _)| name.as_str() == ADAPTER_IFACE)?;
        let powered = props.get("Powered").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        Some((path, powered))
    })
}

pub async fn set_powered(on: bool) {
    let Some(conn) = system_bus().await else { return };
    let Some((path, _)) = adapter(conn).await else { return };
    let Ok(builder) = zbus::fdo::PropertiesProxy::builder(conn).destination(BLUEZ).and_then(|b| b.path(path)) else {
        return;
    };
    if let Ok(props) = builder.build().await {
        let iface = zbus::names::InterfaceName::from_static_str_unchecked(ADAPTER_IFACE);
        let _ = props.set(iface, "Powered", on.into()).await;
    }
}

pub async fn toggle_powered() {
    let Some(conn) = system_bus().await else { return };
    if let Some((_, powered)) = adapter(conn).await {
        set_powered(!powered).await;
    }
}

/// True for signals that can change what this module shows: the adapter's
/// own properties, an adapter (not a device) appearing/disappearing, or
/// bluetoothd itself restarting. Device churn during a scan is ignored.
fn affects_adapter(msg: &zbus::Message) -> bool {
    let header = msg.header();
    match header.member().map(|m| m.as_str()) {
        Some("InterfacesAdded" | "InterfacesRemoved") => msg
            .body()
            .deserialize::<(zbus::zvariant::ObjectPath<'_>, zbus::zvariant::Value<'_>)>()
            .map(|(path, _)| !path.as_str().contains("/dev_"))
            .unwrap_or(true),
        _ => true,
    }
}

/// Event-driven: BlueZ signals over the system bus instead of polling
/// `bluetoothctl show`. The adapter is re-read only when one of the
/// matched signals says something relevant changed.
fn bluetooth_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(4, |mut sender| async move {
        use iced::futures::{SinkExt, StreamExt};
        use zbus::message::Type;
        use zbus::{MatchRule, MessageStream};

        let Some(conn) = system_bus().await else { return };

        let rules = [
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(BLUEZ)
                .and_then(|b| b.interface("org.freedesktop.DBus.Properties"))
                .and_then(|b| b.member("PropertiesChanged"))
                .and_then(|b| b.arg(0, ADAPTER_IFACE))
                .map(|b| b.build()),
            MatchRule::builder()
                .msg_type(Type::Signal)
                .sender(BLUEZ)
                .and_then(|b| b.interface("org.freedesktop.DBus.ObjectManager"))
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
            if let Ok(stream) = MessageStream::for_match_rule(rule, conn, Some(16)).await {
                streams.push(stream);
            }
        }
        let mut events = iced::futures::stream::select_all(streams);

        let mut last = None;
        loop {
            let powered = adapter(conn).await.is_some_and(|(_, powered)| powered);
            if last != Some(powered) {
                last = Some(powered);
                if sender.send(Message::BluetoothPowered(powered)).await.is_err() {
                    return;
                }
            }
            loop {
                match events.next().await {
                    Some(Ok(msg)) if affects_adapter(&msg) => break,
                    Some(_) => continue,
                    None => return,
                }
            }
        }
    })
}

pub struct Bluetooth {
    powered: bool,
    hovered: bool,
}

impl Bluetooth {
    pub fn new() -> Self {
        Self { powered: false, hovered: false }
    }
}

impl Module for Bluetooth {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        let hovered = self.hovered;
        let icon = if self.powered { "\u{f00af}" } else { "\u{f00b2}" };
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
        .on_press(Message::BluetoothTogglePower)
        .on_right_press(Message::OpenBluetoothMenu)
        .on_enter(Message::BluetoothHover(true))
        .on_exit(Message::BluetoothHover(false))
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with_id("bluetooth", bluetooth_stream())
    }

    fn update(&mut self, message: &Message) {
        if let Message::BluetoothPowered(powered) = message {
            self.powered = *powered;
        }
        if let Message::BluetoothHover(v) = message {
            self.hovered = *v;
        }
    }
}

/// Right-click popup — same button-list styling as `power::popup_view`.
pub fn popup_view(colors: AppColors) -> Element<'static, Message> {
    let actions = [
        BluetoothAction::PowerOn,
        BluetoothAction::PowerOff,
        BluetoothAction::Devices,
        BluetoothAction::Adapters,
        BluetoothAction::SendFiles,
    ];

    let buttons = actions.into_iter().map(|action| {
        let fg = colors.text;
        button(text(action.label()).size(14).style(move |_| text::Style { color: Some(fg) }))
            .width(Length::Fill)
            .padding(8)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                text_color: fg,
                ..Default::default()
            })
            .on_press(Message::BluetoothAction(action))
            .into()
    });

    container(column(buttons).spacing(2).padding(6))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |_| container::Style {
            background: Some(colors.bg_fill),
            border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .into()
}
