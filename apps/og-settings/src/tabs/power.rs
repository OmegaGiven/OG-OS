use iced::widget::{column, container, mouse_area, row, scrollable, text, toggler};
use iced::{Background, Border, Element, Length};

use crate::app::{AppColors, Message};
use crate::config::Config;
use crate::sway::{StartupExecEntry, SystemdServiceInfo};

/// Wraps a settings row so clicking anywhere on it (except the toggle
/// itself, which handles its own click) highlights it with an accent
/// border — makes it obvious which toggle belongs to which row once a
/// section gets long.
fn selectable_row<'a>(
    section: u8,
    idx: usize,
    selected: Option<(u8, usize)>,
    colors: AppColors,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    let is_selected = selected == Some((section, idx));
    mouse_area(
        container(content)
            .padding(10)
            .style(move |_| container::Style {
                background: Some(Background::Color(if is_selected { colors.surface } else { iced::Color::TRANSPARENT })),
                border: Border {
                    color: if is_selected { colors.accent } else { iced::Color::TRANSPARENT },
                    width: if is_selected { 2.0 } else { 1.0 },
                    radius: colors.radius.into(),
                },
                ..Default::default()
            })
            .width(Length::Fill)
    )
    .on_press(Message::PowerRowSelected(Some((section, idx))))
    .into()
}

