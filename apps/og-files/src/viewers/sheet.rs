//! Spreadsheets: csv/tsv, xlsx/xlsm/xls/xlsb, ods. Sheet tabs, virtualized
//! grid (only the visible rows *and* columns are built), frozen column
//! letters / row numbers, optional header row. Edit mode (Ctrl+E): click a
//! cell to edit it inline, Enter/Tab move on, add/delete rows & columns at the
//! cursor, sort by the cursor column. Save writes over csv/tsv/xlsx/ods
//! (values only — formulas and styles are lost, so the first save of a
//! workbook asks for a second press); xls/xlsm/xlsb can't be written back and
//! save to `<stem>-edited.xlsx` instead.

use super::Event;
use crate::fsops;
use crate::theme;
use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree::Tree, Operation, Widget};
use iced::advanced::{mouse, overlay, renderer, Clipboard, Shell};
use iced::widget::text::Wrapping;
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, text, text_input, Space,
};
use iced::{keyboard, Element, Font, Length, Rectangle, Size, Task, Theme, Vector};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Rows kept in memory; past this the sheet is read-only-truncated.
const MAX_ROWS: usize = 1_000_000;
const ROW_H: f32 = 24.0;
const LETTER_H: f32 = 20.0;
const NUM_W: f32 = 60.0;
const MIN_W: f32 = 48.0;
const MAX_W: f32 = 320.0;
const CHAR_W: f32 = 7.4;
/// rows sampled for automatic column widths
const WIDTH_SAMPLE: usize = 1000;
const OVERSCAN_ROWS: usize = 4;
const OVERSCAN_COLS: usize = 2;
/// extra room so the frozen strips can scroll as far as the grid
const BAR: f32 = 12.0;

const VALUES_ONLY: &str = "Saving writes values only: formulas, formatting, charts and \
     other sheets' features are lost. Press Save again to confirm.";

