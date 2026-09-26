//! og-bar's full-size config editor (PLAN.md section 8 in og-bar's own
//! PLAN.md at ~/.local/src/og-bar/PLAN.md). Structurally mirrors og-bar's
//! own popout (its `src/settings.rs`) on purpose, including the
//! section system (percent share + Start/Middle/End alignment, which
//! reads as Left/Middle/Right or Top/Middle/Bottom depending on the
//! bar's edge) — kept in sync by hand since they're separate binaries.
//! Unlike the popout (which auto-saves every change instantly), this tab
//! follows the rest of og-settings: changes stage in-memory and only hit
//! disk on "Apply & Save".

use iced::widget::{button, checkbox, column, container, mouse_area, pick_list, row, text, toggler};
use iced::{Background, Border, Color, Element, Length};

use og_config::{BarConfig, BarSection, Edge, ModuleKind, SectionAlign};

use crate::app::{AppColors, Message};
use crate::tabs::theme::TIMEZONES;

/// Mirrors `tabs::theme`'s own `tz_pick` closure (same widget, same
/// options list, same "System Default" placeholder convention) — kept as
/// a free function here instead of importing that closure directly since
/// it borrows `theme::view`'s local `colors` capture and isn't reusable
/// as-is.
fn tz_pick<'a>(colors: AppColors, value: &str, on_select: impl Fn(String) -> Message + 'a) -> Element<'a, Message> {
    // "System Default" <-> empty-string mapping happens in the message
    // handler (app.rs), same convention as `Message::ClockTimezoneSelected`
    // — this widget only ever deals in TIMEZONES' own display strings.
    let current = if value.is_empty() { "System Default" } else { value };
    pick_list(TIMEZONES, TIMEZONES.iter().find(|t| **t == current).copied(), move |v| on_select(v.to_string()))
        .style(move |_, _| iced::widget::pick_list::Style {
            background: Background::Color(colors.surface),
            text_color: colors.text,
            placeholder_color: colors.dim_text,
            handle_color: colors.dim_text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        })
        .menu_style(crate::app::pick_list_menu_style(colors))
        .text_size(11)
        .width(180)
        .into()
}

/// Fallback/vertical-bar main-axis length (its tallness) — a vertical
/// (Left/Right-edge) bar's preview scrolls top-to-bottom, so more of this
/// axis means less scrolling to reach and drag a module. Not tied to
/// window height the way the horizontal preview's length is tied to
/// window width, since a settings window is usually wider than it is
/// tall — this just goes as tall as is comfortably usable.
pub const PREVIEW_LENGTH: f32 = 900.0;
/// Cross-axis size (the bar's own thickness) for a Top/Bottom preview.
/// Wider than the real bar's ~42px thickness for the same reason the
/// vertical one is — full module names and a comfortable drag target need
/// more room than a to-scale strip has.
const PREVIEW_THICKNESS: f32 = 84.0;
/// Cross-axis size for a Left/Right preview. Has to stay a fixed width
/// (see the comment on `strip` below re: `FillPortion` and `Shrink`), so
/// this is sized to comfortably fit the longest module labels rather than
/// picked arbitrarily — wide enough to read "Notifications" or a launcher
/// tooltip without wrapping, not wider than that.
const PREVIEW_THICKNESS_VERTICAL: f32 = 220.0;
const PREVIEW_HANDLE: f32 = 8.0;
/// Width burned by chrome outside the preview itself when computing a
/// horizontal bar's fill-available-width length: the sidebar (180px, see
/// `App::sidebar`) plus margin for the tab content's own padding/scrollbar.
const PREVIEW_CHROME: f32 = 220.0;

/// Horizontal (Top/Bottom) bars get a preview as wide as the window allows
/// instead of a fixed length — `BarDividerCursorMoved` in app.rs computes
/// this same value from the same `window_width` so drag-delta-to-percent
/// math stays calibrated to whatever actually got rendered.
pub fn horizontal_preview_length(window_width: f32) -> f32 {
    (window_width - PREVIEW_CHROME).max(300.0)
}

pub fn kind_label(kind: &ModuleKind) -> String {
    match kind {
        ModuleKind::Workspaces => "Workspaces".to_string(),
        ModuleKind::Clock { .. } => "Clock".to_string(),
        ModuleKind::Cpu => "CPU".to_string(),
        ModuleKind::Memory => "Memory".to_string(),
        ModuleKind::Gpu => "GPU".to_string(),
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
        .on_press(Message::BarSetEdge(edge))
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
        .on_press(Message::BarSetSectionAlign(section, align))
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
        iced::widget::slider(range, value, on_change).step(1u32),
    ]
    .spacing(2)
    .into()
}

