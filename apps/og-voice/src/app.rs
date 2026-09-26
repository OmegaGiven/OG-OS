use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Element, Event, Length, Task};
use iced::keyboard;
use iced::window;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::config::{Config, VoiceConfig, APP_TINT_SEED};
use crate::pipeline::{self, SharedState, Stage};
use crate::placement;
use og_theme::AppColors;

pub const SIZE: (f32, f32) = (380.0, 300.0);

/// Auto-close this long after the answer is shown, if the user hasn't
/// already dismissed it (Escape / click-away) — the "idle time" half of
/// the close condition the brief asks for.
const IDLE_CLOSE_AFTER_DONE: std::time::Duration = std::time::Duration::from_secs(25);

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Close,
    /// Window lost focus. Only actually closes in resting states — see
    /// the `Message::UnfocusedClose` arm in `update()`.
    UnfocusedClose,
    Positioned,
    Reveal(window::Id),
    /// `--interactive` only: text input box edited.
    InputChanged(String),
    /// `--interactive` only: Enter pressed in the text input with
    /// non-empty content — skips STT entirely, same as the brief asks.
    SubmitText,
    /// `--interactive` only: mic button clicked — starts recording from
    /// `Idle`, or signals stop (same as an external `--stop`) from
    /// `Recording`.
    MicToggle,
}

