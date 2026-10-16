//! Typst as an in-process typesetter: an in-memory [`World`] over one
//! generated source file plus the images it references, compiled straight
//! to PDF bytes. No files are read and no packages fetched.
//!
//! Fonts: GNU FreeFont (Sans/Serif/Mono, GPL-3.0-or-later with the font
//! exception) is compiled into the binary so output looks the same on every
//! OS; the system's fonts are added after them (scanned once, lazily) and
//! only serve as fallback for scripts FreeFont lacks (CJK, …).

use std::collections::HashMap;
use std::sync::LazyLock;
use typst::diag::{FileError, FileResult, Severity};
use typst::foundations::{Bytes, Datetime, Duration, Smart};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};
use typst_kit::fonts::FontStore;

/// Family names of the bundled faces (what generated sources ask for).
pub const SANS: &str = "FreeSans";
pub const SERIF: &str = "FreeSerif";
pub const MONO: &str = "FreeMono";

static BUNDLED: [&[u8]; 12] = [
    include_bytes!("../../../../../assets/fonts/FreeSans.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSansBold.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSansOblique.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSansBoldOblique.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSerif.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSerifBold.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSerifItalic.otf"),
    include_bytes!("../../../../../assets/fonts/FreeSerifBoldItalic.otf"),
    include_bytes!("../../../../../assets/fonts/FreeMono.otf"),
    include_bytes!("../../../../../assets/fonts/FreeMonoBold.otf"),
    include_bytes!("../../../../../assets/fonts/FreeMonoOblique.otf"),
    include_bytes!("../../../../../assets/fonts/FreeMonoBoldOblique.otf"),
];

static FONTS: LazyLock<FontStore> = LazyLock::new(|| {
    let mut store = FontStore::new();
    for data in BUNDLED {
        for font in Font::iter(Bytes::new(data)) {
            let info = font.info().clone();
            store.push((font, info));
        }
    }
    // fallback only: generated sources always name the bundled families
    // first. OG_FILES_NO_SYSTEM_FONTS=1 keeps output strictly hermetic.
    if std::env::var_os("OG_FILES_NO_SYSTEM_FONTS").is_none() {
        store.extend(typst_kit::fonts::system());
    }
    store
});

static LIBRARY: LazyLock<LazyHash<Library>> = LazyLock::new(|| LazyHash::new(Library::default()));

struct Mem {
    main: Source,
    files: HashMap<FileId, Bytes>,
}

fn id(path: &str) -> Option<FileId> {
    Some(RootedPath::new(VirtualRoot::Project, VirtualPath::new(path).ok()?).intern())
}

impl World for Mem {
    fn library(&self) -> &LazyHash<Library> {
        &LIBRARY
    }

    fn book(&self) -> &LazyHash<FontBook> {
        FONTS.book()
    }

    fn main(&self) -> FileId {
        self.main.id()
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            Ok(self.main.clone())
        } else {
            Err(FileError::NotSource)
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files
            .get(&id)
            .cloned()
            .ok_or_else(|| FileError::NotFound(id.get().vpath().get_with_slash().into()))
    }

    fn font(&self, index: usize) -> Option<Font> {
        FONTS.font(index)
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        None
    }
}

/// Compile a Typst source (with `files` addressable by their absolute
/// virtual paths, e.g. "/f/3.png") to PDF bytes.
pub fn compile(source: String, files: Vec<(String, Vec<u8>)>) -> Result<Vec<u8>, String> {
    let main_id = id("/main.typ").ok_or("internal: bad main path")?;
    let mut map = HashMap::new();
    for (path, bytes) in files {
        let fid = id(&path).ok_or_else(|| format!("internal: bad file path {path}"))?;
        map.insert(fid, Bytes::new(bytes));
    }
    let world = Mem {
        main: Source::new(main_id, source),
        files: map,
    };
    let result = typst::compile::<typst_layout::PagedDocument>(&world).output;
    let out = match result {
        Ok(doc) => {
            let opts = typst_pdf::PdfOptions {
                creator: Smart::Custom(Some("og-files".into())),
                ..Default::default()
            };
            typst_pdf::pdf(&doc, &opts).map_err(|d| describe(&world, &d))
        }
        Err(d) => Err(describe(&world, &d)),
    };
    // Typst memoizes layouts process-wide; don't let every converted file
    // stay cached for the app's lifetime.
    typst::comemo::evict(0);
    out
}

fn describe(world: &Mem, diags: &[typst::diag::SourceDiagnostic]) -> String {
    let Some(d) = diags
        .iter()
        .find(|d| d.severity == Severity::Error)
        .or(diags.first())
    else {
        return "typesetting failed".into();
    };
    let mut s = format!("typesetting failed: {}", d.message);
    // generated source: point at the offending snippet to ease bug reports
    if let Some(r) = typst::WorldExt::range(world, d.span) {
        let text = world.main.text();
        if let Some(snip) = text.get(r.start..r.end.min(r.start + 80)) {
            s.push_str(&format!(" (at `{snip}`)"));
        }
    }
    s
}