fn module_toggle_row(colors: AppColors, section: usize, index: usize, kind: &ModuleKind, enabled: bool) -> Element<'static, Message> {
    let label = kind_label(kind);
    let checkbox_widget = checkbox(label, enabled)
        .on_toggle(move |_| Message::BarToggleModule(section, index))
        .style(move |_, _| iced::widget::checkbox::Style {
            background: Background::Color(colors.surface),
            icon_color: colors.accent,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: Some(colors.text),
        });

    let remove_btn = button(text("Remove").size(11).style(move |_| text::Style { color: Some(colors.text) }))
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
        .on_press(Message::BarRemoveModule(section, index));

    let top_row = row![checkbox_widget, iced::widget::horizontal_space(), remove_btn].spacing(8);

    if let ModuleKind::Clock { timezone, hour12, show_timezone, show_date } = kind {
        let clock_row = row![
            tz_pick(colors, timezone, move |v| Message::BarSetClockTimezone(section, index, v)),
            row![
                text("12h").size(11).style(move |_| text::Style { color: Some(colors.text) }),
                toggler(*hour12).size(16).on_toggle(move |v| Message::BarSetClockHour12(section, index, v)),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
            row![
                text("Show tz").size(11).style(move |_| text::Style { color: Some(colors.text) }),
                toggler(*show_timezone).size(16).on_toggle(move |v| Message::BarSetClockShowTimezone(section, index, v)),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
            row![
                text("Show date").size(11).style(move |_| text::Style { color: Some(colors.text) }),
                toggler(*show_date).size(16).on_toggle(move |v| Message::BarSetClockShowDate(section, index, v)),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        ]
        .spacing(16)
        .align_y(iced::Alignment::Center);
        column![top_row, clock_row].spacing(6).into()
    } else {
        top_row.into()
    }
}

/// `pick_list` needs its item type to implement `Display`; `ModuleKind`
/// (shared with og-bar/og_config, no UI concerns there) doesn't, so this
/// wraps it just for the dropdown label.
#[derive(Clone, PartialEq)]
struct PickableKind(ModuleKind);

impl std::fmt::Display for PickableKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", kind_label(&self.0))
    }
}

fn add_module_row(colors: AppColors, section: usize, present: &[ModuleKind]) -> Element<'static, Message> {
    let addable: Vec<PickableKind> =
        ModuleKind::addable().into_iter().filter(|k| !present.contains(k)).map(PickableKind).collect();
    if addable.is_empty() {
        return iced::widget::Space::new(Length::Shrink, Length::Shrink).into();
    }
    pick_list(addable, None::<PickableKind>, move |picked| Message::BarAddModule(section, picked.0))
        .placeholder("+ Add module")
        .text_size(12)
        .style(move |_, _| iced::widget::pick_list::Style {
            background: Background::Color(colors.surface),
            text_color: colors.text,
            placeholder_color: colors.dim_text,
            handle_color: colors.dim_text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        })
        .menu_style(crate::app::pick_list_menu_style(colors))
        .width(180)
        .into()
}

/// A single module chip in the live preview — draggable to reorder within
/// or across sections. `on_press` starts the drag; `on_enter` marks this
/// chip's slot as the current drop target (iced has no drop-target
/// hit-test, so hovering is how the target slot is tracked while the
/// mouse button stays down elsewhere).
fn module_chip<'a>(colors: AppColors, section: usize, index: usize, kind: &ModuleKind, is_source: bool, is_target: bool) -> Element<'a, Message> {
    // Full name, not the old 3-letter abbreviation — "Blu" vs "Bluetooth"
    // isn't worth saving the width for when the whole point is being able
    // to tell modules apart while dragging them around.
    let label = kind_label(kind);

    let bg = if is_target && !is_source { colors.accent } else { colors.surface };
    let fg = if is_target && !is_source { colors.bar_bg } else { colors.text };
    let border_color = if is_source { colors.accent } else { colors.border };

    let chip = container(text(label).size(12).style(move |_| text::Style { color: Some(fg) }))
        .padding([6, 10])
        .style(move |_| container::Style {
            background: Some(Background::Color(if is_source { Color { a: 0.35, ..bg } } else { bg })),
            border: Border { color: border_color, width: if is_source { 2.0 } else { 1.0 }, radius: colors.radius.into() },
            ..Default::default()
        });

    mouse_area(chip)
        .on_press(Message::BarModuleDragStart(section, index))
        .on_enter(Message::BarModuleDragOver(section, index))
        .interaction(iced::mouse::Interaction::Grab)
        .into()
}