fn ext_lower(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Does this viewer handle `path` (by extension / sniffing)?
pub fn supported(path: &Path) -> bool {
    matches!(
        ext_lower(path).as_str(),
        "csv" | "tsv" | "tab" | "xlsx" | "xlsm" | "xls" | "xlsb" | "ods"
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Fmt {
    /// delimited text with this delimiter
    Csv(u8),
    Xlsx,
    Ods,
    /// readable but not writable in place (xls, xlsm, xlsb)
    OtherBook,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SheetData {
    pub name: String,
    pub rows: Vec<Vec<String>>,
    /// more than MAX_ROWS rows in the file; only the first MAX_ROWS loaded
    pub truncated: bool,
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    /// shared so the Message clone is cheap
    pub sheets: Arc<Vec<SheetData>>,
    fmt: Fmt,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    tokio::task::spawn_blocking(move || load(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// Every sheet's cells as text (Convert to PDF uses this).
pub(crate) fn load_sheets(path: &Path) -> Result<Arc<Vec<SheetData>>, String> {
    load(path).map(|o| o.sheets)
}

fn load(path: &Path) -> Result<Opened, String> {
    let ext = ext_lower(path);
    let (fmt, sheets) = match ext.as_str() {
        "csv" | "tsv" | "tab" => {
            let delim = if ext == "csv" {
                sniff_file(path)?
            } else {
                b'\t'
            };
            let (rows, truncated) = read_csv(path, delim, MAX_ROWS)?;
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Sheet1".into());
            (
                Fmt::Csv(delim),
                vec![SheetData {
                    name,
                    rows,
                    truncated,
                }],
            )
        }
        "xlsx" => (Fmt::Xlsx, read_book(path)?),
        "ods" => (Fmt::Ods, read_book(path)?),
        "xlsm" | "xls" | "xlsb" => (Fmt::OtherBook, read_book(path)?),
        _ => return Err("not a spreadsheet".into()),
    };
    Ok(Opened {
        path: path.to_path_buf(),
        sheets: Arc::new(sheets),
        fmt,
    })
}

// ------------------------------------------------------------------ reading

fn sniff_file(path: &Path) -> Result<u8, String> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(64 * 1024);
    std::fs::File::open(path)
        .and_then(|f| f.take(64 * 1024).read_to_end(&mut buf))
        .map_err(|e| e.to_string())?;
    Ok(sniff(&buf))
}

/// Pick the delimiter (comma, semicolon, tab, pipe) that splits the first
/// lines into the most consistent non-zero number of fields. Comma wins ties.
fn sniff(sample: &[u8]) -> u8 {
    let text = String::from_utf8_lossy(sample);
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() > 1 && sample.len() >= 64 * 1024 {
        lines.pop(); // probably cut mid-line
    }
    lines.truncate(30);
    let mut best = (b',', (0usize, 0usize));
    for d in [b',', b';', b'\t', b'|'] {
        let counts: Vec<usize> = lines.iter().map(|l| count_outside_quotes(l, d)).collect();
        let mut mode = (0usize, 0usize); // (lines, count)
        for &c in counts.iter().filter(|&&c| c > 0) {
            let n = counts.iter().filter(|&&x| x == c).count();
            if (n, c) > mode {
                mode = (n, c);
            }
        }
        if mode.1 > 0 && mode > best.1 {
            best = (d, mode);
        }
    }
    best.0
}

fn count_outside_quotes(line: &str, d: u8) -> usize {
    let mut quoted = false;
    let mut n = 0;
    for b in line.bytes() {
        if b == b'"' {
            quoted = !quoted;
        } else if b == d && !quoted {
            n += 1;
        }
    }
    n
}

/// Streaming csv read; stops after `cap` rows (returns truncated = true).
fn read_csv(path: &Path, delim: u8, cap: usize) -> Result<(Vec<Vec<String>>, bool), String> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_path(path)
        .map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    let mut rec = csv::ByteRecord::new();
    loop {
        match rdr.read_byte_record(&mut rec) {
            Ok(false) => return Ok((rows, false)),
            Ok(true) => {
                if rows.len() >= cap {
                    return Ok((rows, true));
                }
                let mut row: Vec<String> = rec
                    .iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect();
                if rows.is_empty() {
                    if let Some(first) = row.first_mut() {
                        if let Some(s) = first.strip_prefix('\u{feff}') {
                            *first = s.to_string();
                        }
                    }
                }
                rows.push(row);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn read_book(path: &Path) -> Result<Vec<SheetData>, String> {
    use calamine::Reader;
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for name in wb.sheet_names() {
        let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
        let (r0, c0) = range.start().unwrap_or((0, 0));
        let mut rows: Vec<Vec<String>> = vec![Vec::new(); (r0 as usize).min(MAX_ROWS)];
        let mut truncated = false;
        for r in range.rows() {
            if rows.len() >= MAX_ROWS {
                truncated = true;
                break;
            }
            let mut v = vec![String::new(); c0 as usize];
            v.extend(r.iter().map(cell_str));
            while v.last().is_some_and(|s| s.is_empty()) {
                v.pop();
            }
            rows.push(v);
        }
        out.push(SheetData {
            name,
            rows,
            truncated,
        });
    }
    if out.is_empty() {
        out.push(SheetData {
            name: "Sheet1".into(),
            rows: Vec::new(),
            truncated: false,
        });
    }
    Ok(out)
}

fn cell_str(d: &calamine::Data) -> String {
    use calamine::Data;
    match d {
        Data::Empty => String::new(),
        Data::Float(f) => fmt_num(*f),
        Data::DateTime(dt) => excel_serial(dt.as_f64(), dt.is_duration()),
        other => other.to_string(),
    }
}

fn fmt_num(f: f64) -> String {
    format!("{f}")
}

/// Excel serial day number → "YYYY-MM-DD[ HH:MM:SS]" (or "H:MM:SS" for a
/// duration), without pulling in chrono.
fn excel_serial(v: f64, duration: bool) -> String {
    let total = (v * 86_400.0).round() as i64;
    let (days, secs) = (total.div_euclid(86_400), total.rem_euclid(86_400));
    let hms = |s: i64| format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
    if duration {
        let s = total.abs();
        return format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60);
    }
    if days == 0 {
        return hms(secs); // time-only cell
    }
    // 1899-12-30 is serial 0; unix epoch is serial 25569
    let (y, m, d) = civil_from_days(days - 25_569);
    if secs == 0 {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("{y:04}-{m:02}-{d:02} {}", hms(secs))
    }
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ------------------------------------------------------------------ writing

/// A cell that round-trips exactly through f64 is written as a number;
/// anything else ("007", "1.50", text) stays a string.
fn as_number(s: &str) -> Option<f64> {
    let f: f64 = s.trim().parse().ok()?;
    (f.is_finite() && fmt_num(f) == s).then_some(f)
}

fn write_csv(rows: &[Vec<String>], delim: u8) -> Result<Vec<u8>, String> {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut w = csv::WriterBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .from_writer(Vec::new());
    let blank = vec![String::new(); ncols];
    for r in rows {
        let r = if r.is_empty() { &blank } else { r };
        w.write_record(r).map_err(|e| e.to_string())?;
    }
    w.into_inner().map_err(|e| e.to_string())
}

fn write_xlsx(sheets: &[SheetData]) -> Result<Vec<u8>, String> {
    let mut wb = rust_xlsxwriter::Workbook::new();
    for s in sheets {
        let ws = wb.add_worksheet();
        ws.set_name(&s.name).map_err(|e| e.to_string())?;
        for (r, row) in s.rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() {
                    continue;
                }
                let (r, c) = (r as u32, c as u16);
                match as_number(v) {
                    Some(n) => ws.write_number(r, c, n).map(|_| ()),
                    None => ws.write_string(r, c, v).map(|_| ()),
                }
                .map_err(|e| e.to_string())?;
            }
        }
    }
    wb.save_to_buffer().map_err(|e| e.to_string())
}

fn write_ods(sheets: &[SheetData]) -> Result<Vec<u8>, String> {
    let mut wb = spreadsheet_ods::WorkBook::new_empty();
    for s in sheets {
        let mut sh = spreadsheet_ods::Sheet::new(s.name.as_str());
        for (r, row) in s.rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() {
                    continue;
                }
                match as_number(v) {
                    Some(n) => sh.set_value(r as u32, c as u32, n),
                    None => sh.set_value(r as u32, c as u32, v.as_str()),
                }
            }
        }
        wb.push_sheet(sh);
    }
    spreadsheet_ods::write_ods_buf(&mut wb, Vec::new()).map_err(|e| e.to_string())
}

fn encode(fmt: Fmt, sheets: &[SheetData]) -> Result<Vec<u8>, String> {
    match fmt {
        Fmt::Csv(d) => write_csv(sheets.first().map(|s| &s.rows[..]).unwrap_or(&[]), d),
        Fmt::Xlsx | Fmt::OtherBook => write_xlsx(sheets),
        Fmt::Ods => write_ods(sheets),
    }
}

// -------------------------------------------------------------------- state

/// "A".."Z", "AA".. for a 0-based column.
fn col_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap_or_default()
}

fn text_w(s: &str) -> f32 {
    (s.chars().count() as f32 * CHAR_W + 14.0).clamp(MIN_W, MAX_W)
}

/// Column widths from the first rows' content (capped).
fn auto_widths(rows: &[Vec<String>]) -> Vec<f32> {
    let ncols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut w = vec![MIN_W; ncols];
    for r in rows.iter().take(WIDTH_SAMPLE) {
        for (c, v) in r.iter().enumerate() {
            w[c] = w[c].max(text_w(v));
        }
    }
    // columns that only show up further down still need a width
    for (c, wc) in w.iter_mut().enumerate() {
        *wc = wc.max(text_w(&col_name(c)));
    }
    w
}

/// Sort order for cells: numbers (numerically) before text (naturally),
/// empty cells always last.
fn cell_cmp(a: &str, b: &str) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        _ => {}
    }
    match (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        _ => fsops::natural_cmp(a, b),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct View {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct CellEdit {
    r: usize,
    c: usize,
    value: String,
    original: String,
}

pub struct State {
    path: PathBuf,
    fmt: Fmt,
    sheets: Arc<Vec<SheetData>>,
    /// per sheet: column widths
    widths: Vec<Vec<f32>>,
    cur: usize,
    header: bool,
    editing: bool,
    dirty: bool,
    saving: bool,
    /// the values-only warning was shown and confirmed
    warned: bool,
    /// warning on screen, waiting for the second Save press
    confirm: bool,
    save_as: Option<PathBuf>,
    /// (row, col) in sheet coordinates
    cursor: (usize, usize),
    edit: Option<CellEdit>,
    vp: View,
    grid_id: scrollable::Id,
    head_id: scrollable::Id,
    nums_id: scrollable::Id,
    input_id: text_input::Id,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Move {
    Stay,
    Down,
    Right,
    Left,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Scrolled(scrollable::Viewport),
    Tab(usize),
    ToggleHeader,
    Click(usize, usize),
    Input(String),
    Commit(Move),
    Cancel,
    AddRow,
    DelRow,
    AddCol,
    DelCol,
    Sort(bool),
    ToggleEdit,
    Save,
    Saved(Result<PathBuf, String>),
    Discard,
    Reloaded(Result<Opened, String>),
    /// surface a message in the status bar
    Notify(String),
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let widths = o.sheets.iter().map(|s| auto_widths(&s.rows)).collect();
        (
            Self {
                path: o.path,
                fmt: o.fmt,
                sheets: o.sheets,
                widths,
                cur: 0,
                header: false,
                editing: false,
                dirty: false,
                saving: false,
                warned: false,
                confirm: false,
                save_as: None,
                cursor: (0, 0),
                edit: None,
                vp: View {
                    w: 1600.0,
                    h: 1200.0,
                    ..View::default()
                },
                grid_id: scrollable::Id::unique(),
                head_id: scrollable::Id::unique(),
                nums_id: scrollable::Id::unique(),
                input_id: text_input::Id::unique(),
            },
            Task::none(),
        )
    }

    fn sheet(&self) -> &SheetData {
        &self.sheets[self.cur]
    }

    fn ncols(&self) -> usize {
        self.widths[self.cur].len()
    }

    /// first sheet row shown in the grid body (row 0 is the header when on)
    fn base(&self) -> usize {
        usize::from(self.header && !self.sheet().rows.is_empty())
    }

    fn cell(&self, r: usize, c: usize) -> &str {
        self.sheet()
            .rows
            .get(r)
            .and_then(|row| row.get(c))
            .map(String::as_str)
            .unwrap_or("")
    }

    fn rows_mut(&mut self) -> &mut Vec<Vec<String>> {
        let cur = self.cur;
        &mut Arc::make_mut(&mut self.sheets)[cur].rows
    }

    fn set_cell(&mut self, r: usize, c: usize, v: String) {
        let rows = self.rows_mut();
        while rows.len() <= r {
            rows.push(Vec::new());
        }
        let row = &mut rows[r];
        if row.len() <= c {
            if v.is_empty() {
                return;
            }
            row.resize(c + 1, String::new());
        }
        row[c] = v;
        while row.last().is_some_and(|s| s.is_empty()) {
            row.pop();
        }
        let w = text_w(self.cell(r, c));
        let ws = &mut self.widths[self.cur];
        if ws.len() <= c {
            ws.resize(c + 1, MIN_W);
        }
        ws[c] = ws[c].max(w);
    }

    /// Prefix sums of the current sheet's column widths (len = ncols + 1).
    fn xs(&self) -> Vec<f32> {
        let mut xs = Vec::with_capacity(self.ncols() + 1);
        let mut x = 0.0;
        xs.push(x);
        for w in &self.widths[self.cur] {
            x += w;
            xs.push(x);
        }
        xs
    }

    fn commit_pending(&mut self) {
        if let Some(e) = self.edit.take() {
            if e.value != e.original {
                self.set_cell(e.r, e.c, e.value);
                self.dirty = true;
            }
        }
    }

    fn start_edit(&mut self, r: usize, c: usize) -> Task<Msg> {
        let v = self.cell(r, c).to_string();
        self.cursor = (r, c);
        self.edit = Some(CellEdit {
            r,
            c,
            value: v.clone(),
            original: v,
        });
        Task::batch([text_input::focus(self.input_id.clone()), self.reveal()])
    }

    /// Scroll so the cursor cell is visible (and keep the frozen strips in step).
    fn reveal(&mut self) -> Task<Msg> {
        let (r, c) = self.cursor;
        let mut v = self.vp;
        if r >= self.base() {
            let top = (r - self.base()) as f32 * ROW_H;
            if top < v.y {
                v.y = top;
            } else if top + ROW_H > v.y + v.h {
                v.y = top + ROW_H - v.h;
            }
        }
        let xs = self.xs();
        if c + 1 < xs.len() {
            if xs[c] < v.x {
                v.x = xs[c];
            } else if xs[c + 1] > v.x + v.w {
                v.x = (xs[c + 1] - v.w).min(xs[c]);
            }
        }
        if v.x == self.vp.x && v.y == self.vp.y {
            return Task::none();
        }
        self.vp = v;
        Task::batch([
            scrollable::scroll_to(
                self.grid_id.clone(),
                scrollable::AbsoluteOffset { x: v.x, y: v.y },
            ),
            self.sync(),
        ])
    }

    fn sync(&self) -> Task<Msg> {
        Task::batch([
            scrollable::scroll_to(
                self.head_id.clone(),
                scrollable::AbsoluteOffset {
                    x: self.vp.x,
                    y: 0.0,
                },
            ),
            scrollable::scroll_to(
                self.nums_id.clone(),
                scrollable::AbsoluteOffset {
                    x: 0.0,
                    y: self.vp.y,
                },
            ),
        ])
    }

    fn truncated(&self) -> bool {
        self.sheets.iter().any(|s| s.truncated)
    }

    fn add_row(&mut self) {
        self.commit_pending();
        let at = if self.sheet().rows.is_empty() {
            0
        } else {
            (self.cursor.0 + 1).min(self.sheet().rows.len())
        };
        self.rows_mut().insert(at, Vec::new());
        self.cursor.0 = at;
        self.dirty = true;
    }

    fn del_row(&mut self) {
        self.commit_pending();
        let r = self.cursor.0;
        if r < self.sheet().rows.len() {
            self.rows_mut().remove(r);
            self.cursor.0 = r.min(self.sheet().rows.len().saturating_sub(1));
            self.dirty = true;
        }
    }

    fn add_col(&mut self) {
        self.commit_pending();
        let at = (self.cursor.1 + 1).min(self.ncols());
        for row in self.rows_mut() {
            if row.len() > at {
                row.insert(at, String::new());
            }
        }
        let cur = self.cur;
        self.widths[cur].insert(at, MIN_W);
        self.cursor.1 = at;
        self.dirty = true;
    }

    fn del_col(&mut self) {
        self.commit_pending();
        let c = self.cursor.1;
        for row in self.rows_mut() {
            if row.len() > c {
                row.remove(c);
            }
        }
        let cur = self.cur;
        let ws = &mut self.widths[cur];
        if ws.len() > 1 {
            ws.remove(c.min(ws.len() - 1));
        } else {
            ws[0] = MIN_W;
        }
        self.cursor.1 = c.min(self.ncols() - 1);
        self.dirty = true;
    }

    fn sort(&mut self, asc: bool) {
        self.commit_pending();
        let (c, base) = (self.cursor.1, self.base());
        let rows = self.rows_mut();
        if rows.len() <= base + 1 {
            return;
        }
        fn get(r: &[String], c: usize) -> &str {
            r.get(c).map(String::as_str).unwrap_or("")
        }
        rows[base..].sort_by(|a, b| {
            let (x, y) = (get(a, c), get(b, c));
            // empty cells stay last in both directions
            match (x.is_empty(), y.is_empty()) {
                (false, false) if !asc => cell_cmp(y, x),
                _ => cell_cmp(x, y),
            }
        });
        self.dirty = true;
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        let none = (Task::none(), None);
        match msg {
            Msg::Scrolled(v) => {
                let o = v.absolute_offset();
                self.vp = View {
                    x: o.x,
                    y: o.y,
                    w: v.bounds().width,
                    h: v.bounds().height,
                };
                (self.sync(), None)
            }
            Msg::Tab(i) => {
                if i < self.sheets.len() && i != self.cur {
                    self.commit_pending();
                    self.cur = i;
                    self.cursor = (0, 0);
                    self.vp.x = 0.0;
                    self.vp.y = 0.0;
                    let t = Task::batch([
                        scrollable::scroll_to(
                            self.grid_id.clone(),
                            scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
                        ),
                        self.sync(),
                    ]);
                    return (t, None);
                }
                none
            }
            Msg::ToggleHeader => {
                self.header = !self.header;
                none
            }
            Msg::Click(r, c) => {
                if self.editing {
                    if self.edit.as_ref().is_some_and(|e| e.r == r && e.c == c) {
                        return none;
                    }
                    self.commit_pending();
                    (self.start_edit(r, c), None)
                } else {
                    self.cursor = (r, c);
                    none
                }
            }
            Msg::Input(s) => {
                if let Some(e) = &mut self.edit {
                    e.value = s;
                }
                none
            }
            Msg::Commit(mv) => {
                self.commit_pending();
                let (r, c) = self.cursor;
                let last_row = self.sheet().rows.len().max(1) - 1;
                let (r, c) = match mv {
                    Move::Stay => return none,
                    Move::Down => ((r + 1).min(last_row.max(r)), c),
                    Move::Right => (r, (c + 1).min(self.ncols() - 1)),
                    Move::Left => (r, c.saturating_sub(1)),
                };
                (self.start_edit(r, c), None)
            }
            Msg::Cancel => {
                self.edit = None;
                none
            }
            Msg::AddRow | Msg::DelRow | Msg::AddCol | Msg::DelCol | Msg::Sort(_)
                if !self.editing =>
            {
                none
            }
            Msg::AddRow => {
                self.add_row();
                (self.reveal(), None)
            }
            Msg::DelRow => {
                self.del_row();
                none
            }
            Msg::AddCol => {
                self.add_col();
                (self.reveal(), None)
            }
            Msg::DelCol => {
                self.del_col();
                none
            }
            Msg::Sort(asc) => {
                self.sort(asc);
                let name = col_name(self.cursor.1);
                (
                    Task::none(),
                    Some(Event::Status(format!(
                        "Sorted by column {name} ({})",
                        if asc { "ascending" } else { "descending" }
                    ))),
                )
            }
            Msg::ToggleEdit => {
                if self.editing && self.is_dirty() {
                    return (
                        Task::none(),
                        Some(Event::Status(
                            "Unsaved changes: Save or Discard first".into(),
                        )),
                    );
                }
                self.editing = !self.editing;
                self.edit = None;
                self.confirm = false;
                none
            }
            Msg::Save => (self.save(), None),
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(p) => {
                        self.dirty = false;
                        let s = if p == self.path {
                            format!("Saved over {}", p.display())
                        } else {
                            format!(
                                "Saved as {} (this format can't be written back)",
                                p.display()
                            )
                        };
                        (Task::none(), Some(Event::Status(s)))
                    }
                    Err(e) => (
                        Task::none(),
                        Some(Event::Status(format!("Save failed: {e}"))),
                    ),
                }
            }
            Msg::Discard => (Task::perform(open(self.path.clone()), Msg::Reloaded), None),
            Msg::Reloaded(r) => match r {
                Ok(o) => {
                    self.widths = o.sheets.iter().map(|s| auto_widths(&s.rows)).collect();
                    self.sheets = o.sheets;
                    self.cur = self.cur.min(self.sheets.len() - 1);
                    self.cursor = (0, 0);
                    self.edit = None;
                    self.dirty = false;
                    self.editing = false;
                    self.confirm = false;
                    (
                        Task::none(),
                        Some(Event::Status("Changes discarded".into())),
                    )
                }
                Err(e) => (
                    Task::none(),
                    Some(Event::Status(format!("Reload failed: {e}"))),
                ),
            },
            Msg::Notify(s) => (Task::none(), Some(Event::Status(s))),
        }
    }

    /// Unsaved edits exist (tab close asks first).
    pub fn is_dirty(&self) -> bool {
        self.dirty || self.edit.as_ref().is_some_and(|e| e.value != e.original)
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        Some(self.editing)
    }

    /// Ctrl+E. Leaving edit mode with unsaved changes is refused (the
    /// refusal comes back as a Msg that turns into an Event::Status).
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        if self.editing && self.is_dirty() {
            return Task::done(Msg::Notify("Unsaved changes: Save or Discard first".into()));
        }
        let (t, _) = self.update(Msg::ToggleEdit);
        t
    }

    fn target(&mut self) -> PathBuf {
        match self.fmt {
            Fmt::Csv(_) | Fmt::Xlsx | Fmt::Ods => self.path.clone(),
            Fmt::OtherBook => {
                if let Some(p) = &self.save_as {
                    return p.clone();
                }
                let dir = self.path.parent().unwrap_or(Path::new(".")).to_path_buf();
                let stem = self
                    .path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "sheet".into());
                let p = crate::ops::unique_name(&dir, &format!("{stem}-edited.xlsx"));
                self.save_as = Some(p.clone());
                p
            }
        }
    }

    /// Ctrl+S: write over the original (atomically). No-op if not dirty.
    pub fn save(&mut self) -> Task<Msg> {
        self.commit_pending();
        if self.saving || !self.dirty {
            return Task::none();
        }
        if self.truncated() {
            return Task::done(Msg::Notify(format!(
                "Only the first {MAX_ROWS} rows are loaded; saving would drop the rest"
            )));
        }
        if !matches!(self.fmt, Fmt::Csv(_)) && !self.warned {
            if !self.confirm {
                self.confirm = true;
                return Task::done(Msg::Notify(VALUES_ONLY.into()));
            }
            self.warned = true;
            self.confirm = false;
        }
        let target = self.target();
        self.saving = true;
        let (fmt, sheets) = (self.fmt, self.sheets.clone());
        Task::perform(
            async move {
                let bytes = tokio::task::spawn_blocking(move || encode(fmt, &sheets))
                    .await
                    .map_err(|e| e.to_string())??;
                fsops::write_bytes(target.clone(), bytes).await?;
                Ok(target)
            },
            Msg::Saved,
        )
    }

    // ----------------------------------------------------------------- view

    pub fn view(&self) -> Element<'_, Msg> {
        let mut top = column![self.toolbar()].spacing(4);
        if self.editing {
            top = top.push(self.edit_bar());
        }
        if self.confirm {
            top = top.push(
                container(text(VALUES_ONLY).size(12))
                    .padding([4, 8])
                    .width(Length::Fill)
                    .style(|t: &Theme| {
                        let p = t.extended_palette();
                        container::Style {
                            background: Some(p.danger.weak.color.scale_alpha(0.35).into()),
                            border: iced::Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }
                    }),
            );
        }
        top = top.push(self.cell_bar());
        column![top, self.grid(), self.footer()].spacing(4).into()
    }

    fn toolbar(&self) -> Element<'_, Msg> {
        let tabs = row(self.sheets.iter().enumerate().map(|(i, s)| {
            button(text(s.name.clone()).size(12))
                .padding([3, 10])
                .style(if i == self.cur {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Msg::Tab(i))
                .into()
        }))
        .spacing(4);
        let tabs = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .width(Length::Fill);
        let header = button(
            text(if self.header {
                "Header row: on"
            } else {
                "Header row: off"
            })
            .size(12),
        )
        .style(button::secondary)
        .on_press(Msg::ToggleHeader);
        let mode: Element<'_, Msg> = if self.editing {
            row![
                text(if self.is_dirty() {
                    "EDITING  * modified"
                } else {
                    "EDITING"
                })
                .size(12)
                .color(iced::Color::from_rgb8(0xff, 0xc8, 0x6b)),
                button(text("Discard").size(12))
                    .on_press_maybe(self.is_dirty().then_some(Msg::Discard))
                    .style(button::secondary),
                button(
                    text(if self.confirm {
                        "Confirm save"
                    } else {
                        "Save  Ctrl+S"
                    })
                    .size(12)
                )
                .on_press_maybe((self.is_dirty() && !self.saving).then_some(Msg::Save)),
                button(text("Done").size(12))
                    .on_press_maybe((!self.is_dirty()).then_some(Msg::ToggleEdit))
                    .style(button::secondary),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        } else {
            row![
                text("Read-only").size(12).color(theme::MUTED),
                button(text("Edit  Ctrl+E").size(12)).on_press(Msg::ToggleEdit),
            ]
            .spacing(6)
            .align_y(iced::Alignment::Center)
            .into()
        };
        row![tabs, header, mode]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into()
    }

    fn edit_bar(&self) -> Element<'_, Msg> {
        let b = |l: &'static str, m: Msg| {
            button(text(l).size(12))
                .padding([3, 8])
                .style(button::secondary)
                .on_press(m)
        };
        row![
            b("+ Row", Msg::AddRow),
            b("− Row", Msg::DelRow),
            b("+ Column", Msg::AddCol),
            b("− Column", Msg::DelCol),
            b("Sort A→Z", Msg::Sort(true)),
            b("Sort Z→A", Msg::Sort(false)),
            text("Click a cell to edit · Enter ↓ · Tab → · Shift+Tab ← · Esc cancels")
                .size(11)
                .color(theme::MUTED),
        ]
        .spacing(4)
        .align_y(iced::Alignment::Center)
        .into()
    }

    fn cell_bar(&self) -> Element<'_, Msg> {
        let (r, c) = self.cursor;
        let value = match &self.edit {
            Some(e) => e.value.as_str(),
            None => self.cell(r, c),
        };
        row![
            container(
                text(format!("{}{}", col_name(c), r + 1))
                    .size(12)
                    .font(Font::MONOSPACE)
            )
            .width(NUM_W),
            text(value.to_string()).size(12).wrapping(Wrapping::None),
            Space::with_width(Length::Fill),
        ]
        .push_maybe(self.edit.is_some().then(|| {
            // keep the edit and stay on this cell (Enter moves down)
            button(text("Apply").size(11))
                .on_press(Msg::Commit(Move::Stay))
                .padding([0, 8])
        }))
        .spacing(6)
        .height(18)
        .into()
    }

    fn footer(&self) -> Element<'_, Msg> {
        let s = self.sheet();
        let mut msg = format!("{} rows × {} columns", s.rows.len(), self.ncols());
        if s.truncated {
            msg += &format!(
                " — file has more; only the first {MAX_ROWS} rows are loaded (saving disabled)"
            );
        }
        if let Fmt::Csv(d) = self.fmt {
            let name = match d {
                b',' => "comma",
                b';' => "semicolon",
                b'\t' => "tab",
                b'|' => "pipe",
                _ => "custom",
            };
            msg += &format!(" · {name}-separated");
        }
        if self.fmt == Fmt::OtherBook {
            msg += " · saves to a new -edited.xlsx";
        }
        text(msg).size(12).color(theme::MUTED).into()
    }

    /// Visible column range [c0, c1) for the current horizontal viewport.
    fn col_range(&self, xs: &[f32]) -> (usize, usize) {
        let n = xs.len() - 1;
        let c0 = xs
            .partition_point(|&x| x <= self.vp.x)
            .saturating_sub(1)
            .saturating_sub(OVERSCAN_COLS);
        let c1 = (xs.partition_point(|&x| x < self.vp.x + self.vp.w) + OVERSCAN_COLS).min(n);
        (c0.min(n), c1.max(c0.min(n)))
    }

    fn row_range(&self, len: usize) -> (usize, usize) {
        let first = ((self.vp.y / ROW_H).floor() as usize)
            .saturating_sub(OVERSCAN_ROWS)
            .min(len);
        let last = (first + (self.vp.h / ROW_H).ceil() as usize + 2 * OVERSCAN_ROWS).min(len);
        (first, last)
    }

    fn cell_el(&self, r: usize, c: usize, w: f32, header: bool) -> Element<'_, Msg> {
        if let Some(e) = self.edit.as_ref().filter(|e| e.r == r && e.c == c) {
            let input = text_input("", &e.value)
                .id(self.input_id.clone())
                .size(13)
                .padding([3, 5])
                .width(w)
                .on_input(Msg::Input)
                .on_submit(Msg::Commit(Move::Down));
            return keys(input, |k, m| match k {
                keyboard::Key::Named(keyboard::key::Named::Tab) if m.shift() => {
                    Some(Msg::Commit(Move::Left))
                }
                keyboard::Key::Named(keyboard::key::Named::Tab) => Some(Msg::Commit(Move::Right)),
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Msg::Cancel),
                _ => None,
            });
        }
        let selected = self.cursor == (r, c);
        let t = text(self.cell(r, c).to_string())
            .size(13)
            .wrapping(Wrapping::None);
        let t = if header {
            t.font(Font {
                weight: iced::font::Weight::Bold,
                ..Font::DEFAULT
            })
        } else {
            t
        };
        let body = container(t)
            .width(w)
            .height(ROW_H)
            .padding([3, 6])
            .clip(true)
            .style(move |th: &Theme| cell_style(th, selected, header));
        mouse_area(body).on_press(Msg::Click(r, c)).into()
    }

    fn grid(&self) -> Element<'_, Msg> {
        let xs = self.xs();
        let total_w = *xs.last().unwrap_or(&0.0);
        let (c0, c1) = self.col_range(&xs);
        let base = self.base();
        let nrows = self.sheet().rows.len().saturating_sub(base).max(1);
        let (first, last) = self.row_range(nrows);
        let ws = &self.widths[self.cur];

        let (left, right) = (xs[c0], total_w - xs[c1] + BAR);
        let hspan = |els, h| hspan(els, h, left, right);

        // frozen top strip: letters (+ header names)
        let letters: Vec<Element<'_, Msg>> = (c0..c1)
            .map(|c| {
                container(text(col_name(c)).size(11).color(theme::MUTED))
                    .width(ws[c])
                    .height(LETTER_H)
                    .center_x(ws[c])
                    .style(move |th: &Theme| strip_style(th, self.cursor.1 == c))
                    .into()
            })
            .collect();
        let mut head = column![hspan(letters, LETTER_H)];
        if base == 1 {
            let names = (c0..c1).map(|c| self.cell_el(0, c, ws[c], true)).collect();
            head = head.push(hspan(names, ROW_H));
        }
        let head_h = LETTER_H + base as f32 * ROW_H;
        let head = scrollable(head)
            .id(self.head_id.clone())
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(Length::Fill)
            .height(head_h);

        // frozen left strip: row numbers
        let nums = column![
            Space::with_height(first as f32 * ROW_H),
            column((first..last).map(|i| {
                let r = i + base;
                container(text((r + 1).to_string()).size(11).color(theme::MUTED))
                    .width(NUM_W)
                    .height(ROW_H)
                    .padding([4, 6])
                    .style(move |th: &Theme| strip_style(th, self.cursor.0 == r))
                    .into()
            })),
            Space::with_height((nrows - last) as f32 * ROW_H + BAR),
        ];
        let nums = scrollable(nums)
            .id(self.nums_id.clone())
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .width(NUM_W)
            .height(Length::Fill);

        // body
        let body = column![
            Space::with_height(first as f32 * ROW_H),
            column((first..last).map(|i| {
                let r = i + base;
                hspan(
                    (c0..c1).map(|c| self.cell_el(r, c, ws[c], false)).collect(),
                    ROW_H,
                )
            })),
            Space::with_height((nrows - last) as f32 * ROW_H),
        ]
        .width(total_w + BAR);
        let body = scrollable(body)
            .id(self.grid_id.clone())
            .direction(scrollable::Direction::Both {
                vertical: scrollable::Scrollbar::default(),
                horizontal: scrollable::Scrollbar::default(),
            })
            .on_scroll(Msg::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);

        column![
            row![Space::new(NUM_W, head_h), head],
            row![nums, body].height(Length::Fill),
        ]
        .into()
    }
}