pub fn view<'a>(
    config: &'a Config,
    colors: AppColors,
    startup_execs: &'a [StartupExecEntry],
    systemd_services: &'a [SystemdServiceInfo],
    system_services: &'a [SystemdServiceInfo],
    selected: Option<(u8, usize)>,
) -> Element<'a, Message> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border {
            color: colors.border,
            width: 1.0,
            radius: colors.radius.into(),
        },
        ..Default::default()
    };

    let monitor_card = container(
        column![
            section_title("Monitor Sleep", colors),
            row![
                label_text("Enable monitor sleep", colors),
                iced::widget::horizontal_space(),
                toggler(config.monitor_sleep.enabled)
                    .on_toggle(Message::MonitorSleepToggled)
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            row![
                label_text("Sleep after (minutes)", colors),
                iced::widget::horizontal_space(),
                spin_row(
                    config.monitor_sleep.minutes,
                    Message::MonitorSleepMinus,
                    Message::MonitorSleepPlus,
                    colors,
                ),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
        ]
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    let system_card = container(
        column![
            section_title("System Sleep / Suspend", colors),
            row![
                label_text("Enable system suspend", colors),
                iced::widget::horizontal_space(),
                toggler(config.system_sleep.enabled)
                    .on_toggle(Message::SystemSleepToggled)
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            row![
                label_text("Suspend after (minutes)", colors),
                iced::widget::horizontal_space(),
                spin_row(
                    config.system_sleep.minutes,
                    Message::SystemSleepMinus,
                    Message::SystemSleepPlus,
                    colors,
                ),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
        ]
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    let lock_card = container(
        column![
            section_title("Screen Lock", colors),
            row![
                label_text("Lock on idle", colors),
                iced::widget::horizontal_space(),
                toggler(config.screen_lock.enabled)
                    .on_toggle(Message::ScreenLockToggled)
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            row![
                label_text("Lock after (minutes)", colors),
                iced::widget::horizontal_space(),
                spin_row(
                    config.screen_lock.minutes,
                    Message::ScreenLockMinus,
                    Message::ScreenLockPlus,
                    colors,
                ),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            text("Also locks right before system suspend, regardless of the timeout above.")
                .size(11)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
        ]
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    let gaming_card = container(
        column![
            section_title("Gaming", colors),
            row![
                label_text("Keep games running when tabbed away", colors),
                iced::widget::horizontal_space(),
                toggler(config.gamescope_steam)
                    .on_toggle(Message::GamescopeSteamToggled)
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12),
            text("Runs Steam inside gamescope, a nested compositor — alt-tabbing your desktop then never touches the game's own window focus, so hosted multiplayer games (especially Proton titles) don't throttle or freeze for anyone else connected. Requires the gamescope package. Takes effect the next time you launch Steam.")
                .size(11)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
        ]
        .spacing(16)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Sway startup commands ──────────────────────────────────────────────
    let exec_rows: Vec<Element<Message>> = if startup_execs.is_empty() {
        vec![
            text("No toggleable startup commands found.")
                .size(12)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                .into()
        ]
    } else {
        startup_execs.iter().enumerate().map(|(idx, e)| {
            let content: Element<Message> = row![
                container(
                    column![
                        text(e.command.clone())
                            .style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                        text(if e.always { "exec_always" } else { "exec (once at startup)" })
                            .size(11)
                            .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
                    ]
                    .spacing(2)
                ).width(Length::Fill),
                toggler(e.enabled).on_toggle(move |v| Message::StartupExecToggled(idx, v)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .into();
            selectable_row(0, idx, selected, colors, content)
        }).collect()
    };

    let startup_execs_card = container(
        column(
            std::iter::once(section_title("Sway Startup Commands", colors))
                .chain(exec_rows)
                .collect::<Vec<_>>()
        )
        .spacing(14)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Systemd user services ──────────────────────────────────────────────
    let service_rows: Vec<Element<Message>> = if systemd_services.is_empty() {
        vec![
            text("No toggleable services found.")
                .size(12)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                .into()
        ]
    } else {
        systemd_services.iter().enumerate().map(|(idx, s)| {
            let content: Element<Message> = row![
                text(s.unit.clone())
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
                    .width(Length::Fill),
                toggler(s.enabled).on_toggle(move |v| Message::SystemdServiceToggled(idx, v)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .into();
            selectable_row(1, idx, selected, colors, content)
        }).collect()
    };

    let services_card = container(
        column(
            std::iter::once(section_title("Startup Services (systemd --user)", colors))
                .chain(std::iter::once(
                    text("Changes apply at next login/startup.")
                        .size(11)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                        .into()
                ))
                .chain(service_rows)
                .collect::<Vec<_>>()
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── System-level services (root, need sudo) ────────────────────────────
    let system_service_rows: Vec<Element<Message>> = if system_services.is_empty() {
        vec![
            text("No toggleable system services found.")
                .size(12)
                .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                .into()
        ]
    } else {
        system_services.iter().enumerate().map(|(idx, s)| {
            let content: Element<Message> = row![
                text(s.unit.clone())
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
                    .width(Length::Fill),
                toggler(s.enabled).on_toggle(move |v| Message::SystemServiceToggled(idx, v)),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .into();
            selectable_row(2, idx, selected, colors, content)
        }).collect()
    };

    let system_services_card = container(
        column(
            std::iter::once(section_title("System Services (root)", colors))
                .chain(std::iter::once(
                    text("Runs as root — toggling opens a terminal asking for your sudo password. Applies at next login/startup.")
                        .size(11)
                        .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) })
                        .into()
                ))
                .chain(system_service_rows)
                .collect::<Vec<_>>()
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    scrollable(
        column![monitor_card, system_card, lock_card, gaming_card, startup_execs_card, services_card, system_services_card]
            .spacing(16)
            .padding(20)
    )
    .into()
}

fn section_title<'a>(t: &'a str, colors: AppColors) -> Element<'a, Message> {
    text(t)
        .size(15)
        .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
        .into()
}

fn label_text<'a>(t: &'a str, colors: AppColors) -> Element<'a, Message> {
    text(t)
        .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
        .into()
}

fn spin_row<'a>(
    value: u64,
    on_minus: Message,
    on_plus: Message,
    colors: AppColors,
) -> Element<'a, Message> {
    let btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(colors.surface)),
        text_color: colors.text,
        border: Border {
            color: colors.border,
            width: 1.0,
            radius: colors.radius.into(),
        },
        ..Default::default()
    };
    row![
        iced::widget::button(text("-").color(colors.text))
            .style(btn_style)
            .on_press(on_minus),
        container(
            text(value.to_string())
                .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
        )
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.surface)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .padding([4, 12]),
        iced::widget::button(text("+").color(colors.text))
            .style(btn_style)
            .on_press(on_plus),
    ]
    .spacing(4)
    .align_y(iced::Alignment::Center)
    .into()
}
