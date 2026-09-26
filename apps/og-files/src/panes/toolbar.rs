use iced::widget::{button, container, row, text, text_input};
use iced::{Background, Border, Color, Element, Length};

use crate::app::Message;
use crate::tab::ViewMode;
use crate::theme;

const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    can_back: bool,
    can_forward: bool,
    display_path: String,
    path_edit: Option<&'a str>,
    search_query: &'a str,
    view_mode: ViewMode,
    show_hidden: bool,
    searching: bool,
) -> Element<'a, Message> {
    let pal = theme::p();

    let nav_btn = |label: &'static str, msg: Message, enabled: bool| {
        let b = button(text(label).size(14))
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if enabled {
                    match status {
                        button::Status::Hovered => pal.surface,
                        _ => pal.sec_bg,
                    }
                } else {
                    Color { r: 0.15, g: 0.13, b: 0.18, a: 1.0 }
                })),
                text_color: if enabled { pal.text } else { Color { r: 0.4, g: 0.4, b: 0.45, a: 1.0 } },
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
            .padding([4, 10]);
        if enabled { b.on_press(msg) } else { b }
    };

    let icon_btn = |icon: &'static str, msg: Message| {
        button(text(icon).font(ICON_FONT).size(14))
            .style(theme::flat_button)
            .padding([4, 10])
            .on_press(msg)
    };

    let icon_plus_btn = |icon: &'static str, msg: Message| {
        button(row![text(icon).font(ICON_FONT).size(14), text("+").size(13)].spacing(3).align_y(iced::Alignment::Center))
            .style(theme::flat_button)
            .padding([4, 10])
            .on_press(msg)
    };

    let path_bar: Element<Message> = if let Some(editing_text) = path_edit {
        text_input("Path...", editing_text)
            .id(text_input::Id::new("pathbar"))
            .on_input(Message::PathBarEdit)
            .on_submit(Message::PathBarSubmit)
            .size(13)
            .style(theme::input_style)
            .into()
    } else {
        button(text(display_path.clone()).size(13))
            .on_press(Message::PathBarEdit(display_path))
            .style(|_, _| button::Style {
                background: Some(Background::Color(theme::p().sec_bg)),
                text_color: theme::p().text,
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
            .width(Length::Fill)
            .into()
    };

    let search_placeholder = if searching { "Searching..." } else { "Search..." };
    let search: Element<Message> = text_input(search_placeholder, search_query)
        .on_input(Message::SearchChanged)
        .on_submit(Message::SearchSubmit)
        .size(13)
        .width(180)
        .style(theme::input_style)
        .into();

    let icon_word_btn = |icon: &'static str, label: &'static str, msg: Message| {
        button(row![text(icon).font(ICON_FONT).size(14), text(label).size(13)].spacing(6).align_y(iced::Alignment::Center))
            .style(theme::flat_button)
            .padding([4, 10])
            .on_press(msg)
    };

    let (view_icon, view_label) = match view_mode {
        ViewMode::Grid => ("\u{f00b}", "List"),
        ViewMode::List => ("\u{f00a}", "Grid"),
    };
    let (hidden_icon, hidden_label) = if show_hidden { ("\u{f070}", "Hide") } else { ("\u{f06e}", "Show") };

    let mut items = row![
        nav_btn("←", Message::NavigateBack, can_back),
        nav_btn("→", Message::NavigateForward, can_forward),
        nav_btn("↑", Message::NavigateUp, true),
        icon_btn("\u{f015}", Message::NavigateHome),
        container(path_bar).width(Length::Fill).padding([0, 6]),
        search,
        icon_word_btn(view_icon, view_label, Message::ViewModeToggle),
    ]
    .spacing(4)
    .padding([6, 8])
    .align_y(iced::Alignment::Center);

    if matches!(view_mode, ViewMode::Grid) {
        items = items.push(icon_btn("\u{f010}", Message::ZoomOut));
        items = items.push(icon_btn("\u{f00e}", Message::ZoomIn));
    }

    items = items.push(icon_word_btn(hidden_icon, hidden_label, Message::ShowHiddenToggle));
    items = items.push(icon_btn("\u{f021}", Message::Refresh));
    items = items.push(icon_plus_btn("\u{f07b}", Message::NewFolder));
    items = items.push(icon_plus_btn("\u{f0f6}", Message::NewFile));

    container(items).width(Length::Fill).style(theme::panel).into()
}
