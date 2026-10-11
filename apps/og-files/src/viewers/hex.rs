//! Hex viewer for any file: virtualized offset / hex / ASCII rows, read in
//! 64 KiB chunks on demand (small LRU cache, so multi-GB files never load
//! whole), go to offset (hex or decimal), find bytes or text on a worker
//! (cancellable), and a side strip decoding the selected byte. Read-only.

use super::Event;
use crate::theme;
use crate::widgets::{virtual_column, Vp};
use iced::widget::{
    button, column, container, mouse_area, row, scrollable, text, text_input, Space,
};
use iced::{Element, Font, Length, Task, Theme};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const CHUNK: u64 = 64 * 1024;
const CACHE_CHUNKS: usize = 32;
const PER_ROW: u64 = 16;
const ROW_H: f32 = 18.0;
/// bytes scanned per read while searching
const FIND_BLOCK: usize = 1 << 20;
const CANCELLED: &str = "Cancelled";

/// Does this viewer handle `path` (by extension / sniffing)? Any file.
pub fn supported(path: &Path) -> bool {
    !path.is_dir()
}

/// Everything `open` produced, handed to `State::new`. Must stay
/// `Clone + Debug + Send` (it travels in a Message).
#[derive(Debug, Clone)]
pub struct Opened {
    pub path: PathBuf,
    pub len: u64,
}

/// Load off the UI thread (do blocking work in `spawn_blocking`).
pub async fn open(path: PathBuf) -> Result<Opened, String> {
    tokio::task::spawn_blocking(move || {
        let m = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if m.is_dir() {
            return Err("is a folder".into());
        }
        Ok(Opened { len: m.len(), path })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ------------------------------------------------------------- blocking io

/// Chunk `idx` (CHUNK bytes from idx*CHUNK; shorter at EOF, empty past it).
fn read_chunk(path: &Path, idx: u64) -> Result<Vec<u8>, String> {
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(idx * CHUNK))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity(CHUNK as usize);
    f.take(CHUNK)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// First occurrence of `pat` starting at or after `from` and before `end`
/// (match must start < end), reading `block` bytes at a time so matches
/// spanning reads are still found. Checks `cancel` between reads.
fn find_in(
    path: &Path,
    pat: &[u8],
    from: u64,
    end: u64,
    cancel: &AtomicBool,
    block: usize,
) -> Result<Option<u64>, String> {
    if pat.is_empty() {
        return Ok(None);
    }
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(from)).map_err(|e| e.to_string())?;
    let mut hay: Vec<u8> = Vec::with_capacity(block + pat.len());
    let mut hay_start = from; // file offset of hay[0]
    let mut buf = vec![0u8; block.max(1)];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        if hay_start >= end {
            return Ok(None);
        }
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(None);
        }
        hay.extend_from_slice(&buf[..n]);
        if let Some(i) = memmem(&hay, pat) {
            let at = hay_start + i as u64;
            return Ok((at < end).then_some(at));
        }
        // keep the tail that could start a match spanning into the next read
        let keep = (pat.len() - 1).min(hay.len());
        let drop = hay.len() - keep;
        hay.drain(..drop);
        hay_start += drop as u64;
    }
}

fn memmem(hay: &[u8], pat: &[u8]) -> Option<usize> {
    if pat.len() > hay.len() {
        return None;
    }
    let first = pat[0];
    (0..=hay.len() - pat.len()).find(|&i| hay[i] == first && &hay[i..i + pat.len()] == pat)
}

// ------------------------------------------------------------------ parsing

/// "0x1F", "1Fh", "$1F" or anything with hex letters → hex; plain digits →
/// decimal.
fn parse_offset(s: &str) -> Option<u64> {
    let s = s.trim().replace(['_', ' '], "");
    if s.is_empty() {
        return None;
    }
    let hex = |h: &str| u64::from_str_radix(h, 16).ok();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return hex(h);
    }
    if let Some(h) = s.strip_prefix('$') {
        return hex(h);
    }
    if let Some(h) = s.strip_suffix(['h', 'H']) {
        return hex(h);
    }
    if s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    hex(&s)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FindMode {
    /// hex if it looks like bytes ("de ad be ef", "0xdeadbeef"), else text
    Auto,
    Hex,
    Text,
}

fn parse_hex_bytes(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    let digits: String = s.split_whitespace().collect();
    if digits.is_empty() || digits.len() % 2 != 0 {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok())
        .collect()
}

