use iced::widget::{button, column, container, row, scrollable, text, text_input, toggler};
use iced::{Background, Border, Color, Element, Length};

use crate::app::{AppColors, Message};
use crate::sway::{BluetoothAdapter, BluetoothDevice, EthernetInterface, WifiNetwork, WifiStatus};
use crate::galias::Alias;
use crate::vpn::{SplitApp, TailscaleStatus, VpnState};

pub fn view<'a>(
    colors: AppColors,
    wifi_status: &'a WifiStatus,
    wifi_networks: &'a [WifiNetwork],
    ethernet_interfaces: &'a [EthernetInterface],
    bluetooth_adapter: &'a BluetoothAdapter,
    bluetooth_devices: &'a [BluetoothDevice],
    scanning: bool,
    tailscale: &'a TailscaleStatus,
    vpn_state: &'a VpnState,
    split_apps: &'a [SplitApp],
    vpn_add_open: bool,
    vpn_add_name: &'a str,
    vpn_add_conf_text: &'a str,
    galias_aliases: &'a [Alias],
    galias_key: &'a str,
    galias_url: &'a str,
) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let accent_btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.accent)),
        text_color: colors.bar_bg,
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    };
    let danger_btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
        text_color: Color::WHITE,
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    };

    let title = move |t: &'a str| -> Element<'a, Message> {
        text(t).size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let label = move |t: String| -> Element<'a, Message> {
        text(t).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let dim = move |t: String| -> Element<'a, Message> {
        text(t).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()
    };
    // Three-tier brightness since the shared theme has no dedicated
    // "warning" color: Connected gets the strongest (accent), Paired-but-
    // disconnected sits at full text brightness (distinct from both ends),
    // Not paired gets the dimmest.
    let bt_status_pill = move |status: &'static str| -> Element<'a, Message> {
        let color = match status {
            "Connected" => colors.accent,
            "Paired" => colors.text,
            _ => colors.dim_text,
        };
        container(text(status).size(11).style(move |_| iced::widget::text::Style { color: Some(color) }))
            .padding([2, 8])
            .style(move |_| container::Style {
                background: Some(Background::Color(color.scale_alpha(0.15))),
                border: Border { color, width: 1.0, radius: 999.0.into() },
                ..Default::default()
            })
            .into()
    };
    #[derive(Clone, Copy)]
    enum ActionKind { Plain, Accent, Danger }
    let action_btn = move |label: &'static str, msg: Message, kind: ActionKind| -> Element<'a, Message> {
        let text_color = match kind {
            ActionKind::Plain => colors.text,
            ActionKind::Accent => colors.bar_bg,
            ActionKind::Danger => Color::WHITE,
        };
        let btn = button(text(label).size(12).style(move |_| iced::widget::text::Style { color: Some(text_color) }))
            .on_press(msg)
            .padding([4, 10]);
        match kind {
            ActionKind::Plain => btn.style(btn_style),
            ActionKind::Accent => btn.style(accent_btn_style),
            ActionKind::Danger => btn.style(danger_btn_style),
        }
        .into()
    };

    // ── Wi-Fi ────────────────────────────────────────────────────────────
    let wifi_status_row: Element<Message> = if wifi_status.connected_ssid.is_some() {
        row![
            dim(format!(
                "Connected to {} {}",
                wifi_status.connected_ssid.as_deref().unwrap_or(""),
                wifi_status.ip.as_deref().map(|ip| format!("· {ip}")).unwrap_or_default(),
            )),
            iced::widget::horizontal_space(),
            action_btn("Disconnect", Message::WifiDisconnect, ActionKind::Plain),
        ]
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        dim("Not connected".to_string())
    };

    let scan_label: &'static str = if scanning { "Scanning…" } else { "Scan" };
    let wifi_scan_btn = button(text(scan_label).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
        .style(btn_style)
        .on_press_maybe(if scanning || !wifi_status.powered { None } else { Some(Message::WifiScanStart) })
        .padding([6, 14]);

    let mut wifi_rows: Vec<Element<Message>> = Vec::new();
    if wifi_status.powered {
        if wifi_networks.is_empty() {
            wifi_rows.push(dim("No networks found. Try Scan.".to_string()));
        } else {
            for net in wifi_networks {
                let is_connected = wifi_status.connected_ssid.as_deref() == Some(net.ssid.as_str());
                let bars = "●".repeat(net.signal_bars as usize) + &"○".repeat(4usize.saturating_sub(net.signal_bars as usize));
                let lock = if net.security.is_empty() || net.security.eq_ignore_ascii_case("open") { "" } else { " 🔒" };

                let action: Element<Message> = if is_connected {
                    action_btn("Disconnect", Message::WifiDisconnect, ActionKind::Plain)
                } else {
                    action_btn("Connect", Message::WifiConnect(net.ssid.clone()), ActionKind::Accent)
                };
                let forget: Element<Message> = if net.known {
                    action_btn("Forget", Message::WifiForget(net.ssid.clone()), ActionKind::Danger)
                } else {
                    iced::widget::Space::new(0, 0).into()
                };

                wifi_rows.push(
                    row![
                        text(bars).size(11).style(move |_| iced::widget::text::Style { color: Some(colors.accent) }).width(50),
                        column![
                            label(format!("{}{}", net.ssid, lock)),
                            dim(if is_connected { "Connected".to_string() } else { net.security.clone() }),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        forget,
                        action,
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(10)
                    .into()
                );
            }
        }
    }

    let wifi_card = container(
        column(
            std::iter::once(
                row![
                    title("Wi-Fi"),
                    iced::widget::horizontal_space(),
                    wifi_scan_btn,
                    toggler(wifi_status.powered).on_toggle(Message::WifiPowerToggled),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(12)
                .into()
            )
            .chain(std::iter::once(wifi_status_row))
            .chain(wifi_rows)
            .collect::<Vec<_>>()
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Ethernet ─────────────────────────────────────────────────────────
    let eth_rows: Vec<Element<Message>> = if ethernet_interfaces.is_empty() {
        vec![dim("No wired interfaces found.".to_string())]
    } else {
        ethernet_interfaces.iter().map(|e| {
            let status = if e.up && e.carrier { "Connected" } else if e.up { "Up (no cable)" } else { "Down" };
            let name = e.name.clone();
            row![
                column![
                    label(e.name.clone()),
                    dim(format!("{status}{}", e.ip.as_deref().map(|ip| format!(" · {ip}")).unwrap_or_default())),
                ]
                .spacing(2)
                .width(Length::Fill),
                toggler(e.up).on_toggle(move |v| Message::EthernetToggled(name.clone(), v)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .into()
        }).collect()
    };

    let eth_card = container(
        column(
            std::iter::once(title("Ethernet"))
                .chain(std::iter::once(
                    dim("Toggling opens a terminal asking for your sudo password.".to_string())
                ))
                .chain(eth_rows)
                .collect::<Vec<_>>()
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Bluetooth ────────────────────────────────────────────────────────
    let bt_scan_label: &'static str = if scanning { "Scanning…" } else { "Scan" };
    let bt_scan_btn = button(text(bt_scan_label).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
        .style(btn_style)
        .on_press_maybe(if scanning || !bluetooth_adapter.powered { None } else { Some(Message::BluetoothScanStart) })
        .padding([6, 14]);

    let mut bt_rows: Vec<Element<Message>> = Vec::new();
    if !bluetooth_adapter.present {
        bt_rows.push(dim("No Bluetooth adapter found.".to_string()));
    } else if bluetooth_adapter.powered {
        bt_rows.push(
            row![
                label("Discoverable".to_string()), iced::widget::horizontal_space(),
                toggler(bluetooth_adapter.discoverable).on_toggle(Message::BluetoothDiscoverableToggled),
            ]
            .align_y(iced::Alignment::Center).spacing(12).into()
        );
        bt_rows.push(
            row![
                label("Pairable".to_string()), iced::widget::horizontal_space(),
                toggler(bluetooth_adapter.pairable).on_toggle(Message::BluetoothPairableToggled),
            ]
            .align_y(iced::Alignment::Center).spacing(12).into()
        );

        if bluetooth_devices.is_empty() {
            bt_rows.push(dim("No devices found. Try Scan.".to_string()));
        } else {
            for d in bluetooth_devices {
                let status = if d.connected { "Connected" } else if d.paired { "Paired" } else { "Not paired" };
                let mac = d.mac.clone();

                let mut actions: Vec<Element<Message>> = Vec::new();
                if d.paired {
                    if d.connected {
                        actions.push(action_btn("Disconnect", Message::BluetoothDisconnect(mac.clone()), ActionKind::Plain));
                    } else {
                        actions.push(action_btn("Connect", Message::BluetoothConnect(mac.clone()), ActionKind::Accent));
                    }
                    actions.push(action_btn("Remove", Message::BluetoothRemove(mac.clone()), ActionKind::Danger));
                } else {
                    actions.push(action_btn("Pair", Message::BluetoothPair(mac.clone()), ActionKind::Accent));
                }

                bt_rows.push(
                    row![
                        column![
                            row![label(d.name.clone()), bt_status_pill(status)]
                                .align_y(iced::Alignment::Center)
                                .spacing(8),
                            dim(d.mac.clone()),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        row(actions).spacing(8),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(10)
                    .into()
                );
            }
        }
    }

    let bt_card = container(
        column(
            std::iter::once(
                row![
                    title("Bluetooth"),
                    iced::widget::horizontal_space(),
                    bt_scan_btn,
                    toggler(bluetooth_adapter.powered).on_toggle(Message::BluetoothPowerToggled),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(12)
                .into()
            )
            .chain(bt_rows)
            .collect::<Vec<_>>()
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Tailscale (private VPN) ─────────────────────────────────────────
    let mut ts_rows: Vec<Element<Message>> = Vec::new();
    if !tailscale.installed {
        ts_rows.push(dim("tailscale not found on this system.".to_string()));
    } else if !tailscale.running {
        ts_rows.push(dim("Tailscale is installed but not running (`tailscale up`).".to_string()));
    } else {
        ts_rows.push(
            row![
                label(tailscale.self_hostname.clone()),
                iced::widget::horizontal_space(),
                dim(tailscale.self_ip.clone().unwrap_or_default()),
            ]
            .align_y(iced::Alignment::Center)
            .into()
        );
        ts_rows.push(title("Exit node"));
        let none_active = !tailscale.peers.iter().any(|p| p.is_exit_node);
        ts_rows.push(
            row![
                label("None (direct connection)".to_string()),
                iced::widget::horizontal_space(),
                if none_active {
                    dim("Active".to_string())
                } else {
                    action_btn("Use this", Message::VpnExitNodeSelected(None), ActionKind::Accent)
                },
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into()
        );
        // `tailscale.peers` now holds every tailnet peer (the AI Context
        // tab needs the full list too), so this picker filters down to
        // exit-node-capable ones itself instead of relying on the list
        // already being pre-filtered.
        let exit_capable: Vec<&crate::vpn::TailscalePeer> =
            tailscale.peers.iter().filter(|p| p.exit_node_option).collect();
        if exit_capable.is_empty() {
            ts_rows.push(dim("No peers on this tailnet advertise as an exit node.".to_string()));
        }
        for p in exit_capable {
            let hostname = p.hostname.clone();
            ts_rows.push(
                row![
                    column![
                        label(p.hostname.clone()),
                        dim(format!("{} · {}", p.ip, if p.online { "online" } else { "offline" })),
                    ]
                    .spacing(2)
                    .width(Length::Fill),
                    if p.is_exit_node {
                        dim("Active".to_string())
                    } else {
                        action_btn("Use this", Message::VpnExitNodeSelected(Some(hostname)), ActionKind::Accent)
                    },
                ]
                .align_y(iced::Alignment::Center)
                .spacing(10)
                .into()
            );
        }
    }
    let ts_card = container(
        column(std::iter::once(title("Tailscale (private VPN)")).chain(ts_rows).collect::<Vec<_>>())
            .spacing(12)
            .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Public VPN (WireGuard) ───────────────────────────────────────────
    let mut vpn_rows: Vec<Element<Message>> = Vec::new();
    if !vpn_state.helper_installed {
        vpn_rows.push(dim("og-vpn-apply helper not installed yet — see scripts/og-vpn-sudoers.".to_string()));
    } else if vpn_state.configs.is_empty() {
        vpn_rows.push(dim("No WireGuard configs added yet.".to_string()));
    } else {
        for c in vpn_state.configs.iter() {
            let is_whole = c.whole_active;
            let is_partial = vpn_state.netns_active && vpn_state.netns_config.as_deref() == Some(c.name.as_str());
            let whole_name = c.name.clone();
            let partial_name = c.name.clone();
            let del_name = c.name.clone();
            vpn_rows.push(
                column![
                    row![
                        label(c.name.clone()),
                        iced::widget::horizontal_space(),
                        action_btn("Delete", Message::VpnDeleteConfig(del_name), ActionKind::Danger),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(10),
                    row![
                        action_btn(
                            if !is_whole && !is_partial { "● Off" } else { "Off" },
                            Message::VpnSetOff,
                            if !is_whole && !is_partial { ActionKind::Accent } else { ActionKind::Plain },
                        ),
                        action_btn(
                            if is_whole { "● Whole system" } else { "Whole system" },
                            Message::VpnSetWhole(whole_name),
                            if is_whole { ActionKind::Accent } else { ActionKind::Plain },
                        ),
                        action_btn(
                            if is_partial { "● Partial (per-app)" } else { "Partial (per-app)" },
                            Message::VpnSetPartial(partial_name),
                            if is_partial { ActionKind::Accent } else { ActionKind::Plain },
                        ),
                    ]
                    .spacing(8),
                ]
                .spacing(6)
                .into()
            );
        }
    }

    let add_toggle_btn = button(text(if vpn_add_open { "Cancel" } else { "Add WireGuard config" }).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
        .style(btn_style)
        .on_press(Message::VpnAddOpenToggled(!vpn_add_open))
        .padding([6, 14]);

    let mut vpn_children: Vec<Element<Message>> = vec![
        row![title("Public VPN (WireGuard)"), iced::widget::horizontal_space(), add_toggle_btn]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .into(),
    ];
    if vpn_add_open {
        vpn_children.push(
            column![
                text_input("Name (e.g. proton-us)", vpn_add_name)
                    .on_input(Message::VpnAddNameChanged)
                    .padding(8),
                text_input("Paste the .conf contents here", vpn_add_conf_text)
                    .on_input(Message::VpnAddConfTextChanged)
                    .padding(8),
                action_btn("Save config", Message::VpnAddConfigSubmit, ActionKind::Accent),
            ]
            .spacing(8)
            .into()
        );
    }
    vpn_children.extend(vpn_rows);
    let vpn_card = container(column(vpn_children).spacing(12).padding(20))
        .style(card_style)
        .width(Length::Fill);

    // ── Per-app split-tunnel list (only meaningful once a config is in Partial mode) ──
    let mut app_rows: Vec<Element<Message>> = Vec::new();
    if !vpn_state.netns_active {
        app_rows.push(dim("Put a config in Partial mode above to route individual apps through it.".to_string()));
    } else if split_apps.is_empty() {
        app_rows.push(dim("No apps found in /usr/share/applications.".to_string()));
    } else {
        for a in split_apps.iter() {
            let id = a.id.clone();
            let name = a.name.clone();
            app_rows.push(
                row![
                    label(a.name.clone()),
                    iced::widget::horizontal_space(),
                    toggler(a.routed).on_toggle(move |v| Message::VpnAppRouteToggled(id.clone(), name.clone(), v)),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(12)
                .into()
            );
        }
    }
    let app_card = container(
        column(std::iter::once(title("Route through VPN")).chain(app_rows).collect::<Vec<_>>())
            .spacing(10)
            .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Web shortcuts (go/alias) ─────────────────────────────────────────
    let galias_rows: Vec<Element<Message>> = if galias_aliases.is_empty() {
        vec![dim("No shortcuts yet.".to_string())]
    } else {
        galias_aliases.iter().map(|a| {
            let key = a.key.clone();
            row![
                column![
                    label(format!("go/{}", a.key)),
                    dim(a.url.clone()),
                ]
                .spacing(2)
                .width(Length::Fill),
                action_btn("Remove", Message::GaliasRemove(key), ActionKind::Danger),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into()
        }).collect()
    };

    let galias_card = container(
        column![
            title("Web shortcuts (go/alias)"),
            dim("Type go/<name> in a browser to jump straight to a saved URL.".to_string()),
            column![
                iced::widget::text_input("name (e.g. radarr)", galias_key)
                    .on_input(Message::GaliasKeyChanged)
                    .padding(8),
                iced::widget::text_input("destination URL", galias_url)
                    .on_input(Message::GaliasUrlChanged)
                    .on_submit(Message::GaliasAddSubmit)
                    .padding(8),
                action_btn("Add shortcut", Message::GaliasAddSubmit, ActionKind::Accent),
            ]
            .spacing(8),
            column(galias_rows).spacing(10),
        ]
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    scrollable(
        column![wifi_card, eth_card, bt_card, ts_card, vpn_card, app_card, galias_card]
            .spacing(16)
            .padding(20)
    )
    .into()
}