/// One virtualized row: spacers stand in for the off-screen columns.
fn hspan<'a>(els: Vec<Element<'a, Msg>>, h: f32, left: f32, right: f32) -> Element<'a, Msg> {
    row![Space::with_width(left), row(els), Space::with_width(right)]
        .height(h)
        .into()
}

fn cell_style(t: &Theme, selected: bool, header: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: if selected {
            Some(p.primary.weak.color.scale_alpha(0.45).into())
        } else if header {
            Some(p.background.strong.color.scale_alpha(0.35).into())
        } else {
            None
        },
        text_color: Some(p.background.base.text),
        border: iced::Border {
            width: if selected { 1.0 } else { 0.5 },
            color: if selected {
                p.primary.base.color
            } else {
                p.background.strong.color.scale_alpha(0.6)
            },
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn strip_style(t: &Theme, active: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: Some(if active {
            p.primary.weak.color.scale_alpha(0.3).into()
        } else {
            p.background.weak.color.scale_alpha(0.3).into()
        }),
        ..Default::default()
    }
}

// ------------------------------------------------- key-intercepting wrapper

/// Wraps `content` and turns selected key presses into messages before the
/// child sees them (text_input swallows Tab/Escape otherwise).
fn keys<'a>(
    content: impl Into<Element<'a, Msg>>,
    on_key: fn(&keyboard::Key, keyboard::Modifiers) -> Option<Msg>,
) -> Element<'a, Msg> {
    Element::new(Keys {
        content: content.into(),
        on_key,
    })
}

