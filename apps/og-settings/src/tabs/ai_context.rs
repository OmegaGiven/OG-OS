use std::collections::HashSet;

use iced::widget::{button, checkbox, column, container, row, text, text_editor, text_input};
use iced::{Background, Border, Length};

use crate::ai_context::AiContext;
use crate::app::{AiSkillDraft, AppColors, Message};
use crate::vpn::TailscaleStatus;
use crate::voice_history::VoiceHistoryEntry;

pub fn view<'a>(
    colors: AppColors,
    allow_execution: bool,
    ai_context: &'a AiContext,
    notes_editor: &'a text_editor::Content,
    tailscale: &'a TailscaleStatus,
    add_name: &'a str,
    add_address: &'a str,
    add_notes: &'a str,
    ai_skills: &'a [AiSkillDraft],
    skill_add_name: &'a str,
    skill_add_description: &'a str,
    skill_add_body: &'a text_editor::Content,
    voice_history: &'a [VoiceHistoryEntry],
    voice_history_expanded: &'a HashSet<usize>,
    voice_history_keep_input: &'a str,
) -> iced::Element<'a, Message> {
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
    let danger_btn_style = move |_: &_, _| iced::widget::button::Style {
        background: Some(Background::Color(iced::Color { r: 0.6, g: 0.1, b: 0.1, a: 1.0 })),
        text_color: iced::Color::WHITE,
        border: Border { radius: colors.radius.into(), ..Default::default() },
        ..Default::default()
    };
    let text_input_style = move |_: &_, _| iced::widget::text_input::Style {
        background: Background::Color(colors.surface),
        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
        icon: colors.dim_text,
        placeholder: colors.dim_text,
        value: colors.text,
        selection: colors.accent,
    };

    let title = move |t: &'a str| -> iced::Element<'a, Message> {
        text(t).size(15).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let label = move |t: String| -> iced::Element<'a, Message> {
        text(t).style(move |_| iced::widget::text::Style { color: Some(colors.text) }).into()
    };
    let dim = move |t: String| -> iced::Element<'a, Message> {
        text(t).size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }).into()
    };

    #[derive(Clone, Copy)]
    enum ActionKind { Plain, Accent, Danger }
    let action_btn = move |lbl: &'static str, msg: Message, kind: ActionKind| -> iced::Element<'a, Message> {
        let text_color = match kind {
            ActionKind::Plain => colors.text,
            ActionKind::Accent => colors.bar_bg,
            ActionKind::Danger => iced::Color::WHITE,
        };
        let btn = button(text(lbl).size(12).style(move |_| iced::widget::text::Style { color: Some(text_color) }))
            .on_press(msg)
            .padding([4, 10]);
        match kind {
            ActionKind::Plain => btn.style(btn_style),
            ActionKind::Accent => btn.style(accent_btn_style),
            ActionKind::Danger => btn.style(danger_btn_style),
        }
        .into()
    };

    // ── Execution permission ─────────────────────────────────────────────
    // Safety-critical toggle, kept in its own compact card above
    // everything else on this tab rather than buried among descriptive
    // context. Backed by og-voice's own `voice-config.json`
    // (`voice_config.rs`), not `ai-context.json` — see that module's docs
    // for why this one setting lives in a separate small file.
    let execution_card = container(
        column![
            checkbox("Allow AI to execute actions (skip permission prompts)", allow_execution)
                .on_toggle(Message::AiAllowExecutionToggled)
                .style(move |_, _| iced::widget::checkbox::Style {
                    background: Background::Color(colors.surface),
                    icon_color: colors.accent,
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    text_color: Some(colors.text),
                }),
            dim("When enabled, voice/typed commands can run real system actions (SSH, file edits, shell commands) immediately with no confirmation step. When disabled, Claude can only describe what it would do.".to_string()),
        ]
        .spacing(8)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Tailnet Hosts ────────────────────────────────────────────────────
    let mut host_rows: Vec<iced::Element<Message>> = Vec::new();

    if tailscale.installed && tailscale.running {
        for peer in tailscale.peers.iter() {
            // Already-saved rows (tailnet-sourced or manually added under
            // the same name) get an editable notes field; live peers not
            // yet saved get an "Add" prompt instead.
            if let Some(idx) = ai_context.hosts.iter().position(|h| h.name == peer.hostname) {
                let notes = ai_context.hosts[idx].notes.clone();
                host_rows.push(
                    column![
                        row![
                            label(peer.hostname.clone()),
                            dim(format!("{} · {}", peer.ip, if peer.online { "online" } else { "offline" })),
                            iced::widget::horizontal_space(),
                            action_btn("Remove", Message::AiContextHostRemove(idx), ActionKind::Danger),
                        ]
                        .align_y(iced::Alignment::Center)
                        .spacing(10),
                        text_input("What should an AI agent know before sshing in?", &notes)
                            .on_input(move |v| Message::AiContextHostNotesChanged(idx, v))
                            .style(text_input_style),
                    ]
                    .spacing(6)
                    .into(),
                );
            } else {
                let name = peer.hostname.clone();
                let ip = peer.ip.clone();
                host_rows.push(
                    row![
                        column![
                            label(peer.hostname.clone()),
                            dim(format!("{} · {}", peer.ip, if peer.online { "online" } else { "offline" })),
                        ]
                        .spacing(2)
                        .width(Length::Fill),
                        action_btn("Add", Message::AiContextAddFromTailnet(name, ip), ActionKind::Accent),
                    ]
                    .align_y(iced::Alignment::Center)
                    .spacing(10)
                    .into(),
                );
            }
        }
        if tailscale.peers.is_empty() {
            host_rows.push(dim("No other machines on this tailnet yet.".to_string()));
        }
    } else if !tailscale.installed {
        host_rows.push(dim("tailscale not found on this system — add hosts manually below.".to_string()));
    } else {
        host_rows.push(dim("Tailscale is installed but not running — add hosts manually below.".to_string()));
    }

    // Manually-added hosts (not sourced from a live tailnet peer, e.g. an
    // SSH-config alias) get their own editable rows too, name/address
    // included since there's no live source to fall back on for those.
    for (idx, h) in ai_context.hosts.iter().enumerate() {
        if h.from_tailnet {
            continue;
        }
        let name = h.name.clone();
        let address = h.address.clone();
        let notes = h.notes.clone();
        host_rows.push(
            column![
                row![
                    label(if name.is_empty() { "(unnamed host)".to_string() } else { name.clone() }),
                    dim(address.clone()),
                    iced::widget::horizontal_space(),
                    action_btn("Remove", Message::AiContextHostRemove(idx), ActionKind::Danger),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(10),
                text_input("What should an AI agent know before sshing in?", &notes)
                    .on_input(move |v| Message::AiContextHostNotesChanged(idx, v))
                    .style(text_input_style),
            ]
            .spacing(6)
            .into(),
        );
    }

    let add_row = column![
        text("+ Add host manually").size(12)
            .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
        row![
            text_input("name (e.g. nas)", add_name)
                .on_input(Message::AiContextAddNameChanged)
                .style(text_input_style)
                .width(Length::FillPortion(1)),
            text_input("address (SSH alias, IP, hostname…)", add_address)
                .on_input(Message::AiContextAddAddressChanged)
                .style(text_input_style)
                .width(Length::FillPortion(1)),
        ]
        .spacing(10),
        text_input("notes", add_notes)
            .on_input(Message::AiContextAddNotesChanged)
            .style(text_input_style),
        action_btn("Add host", Message::AiContextAddSubmit, ActionKind::Accent),
    ]
    .spacing(8);

    let hosts_card = container(
        column(
            std::iter::once(title("Tailnet Hosts"))
                .chain(std::iter::once(
                    dim("Machines an AI agent may be asked to act on — what they are, what's on them, anything worth knowing before sshing in.".to_string())
                ))
                .chain(host_rows)
                .chain(std::iter::once(add_row.into()))
                .collect::<Vec<_>>(),
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── General Notes ────────────────────────────────────────────────────
    let notes_card = container(
        column![
            title("General Notes"),
            dim("Free text, fed to AI agents as system context. e.g. \"Prefer terse commit messages. Always confirm before destructive commands. My homelab uses Arch everywhere except `download` which runs Debian…\"".to_string()),
            text_editor(notes_editor)
                .placeholder("How do you like things done?")
                .on_action(Message::AiContextNotesEdited)
                .height(220)
                .style(move |_, _| text_editor::Style {
                    background: Background::Color(colors.surface),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    icon: colors.dim_text,
                    placeholder: colors.dim_text,
                    value: colors.text,
                    selection: colors.accent,
                }),
        ]
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── Skills ───────────────────────────────────────────────────────────
    // Reusable named procedures the voice agent can draw on — stored as
    // real Claude-Code-format `SKILL.md` files (see `ai_skills.rs`).
    let mut skill_rows: Vec<iced::Element<Message>> = Vec::new();
    for (idx, s) in ai_skills.iter().enumerate() {
        let mut rows: Vec<iced::Element<Message>> = vec![
            row![
                column![
                    label(if s.name.trim().is_empty() { "(unnamed skill)".to_string() } else { s.name.clone() }),
                    dim(format!("slug: {}", crate::ai_skills::slugify(&s.name))),
                ]
                .spacing(2)
                .width(Length::FillPortion(1)),
                text_input("one-line description", &s.description)
                    .on_input(move |v| Message::AiSkillsDescriptionChanged(idx, v))
                    .style(text_input_style)
                    .width(Length::FillPortion(2)),
                action_btn(if s.expanded { "Collapse" } else { "Edit body" }, Message::AiSkillsToggleExpand(idx), ActionKind::Plain),
                action_btn("Remove", Message::AiSkillsRemove(idx), ActionKind::Danger),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10)
            .into(),
        ];
        if s.expanded {
            rows.push(
                text_editor(&s.body)
                    .placeholder("Step-by-step instructions this skill should carry out...")
                    .on_action(move |a| Message::AiSkillsBodyEdited(idx, a))
                    .height(160)
                    .style(move |_, _| text_editor::Style {
                        background: Background::Color(colors.surface),
                        border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                        icon: colors.dim_text,
                        placeholder: colors.dim_text,
                        value: colors.text,
                        selection: colors.accent,
                    })
                    .into(),
            );
        }
        skill_rows.push(column(rows).spacing(6).into());
    }
    if ai_skills.is_empty() {
        skill_rows.push(dim("No skills yet — add one below.".to_string()));
    }

    let skill_add_row = column![
        text("+ Add skill").size(12)
            .style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
        row![
            text_input("name (e.g. restarting sonarr)", skill_add_name)
                .on_input(Message::AiSkillsAddNameChanged)
                .style(text_input_style)
                .width(Length::FillPortion(1)),
            text_input("one-line description", skill_add_description)
                .on_input(Message::AiSkillsAddDescriptionChanged)
                .style(text_input_style)
                .width(Length::FillPortion(1)),
        ]
        .spacing(10),
        text_editor(skill_add_body)
            .placeholder("Step-by-step instructions...")
            .on_action(Message::AiSkillsAddBodyEdited)
            .height(120)
            .style(move |_, _| text_editor::Style {
                background: Background::Color(colors.surface),
                border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                icon: colors.dim_text,
                placeholder: colors.dim_text,
                value: colors.text,
                selection: colors.accent,
            }),
        action_btn("Add skill", Message::AiSkillsAddSubmit, ActionKind::Accent),
    ]
    .spacing(8);

    let skills_card = container(
        column(
            std::iter::once(title("Skills"))
                .chain(std::iter::once(
                    dim("Reusable named procedures for the voice agent — \"restarting Sonarr\", \"how I want commit messages written\" — documented once, reused across sessions. Written to ~/.config/sway-power/.claude/skills/<slug>/SKILL.md.".to_string())
                ))
                .chain(skill_rows)
                .chain(std::iter::once(skill_add_row.into()))
                .collect::<Vec<_>>(),
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    // ── History ──────────────────────────────────────────────────────────
    // Read-only log of past voice sessions — what was said, what Claude
    // answered, and what it actually did (tool calls) getting there.
    // Written by og-voice (`og-voice/src/history.rs`), reloaded fresh from
    // `~/.config/sway-power/voice-history.jsonl` every time this tab is
    // opened (see `Message::TabSelected` in `app.rs`) — no staged-edit /
    // Apply&Save here, it's a viewer, not a setting.
    let mut history_rows: Vec<iced::Element<Message>> = Vec::new();
    for (idx, entry) in voice_history.iter().enumerate() {
        let expanded = voice_history_expanded.contains(&idx);
        let preview: String = entry.transcript.chars().take(60).collect();
        let preview = if entry.transcript.chars().count() > 60 {
            format!("{preview}…")
        } else if preview.is_empty() {
            "(no transcript)".to_string()
        } else {
            preview
        };

        let header_row = button(
            row![
                dim(entry.timestamp.clone()),
                label(preview),
                iced::widget::horizontal_space(),
                dim(if expanded { "▲".to_string() } else { "▼".to_string() }),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10),
        )
        .on_press(Message::AiVoiceHistoryToggleExpand(idx))
        .padding([6, 8])
        .style(btn_style);

        let mut rows: Vec<iced::Element<Message>> = vec![header_row.into()];

        if expanded {
            rows.push(dim("Transcript".to_string()));
            rows.push(label(entry.transcript.clone()));
            rows.push(dim("Response".to_string()));
            rows.push(label(entry.response.clone()));
            if entry.actions.is_empty() {
                rows.push(dim("No actions taken.".to_string()));
            } else {
                rows.push(dim("Actions".to_string()));
                for action in &entry.actions {
                    rows.push(dim(format!("- {}: {}", action.tool, action.summary)));
                }
            }
        }

        history_rows.push(
            container(column(rows).spacing(6).padding(10))
                .style(move |_: &_| container::Style {
                    background: Some(Background::Color(colors.surface)),
                    border: Border { color: colors.border, width: 1.0, radius: colors.radius.into() },
                    ..Default::default()
                })
                .width(Length::Fill)
                .into(),
        );
    }
    if voice_history.is_empty() {
        history_rows.push(dim("No voice sessions logged yet.".to_string()));
    }

    let prune_row: iced::Element<Message> = row![
        action_btn("Clear All", Message::AiVoiceHistoryClearAll, ActionKind::Danger),
        text("Keep last").size(12).style(move |_| iced::widget::text::Style { color: Some(colors.dim_text) }),
        text_input("200", voice_history_keep_input)
            .on_input(Message::AiVoiceHistoryKeepInputChanged)
            .style(text_input_style)
            .width(Length::Fixed(70.0)),
        action_btn("Prune", Message::AiVoiceHistoryKeepSubmit, ActionKind::Plain),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(10)
    .into();

    let history_card = container(
        column(
            std::iter::once(title("History"))
                .chain(std::iter::once(
                    dim("Past voice sessions — what was said, what Claude answered, and what it actually did getting there. Read-only; refreshed each time this tab opens.".to_string())
                ))
                .chain(std::iter::once(prune_row))
                .chain(history_rows)
                .collect::<Vec<_>>(),
        )
        .spacing(12)
        .padding(20),
    )
    .style(card_style)
    .width(Length::Fill);

    column![execution_card, hosts_card, notes_card, skills_card, history_card].spacing(16).padding(20).into()
}
