//! "Convert to PDF" for files that aren't PDFs, plus the unlock screen for
//! password-protected PDFs.
//!
//! Images convert natively (`tools::layout::images_to_pdf`). Office
//! documents, HTML, Markdown, e-mail and e-books go through the optional
//! Stirling server (`tools::server::convert_to_pdf`), since they need
//! LibreOffice-class engines.

use super::pdf::tools::{layout, server, structure};
use super::Event;
use crate::filetype::Kind;
use crate::theme;
use iced::widget::{button, center, column, text, text_input};
use iced::{Element, Task};
use std::path::{Path, PathBuf};

const SERVER_EXTS: [&str; 18] = [
    "doc", "docx", "odt", "rtf", "ppt", "pptx", "odp", "xls", "xlsx", "ods", "epub", "html", "htm", "md", "markdown",
    "eml", "txt", "csv",
];

fn ext(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// Can this file be turned into a PDF, and does it need the server?
pub fn convertible(path: &Path, kind: Kind) -> Option<bool> {
    if kind == Kind::Image {
        return Some(false);
    }
    SERVER_EXTS.contains(&ext(path).as_str()).then_some(true)
}

/// Convert and write `<stem>.pdf` next to the file (never overwrites).
pub async fn to_pdf(path: PathBuf) -> Result<PathBuf, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    let is_image = crate::filetype::Kind::of(&path, false) == Kind::Image;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let pdf = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        if is_image {
            return layout::images_to_pdf(&[bytes], None, 0.0);
        }
        let cfg = server::ServerConfig::load()
            .ok_or("this needs the Stirling server — set it up under PDF Tools → Server settings")?;
        let r = server::convert_to_pdf(&cfg, &name, &bytes)?;
        if r.ext != "pdf" {
            return Err(format!("server returned .{} instead of a PDF", r.ext));
        }
        Ok(r.bytes)
    })
    .await
    .map_err(|e| e.to_string())??;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "converted".into());
    let mut out = dir.join(format!("{stem}.pdf"));
    let mut n = 2;
    while tokio::fs::try_exists(&out).await.unwrap_or(false) {
        out = dir.join(format!("{stem} ({n}).pdf"));
        n += 1;
    }
    crate::fsops::write_bytes(out.clone(), pdf).await?;
    Ok(out)
}

/// Server-side "extract image scans": several photos scanned on one sheet
/// come back as separate images (usually a zip), written beside the scan.
pub async fn split_scan(path: PathBuf) -> Result<PathBuf, String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let reply = tokio::task::spawn_blocking(move || {
        let cfg = server::ServerConfig::load()
            .ok_or("this needs the Stirling server — set it up under PDF Tools → Server settings")?;
        server::scanner_image_split(&cfg, &name, &bytes)
    })
    .await
    .map_err(|e| e.to_string())??;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "scan".into());
    let mut out = dir.join(format!("{stem}-photos.{}", reply.ext));
    let mut n = 2;
    while tokio::fs::try_exists(&out).await.unwrap_or(false) {
        out = dir.join(format!("{stem}-photos ({n}).{}", reply.ext));
        n += 1;
    }
    crate::fsops::write_bytes(out.clone(), reply.bytes).await?;
    Ok(out)
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
        Self { path, password: String::new(), error: None, busy: false }
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
                        let plain = tokio::task::spawn_blocking(move || structure::decrypt(&bytes, &pw))
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
