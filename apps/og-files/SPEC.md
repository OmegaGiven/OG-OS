# OG Files — spec

The successor to `og-files`: a file manager whose right side is a set of
tabbed viewers and editors, so browsing a folder and working on what's in it
happen in one window. It has to run as a standalone app on any Linux desktop,
not only inside OG-OS.

Status: **scaffold** (2026-10-11). Built so far: the pane layout, the
files pane (tree, list and grid views, kind filter, live background search,
focus mode), tabs with an explicit edit mode, the inspector, the details log
and three working viewers. Everything else is described here.

The crate is `og-files` while it's being built. When it replaces og-files
at the end of phase 1, it **takes over the `og-files` name** (binary and
app_id) and the old crate gets archived.

```bash
cargo run -p og-files -- ~/Documents               # folder: shown in the files pane
cargo run -p og-files -- notes.md                  # file: opened in a tab
cargo run -p og-files -- --view grid --focus ~/Pictures
cargo run -p og-files -- --search invoice          # start with a search running
og-files extract backup.tar.zst ~/restore          # no window: see §6c
```

---

## 1. Goals

1. **Files left, work right.** The files pane is the main place to see
   files: a tree, list or grid that can widen to fill the window. Whatever
   you click opens in a tab on the right with the tools for that file type.
2. **Handle the common file types in-app.** Video, audio, images, tables,
   documents and 3D models get a built-in viewer and a basic editor, so
   day-to-day work doesn't hand off to `xdg-open` and five other apps.
3. **Never lag.** Scrolling, typing and navigating stay smooth in folders
   with 10k+ entries and on slow or dead network mounts.
4. **Standalone.** One binary that builds without the rest of OG-OS. Suite
   integration (theme, cross-app drag) is an optional extra.

**Not goals:** a pro editor in any category (no NLE timeline, no Photoshop,
no Excel formulas engine, no Blender). "Basic" means quick fixes: trim, crop,
rotate, edit a cell, fix a typo, look at a model.

## 2. Why a rewrite instead of fixing og-files

Audit of `apps/og-files` (5.6k LOC, one commit, no tests):

| Problem in og-files | Why it matters here |
|---|---|
| `refresh_preview()` runs a blocking `get_file_details` (stat, EXIF, image decode, dir count) after **every** message, including scroll, cursor move and a 20 Hz drop-poll tick | This is the scroll lag. Fixing it means touching most `update` arms. |
| Sync disk/subprocess work on the UI thread: watcher refresh, compress/extract, unmount, `statvfs` on network mounts every 2 s | A dead SMB mount freezes the whole app. |
| tiny-skia (CPU) renderer only | Video and 3D need wgpu and shader widgets. |
| `app.rs` is 1.8k lines, flat 85-variant `Message`, hand-placed overlays | The new 4-pane layout would throw away about 70% of it anyway. |
| Trash is a plain `rename` into `~/.local/share/Trash` | Fails across devices (USB drives). |
| Drag-out: stuck drag state, no percent-encoding, surface race | Explains "can't drag files into other apps". |

**Port from og-files** (good code, about 1.3k LOC): `jobs.rs` (threaded
copy/move with progress, conflicts, undo), `watcher.rs`, `thumbs.rs`
(move the stat out of `view`), selection/sort from `tab.rs`, `desktop.rs`
(open-with registry), trash/EXIF helpers, and the og-wayland drag code with
its bugs fixed (§6).

## 3. Layout

From the sketch, as built:

```
┌──────────────────────────┬──────────────────────────┬───────────┐
│ File manager             │ [tab][tab][tab]…          │ Inspector │
│ [Tree|List|Grid] [Focus] │                           │ properties│
│ [Search]                 │      CONTENT VIEWER       │ tools for │
│ All·3D·Table·Doc·Img·Vid │  text · image · table     │ this type │
│ Bookmarks / Devices /    │  (video · audio · doc ·   │           │
│ Network  — or —  search  │   3D coming)              │           │
│ results                  ├──────────────────────────┴───────────┤
│                          │ File details & history                │
└──────────────────────────┴───────────────────────────────────────┘
```

**Panes**
- Every pane is a `pane_grid` pane. Split lines resize, and title bars drag
  so panes can be rearranged. That covers the sketch's "swap direction if
  better" note.

**Files pane** (left). It is the main place to see files, so it shows both
folders and files.
- **Views:**
  - **Tree:** Bookmarks (Folders / Files), Devices (Home, Root, mounted
    drives) and Network (mounted shares). Folders expand in place, and files
    open in a tab.
  - **List:** one folder at a time, with breadcrumb and Up. Shows name,
    size and modified date.
  - **Grid:** the same folder as tiles with **thumbnails**. Thumbnails are
    generated in-process, cached, and made only while the grid is showing.
- **Kind filter** (All / 3D / Table / Document / Image / Video / Audio)
  applies to whatever is visible: expanded tree branches, the list or grid,
  and search results. Folders always stay visible so you can keep drilling
  down.
- **Search** (button or `Ctrl+F`):
  - **First pass:** instant matches among everything already loaded in the
    tree.
  - **Second pass:** a background walk of home, bookmarked folders and
    mounted drives that keeps finding more while you work. It shows a
    "still searching…" status and a count of files scanned.
  - **Results are append-only.** New hits are added at the bottom and never
    sorted or inserted above, so the list never jumps under a user who has
    scrolled.
  - Typing a new query cancels the old walk.
- **Size:**
  - `Ctrl+B` hides the pane.
  - **Focus** widens it to about 85% of the window for pure browsing and
    searching. It also hides the inspector so nothing gets squeezed.
  - Opening a file from focus mode returns to the normal layout.

**Viewer, inspector and details**
- **Tabs:** one per file, and reopening a file focuses its existing tab.
  Folders don't get tabs: browsing happens in the files pane.
- **Edit mode:** files open **read-only**. Editing is an explicit mode, by
  the Edit button or `Ctrl+E`:
  - The tab shows `EDIT` while editing, and `*` once there are unsaved
    changes.
  - **Save** (`Ctrl+S`) **writes over the original file**, atomically.
  - **Discard** reloads the file from disk.
  - Leaving edit mode or closing the tab is refused while there are unsaved
    changes.
  - Every viewer that can edit follows this same model: text and markdown
    now, tables, docs and images later.
- **Video is the exception:** edits (trim, snapshot) are **exported as a new
  file** next to the original. The default name is `<name>-edit.<ext>` and
  can be changed. The source video is never modified.
- **Inspector:** properties of the focused tab, plus a per-type tools area.
- **Details & history:** an activity log of what you opened, saved,
  discarded and any errors (§8).

## 4. Architecture

```
src/
  main.rs           iced::application entry
  app.rs            pane_grid shell, shortcuts, routes child events
  sidebar.rs        files pane: header, view mode, kind filter, focus, search box
  tree.rs           tree view: sections, lazy nodes, cached flattened rows
  browse.rs         list + grid views of one folder, breadcrumb, thumbnails
  search.rs         two-pass live search, append-only results, cancellable walk
  thumbs.rs         in-process thumbnail generation + disk cache (one worker)
  tabs.rs           tab strip, tabs addressed by stable id
  inspector.rs      metadata of focused tab + tools area
  details.rs        details & history strip
  viewers/
    mod.rs          Viewer enum, async loader, dispatch
    text.rs         text_editor: read-only / edit mode, save-over, discard
    image.rs        fit / pan-zoom
    table.rs        CSV/TSV grid (own parser for now)
    placeholder.rs  stand-in that names each planned backend
  fsops.rs          all filesystem access, async, never called from view()
  filetype.rs       path → Kind (viewer family)
  openwith.rs       desktop entries, MIME (globs2 + magic), mimeapps.list, Exec codes, launch
  terminal.rs       terminal lookup + per-terminal working-dir / exec flags
  archops.rs        archive extract / compress as jobs (Details list, cancel, undo)
  cli.rs            subcommands that run without a window (§6c)
  config.rs         XDG config/cache, bookmarks.json, settings.json
  theme.rs          standalone palette
```

