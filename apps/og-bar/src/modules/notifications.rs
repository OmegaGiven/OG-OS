use iced::widget::{button, container, stack, text};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

/// Counts visible notifications the same way og-notif-center does: mako's
/// own history minus whatever's been locally hidden. The hidden-ids file is
/// the shared contract between the two (see og-notif-center's
/// `notif.rs`) — read here rather than duplicating og-notif-center's
/// dismiss/restore logic, since this module only ever needs a count.
fn unread_count() -> u32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let hidden_path = format!("{home}/.local/share/notification-hidden.json");
    let hidden: std::collections::HashSet<String> = std::fs::read_to_string(&hidden_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let Ok(out) = std::process::Command::new("makoctl").args(["history", "-j"]).output() else {
        return 0;
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return 0;
    };
    let Some(arr) = json.as_array() else { return 0 };

    arr.iter().filter(|entry| !hidden.contains(&entry.to_string())).count() as u32
}

pub struct Notifications {
    count: u32,
}

impl Notifications {
    pub fn new() -> Self {
        Self { count: 0 }
    }
}

impl Module for Notifications {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = colors.text;
        let icon = container(
            text("\u{f0f3}").size(16).font(icon_font::nerd_font()).style(move |_| text::Style { color: Some(fg) }),
        )
        .width(size as u16)
        .height(size as u16)
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        let content: Element<'_, Message> = if self.count > 0 {
            let badge_label = if self.count > 99 { "99+".to_string() } else { self.count.to_string() };
            let badge = container(text(badge_label).size(9).style(move |_| text::Style { color: Some(Color::WHITE) }))
                .padding([1, 4])
                .style(move |_| container::Style {
                    background: Some(Background::Color(colors.accent)),
                    border: Border { radius: 8.0.into(), ..Default::default() },
                    ..Default::default()
                });
            stack![icon, container(badge).align_right(Length::Fill).align_top(Length::Fill)].into()
        } else {
            icon.into()
        };

        button(content)
            .padding(0)
            .style(move |_, status| button::Style {
                background: Some(Background::Color(if matches!(status, button::Status::Hovered) {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                text_color: fg,
                ..Default::default()
            })
            // The `-launch` wrapper (not the raw binary) — it grabs a
            // compositor-level Escape bind for as long as the popup is
            // open, coordinated with og-search-launch's identical
            // mechanism via og-popup-escape-guard, so Escape closes this
            // even when it isn't the focused window.
            .on_press(Message::Launch("og-notif-center-launch".to_string()))
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with_id("notifications", notifications_stream())
    }

    fn update(&mut self, message: &Message) {
        if let Message::NotificationCount(count) = message {
            self.count = *count;
        }
    }
}

/// Event-driven instead of polling makoctl every 5 s. The count only
/// changes when:
/// - mako moves a notification into history — it broadcasts
///   `NotificationClosed` on the session bus (expired or dismissed);
/// - mako restarts (history is lost) — `NameOwnerChanged`;
/// - og-notif-center hides/clears entries — it rewrites the hidden-ids
///   file, watched with inotify.
/// `makoctl history -j` is still what gets counted, since its exact JSON is
/// the hidden-file contract shared with og-notif-center.
fn notifications_stream() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(4, |mut sender| async move {
        use iced::futures::stream::{self, BoxStream, StreamExt};
        use iced::futures::SinkExt;
        use zbus::message::Type;
        use zbus::{MatchRule, MessageStream};

        const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
        let mut sources: Vec<BoxStream<'static, ()>> = Vec::new();

        if let Ok(conn) = zbus::Connection::session().await {
            let rules = [
                MatchRule::builder()
                    .msg_type(Type::Signal)
                    .interface(NOTIFICATIONS)
                    .and_then(|b| b.member("NotificationClosed"))
                    .map(|b| b.build()),
                MatchRule::builder()
                    .msg_type(Type::Signal)
                    .sender("org.freedesktop.DBus")
                    .and_then(|b| b.member("NameOwnerChanged"))
                    .and_then(|b| b.arg(0, NOTIFICATIONS))
                    .map(|b| b.build()),
            ];
            for rule in rules.into_iter().flatten() {
                if let Ok(signals) = MessageStream::for_match_rule(rule, &conn, Some(16)).await {
                    sources.push(signals.map(|_| ()).boxed());
                }
            }
        }

        // Watch the directory, not the file: it may not exist yet, and a
        // file watch dies if it's ever replaced rather than rewritten.
        let home = std::env::var("HOME").unwrap_or_default();
        if let Ok(inotify) = inotify::Inotify::init() {
            use inotify::WatchMask;
            let dir = format!("{home}/.local/share");
            let mask = WatchMask::CLOSE_WRITE | WatchMask::MOVED_TO | WatchMask::DELETE;
            if inotify.watches().add(&dir, mask).is_ok() {
                if let Ok(events) = inotify.into_event_stream([0u8; 4096]) {
                    let hidden_file = std::ffi::OsStr::new("notification-hidden.json");
                    sources.push(
                        events
                            .filter_map(move |event| {
                                let hit = matches!(&event, Ok(e) if e.name.as_deref() == Some(hidden_file));
                                std::future::ready(hit.then_some(()))
                            })
                            .boxed(),
                    );
                }
            }
        }

        let mut events = stream::select_all(sources);
        let mut last = None;
        loop {
            if let Ok(count) = tokio::task::spawn_blocking(unread_count).await {
                if last != Some(count) {
                    last = Some(count);
                    if sender.send(Message::NotificationCount(count)).await.is_err() {
                        return;
                    }
                }
            }
            if events.next().await.is_none() {
                return;
            }
            // Coalesce bursts ("dismiss all" closes every notification at
            // once, one signal each) into a single re-read.
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            while let Some(Some(())) = iced::futures::FutureExt::now_or_never(events.next()) {}
        }
    })
}
