use iced::widget::{button, column, container, row, scrollable, text, toggler};
use iced::{Background, Border, Color, Element, Length};

use crate::app::{AppColors, Message};
use crate::printing::{DetectedDevice, Printer};

pub fn view<'a>(
    colors: AppColors,
    cups_running: bool,
    printers: &'a [Printer],
    detected: &'a [DetectedDevice],
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

    // ── CUPS service status ─────────────────────────────────────────────
    let cups_card = container(
        row![
            column![
                title("Printing service (CUPS)"),
                dim(if cups_running { "Running".to_string() } else { "Not running — printers won't work until this is on.".to_string() }),
            ]
            .spacing(2)
            .width(Length::Fill),
            toggler(cups_running).on_toggle(Message::PrintingCupsToggled),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Configured printers ──────────────────────────────────────────────
    let printer_rows: Vec<Element<Message>> = if printers.is_empty() {
        vec![dim("No printers configured yet.".to_string())]
    } else {
        printers.iter().map(|p| {
            let name = p.name.clone();
            let name2 = p.name.clone();
            let name3 = p.name.clone();
            let default_action: Element<Message> = if p.is_default {
                dim("Default".to_string())
            } else {
                action_btn("Set default", Message::PrintingSetDefault(name.clone()), ActionKind::Accent)
            };
            row![
                column![
                    label(p.name.clone()),
                    dim(format!("{}{} · {}", p.make_model, if p.make_model.is_empty() { "" } else { " " }, p.state)),
                ]
                .spacing(2)
                .width(Length::Fill),
                default_action,
                action_btn("Test page", Message::PrintingTestPage(name2), ActionKind::Plain),
                action_btn("Remove", Message::PrintingRemove(name3), ActionKind::Danger),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into()
        }).collect()
    };
    let printers_card = container(
        column(std::iter::once(title("Printers")).chain(printer_rows).collect::<Vec<_>>())
            .spacing(10)
            .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Detected devices needing setup ──────────────────────────────────
    let detected_rows: Vec<Element<Message>> = if detected.is_empty() {
        vec![dim("No new printers detected. Plug one in via USB or make sure it's on the same network.".to_string())]
    } else {
        detected.iter().map(|d| {
            let uri = d.uri.clone();
            let hint = crate::printing::driver_package_hint(&d.info);
            let mut col = column![
                row![
                    label(d.info.clone()),
                    iced::widget::horizontal_space(),
                    action_btn("Add (driverless)", Message::PrintingAddDriverless(uri.clone()), ActionKind::Accent),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(10),
            ]
            .spacing(4);
            if let Some((vendor, pkg)) = hint {
                col = col.push(dim(format!("If driverless setup doesn't work: looks like a {vendor} printer — try installing {pkg}.")));
            }
            col.into()
        }).collect()
    };
    let detected_card = container(
        column(std::iter::once(title("Detected devices")).chain(detected_rows).collect::<Vec<_>>())
            .spacing(10)
            .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    scrollable(
        column![cups_card, printers_card, detected_card]
            .spacing(16)
            .padding(20)
    )
    .into()
}
