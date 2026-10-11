//! Right-hand inspector: properties of the focused file plus (later) the
//! per-viewer tool panel (image adjustments, table column types, video trim…).

use crate::filetype::Kind;
use crate::fsops::{self, Meta};
use crate::theme;
use iced::widget::{column, row, scrollable, text, Space};
use iced::{Element, Length, Task};
use std::path::PathBuf;

#[derive(Default)]
pub struct Inspector {
    target: Option<PathBuf>,
    meta: Option<Result<Meta, String>>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(PathBuf, Result<Meta, String>),
}

impl Inspector {
    pub fn focus(&mut self, target: Option<(PathBuf, Kind)>) -> Task<Msg> {
        self.meta = None;
        self.target = target.map(|(p, _)| p);
        match &self.target {
            Some(p) => {
                let p = p.clone();
                Task::perform(fsops::metadata(p.clone()), move |r| {
                    Msg::Loaded(p.clone(), r)
                })
            }
            None => Task::none(),
        }
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            // drop stale results if focus moved on while loading
            Msg::Loaded(p, r) if self.target.as_ref() == Some(&p) => self.meta = Some(r),
            Msg::Loaded(..) => {}
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let field = |k: &'static str, v: String| -> Element<'_, Msg> {
            row![
                text(k).size(12).color(theme::MUTED).width(80),
                text(v).size(12)
            ]
            .spacing(6)
            .into()
        };
        let body: Element<'_, Msg> = match &self.meta {
            None if self.target.is_none() => text("Nothing selected").color(theme::MUTED).into(),
            None => text("...").into(),
            Some(Err(e)) => text(e.clone()).into(),
            Some(Ok(m)) => {
                let mut c = column![
                    text(
                        m.path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    )
                    .size(16),
                    field("Type", m.kind.label().into()),
                    field("Size", fsops::human_size(m.size)),
                    field("Modified", fsops::ago(m.modified)),
                    field(
                        "Mode",
                        m.mode
                            .map(|x| format!("{:o}", x & 0o7777))
                            .unwrap_or_else(|| "—".into())
                    ),
                    field(
                        "Writable",
                        if m.readonly {
                            "no".into()
                        } else {
                            "yes".into()
                        }
                    ),
                ]
                .spacing(6);
                if let Some(n) = m.child_count {
                    c = c.push(field("Items", n.to_string()));
                }
                c = c.push(Space::with_height(12));
                c = c.push(text("Tools").size(14));
                c = c.push(text(tools_for(m.kind)).size(12).color(theme::MUTED));
                c.into()
            }
        };
        column![
            text("Inspector").size(15),
            scrollable(body).height(Length::Fill)
        ]
        .spacing(10)
        .padding(12)
        .into()
    }
}

fn tools_for(kind: Kind) -> &'static str {
    match kind {
        Kind::Image => "crop · rotate · resize · exposure (phase 3)",
        Kind::Video => "trim · snapshot frame · export (phase 3)",
        Kind::Audio => "trim · normalize · tags (phase 3)",
        Kind::Table => "column types · sort · filter (phase 2)",
        Kind::Text => "find/replace · word count · syntax (phase 2)",
        Kind::Doc => "PDF: rotate · delete · reorder · insert (edit mode); extract · split (export). OCR, compress, convert, sign, redact: SPEC.md §5a",
        Kind::Model3d => "wireframe · normals · dimensions (phase 3)",
        _ => "—",
    }
}
