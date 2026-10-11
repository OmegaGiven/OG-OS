//! Plain-text / code / markdown on iced's text_editor.
//!
//! Opens read-only. Edit mode is explicit (button or Ctrl+E); Save writes
//! over the original file (atomically), Discard reloads it from disk.

use super::Event;
use crate::filetype::Kind;
use crate::fsops;
use crate::theme;
use iced::widget::{button, column, row, text, text_editor, Space};
use iced::{Element, Font, Length, Task};
use std::path::{Path, PathBuf};

/// Docs that are really text. PDF/DOCX/EPUB go to the placeholder until the
/// doc backend exists.
pub fn supported(path: &Path, kind: Kind) -> bool {
    kind == Kind::Text
        || matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("md" | "markdown" | "txt")
        )
}

pub struct State {
    path: PathBuf,
    content: text_editor::Content,
    pub editing: bool,
    pub dirty: bool,
    saving: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Edit(text_editor::Action),
    ToggleEdit,
    Save,
    Saved(Result<(), String>),
    Discard,
    Reloaded(Result<String, String>),
}

impl State {
    pub fn new(path: PathBuf, s: &str) -> Self {
        Self {
            path,
            content: text_editor::Content::with_text(s),
            editing: false,
            dirty: false,
            saving: false,
        }
    }

    pub fn save(&mut self) -> Task<Msg> {
        if self.saving || !self.dirty {
            return Task::none();
        }
        self.saving = true;
        Task::perform(
            fsops::write_text(self.path.clone(), self.content.text()),
            Msg::Saved,
        )
    }

    pub fn toggle_edit(&mut self) -> Task<Msg> {
        let (t, _) = self.update(Msg::ToggleEdit);
        t
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Edit(a) => {
                // read-only: selection and cursor moves only, no edits
                if self.editing || !a.is_edit() {
                    self.dirty |= a.is_edit();
                    self.content.perform(a);
                }
                (Task::none(), None)
            }
            Msg::ToggleEdit => {
                if self.editing && self.dirty {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Unsaved changes: Save or Discard first".into(),
                        )),
                    );
                }
                self.editing = !self.editing;
                (Task::none(), None)
            }
            Msg::Save => (self.save(), None),
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(()) => {
                        self.dirty = false;
                        (
                            Task::none(),
                            Some(Event::Status(format!("Saved over {}", self.path.display()))),
                        )
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Save failed: {e}"))),
                    ),
                }
            }
            Msg::Discard => (
                Task::perform(
                    fsops::read_text(self.path.clone(), super::TEXT_LIMIT),
                    Msg::Reloaded,
                ),
                None,
            ),
            Msg::Reloaded(r) => match r {
                Ok(s) => {
                    self.content = text_editor::Content::with_text(&s);
                    self.dirty = false;
                    self.editing = false;
                    (
                        Task::none(),
                        Some(Event::Status("Changes discarded".into())),
                    )
                }
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Reload failed: {e}"))),
                ),
            },
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bar: Element<'_, Msg> = if self.editing {
            row![
                text(if self.dirty {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(iced::Color::from_rgb8(0xff, 0xc8, 0x6b)),
                Space::with_width(Length::Fill),
                button(text("Discard").size(12))
                    .on_press_maybe(self.dirty.then_some(Msg::Discard))
                    .style(button::secondary),
                button(text("Save  Ctrl+S").size(12))
                    .on_press_maybe(self.dirty.then_some(Msg::Save)),
                button(text("Done").size(12))
                    .on_press_maybe((!self.dirty).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                text("Read-only").size(12).color(theme::MUTED),
                Space::with_width(Length::Fill),
                button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit),
            ]
            .align_y(iced::Alignment::Center)
            .into()
        };
        // always interactive so read-only text can still be selected and
        // copied; `update` drops edit actions unless in edit mode
        let editor = text_editor(&self.content)
            .font(Font::MONOSPACE)
            .height(Length::Fill)
            .on_action(Msg::Edit);
        column![bar, editor].spacing(6).into()
    }
}
