use iced::widget::canvas::{self, Frame, Geometry};
use iced::{Color, Element, Length, Pixels, Rectangle, Renderer, Theme};
use iced::mouse;

use crate::app::{AppColors, Message, ModuleDrag};
use crate::config::Config;

pub const CANVAS_WIDTH: f32 = 660.0;
pub const MIN_CANVAS_HEIGHT: f32 = 240.0;
/// Above this many chips in the tallest column, the arrange card scrolls
/// instead of growing further — otherwise a busy section pushes the rest
/// of the Theme tab down indefinitely.
const MAX_VIEWPORT_HEIGHT: f32 = 320.0;
const COL_GAP: f32 = 12.0;
const HEADER_H: f32 = 28.0;
const CHIP_H: f32 = 28.0;
const CHIP_GAP: f32 = 6.0;
/// Width of the "×" remove hit-zone at the right edge of each chip.
const REMOVE_ZONE_W: f32 = 22.0;

/// Modules safe to add from the palette below the canvas: native waybar
/// module types render fine with zero extra config, and the `custom/*`
/// ones all have a hand-authored behavior block already in
/// `~/.config/waybar/config` (on-click/exec/etc) — anything else would show
/// up blank since custom modules have no meaning without that block.
pub const ADDABLE_MODULES: &[&str] = &[
    "sway/workspaces", "tray", "clock", "cpu", "memory", "network", "bluetooth", "pulseaudio", "backlight",
    "custom/sysctl", "custom/claude", "custom/settings", "custom/og-apps", "custom/terminal",
    "custom/power", "custom/clipboard", "custom/notifications",
];

pub fn available_to_add(config: &Config) -> Vec<&'static str> {
    ADDABLE_MODULES.iter().filter(|id| column_of(config, id).is_none()).copied().collect()
}

fn col_w() -> f32 {
    (CANVAS_WIDTH - COL_GAP * 2.0) / 3.0
}

/// The canvas grows to fit the tallest of the three columns instead of
/// clipping/overlapping chips once a section gets long; `view()` wraps it
/// in a scrollable so growth doesn't push the rest of the tab down forever.
fn canvas_height(config: &Config) -> f32 {
    let max_rows = [config.modules_left.len(), config.modules_center.len(), config.modules_right.len()]
        .into_iter()
        .max()
        .unwrap_or(0);
    let content_h = HEADER_H + max_rows as f32 * (CHIP_H + CHIP_GAP) + 12.0;
    content_h.max(MIN_CANVAS_HEIGHT)
}

/// Friendly label for a raw waybar module id. Falls back to the id itself
/// for anything not explicitly known (custom modules, etc).
pub fn display_name(id: &str, config: &Config) -> String {
    match id {
        "sway/workspaces" => "Workspaces".into(),
        "custom/sysctl" => "System Control".into(),
        "custom/claude" => "Claude".into(),
        "custom/settings" => "Settings".into(),
        "cpu" => "CPU".into(),
        "memory" => "Memory".into(),
        "tray" => "Tray".into(),
        "bluetooth" => "Bluetooth".into(),
        "network" => "Network".into(),
        "pulseaudio" => "Volume".into(),
        "clock" => "Clock".into(),
        other if other.starts_with("clock#") => {
            let suffix = &other["clock#".len()..];
            let tz = suffix.parse::<u32>().ok()
                .and_then(|id| config.extra_clocks.iter().find(|c| c.id == id))
                .map(|c| c.timezone.as_str())
                .unwrap_or("?");
            format!("Clock ({tz})")
        }
        other => other.to_string(),
    }
}

fn column_of(config: &Config, name: &str) -> Option<usize> {
    if config.modules_left.iter().any(|m| m == name) { return Some(0); }
    if config.modules_center.iter().any(|m| m == name) { return Some(1); }
    if config.modules_right.iter().any(|m| m == name) { return Some(2); }
    None
}

fn list_for<'a>(config: &'a Config, section: usize) -> &'a [String] {
    match section {
        0 => &config.modules_left,
        1 => &config.modules_center,
        _ => &config.modules_right,
    }
}

/// Chip position (top-left corner) for a module currently at `idx` within
/// its section's list, in idle (non-dragging) layout.
fn chip_pos(section: usize, idx: usize) -> (f32, f32) {
    let x = section as f32 * (col_w() + COL_GAP);
    let y = HEADER_H + idx as f32 * (CHIP_H + CHIP_GAP);
    (x, y)
}

/// Given a cursor position, figure out which section/index a dropped chip
/// should land in. Pure function shared by drawing (drop indicator) and by
/// the app's `ModuleDragEnd` handler (actual commit).
pub fn drop_target(config: &Config, cursor_x: f32, cursor_y: f32) -> (usize, usize) {
    let section = ((cursor_x / (col_w() + COL_GAP)).floor().max(0.0) as usize).min(2);
    let list = list_for(config, section);
    let y_rel = (cursor_y - HEADER_H).max(0.0);
    let idx = ((y_rel / (CHIP_H + CHIP_GAP)).round() as usize).min(list.len());
    (section, idx)
}