fn parse_pattern(s: &str, mode: FindMode) -> Result<Vec<u8>, String> {
    if s.is_empty() {
        return Err("Nothing to find".into());
    }
    match mode {
        FindMode::Text => Ok(s.as_bytes().to_vec()),
        FindMode::Hex => {
            parse_hex_bytes(s).ok_or_else(|| "Not hex bytes (e.g. de ad be ef)".into())
        }
        FindMode::Auto => {
            let t = s.trim();
            let tokens: Vec<&str> = t.split_whitespace().collect();
            let looks_hex = t.starts_with("0x")
                || (tokens.len() > 1
                    && tokens
                        .iter()
                        .all(|w| w.len() % 2 == 0 && w.bytes().all(|b| b.is_ascii_hexdigit())));
            match parse_hex_bytes(t) {
                Some(b) if looks_hex => Ok(b),
                _ => Ok(s.as_bytes().to_vec()),
            }
        }
    }
}

// -------------------------------------------------------------------- cache

/// Chunk payload; Debug prints only the length.
#[derive(Clone)]
pub struct Chunk(Arc<Vec<u8>>);

impl std::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Chunk({} bytes)", self.0.len())
    }
}

struct Lru {
    map: HashMap<u64, Chunk>,
    order: VecDeque<u64>,
    cap: usize,
}

impl Lru {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    fn get(&self, idx: u64) -> Option<&Chunk> {
        self.map.get(&idx)
    }

    fn touch(&mut self, idx: u64) {
        if let Some(p) = self.order.iter().position(|&i| i == idx) {
            self.order.remove(p);
            self.order.push_back(idx);
        }
    }

    fn insert(&mut self, idx: u64, c: Chunk) {
        if self.map.insert(idx, c).is_none() {
            self.order.push_back(idx);
        } else {
            self.touch(idx);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

// -------------------------------------------------------------------- state

pub struct State {
    /// selection panel beside the rows (wide) or underneath (narrow)
    beside: bool,
    path: PathBuf,
    len: u64,
    cache: Lru,
    pending: HashSet<u64>,
    vp: Vp,
    sel: Option<u64>,
    goto: String,
    find: String,
    mode: FindMode,
    searching: Option<Arc<AtomicBool>>,
    error: Option<String>,
    scroll_id: scrollable::Id,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Scrolled(scrollable::Viewport),
    Loaded(u64, Result<Chunk, String>),
    Select(u64),
    GotoInput(String),
    Goto,
    FindInput(String),
    CycleMode,
    FindNext,
    /// Some((offset, wrapped around to the start))
    Found(Result<Option<(u64, bool)>, String>),
    CancelFind,
}

impl State {
    pub fn new(o: Opened) -> (Self, Task<Msg>) {
        let mut s = Self {
            beside: false,
            path: o.path,
            len: o.len,
            cache: Lru::new(CACHE_CHUNKS),
            pending: HashSet::new(),
            vp: Vp::default(),
            sel: None,
            goto: String::new(),
            find: String::new(),
            mode: FindMode::Auto,
            searching: None,
            error: None,
            scroll_id: scrollable::Id::unique(),
        };
        let t = s.request();
        (s, t)
    }

    fn rows(&self) -> usize {
        self.len.div_ceil(PER_ROW) as usize
    }

    fn byte(&self, off: u64) -> Option<u8> {
        if off >= self.len {
            return None;
        }
        let c = self.cache.get(off / CHUNK)?;
        c.0.get((off % CHUNK) as usize).copied()
    }

    fn load(&mut self, idx: u64) -> Task<Msg> {
        if idx * CHUNK >= self.len.max(1) || self.pending.contains(&idx) {
            return Task::none();
        }
        if self.cache.get(idx).is_some() {
            self.cache.touch(idx);
            return Task::none();
        }
        self.pending.insert(idx);
        let path = self.path.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || read_chunk(&path, idx))
                    .await
                    .map_err(|e| e.to_string())?
                    .map(|v| Chunk(Arc::new(v)))
            },
            move |r| Msg::Loaded(idx, r),
        )
    }