fn preview_section<'a>(
    colors: AppColors,
    vertical: bool,
    index: usize,
    sec: &'a BarSection,
    drag: Option<(usize, usize)>,
    drag_over: Option<(usize, usize)>,
) -> Element<'a, Message> {
    let tint = if index % 2 == 0 { colors.surface } else { Color { a: 0.5, ..colors.surface } };

    let mut chips: Vec<Element<'a, Message>> = sec
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let is_source = drag == Some((index, i));
            let is_target = drag_over == Some((index, i));
            module_chip(colors, index, i, &m.kind, is_source, is_target)
        })
        .collect();

    // Trailing drop zone so a chip can be dropped at the end of a section,
    // not just before an existing one — otherwise there'd be no way to
    // move something to the last slot.
    let end_index = sec.modules.len();
    let end_is_target = drag_over == Some((index, end_index));
    let end_zone = mouse_area(
        container(text("").size(1))
            .width(Length::Fixed(10.0))
            .height(Length::Fixed(14.0))
            .style(move |_| container::Style {
                background: if end_is_target { Some(Background::Color(colors.accent)) } else { None },
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            }),
    )
    .on_enter(Message::BarModuleDragOver(index, end_index));
    chips.push(end_zone.into());

    // Slim single-line track with a rounded pill scroller, instead of the
    // default chunky scrollbar — width(3) is the thin line, scroller_width(8)
    // is the fatter pill sitting on it. `spacing(6)` reserves a gap between
    // the chips and the bar instead of floating it as an overlay directly
    // on top of the last row of chips.
    let bar_thin = iced::widget::scrollable::Scrollbar::new().width(3.0).scroller_width(8.0).margin(2.0).spacing(6.0);
    let scrollbar_style = move |_: &iced::Theme, _status| iced::widget::scrollable::Style {
        container: container::Style::default(),
        vertical_rail: iced::widget::scrollable::Rail {
            background: Some(Background::Color(Color { a: 0.15, ..colors.text })),
            border: Border { radius: 2.0.into(), ..Default::default() },
            scroller: iced::widget::scrollable::Scroller { color: colors.accent, border: Border { radius: 4.0.into(), ..Default::default() } },
        },
        horizontal_rail: iced::widget::scrollable::Rail {
            background: Some(Background::Color(Color { a: 0.15, ..colors.text })),
            border: Border { radius: 2.0.into(), ..Default::default() },
            scroller: iced::widget::scrollable::Scroller { color: colors.accent, border: Border { radius: 4.0.into(), ..Default::default() } },
        },
        gap: None,
    };

    // Chips stack along whichever axis the bar itself runs on — a
    // vertical (Left/Right-edge) bar's sections scroll their modules
    // top-to-bottom, a horizontal one scrolls left-to-right.
    let chip_strip: Element<'a, Message> = if vertical {
        iced::widget::scrollable(column(chips).spacing(6).align_x(iced::Alignment::Center))
            .direction(iced::widget::scrollable::Direction::Vertical(bar_thin))
            .style(scrollbar_style)
            .into()
    } else {
        iced::widget::scrollable(row(chips).spacing(6).align_y(iced::Alignment::Center))
            .direction(iced::widget::scrollable::Direction::Horizontal(bar_thin))
            .style(scrollbar_style)
            .into()
    };

    let block = container(chip_strip)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(4)
        .style(move |_| container::Style {
            background: Some(Background::Color(tint)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        });

    let portion = sec.percent.max(1) as u16;
    if vertical {
        block.width(Length::Fill).height(Length::FillPortion(portion)).into()
    } else {
        block.width(Length::FillPortion(portion)).height(Length::Fill).into()
    }
}

