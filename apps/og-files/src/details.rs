//! Bottom strip: file details & history — an activity log (opened, saved,
//! copied, trashed, errors…) persisted to
//! `$XDG_STATE_HOME/og-files/activity.jsonl`, filterable to the focused
//! file. Also hosts the running file jobs. The log can be hidden (kept
//! hidden across restarts — handy for demos/screen sharing) or cleared.

use crate::theme;
use iced::widget::{button, column, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const KEEP: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// unix seconds
    pub t: u64,
    pub msg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

#[derive(Default)]
pub struct Details {
    log: Vec<Entry>,
    unsaved: Vec<Entry>,
    status: String,
    /// show only entries about this file
    pub focus: Option<PathBuf>,
    only_focus: bool,
    /// entries and the status line are not shown (still recorded)
    hidden: bool,
    /// "Clear" was pressed once; the next press really clears
    clear_armed: bool,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Vec<Entry>, bool),
    OnlyFocus(bool),
    Hide(bool),
    Clear,
    Saved,
}

fn log_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::config::home().join(".local/state"));
    base.join("og-files").join("activity.jsonl")
}

/// Exists while the history is hidden.
fn hidden_flag() -> PathBuf {
    log_path().with_file_name("history-hidden")
}

pub fn load() -> Task<Msg> {
    Task::perform(
        async {
            let text = tokio::fs::read_to_string(log_path())
                .await
                .unwrap_or_default();
            let mut v: Vec<Entry> = text
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect();
            let skip = v.len().saturating_sub(KEEP);
            v.drain(..skip);
            let hidden = tokio::fs::try_exists(hidden_flag()).await.unwrap_or(false);
            (v, hidden)
        },
        |(v, hidden)| Msg::Loaded(v, hidden),
    )
}

impl Details {
    pub fn record(&mut self, msg: impl Into<String>) {
        self.record_for(msg, None);
    }

    pub fn record_for(&mut self, msg: impl Into<String>, path: Option<&Path>) {
        let msg = msg.into();
        self.status = msg.clone();
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let e = Entry {
            t,
            msg,
            path: path.map(Path::to_path_buf),
        };
        self.unsaved.push(e.clone());
        self.log.push(e);
        if self.log.len() > KEEP {
            self.log.remove(0);
        }
    }

    /// Append pending entries to the log file (the app calls this after
    /// every update; it's a no-op when nothing is pending).
    pub fn flush(&mut self) -> Task<Msg> {
        if self.unsaved.is_empty() {
            return Task::none();
        }
        let lines: String = self
            .unsaved
            .drain(..)
            .filter_map(|e| serde_json::to_string(&e).ok())
            .map(|l| l + "\n")
            .collect();
        Task::perform(
            async move {
                use tokio::io::AsyncWriteExt;
                let p = log_path();
                if let Some(d) = p.parent() {
                    let _ = tokio::fs::create_dir_all(d).await;
                }
                if let Ok(mut f) = tokio::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .await
                {
                    let _ = f.write_all(lines.as_bytes()).await;
                }
            },
            |_| Msg::Saved,
        )
    }

    pub fn update(&mut self, m: Msg) -> Task<Msg> {
        if !matches!(m, Msg::Clear | Msg::Saved) {
            self.clear_armed = false;
        }
        match m {
            Msg::Loaded(mut older, hidden) => {
                older.append(&mut self.log);
                self.log = older;
                self.hidden = hidden;
                Task::none()
            }
            Msg::OnlyFocus(b) => {
                self.only_focus = b;
                Task::none()
            }
            Msg::Hide(b) => {
                self.hidden = b;
                Task::perform(
                    async move {
                        let f = hidden_flag();
                        if b {
                            if let Some(d) = f.parent() {
                                let _ = tokio::fs::create_dir_all(d).await;
                            }
                            let _ = tokio::fs::write(&f, b"").await;
                        } else {
                            let _ = tokio::fs::remove_file(&f).await;
                        }
                    },
                    |_| Msg::Saved,
                )
            }
            Msg::Clear if !self.clear_armed => {
                self.clear_armed = true;
                Task::none()
            }
            Msg::Clear => {
                self.clear_armed = false;
                self.log.clear();
                self.unsaved.clear();
                self.status.clear();
                Task::perform(
                    async {
                        let p = log_path();
                        if tokio::fs::try_exists(&p).await.unwrap_or(false) {
                            let _ = tokio::fs::write(&p, b"").await;
                        }
                    },
                    |_| Msg::Saved,
                )
            }
            Msg::Saved => Task::none(),
        }
    }

