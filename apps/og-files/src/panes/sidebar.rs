use iced::widget::{button, column, container, progress_bar, row, scrollable, text};
use iced::{Background, Border, Element, Length, Padding};
use std::path::PathBuf;

use crate::app::Message;
use crate::devices::Device;
use crate::filesystem::{format_size, home_dir, DriveInfo};
use crate::theme;

const ICON_FONT: iced::Font = iced::Font::with_name("Symbols Nerd Font");

pub fn bookmarks() -> Vec<(&'static str, &'static str, PathBuf)> {
    let home = home_dir();
    vec![
        ("\u{f015}", "Home", home.clone()),
        ("\u{f108}", "Desktop", home.join("Desktop")),
        ("\u{f019}", "Downloads", home.join("Downloads")),
        ("\u{f0f6}", "Documents", home.join("Documents")),
        ("\u{f1c5}", "Pictures", home.join("Pictures")),
        ("\u{f001}", "Music", home.join("Music")),
        ("\u{f1c8}", "Videos", home.join("Videos")),
        ("\u{f1f8}", "Trash", home.join(".local/share/Trash/files")),
    ]
}

fn section_label(label: &str) -> Element<'_, Message> {
    container(text(label).size(11).style(theme::muted_text))
        .padding(Padding { top: 10.0, bottom: 4.0, left: 8.0, right: 8.0 })
        .into()
}

fn nav_button<'a>(icon: &'a str, label: String, path: PathBuf, is_active: bool) -> Element<'a, Message> {
    let pal = theme::p();
    let content = row![
        container(text(icon).font(ICON_FONT).size(14).style(move |_| iced::widget::text::Style {
            color: Some(if is_active { pal.bg } else { pal.text }),
        }))
        .width(18),
        text(label).size(13),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    button(content)
        .on_press(Message::Navigate(path))
        .style(move |_, status| {
            let bg = if is_active {
                pal.accent
            } else {
                match status {
                    button::Status::Hovered => pal.surface,
                    _ => pal.sec_bg,
                }
            };
            button::Style {
                background: Some(Background::Color(bg)),
                text_color: if is_active { pal.bg } else { pal.text },
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            }
        })
        .width(Length::Fill)
        .into()
}

fn drive_row<'a>(icon: &'a str, label: &'a str, used: u64, total: u64, on_click: Option<Message>, is_active: bool, actions: Element<'a, Message>) -> Element<'a, Message> {
    let pal = theme::p();
    let fraction = if total == 0 { 0.0 } else { used as f32 / total as f32 };

    let header = row![
        container(text(icon).font(ICON_FONT).size(14).style(move |_| iced::widget::text::Style {
            color: Some(if is_active { pal.bg } else { pal.text }),
        }))
        .width(18),
        text(label).size(13).width(Length::Fill),
        actions,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);

    let mut col = column![header].spacing(4).width(Length::Fill);

    if total > 0 {
        let usage_text = text(format!("{} / {}", format_size(used), format_size(total)))
            .size(10)
            .style(move |_| iced::widget::text::Style { color: Some(if is_active { pal.bg } else { pal.muted }) });
        let bar = progress_bar(0.0..=1.0, fraction).height(4).style(move |_| progress_bar::Style {
            background: Background::Color(if is_active { iced::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.15 } } else { iced::Color { r: 1.0, g: 1.0, b: 1.0, a: 0.08 } }),
            bar: Background::Color(if is_active { pal.bg } else { pal.accent }),
            border: Border { radius: 2.0.into(), ..Default::default() },
        });
        col = col.push(bar);
        col = col.push(usage_text);
    }

    let inner = container(col).padding([6, 8]).style(move |_| iced::widget::container::Style {
        background: Some(Background::Color(if is_active { pal.accent } else { pal.sec_bg })),
        border: Border { radius: 4.0.into(), ..Default::default() },
        ..Default::default()
    });

    if let Some(msg) = on_click {
        button(inner)
            .on_press(msg)
            .padding(0)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(match status {
                    button::Status::Hovered => pal.surface,
                    _ => iced::Color::TRANSPARENT,
                })),
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            })
            .width(Length::Fill)
            .into()
    } else {
        inner.width(Length::Fill).into()
    }
}

