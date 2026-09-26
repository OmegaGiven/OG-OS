//! Native pavucontrol equivalent — all 5 of its tabs (Output/Input Devices,
//! Playback, Recording, Configuration), backed by `crate::audio`'s
//! `pactl -f json` parsing.

use iced::widget::{button, column, container, pick_list, row, slider, text};
use iced::{Background, Border, Color, Length};
use std::collections::HashMap;

use crate::app::{AppColors, Message};
use crate::audio::{AudioCard, AudioDevice, AudioSnapshot, AudioStream, AudioTarget};
use crate::audio_meter::Meter;
use crate::surround::{OutDev, Preset, StreamInfo, TestSignal, CHANNEL_LABELS};

/// Live peak meter bar — a thin filled track under a device's volume
/// slider. `pct` is 0.0-100.0 from `Meter::level_pct()`; `None` means no
/// meter is running for this device (wrong subtab, or `parec` failed to
/// spawn).
fn meter_bar<'a>(colors: AppColors, pct: Option<f32>) -> Element<'a> {
    let pct = pct.unwrap_or(0.0);
    // Green up to 70%, amber to 90%, red above — same "getting hot" ramp
    // as a VU meter, so clipping is obvious at a glance.
    let fill_color = if pct >= 90.0 {
        Color { r: 0.8, g: 0.15, b: 0.15, a: 1.0 }
    } else if pct >= 70.0 {
        Color { r: 0.85, g: 0.65, b: 0.1, a: 1.0 }
    } else {
        Color { r: 0.2, g: 0.7, b: 0.35, a: 1.0 }
    };

    // Tenths-of-a-percent FillPortion instead of whole percent — 100 steps
    // across a full-width bar was coarse enough to visibly step ("notch")
    // as the level moved; 1000 steps reads as continuous motion.
    let tenths = (pct * 10.0).round().clamp(0.0, 1000.0) as u16;

    // Fill gets its own radius (not just the outer track) so both its
    // ends — leading and trailing — read as rounded caps at any fill
    // level, instead of a square-cut rectangle sitting inside a rounded
    // track.
    let filled = container(text("").size(1)).width(Length::FillPortion(tenths.max(1))).height(Length::Fixed(6.0)).style(move |_| container::Style {
        background: Some(Background::Color(fill_color)),
        border: Border { radius: 3.0.into(), ..Default::default() },
        ..Default::default()
    });
    let empty = container(text("").size(1)).width(Length::FillPortion(1000 - tenths));

    container(row![filled, empty].height(Length::Fixed(6.0)))
        .width(Length::Fill)
        .clip(true)
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.surface)),
            border: Border { color: colors.border, width: 1.0, radius: 3.0.into() },
            ..Default::default()
        })
        .into()
}

/// Same danger-red already used for "Remove section" etc. elsewhere in this
/// app — reused here so a muted device reads as unambiguously "off" instead
/// of blending into the rest of the card.
const MUTE_RED: Color = Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 };

fn nerd_font() -> iced::Font {
    iced::Font::with_name("Symbols Nerd Font")
}