**Hard rules.** These are the lessons from og-files. Review every PR
against them.

1. `view()` never touches the filesystem, spawns a process or sorts/filters
   a big list. Derived lists (tree rows, folder `visible`) are cached and
   rebuilt only when their inputs change.
2. `update()` never blocks. Disk, network and subprocess work goes through
   `Task::perform` + `spawn_blocking` (see `fsops::blocking`).
3. Async results carry their target (tab id, path), and stale results are
   dropped (see `Tabs::update`, `Inspector::update`).
4. Lists that grow while visible (search results) are append-only, so a
   scrolled user's position never moves.
5. Messages are nested per component (`Message::Tabs(tabs::Msg::Viewer(id,
   viewers::Msg::Text(..)))`). There is no flat god-enum.
6. Subscriptions are event-driven (channels, `Subscription::run`). No
   polling ticks: og-files ran three of them.
7. Writes are atomic (temp file + rename). Editing is explicit (edit mode)
   and saves overwrite in place, except video, which exports a new file.

**Viewer contract.** Each viewer family is a module that provides:

- `supported(path) -> bool`, used when a family covers only some formats
  (for example table = csv/tsv until calamine lands).
- A loader arm in `viewers::load` that produces plain-data `Loaded`. It must
  be `Clone`, because iced messages must be.
- `State::new(Loaded data)`. Live state may hold non-Clone things such as
  `text_editor::Content`, decoders or GPU buffers.
- `update(Msg) -> (Task, Option<Event>)`, where `Event` is `Open` or `Status`.
- `view() -> Element`.
- An optional `save()`, plus `is_dirty()` via `Viewer::is_dirty`.

Dispatch is an enum, not trait objects. That keeps iced's generic Element
types simple. Adding a family means adding an enum arm in `viewers/mod.rs`.

## 5. File types: viewers, editors, backends

"In-app, no outside dependencies" means **no shelling out to other
programs** (og-files called ffmpeg, pdftoppm, rsvg-convert, tar, zip,
udisksctl, lsblk, xdg-open). Pure-Rust crates are preferred. Two families
can't reasonably be pure Rust:

- **Video:** no production pure-Rust H.264/HEVC decoder exists. Use
  GStreamer as a *library*. It's on every desktop distro already and handles
  hardware decode.
- **PDF:** solved in pure Rust after all: `hayro` renders and `lopdf`
  edits. See §5a.

| Family | Formats | View | Basic edit | Backend (crate) | Phase |
|---|---|---|---|---|---|
| Folder (files pane) | dirs | tree / list / grid + thumbnails, kind filter ✅ | rename, new, delete via jobs | std + ported `jobs.rs` | 0 ✅ / 1 |
| Text / code | txt, md, json, toml, rs, … | ✅ read-only / edit mode | ✅ edit, save over, discard; find/replace, syntax colours | iced `text_editor` ✅, `syntect` or iced `highlighter` | 0 ✅ / 2 |
| Image | png, jpg, webp, gif, bmp, tiff, svg, avif | ✅ fit + pan/zoom | crop, rotate, resize, exposure; save over (edit mode) | `image` ✅, `resvg` (svg), `kamadak-exif` | 0 ✅ / 3 |
| Table | csv, tsv ✅; xlsx, xls, ods | ✅ read-only grid | edit cells, add/remove rows, sort; write csv | own parser ✅ → `csv`, `calamine` (read xlsx/ods), `rust_xlsxwriter` | 0 ✅ / 2 |
| Doc | md ✅; **pdf ✅**; **docx ✅**; odt, rtf, epub ✅ (read) | PDF ✅ pages + thumbnails + zoom; md rendered preview; **docx ✅ page view** (rich text, lists, tables, images); odt/rtf/epub reader | PDF ✅ page editing (§5a); md source edit; **docx ✅ Word-style editor (§5b)** with lossless round-trip | `hayro` ✅ (render), `lopdf` ✅ (edit), `zip` + `quick-xml` ✅ (docx/odt/epub), iced advanced text ✅ (docx editor widget) | 0 ✅ / 2 ✅ |
| Audio | mp3, flac, ogg, opus, wav, m4a/aac | waveform, play/seek, tags | trim, normalize, edit tags, export wav/flac | `symphonia` (decode), `cpal` (output), `lofty` (tags), `hound` (wav write) | 2 |
| Video | mp4, mkv, webm, mov, avi, … | ✅ embedded **libmpv** (loaded at runtime): GPU decoding (`hwdec=auto-safe`, Vulkan/VAAPI), subtitles, track menus, A/V sync, speed with pitch correction, chapters, A-B loop, folder playlist, resume, stats, mpv keyboard shortcuts, fullscreen. No letterbox: the picture is drawn at its fitted size on the app background with an optional edge line | ✅ trim (stream copy) + frame snapshot, always **new files** | libmpv SW render → one wgpu texture per player; FFmpeg (ffmpeg-next) fallback, thumbnails and trim | ✅ (zero-copy GPU display: later) |
| 3D model | glTF/glb, obj, stl, ply | orbit/zoom, wireframe, normals, bounding-box size | none (view only; maybe re-export stl→glb) | `gltf`, `tobj`, `stl_io`, iced `shader` widget on wgpu | 3 |
| Archive | zip, tar(.gz/.xz/.zst/.bz2), 7z, single .gz/.xz/.zst/.bz2 | ✅ browse as a folder | ✅ extract here / to folder, compress to zip · tar.gz · tar.zst · tar.xz · 7z, as jobs (§6b) | `zip`, `tar`, `flate2`, `xz2`, `zstd`, `bzip2`, `sevenz-rust2` | ✅ |
| Binary | anything else | hex view (virtualized) | none | own | 2 |

**Renderer:** og-files uses iced's default features (wgpu first, with
tiny-skia as fallback). Video and 3D need wgpu. On machines without a GPU
those two viewers show a "needs GPU" placeholder and everything else works.

**Large files:** the text viewer caps at 16 MB (`viewers::TEXT_LIMIT`) and
the table viewer at 5,000 rows. Phase 2 replaces both caps with windowed or
streamed loading.

## 5a. PDF toolkit (Stirling-PDF, rebuilt natively)

The goal is to bring Stirling-PDF's feature set (a self-hosted Stirling-PDF
server, optionally) into the file manager.

**Licensing.** The fork's only change from upstream is unlocking the
enterprise features. Stirling's `app/core` is MIT. `app/proprietary` is
under a commercial license, so **nothing is ported from it**. Everything
here is a fresh Rust implementation on different libraries; no Java is
translated.

**Engine**

| Part | Crate | Notes |
|---|---|---|
| Render | `hayro` 0.7 (pure Rust, MIT/Apache, CPU) | One worker thread per open PDF that keeps its parse and font cache (`viewers/pdf/engine.rs`). Panics are caught per page. Bitmaps are capped at 40 MP. Limits: no blending/knockout groups yet, and password-protected files fail to open. |
| Edit | `lopdf` 0.45 (pure Rust, MIT) | `compose::build(sources, plan)` turns a plan of (source, page, rotation) into a new PDF. That single primitive covers reorder, delete, rotate, duplicate, merge/insert, extract and split. Inherited page attributes are pushed down, and unused objects are pruned. |
| Vision | `ocrs` 0.13 on `rten` 0.26 (OCR), `rqrr` 0.11 (QR), `imageproc` 0.27 (photo segmentation); all pure Rust | Offline and in-process. The OCR detection and recognition models (CC BY-SA 4.0, 12.2 MB) are compiled in from `assets/ocr/`; source, sha256 and license are in its README. Pages are rendered with hayro. |
| Convert from PDF | `tools/extract.rs` + `tools/convert/` (pure Rust, in-process) | hayro interprets each page into a recording device (glyphs with Unicode, font, size, colour; images; rules), then a layout pass builds lines, columns, paragraphs, headings, lists and tables that eleven writers turn into files. See *Convert from PDF* below. |
| Signatures | RustCrypto: `rsa` 0.9, `p256`/`p384` 0.13, `sha1`/`sha2`, `der` + `x509-cert` 0.2, `pkcs8`, `p12-keystore` 0.2 (all pure Rust, MIT/Apache) | CMS built and parsed with a small DER reader/writer in `tools/sign.rs`. |
| Heavy tools | Stirling REST API on a self-hosted server (optional) | For jobs that need Tesseract, Ghostscript, qpdf or LibreOffice. The app uploads the file and saves the result next to the original. Config goes in `$XDG_CONFIG_HOME/og-files/stirling.json` (`{ "url": "https://stirling.example.ts.net:8081", "api_key": "…" }`, sent as `X-API-KEY`). Without it those tools are just hidden, so the app stays standalone. |

