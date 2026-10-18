//! Bluetooth card (Network tab) and the pairing-agent dialog. State lives
//! in `crate::bluetooth::BtUi`; this file only renders it, in the same
//! card/button language as the rest of `tabs::network`.

use iced::widget::{button, column, container, row, text, text_input, toggler, Space};
use iced::{Alignment, Background, Border, Color, Element, Length};

use crate::app::{AppColors, Message};
use crate::bluetooth::model::{icon_glyph, signal_bars};
use crate::bluetooth::{
    agent, pairing_mode_hint, AgentPrompt, BtDevice, BtMsg, BtUi, DeviceOp, OpState,
};

const DANGER: Color = Color {
    r: 0.6,
    g: 0.1,
    b: 0.1,
    a: 1.0,
};
const ERROR_TEXT: Color = Color {
    r: 0.95,
    g: 0.45,
    b: 0.42,
    a: 1.0,
};
const BATTERY_GLYPH: &str = "\u{f0079}";

fn nerd_font() -> iced::Font {
    iced::Font::with_name("Symbols Nerd Font")
}

fn bt(m: BtMsg) -> Message {
    Message::Bluetooth(m)
}

#[derive(Clone, Copy)]
enum Kind {
    Plain,
    Accent,
    Danger,
}

fn action_btn<'a>(
    colors: AppColors,
    label: &str,
    msg: Option<Message>,
    kind: Kind,
) -> Element<'a, Message> {
    let text_color = match kind {
        Kind::Plain => colors.text,
        Kind::Accent => colors.bar_bg,
        Kind::Danger => Color::WHITE,
    };
    let disabled = msg.is_none();
    button(
        text(label.to_string())
            .size(12)
            .style(move |_| text::Style {
                color: Some(text_color),
            }),
    )
    .on_press_maybe(msg)
    .padding([4, 10])
    .style(move |_, _| {
        let (bg, border) = match kind {
            Kind::Plain => (
                colors.surface,
                Border {
                    color: colors.border,
                    width: 1.0,
                    radius: colors.radius.into(),
                },
            ),
            Kind::Accent => (
                colors.accent,
                Border {
                    radius: colors.radius.into(),
                    ..Default::default()
                },
            ),
            Kind::Danger => (
                DANGER,
                Border {
                    radius: colors.radius.into(),
                    ..Default::default()
                },
            ),
        };
        button::Style {
            background: Some(Background::Color(if disabled {
                bg.scale_alpha(0.45)
            } else {
                bg
            })),
            text_color,
            border,
            ..Default::default()
        }
    })
    .into()
}

fn label<'a>(colors: AppColors, t: String) -> Element<'a, Message> {
    text(t)
        .style(move |_| text::Style {
            color: Some(colors.text),
        })
        .into()
}

fn dim<'a>(colors: AppColors, t: String) -> Element<'a, Message> {
    text(t)
        .size(12)
        .style(move |_| text::Style {
            color: Some(colors.dim_text),
        })
        .into()
}

fn small<'a>(t: String, color: Color) -> Element<'a, Message> {
    text(t)
        .size(12)
        .style(move |_| text::Style { color: Some(color) })
        .into()
}

fn section_title<'a>(colors: AppColors, t: String) -> Element<'a, Message> {
    text(t)
        .size(13)
        .style(move |_| text::Style {
            color: Some(colors.dim_text),
        })
        .into()
}