/// Single click toggles mute — same glyphs og-bar's own volume module uses
/// (`\u{f0581}` muted / `\u{f057e}` unmuted) so the two apps read as one
/// system instead of inventing a second icon language.
fn mute_button<'a>(colors: AppColors, muted: bool, target: AudioTarget) -> Element<'a> {
    let icon = if muted { "\u{f0581}" } else { "\u{f057e}" };
    let fg = if muted { Color::WHITE } else { colors.text };
    button(text(icon).size(16).font(nerd_font()).style(move |_| text::Style { color: Some(fg) }))
        .padding([6, 10])
        .style(move |_, status| iced::widget::button::Style {
            background: Some(Background::Color(if muted {
                MUTE_RED
            } else if matches!(status, iced::widget::button::Status::Hovered) {
                colors.surface
            } else {
                Color::TRANSPARENT
            })),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: fg,
            ..Default::default()
        })
        .on_press(Message::AudioMuteToggled(target, !muted))
        .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioSubTab {
    Output,
    Input,
    Playback,
    Recording,
    Configuration,
    Spatial,
}

impl AudioSubTab {
    pub const ALL: [AudioSubTab; 6] = [
        AudioSubTab::Output,
        AudioSubTab::Input,
        AudioSubTab::Playback,
        AudioSubTab::Recording,
        AudioSubTab::Configuration,
        AudioSubTab::Spatial,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            AudioSubTab::Output => "Output Devices",
            AudioSubTab::Input => "Input Devices",
            AudioSubTab::Playback => "Playback",
            AudioSubTab::Recording => "Recording",
            AudioSubTab::Configuration => "Configuration",
            AudioSubTab::Spatial => "Spatial",
        }
    }
}

/// Everything the Spatial subtab needs, gathered by `app.rs` so `view`'s
/// signature only grows by one argument.
pub struct SpatialUi<'a> {
    pub route: crate::surround::RouteMode,
    pub preset: Preset,
    pub error: Option<&'a str>,
    pub streams: &'a [StreamInfo],
    pub test_running: bool,
    /// localization soundstage
    pub stage_test_mode: bool,
    pub stage_reveal: Option<u8>,
    pub stage_guess: Option<u8>,
    pub game_target: Option<u8>,
    pub game_hits: u32,
    pub game_near: u32,
    pub game_rounds: u32,
    /// real output devices + which one the chain feeds
    pub outs: &'a [OutDev],
    pub out_selected: Option<OutDev>,
}

/// "Hear yourself" toggle — only meaningful for input devices (a source),
/// so `active` is `None` on the Output subtab and the button doesn't
/// render at all there. Same visual language as `mute_button`: an icon
/// button that fills solid when the loopback for *this* device is the one
/// currently running (only one can be active at a time — see
/// AudioMicMonitorToggled in app.rs).
fn monitor_button<'a>(colors: AppColors, active: bool, source_name: String) -> Element<'a> {
    // md-headphones / md-headphones_off — verified against this system's
    // actual SymbolsNerdFont-Regular.ttf cmap (0xf0910, tried first, turned
    // out to map to "md-tag_minus", not headphones at all).
    let icon = if active { "\u{f02cb}" } else { "\u{f07ce}" };
    let fg = if active { Color::WHITE } else { colors.text };
    button(text(icon).size(16).font(nerd_font()).style(move |_| text::Style { color: Some(fg) }))
        .padding([6, 10])
        .style(move |_, status| iced::widget::button::Style {
            background: Some(Background::Color(if active {
                colors.accent
            } else if matches!(status, iced::widget::button::Status::Hovered) {
                colors.surface
            } else {
                Color::TRANSPARENT
            })),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            text_color: fg,
            ..Default::default()
        })
        .on_press(Message::AudioMicMonitorToggled(source_name))
        .into()
}