**Model.** The same rules as every other viewer apply: view mode is
read-only, and **Edit pages** (`Ctrl+E`) enables page operations. **Save**
writes over the original and **Discard** reloads it. Exports (extract,
split, and any tool that makes a different document) always write
**new files** next to the original (`<name>-pages-1-3.pdf`,
`<name>-part-01.pdf`), work in either mode, and include unsaved edits.

**Tools panel.** The **Tools** button in the PDF toolbar opens 55 tools
in five groups. Each tool has a form, and the page fields default to the
ticked pages. The buttons are:
- **Apply** (edit mode): the result becomes the working copy. Save
  overwrites, Discard undoes.
- **Save as new file**: writes `<name>-<tool>.pdf` beside the original.
- **Run / Save files**: for reports and tools that produce other files.

Other PDF toolbar additions: **Save copy**, plus **Duplicate** and **Blank
page** in edit mode. Outside the PDF viewer: password-protected PDFs open
behind an unlock screen. Images get **Make PDF** and **Split
scanned photos** (both native). Office, HTML, Markdown and e-book files get
**Convert to PDF** (server).
behind an unlock screen. Images get **Make PDF** (native) and **Split
scanned photos** (server). Office documents, spreadsheets, presentations,
HTML, Markdown, plain text, e-mail, e-books, comics and SVG get **Convert
to PDF** (native, see "Anything → PDF" below) from the placeholder view and
from the files pane's context menu (any selection of supported files).

**Status: all Stirling core tools are built** (2026-10-11). "native" means
in-process Rust; "server" means the Stirling REST API.

