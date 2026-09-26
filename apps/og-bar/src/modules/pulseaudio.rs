use iced::widget::{column, container, mouse_area, text};
use iced::{Background, Border, Color, Element, Length, Subscription};

use crate::icon_font;
use crate::message::Message;
use crate::module::{Module, Orientation};
use og_theme::AppColors;

pub fn read_volume_pct() -> Option<u32> {
    let out = std::process::Command::new("pactl")
        .args(["get-sink-volume", "@DEFAULT_SINK@"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    // e.g. "Volume: front-left: 45000 /  69% / -8.31 dB, ..."
    let pct_str = text.split('/').nth(1)?.trim().trim_end_matches('%');
    pct_str.trim().parse().ok()
}

pub fn read_muted() -> bool {
    std::process::Command::new("pactl")
        .args(["get-sink-mute", "@DEFAULT_SINK@"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.contains("yes"))
        .unwrap_or(false)
}

pub struct Pulseaudio {
    volume_pct: u32,
    muted: bool,
    hovered: bool,
}

impl Pulseaudio {
    pub fn new() -> Self {
        Self { volume_pct: 0, muted: false, hovered: false }
    }
}

impl Module for Pulseaudio {
    fn view(&self, colors: AppColors, size: u32, _orientation: Orientation) -> Element<'_, Message> {
        let fg = if self.muted { Color::WHITE } else { colors.text };
        // Icon glyph and the percentage text can't share one Text widget —
        // Symbols Nerd Font has no ASCII/digit glyphs at all, so a joined
        // string tofu's everything but the icon. Two widgets, two fonts,
        // stacked. Percentage stays visible even muted — mute state is
        // the icon's job, hiding the number too just loses information.
        let icon = if self.muted { "\u{f0581}" } else { "\u{f057e}" };
        let sub = format!("{}%", self.volume_pct);
        mouse_area(
            container(
                container(
                    column![
                        text(icon).size(14).font(icon_font::nerd_font()).align_x(iced::alignment::Horizontal::Center).style(move |_| text::Style { color: Some(fg) }),
                        text(sub).size(11).align_x(iced::alignment::Horizontal::Center).style(move |_| text::Style { color: Some(fg) }),
                    ]
                    .align_x(iced::Alignment::Center),
                )
                .width(size as u16)
                .height(size as u16)
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            )
            .style(move |_| container::Style {
                border: Border { radius: colors.radius.into(), ..Default::default() },
                // Muted red wins over hover — same danger-red og-settings'
                // Audio tab uses for its mute button, so a muted state
                // reads the same way from either app.
                background: Some(Background::Color(if self.muted {
                    Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 }
                } else if self.hovered {
                    colors.header_btn_bg
                } else {
                    Color::TRANSPARENT
                })),
                ..Default::default()
            }),
        )
        .on_press(Message::PulseaudioToggleMute)
        .on_right_press(Message::Launch("og-settings --tab audio".to_string()))
        .on_enter(Message::PulseaudioHover(true))
        .on_exit(Message::PulseaudioHover(false))
        .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with_id("pulseaudio", pulseaudio_stream())
    }

    fn update(&mut self, message: &Message) {
        if let Message::PulseaudioState(volume_pct, muted) = message {
            self.volume_pct = *volume_pct;
            self.muted = *muted;
        }
        if let Message::PulseaudioHover(v) = message {
            self.hovered = *v;
        }
    }
}

/// Event-driven instead of polling: `pactl subscribe` prints a line for
/// every server-side change, and only sink/server events (volume, mute,
/// default-sink switch) trigger a re-read. Idle audio = zero work.
fn pulseaudio_stream() -> impl iced::futures::Stream<Item = Message> {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::process::Command;

    iced::stream::channel(4, |mut sender| async move {
        use iced::futures::SinkExt;

        let read = || tokio::task::spawn_blocking(|| (read_volume_pct(), read_muted()));
        let mut last: Option<(u32, bool)> = None;

        loop {
            let mut command = Command::new("pactl");
            command.arg("subscribe").stdout(Stdio::piped()).kill_on_drop(true);
            unsafe {
                command.pre_exec(|| {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    Ok(())
                });
            }
            let child = command.spawn();

            // Read once per (re)connect so a sound-server restart can't
            // leave a stale value on the bar.
            let mut changed = true;
            let mut lines = child
                .ok()
                .and_then(|mut c| c.stdout.take().map(|out| (c, BufReader::new(out).lines())));

            loop {
                if changed {
                    if let Ok((Some(volume), muted)) = read().await {
                        if last != Some((volume, muted)) {
                            last = Some((volume, muted));
                            if sender.send(Message::PulseaudioState(volume, muted)).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                let Some((_child, reader)) = lines.as_mut() else { break };
                match reader.next_line().await {
                    Ok(Some(line)) => {
                        // e.g. "Event 'change' on sink #56" — but not
                        // "sink-input", which fires for every app stream.
                        changed = line.contains(" on sink #") || line.contains(" on server")
                    }
                    _ => break,
                }
            }

            // pactl missing or the sound server went away: retry shortly.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
    })
}