fn device_card<'a>(
    colors: AppColors,
    dev: &'a AudioDevice,
    target: AudioTarget,
    meter_pct: Option<f32>,
    monitor_active: Option<bool>,
) -> Element<'a> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: if dev.is_default { colors.accent } else { colors.border }, width: if dev.is_default { 2.0 } else { 1.0 }, radius: colors.radius.into() },
        ..Default::default()
    };

    let default_btn: Element<'a> = if dev.is_default {
        text("Default").size(11).style(move |_| text::Style { color: Some(colors.accent) }).into()
    } else {
        button(text("Set as default").size(11).style(move |_| text::Style { color: Some(colors.text) }))
            .style(move |_, status| iced::widget::button::Style {
                background: Some(Background::Color(if matches!(status, iced::widget::button::Status::Hovered) { colors.accent } else { colors.surface })),
                text_color: colors.text,
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .on_press(Message::AudioSetDefault(target.clone()))
            .into()
    };

    let mute_target = target.clone();
    let vol_target = target.clone();

    let mut control_row = row![
        mute_button(colors, dev.mute, mute_target),
        slider(0..=150u32, dev.volume_pct, move |v| Message::AudioVolumeChanged(vol_target.clone(), v)).step(1u32),
        text(format!("{}%", dev.volume_pct)).size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(10);
    if let (Some(active), AudioTarget::Source(name)) = (monitor_active, &target) {
        control_row = control_row.push(monitor_button(colors, active, name.clone()));
    }

    container(
        column![
            row![
                text(dev.description.clone()).size(14).style(move |_| text::Style { color: Some(colors.text) }),
                iced::widget::horizontal_space(),
                default_btn,
            ]
            .align_y(iced::Alignment::Center)
            .spacing(8),
            control_row,
            meter_bar(colors, meter_pct),
        ]
        .spacing(6)
        .padding(12),
    )
    .style(card_style)
    .width(Length::Fill)
    .into()
}

fn stream_card<'a>(colors: AppColors, s: &'a AudioStream, target: AudioTarget) -> Element<'a> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };
    let mute_target = target.clone();
    let vol_target = target;

    container(
        column![
            text(s.app_name.clone()).size(14).style(move |_| text::Style { color: Some(colors.text) }),
            row![
                mute_button(colors, s.mute, mute_target),
                slider(0..=150u32, s.volume_pct, move |v| Message::AudioVolumeChanged(vol_target.clone(), v)).step(1u32),
                text(format!("{}%", s.volume_pct)).size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10),
        ]
        .spacing(6)
        .padding(12),
    )
    .style(card_style)
    .width(Length::Fill)
    .into()
}

fn card_config<'a>(colors: AppColors, card: &'a AudioCard) -> Element<'a> {
    let card_style = move |_: &_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    };

    let profile_rows: Vec<Element<'a>> = card
        .profiles
        .iter()
        .filter(|p| p.available)
        .map(|p| {
            let active = p.id == card.active_profile;
            let card_name = card.name.clone();
            let profile_id = p.id.clone();
            button(text(p.description.clone()).size(12).style(move |_| text::Style { color: Some(if active { colors.bar_bg } else { colors.text }) }))
                .width(Length::Fill)
                .padding([6, 10])
                .style(move |_, _| iced::widget::button::Style {
                    background: Some(Background::Color(if active { colors.accent } else { iced::Color::TRANSPARENT })),
                    text_color: if active { colors.bar_bg } else { colors.text },
                    border: Border { radius: colors.radius.into(), ..Default::default() },
                    ..Default::default()
                })
                .on_press(Message::AudioProfileSelected(card_name.clone(), profile_id.clone()))
                .into()
        })
        .collect();

    container(
        column![
            text(card.description.clone()).size(14).style(move |_| text::Style { color: Some(colors.text) }),
            column(profile_rows).spacing(2),
        ]
        .spacing(8)
        .padding(12),
    )
    .style(card_style)
    .width(Length::Fill)
    .into()
}

type Element<'a> = iced::Element<'a, Message>;

