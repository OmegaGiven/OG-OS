//! The popout customization menu (PLAN.md section 7) — the actual point of
//! this rebuild. Item-size/spacing/padding/module-enable changes apply live
//! (the popup reads straight from `BarConfig`, same as the bar itself).
//! Edge/thickness changes need a real wlr layer-shell relayout, which this
//! process can't do to its own already-mapped surface — "Apply & Save"
//! writes the config and respawns the bar to pick up the new anchor/size,
//! matching the plan's own carve-out for changes that need a full relayout.

use iced::widget::{button, checkbox, column, container, mouse_area, row, slider, text, text_input};
use iced::{Background, Border, Color, Element, Length};

use og_config::{Edge, ModuleKind, SectionAlign};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

#[derive(Default)]
pub struct SettingsButton {
    // button::Style's hover styling comes for free from its Status
    // callback, but this needs both left- and right-click (button only
    // gives on_press), so it's built on mouse_area instead — which has no
    // such Status, hence tracking hover state by hand via on_enter/on_exit.
    hovered: bool,
}

impl SettingsButton {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for SettingsButton {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        let bg = if self.hovered { colors.header_btn_bg } else { Color::TRANSPARENT };
        // Left click: the full og-settings Bar tab (same plain-launch
        // pattern the network/bluetooth buttons already use via
        // Message::Launch — not adding a focus-or-launch shell fallback
        // here, since Launch's own setsid wrapping only applies to the
        // first command in a string and a `cmd1 || cmd2 &` compound
        // wouldn't setsid the fallback branch correctly). Right click:
        // the quick on-the-spot popout, unchanged.
        mouse_area(
            container(
                container(text("\u{eaf8}").size(16).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(fg) }))
                    .width(size as u16)
                    .height(size as u16)
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            )
            .style(move |_| container::Style {
                background: Some(Background::Color(bg)),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            }),
        )
        .on_press(Message::Launch("og-settings --tab bar".to_string()))
        .on_right_press(Message::OpenSettingsPopup)
        .on_enter(Message::SettingsButtonHover(true))
        .on_exit(Message::SettingsButtonHover(false))
        .into()
    }

    fn update(&mut self, message: &Message) {
        if let Message::SettingsButtonHover(v) = message {
            self.hovered = *v;
        }
    }
}

fn kind_label(kind: &ModuleKind) -> String {
    match kind {
        ModuleKind::Workspaces => "Workspaces".to_string(),
        ModuleKind::Clock { .. } => "Clock".to_string(),
        ModuleKind::Cpu => "CPU".to_string(),
        ModuleKind::Gpu => "GPU".to_string(),
        ModuleKind::Memory => "Memory".to_string(),
        ModuleKind::Tray => "Tray".to_string(),
        ModuleKind::Bluetooth => "Bluetooth".to_string(),
        ModuleKind::Network => "Network".to_string(),
        ModuleKind::Pulseaudio => "Volume".to_string(),
        ModuleKind::Notifications => "Notifications".to_string(),
        ModuleKind::Clipboard => "Clipboard".to_string(),
        ModuleKind::Launcher { tooltip, .. } => tooltip.clone(),
        ModuleKind::Settings => "Settings".to_string(),
        ModuleKind::Power => "Power".to_string(),
    }
}

fn edge_button(colors: AppColors, label: &'static str, edge: Edge, current: Edge) -> Element<'static, Message> {
    let selected = edge == current;
    let fg = if selected { colors.bar_bg } else { colors.text };
    let bg = if selected { colors.accent } else { Color::TRANSPARENT };
    button(text(label).size(13).style(move |_| text::Style { color: Some(fg) }))
        .padding(6)
        .style(move |_, _| button::Style {
            background: Some(Background::Color(bg)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: fg,
            ..Default::default()
        })
        .on_press(Message::SetEdge(edge))
        .into()
}

fn labeled_slider(
    colors: AppColors,
    label: &'static str,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
    on_change: impl Fn(u32) -> Message + 'static,
) -> Element<'static, Message> {
    column![
        text(format!("{label}: {value}")).size(12).style(move |_| text::Style { color: Some(colors.text) }),
        slider(range, value, on_change).step(1u32),
    ]
    .spacing(2)
    .into()
}

fn align_button(colors: AppColors, label: &'static str, align: SectionAlign, current: SectionAlign, section: usize) -> Element<'static, Message> {
    let selected = align == current;
    let fg = if selected { colors.bar_bg } else { colors.text };
    let bg = if selected { colors.accent } else { Color::TRANSPARENT };
    button(text(label).size(11).style(move |_| text::Style { color: Some(fg) }))
        .padding([3, 8])
        .style(move |_, _| button::Style {
            background: Some(Background::Color(bg)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: fg,
            ..Default::default()
        })
        .on_press(Message::SetSectionAlign(section, align))
        .into()
}

fn module_toggle_row(colors: AppColors, section: usize, index: usize, kind: &ModuleKind, enabled: bool) -> Element<'static, Message> {
    let label = kind_label(kind);
    let mut r = row![checkbox(label, enabled).on_toggle(move |_| Message::ToggleModule(section, index))].spacing(8);
    if let ModuleKind::Clock { timezone, .. } = kind {
        r = r.push(
            text_input("IANA tz, e.g. America/Chicago (blank = local)", timezone)
                .size(11)
                .width(Length::Fixed(180.0))
                .on_input(move |v| Message::SetClockTimezone(section, index, v)),
        );
    }
    r = r.push(iced::widget::horizontal_space());
    r = r.push(
        button(text("\u{f1f8}").size(12).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(colors.text) }))
            .padding(4)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                text_color: colors.text,
                ..Default::default()
            })
            .on_press(Message::RemoveModule(section, index)),
    );
    r.into()
}

