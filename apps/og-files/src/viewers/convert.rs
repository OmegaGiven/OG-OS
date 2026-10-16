//! "Convert to PDF" for files that aren't PDFs, plus the unlock screen for
//! password-protected PDFs.
//!
//! Everything converts natively: images through
//! `tools::layout::images_to_pdf`, office documents, HTML, Markdown,
//! e-mail, e-books, comics and SVG through `tools::topdf` (Typst
//! typesetting in-process).

use super::pdf::tools::{layout, scans, structure, topdf};
use super::Event;
use crate::filetype::Kind;
use crate::theme;
use iced::widget::{button, center, column, text, text_input};
use iced::{Element, Task};
use std::path::{Path, PathBuf};

/// Can this file be turned into a PDF (all in-process)?
pub fn convertible(path: &Path, kind: Kind) -> bool {
    kind == Kind::Image || topdf::supported(path)
}

/// Convert and write `<stem>.pdf` next to the file (never overwrites).
pub async fn to_pdf(path: PathBuf) -> Result<PathBuf, String> {
    // SVG is an "image" by MIME type but converts as a vector drawing
    let raster = Kind::of(&path, false) == Kind::Image && !topdf::supported(&path);
    let src = path.clone();
    let pdf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        if raster {
            let bytes = std::fs::read(&src).map_err(|e| e.to_string())?;
            return layout::images_to_pdf(&[bytes], None, 0.0);
        }
        topdf::file_to_pdf(&src)
    })
    .await
    .map_err(|e| e.to_string())??;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "converted".into());
    let mut out = dir.join(format!("{stem}.pdf"));
    let mut n = 2;
    while tokio::fs::try_exists(&out).await.unwrap_or(false) {
        out = dir.join(format!("{stem} ({n}).pdf"));
        n += 1;
    }
    crate::fsops::write_bytes(out.clone(), pdf).await?;
    Ok(out)
}

/// "Extract image scans", natively (`tools::scans`): several photos scanned
/// on one sheet are cropped, straightened and written beside the scan as
/// `<stem>-photo-01.<ext>` …. Returns a status line.
pub async fn split_scan(path: PathBuf) -> Result<String, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    let photos = tokio::task::spawn_blocking(move || {
        scans::split_image_file(&bytes, scans::Options::default())
    })
    .await
    .map_err(|_| "the photo splitter crashed".to_string())??;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scan".into());
    let count = photos.len();
    let mut first = None;
    for (suffix, bytes) in photos {
        let (body, ext) = suffix.rsplit_once('.').unwrap_or((suffix.as_str(), "png"));
        let mut out = dir.join(format!("{stem}-{body}.{ext}"));
        let mut n = 2;
        while tokio::fs::try_exists(&out).await.unwrap_or(false) {
            out = dir.join(format!("{stem}-{body} ({n}).{ext}"));
            n += 1;
        }
        crate::fsops::write_bytes(out.clone(), bytes).await?;
        first.get_or_insert(out);
    }
    let first = first.map(|p| p.display().to_string()).unwrap_or_default();
    Ok(match count {
        1 => format!("Found 1 photo: {first}"),
        n => format!("Found {n} photos: {first} …"),
    })
}

pub fn split_status(r: Result<String, String>) -> Event {
    Event::Status(match r {
        Ok(s) => s,
        Err(e) => format!("Split scanned photos failed: {e}"),
    })
}

pub fn status(r: Result<PathBuf, String>) -> Event {
    Event::Status(match r {
        Ok(p) => format!("Converted to {}", p.display()),
        Err(e) => format!("Convert to PDF failed: {e}"),
    })
}

// ---- locked PDFs ----

pub struct Locked {
    pub path: PathBuf,
    password: String,
    error: Option<String>,
    busy: bool,
}

#[derive(Debug, Clone)]
pub enum LockedMsg {
    Password(String),
    Unlock,
    Unlocked(Result<super::pdf::Source, String>),
}

impl Locked {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            password: String::new(),
            error: None,
            busy: false,
        }
    }

    /// Returns the opened source when unlocking succeeds.
    pub fn update(&mut self, msg: LockedMsg) -> (Task<LockedMsg>, Option<super::pdf::Source>) {
        match msg {
            LockedMsg::Password(p) => {
                self.password = p;
                (Task::none(), None)
            }
            LockedMsg::Unlock if !self.busy => {
                self.busy = true;
                self.error = None;
                let (path, pw) = (self.path.clone(), self.password.clone());
                let task = Task::perform(
                    async move {
                        let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
                        let plain =
                            tokio::task::spawn_blocking(move || structure::decrypt(&bytes, &pw))
                                .await
                                .map_err(|e| e.to_string())??;
                        super::pdf::open_bytes(path, plain).await
                    },
                    LockedMsg::Unlocked,
                );
                (task, None)
            }
            LockedMsg::Unlock => (Task::none(), None),
            LockedMsg::Unlocked(r) => {
                self.busy = false;
                match r {
                    Ok(src) => (Task::none(), Some(src)),
                    Err(e) => {
                        self.error = Some(e);
                        (Task::none(), None)
                    }
                }
            }
        }
    }

    pub fn view(&self) -> Element<'_, LockedMsg> {
        let mut c = column![
            text("This PDF is password-protected").size(18),
            text("Opened copies stay unlocked in the viewer; the file on disk only changes if you edit and save.")
                .size(12)
                .color(theme::MUTED),
            text_input("password", &self.password)
                .secure(true)
                .on_input(LockedMsg::Password)
                .on_submit(LockedMsg::Unlock)
                .width(320),
            button(text(if self.busy { "Unlocking…" } else { "Unlock" })).on_press(LockedMsg::Unlock),
        ]
        .spacing(10)
        .align_x(iced::Alignment::Center);
        if let Some(e) = &self.error {
            c = c.push(text(e.clone()).color(iced::Color::from_rgb8(0xff, 0x7a, 0x7a)));
        }
        center(c).into()
    }
}