    /// Load the chunks behind the visible rows (plus the selection strip).
    fn request(&mut self) -> Task<Msg> {
        if self.len == 0 {
            return Task::none();
        }
        let first_row = (self.vp.offset / ROW_H).max(0.0) as u64;
        let last_row = first_row + (self.vp.height / ROW_H).ceil() as u64 + 8;
        let a = (first_row.saturating_sub(8) * PER_ROW) / CHUNK;
        let b = ((last_row * PER_ROW).min(self.len - 1)) / CHUNK;
        let mut idxs: Vec<u64> = (a..=b).collect();
        if let Some(s) = self.sel {
            idxs.push(s / CHUNK);
            idxs.push((s + 3).min(self.len - 1) / CHUNK);
        }
        idxs.dedup();
        Task::batch(idxs.into_iter().map(|i| self.load(i)).collect::<Vec<_>>())
    }

    /// Select `off` and scroll it into the middle of the view.
    fn jump(&mut self, off: u64) -> Task<Msg> {
        let off = off.min(self.len.saturating_sub(1));
        self.sel = Some(off);
        let row = off / PER_ROW;
        if self.vp.reveal(row as usize, ROW_H).is_some() {
            let y = (row as f32 * ROW_H - self.vp.height / 2.0).max(0.0);
            self.vp.offset = y;
            Task::batch([
                scrollable::scroll_to(
                    self.scroll_id.clone(),
                    scrollable::AbsoluteOffset { x: 0.0, y },
                ),
                self.request(),
            ])
        } else {
            self.request()
        }
    }