fn pill<'a>(t: &'static str, color: Color) -> Element<'a, Message> {
    container(
        text(t)
            .size(11)
            .style(move |_| text::Style { color: Some(color) }),
    )
    .padding([2, 8])
    .style(move |_| container::Style {
        background: Some(Background::Color(color.scale_alpha(0.15))),
        border: Border {
            color,
            width: 1.0,
            radius: 999.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn device_icon<'a>(colors: AppColors, icon: Option<&str>, active: bool) -> Element<'a, Message> {
    let color = if active { colors.accent } else { colors.text };
    container(
        text(icon_glyph(icon))
            .size(20)
            .font(nerd_font())
            .style(move |_| text::Style { color: Some(color) }),
    )
    .width(32)
    .center_x(32)
    .into()
}

fn bars<'a>(colors: AppColors, rssi: Option<i16>) -> Element<'a, Message> {
    let n = signal_bars(rssi) as usize;
    if n == 0 {
        return Space::new(0, 0).into();
    }
    text("●".repeat(n) + &"○".repeat(4 - n))
        .size(9)
        .style(move |_| text::Style {
            color: Some(colors.accent),
        })
        .into()
}

/// Status / error line under a device name, plus the extra buttons a
/// failed operation offers.
fn op_lines<'a>(
    colors: AppColors,
    address: &str,
    op: &DeviceOp,
) -> (Vec<Element<'a, Message>>, Vec<Element<'a, Message>>) {
    match &op.state {
        OpState::Busy(l) => {
            let mut lines = vec![small(l.clone(), colors.accent)];
            if l.starts_with("Waiting") {
                lines.push(dim(colors, pairing_mode_hint(op.airpods).to_string()));
            }
            (lines, Vec::new())
        }
        OpState::Failed { message, repair } => {
            let mut lines = vec![small(message.clone(), ERROR_TEXT)];
            let mut buttons = Vec::new();
            if *repair {
                lines.push(dim(
                    colors,
                    format!(
                        "Forget & pair again removes the old entry and pairs from scratch. {}",
                        pairing_mode_hint(op.airpods)
                    ),
                ));
                buttons.push(action_btn(
                    colors,
                    "Forget & pair again",
                    Some(bt(BtMsg::Repair(address.to_string()))),
                    Kind::Accent,
                ));
            }
            buttons.push(action_btn(
                colors,
                "Dismiss",
                Some(bt(BtMsg::DismissOp(address.to_string()))),
                Kind::Plain,
            ));
            (lines, buttons)
        }
    }
}

fn device_row<'a>(colors: AppColors, ui: &BtUi, d: &BtDevice) -> Element<'a, Message> {
    let op = ui.ops.get(&d.address);
    let busy = op.is_some_and(DeviceOp::is_busy);
    let addr = d.address.clone();

    let mut title_row: Vec<Element<Message>> = vec![label(colors, d.display_name())];
    if d.is_known() {
        title_row.push(if d.connected {
            pill("Connected", colors.accent)
        } else {
            pill("Paired", colors.text)
        });
    }
    if let Some(b) = d.battery {
        title_row.push(
            row![
                text(BATTERY_GLYPH)
                    .size(13)
                    .font(nerd_font())
                    .style(move |_| text::Style {
                        color: Some(colors.dim_text)
                    }),
                dim(colors, format!("{b}%")),
            ]
            .spacing(2)
            .align_y(Alignment::Center)
            .into(),
        );
    }
    if !d.is_known() {
        title_row.push(bars(colors, d.rssi));
    }

    let mut lines: Vec<Element<Message>> =
        vec![row(title_row).spacing(8).align_y(Alignment::Center).into()];
    let mut extra_buttons = Vec::new();
    match op {
        Some(op) => {
            let (l, b) = op_lines(colors, &d.address, op);
            lines.extend(l);
            extra_buttons = b;
        }
        None => {
            let sub = if d.is_known() {
                d.address.clone()
            } else if d.half_paired() {
                format!("{} · unfinished pairing — Pair will reset it", d.address)
            } else if d.rssi.is_none() {
                format!("{} · not seen recently", d.address)
            } else {
                d.address.clone()
            };
            lines.push(dim(colors, sub));
        }
    }

    let mut actions: Vec<Element<Message>> = Vec::new();
    let failed = op.is_some_and(|o| !o.is_busy());
    if failed {
        actions.extend(extra_buttons);
    } else if d.is_known() {
        if d.connected {
            actions.push(action_btn(
                colors,
                "Disconnect",
                (!busy).then(|| bt(BtMsg::Disconnect(addr.clone()))),
                Kind::Plain,
            ));
        } else {
            actions.push(action_btn(
                colors,
                "Connect",
                (!busy).then(|| bt(BtMsg::Connect(addr.clone()))),
                Kind::Accent,
            ));
        }
        actions.push(action_btn(
            colors,
            "Forget",
            (!busy).then(|| bt(BtMsg::Forget(addr.clone()))),
            Kind::Danger,
        ));
    } else {
        actions.push(action_btn(
            colors,
            "Pair",
            (!busy).then(|| bt(BtMsg::Pair(addr.clone()))),
            Kind::Accent,
        ));
    }

    row![
        device_icon(colors, d.icon.as_deref(), d.connected),
        column(lines).spacing(3).width(Length::Fill),
        row(actions).spacing(8).align_y(Alignment::Center),
    ]
    .align_y(Alignment::Center)
    .spacing(10)
    .into()
}

/// A device that is mid-operation but currently has no BlueZ object
/// (forgotten and waiting to be re-discovered).
fn ghost_row<'a>(colors: AppColors, address: &str, op: &DeviceOp) -> Element<'a, Message> {
    let (lines, buttons) = op_lines(colors, address, op);
    row![
        device_icon(colors, op.icon.as_deref(), false),
        column(
            std::iter::once(label(colors, op.name.clone()))
                .chain(lines)
                .collect::<Vec<_>>()
        )
        .spacing(3)
        .width(Length::Fill),
        row(buttons).spacing(8),
    ]
    .align_y(Alignment::Center)
    .spacing(10)
    .into()
}

fn toggle_row<'a>(
    colors: AppColors,
    name: &str,
    hint: &str,
    on: bool,
    f: fn(bool) -> BtMsg,
) -> Element<'a, Message> {
    row![
        column![
            label(colors, name.to_string()),
            dim(colors, hint.to_string())
        ]
        .spacing(2)
        .width(Length::Fill),
        toggler(on).on_toggle(move |v| bt(f(v))),
    ]
    .align_y(Alignment::Center)
    .spacing(12)
    .into()
}

