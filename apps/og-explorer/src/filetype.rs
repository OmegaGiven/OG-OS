//! Maps a path to the viewer family that handles it.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Folder,
    Text,
    Image,
    Video,
    Audio,
    Table,
    Doc,
    Model3d,
    Archive,
    Binary,
}

impl Kind {
    pub fn of(path: &Path, is_dir: bool) -> Kind {
        if is_dir {
            return Kind::Folder;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "csv" | "tsv" | "xlsx" | "xls" | "ods" => return Kind::Table,
            "pdf" | "docx" | "odt" | "epub" | "md" | "markdown" => return Kind::Doc,
            "gltf" | "glb" | "obj" | "stl" | "ply" | "fbx" | "3mf" => return Kind::Model3d,
            "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z" | "rar" => return Kind::Archive,
            _ => {}
        }
        let mime = mime_guess::from_path(path).first_raw().unwrap_or("");
        match mime.split('/').next().unwrap_or("") {
            "image" => Kind::Image,
            "video" => Kind::Video,
            "audio" => Kind::Audio,
            "text" => Kind::Text,
            _ if mime.contains("json") || mime.contains("xml") || mime.contains("javascript") => {
                Kind::Text
            }
            // No extension or unknown: the text viewer sniffs for UTF-8 and
            // falls back to the binary placeholder itself.
            _ if ext.is_empty() || mime.is_empty() => Kind::Text,
            _ => Kind::Binary,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Folder => "Folder",
            Kind::Text => "Text",
            Kind::Image => "Image",
            Kind::Video => "Video",
            Kind::Audio => "Audio",
            Kind::Table => "Table",
            Kind::Doc => "Document",
            Kind::Model3d => "3D model",
            Kind::Archive => "Archive",
            Kind::Binary => "Binary",
        }
    }

    /// Icon from the bundled Bootstrap Icons font (see `widgets::icon`).
    pub fn glyph(self) -> iced_fonts::Bootstrap {
        use iced_fonts::Bootstrap as B;
        match self {
            Kind::Folder => B::FolderFill,
            Kind::Text => B::FileEarmarkText,
            Kind::Image => B::FileEarmarkImage,
            Kind::Video => B::FileEarmarkPlay,
            Kind::Audio => B::FileEarmarkMusic,
            Kind::Table => B::FileEarmarkSpreadsheet,
            Kind::Doc => B::FileEarmarkRichtext,
            Kind::Model3d => B::Box,
            Kind::Archive => B::FileEarmarkZip,
            Kind::Binary => B::FileEarmarkBinary,
        }
    }

    /// Short text tag (used where an icon font isn't appropriate).
    #[allow(dead_code)]
    pub fn icon(self) -> &'static str {
        match self {
            Kind::Folder => "DIR",
            Kind::Text => "TXT",
            Kind::Image => "IMG",
            Kind::Video => "VID",
            Kind::Audio => "AUD",
            Kind::Table => "TBL",
            Kind::Doc => "DOC",
            Kind::Model3d => "3D",
            Kind::Archive => "ZIP",
            Kind::Binary => "BIN",
        }
    }
}

/// The quick-filter chips under the tab strip (sketch: "3d Models / Table
/// Files / Docs / Images / Videos").
pub const FILTERS: [Kind; 6] = [
    Kind::Model3d,
    Kind::Table,
    Kind::Doc,
    Kind::Image,
    Kind::Video,
    Kind::Audio,
];