    pub fn update(&mut self, msg: Msg) -> (Task<Msg>, Option<Event>) {
        match msg {
            Msg::Scrolled(v) => {
                self.vp = Vp::from(v);
                // decide on the full width (rows + panel when beside) so the
                // layout doesn't flip-flop at the threshold
                let total = self.vp.width + if self.beside { 198.0 } else { 0.0 };
                self.beside = total > 900.0;
                (self.request(), None)
            }
            Msg::Loaded(idx, r) => {
                self.pending.remove(&idx);
                match r {
                    Ok(c) => {
                        self.cache.insert(idx, c);
                        (Task::none(), None)
                    }
                    Err(e) => {
                        self.error = Some(e.clone());
                        (
                            Task::none(),
                            Some(Event::Status(format!("Read failed: {e}"))),
                        )
                    }
                }
            }
            Msg::Select(off) => {
                self.sel = Some(off);
                (self.request(), None)
            }
            Msg::GotoInput(s) => {
                self.goto = s;
                (Task::none(), None)
            }
            Msg::Goto => match parse_offset(&self.goto) {
                Some(off) if off < self.len => (self.jump(off), None),
                Some(off) => (
                    self.jump(off),
                    Some(Event::Status(format!(
                        "Offset {off:#x} is past the end ({:#x} bytes)",
                        self.len
                    ))),
                ),
                None => (
                    Task::none(),
                    Some(Event::Status(
                        "Offset: decimal (1234) or hex (0x4d2, 4d2h)".into(),
                    )),
                ),
            },
            Msg::FindInput(s) => {
                self.find = s;
                (Task::none(), None)
            }
            Msg::CycleMode => {
                self.mode = match self.mode {
                    FindMode::Auto => FindMode::Hex,
                    FindMode::Hex => FindMode::Text,
                    FindMode::Text => FindMode::Auto,
                };
                (Task::none(), None)
            }
            Msg::FindNext => {
                if self.searching.is_some() || self.len == 0 {
                    return (Task::none(), None);
                }
                let pat = match parse_pattern(&self.find, self.mode) {
                    Ok(p) => p,
                    Err(e) => return (Task::none(), Some(Event::Status(e))),
                };
                let cancel = Arc::new(AtomicBool::new(false));
                self.searching = Some(cancel.clone());
                let (path, len) = (self.path.clone(), self.len);
                let from = self.sel.map(|s| s + 1).unwrap_or(0).min(len);
                let task = Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            if let Some(at) = find_in(&path, &pat, from, len, &cancel, FIND_BLOCK)?
                            {
                                return Ok(Some((at, false)));
                            }
                            // wrap around: the part before `from`
                            let end = from.min(len);
                            Ok(find_in(&path, &pat, 0, end, &cancel, FIND_BLOCK)?
                                .map(|at| (at, true)))
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    Msg::Found,
                );
                (task, Some(Event::Status("Searching…".into())))
            }
            Msg::Found(r) => {
                self.searching = None;
                match r {
                    Ok(Some((at, wrapped))) => (
                        self.jump(at),
                        Some(Event::Status(format!(
                            "Found at {at:#x} ({at}){}",
                            if wrapped { " — wrapped to start" } else { "" }
                        ))),
                    ),
                    Ok(None) => (Task::none(), Some(Event::Status("Not found".into()))),
                    Err(e) => (Task::none(), Some(Event::Status(format!("Find: {e}")))),
                }
            }
            Msg::CancelFind => {
                if let Some(c) = &self.searching {
                    c.store(true, Ordering::Relaxed);
                }
                (Task::none(), None)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bar = row![
            text("Go to").size(12).color(theme::MUTED),
            text_input("0x1f40 or 8000", &self.goto)
                .size(12)
                .width(140)
                .on_input(Msg::GotoInput)
                .on_submit(Msg::Goto),
            Space::with_width(12),
            text("Find").size(12).color(theme::MUTED),
            text_input("de ad be ef  or text", &self.find)
                .size(12)
                .width(200)
                .on_input(Msg::FindInput)
                .on_submit(Msg::FindNext),
            button(
                text(match self.mode {
                    FindMode::Auto => "Auto",
                    FindMode::Hex => "Hex",
                    FindMode::Text => "Text",
                })
                .size(12)
            )
            .style(button::secondary)
            .on_press(Msg::CycleMode),
            if self.searching.is_some() {
                button(text("Cancel").size(12))
                    .style(button::danger)
                    .on_press(Msg::CancelFind)
            } else {
                button(text("Find next").size(12)).on_press(Msg::FindNext)
            },
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center)
        // wraps instead of crushing labels when the viewer is narrow
        .wrap();
        let size = text(format!(
            "{} bytes ({}) · read-only",
            self.len,
            crate::fsops::human_size(self.len)
        ))
        .size(12)
        .color(theme::MUTED);

        let digits = (64 - self.len.max(1).leading_zeros()).div_ceil(4).max(8) as usize;
        let list = virtual_column(self.rows(), ROW_H, self.vp, |r| {
            self.row_el(r as u64, digits)
        });
        let list = scrollable(list)
            .id(self.scroll_id.clone())
            .on_scroll(Msg::Scrolled)
            .width(Length::Fill)
            .height(Length::Fill);
        let body: Element<'_, Msg> = if self.len == 0 {
            container(text("Empty file").color(theme::MUTED))
                .center(Length::Fill)
                .into()
        } else {
            list.into()
        };
        // the byte rows need ~720 px; below that the selection panel moves
        // underneath instead of squeezing the rows
        let main: Element<'_, Msg> = if self.beside {
            row![body, self.side(true)].spacing(8).into()
        } else {
            column![body, self.side(false)].spacing(6).into()
        };
        column![bar, size, main].spacing(6).into()
    }

    fn row_el(&self, r: u64, digits: usize) -> Element<'_, Msg> {
        let start = r * PER_ROW;
        let mono = |s: String| text(s).size(13).font(Font::MONOSPACE);
        let offset = mono(format!("{start:0digits$x}")).color(theme::MUTED);
        if self.cache.get(start / CHUNK).is_none() {
            return row![offset, Space::with_width(16), mono("…".into())].into();
        }
        let end = (start + PER_ROW).min(self.len);
        let mut hex = row![].spacing(0);
        let mut ascii = row![];
        for off in start..start + PER_ROW {
            let selected = self.sel == Some(off);
            let (h, a) = match self.byte(off).filter(|_| off < end) {
                Some(b) => (
                    format!("{b:02x}"),
                    if b.is_ascii_graphic() || b == b' ' {
                        (b as char).to_string()
                    } else {
                        "·".into()
                    },
                ),
                None => (String::new(), String::new()),
            };
            let style = move |t: &Theme| byte_style(t, selected);
            hex = hex.push(
                mouse_area(container(mono(h)).width(24).padding([0, 2]).style(style))
                    .on_press(Msg::Select(off)),
            );
            if off - start == 7 {
                hex = hex.push(Space::with_width(8));
            }
            ascii = ascii.push(
                mouse_area(container(mono(a)).width(9).style(style)).on_press(Msg::Select(off)),
            );
        }
        row![
            offset,
            Space::with_width(16),
            hex,
            Space::with_width(16),
            ascii
        ]
        .into()
    }

    fn side(&self, beside: bool) -> Element<'_, Msg> {
        let mono = |s: String| text(s).size(12).font(Font::MONOSPACE);
        let mut col = column![text("Selection").size(12).color(theme::MUTED)].spacing(3);
        match self.sel {
            None => col = col.push(text("Click a byte").size(12).color(theme::MUTED)),
            Some(off) => {
                let b = |i: u64| self.byte(off + i);
                let n = |k: u64| -> Option<Vec<u8>> { (0..k).map(b).collect() };
                let fmt = |v: Option<String>| v.unwrap_or_else(|| "—".into());
                col = col
                    .push(mono(format!("offset {off:#x}")))
                    .push(mono(format!("       {off}")))
                    .push(Space::with_height(4))
                    .push(mono(format!("u8     {}", fmt(b(0).map(|x| x.to_string())))))
                    .push(mono(format!(
                        "i8     {}",
                        fmt(b(0).map(|x| (x as i8).to_string()))
                    )))
                    .push(mono(format!(
                        "bin    {}",
                        fmt(b(0).map(|x| format!("{x:08b}")))
                    )))
                    .push(mono(format!(
                        "u16 LE {}",
                        fmt(n(2).map(|v| u16::from_le_bytes([v[0], v[1]]).to_string()))
                    )))
                    .push(mono(format!(
                        "u16 BE {}",
                        fmt(n(2).map(|v| u16::from_be_bytes([v[0], v[1]]).to_string()))
                    )))
                    .push(mono(format!(
                        "u32 LE {}",
                        fmt(n(4).map(|v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]).to_string()))
                    )))
                    .push(mono(format!(
                        "u32 BE {}",
                        fmt(n(4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]).to_string()))
                    )));
            }
        }
        if let Some(e) = &self.error {
            col = col.push(text(format!("Read error: {e}")).size(11));
        }
        let c = container(col).padding(8).style(theme::panel);
        if beside {
            c.width(190).height(Length::Fill).into()
        } else {
            c.width(Length::Fill).into()
        }
    }

    /// Unsaved edits exist (tab close asks first). Read-only: never.
    pub fn is_dirty(&self) -> bool {
        false
    }

    /// Some(editing) if this viewer has an edit mode, None otherwise.
    pub fn is_editing(&self) -> Option<bool> {
        None
    }

    /// Ctrl+E: no edit mode.
    pub fn toggle_edit(&mut self) -> Task<Msg> {
        Task::none()
    }

    /// Ctrl+S: read-only, nothing to save.
    pub fn save(&mut self) -> Task<Msg> {
        Task::none()
    }
}