pub fn view<'a>(
    colors: AppColors,
    subtab: AudioSubTab,
    snapshot: &'a AudioSnapshot,
    meters: &'a HashMap<String, Meter>,
    mic_monitor: Option<&'a (String, u32)>,
    spatial: SpatialUi<'a>,
) -> Element<'a> {
    let tab_row: Vec<Element> = AudioSubTab::ALL
        .iter()
        .map(|t| {
            let active = *t == subtab;
            let t = *t;
            button(text(t.label()).size(13).style(move |_| text::Style { color: Some(if active { colors.bar_bg } else { colors.text }) }))
                .padding([8, 14])
                .style(move |_, _| iced::widget::button::Style {
                    background: Some(Background::Color(if active { colors.accent } else { colors.surface })),
                    text_color: if active { colors.bar_bg } else { colors.text },
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .on_press(Message::AudioSubTabSelected(t))
                .into()
        })
        .collect();

    let empty = |label: &'static str| -> Element<'static> {
        text(label).size(12).style(move |_| text::Style { color: Some(colors.dim_text) }).into()
    };

    let content: Element = match subtab {
        AudioSubTab::Output => {
            if snapshot.sinks.is_empty() {
                empty("No output devices found.")
            } else {
                column(snapshot.sinks.iter().map(|d| {
                    let pct = meters.get(&d.name).map(Meter::level_pct);
                    device_card(colors, d, AudioTarget::Sink(d.name.clone()), pct, None)
                }))
                .spacing(10)
                .into()
            }
        }
        AudioSubTab::Input => {
            if snapshot.sources.is_empty() {
                empty("No input devices found.")
            } else {
                column(snapshot.sources.iter().map(|d| {
                    let pct = meters.get(&d.name).map(Meter::level_pct);
                    let active = mic_monitor.is_some_and(|(name, _)| *name == d.name);
                    device_card(colors, d, AudioTarget::Source(d.name.clone()), pct, Some(active))
                }))
                .spacing(10)
                .into()
            }
        }
        AudioSubTab::Playback => {
            if snapshot.sink_inputs.is_empty() {
                empty("Nothing is currently playing audio.")
            } else {
                column(snapshot.sink_inputs.iter().map(|s| stream_card(colors, s, AudioTarget::SinkInput(s.index)))).spacing(10).into()
            }
        }
        AudioSubTab::Recording => {
            if snapshot.source_outputs.is_empty() {
                empty("Nothing is currently recording audio.")
            } else {
                column(snapshot.source_outputs.iter().map(|s| stream_card(colors, s, AudioTarget::SourceOutput(s.index)))).spacing(10).into()
            }
        }
        AudioSubTab::Configuration => {
            if snapshot.cards.is_empty() {
                empty("No audio cards found.")
            } else {
                column(snapshot.cards.iter().map(|c| card_config(colors, c))).spacing(10).into()
            }
        }
        AudioSubTab::Spatial => spatial_view(colors, &spatial),
    };

    column![
        row(tab_row).spacing(6),
        iced::widget::scrollable(content).height(Length::Fill),
    ]
    .spacing(16)
    .padding(20)
    .into()
}

// ══ Spatial subtab ════════════════════════════════════════════════════════
//
// Experimental headphone audio lab: pick an output mode (Stereo / Mono /
// Virtual surround), see what channel layout each app is really feeding the
// system, and fire test tones. Backed by `crate::surround`.

fn seg_button<'a>(colors: AppColors, label: &'a str, active: bool, msg: Message) -> Element<'a> {
    button(text(label).size(13).style(move |_| text::Style {
        color: Some(if active { colors.bar_bg } else { colors.text }),
    }))
    .padding([8, 16])
    .style(move |_, status| iced::widget::button::Style {
        background: Some(Background::Color(if active {
            colors.accent
        } else if matches!(status, iced::widget::button::Status::Hovered) {
            colors.surface
        } else {
            colors.sec_bg
        })),
        text_color: if active { colors.bar_bg } else { colors.text },
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    })
    .on_press(msg)
    .into()
}

fn small_button<'a>(colors: AppColors, label: String, msg: Message) -> Element<'a> {
    button(text(label).size(12).style(move |_| text::Style { color: Some(colors.text) }))
        .padding([6, 10])
        .style(move |_, status| iced::widget::button::Style {
            background: Some(Background::Color(if matches!(status, iced::widget::button::Status::Hovered) {
                colors.accent
            } else {
                colors.surface
            })),
            text_color: colors.text,
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        })
        .on_press(msg)
        .into()
}