pub fn card<'a>(colors: AppColors, ui: &'a BtUi) -> Element<'a, Message> {
    let adapter = ui.adapter();
    let powered = adapter.is_some_and(|a| a.powered);

    let scan_ctrl: Element<Message> = match ui.scan_remaining() {
        Some(left) if powered => {
            let secs = left.as_secs();
            row![
                small(
                    format!("Scanning… {}:{:02}", secs / 60, secs % 60),
                    colors.accent
                ),
                action_btn(colors, "Stop", Some(bt(BtMsg::ScanStop)), Kind::Plain),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        }
        _ => button(text("Add device").size(12).style(move |_| text::Style {
            color: Some(colors.text),
        }))
        .style(move |_, _| button::Style {
            background: Some(Background::Color(if powered {
                colors.surface
            } else {
                colors.surface.scale_alpha(0.45)
            })),
            text_color: colors.text,
            border: Border {
                color: colors.border,
                width: 1.0,
                radius: colors.radius.into(),
            },
            ..Default::default()
        })
        .on_press_maybe(powered.then(|| bt(BtMsg::ScanStart)))
        .padding([6, 14])
        .into(),
    };

    let header = row![
        text("Bluetooth").size(15).style(move |_| text::Style {
            color: Some(colors.text)
        }),
        Space::with_width(Length::Fill),
        scan_ctrl,
        toggler(powered).on_toggle_maybe(adapter.is_some().then_some(|v| bt(BtMsg::SetPowered(v)))),
    ]
    .align_y(Alignment::Center)
    .spacing(12);

    let mut rows: Vec<Element<Message>> = vec![header.into()];

    if let Some(e) = &ui.bus_error {
        rows.push(small(e.clone(), ERROR_TEXT));
    }
    if let Some(e) = &ui.adapter_error {
        rows.push(
            row![
                container(small(e.clone(), ERROR_TEXT)).width(Length::Fill),
                action_btn(
                    colors,
                    "Dismiss",
                    Some(bt(BtMsg::DismissAdapterError)),
                    Kind::Plain
                ),
            ]
            .align_y(Alignment::Center)
            .into(),
        );
    }

    let Some(adapter) = adapter else {
        if ui.bus_error.is_none() {
            rows.push(dim(colors, "No Bluetooth adapter found.".to_string()));
        }
        return finish(colors, rows);
    };
    if !adapter.powered {
        rows.push(dim(colors, "Bluetooth is off.".to_string()));
        return finish(colors, rows);
    }

    rows.push(dim(
        colors,
        format!(
            "This computer is visible as “{}” · {}",
            adapter.alias, adapter.address
        ),
    ));
    if let Some(Err(e)) = &ui.agent {
        rows.push(small(
            format!("Pairing helper unavailable ({e}) — devices that need a PIN can't pair."),
            ERROR_TEXT,
        ));
    }
    rows.push(toggle_row(
        colors,
        "Discoverable",
        "Let other devices find this computer",
        adapter.discoverable,
        BtMsg::SetDiscoverable,
    ));
    rows.push(toggle_row(
        colors,
        "Pairable",
        "Accept pairing requests (turned on automatically when you pair)",
        adapter.pairable,
        BtMsg::SetPairable,
    ));

    let lists = ui.state.grouped();

    // ── My devices ──
    rows.push(section_title(colors, "My devices".to_string()));
    if lists.mine.is_empty() {
        rows.push(dim(colors, "No paired devices yet.".to_string()));
    }
    for d in &lists.mine {
        rows.push(device_row(colors, ui, d));
    }

    // ── Available ──
    rows.push(section_title(
        colors,
        if ui.scanning() {
            "Available devices — searching…".to_string()
        } else {
            "Available devices".to_string()
        },
    ));
    let ghosts: Vec<(&String, &DeviceOp)> = ui
        .ops
        .iter()
        .filter(|(addr, _)| ui.state.device_by_address(addr).is_none())
        .collect();
    for (addr, op) in &ghosts {
        rows.push(ghost_row(colors, addr, op));
    }
    for d in &lists.available {
        rows.push(device_row(colors, ui, d));
    }
    if lists.available.is_empty() && ghosts.is_empty() {
        rows.push(dim(
            colors,
            if ui.scanning() {
                "Looking for devices… Put yours in pairing mode. AirPods: open the case and hold the button on the back until the light flashes white."
            } else {
                "Click Add device to search for new devices."
            }
            .to_string(),
        ));
    }

    if !lists.unnamed.is_empty() {
        let n = lists.unnamed.len();
        let label = if ui.show_unnamed {
            format!("Hide unnamed devices ({n})")
        } else {
            format!("Show unnamed devices ({n})")
        };
        rows.push(
            button(text(label).size(12).style(move |_| text::Style {
                color: Some(colors.dim_text),
            }))
            .style(move |_, _| button::Style {
                background: None,
                text_color: colors.dim_text,
                ..Default::default()
            })
            .padding(0)
            .on_press(bt(BtMsg::ToggleUnnamed))
            .into(),
        );
        if ui.show_unnamed {
            for d in &lists.unnamed {
                rows.push(device_row(colors, ui, d));
            }
        }
    }

    finish(colors, rows)
}