    pub fn view<'a, M: Clone + 'a>(
        &'a self,
        jobs: Element<'a, M>,
        wrap: impl Fn(Msg) -> M + 'a,
    ) -> Element<'a, M> {
        let filter = self.focus.as_ref().filter(|_| self.only_focus);
        let shown = if self.hidden { &[][..] } else { &self.log[..] };
        let rows = shown
            .iter()
            .rev()
            .filter(|e| {
                filter.is_none_or(|f| {
                    e.path.as_ref() == Some(f) || e.msg.contains(&*f.to_string_lossy())
                })
            })
            .take(300)
            .map(|e| {
                row![
                    text(ago(e.t)).size(12).color(theme::MUTED).width(90),
                    text(e.msg.clone()).size(12)
                ]
                .into()
            });
        let focus_btn: Element<'a, M> = match &self.focus {
            Some(f) => button(
                text(if self.only_focus {
                    "Showing this file — show all".to_string()
                } else {
                    format!(
                        "Only {}",
                        f.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                })
                .size(11),
            )
            .on_press(wrap(Msg::OnlyFocus(!self.only_focus)))
            .style(button::text)
            .into(),
            None => Space::with_width(0).into(),
        };
        let focus_btn = if self.hidden {
            Space::with_width(0).into()
        } else {
            focus_btn
        };
        let hide_btn = button(
            row![
                crate::widgets::icon(if self.hidden {
                    iced_fonts::Bootstrap::Eye
                } else {
                    iced_fonts::Bootstrap::EyeSlash
                })
                .size(12),
                text(if self.hidden { "Show history" } else { "Hide" }).size(11),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        )
        .on_press(wrap(Msg::Hide(!self.hidden)))
        .style(button::text);
        let clear_btn = button(
            row![
                crate::widgets::icon(iced_fonts::Bootstrap::Trash).size(12),
                text(if self.clear_armed {
                    "Click again to clear all history"
                } else {
                    "Clear"
                })
                .size(11),
            ]
            .spacing(4)
            .align_y(iced::Alignment::Center),
        )
        .on_press_maybe((!self.log.is_empty()).then(|| wrap(Msg::Clear)))
        .style(if self.clear_armed {
            button::danger
        } else {
            button::text
        });
        let status = if self.hidden {
            "History hidden".to_string()
        } else {
            self.status.clone()
        };
        column![
            row![
                text("File details & history").size(15),
                text(status).size(12).color(theme::MUTED),
                Space::with_width(Length::Fill),
                focus_btn,
                hide_btn,
                clear_btn,
            ]
            .spacing(16)
            .align_y(iced::Alignment::Center),
            jobs,
            scrollable(column(rows).spacing(2)).height(Length::Fill),
        ]
        .spacing(8)
        .padding(12)
        .into()
    }
}

fn ago(t: u64) -> String {
    crate::fsops::ago(Some(UNIX_EPOCH + Duration::from_secs(t)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_needs_two_presses_and_other_input_disarms() {
        let mut d = Details::default();
        d.record("opened a.txt");
        d.record("saved a.txt");
        let _ = d.update(Msg::Clear);
        assert_eq!(d.log.len(), 2);
        let _ = d.update(Msg::OnlyFocus(true)); // disarms
        let _ = d.update(Msg::Clear);
        assert_eq!(d.log.len(), 2);
        let _ = d.update(Msg::Clear);
        assert!(d.log.is_empty() && d.unsaved.is_empty() && d.status.is_empty());
    }

    #[test]
    fn hidden_still_records() {
        let mut d = Details::default();
        d.hidden = true;
        d.record("opened b.txt");
        assert_eq!(d.log.len(), 1);
    }
}