fn byte_style(t: &Theme, selected: bool) -> container::Style {
    let p = t.extended_palette();
    container::Style {
        background: selected.then(|| p.primary.weak.color.scale_alpha(0.6).into()),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Tmp {
            static N: AtomicU64 = AtomicU64::new(0);
            let p = crate::config::home()
                .join(".cache/ogx-p2-sheet-tests")
                .join(format!(
                    "hex-{tag}-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::SeqCst)
                ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn pattern_file(t: &Tmp, len: usize) -> PathBuf {
        let p = t.0.join("data.bin");
        let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn chunk_reader_near_eof() {
        let t = Tmp::new("chunk");
        let len = 2 * CHUNK as usize + 100;
        let p = pattern_file(&t, len);
        let c0 = read_chunk(&p, 0).unwrap();
        assert_eq!(c0.len(), CHUNK as usize);
        assert_eq!(c0[5], 5);
        let c1 = read_chunk(&p, 1).unwrap();
        assert_eq!(c1[0], (CHUNK as usize % 251) as u8);
        let c2 = read_chunk(&p, 2).unwrap();
        assert_eq!(c2.len(), 100);
        assert_eq!(c2[99], ((len - 1) % 251) as u8);
        assert!(read_chunk(&p, 3).unwrap().is_empty());

        // state view of the same file: byte lookups through the cache
        let (mut s, _) = State::new(Opened {
            path: p.clone(),
            len: len as u64,
        });
        let _ = s.update(Msg::Loaded(2, Ok(Chunk(Arc::new(c2)))));
        assert_eq!(s.byte(len as u64 - 1), Some(((len - 1) % 251) as u8));
        assert_eq!(s.byte(len as u64), None);
        assert_eq!(s.byte(0), None); // chunk 0 not loaded
        assert_eq!(s.rows(), len.div_ceil(16));
        let _ = s.update(Msg::Select(len as u64 - 2));
        let _ = s.view();
    }

    #[test]
    fn find_across_boundaries() {
        let t = Tmp::new("find");
        let p = t.0.join("f.bin");
        let mut data = vec![0u8; 100];
        data[14..18].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        data[60..64].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        std::fs::write(&p, &data).unwrap();
        let no = AtomicBool::new(false);
        let pat = [0xde, 0xad, 0xbe, 0xef];
        // block 16: the first match straddles the 16-byte boundary
        assert_eq!(find_in(&p, &pat, 0, 100, &no, 16).unwrap(), Some(14));
        assert_eq!(find_in(&p, &pat, 15, 100, &no, 16).unwrap(), Some(60));
        assert_eq!(find_in(&p, &pat, 61, 100, &no, 16).unwrap(), None);
        // end bound (used for the wrap-around pass)
        assert_eq!(find_in(&p, &pat, 0, 14, &no, 16).unwrap(), None);
        assert_eq!(find_in(&p, &pat, 0, 15, &no, 7).unwrap(), Some(14));
        // tiny blocks
        assert_eq!(find_in(&p, &pat, 0, 100, &no, 1).unwrap(), Some(14));
        let yes = AtomicBool::new(true);
        assert_eq!(find_in(&p, &pat, 0, 100, &yes, 16), Err(CANCELLED.into()));
        // across a real 64 KiB chunk boundary with the default block size
        let p2 = t.0.join("g.bin");
        let mut big = vec![0u8; 3 * CHUNK as usize];
        let at = CHUNK as usize - 3;
        big[at..at + 6].copy_from_slice(b"needle");
        std::fs::write(&p2, &big).unwrap();
        assert_eq!(
            find_in(&p2, b"needle", 0, big.len() as u64, &no, CHUNK as usize).unwrap(),
            Some(at as u64)
        );
    }

    #[test]
    fn goto_and_pattern_parsing() {
        assert_eq!(parse_offset("1234"), Some(1234));
        assert_eq!(parse_offset("0x1F"), Some(31));
        assert_eq!(parse_offset("0X1f"), Some(31));
        assert_eq!(parse_offset("1fh"), Some(31));
        assert_eq!(parse_offset("$ff"), Some(255));
        assert_eq!(parse_offset("ff"), Some(255));
        assert_eq!(parse_offset(" 1_000 "), Some(1000));
        assert_eq!(parse_offset(""), None);
        assert_eq!(parse_offset("xyz"), None);
        assert_eq!(
            parse_pattern("de ad be ef", FindMode::Auto).unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            parse_pattern("0xdeadbeef", FindMode::Auto).unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            parse_pattern("cafe", FindMode::Auto).unwrap(),
            b"cafe".to_vec()
        );
        assert_eq!(
            parse_pattern("cafe", FindMode::Hex).unwrap(),
            vec![0xca, 0xfe]
        );
        assert_eq!(
            parse_pattern("de ad", FindMode::Text).unwrap(),
            b"de ad".to_vec()
        );
        assert!(parse_pattern("abc", FindMode::Hex).is_err());
        assert!(parse_pattern("", FindMode::Auto).is_err());
    }

    #[tokio::test]
    async fn open_reports_length() {
        let t = Tmp::new("open");
        let p = pattern_file(&t, 4321);
        let o = open(p).await.unwrap();
        assert_eq!(o.len, 4321);
        assert!(open(t.0.clone()).await.is_err());
    }

    #[test]
    fn lru_evicts_oldest() {
        let mut l = Lru::new(2);
        let c = || Chunk(Arc::new(vec![1]));
        l.insert(1, c());
        l.insert(2, c());
        l.touch(1);
        l.insert(3, c());
        assert!(l.get(1).is_some() && l.get(3).is_some() && l.get(2).is_none());
    }

    #[test]
    fn view_builds_and_goto() {
        let t = Tmp::new("view");
        let p = pattern_file(&t, 1000);
        let (mut s, _) = State::new(Opened { path: p, len: 1000 });
        let _ = s.update(Msg::GotoInput("0x40".into()));
        let _ = s.update(Msg::Goto);
        assert_eq!(s.sel, Some(0x40));
        let _ = s.update(Msg::FindInput("x".into()));
        let _ = s.update(Msg::CycleMode); // Hex: "x" isn't hex bytes
        let _ = s.update(Msg::FindNext);
        assert!(s.searching.is_none());
        let _ = s.update(Msg::CycleMode); // Text
        let _ = s.update(Msg::FindNext);
        assert!(s.searching.is_some());
        let _ = s.update(Msg::CancelFind);
        let _ = s.update(Msg::Found(Ok(Some((5, true)))));
        assert_eq!(s.sel, Some(5));
        let _ = s.view();
        assert_eq!((s.is_dirty(), s.is_editing()), (false, None));
        let _ = s.toggle_edit();
        let _ = s.save();
        assert!(supported(Path::new("/nonexistent/file.bin")));
    }
}