fn finish<'a>(colors: AppColors, rows: Vec<Element<'a, Message>>) -> Element<'a, Message> {
    container(column(rows).spacing(12).padding(20))
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.sec_bg)),
            border: Border {
                color: colors.border,
                width: 1.0,
                radius: colors.radius.into(),
            },
            ..Default::default()
        })
        .width(Length::Fill)
        .into()
}

/// Modal for the first open agent request, or `None`.
pub fn prompt_modal<'a>(colors: AppColors, ui: &'a BtUi) -> Option<Element<'a, Message>> {
    let req = ui.prompts.first()?;
    let name = ui.name_for_path(&req.device);
    let id = req.id;

    let big = move |t: String| -> Element<'a, Message> {
        container(text(t).size(34).style(move |_| text::Style {
            color: Some(colors.text),
        }))
        .center_x(Length::Fill)
        .padding([6, 0])
        .into()
    };
    let input = |placeholder: &str| -> Element<'a, Message> {
        text_input(placeholder, &ui.prompt_input)
            .on_input(|s| bt(BtMsg::PromptInput(s)))
            .on_submit(bt(BtMsg::PromptAnswer(id, true)))
            .padding(8)
            .style(move |_, _| text_input::Style {
                background: Background::Color(colors.surface),
                border: Border {
                    color: colors.border,
                    width: 1.0,
                    radius: colors.radius.into(),
                },
                icon: colors.dim_text,
                placeholder: colors.dim_text,
                value: colors.text,
                selection: colors.accent,
            })
            .into()
    };
    let yes_no = |no: &str, yes: &str, yes_enabled: bool| -> Element<'a, Message> {
        row![
            Space::with_width(Length::Fill),
            action_btn(
                colors,
                no,
                Some(bt(BtMsg::PromptAnswer(id, false))),
                Kind::Plain
            ),
            action_btn(
                colors,
                yes,
                yes_enabled.then(|| bt(BtMsg::PromptAnswer(id, true))),
                Kind::Accent
            ),
        ]
        .spacing(10)
        .into()
    };
    let cancel_only = || -> Element<'a, Message> {
        row![
            Space::with_width(Length::Fill),
            action_btn(
                colors,
                "Cancel",
                Some(bt(BtMsg::PromptCancelDisplay(id))),
                Kind::Plain
            )
        ]
        .into()
    };

    let body: Vec<Element<Message>> = match &req.prompt {
        AgentPrompt::Confirm { passkey } => vec![
            label(
                colors,
                format!("Does this code match the one shown on {name}?"),
            ),
            big(agent::format_passkey(*passkey)),
            yes_no("No", "Yes, it matches", true),
        ],
        AgentPrompt::Authorize => vec![
            label(colors, format!("{name} wants to pair with this computer.")),
            yes_no("Deny", "Allow", true),
        ],
        AgentPrompt::AuthorizeService { uuid } => vec![
            label(
                colors,
                format!("{name} (not paired) wants to use a service on this computer."),
            ),
            dim(colors, uuid.clone()),
            yes_no("Deny", "Allow", true),
        ],
        AgentPrompt::PinCode => vec![
            label(colors, format!("Enter the PIN code for {name}.")),
            dim(
                colors,
                "Check the device's manual — it's often 0000 or 1234.".to_string(),
            ),
            input("PIN"),
            yes_no(
                "Cancel",
                "Pair",
                agent::parse_pin(&ui.prompt_input).is_some(),
            ),
        ],
        AgentPrompt::Passkey => vec![
            label(
                colors,
                format!("Enter the 6-digit passkey shown on {name}."),
            ),
            input("123456"),
            yes_no(
                "Cancel",
                "OK",
                agent::parse_passkey(&ui.prompt_input).is_some(),
            ),
        ],
        AgentPrompt::DisplayPinCode { pincode } => vec![
            label(
                colors,
                format!("Type this PIN on {name}, then press Enter."),
            ),
            big(pincode.clone()),
            cancel_only(),
        ],
        AgentPrompt::DisplayPasskey { passkey, entered } => {
            let code = agent::format_passkey(*passkey);
            let typed = (*entered as usize).min(code.len());
            vec![
                label(
                    colors,
                    format!("Type this code on {name}, then press Enter."),
                ),
                big(code),
                dim(
                    colors,
                    if typed > 0 {
                        format!("{typed} of 6 digits typed")
                    } else {
                        "Waiting for you to type it…".to_string()
                    },
                ),
                cancel_only(),
            ]
        }
    };

    let more = ui.prompts.len().saturating_sub(1);
    let mut children: Vec<Element<Message>> = vec![row![
        text(icon_glyph(None))
            .size(18)
            .font(nerd_font())
            .style(move |_| text::Style {
                color: Some(colors.accent)
            }),
        text("Bluetooth pairing")
            .size(15)
            .style(move |_| text::Style {
                color: Some(colors.text)
            }),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()];
    children.extend(body);
    if more > 0 {
        children.push(dim(colors, format!("{more} more request(s) waiting")));
    }

    let card = container(column(children).spacing(14).padding(24))
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.sec_bg)),
            border: Border {
                color: colors.border,
                width: 1.0,
                radius: (colors.radius + 2.0).into(),
            },
            ..Default::default()
        })
        .width(380);

    // `opaque`: clicks on the dimmed backdrop must not reach the page below.
    Some(iced::widget::opaque(
        container(card)
            .style(|_| container::Style {
                background: Some(Background::Color(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.55,
                })),
                ..Default::default()
            })
            .center(Length::Fill),
    ))
}