fn icon_action_btn(icon: &'static str, msg: Message, tint_danger: bool) -> Element<'static, Message> {
    let pal = theme::p();
    button(text(icon).font(ICON_FONT).size(12))
        .on_press(msg)
        .padding(4)
        .style(move |_, status| button::Style {
            background: Some(Background::Color(match status {
                button::Status::Hovered if tint_danger => iced::Color { a: 0.3, ..pal.danger },
                button::Status::Hovered => pal.surface,
                _ => iced::Color::TRANSPARENT,
            })),
            text_color: pal.text,
            border: Border { radius: 3.0.into(), ..Default::default() },
            ..Default::default()
        })
        .into()
}

#[allow(clippy::too_many_arguments)]
pub fn view<'a>(
    current_path: &PathBuf,
    fixed_devices: &'a [DriveInfo],
    network: &'a [DriveInfo],
    removable: &'a [Device],
    recent: &'a [PathBuf],
    user_bookmarks: &'a [PathBuf],
    width: f32,
    drop_target: Option<&'a PathBuf>,
) -> Element<'a, Message> {
    let mut col = column![].spacing(2).padding([8, 4]);

    col = col.push(section_label("Places"));
    for (icon, label, path) in bookmarks() {
        let is_active = current_path == &path;
        col = col.push(sidebar_drop_wrap(nav_button(icon, label.to_string(), path.clone(), is_active), &path, drop_target));
    }

    col = col.push(section_label("Bookmarks"));
    if user_bookmarks.is_empty() {
        col = col.push(container(text("Right-click a folder to bookmark it").size(11).style(theme::muted_text)).padding([2, 8]));
    }
    for path in user_bookmarks {
        let label = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string_lossy().to_string());
        let is_active = current_path == path;
        col = col.push(sidebar_drop_wrap(nav_button("\u{f02e}", label, path.clone(), is_active), path, drop_target));
    }

    col = col.push(section_label("Devices"));
    if fixed_devices.is_empty() && removable.is_empty() {
        col = col.push(container(text("No devices").size(12).style(theme::muted_text)).padding([2, 8]));
    }
    for drive in fixed_devices {
        let is_active = current_path == &drive.mount_point;
        let click = (!is_active).then(|| Message::Navigate(drive.mount_point.clone()));
        col = col.push(drive_row("\u{f0a0}", &drive.label, drive.used, drive.total, click, is_active, iced::widget::Space::with_width(0).into()));
    }
    for dev in removable {
        let is_active = dev.mount_point.as_ref() == Some(current_path);
        let actions: Element<Message> = if dev.is_mounted() {
            row![
                icon_action_btn("\u{f014}", Message::DeviceUnmount(dev.dev.clone()), false),
                icon_action_btn("\u{f050}", Message::DeviceEject(dev.dev.clone()), false),
            ]
            .spacing(2)
            .into()
        } else {
            icon_action_btn("\u{f019}", Message::DeviceMount(dev.dev.clone()), false)
        };
        // Unmounted: clicking the row mounts it (Message::DeviceMounted
        // auto-navigates in once it succeeds — see app.rs). Mounted and
        // not the active folder: clicking navigates straight in, same as
        // any other drive. Mounted and active: no click target, matches
        // every other row here.
        let click = if is_active {
            None
        } else if dev.is_mounted() {
            dev.mount_point.clone().map(Message::Navigate)
        } else {
            Some(Message::DeviceMount(dev.dev.clone()))
        };
        col = col.push(drive_row("\u{f287}", &dev.label, 0, 0, click, is_active, actions));
    }

    col = col.push(section_label("Network"));
    if network.is_empty() {
        col = col.push(container(text("No network drives").size(12).style(theme::muted_text)).padding([2, 8]));
    }
    for drive in network {
        let is_active = current_path == &drive.mount_point;
        let click = (!is_active).then(|| Message::Navigate(drive.mount_point.clone()));
        col = col.push(drive_row("\u{f0ac}", &drive.label, drive.used, drive.total, click, is_active, iced::widget::Space::with_width(0).into()));
    }

    col = col.push(section_label("Recent"));
    if recent.is_empty() {
        col = col.push(container(text("No recent places").size(12).style(theme::muted_text)).padding([2, 8]));
    }
    for path in recent {
        let label = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string_lossy().to_string());
        let is_active = current_path == path;
        col = col.push(nav_button("\u{f017}", label, path.clone(), is_active));
    }

    container(scrollable(col))
        .width(width)
        .height(Length::Fill)
        .style(theme::panel)
        .into()
}

/// Wraps a places/bookmark row so it can also act as an in-window drop
/// target — dragging a file onto "Downloads" in the sidebar moves it there
/// without navigating first.
fn sidebar_drop_wrap<'a>(inner: Element<'a, Message>, path: &PathBuf, drop_target: Option<&'a PathBuf>) -> Element<'a, Message> {
    let is_target = drop_target == Some(path);
    let path_enter = path.clone();
    let path_exit = path.clone();
    let wrapped = iced::widget::mouse_area(inner)
        .on_enter(Message::EntryHoverEnter(path_enter))
        .on_exit(Message::EntryHoverExit(path_exit));
    if is_target {
        container(wrapped)
            .style(move |_| iced::widget::container::Style {
                border: Border { color: theme::p().accent, width: 2.0, radius: 4.0.into() },
                ..Default::default()
            })
            .into()
    } else {
        wrapped.into()
    }
}
