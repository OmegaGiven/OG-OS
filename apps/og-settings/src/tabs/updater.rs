use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Element, Length};

use crate::app::{AppColors, Message, UpdateSection};
use crate::sway::{UpdateItem, UpdateStatus};

pub fn view<'a>(
    colors: AppColors,
    status: &'a UpdateStatus,
    checking: bool,
    search: &'a str,
    collapsed: &'a [UpdateSection],
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

    let title = move |t: String| -> Element<'a, Message> {
        text(t).size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let dim = move |t: String| -> Element<'a, Message> {
        text(t).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()
    };
    let action_btn = move |label: &'static str, msg: Option<Message>, accent: bool| -> Element<'a, Message> {
        let text_color = if accent { colors.bar_bg } else { colors.text };
        let btn = button(text(label).size(12).style(move |_| iced::widget::text::Style { color: Some(text_color) }))
            .on_press_maybe(msg)
            .padding([6, 14]);
        (if accent { btn.style(accent_btn_style) } else { btn.style(btn_style) }).into()
    };

    let check_label: &'static str = if checking { "Checking…" } else { "Check for Updates" };
    let check_btn = action_btn(check_label, if checking { None } else { Some(Message::UpdatesCheckStart) }, false);

    let total = status.pacman.len() + status.aur.len() + status.flatpak.len();
    let search_lower = search.to_lowercase();
    let matches = move |name: &str| search_lower.is_empty() || name.to_lowercase().contains(&search_lower);

    let source_section = |section: UpdateSection,
                           name: &'static str,
                           items: &'a [UpdateItem],
                           available: bool,
                           apply_all: Message,
                           apply_one: fn(String) -> Message|
     -> Element<'a, Message> {
        let is_collapsed = collapsed.contains(&section);
        let filtered: Vec<&UpdateItem> = items.iter().filter(|i| matches(&i.name)).collect();

        let status_line = if !available {
            dim("Not installed".to_string())
        } else if items.is_empty() {
            dim("Up to date".to_string())
        } else {
            dim(format!("{} update{} available", items.len(), if items.len() == 1 { "" } else { "s" }))
        };

        let toggle_label: &'static str = if is_collapsed { "▸" } else { "▾" };
        let toggle_btn = button(text(toggle_label).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.text) }))
            .style(btn_style)
            .on_press(Message::UpdatesSectionToggled(section))
            .padding([4, 10]);

        let header = row![
            toggle_btn,
            column![title(name.to_string()), status_line].spacing(4).width(Length::Fill),
            action_btn("Update All", (available && !items.is_empty()).then_some(apply_all), available && !items.is_empty()),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12);

        let mut section_col: Vec<Element<Message>> = vec![header.into()];

        if !is_collapsed && available && !items.is_empty() {
            if filtered.is_empty() {
                section_col.push(dim("No updates match your search.".to_string()));
            }
            for item in filtered {
                let version_text = if item.old_version.is_empty() {
                    item.new_version.clone()
                } else {
                    format!("{} → {}", item.old_version, item.new_version)
                };
                let id = item.id.clone();
                section_col.push(
                    row![
                        column![
                            text(item.name.clone()).size(13).style(move |_| iced::widget::text::Style { color: Some(colors.text) }),
                            dim(version_text),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        action_btn("Update", Some(apply_one(id)), false),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(12)
                    .into()
                );
            }
        }

        container(column(section_col).spacing(12).padding(20)).style(card_style).width(Length::Fill).into()
    };

    let search_bar = text_input("Search updates by name…", search)
        .on_input(Message::UpdatesSearchChanged)
        .style(move |_, _| iced::widget::text_input::Style {
            background: Background::Color(colors.surface),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            icon: colors.dim_text,
            placeholder: colors.dim_text,
            value: colors.text,
            selection: colors.accent,
        })
        .padding(10)
        .width(Length::Fill);

    let cards = column![
        row![
            title(format!("System Updates — {total} available")),
            iced::widget::horizontal_space(),
            check_btn,
        ]
        .align_y(iced::Alignment::Center)
        .spacing(12),
        search_bar,
        source_section(UpdateSection::Pacman, "Pacman (official repos)", &status.pacman, true, Message::UpdatesApplyPacman(None), |id| Message::UpdatesApplyPacman(Some(id))),
        source_section(UpdateSection::Aur, "AUR (yay)", &status.aur, status.aur_available, Message::UpdatesApplyAur(None), |id| Message::UpdatesApplyAur(Some(id))),
        source_section(UpdateSection::Flatpak, "Flatpak", &status.flatpak, status.flatpak_available, Message::UpdatesApplyFlatpak(None), |id| Message::UpdatesApplyFlatpak(Some(id))),
        dim("Applying updates opens a terminal asking for your sudo password.".to_string()),
    ]
    .spacing(16);

    scrollable(container(cards).padding(20)).into()
}