fn add_module_row(colors: AppColors, section: usize, present: &[ModuleKind]) -> Element<'static, Message> {
    let addable: Vec<Element<Message>> = ModuleKind::addable()
        .iter()
        .filter(|k| !present.contains(k))
        .map(|kind| {
            let kind = kind.clone();
            let label = kind_label(&kind);
            button(text(format!("+ {label}")).size(11).style(move |_| text::Style { color: Some(colors.text) }))
                .padding([2, 6])
                .style(move |_, status| button::Style {
                    background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                        colors.accent
                    } else {
                        colors.surface
                    })),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    text_color: colors.text,
                    ..Default::default()
                })
                .on_press(Message::AddModule(section, kind))
                .into()
        })
        .collect();
    if addable.is_empty() {
        return iced::widget::Space::new(Length::Shrink, Length::Shrink).into();
    }
    row(addable).spacing(4).wrap().into()
}

pub fn popup_view(colors: AppColors, bar_cfg: &og_config::BarConfig) -> Element<'_, Message> {
    let edge_row = row![
        edge_button(colors, "Top", Edge::Top, bar_cfg.position),
        edge_button(colors, "Bottom", Edge::Bottom, bar_cfg.position),
        edge_button(colors, "Left", Edge::Left, bar_cfg.position),
        edge_button(colors, "Right", Edge::Right, bar_cfg.position),
    ]
    .spacing(4);

    let sliders = column![
        labeled_slider(colors, "Thickness", bar_cfg.thickness, 16..=96, Message::SetThickness),
        labeled_slider(colors, "Item size", bar_cfg.item_size, 16..=80, Message::SetItemSize),
        labeled_slider(colors, "Spacing", bar_cfg.spacing, 0..=32, Message::SetSpacing),
        labeled_slider(colors, "Padding", bar_cfg.padding, 0..=32, Message::SetPadding),
    ]
    .spacing(8);

    let section_block = |index: usize, sec: &og_config::BarSection| -> Element<'static, Message> {
        let present: Vec<ModuleKind> = sec.modules.iter().map(|m| m.kind.clone()).collect();
        let rows: Vec<Element<Message>> = sec
            .modules
            .iter()
            .enumerate()
            .map(|(i, m)| module_toggle_row(colors, index, i, &m.kind, m.enabled))
            .collect();
        let align_row = row![
            align_button(colors, SectionAlign::Start.label(bar_cfg.position), SectionAlign::Start, sec.align, index),
            align_button(colors, SectionAlign::Middle.label(bar_cfg.position), SectionAlign::Middle, sec.align, index),
            align_button(colors, SectionAlign::End.label(bar_cfg.position), SectionAlign::End, sec.align, index),
        ]
        .spacing(4);
        let remove_section_btn = button(text("Remove section").size(11).style(move |_| text::Style { color: Some(colors.text) }))
            .padding([2, 6])
            .style(move |_, _| button::Style {
                background: Some(Background::Color(Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
                text_color: Color::WHITE,
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .on_press(Message::RemoveSection(index));
        column![
            row![
                text(format!("Section {}", index + 1)).size(11).style(move |_| text::Style { color: Some(colors.dim_text) }),
                iced::widget::horizontal_space(),
                remove_section_btn,
            ]
            .align_y(iced::Alignment::Center),
            labeled_slider(colors, "Share", sec.percent, 5..=100, move |v| Message::SetSectionPercent(index, v)),
            align_row,
            column(rows).spacing(4),
            add_module_row(colors, index, &present),
        ]
        .spacing(6)
        .into()
    };

    let add_section_btn = button(text("+ Add section").size(12).style(move |_| text::Style { color: Some(colors.text) }))
        .padding([4, 10])
        .style(move |_, status| button::Style {
            background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                colors.accent
            } else {
                colors.surface
            })),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: colors.text,
            ..Default::default()
        })
        .on_press(Message::AddSection);

    let module_list = column(
        bar_cfg.sections.iter().enumerate().map(|(i, s)| section_block(i, s)).chain(std::iter::once(add_section_btn.into())),
    )
    .spacing(12);

    let apply_button = button(text("Apply thickness change").size(12))
        .padding(6)
        .style(move |_, status| button::Style {
            background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                colors.header_btn_bg
            } else {
                colors.surface
            })),
            border: Border { radius: colors.radius.into(), ..Default::default() },
            text_color: colors.text,
            ..Default::default()
        })
        .on_press(Message::ApplyRelayout);

    container(
        iced::widget::scrollable(
            column![
                text("Bar edge").size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
                edge_row,
                sliders,
                text("Modules").size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
                module_list,
                apply_button,
            ]
            .spacing(10)
            .padding(10),
        ),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| container::Style {
        background: Some(colors.bg_fill),
        border: Border { color: colors.accent, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    })
    .into()
}