| Group | Tools | Engine |
|---|---|---|
| Page editing | view, thumbnails, zoom, rotate, delete, reorder, duplicate, blank page, insert PDF, extract, split every N, select | native (plan + compose) |
| Organize | split by ranges / bookmarks / file size, select pages by range, remove blank pages, crop, n-up (2–16), booklet, fit to paper, scale, split into sections, one long page, overlay PDF, insert images | native |
| Organize | insert web page | server |
| Vision | **OCR → searchable PDF** (invisible text layer, skip or redo pages that have text, page range), **auto-split at QR separator pages** (Stirling's sheet or any QR code; duplex drops the blank back), **split scanned photos** (PDF pages, and image files from the image viewer) | native |
| Organize | insert web page (fetch + typeset, see below) | native |
| Organize | auto-split at QR pages | server |
| Add & edit | page numbers, text stamp, watermark (centred / tiled), image stamp, **drawn signature** (canvas pad + click to place), attachments, bookmarks editor, metadata editor / wipe | native |
| Security | AES-256 password + permissions, remove password, sanitize (JS, files, XMP, info, links, fonts), redact text, **redact areas** (drag boxes on the page, true removal by rasterizing), remove annotations / images, flatten forms, flatten to images, unlock forms, remove signatures, show JavaScript | native |
| Security | sign with certificate (PAdES B-B; PKCS#12 or PEM; RSA / ECDSA P-256 / P-384), validate signatures | native (`tools/sign.rs`) |
| Convert & extract | pages → PNG/JPEG, extract images, extract attachments, document info, compare (text diff), auto-rename | native |
| Convert & extract | PDF → Word/ODT/Excel/PowerPoint/HTML/Markdown/text/RTF/XML/CSV/EPUB | native |
| Convert & extract | Office/HTML/MD/EML/e-book/image → PDF | server |
| Convert & extract | Office/HTML/MD/text/EML/e-book/comic/SVG/image/web page → PDF | native (Typst, `tools/topdf`) |
| Convert & extract | PDF → Word/ODT/Excel/PowerPoint/HTML/Markdown/text/RTF/XML/CSV/EPUB | server |
| Optimize & colour | lossless compress, repair, greyscale, invert, adjust colours, fake scan | native |
| Optimize & colour | strong compress (Ghostscript/qpdf), repair via server, PDF/A-2b | server |

**Convert from PDF** (Tools → *Convert PDF to…*, `tools/convert`). No
server, no external programs. Files are written next to the PDF as
`<name>-converted.<ext>`.

How the text is found (`tools/extract.rs`):
- **Capture.** hayro's interpreter drives a recording `Device`. Text
  decoding is hayro's: ToUnicode CMaps, simple-font encodings with
  Differences, the standard encodings, and Identity-H CID fonts. Ligatures
  are split (ﬁ → fi). Fonts are matched to their resource dictionaries by
  hayro's cache key, which gives the family name (cleaned, e.g.
  `ABCDEF+TimesNewRomanPS-BoldMT` → Times New Roman) and bold, italic and
  monospace from the name, descriptor flags, weight and italic angle.
  Invisible (OCR) text is kept and flagged. Fill+stroke and overprinted
  duplicates are removed.
- **Images.** Plain DeviceRGB/Gray JPEGs are passed through unchanged.
  Everything else is decoded by hayro and saved as PNG (alpha kept).
  The same image drawn twice is stored once.
- **Layout.** Baselines are clustered into lines (superscripts and
  subscripts join their line). Words come from spaces or gaps over 0.15 em.
  Lines split into segments at large gaps. *Ruled tables* come from grids
  of drawn rules (merged cells too). *Unruled tables* come from runs of
  lines whose segments fall into consistent column bands. Prose columns
  are excluded, except a long description column on the right. The
  reading order is a recursive XY-cut that compares the widest gutter with
  the widest horizontal gap, so two-column pages read column by column.
  Then come paragraphs (spacing, indent, size or weight change, short last
  line, TOC leader lines, code lines kept apart), alignment, headings
  (size ≥ 1.15 × the body size, levels by size, or short bold lines) and
  lists (bullets, `1.` `a)` `(iv)`, nesting by indent).

What each format gets:
- **Word / ODT / RTF:** Heading 1–3 styles, runs with bold, italic, size,
  colour and font, alignment, native numbered and bulleted lists (each
  ordered list restarts at the PDF's number), tables (borders when the
  PDF had them, merged cells), images, a page break per PDF page, and the
  PDF's page size and margins.
- **PowerPoint:** one slide per page. The slide picture is the page
  rendered *without its text* (text operators are stripped with lopdf).
  The text goes on top as editable text boxes in place, one per paragraph
  or table cell, keeping its line breaks. OCR text stays transparent. If
  the file can't be rewritten (encrypted), the full render is used with
  transparent text boxes.
- **HTML:** one self-contained semantic file (h1–h3, p, nested ul/ol,
  table with th, inline images).
- **EPUB:** EPUB 3 with an NCX. Chapters start at level-1 headings, or
  one per page.
- **Markdown:** GFM tables. Images are written as separate
  `<name>-converted-img-NN.*` files and linked.
- **Text:** reading order, tables tab-separated, a form feed between
  pages.
- **Excel:** one sheet per table (numbers stored as numbers, bold header
  row frozen, merged cells). With no tables, one sheet per page with one
  row per line.
- **CSV:** one file per table (`<name>-page02-table1.csv`). With no tables,
  every line as (page, text).
- **XML:** document → page → heading / paragraph / list-item / table /
  image → line → span, with positions (pt, top-left origin), fonts,
  sizes, colours and flags.

On scanned pages with an OCR layer, the reflowed formats show the OCR
text and drop the page-sized scan. A PDF with no text at all is refused
("run OCR first"), except for PowerPoint.

Limits: right-to-left and vertical scripts come out in visual order.
Rotated text becomes separate lines. Type3 glyph widths are estimated.
Text that is clipped or hidden under an image is still extracted.
Running headers and footers are kept as paragraphs. Unruled-table
detection is heuristic: a numbered code listing can come out as a
two-column table, and a sparse table can come out as text. Equations come
out as plain text.

**Anything → PDF** (`tools/topdf/`, replaces the server's `file/pdf`,
`html/pdf`, `markdown/pdf`, `eml/pdf`, `svg/pdf`, `ebook/pdf`, `cbz/pdf`,
`cbr/pdf` and `url/pdf`). Every input becomes a small document model
(`ir.rs`: headings, paragraphs of styled runs, labelled list items, lists,
tables with spans/repeated headers, images, quotes, code, page breaks,
absolutely placed boxes for slides), which is emitted as Typst source and
typeset **in-process by Typst 0.15** (`typst` + `typst-pdf` as libraries;
`engine.rs` is an in-memory `World` — no files, no packages). All user text
enters Typst as string literals, never markup. Headings become the PDF
outline; the document title goes into the PDF metadata. Fonts: GNU
FreeFont (Sans/Serif/Mono, GPL-3.0-or-later + font exception, 6.7 MiB) is
compiled in so output is identical on every OS (`assets/fonts/README.md`);
system fonts are only a glyph fallback (CJK…; `OG_FILES_NO_SYSTEM_FONTS=1`
turns that off). Images are validated (and BMP/TIFF re-encoded) before they
reach Typst, so one broken picture never fails a document.

| Input | Route | Fidelity / limits |
|---|---|---|
| .docx | the editor's DOCX model (§5b): styles resolved, run b/i/u/strike/size/colour, alignment, spacing, indents, Word list numbering, tables (grid widths, column spans, nested tables), inline images, hyperlinks, page breaks, last section's page size + margins, `dc:title` | no headers/footers, footnote text, floating positions, text boxes, vertical merges, charts |
| .odt .rtf | document viewer's block model (`doc.rs`) | headings, bold/italic, lists, tables; no images |
| .txt .text .log | monospace (layout kept), BOM/UTF-16/Latin-1 aware | `office::text(_, false)` reflows instead |
| .md | pulldown-cmark → HTML → HTML path; images relative to the file | GFM tables, task lists, footnotes, strikethrough |
| .html .htm .xhtml | html5ever (`scraper`) DOM → document | semantic tags, inline `style` basics (colour, weight, italic, decoration, size, text-align, display:none), `<font>`, `bgcolor` cells, local + `data:` images, inline `<svg>`; **not a browser**: no stylesheets, scripts, floats, positioning; layout tables (one cell/column, no borders) are unwrapped |
| web page (Insert web page) | `ureq` (10 s connect, 30 s total, 16 MB page, ≤300 images / 128 MB) → HTML path, links absolutised | JavaScript-built pages come out empty |
| .eml | `mail-parser`: Subject heading, From/To/Cc/Reply-To/Date block, HTML (or text) body with `cid:` images, attachment list + picture attachments | |
| .xlsx .xlsm .xls .xlsb .ods .csv .tsv | the sheet viewer's loaders: one section per sheet (outline entry), header row repeated and shaded, numbers right-aligned, column widths from content, landscape when wide, proportional columns + smaller text when still too wide | values only (no cell formatting/formulas); ≤10 000 rows/sheet, 400 000 cells |
| .pptx | slide size pages; text boxes/pictures/tables/groups at their EMU positions, placeholders positioned from layout/master, run b/i/u/strike/size/RGB, alignment, bullets/auto-numbers, solid fills and slide backgrounds | no theme colours/fonts, gradients, shape geometry, charts, SmartArt |
| .odp | page layout size; frames (text boxes, images), custom shapes with text, automatic text styles | same limits |
| .svg | page = drawing size, embedded as vector (krilla-svg, svg2pdf's successor inside typst-pdf) | |
| .epub | spine order, XHTML chapters through the HTML path, package images, page break per chapter, nav/NCX labels as chapter headings where missing | |
| .mobi .azw .azw3 | `mobi` crate (PalmDOC/HUFF), markup → HTML path, `recindex` / `kindle:embed` images | DRM fails; KF8-only books are best effort |
| .fb2 | XML → HTML (sections, titles, emphasis, poems, epigraphs, base64 binaries) | |
| .cbz .cbr .cb7 .cbt | archive images natural-sorted → existing images → PDF (page per image); RAR via the pure-Rust `rars` crate (RAR 1.3–7, solid) | ≤2 GB of pages |
| .doc .ppt (binary) | — | refused with "save as .docx/.pptx" |
| Convert & extract | PDF → Word/ODT/Excel/PowerPoint/HTML/Markdown/text/RTF/XML/CSV/EPUB; Office/HTML/MD/EML/e-book/image → PDF | server |
| Optimize & colour | lossless compress, **compress levels 1–9** (image downsampling by placed resolution), repair, **PDF/A-2b** + verifier, greyscale, invert, adjust colours, fake scan | native (`tools/compress.rs`, `repair.rs`, `pdfa.rs`) |
| Optimize & colour | OCR (Tesseract) | server |

**Native compress / repair / PDF/A / certificate signatures** (2026-10-16,
pure Rust; no Ghostscript, qpdf, Java or OpenSSL). Each tool that changes
the document also shows a report in the panel (`JobOut::PdfReport`).
- **Compress (images)**, levels 1–9 like Stirling. 1: merge duplicate
  objects (hash + compare), Flate any uncompressed stream, prune, write
  object + xref streams. 2: also recompress Flate/LZW/ASCII/RunLength
  streams at level 9 and drop page thumbnails and `/PieceInfo`. 3–9: also
  images, targets 300/220/150/150/120/96/72 dpi at JPEG quality
  90/85/80/70/60/50/40. The effective resolution of every image comes
  from the CTM at each `Do` on every page (through Form XObjects); the
  smallest placement wins. Images in patterns or annotation appearances,
  with colour-key masks, Decode arrays, CMYK/Indexed/16-bit/1-bit data,
  or JPX/JBIG2/CCITT data are never resampled. Photos become JPEG; line
  art and masked images stay Flate, and a soft mask is resized with its
  image. Optional greyscale converts images and `rg/RG/k/K`. The result
  must parse, keep the page count, render every page the input rendered,
  and be smaller. Otherwise the original is kept, and the report says why.
  Measured on local manuals: 6.6 MB colour-management guide → 58 % (L2),
  1.5 MB manual → 29 % (L5), 3.2 MB image-heavy readme → 16 % (L5) / 8 %
  (L9), 1.4 MB text manual → 61 % (L1); page renders unchanged at 30 dpi
  (mean |Δ| ≤ 0.01).
- **Repair** scans the bytes for `n g obj … endobj` and ignores the xref.
  Later copies win, and object streams are unpacked in place. Wrong
  `/Length`s are re-measured against `endstream`, objects cut off by a
  truncated tail are dropped, and `/Root` comes from the last trailer or
  xref-stream dict, else from a `/Type /Catalog` scan. lopdf parses that
  synthetic file. The plain lopdf reading wins only if it yields more
  pages. Then dangling references are nulled, the page tree is rebuilt,
  and objects are renumbered. Every page is rendered with hayro, and
  pages that still fail are listed.
- **PDF/A-2b** (best effort): `%PDF-1.7` + binary comment, trailer `/ID`,
  an sRGB `GTS_PDFA1` output intent (ICC v2 profile generated in code), and
  `/DefaultCMYK` (bundled CC0 CGATS profile, `assets/icc/`) when DeviceCMYK
  is used. XMP (`pdfaid` 2/B, Dublin Core) is kept in sync with Info and
  stays unfiltered. It removes JavaScript, forbidden actions, `/AA`,
  embedded files, XFA, `NeedAppearances`, media/3D annotations, image
  `Interpolate`/`Alternates`/`OPI`, PostScript/reference XObjects,
  transfer functions, external streams and LZW. Annotations become
  printable and keep only `/AP /N`. Non-embedded standard-14 fonts, and
  TrueType Arial/Times New Roman/Courier New stand-ins, get the
  metric-compatible Foxit CFF fonts that hayro already compiles in
  (`FontFile3 /Type1C`), with `/Widths` from the program (checked against
  the AFM metrics in tests), a full FontDescriptor and ToUnicode.
  A built-in verifier (`pdfa::verify`) runs on the result, and remaining
  violations are listed. It checks header, ID, encryption, XMP + Info
  consistency, output intent/ICC header, device colours vs. intent, font
  embedding (+ CIDToGIDMap), width consistency of embedded Type 1/CFF
  programs for the codes the pages show, actions, annotations,
  forms, embedded files, LZW/external streams, image/XObject/ExtGState
  rules, optional content and name/string limits. It does not check
  content-stream operators, glyph presence, CIDSet/CharSet, TrueType cmap
  tables or XMP extension schemas, so use veraPDF for a formal check.
  Can't fix: other non-embedded fonts (Calibri, CID fonts…), annotations
  without appearance streams, widths that already disagree with an
  embedded font.
- **Sign with certificate**: `.p12`/`.pfx` (legacy 3DES/RC2 or AES PBES2)
  or PEM (certificate plus `PRIVATE KEY` / `ENCRYPTED PRIVATE KEY` / `RSA
  PRIVATE KEY` / `EC PRIVATE KEY`, in one file or with a separate key
  file). Keys: RSA (PKCS#1 v1.5, SHA-256), ECDSA P-256 (SHA-256), P-384
  (SHA-384). The signature is written as an **incremental update**, so
  earlier signatures stay valid. It adds a merged `/FT /Sig` field +
  widget, either invisible or visible (signer/date/reason/location box at
  a chosen page and corner). The signature dict carries
  `/SubFilter /ETSI.CAdES.detached`, `/ByteRange` and a fixed-width
  `/Contents` reserve. The CMS SignedData is detached, with content-type,
  message-digest and signing-certificate-v2 attributes, and embeds the
  signer certificate and chain. Per PAdES, the signing time goes in `/M`,
  not in a CMS attribute. There is no timestamp (B-T needs a TSA).
  Verified by the built-in validator, and in an `#[ignore]` test by
  poppler `pdfsig` and `openssl cms -verify`.
- **Validate signatures**: for each signature it checks byte-range sanity,
  that the gap is exactly `/Contents`, the digest against
  message-digest, content-type, and the CMS signature (RSA PKCS#1 v1.5 /
  PSS, ECDSA P-256/P-384; SHA-1/256/384/512). It also checks
  `adbe.pkcs7.sha1` content, whether the signature covers the whole
  file (or how many bytes were appended), and signer/issuer, validity at
  signing time. It reports whether the certificate is self-signed and
  walks the embedded chain, verifying each link's signature. An RFC 3161
  timestamp is checked too (its own CMS signature plus the message
  imprint). Trust isn't evaluated, because there is no certificate store.
  `adbe.x509.rsa_sha1` is reported as unsupported.

**Server endpoints** (paths and fields taken from upstream `app/core` only,
listed in `tools/server.rs`): `/api/v1/misc/{compress-pdf, repair}`,
`/api/v1/convert/{pdf/pdfa, file/pdf, html/pdf,
markdown/pdf, eml/pdf, img/pdf, svg/pdf, ebook/pdf, url/pdf}`,
`/api/v1/security/{cert-sign, validate-signature}`, and
`/api/v1/info/status` for **Test connection**. (`server::convert_from_pdf`
and its `pdf/word … pdf/epub` endpoints are no longer called: PDF → other
formats is native.) The URL and API key are
entered in Tools → Server settings and saved to `stirling.json` (mode 600).

**Known limits.** Each module's doc comments list them per function. The
main ones:
- Rebuilding pages (n-up, booklet, scale, overlay…) drops those pages'
  annotations, and the outline of the result.
- Page edits and every tool save without encryption. Re-apply a password
  last.
- Redact text handles simple single-byte fonts only. Text in CID fonts is
  reported as "not found", so use Redact areas for those.
- Raster tools (colour filters, redact areas, flatten to images) remove
  text selection on the affected pages.
- The signature box assumes an unrotated page.
- OCR (`tools/ocr.rs`) reads Latin script only: A–Z, a–z, digits and
  ASCII punctuation. Accented letters come out unaccented; other scripts
  (Cyrillic, Greek, CJK, Arabic…) are refused with a message. Pages render
  at 150 dpi, where the models read best (body text 20–40 px tall; higher
  dpi measurably dropped letters). Very small print (< 7 pt) is unreliable.
  Text is assumed horizontal, so skewed lines get an axis-aligned box.
  The layer is a Type 3 font with empty glyphs and a ToUnicode CMap,
  drawn in render mode 3 inside its own `q … Q` after the untouched
  original content, and it follows `/Rotate` and the CropBox. "Redo pages
  that already have text" doesn't remove the old text, so those pages can
  extract twice. There is no progress bar or cancel (the job system has
  neither). A sparse page takes well under a second; dense pages take longer.
- Auto-split renders each page at 200 dpi; QR codes under ~1.5 cm can be
  missed. By default only Stirling's separator sheet splits.
- Split scanned photos needs a background gap of about 2 mm (at 300 dpi)
  between photos. Photos with large paper-coloured edges can be cropped
  short, and tilt is straightened up to ±45°.
- Decrypt can't tell whether a password was actually needed.
- Phase 2: a `compose::build_from_doc` variant so split-by-size doesn't
  re-parse the file on every probe.

Pipelines and "recipes" (saved tool chains run on a selection) are still
open: phase 3.

**Tests (62 + 18 for anything → PDF).** `cargo test -p og-files` covers:
- `tools/topdf/tests.rs`: one test per input type from tiny generated
  fixtures (docx/pptx/odp/epub/eml/mobi/fb2/comics built in memory, a local
  HTTP server for web pages): output parses, page count, page size, text,
  images, outline and title are checked;
- compose, pages, and every public function of structure, layout, raster
  and server (server tests use a local mock HTTP server, no network);
- the converters (`tools/convert/tests.rs`): a generated three-page PDF
  (standard-14 and CID/ToUnicode text, two columns, a heading, a list, a
  ruled and an unruled table, an image) checks the reading order, the
  headings and the tables found, and that every output parses. That
  means zip structure, well-formed XML parts, xlsx read back with
  calamine, docx reopened by the app's docx parser, and docx/odt/rtf/epub
  read by the app's document viewer. The ignored `soffice_opens_outputs`
  test converts the office outputs with LibreOffice when it is installed.
  `OGX_PDF=… OGX_DEMO_DIR=… cargo test -p og-files convert_real_file --
  --ignored` writes every format for a real file;
- `every_native_tool_runs_end_to_end`, which drives all native tools
  through the UI path (form → job → output) and checks that each result
  parses.

Test hooks:
- **Real files:** `OGX_PDF=file.pdf cargo test -p og-files real_file`.
- **Visual check:** `OGX_PDF=… OGX_DEMO_DIR=/tmp/out cargo test -p
  og-files demo_outputs` writes sample outputs for eyeballing.
- **Screenshots:** `OGX_TOOL="Watermark" og-files file.pdf` opens the
  Tools panel on that tool (`OGX_TOOL=""` opens the list).

## 5b. Word (.docx) editor

`viewers/docx/` (model, parse, write, styles, widget). `.docx` opens here;
a package the editor can't model safely (not a zip, no prefixed
WordprocessingML, broken XML) falls back to the read-only reader in
`viewers/doc.rs`. ODT/RTF/EPUB stay in `doc.rs`.

**View mode** (default) shows the document as a page: an off-white sheet
(816 px = Letter width, 1-inch margins, narrower panes shrink the margins)
on the dark pane, with headings/Title from the document's own styles
(size, colour, bold via `basedOn`), bold/italic/underline/strike, run
sizes and colours, alignment (per visual line), indents, paragraph
spacing, bullets and numbers (numbering.xml formats, per-numId counters),
tables (tblGrid widths, gridSpan), embedded pictures (PNG/JPEG/GIF/BMP/
WebP/TIFF; others show an `[image]` chip), hyperlinks, page breaks and
content controls (e.g. a TOC) as muted read-only blocks. Text is selectable
(click, drag, double/triple click, Shift+click) and Ctrl+C / Ctrl+A work;
arrows / PageUp / PageDown / Space / Home / End scroll.

**Edit mode** (Ctrl+E / Edit) adds a caret and a toolbar: style menu
(Normal, Title, Subtitle, Heading 1–4, Quote), B / I / U / S toggles that
reflect the caret or selection, bulleted / numbered list toggles, left /
centre / right / justify, undo / redo, plus a live word count. Keys:
typing, Backspace / Delete (Ctrl = by word; at a paragraph edge they merge
paragraphs; Backspace at the start of a list item removes the bullet),
Enter splits (Enter on an empty list item ends the list; Enter at the end
of a heading continues in Normal), Shift+Enter = line break (`w:br`), Tab
= next table cell (selects it; Shift+Tab back) or a tab character,
arrows / Ctrl+arrows by word / Home / End (visual line) / PageUp / PageDown
/ Ctrl+Home / Ctrl+End, all with Shift to select, Ctrl+B/I/U (on the
selection, or as the typing style at the caret), Ctrl+Z / Ctrl+Shift+Z /
Ctrl+Y, Ctrl+X / Ctrl+C / Ctrl+V (plain text; pasted lines become
paragraphs). Table cells are edited in place (caret moves into cells).
While the page has keyboard focus every plain key goes to the editor —
Delete/F2/Backspace/letters never reach the files pane; Ctrl+S, Ctrl+E,
Ctrl+W still reach the app. Escape or clicking elsewhere releases focus.
Save (Ctrl+S) writes over the original atomically (temp file + rename);
leaving edit mode with unsaved changes is refused (Save or Discard);
the tab shows the modified dot and close asks first.

**Round-trip guarantee.** The model keeps the original document.xml text
and, for every block, its byte span. Saving rebuilds document.xml:
untouched paragraphs, tables and unknown elements (sectPr, bookmarks,
content controls, anything else) are copied **byte for byte**, including
the whitespace between them; only paragraphs that were actually changed
are regenerated, reusing their original `<w:p …>` start tag, their pPr,
and each run's original rPr with just the toggled `b`/`i`/`u`/`strike`
merged in (in schema order; un-bolding a bold-by-style run writes
`<w:b w:val="0"/>`). Inline elements the editor doesn't edit (pictures,
hyperlinks, fields, tracked changes, footnote references, bookmarks) are
atomic objects kept verbatim. A table is copied verbatim until one of its
cells changes; then only the changed cell paragraphs are regenerated and
the row/cell XML around them is copied. Every other zip entry (styles,
numbering, headers/footers, media, rels, [Content_Types].xml, docProps) is
copied raw — same compressed bytes — in the original order. An unedited
save therefore reproduces document.xml exactly, and undoing back to the
saved state gives byte-identical XML again (tests in `docx/tests.rs`,
including an optional LibreOffice conversion check run with `--ignored`).
New paragraphs inherit the pPr of the paragraph they were split from.
Heading styles are looked up by **name** in styles.xml (`heading 1`, works
with localized ids), else the conventional `Heading1` id. Lists reuse the
document's own bullet / decimal definitions; if numbering.xml has none (or
doesn't exist) a minimal abstractNum/num is appended (creating the part,
its relationship and content-type override when needed).