struct Keys<'a> {
    content: Element<'a, Msg>,
    on_key: fn(&keyboard::Key, keyboard::Modifiers) -> Option<Msg>,
}

impl Widget<Msg, Theme, iced::Renderer> for Keys<'_> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        )
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_ref(&self.content))
    }

    fn operate(
        &self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget()
            .operate(&mut tree.children[0], layout, renderer, operation)
    }

    fn on_event(
        &mut self,
        tree: &mut Tree,
        event: iced::Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        viewport: &Rectangle,
    ) -> iced::event::Status {
        if let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = &event {
            if let Some(m) = (self.on_key)(key, *modifiers) {
                shell.publish(m);
                return iced::event::Status::Captured;
            }
        }
        self.content.as_widget_mut().on_event(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        )
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'_>,
        renderer: &iced::Renderer,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Msg, Theme, iced::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, translation)
    }
}

// -------------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AO};

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = crate::config::home()
                .join(".cache/ogx-p2-sheet-tests")
                .join(format!(
                    "sheet-{tag}-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, AO::SeqCst)
                ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
        fn p(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn state(path: &Path) -> State {
        State::new(load(path).unwrap()).0
    }

    fn rows(s: &State) -> Vec<Vec<String>> {
        s.sheet().rows.clone()
    }

    fn v(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| s.to_string()).collect())
            .collect()
    }

    fn edit_cell(s: &mut State, r: usize, c: usize, val: &str) {
        let _ = s.update(Msg::Click(r, c));
        let _ = s.update(Msg::Input(val.into()));
        let _ = s.update(Msg::Commit(Move::Stay));
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff(b"a,b,c\n1,2,3\n4,5,6\n"), b',');
        assert_eq!(sniff(b"a;b;c\n1;2,5;3\n4;5;6\n"), b';');
        assert_eq!(sniff(b"a\tb\tc\n1\t2\t3\n"), b'\t');
        // delimiters inside quotes don't count
        assert_eq!(sniff(b"\"x;y;z\",b\n\"p;q;r\",c\n"), b',');
        assert_eq!(sniff(b"single\ncolumn\n"), b',');
        let t = Tmp::new("sniff");
        std::fs::write(t.p("s.csv"), "name;age\nann;3\nbob;4\n").unwrap();
        let o = load(&t.p("s.csv")).unwrap();
        assert_eq!(o.fmt, Fmt::Csv(b';'));
        assert_eq!(
            o.sheets[0].rows,
            v(&[&["name", "age"], &["ann", "3"], &["bob", "4"]])
        );
    }

    #[test]
    fn csv_cap_and_bom() {
        let t = Tmp::new("cap");
        std::fs::write(t.p("a.csv"), "\u{feff}h1,h2\n1,2\n3,4\n5,6\n").unwrap();
        let (r, trunc) = read_csv(&t.p("a.csv"), b',', 2).unwrap();
        assert!(trunc);
        assert_eq!(r, v(&[&["h1", "h2"], &["1", "2"]]));
        let (r, trunc) = read_csv(&t.p("a.csv"), b',', 10).unwrap();
        assert!(!trunc);
        assert_eq!(r.len(), 4);
    }

    #[test]
    fn csv_edit_save_round_trip() {
        let t = Tmp::new("csv");
        let p = t.p("data.csv");
        std::fs::write(&p, "name,qty\n\"Smith, J\",3\nann,10\n").unwrap();
        let mut s = state(&p);
        assert_eq!(s.is_editing(), Some(false));
        // read-only: clicks select, nothing is edited
        let _ = s.update(Msg::Click(1, 1));
        let _ = s.update(Msg::AddRow);
        assert!(!s.is_dirty());
        let _ = s.toggle_edit();
        assert_eq!(s.is_editing(), Some(true));
        edit_cell(&mut s, 1, 1, "4");
        assert!(s.is_dirty());
        // leaving edit mode while dirty is refused
        let _ = s.toggle_edit();
        assert_eq!(s.is_editing(), Some(true));
        // Enter moves down into the next row's cell
        let _ = s.update(Msg::Click(2, 0));
        let _ = s.update(Msg::Input("bob".into()));
        let _ = s.update(Msg::Commit(Move::Down));
        assert_eq!(s.cursor, (2, 0));
        let _ = s.update(Msg::Commit(Move::Right));
        assert_eq!(s.cursor, (2, 1));
        let _ = s.update(Msg::Cancel);
        let bytes = encode(s.fmt, &s.sheets).unwrap();
        std::fs::write(&p, bytes).unwrap();
        let back = load(&p).unwrap();
        assert_eq!(
            back.sheets[0].rows,
            v(&[&["name", "qty"], &["Smith, J", "4"], &["bob", "10"]])
        );
        // tsv keeps its delimiter
        let tp = t.p("d.tsv");
        std::fs::write(&tp, "a\tb\n1\t2\n").unwrap();
        let mut s = state(&tp);
        let _ = s.toggle_edit();
        edit_cell(&mut s, 1, 0, "x y");
        std::fs::write(&tp, encode(s.fmt, &s.sheets).unwrap()).unwrap();
        assert_eq!(std::fs::read_to_string(&tp).unwrap(), "a\tb\nx y\t2\n");
    }

    #[test]
    fn rows_cols_and_sort() {
        let t = Tmp::new("ops");
        let p = t.p("o.csv");
        std::fs::write(&p, "k,v\nb,10\na,9\nc,\nd,100\n").unwrap();
        let mut s = state(&p);
        let _ = s.toggle_edit();
        let _ = s.update(Msg::ToggleHeader);
        // sort by column B (numeric, empties last), header stays put
        let _ = s.update(Msg::Click(1, 1));
        let _ = s.update(Msg::Cancel);
        let _ = s.update(Msg::Sort(true));
        assert_eq!(
            rows(&s),
            v(&[
                &["k", "v"],
                &["a", "9"],
                &["b", "10"],
                &["d", "100"],
                &["c", ""]
            ])
        );
        let _ = s.update(Msg::Sort(false));
        assert_eq!(
            rows(&s),
            v(&[
                &["k", "v"],
                &["d", "100"],
                &["b", "10"],
                &["a", "9"],
                &["c", ""]
            ])
        );
        // natural text sort on column A
        let _ = s.update(Msg::Click(1, 0));
        let _ = s.update(Msg::Cancel);
        let _ = s.update(Msg::Sort(true));
        assert_eq!(
            rows(&s)[1..]
                .iter()
                .map(|r| r[0].as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c", "d"]
        );
        // add a row below the cursor (row 1) and fill it
        let _ = s.update(Msg::AddRow);
        assert_eq!(s.cursor.0, 2);
        assert_eq!(rows(&s).len(), 6);
        edit_cell(&mut s, 2, 0, "new");
        assert_eq!(rows(&s)[2], vec!["new".to_string()]);
        // delete it again
        let _ = s.update(Msg::Click(2, 0));
        let _ = s.update(Msg::Cancel);
        let _ = s.update(Msg::DelRow);
        assert_eq!(rows(&s).len(), 5);
        assert!(rows(&s)
            .iter()
            .all(|r| r.first().map(String::as_str) != Some("new")));
        // columns: insert right of A, then delete it
        let _ = s.update(Msg::Click(0, 0));
        let _ = s.update(Msg::Cancel);
        let _ = s.update(Msg::AddCol);
        assert_eq!(s.ncols(), 3);
        assert_eq!(rows(&s)[0], v(&[&["k", "", "v"]])[0]);
        let _ = s.update(Msg::DelCol);
        assert_eq!(s.ncols(), 2);
        assert_eq!(rows(&s)[0], v(&[&["k", "v"]])[0]);
        assert!(s.is_dirty());
    }

    #[test]
    fn xlsx_edit_save_round_trip() {
        use calamine::{Data, Reader};
        let t = Tmp::new("xlsx");
        let p = t.p("book.xlsx");
        {
            let mut wb = rust_xlsxwriter::Workbook::new();
            let ws = wb.add_worksheet();
            ws.set_name("Prices").unwrap();
            ws.write_string(0, 0, "item").unwrap();
            ws.write_string(0, 1, "price").unwrap();
            ws.write_string(1, 0, "tea").unwrap();
            ws.write_number(1, 1, 2.5).unwrap();
            let ws2 = wb.add_worksheet();
            ws2.set_name("Notes").unwrap();
            ws2.write_string(2, 1, "hello").unwrap();
            wb.save(&p).unwrap();
        }
        let mut s = state(&p);
        assert_eq!(s.fmt, Fmt::Xlsx);
        assert_eq!(s.sheets.len(), 2);
        assert_eq!(s.sheets[1].rows, v(&[&[], &[], &["", "hello"]]));
        let _ = s.toggle_edit();
        edit_cell(&mut s, 1, 1, "3.75");
        let _ = s.update(Msg::AddRow);
        edit_cell(&mut s, 2, 0, "007");
        // first Save only warns (values only), the second one saves
        let _ = s.save();
        assert!(s.confirm && !s.saving);
        let _ = s.save();
        assert!(s.saving && !s.confirm && s.warned);
        std::fs::write(&p, encode(s.fmt, &s.sheets).unwrap()).unwrap();

        let mut wb = calamine::open_workbook_auto(&p).unwrap();
        assert_eq!(
            wb.sheet_names(),
            vec!["Prices".to_string(), "Notes".to_string()]
        );
        let r = wb.worksheet_range("Prices").unwrap();
        assert_eq!(r.get_value((1, 1)), Some(&Data::Float(3.75)));
        assert_eq!(r.get_value((2, 0)), Some(&Data::String("007".into())));
        let r = wb.worksheet_range("Notes").unwrap();
        assert_eq!(r.get_value((2, 1)), Some(&Data::String("hello".into())));
    }

    #[test]
    fn ods_round_trip() {
        let t = Tmp::new("ods");
        let p = t.p("b.ods");
        let sheets = vec![SheetData {
            name: "S".into(),
            rows: v(&[&["a", "1"], &["", "x"]]),
            truncated: false,
        }];
        std::fs::write(&p, write_ods(&sheets).unwrap()).unwrap();
        let o = load(&p).unwrap();
        assert_eq!(o.fmt, Fmt::Ods);
        assert_eq!(o.sheets[0].rows, sheets[0].rows);
    }

    #[test]
    fn view_builds() {
        let t = Tmp::new("view");
        let p = t.p("v.csv");
        std::fs::write(&p, "a,b\n1,2\n").unwrap();
        let mut s = state(&p);
        let _ = s.view();
        let _ = s.update(Msg::ToggleHeader);
        let _ = s.toggle_edit();
        let _ = s.update(Msg::Click(1, 1));
        let _ = s.view();
        s.confirm = true;
        let _ = s.view();
    }

    #[test]
    fn helpers() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
        assert_eq!(as_number("3"), Some(3.0));
        assert_eq!(as_number("1.50"), None);
        assert_eq!(as_number("abc"), None);
        assert_eq!(excel_serial(45_000.0, false), "2023-03-15");
        assert_eq!(excel_serial(45_000.5, false), "2023-03-15 12:00:00");
        assert!(supported(Path::new("x.XLSX")) && !supported(Path::new("x.txt")));
    }
}