pub struct ArrangeCanvas<'a> {
    pub config: &'a Config,
    pub colors: AppColors,
    pub dragging: Option<&'a ModuleDrag>,
}

impl<'a> ArrangeCanvas<'a> {
    fn hit_at(&self, px: f32, py: f32) -> Option<String> {
        for section in 0..3 {
            for (idx, name) in list_for(self.config, section).iter().enumerate() {
                let (x, y) = chip_pos(section, idx);
                if px >= x && px <= x + col_w() && py >= y && py <= y + CHIP_H {
                    return Some(name.clone());
                }
            }
        }
        None
    }

    /// The small "×" zone at the right edge of a chip — checked before
    /// `hit_at` so clicking it removes instead of starting a drag.
    fn hit_remove_at(&self, px: f32, py: f32) -> Option<String> {
        for section in 0..3 {
            for (idx, name) in list_for(self.config, section).iter().enumerate() {
                let (x, y) = chip_pos(section, idx);
                let zone_x = x + col_w() - REMOVE_ZONE_W;
                if px >= zone_x && px <= x + col_w() && py >= y && py <= y + CHIP_H {
                    return Some(name.clone());
                }
            }
        }
        None
    }
}

impl<'a> canvas::Program<Message> for ArrangeCanvas<'a> {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> (canvas::event::Status, Option<Message>) {
        let local = cursor.position_in(bounds);

        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(pos) = local {
                    if let Some(name) = self.hit_remove_at(pos.x, pos.y) {
                        return (canvas::event::Status::Captured, Some(Message::ModuleRemove(name)));
                    }
                    if let Some(name) = self.hit_at(pos.x, pos.y) {
                        let section = column_of(self.config, &name).unwrap_or(0);
                        let idx = list_for(self.config, section).iter().position(|m| m == &name).unwrap_or(0);
                        let (bx, by) = chip_pos(section, idx);
                        return (canvas::event::Status::Captured,
                            Some(Message::ModuleDragStart(name, pos.x - bx, pos.y - by, pos.x, pos.y)));
                    }
                }
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if self.dragging.is_some() {
                    let cx = local.map(|p| p.x).unwrap_or_else(|| {
                        cursor.position().map(|p| (p.x - bounds.x).clamp(0.0, bounds.width)).unwrap_or(0.0)
                    });
                    let cy = local.map(|p| p.y).unwrap_or_else(|| {
                        cursor.position().map(|p| (p.y - bounds.y).clamp(0.0, bounds.height)).unwrap_or(0.0)
                    });
                    return (canvas::event::Status::Captured, Some(Message::ModuleDragMove(cx, cy)));
                }
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if self.dragging.is_some() {
                    return (canvas::event::Status::Captured, Some(Message::ModuleDragEnd));
                }
            }
            _ => {}
        }
        (canvas::event::Status::Ignored, None)
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let colors = self.colors;
        let mut frame = Frame::new(renderer, bounds.size());
        let bg = Color { r: colors.bar_bg.r * 0.6, g: colors.bar_bg.g * 0.6, b: colors.bar_bg.b * 0.6, a: 1.0 };
        frame.fill_rectangle(iced::Point::ORIGIN, bounds.size(), bg);

        let headers = ["Left", "Center", "Right"];
        for section in 0..3 {
            let x = section as f32 * (col_w() + COL_GAP);
            frame.fill_text(canvas::Text {
                content: headers[section].to_string(),
                position: iced::Point::new(x + col_w() / 2.0, 6.0),
                color: Color { a: 0.7, ..colors.text },
                size: Pixels(12.0),
                font: iced::Font::MONOSPACE,
                horizontal_alignment: iced::alignment::Horizontal::Center,
                vertical_alignment: iced::alignment::Vertical::Top,
                line_height: iced::widget::text::LineHeight::default(),
                shaping: iced::widget::text::Shaping::Basic,
            });
            // Column outline
            frame.fill_rectangle(
                iced::Point::new(x, HEADER_H),
                iced::Size::new(col_w(), bounds.height - HEADER_H - 8.0),
                Color { a: 0.08, ..colors.text },
            );
        }

        // Drop indicator while dragging
        if let Some(d) = self.dragging {
            let (section, idx) = drop_target(self.config, d.cursor_x, d.cursor_y);
            let (ix, iy) = chip_pos(section, idx);
            frame.fill_rectangle(
                iced::Point::new(ix + 2.0, iy - CHIP_GAP / 2.0 - 1.0),
                iced::Size::new(col_w() - 4.0, 2.0),
                colors.accent,
            );
        }

        for section in 0..3 {
            for (idx, name) in list_for(self.config, section).iter().enumerate() {
                let is_dragging = self.dragging.is_some_and(|d| &d.name == name);
                let (x, y) = if is_dragging {
                    let d = self.dragging.unwrap();
                    (d.cursor_x - d.off_x, d.cursor_y - d.off_y)
                } else {
                    chip_pos(section, idx)
                };
                let hovered = !is_dragging && cursor.position_in(bounds)
                    .map(|p| p.x >= x && p.x <= x + col_w() && p.y >= y && p.y <= y + CHIP_H)
                    .unwrap_or(false);

                let fill = if is_dragging {
                    colors.accent
                } else if hovered {
                    Color { r: colors.surface.r * 0.6 + colors.accent.r * 0.4, g: colors.surface.g * 0.6 + colors.accent.g * 0.4, b: colors.surface.b * 0.6 + colors.accent.b * 0.4, a: 1.0 }
                } else {
                    colors.surface
                };
                let bw = if is_dragging { 2.0f32 } else { 1.0 };
                frame.fill_rectangle(
                    iced::Point::new(x - bw, y - bw),
                    iced::Size::new(col_w() + bw * 2.0, CHIP_H + bw * 2.0),
                    Color { a: 0.35, ..colors.accent },
                );
                frame.fill_rectangle(iced::Point::new(x, y), iced::Size::new(col_w(), CHIP_H), fill);

                let label_color = if is_dragging { colors.bar_bg } else { colors.text };
                frame.fill_text(canvas::Text {
                    content: display_name(name, self.config),
                    position: iced::Point::new(x + 8.0, y + CHIP_H / 2.0),
                    color: label_color,
                    size: Pixels(12.0),
                    font: iced::Font::default(),
                    horizontal_alignment: iced::alignment::Horizontal::Left,
                    vertical_alignment: iced::alignment::Vertical::Center,
                    line_height: iced::widget::text::LineHeight::default(),
                    shaping: iced::widget::text::Shaping::Basic,
                });

                if !is_dragging {
                    let remove_hovered = cursor.position_in(bounds)
                        .map(|p| p.x >= x + col_w() - REMOVE_ZONE_W && p.x <= x + col_w() && p.y >= y && p.y <= y + CHIP_H)
                        .unwrap_or(false);
                    frame.fill_text(canvas::Text {
                        content: "×".to_string(),
                        position: iced::Point::new(x + col_w() - REMOVE_ZONE_W / 2.0, y + CHIP_H / 2.0),
                        color: if remove_hovered { colors.accent } else { Color { a: 0.5, ..colors.text } },
                        size: Pixels(15.0),
                        font: iced::Font::default(),
                        horizontal_alignment: iced::alignment::Horizontal::Center,
                        vertical_alignment: iced::alignment::Vertical::Center,
                        line_height: iced::widget::text::LineHeight::default(),
                        shaping: iced::widget::text::Shaping::Basic,
                    });
                }
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(&self, _state: &(), bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        if self.dragging.is_some() {
            return mouse::Interaction::Grabbing;
        }
        if let Some(pos) = cursor.position_in(bounds) {
            if self.hit_remove_at(pos.x, pos.y).is_some() {
                return mouse::Interaction::Pointer;
            }
            if self.hit_at(pos.x, pos.y).is_some() {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::default()
    }
}

pub fn view<'a>(config: &'a Config, colors: AppColors, dragging: Option<&'a ModuleDrag>) -> Element<'a, Message> {
    let height = canvas_height(config);
    let canvas = iced::widget::canvas(ArrangeCanvas { config, colors, dragging })
        .width(Length::Fill)
        .height(height);

    let canvas_el: Element<Message> = if height > MAX_VIEWPORT_HEIGHT {
        iced::widget::scrollable(canvas)
            .height(Length::Fixed(MAX_VIEWPORT_HEIGHT))
            .into()
    } else {
        canvas.into()
    };

    let to_add = available_to_add(config);
    let palette: Element<Message> = if to_add.is_empty() {
        iced::widget::text("All available modules are already on the bar.")
            .size(12)
            .style(move |_| iced::widget::text::Style { color: Some(Color { a: 0.55, ..colors.text } ) })
            .into()
    } else {
        let chips: Vec<Element<Message>> = to_add.into_iter().map(|id| {
            let label = display_name(id, config);
            iced::widget::button(
                iced::widget::text(format!("+ {label}")).size(12)
                    .style(move |_| iced::widget::text::Style { color: Some(colors.text) })
            )
            .style(move |_, status| iced::widget::button::Style {
                background: Some(iced::Background::Color(match status {
                    iced::widget::button::Status::Hovered => colors.accent,
                    _ => colors.surface,
                })),
                text_color: colors.text,
                border: iced::Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .on_press(Message::ModuleAdd(id.to_string()))
            .padding([4, 10])
            .into()
        }).collect();
        iced::widget::row(chips).spacing(8).wrap().into()
    };

    iced::widget::column![
        canvas_el,
        iced::widget::text("Available modules — click to add to the Right column, then drag to place. Click the × on a chip above to remove it.")
            .size(11)
            .style(move |_| iced::widget::text::Style { color: Some(Color { a: 0.55, ..colors.text }) }),
        palette,
    ]
    .spacing(10)
    .into()
}