fn preview_divider(colors: AppColors, vertical: bool, handle: usize) -> Element<'static, Message> {
    let bar = container(text(""))
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.accent)),
            ..Default::default()
        });
    let bar: Element<'static, Message> = if vertical {
        bar.width(Length::Fill).height(Length::Fixed(PREVIEW_HANDLE)).into()
    } else {
        bar.width(Length::Fixed(PREVIEW_HANDLE)).height(Length::Fill).into()
    };
    mouse_area(bar).on_press(Message::BarDividerDragStart(handle)).into()
}

/// Live, to-scale mock of the bar: sections sized by their `percent` share
/// (same `FillPortion` math og-bar itself uses, see og-bar's `bar.rs`), with
/// draggable handles between them to resize a section by eye, and each
/// module rendered as its own chip that can be dragged to reorder within
/// or move across sections.
fn bar_preview<'a>(colors: AppColors, bar_config: &'a BarConfig, drag: Option<(usize, usize)>, drag_over: Option<(usize, usize)>, window_width: f32) -> Element<'a, Message> {
    let vertical = matches!(bar_config.position, Edge::Left | Edge::Right);

    let mut children: Vec<Element<'a, Message>> = Vec::new();
    for (i, sec) in bar_config.sections.iter().enumerate() {
        if i > 0 {
            children.push(preview_divider(colors, vertical, i - 1));
        }
        children.push(preview_section(colors, vertical, i, sec, drag, drag_over));
    }
    if children.is_empty() {
        children.push(
            text("No sections yet — add one below.")
                .size(12)
                .style(move |_| text::Style { color: Some(Color { a: 0.6, ..colors.text }) })
                .into(),
        );
    }

    // The row/column's own length must be `Fixed`, not the default `Shrink`
    // — iced's flex layout zeroes out `FillPortion` children whenever the
    // parent's main-axis length is `Shrink` (it has no "extra" space to
    // divide up), so setting only the wrapping container's size below
    // would leave every section collapsed to nothing.
    let strip: Element<'a, Message> = if vertical {
        column(children).width(Length::Fixed(PREVIEW_THICKNESS_VERTICAL)).height(Length::Fixed(PREVIEW_LENGTH)).into()
    } else {
        row(children).width(Length::Fixed(horizontal_preview_length(window_width))).height(Length::Fixed(PREVIEW_THICKNESS)).into()
    };

    container(strip)
        .style(move |_| container::Style {
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .into()
}

fn section_block(
    colors: AppColors,
    position: Edge,
    index: usize,
    sec: &BarSection,
    drag: Option<usize>,
    drag_over: Option<usize>,
) -> Element<'static, Message> {
    let present: Vec<ModuleKind> = sec.modules.iter().map(|m| m.kind.clone()).collect();
    let rows: Vec<Element<Message>> = sec
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| module_toggle_row(colors, index, i, &m.kind, m.enabled))
        .collect();

    let align_row = row![
        align_button(colors, SectionAlign::Start.label(position), SectionAlign::Start, sec.align, index),
        align_button(colors, SectionAlign::Middle.label(position), SectionAlign::Middle, sec.align, index),
        align_button(colors, SectionAlign::End.label(position), SectionAlign::End, sec.align, index),
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
        .on_press(Message::BarRemoveSection(index));

    let is_source = drag == Some(index);
    let is_target = drag_over == Some(index) && !is_source;

    // Drag handle is just the label + grip glyph, not the whole header row
    // — the "Remove section" button sits in the same row and needs its own
    // clicks, so the draggable area stops short of it instead of wrapping
    // the row (and the button) in one mouse_area.
    let handle = mouse_area(
        row![
            text("\u{22ee}").size(13).style(move |_| text::Style { color: Some(Color { a: 0.5, ..colors.text }) }),
            text(format!("Section {}", index + 1)).size(12).style(move |_| text::Style { color: Some(Color { a: 0.7, ..colors.text }) }),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center),
    )
    .on_press(Message::BarSectionDragStart(index))
    .interaction(iced::mouse::Interaction::Grab);

    let header = row![handle, iced::widget::horizontal_space(), remove_section_btn].align_y(iced::Alignment::Center);

    let card = container(
        column![
            header,
            align_row,
            column(rows).spacing(4),
            add_module_row(colors, index, &present),
        ]
        .spacing(8)
        .padding(10),
    )
    .style(move |_| container::Style {
        background: if is_source { Some(Background::Color(Color { a: 0.4, ..colors.sec_bg })) } else { None },
        border: Border { color: if is_target { colors.accent } else { colors.border }, width: if is_target { 2.0 } else { 1.0 }, radius: colors.radius.into() },
        ..Default::default()
    })
    .width(Length::Fill);

    mouse_area(card).on_enter(Message::BarSectionDragOver(index)).into()
}

