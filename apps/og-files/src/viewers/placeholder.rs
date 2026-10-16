//! Stand-in for viewer families not built yet. Each names its planned
//! backend so the roadmap is visible in the app itself.

use crate::filetype::Kind;
use crate::theme;
use iced::widget::{button, center, column, text};
use iced::Element;
use std::path::PathBuf;

pub struct State {
    kind: Kind,
    /// Some(needs_server) when "Convert to PDF" is possible
    convert: Option<bool>,
}

impl State {
    pub fn new(path: PathBuf, kind: Kind) -> Self {
        Self {
            kind,
            convert: super::convert::convertible(&path, kind),
        }
    }

    pub fn view<M: Clone + 'static>(&self, on_convert: M) -> Element<'_, M> {
        let plan = match self.kind {
            Kind::Video => "Phase 2: GStreamer playback into a wgpu texture (play, seek, trim).",
            Kind::Audio => "Phase 2: symphonia decode + cpal output, waveform view, trim.",
            Kind::Doc => "Phase 2: DOCX/ODT/EPUB text view. Convert to PDF works now.",
            Kind::Model3d => "Phase 3: glTF/OBJ/STL into an iced shader widget (orbit camera).",
            Kind::Table => "Phase 2: xlsx/ods via calamine.",
            Kind::Archive => "Phase 2: browse archives as folders (zip / tar crates).",
            _ => "Phase 2: hex view for binary files.",
        };
        let mut c = column![
            crate::widgets::kind_icon(self.kind).size(56),
            text(format!("{} viewer not built yet", self.kind.label())).size(18),
            text(plan).color(theme::MUTED),
        ]
        .spacing(8)
        .align_x(iced::Alignment::Center);
        if let Some(server) = self.convert {
            c = c.push(
                button(text(if server {
                    "Convert to PDF (Stirling server)"
                } else {
                    "Convert to PDF"
                }))
                .on_press(on_convert),
            );
        }
        center(c).into()
    }
}