**Layout** is lazy: only blocks near the viewport are shaped (cosmic-text
via iced's `Paragraph`), cached per paragraph id + revision + width;
blocks off screen use height estimates that are corrected with scroll
anchoring as they come into view, so 100+ page documents open and scroll
instantly. Undo snapshots share unchanged blocks (`Arc`), so they're cheap.

**Known limits:** no real pagination (one continuous sheet; page breaks show
as a marker), no headers/footers/footnotes display, floating images and
text boxes are shown as inline chips, hyperlink/field text is atomic (can't
be edited inside), no font family / size / colour picking, no table
structure editing (add/remove rows), RTL text is laid out but caret
movement assumes left-to-right, clipboard is plain text only.

## 6. File-manager features

**Interaction model (built):**
- **Selection:** press selects (Ctrl toggles, Shift selects a range).
  Pressing a file opens it in a reusable **preview tab**; a double-click
  pins the tab or enters/expands a folder.
- **Context menu:** right-click on items or empty space.
- **Keyboard:**
  - arrows / Home / End / PageUp / PageDown, plus type-ahead to jump by name
  - Enter opens, Backspace or Alt+↑ goes to the parent folder
  - F2 rename, Del trash, Shift+Del delete permanently
  - Ctrl+C / X / V clipboard, Ctrl+Z undo, Ctrl+A select all
  - Ctrl+Shift+N new folder, Ctrl+L editable path, Ctrl+F search
  - Ctrl+Alt+T open a terminal in the current (or selected) folder
- **Drag and drop inside the app:** drop on a folder, drive or the Trash.
  Drop moves; Ctrl copies.

| Feature | How | Status |
|---|---|---|
| Copy / move / trash / delete / restore | `ops.rs`: one thread per job, progress in Details, cancel, conflict dialog (replace / skip / keep both, apply to all), undo stack | ✅ |
| Trash | freedesktop spec: home trash + per-volume `.Trash-$uid` (works on USB), Trash view with restore / delete / empty | ✅ |
| Live refresh | `devices::watch_dirs` (notify, debounced) on the folders on screen, restarted when they change | ✅ |
| Thumbnails | images in-process (grid view); video / PDF thumbnails later | ✅ / 2 |
| Drives | UDisks2 over D-Bus: listed in Devices, mount on expand, unmount, safe eject (unmounts, locks LUKS, powers off), live updates | ✅ (unlocking LUKS: later) |
| Network | see §6a | ✅ (NFS: later) |
| Search | instant pass + background walk, append-only, 180 ms pause while typing, optional search inside text files | ✅ (fuzzy: 2) |
| Large folders | tree, list, grid and search results are virtualized | ✅ |
| Drag and drop with other apps | `extdnd.rs` + fixed `og-wayland` (og-suite builds) | see §7 |
| Open with | `openwith.rs`: desktop files from $XDG_DATA_HOME, $XDG_DATA_DIRS and flatpak exports (Hidden / NoDisplay / OnlyShowIn / NotShowIn / TryExec honoured); MIME from shared-mime-info `globs2` (+ `subclasses`, `aliases`; `mime_guess` when absent) then magic bytes; defaults from the mimeapps.list chain ($XDG_CONFIG_HOME, desktop-specific `$desktop-mimeapps.list`, $XDG_CONFIG_DIRS, data dirs) then Added Associations, mimeinfo.cache and parent types (text/* → text/plain). Exec field codes (%f %F %u %U %i %c %k %%, deprecated ones dropped), spec quoting, Terminal=true runs in the terminal. Apps start detached (own session, no stdio, reaped). Context menu: **Open in ‹default app›**, **Open with…** dialog (default marked, recommended, *Other app…* searchable list of every app, "Always open .ext files with this app" writes `[Default Applications]` in $XDG_CONFIG_HOME/mimeapps.list keeping every other line). Several files: %F/%U apps get them all at once, %f/%u apps one launch per file | ✅ |
| Open terminal here | context menu on a folder / empty space, **Ctrl+Alt+T** (current folder, or the selected folder). Terminal: `terminal` in `$XDG_CONFIG_HOME/og-files/settings.json`, else $TERMINAL, x-terminal-emulator, then alacritty, foot, kitty, wezterm, gnome-terminal, konsole, xfce4-terminal, xterm; each gets its own working-dir flag plus the process cwd | ✅ |
| Archives | §6b | ✅ |
| New text / Markdown file | context menu (empty space or folder) + Ctrl+N; extension added unless typed; opens straight into edit mode with the editor focused (.md starts with `# name`) | ✅ |
| Bookmarks | add/remove from the context menu, `bookmarks.json` | ✅ |
| Activity log | persisted to `$XDG_STATE_HOME/og-files/activity.jsonl`, "only this file" filter, Hide (remembered, for demos) and Clear (press twice) | ✅ |
| Icons | bundled Bootstrap Icons font, tinted per file type | ✅ |

## 6a. Network storage

Sidebar **Network** group, top to bottom:
- **Connect to server…** opens a form. Pasting a URL (`sftp://`, `ssh://`,
  `smb://`, `dav://`, `davs://`, `ftp://`) fills the fields. Then Test, then
  Connect / Save.
- **Saved connections**, stored in `$XDG_CONFIG_HOME/og-files/connections.json`
  with no secrets in the file.
- **Tailnet:** peers from the Tailscale LocalAPI (unix socket). Phones and
  tablets are hidden, and offline peers are dimmed.
- **On this network:** Avahi browse for `_sftp-ssh`, `_ssh`, `_smb` and
  `_webdav(s)`, updated live.
- **From ~/.ssh/config:** concrete `Host` entries.
- **Other mounts:** cifs/nfs/sshfs/fuse.rclone mounts made outside the app.

Pressing a discovered host prefills the form. Pressing a saved connection
mounts it and opens it.

**Backend:** rclone mounts at `~/Network/<name>`.
- Remotes are passed as `RCLONE_CONFIG_OGF_*` env vars, so the user's
  rclone.conf is never touched.
- Passwords live in the Secret Service keyring and are obscured via stdin,
  never argv.
- A missing password raises a prompt ("Password required"), with an option
  to remember it.
- SFTP uses ssh-agent / keys by default.
- Context menu: Connect, Disconnect, Edit, Copy address, Forget saved
  password, Remove.
- Auto-mount at start is a per-connection option. Every 60 s the app
  refreshes the mount state and the tailnet.
- Runtime deps: `rclone`, `fuse3`. Without them the form shows an install
  hint.

## 6b. Archives in the files pane

Context menu on archive files (anything the archive viewer reads):
- **Extract here** (several archives at once): an archive with one
  top-level item (the usual `project-1.2/`, or a lone file) puts it
  straight into the folder; anything else goes into a new folder named
  after the archive. Taken names become `name (2)`. Extraction runs in a
  hidden staging folder that is removed on failure or cancel.
- **Extract to folder…**: asks for the folder name (default: the archive
  name without its extension).
- **Compress…** (any selection): name (default: the item's name, or
  "Archive") and format: **.zip, .tar.gz, .tar.zst, .tar.xz, .7z**.

All of them are jobs in the Details list (progress, Cancel). Undo
(Ctrl+Z) moves the output to the Trash. An encrypted zip or 7z asks for the
password and retries. Same in-process engines as the viewer (no external
tools); zip-slip names and escaping symlinks are refused. 7z output stores
files and folders only (symlinks are left out).

## 6c. Command line

Subcommands run without opening a window (detected in `main` before iced
starts; the GUI flags `--view/--search/--focus/--usage` are unchanged):

```bash
og-files extract ARCHIVE [DEST] [--password PW]   # smart "extract here" into DEST (default .); prints the result path
og-files compress OUT.{zip,tar.gz,tar.zst,tar.xz,7z} FILE...
og-files list ARCHIVE [--password PW]             # entries with sizes
og-files open FILE...                             # default apps
og-files open-with APP_ID FILE...                 # e.g. org.gnome.Loupe, libreoffice-writer
og-files apps FILE                                # MIME type + apps that open it, default marked *
og-files --help
```

Big jobs print a percentage on stderr (when it is a terminal). Errors exit
with 1, bad arguments with 2. `compress` never overwrites an existing file.

## 7. Standalone vs OG-OS suite

- **Default build:** no `og-*` path dependencies. It uses XDG paths, its own
  palette and its own config. It builds with `cargo build -p og-files`
  and could be split into its own repo.
- **Feature `og-suite` (built):** adds `og-config` and `og-theme`, so the
  theme follows the suite's `~/.config/sway-power/config.json` and reloads
  live when it changes. It also adds `og-wayland` for drag-and-drop with
  other apps. The distro package and the local install build with
  `--features og-suite`.
- **Icons:** bundle an icon font (Lucide or Phosphor TTF via `include_bytes!`).
  Don't depend on Nerd Fonts being installed: og-files shows tofu without
  them. Until then the scaffold uses text tags (DIR, IMG, TBL…) because
  iced's default font has no arrow or shape glyphs.
- **Replacing og-files (decided):** when phase 1 is done, the binary and
  app_id become `og-files`, the crate directory is renamed, and the old crate
  moves to `archive/`.

### Shipping to other devices

og-files is meant to run on any OG-OS install, so its media stack is part
of the package, not assumed:

- **The `og-files` package** (`distro/pkgbuilds/og-files/PKGBUILD`) depends
  on:
  - `mpv`, which provides libmpv, the video engine, loaded at runtime
  - `ffmpeg` libraries, for the fallback player, thumbnails and trim export
  - `libva` + `mesa`, for hardware decoding on AMD out of the box
  - `alsa-lib`, `udisks2`, `vulkan-icd-loader`, `wayland`, `libxkbcommon`
- **GPU decode drivers for other vendors** are optional dependencies:
  `intel-media-driver` / `libva-intel-driver` for Intel and
  `libva-nvidia-driver` for NVIDIA proprietary. The ISO already ships both
  Intel drivers and all Vulkan drivers (`distro/packages.x86_64`).
- **Runtime behaviour:**
  - libmpv is opened at runtime; if it's missing (e.g. a stripped-down
    standalone build) the video tab falls back to the built-in FFmpeg
    player (cargo feature `ffmpeg`, on by default in the monorepo) or,
    without it, shows an "Install mpv" placeholder instead of failing.
  - Hardware decoding uses `hwdec=auto-safe`, so any machine without a
    working VAAPI driver silently decodes in software.
- **Outside OG-OS** (other distros, macOS, Windows): build with
  `--no-default-features` (add `--features ffmpeg` for the FFmpeg
  fallback, thumbnails and trim) and install `mpv`; see §11 and README.md. A self-contained bundle
  (AppImage/Flatpak shipping libmpv) is possible later if needed.

## 8. History (bottom panel)

**Decided:** saving writes over the file. There are no app-kept revision
copies. Safety comes from editing being an explicit mode (§3) and from
atomic writes. Video never overwrites: it exports a new file.

The bottom panel is therefore an **activity log**: opened, edit mode
entered, saved over, discarded, exported, errors.
- Persisted to `$XDG_STATE_HOME/og-files/activity.jsonl` (last 2000
  entries) ✅.
- "Only this file" filter, so selecting a file shows *its* history ✅.
- Running file jobs show here with progress bars and Cancel ✅.
- Optional later: show `git log` for files inside a repo, and btrfs/snapper
  snapshots where the volume has them (OG-OS set those up 2026-09-08). These
  are read-only views of history that already exists, not new copies.

## 9. Roadmap

**Phase 0: scaffold ✅ (2026-10-11)**
- [x] pane_grid layout matching the sketch, resizable, draggable
- [x] files pane: tree / list / grid views, thumbnails, kind filter over everything visible
- [x] focus mode (files pane ~85%, inspector hidden) and Ctrl+B hide
- [x] two-pass live search with append-only background results
- [x] tabs with stable ids, dedupe, dirty guard
- [x] CLI: `og-files [--view tree|list|grid] [--search TEXT] [--focus] [PATH]`; subcommands extract / compress / list / open / open-with / apps (§6c)
- [x] viewers: text (read-only → edit mode, save over, discard), image, CSV/TSV
- [x] PDF: hayro viewer + thumbnails; edit pages (rotate, delete, reorder, insert, duplicate, blank); export (extract, split, save copy)
- [x] PDF toolkit: all Stirling core tools, 54 in a Tools panel (native + optional Stirling server), unlock screen, convert-to-PDF from other viewers; 62 tests
- [x] inspector metadata, details activity log
- [x] placeholders naming each planned backend

**Phase 1: replace og-files for daily use ✅ (2026-10-11)**
- [x] file-ops engine (jobs, conflicts, cancel, undo) and freedesktop trash incl. per-volume
- [x] virtualized tree / list / grid / search results
- [x] UDisks2 drives via zbus (mount, unmount, eject, live updates)
- [x] context menu, rename, new folder / file, delete, bookmarks, copy path, combine images into PDF
- [x] selection model, preview tabs, keyboard navigation, type-ahead, path bar, search pause + content search
- [x] drag and drop inside the app; with other apps behind `og-suite`
- [x] `og-suite` feature (suite palette with live reload), bundled icons, persisted per-file activity log
- [x] confirm dialogs (delete, empty trash, close unsaved tab), selectable read-only text
- [ ] take over the `og-files` name (waiting on the user)
- [x] tests: ops, devices, selection / drag state, plus everything from phase 0 (99+)

**Phase 2: media and docs**
- [ ] audio (symphonia + cpal, waveform, tags)
- [ ] video playback (GStreamer to wgpu)
- [ ] PDF: tool recipes/pipelines, keep outlines through rebuilds, encrypted-save option; markdown preview
- [x] Word (.docx) editor with lossless round-trip (§5b)
- [ ] xlsx/ods read, table cell editing, csv write
- [x] archives as folders; extract / compress from the context menu (§6b)
- [x] open with / default apps / open terminal here (§6)
- [ ] hex viewer, syntax highlighting
- [x] network "connect to server" (§6a)

**Phase 3: basic editing and history**
- [ ] image edits (crop, rotate, resize, adjust)
- [ ] audio trim (save over in edit mode); video trim + frame snapshot (export new file)
- [ ] 3D viewer
- [ ] optional git / snapshot history views in the bottom panel

## 10. Decisions and open questions

**Decided (2026-10-11):**
- **Filter:** applies to everything visible in the files pane (expanded
  tree, list, grid, search results). Folders stay visible.
- **Search:** instant first pass, then a background search that keeps
  adding results without moving the user's scroll.
- **History / saving:** explicit edit mode, and save overwrites the file. No
  revision copies.
- **Video:** edits export as a new file.
- **Name:** takes over `og-files`.
- **Files pane:** shows folders and files. Tree, list and grid (thumbnail)
  views, and it can widen to fill most of the window (focus).

**Still open:**
1. **Video export depth:** is lossless trim and frame snapshot enough, or
   do you want re-encode options (resize, convert format, crop)? Re-encoding
   means bundling GStreamer encoders.
2. **Search scope:** home + bookmarks + mounted drives now. Should Root `/`
   or specific excluded folders be configurable?
3. **Grid in tree mode:** should the tree be able to show thumbnails inline
   (bigger rows for images), or are thumbnails grid-only?

## 11. Platform support

Linux is the reference platform; macOS (aarch64, x86_64) and Windows
(x86_64 MSVC) build from the same code with `--no-default-features`
(optionally `--features ffmpeg`). Per-OS code sits behind
`cfg(target_os = …)`; small shared helpers are in `platform.rs` (finding
programs, detached / console-less child processes, symlinks, hidden files,
the Windows console for CLI subcommands).

| Feature | Linux | macOS | Windows |
|---|---|---|---|
| Browse, viewers, editors, archives, search, CLI | ✅ | ✅ | ✅ (hidden attribute honoured; symlinks in archives skipped) |
| Config / state / cache dirs | XDG (unchanged) | `~/Library/Application Support`, `~/Library/Caches` | `%APPDATA%`, `%LOCALAPPDATA%` |
| Move to trash | ✅ freedesktop, per-volume | ✅ system Trash (`trash` crate) | ✅ Recycle Bin (`trash` crate) |
| Trash view (list / restore / empty), undo of trash | ✅ | hidden — use the Finder; the status line says undo isn't available | ✅ (`trash::os_limited`) |
| Drives | ✅ UDisks2: mount, unmount, eject, live | mounted volumes (`/`, `/Volumes/*`) with size; unmount + eject via `diskutil` | mounted drives with size, read-only (no unmount / eject) |
| Live folder refresh | ✅ inotify | ✅ FSEvents | ✅ ReadDirectoryChanges |
| Open in default app | mimeapps.list | `open` | ShellExecute |
| Open with… | app dialog + "always open with" | app dialog (bundles in /Applications, ~/Applications, /System/Applications → `open -a`); no "set default" | the system "Open with" dialog |
| Open terminal here | `$TERMINAL` / known terminals | Terminal.app, or `terminal` setting (app name or command) | Windows Terminal, else cmd (or `powershell` / `pwsh`) |
| Inspector permissions | mode + writable | mode + writable | read-only flag |
| Disk usage | hard links counted once, other filesystems not entered, statvfs | same | apparent sizes, no hard-link dedupe; GetDiskFreeSpaceEx |
| Network: saved connections, Test | ✅ | ✅ | ✅ |
| Network: mount | rclone + fuse3 (`--daemon`) | official rclone + macFUSE / FUSE-T; unmount `umount`, then `diskutil unmount force` | rclone + WinFsp, started in the background (pid file in the state dir); unmount ends it |
| Network: tailnet peers | LocalAPI unix socket | `tailscale status --json` | `tailscale status --json` |
| Network: LAN discovery (Avahi) | ✅ | none (section stays empty) | none (section stays empty) |
| Remembered passwords | Secret Service | Keychain (`keyring` crate) | Credential Manager (`keyring` crate) |
| Video (libmpv, loaded at runtime) | `../lib/libmpv.so.2` (AppImage), then `libmpv.so.2` / `.so` | `.app/Contents/Frameworks`, Homebrew, then `libmpv.2.dylib` | `libmpv-2.dll` next to the exe, then the DLL search path |
| Video without libmpv | FFmpeg player (feature `ffmpeg`), else an "Install mpv" placeholder | same | same |
| Video thumbnails, trim export | feature `ffmpeg` | feature `ffmpeg` | feature `ffmpeg` |
| Audio | ALSA (cpal) | CoreAudio | WASAPI |
| `og-suite` (suite theme, Wayland drag-out) | ✅ | — | — |

Windows release builds use the GUI subsystem (no console window); the CLI
subcommands attach to the parent console so they still print. Helper
processes (rclone, tailscale) start without a console window; apps opened
from og-files are detached.

Tests: the whole suite runs on Linux. Tests of Linux-only backends
(UDisks2, freedesktop trash, Avahi, the LocalAPI socket, desktop entries,
unix permissions) are `cfg(target_os = "linux")`; pure per-OS logic
(terminal choice, libmpv search order, mac app bundles, mount output and
pid files) is tested on Linux too.