fn spatial_view<'a>(colors: AppColors, s: &SpatialUi<'a>) -> Element<'a> {
    let dim = move |t: &str| text(t.to_string()).size(12).style(move |_| text::Style { color: Some(colors.dim_text) });
    let head = move |t: &str| text(t.to_string()).size(13).style(move |_| text::Style { color: Some(colors.text) });

    use crate::surround::RouteMode;
    let is_off = s.route == RouteMode::Off;
    let is_surround = !is_off; // preset / output picker visible whenever the chain is up

    // ── mode selector ───────────────────────────────────────────────
    let selector = row(RouteMode::ALL.iter().map(|m| {
        let m = *m;
        seg_button(colors, m.label(), s.route == m, Message::SpatialRoute(m))
    }))
    .spacing(6);

    let mode_note = dim(match s.route {
        RouteMode::Off => "No processing. Movies with 5.1/7.1 get folded to plain stereo like normal.",
        RouteMode::Auto => "5.1 / 7.1 streams (movies, surround games) route through the HRTF sink automatically. Stereo — music, YouTube, games that do their own 3D audio — is left untouched.",
        RouteMode::Always => "Every stream goes through the HRTF sink, stereo included.",
    });

    let preset_row: Element<'a> = if is_surround {
        row![
            head("HRIR profile"),
            pick_list(&Preset::ALL[..], Some(s.preset), Message::SpatialPresetSelected),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center)
        .into()
    } else {
        column![].into()
    };

    let banner_text: Option<String> = s.error.map(|e| e.to_string());
    let error_banner: Element<'a> = match banner_text {
        Some(e) => container(text(e).size(12).style(move |_| text::Style { color: Some(Color::WHITE) }))
            .padding(10)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(MUTE_RED)),
                border: Border { radius: colors.radius.into(), ..Default::default() },
                ..Default::default()
            })
            .into(),
        None => column![].into(),
    };

    let out_row: Element<'a> = if is_off {
        column![].into()
    } else {
        row![
            head("Send binaural output to"),
            pick_list(s.outs, s.out_selected.clone(), Message::SpatialOutputSelected),
        ]
        .spacing(12)
        .align_y(iced::Alignment::Center)
        .into()
    };

    let mode_card = container(
        column![head("Output mode"), selector, mode_note, preset_row, out_row, error_banner]
            .spacing(10)
            .padding(14),
    )
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    });

    // ── what's playing ──────────────────────────────────────────────
    let stream_rows: Element<'a> = if s.streams.is_empty() {
        dim("Nothing is playing. Start a track or a movie and its real channel layout shows here.").into()
    } else {
        column(s.streams.iter().map(|st| {
            let surround = st.channels > 2;
            let in_color = if surround { colors.accent } else { colors.dim_text };
            let name = if st.corked { format!("{} (paused)", st.app) } else { st.app.clone() };
            let via = if st.via_hrtf { "HRTF" } else { "direct" };
            let via_color = if st.via_hrtf { colors.accent } else { colors.dim_text };
            // "App    5.1 in  →  HRTF  →  Headphones"
            row![
                text(name).size(13).style(move |_| text::Style { color: Some(colors.text) }),
                iced::widget::horizontal_space(),
                text(format!("{} in", st.layout)).size(12).style(move |_| text::Style { color: Some(in_color) }),
                text("\u{2192}").size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
                text(via).size(12).style(move |_| text::Style { color: Some(via_color) }),
                text("\u{2192}").size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
                text(st.dest.clone()).size(12).style(move |_| text::Style { color: Some(colors.text) }),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
        }))
        .spacing(8)
        .into()
    };
    let streams_card = container(column![head("What's playing  (in \u{2192} path \u{2192} out)"), stream_rows].spacing(10).padding(14))
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(colors.sec_bg)),
            border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
            ..Default::default()
        });

    // ── test signals ────────────────────────────────────────────────
    let chan_btns: Element<'a> = iced::widget::row(CHANNEL_LABELS.iter().enumerate().map(|(i, label)| {
        small_button(colors, label.to_string(), Message::SpatialTest(TestSignal::Channel(i as u8)))
    }))
    .spacing(6)
    .wrap()
    .into();
    let other_btns = row![
        small_button(colors, "Left only".into(), Message::SpatialTest(TestSignal::LeftOnly)),
        small_button(colors, "Right only".into(), Message::SpatialTest(TestSignal::RightOnly)),
        small_button(colors, "20 Hz → 20 kHz sweep".into(), Message::SpatialTest(TestSignal::Sweep)),
    ]
    .spacing(6);
    let stop_btn: Element<'a> = if s.test_running {
        small_button(colors, "■ Stop".into(), Message::SpatialTestStop)
    } else {
        column![].into()
    };
    let walk_btn = seg_button(
        colors,
        "\u{25B6}  Test all channels",
        false,
        Message::SpatialTest(TestSignal::AllChannels),
    );
    let test_card = container(
        column![
            head("Test signals"),
            dim("Played through whatever the current mode routes to — with Virtual surround on, each channel should land at a different point around your head."),
            row![walk_btn, iced::widget::horizontal_space(), stop_btn].align_y(iced::Alignment::Center),
            dim("…or fire one channel at a time:"),
            chan_btns,
            other_btns,
        ]
        .spacing(10)
        .padding(14),
    )
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    });

    // ── localization soundstage ─────────────────────────────────────
    let mode_row = row![
        seg_button(colors, "Free play", !s.stage_test_mode, Message::SpatialStageMode(false)),
        seg_button(colors, "Test me", s.stage_test_mode, Message::SpatialStageMode(true)),
    ]
    .spacing(6);

    let stage = crate::soundstage::view(
        s.stage_reveal,
        s.stage_guess,
        s.game_target.is_some(),
        colors,
    );

    let stage_controls: Element<'a> = if s.stage_test_mode {
        let score = if s.game_rounds > 0 {
            // exact = 1, adjacent = 0.5
            let weighted = (s.game_hits * 100 + s.game_near * 50) / s.game_rounds;
            format!(
                "exact {} · close {} / {}  ({}%)",
                s.game_hits, s.game_near, s.game_rounds, weighted
            )
        } else {
            "no rounds yet".to_string()
        };
        let verdict: Element<'a> = match (s.stage_guess, s.stage_reveal) {
            (Some(g), Some(t)) if g == t => text("\u{2713} spot on")
                .size(13)
                .style(move |_| text::Style { color: Some(colors.accent) })
                .into(),
            (Some(g), Some(t)) if crate::soundstage::adjacent(g, t) => text("\u{2248} one off")
                .size(13)
                .style(move |_| text::Style { color: Some(Color { r: 0.82, g: 0.6, b: 0.15, a: 1.0 }) })
                .into(),
            (Some(_), Some(_)) => text("\u{2717} missed")
                .size(13)
                .style(move |_| text::Style { color: Some(MUTE_RED) })
                .into(),
            _ => column![].into(),
        };
        column![
            row![
                small_button(colors, "\u{25B6} New sound".into(), Message::SpatialGameNext),
                iced::widget::horizontal_space(),
                verdict,
                text(score).size(12).style(move |_| text::Style { color: Some(colors.dim_text) }),
            ]
            .spacing(12)
            .align_y(iced::Alignment::Center),
            dim("A hidden direction plays. Click where you heard it. Tells you how much is your ears vs the HRIR."),
        ]
        .spacing(8)
        .into()
    } else {
        dim("Click any direction around the head to hear a sound placed there.").into()
    };

    let stage_card = container(
        column![head("Localization"), mode_row, stage, stage_controls]
            .spacing(10)
            .padding(14)
            .align_x(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(colors.sec_bg)),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        ..Default::default()
    });

    column![mode_card, streams_card, test_card, stage_card].spacing(14).into()
}