pub struct App {
    config: Config,
    voice_config: VoiceConfig,
    shared: SharedState,
    stop_flag: Arc<AtomicBool>,
    start_flag: Arc<AtomicBool>,
    stage: Stage,
    transcript: String,
    answer: String,
    done_since: Option<Instant>,
    /// Whether this is a `--interactive` launch (idle-first, text input +
    /// mic toggle) vs. the default hotkey-triggered recording-first
    /// launch. Only affects `view()`/`update()` branching here — the
    /// underlying pipeline (`pipeline.rs`) doesn't need to know which
    /// mode triggered it.
    interactive: bool,
    ai_cli: String,
    system_prompt: Option<String>,
    stt_url: String,
    /// `--interactive` only: current contents of the text input box.
    input_value: String,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        shared: SharedState,
        stop_flag: Arc<AtomicBool>,
        start_flag: Arc<AtomicBool>,
        interactive: bool,
        ai_cli: String,
        system_prompt: Option<String>,
        stt_url: String,
    ) -> (Self, Task<Message>) {
        let stage = if interactive { Stage::Idle } else { Stage::StartingStt };
        (
            Self {
                config: Config::load(),
                voice_config: VoiceConfig::load(),
                shared,
                stop_flag,
                start_flag,
                stage,
                transcript: String::new(),
                answer: String::new(),
                done_since: None,
                interactive,
                ai_cli,
                system_prompt,
                stt_url,
                input_value: String::new(),
            },
            Task::perform(async { placement::move_to_corner(SIZE) }, |_| Message::Positioned)
                .chain(window::get_latest().map(|id| match id {
                    Some(id) => Message::Reveal(id),
                    None => Message::Positioned,
                })),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                let s = self.shared.lock().unwrap();
                self.stage = s.stage.clone();
                self.transcript = s.transcript.clone();
                self.answer = s.answer.clone();
                drop(s);

                // Nothing was said — close immediately rather than
                // lingering with an empty/error bubble, per the "no
                // text = just close" rule.
                if matches!(self.stage, Stage::Cancelled) {
                    return iced::exit();
                }

                if matches!(self.stage, Stage::Done | Stage::Failed(_)) {
                    if self.done_since.is_none() {
                        self.done_since = Some(Instant::now());
                    }
                    if self.done_since.unwrap().elapsed() >= IDLE_CLOSE_AFTER_DONE {
                        return iced::exit();
                    }

                    // Hotkey pressed again while the previous answer is
                    // still showing (`og-voice --start`, sent by
                    // `og-voice-launch` when it finds this instance
                    // already running) — start a fresh recording turn
                    // in place instead of doing nothing.
                    if self.start_flag.swap(false, Ordering::Relaxed) {
                        self.transcript.clear();
                        self.answer.clear();
                        self.done_since = None;
                        {
                            let mut s = self.shared.lock().unwrap();
                            s.transcript.clear();
                            s.answer.clear();
                        }
                        pipeline::start(
                            self.stt_url.clone(),
                            self.ai_cli.clone(),
                            self.system_prompt.clone(),
                            self.voice_config.allow_execution,
                            self.stop_flag.clone(),
                            self.shared.clone(),
                        );
                    }
                } else {
                    // Not in a resting state — a stray `--start` signal
                    // (e.g. delivered while still `Recording`/
                    // `Processing`) shouldn't queue up and fire the
                    // instant we land on `Done`.
                    self.start_flag.store(false, Ordering::Relaxed);
                }
                Task::none()
            }
            Message::Close => iced::exit(),
            Message::UnfocusedClose => {
                // See `subscription()` for why `Unfocused` isn't treated
                // as click-away unconditionally: it also fires as a side
                // effect of releasing the held hotkey.
                if matches!(self.stage, Stage::Idle | Stage::Done | Stage::Failed(_)) {
                    iced::exit()
                } else {
                    Task::none()
                }
            }
            Message::Positioned => Task::none(),
            Message::Reveal(id) => window::change_mode(id, window::Mode::Windowed),
            Message::InputChanged(value) => {
                self.input_value = value;
                Task::none()
            }
            Message::SubmitText => {
                let text = self.input_value.trim().to_string();
                if self.interactive && self.stage == Stage::Idle && !text.is_empty() {
                    self.input_value.clear();
                    pipeline::submit_text(
                        text,
                        self.ai_cli.clone(),
                        self.system_prompt.clone(),
                        self.voice_config.allow_execution,
                        self.shared.clone(),
                    );
                }
                Task::none()
            }
            Message::MicToggle => {
                if self.interactive {
                    match self.stage {
                        Stage::Idle => {
                            pipeline::start(
                                self.stt_url.clone(),
                                self.ai_cli.clone(),
                                self.system_prompt.clone(),
                                self.voice_config.allow_execution,
                                self.stop_flag.clone(),
                                self.shared.clone(),
                            );
                        }
                        Stage::Recording => {
                            // Same signal `og-voice --stop` sends via
                            // SIGUSR1 — the capture loop in `pipeline.rs`
                            // polls this flag and doesn't care which path
                            // set it.
                            self.stop_flag.store(true, Ordering::Relaxed);
                        }
                        _ => {}
                    }
                }
                Task::none()
            }
        }
    }

    pub fn subscription(&self) -> iced::Subscription<Message> {
        // `Unfocused` also fires as a side effect of releasing the
        // held-down hotkey itself (sway momentarily shuffles focus while
        // dispatching the `--release` bind) — not just real click-away.
        // Closing unconditionally on it used to kill an in-flight
        // recording/Claude call before it could show the answer, so this
        // routes through `Message::UnfocusedClose`, which `update()`
        // only honors in resting states (`Idle`/`Done`/`Failed`), never
        // `Recording`/`Processing`/`StartingStt`/`Cancelled`.
        let key_events = iced::event::listen_with(|event, _status, _id| match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. })
                if key == keyboard::Key::Named(keyboard::key::Named::Escape) =>
            {
                Some(Message::Close)
            }
            Event::Window(iced::window::Event::Unfocused) => Some(Message::UnfocusedClose),
            _ => None,
        });
        let tick = iced::time::every(pipeline::POLL_INTERVAL).map(|_| Message::Tick);
        iced::Subscription::batch([key_events, tick])
    }

    pub fn view(&self) -> Element<'_, Message> {
        let colors = AppColors::from_config(&self.config, APP_TINT_SEED);

        let status_label = match &self.stage {
            Stage::Idle => "Type or press the mic…",
            Stage::StartingStt => "Starting transcription…",
            Stage::Recording => "Listening…",
            Stage::Processing => "Processing…",
            Stage::Done => "Done",
            Stage::Failed(_) => "Error",
            // Never actually rendered — `Tick` exits the process the
            // instant it sees this stage — but the match must stay
            // exhaustive.
            Stage::Cancelled => "",
        };
        let status_color = match &self.stage {
            Stage::Failed(_) => Color::from_rgb(0.9, 0.35, 0.35),
            Stage::Done => colors.accent,
            _ => colors.dim_text,
        };

        let dot_color = match self.stage {
            Stage::Idle => colors.dim_text,
            Stage::Recording => Color::from_rgb(0.9, 0.3, 0.3),
            Stage::Processing | Stage::StartingStt => colors.accent,
            Stage::Done => Color::from_rgb(0.35, 0.8, 0.45),
            Stage::Failed(_) => Color::from_rgb(0.9, 0.35, 0.35),
            Stage::Cancelled => colors.dim_text,
        };
        let dot = container(text(""))
            .width(Length::Fixed(8.0))
            .height(Length::Fixed(8.0))
            .style(move |_| container::Style {
                background: Some(Background::Color(dot_color)),
                border: Border { radius: 4.0.into(), ..Default::default() },
                ..Default::default()
            });

        let header = row![
            dot,
            text("og-voice").size(14).style(move |_| text::Style { color: Some(colors.text) }),
            iced::widget::horizontal_space(),
            text(status_label).size(13).style(move |_| text::Style { color: Some(status_color) }),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(8);

        let transcript_block: Element<'_, Message> = if self.transcript.is_empty() {
            text(match self.stage {
                Stage::StartingStt => "Warming up the STT server…",
                Stage::Idle => "",
                _ => "Say something…",
            })
            .size(13)
            .style(move |_| text::Style { color: Some(colors.dim_text) })
            .into()
        } else {
            text(self.transcript.clone())
                .size(14)
                .style(move |_| text::Style { color: Some(colors.text) })
                .into()
        };

        // `--interactive` only: text input + mic toggle, shown while
        // `Idle` (both usable) and `Recording` (mic still usable to stop;
        // the text input is present but disabled — no `.on_input`/
        // `.on_submit` attached — since a typed prompt and an in-progress
        // recording shouldn't both be racing to produce the final
        // transcript). Hidden once `Processing`/`Done`/`Failed` — no more
        // input is meaningful once a prompt is already in flight.
        let show_input_row = self.interactive && matches!(self.stage, Stage::Idle | Stage::Recording);

        let mut body = column![].spacing(10);

        if show_input_row {
            let mut input = text_input("Type a prompt, or press the mic…", &self.input_value)
                .padding(8)
                .size(14)
                .width(Length::Fill)
                .style(move |_theme, _status| text_input::Style {
                    background: Background::Color(colors.surface),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    icon: colors.dim_text,
                    placeholder: colors.dim_text,
                    value: colors.text,
                    selection: colors.accent,
                });
            if self.stage == Stage::Idle {
                input = input.on_input(Message::InputChanged).on_submit(Message::SubmitText);
            }

            let mic_label = if self.stage == Stage::Recording { "\u{23F9}" } else { "\u{1F3A4}" };
            let mic_button = button(text(mic_label).size(15))
                .padding(8)
                .on_press(Message::MicToggle)
                .style(move |_theme, _status| button::Style {
                    background: Some(Background::Color(colors.header_btn_bg)),
                    text_color: colors.text,
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                });

            body = body.push(row![input, mic_button].spacing(8).align_y(iced::Alignment::Center));
        }

        if !(self.interactive && self.stage == Stage::Idle) {
            body = body.push(
                container(transcript_block).padding(10).width(Length::Fill).style(move |_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                }),
            );
        }

        match &self.stage {
            Stage::Done => {
                body = body.push(
                    container(
                        // Black, not `colors.text` (white) — the answer
                        // box's background is a solid `accent_fill`
                        // (orange in the default theme), and white on
                        // orange reads poorly. Black holds contrast
                        // against any accent color the theme picks.
                        text(self.answer.clone())
                            .size(14)
                            .style(move |_| text::Style { color: Some(Color::BLACK) }),
                    )
                    .padding(10)
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(colors.accent_fill),
                        border: Border { radius: colors.radius.into(), ..Default::default() },
                        ..Default::default()
                    }),
                );
            }
            Stage::Failed(err) => {
                body = body.push(
                    container(
                        text(err.clone())
                            .size(13)
                            .style(move |_| text::Style { color: Some(Color::WHITE) }),
                    )
                    .padding(10)
                    .width(Length::Fill)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(Color::from_rgb(0.6, 0.2, 0.2))),
                        border: Border { radius: colors.radius.into(), ..Default::default() },
                        ..Default::default()
                    }),
                );
            }
            _ => {}
        }

        let content = column![header, scrollable(body).height(Length::Fill)].spacing(10).padding(14);

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(colors.bg_fill),
                border: Border { color: colors.accent, width: 2.0, radius: colors.radius.into() },
                ..Default::default()
            })
            .into()
    }
}
