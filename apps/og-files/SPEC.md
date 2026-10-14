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

The goal is to bring the homelab Stirling-PDF (`stirling-homelab` on `go`,
fork `OmegaGiven/Stirling-PDF@homelab-edition`) into the file manager.

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
| Heavy tools | Stirling REST API on `go` (optional) | For jobs that need Tesseract, Ghostscript, qpdf or LibreOffice. The app uploads the file and saves the result next to the original. Config goes in `$XDG_CONFIG_HOME/og-files/stirling.json` (`{ "url": "https://go.tail6c6721.ts.net:8081", "api_key": "…" }`, sent as `X-API-KEY`). Without it those tools are just hidden, so the app stays standalone. |

**Model.** The same rules as every other viewer apply: view mode is
read-only, and **Edit pages** (`Ctrl+E`) enables page operations. **Save**
writes over the original and **Discard** reloads it. Exports (extract,
split, and any tool that makes a different document) always write
**new files** next to the original (`<name>-pages-1-3.pdf`,
`<name>-part-01.pdf`), work in either mode, and include unsaved edits.

**Tools panel.** The **Tools** button in the PDF toolbar opens 54 tools
in five groups. Each tool has a form, and the page fields default to the
ticked pages. The buttons are:
- **Apply** (edit mode): the result becomes the working copy. Save
  overwrites, Discard undoes.
- **Save as new file**: writes `<name>-<tool>.pdf` beside the original.
- **Run / Save files**: for reports and tools that produce other files.

Other PDF toolbar additions: **Save copy**, plus **Duplicate** and **Blank
page** in edit mode. Outside the PDF viewer: password-protected PDFs open
behind an unlock screen. Images get **Make PDF** (native) and **Split
scanned photos** (server). Office, HTML, Markdown and e-book files get
**Convert to PDF** (server).

**Status: all Stirling core tools are built** (2026-10-11). "native" means
in-process Rust; "server" means the Stirling REST API.

| Group | Tools | Engine |
|---|---|---|
| Page editing | view, thumbnails, zoom, rotate, delete, reorder, duplicate, blank page, insert PDF, extract, split every N, select | native (plan + compose) |
| Organize | split by ranges / bookmarks / file size, select pages by range, remove blank pages, crop, n-up (2–16), booklet, fit to paper, scale, split into sections, one long page, overlay PDF, insert images | native |
| Organize | auto-split at QR pages, insert web page | server |
| Add & edit | page numbers, text stamp, watermark (centred / tiled), image stamp, **drawn signature** (canvas pad + click to place), attachments, bookmarks editor, metadata editor / wipe | native |
| Security | AES-256 password + permissions, remove password, sanitize (JS, files, XMP, info, links, fonts), redact text, **redact areas** (drag boxes on the page, true removal by rasterizing), remove annotations / images, flatten forms, flatten to images, unlock forms, remove signatures, show JavaScript | native |
| Security | sign with certificate (PKCS#12), validate signatures | server |
| Convert & extract | pages → PNG/JPEG, extract images, extract attachments, document info, compare (text diff), auto-rename | native |
| Convert & extract | PDF → Word/ODT/Excel/PowerPoint/HTML/Markdown/text/RTF/XML/CSV/EPUB; Office/HTML/MD/EML/e-book/image → PDF | server |
| Optimize & colour | lossless compress, repair, greyscale, invert, adjust colours, fake scan | native |
| Optimize & colour | strong compress (Ghostscript/qpdf), repair via server, OCR (Tesseract), PDF/A-2b | server |

**Server endpoints** (paths and fields taken from upstream `app/core` only,
listed in `tools/server.rs`): `/api/v1/misc/{ocr-pdf, compress-pdf, repair,
auto-split-pdf, extract-image-scans}`,
`/api/v1/convert/{pdf/pdfa, pdf/word, pdf/presentation, pdf/text, pdf/xlsx,
pdf/csv, pdf/html, pdf/markdown, pdf/xml, pdf/epub, file/pdf, html/pdf,
markdown/pdf, eml/pdf, img/pdf, svg/pdf, ebook/pdf, url/pdf}`,
`/api/v1/security/{cert-sign, validate-signature}`, and
`/api/v1/info/status` for **Test connection**. The URL and API key are
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
- Decrypt can't tell whether a password was actually needed.
- Phase 2: a `compose::build_from_doc` variant so split-by-size doesn't
  re-parse the file on every probe.

Pipelines and "recipes" (saved tool chains run on a selection) are still
open: phase 3.

**Tests (62).** `cargo test -p og-files` covers:
- compose, pages, and every public function of structure, layout, raster
  and server (server tests use a local mock HTTP server, no network);
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
    player instead of failing.
  - Hardware decoding uses `hwdec=auto-safe`, so any machine without a
    working VAAPI driver silently decodes in software.
- **Outside OG-OS** (other distros): build with `--no-default-features` and
  install `mpv` + `ffmpeg` from the distro. A self-contained bundle
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