pub fn view<'a>(
    bar_config: &'a BarConfig,
    colors: AppColors,
    module_drag: Option<(usize, usize)>,
    module_drag_over: Option<(usize, usize)>,
    section_drag: Option<usize>,
    section_drag_over: Option<usize>,
    window_width: f32,
    available_outputs: &'a [String],
) -> Element<'a, Message> {
    let edge_row = row![
        edge_button(colors, "Top", Edge::Top, bar_config.position),
        edge_button(colors, "Bottom", Edge::Bottom, bar_config.position),
        edge_button(colors, "Left", Edge::Left, bar_config.position),
        edge_button(colors, "Right", Edge::Right, bar_config.position),
    ]
    .spacing(6);

    // "All screens" plus one entry per connected output — picking a
    // specific output pins og-bar to just that one (StartMode::
    // TargetScreen), so there's only ever one surface and no
    // cross-output focus interaction to worry about at all.
    const ALL_SCREENS: &str = "All screens";
    let output_options: Vec<String> = std::iter::once(ALL_SCREENS.to_string())
        .chain(available_outputs.iter().cloned())
        .collect();
    let current_output = match &bar_config.output_mode {
        og_config::BarOutputMode::AllScreens => ALL_SCREENS.to_string(),
        og_config::BarOutputMode::SingleOutput(name) => name.clone(),
    };
    let output_mode_row = row![
        text("Show on").size(13).style(move |_| text::Style { color: Some(colors.text) }),
        pick_list(output_options, Some(current_output), |v: String| {
            Message::BarSetOutputMode(if v == ALL_SCREENS { None } else { Some(v) })
        })
        .style(move |_, _| iced::widget::pick_list::Style {
            text_color: colors.text,
            placeholder_color: colors.text,
            handle_color: colors.text,
            background: Background::Color(colors.surface),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        }),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let auto_hide_row = row![
        toggler(bar_config.auto_hide).size(16).on_toggle(Message::BarSetAutoHide),
        text("Auto-hide (reveal on hover)").size(13).style(move |_| text::Style { color: Some(colors.text) }),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let sliders = column![
        labeled_slider(colors, "Thickness", bar_config.thickness, 16..=96, Message::BarSetThickness),
        labeled_slider(colors, "Item size", bar_config.item_size, 16..=80, Message::BarSetItemSize),
        labeled_slider(colors, "Spacing", bar_config.spacing, 0..=32, Message::BarSetSpacing),
        labeled_slider(colors, "Padding", bar_config.padding, 0..=32, Message::BarSetPadding),
    ]
    .spacing(10);

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
        .on_press(Message::BarAddSection);

    let sections = column(
        bar_config
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| section_block(colors, bar_config.position, i, s, section_drag, section_drag_over))
            .chain(std::iter::once(add_section_btn.into())),
    )
    .spacing(12);

    container(
        column![
            text("Bar edge").size(13).style(move |_| text::Style { color: Some(Color { a: 0.7, ..colors.text }) }),
            edge_row,
            output_mode_row,
            auto_hide_row,
            sliders,
            text("Sections").size(13).style(move |_| text::Style { color: Some(Color { a: 0.7, ..colors.text }) }),
            text("Each section gets a share of the bar's length (not required to sum to 100 — it's a relative split) and packs its modules toward the start, middle, or end of that share.")
                .size(11)
                .style(move |_| text::Style { color: Some(Color { a: 0.5, ..colors.text }) }),
            text("Drag a handle to resize the two sections it sits between. Drag a module chip to reorder it or move it into a different section. Drag a section's \u{22ee} header to reorder sections.")
                .size(11)
                .style(move |_| text::Style { color: Some(Color { a: 0.5, ..colors.text }) }),
            container(bar_preview(colors, bar_config, module_drag, module_drag_over, window_width)).center_x(Length::Fill).padding([8, 0]),
            sections,
            text("Apply & Save restarts og-bar automatically to pick up any change here.")
                .size(11)
                .style(move |_| text::Style { color: Some(Color { a: 0.5, ..colors.text }) }),
        ]
        .spacing(16)
        .padding(4),
    )
    .width(Length::Fill)
    .into()
}
