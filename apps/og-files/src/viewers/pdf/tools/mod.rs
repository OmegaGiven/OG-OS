//! PDF toolkit — native rebuild of the Stirling-PDF tool set (SPEC.md §5a).
//!
//! Every tool is a pure function over PDF bytes, run on the blocking pool by
//! the viewer. Contract shared by all modules:
//!   * input: the *current* document bytes (unsaved page edits included)
//!   * page lists are 0-based indices into that document
//!   * output: [`Output`] — a new version of the document, separate files,
//!     or a text report
//!   * never panic on malformed input; return `Err(String)` with a short,
//!     user-facing message
//!
//! Modules:
//!   * [`pages`]     page-range parsing ("1-3,7,10-", "odd", "all")
//!   * [`structure`] lopdf-only document surgery (metadata, security,
//!                   sanitize, attachments, bookmarks, forms, compare…)
//!   * [`layout`]    page composition + stamping via Form XObjects (n-up,
//!                   booklet, scale, overlay, page numbers, watermarks,
//!                   images → PDF, signatures)
//!   * [`raster`]    hayro-rendered operations (to images, flatten, colour,
//!                   redact regions, blank-page detection, fake scan)
//!   * [`server`]    optional Stirling-PDF REST backend for OCR, compress,
//!                   office conversion, PDF/A, certificate signing

pub mod layout;
pub mod pages;
pub mod raster;
pub mod server;
pub mod structure;

#[derive(Debug, Clone)]
pub enum Output {
    /// A new version of the document; becomes the viewer's working copy
    /// (Save writes it over the original, Save copy to a new file).
    Pdf(Vec<u8>),
    /// Separate files written next to the original as `<stem>-<suffix>`,
    /// e.g. `("page-03.png", bytes)` → `report-page-03.png`.
    Files(Vec<(String, Vec<u8>)>),
    /// Text shown in the tool panel (info, JavaScript dump, comparisons).
    Report(String),
}

/// Standard sheet sizes in PDF points (1/72 inch), portrait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paper {
    A3,
    A4,
    A5,
    Letter,
    Legal,
    Tabloid,
}

impl Paper {
    pub const ALL: [Paper; 6] = [
        Paper::A4,
        Paper::Letter,
        Paper::A3,
        Paper::A5,
        Paper::Legal,
        Paper::Tabloid,
    ];

    pub fn size(self) -> (f32, f32) {
        match self {
            Paper::A3 => (841.89, 1190.55),
            Paper::A4 => (595.28, 841.89),
            Paper::A5 => (419.53, 595.28),
            Paper::Letter => (612.0, 792.0),
            Paper::Legal => (612.0, 1008.0),
            Paper::Tabloid => (792.0, 1224.0),
        }
    }
}

impl std::fmt::Display for Paper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Paper::A3 => "A3",
            Paper::A4 => "A4",
            Paper::A5 => "A5",
            Paper::Letter => "Letter",
            Paper::Legal => "Legal",
            Paper::Tabloid => "Tabloid",
        })
    }
}

/// 3×3 anchor grid used by page numbers, stamps and image placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub const ALL: [Anchor; 9] = [
        Anchor::TopLeft,
        Anchor::Top,
        Anchor::TopRight,
        Anchor::Left,
        Anchor::Center,
        Anchor::Right,
        Anchor::BottomLeft,
        Anchor::Bottom,
        Anchor::BottomRight,
    ];
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Anchor::TopLeft => "top left",
            Anchor::Top => "top",
            Anchor::TopRight => "top right",
            Anchor::Left => "left",
            Anchor::Center => "center",
            Anchor::Right => "right",
            Anchor::BottomLeft => "bottom left",
            Anchor::Bottom => "bottom",
            Anchor::BottomRight => "bottom right",
        })
    }
}

/// RGB, 0.0–1.0 per channel.
pub type Rgb = [f32; 3];
