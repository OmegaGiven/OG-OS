//! libmpv backend: the client API is loaded at runtime (`dlopen`
//! "libmpv.so.2", then "libmpv.so") through a small hand-written FFI, so the
//! app builds and runs without mpv installed (the viewer then falls back to
//! the FFmpeg player).
//!
//! A `Player` owns one mpv instance and two threads:
//! * the **event thread** blocks in `mpv_wait_event`, turns property changes
//!   and file events into `UiEv`s for the UI (time-pos throttled to 10 Hz);
//! * the **render thread** is woken by mpv's render-update callback (or a
//!   widget resize), renders with the SW render API into a 64-byte aligned
//!   RGBA buffer sized to the on-screen video rect, and publishes it by
//!   swapping buffers with the shared `Frame` slot (no copy) and bumping a
//!   serial. The shader widget uploads the slot to its texture.
//!
//! Constants and struct layouts follow mpv's client.h / render.h (client API
//! 2.x, mpv 0.41).

use libloading::Library;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tmpsc;

// ───────────────────────────────────────────────────────────── client.h

pub const FORMAT_NONE: c_int = 0;
pub const FORMAT_STRING: c_int = 1;
pub const FORMAT_FLAG: c_int = 3;
pub const FORMAT_INT64: c_int = 4;
pub const FORMAT_DOUBLE: c_int = 5;
pub const FORMAT_NODE: c_int = 6;
pub const FORMAT_NODE_ARRAY: c_int = 7;
pub const FORMAT_NODE_MAP: c_int = 8;

pub const EVENT_NONE: c_int = 0;
pub const EVENT_SHUTDOWN: c_int = 1;
pub const EVENT_START_FILE: c_int = 6;
pub const EVENT_END_FILE: c_int = 7;
pub const EVENT_FILE_LOADED: c_int = 8;
pub const EVENT_PROPERTY_CHANGE: c_int = 22;

/// mpv_end_file_reason
pub const END_FILE_EOF: c_int = 0;
pub const END_FILE_ERROR: c_int = 4;

// ───────────────────────────────────────────────────────────── render.h

const RENDER_PARAM_INVALID: c_int = 0;
const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_SW_SIZE: c_int = 17;
const RENDER_PARAM_SW_FORMAT: c_int = 18;
const RENDER_PARAM_SW_STRIDE: c_int = 19;
const RENDER_PARAM_SW_POINTER: c_int = 20;
const RENDER_UPDATE_FRAME: u64 = 1;

#[repr(C)]
struct Handle {
    _p: [u8; 0],
}
#[repr(C)]
struct RenderCtx {
    _p: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
union RawNodeU {
    string: *mut c_char,
    flag: c_int,
    int64: i64,
    double_: f64,
    list: *mut RawNodeList,
    ba: *mut c_void,
}

/// `mpv_node`
#[repr(C)]
struct RawNode {
    u: RawNodeU,
    format: c_int,
}

/// `mpv_node_list`
#[repr(C)]
struct RawNodeList {
    num: c_int,
    values: *mut RawNode,
    keys: *mut *mut c_char,
}

/// `mpv_event`
#[repr(C)]
struct RawEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

/// `mpv_event_property`
#[repr(C)]
struct RawEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

/// `mpv_event_end_file` (leading fields)
#[repr(C)]
struct RawEndFile {
    reason: c_int,
    error: c_int,
}

/// `mpv_render_param`
#[repr(C)]
struct RawRenderParam {
    ty: c_int,
    data: *mut c_void,
}

type UpdateCb = unsafe extern "C" fn(*mut c_void);

/// The resolved libmpv entry points (the library stays loaded for the
/// process lifetime).
pub struct Api {
    _lib: Library,
    client_api_version: unsafe extern "C" fn() -> std::ffi::c_ulong,
    create: unsafe extern "C" fn() -> *mut Handle,
    initialize: unsafe extern "C" fn(*mut Handle) -> c_int,
    set_option_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    set_property: unsafe extern "C" fn(*mut Handle, *const c_char, c_int, *mut c_void) -> c_int,
    set_property_string: unsafe extern "C" fn(*mut Handle, *const c_char, *const c_char) -> c_int,
    get_property: unsafe extern "C" fn(*mut Handle, *const c_char, c_int, *mut c_void) -> c_int,
    command: unsafe extern "C" fn(*mut Handle, *mut *const c_char) -> c_int,
    command_async: unsafe extern "C" fn(*mut Handle, u64, *mut *const c_char) -> c_int,
    command_ret: unsafe extern "C" fn(*mut Handle, *mut *const c_char, *mut RawNode) -> c_int,
    observe_property: unsafe extern "C" fn(*mut Handle, u64, *const c_char, c_int) -> c_int,
    wait_event: unsafe extern "C" fn(*mut Handle, f64) -> *mut RawEvent,
    wakeup: unsafe extern "C" fn(*mut Handle),
    terminate_destroy: unsafe extern "C" fn(*mut Handle),
    free: unsafe extern "C" fn(*mut c_void),
    free_node_contents: unsafe extern "C" fn(*mut RawNode),
    error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    render_create:
        unsafe extern "C" fn(*mut *mut RenderCtx, *mut Handle, *mut RawRenderParam) -> c_int,
    render_set_update_callback: unsafe extern "C" fn(*mut RenderCtx, Option<UpdateCb>, *mut c_void),
    render_update: unsafe extern "C" fn(*mut RenderCtx) -> u64,
    render_render: unsafe extern "C" fn(*mut RenderCtx, *mut RawRenderParam) -> c_int,
    render_free: unsafe extern "C" fn(*mut RenderCtx),
}

/// Library names tried in order.
pub const LIB_NAMES: [&str; 2] = ["libmpv.so.2", "libmpv.so"];

impl Api {
    fn load() -> Result<Api, String> {
        let mut last = String::from("libmpv not found");
        for name in LIB_NAMES {
            // SAFETY: loading libmpv runs only its (trusted) initialisers
            match unsafe { Library::new(name) } {
                Ok(lib) => return Self::resolve(lib),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    fn resolve(lib: Library) -> Result<Api, String> {
        macro_rules! sym {
            ($n:literal) => {
                // SAFETY: the symbol's C signature matches the field type
                // (client.h / render.h)
                *unsafe { lib.get(concat!($n, "\0").as_bytes()) }
                    .map_err(|e| format!("{}: {e}", $n))?
            };
        }
        let api = Api {
            client_api_version: sym!("mpv_client_api_version"),
            create: sym!("mpv_create"),
            initialize: sym!("mpv_initialize"),
            set_option_string: sym!("mpv_set_option_string"),
            set_property: sym!("mpv_set_property"),
            set_property_string: sym!("mpv_set_property_string"),
            get_property: sym!("mpv_get_property"),
            command: sym!("mpv_command"),
            command_async: sym!("mpv_command_async"),
            command_ret: sym!("mpv_command_ret"),
            observe_property: sym!("mpv_observe_property"),
            wait_event: sym!("mpv_wait_event"),
            wakeup: sym!("mpv_wakeup"),
            terminate_destroy: sym!("mpv_terminate_destroy"),
            free: sym!("mpv_free"),
            free_node_contents: sym!("mpv_free_node_contents"),
            error_string: sym!("mpv_error_string"),
            render_create: sym!("mpv_render_context_create"),
            render_set_update_callback: sym!("mpv_render_context_set_update_callback"),
            render_update: sym!("mpv_render_context_update"),
            render_render: sym!("mpv_render_context_render"),
            render_free: sym!("mpv_render_context_free"),
            _lib: lib,
        };
        // SAFETY: no arguments, returns a plain integer
        let v = unsafe { (api.client_api_version)() } as u64;
        if v >> 16 != 2 {
            return Err(format!("unsupported libmpv client API {}.{}", v >> 16, v & 0xffff));
        }
        Ok(api)
    }

    /// Client API version as (major, minor).
    #[cfg(test)]
    pub fn version(&self) -> (u32, u32) {
        // SAFETY: as above
        let v = unsafe { (self.client_api_version)() } as u64;
        ((v >> 16) as u32, (v & 0xffff) as u32)
    }

    fn err(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string for any code
        unsafe { CStr::from_ptr((self.error_string)(code)) }
            .to_string_lossy()
            .into_owned()
    }
}

/// The process-wide libmpv, loaded on first use.
pub fn api() -> Result<&'static Api, &'static str> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(Api::load).as_ref().map_err(|e| e.as_str())
}

// ───────────────────────────────────────────────────────────────── nodes

/// An owned copy of an `mpv_node`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Node {
    #[default]
    None,
    Str(String),
    Flag(bool),
    Int(i64),
    Double(f64),
    Array(Vec<Node>),
    Map(Vec<(String, Node)>),
}

impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Node::Map(m) => m.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<&str> {
        match self {
            Node::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn f64(&self) -> Option<f64> {
        match self {
            Node::Double(d) => Some(*d),
            Node::Int(i) => Some(*i as f64),
            Node::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn i64(&self) -> Option<i64> {
        match self {
            Node::Int(i) => Some(*i),
            Node::Double(d) => Some(*d as i64),
            Node::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn flag(&self) -> Option<bool> {
        match self {
            Node::Flag(b) => Some(*b),
            Node::Str(s) => match s.as_str() {
                "yes" => Some(true),
                "no" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
    pub fn array(&self) -> &[Node] {
        match self {
            Node::Array(a) => a,
            _ => &[],
        }
    }

    /// SAFETY: `n` points to a valid mpv_node (owned by mpv).
    unsafe fn from_raw(n: *const RawNode) -> Node {
        if n.is_null() {
            return Node::None;
        }
        let n = &*n;
        let cstr = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        match n.format {
            FORMAT_STRING => Node::Str(cstr(n.u.string)),
            FORMAT_FLAG => Node::Flag(n.u.flag != 0),
            FORMAT_INT64 => Node::Int(n.u.int64),
            FORMAT_DOUBLE => Node::Double(n.u.double_),
            FORMAT_NODE_ARRAY | FORMAT_NODE_MAP => {
                let l = n.u.list;
                if l.is_null() {
                    return Node::None;
                }
                let l = &*l;
                let items = (0..l.num.max(0) as usize).map(|i| Node::from_raw(l.values.add(i)));
                if n.format == FORMAT_NODE_ARRAY {
                    Node::Array(items.collect())
                } else {
                    let keys = (0..l.num.max(0) as usize).map(|i| cstr(*l.keys.add(i)));
                    Node::Map(keys.zip(items).collect())
                }
            }
            _ => Node::None,
        }
    }
}

// ───────────────────────────────────────────────────────────────── handle

/// One mpv instance. The client API is thread-safe; the handle is destroyed
/// (`mpv_terminate_destroy`) when the last `Arc<Mpv>` goes — after the render
/// thread freed its render context, as mpv requires.
pub struct Mpv {
    api: &'static Api,
    h: *mut Handle,
}

// SAFETY: libmpv's client API may be used from any thread
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: last user of the handle
        unsafe { (self.api.terminate_destroy)(self.h) }
    }
}

fn cs(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

/// Events the player hands to its owner.
#[derive(Debug)]
pub enum Event {
    None,
    Shutdown,
    Prop(u64, Node),
    StartFile,
    FileLoaded,
    EndFile { reason: c_int, error: Option<String> },
    Other,
}

impl Mpv {
    /// mpv_create + options (before init) + mpv_initialize.
    pub fn new(options: &[(String, String)]) -> Result<Mpv, String> {
        let api = api().map_err(str::to_string)?;
        // SAFETY: plain constructor
        let h = unsafe { (api.create)() };
        if h.is_null() {
            return Err("mpv_create failed".into());
        }
        let m = Mpv { api, h };
        for (k, v) in options {
            if let Err(e) = m.set_option(k, v) {
                // unknown options (older/newer mpv) are not fatal
                eprintln!("og-files: mpv option {k}={v}: {e}");
            }
        }
        // SAFETY: valid handle, not initialised yet
        let r = unsafe { (api.initialize)(h) };
        if r < 0 {
            return Err(format!("mpv_initialize: {}", api.err(r)));
        }
        Ok(m)
    }

    fn check(&self, r: c_int) -> Result<(), String> {
        if r < 0 {
            Err(self.api.err(r))
        } else {
            Ok(())
        }
    }

    pub fn set_option(&self, k: &str, v: &str) -> Result<(), String> {
        let (k, v) = (cs(k), cs(v));
        // SAFETY: valid handle and NUL-terminated strings
        self.check(unsafe { (self.api.set_option_string)(self.h, k.as_ptr(), v.as_ptr()) })
    }

    pub fn set_str(&self, k: &str, v: &str) -> Result<(), String> {
        let (k, v) = (cs(k), cs(v));
        // SAFETY: as above
        self.check(unsafe { (self.api.set_property_string)(self.h, k.as_ptr(), v.as_ptr()) })
    }

    pub fn set_flag(&self, k: &str, on: bool) -> Result<(), String> {
        let k = cs(k);
        let mut v: c_int = on.into();
        // SAFETY: FORMAT_FLAG takes an int*
        self.check(unsafe {
            (self.api.set_property)(self.h, k.as_ptr(), FORMAT_FLAG, &mut v as *mut c_int as _)
        })
    }

    #[cfg(test)]
    pub fn set_f64(&self, k: &str, v: f64) -> Result<(), String> {
        let k = cs(k);
        let mut v = v;
        // SAFETY: FORMAT_DOUBLE takes a double*
        self.check(unsafe {
            (self.api.set_property)(self.h, k.as_ptr(), FORMAT_DOUBLE, &mut v as *mut f64 as _)
        })
    }

    pub fn get_f64(&self, k: &str) -> Option<f64> {
        let k = cs(k);
        let mut v = 0f64;
        // SAFETY: FORMAT_DOUBLE writes a double
        let r = unsafe {
            (self.api.get_property)(self.h, k.as_ptr(), FORMAT_DOUBLE, &mut v as *mut f64 as _)
        };
        (r >= 0).then_some(v)
    }

    pub fn get_str(&self, k: &str) -> Option<String> {
        let k = cs(k);
        let mut p: *mut c_char = std::ptr::null_mut();
        // SAFETY: FORMAT_STRING writes a char* we must mpv_free
        let r = unsafe {
            (self.api.get_property)(self.h, k.as_ptr(), FORMAT_STRING, &mut p as *mut _ as _)
        };
        if r < 0 || p.is_null() {
            return None;
        }
        // SAFETY: valid C string from mpv, freed right after
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { (self.api.free)(p as _) };
        Some(s)
    }

    #[cfg(test)]
    pub fn get_node(&self, k: &str) -> Option<Node> {
        let k = cs(k);
        let mut n = RawNode {
            u: RawNodeU { int64: 0 },
            format: FORMAT_NONE,
        };
        // SAFETY: FORMAT_NODE fills an mpv_node we free with
        // mpv_free_node_contents
        let r = unsafe {
            (self.api.get_property)(self.h, k.as_ptr(), FORMAT_NODE, &mut n as *mut _ as _)
        };
        if r < 0 {
            return None;
        }
        let out = unsafe { Node::from_raw(&n) };
        unsafe { (self.api.free_node_contents)(&mut n) };
        Some(out)
    }

    fn argv(args: &[&str]) -> (Vec<CString>, Vec<*const c_char>) {
        let owned: Vec<CString> = args.iter().map(|a| cs(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        (owned, ptrs)
    }

    /// Blocking command.
    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let (_own, mut ptrs) = Self::argv(args);
        // SAFETY: NULL-terminated argv of valid strings
        self.check(unsafe { (self.api.command)(self.h, ptrs.as_mut_ptr()) })
    }

    /// Fire-and-forget command (mpv copies the arguments); the reply comes
    /// back as an `MPV_EVENT_COMMAND_REPLY` carrying `id` (ignored here).
    pub fn command_async(&self, id: u64, args: &[&str]) {
        let (_own, mut ptrs) = Self::argv(args);
        // SAFETY: as above
        unsafe { (self.api.command_async)(self.h, id, ptrs.as_mut_ptr()) };
    }

    /// Blocking command returning its result node.
    pub fn command_ret(&self, args: &[&str]) -> Result<Node, String> {
        let (_own, mut ptrs) = Self::argv(args);
        let mut n = RawNode {
            u: RawNodeU { int64: 0 },
            format: FORMAT_NONE,
        };
        // SAFETY: as above; the result node is freed after copying
        let r = unsafe { (self.api.command_ret)(self.h, ptrs.as_mut_ptr(), &mut n) };
        self.check(r)?;
        let out = unsafe { Node::from_raw(&n) };
        unsafe { (self.api.free_node_contents)(&mut n) };
        Ok(out)
    }

    pub fn observe(&self, id: u64, name: &str) {
        let k = cs(name);
        // SAFETY: valid handle and string
        unsafe { (self.api.observe_property)(self.h, id, k.as_ptr(), FORMAT_NODE) };
    }

    pub fn wakeup(&self) {
        // SAFETY: valid handle; callable from any thread
        unsafe { (self.api.wakeup)(self.h) }
    }

    /// `mpv_wait_event`; only ever called from one thread at a time.
    pub fn wait_event(&self, timeout: f64) -> Event {
        // SAFETY: the returned event stays valid until the next call
        let e = unsafe { &*(self.api.wait_event)(self.h, timeout) };
        match e.event_id {
            EVENT_NONE => Event::None,
            EVENT_SHUTDOWN => Event::Shutdown,
            EVENT_START_FILE => Event::StartFile,
            EVENT_FILE_LOADED => Event::FileLoaded,
            EVENT_END_FILE if !e.data.is_null() => {
                // SAFETY: END_FILE carries an mpv_event_end_file
                let d = unsafe { &*(e.data as *const RawEndFile) };
                Event::EndFile {
                    reason: d.reason,
                    error: (d.reason == END_FILE_ERROR).then(|| self.api.err(d.error)),
                }
            }
            EVENT_PROPERTY_CHANGE if !e.data.is_null() => {
                // SAFETY: PROPERTY_CHANGE carries an mpv_event_property
                let p = unsafe { &*(e.data as *const RawEventProperty) };
                let v = if p.format == FORMAT_NODE {
                    unsafe { Node::from_raw(p.data as *const RawNode) }
                } else {
                    Node::None
                };
                Event::Prop(e.reply_userdata, v)
            }
            _ => Event::Other,
        }
    }
}

// ───────────────────────────────────────────────────────────── rendering

/// Largest render target (physical pixels).
pub const MAX_RENDER: (u32, u32) = (3840, 2160);

/// Fit a video of display aspect `aspect` (w/h) inside `widget` (physical
/// pixels) — the letterboxed video rect — then cap at `MAX_RENDER` keeping
/// the aspect. No aspect known: the whole widget (capped).
pub fn target_size(widget: (u32, u32), aspect: Option<f64>) -> (u32, u32) {
    let (ww, wh) = (widget.0 as f64, widget.1 as f64);
    if ww < 1.0 || wh < 1.0 {
        return (0, 0);
    }
    let (mut w, mut h) = match aspect.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) if ww / wh > a => (wh * a, wh),
        Some(a) => (ww, ww / a),
        None => (ww, wh),
    };
    let s = (MAX_RENDER.0 as f64 / w).min(MAX_RENDER.1 as f64 / h).min(1.0);
    w *= s;
    h *= s;
    ((w.round() as u32).max(1), (h.round() as u32).max(1))
}

/// Bytes per row: RGBA rounded up to 256 (mpv wants multiples of 64 for
/// its SIMD paths; wgpu copies like 256).
pub fn stride_for(w: u32) -> usize {
    (w as usize * 4).div_ceil(256) * 256
}

#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct Block([u8; 64]);

/// A byte buffer whose start is 64-byte aligned.
#[derive(Default)]
pub struct AlignedBuf(Vec<Block>);

impl AlignedBuf {
    fn resize(&mut self, len: usize) {
        self.0.resize(len.div_ceil(64), Block([0; 64]));
    }
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: Block is plain bytes; length in bytes = blocks × 64
        unsafe { std::slice::from_raw_parts(self.0.as_ptr() as *const u8, self.0.len() * 64) }
    }
    fn ptr(&mut self) -> *mut c_void {
        self.0.as_mut_ptr() as _
    }
}

/// A rendered picture: `h` rows of `stride` bytes, RGB0 (alpha garbage).
#[derive(Default)]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub stride: usize,
    pub buf: AlignedBuf,
}

/// What the render thread waits for.
#[derive(Default)]
struct Wake {
    update: bool,
    resize: bool,
    quit: bool,
}

/// State shared by the UI, the shader primitive and the mpv threads.
pub struct Shared {
    /// latest frame (the shader uploads it when `serial` moved)
    pub frame: Mutex<Frame>,
    pub serial: AtomicU64,
    /// on-screen widget size in physical pixels (set by the shader)
    widget: Mutex<(u32, u32)>,
    /// display aspect of the video (after rotation / override)
    aspect: Mutex<Option<f64>>,
    wake: Mutex<Wake>,
    cv: Condvar,
    /// mirror of mpv's `pause`; frames rendered while paused get a UI
    /// message (while playing the shader redraws every vsync anyway)
    pub paused: AtomicBool,
    frame_msg_pending: AtomicBool,
    tx: Mutex<Option<tmpsc::UnboundedSender<UiEv>>>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            frame: Mutex::new(Frame::default()),
            serial: AtomicU64::new(0),
            widget: Mutex::new((0, 0)),
            aspect: Mutex::new(None),
            wake: Mutex::new(Wake::default()),
            cv: Condvar::new(),
            paused: AtomicBool::new(true),
            frame_msg_pending: AtomicBool::new(false),
            tx: Mutex::new(None),
        }
    }

    fn poke(&self, f: impl FnOnce(&mut Wake)) {
        if let Ok(mut w) = self.wake.lock() {
            f(&mut w);
            self.cv.notify_one();
        }
    }

    /// The widget's physical size (from the shader's `prepare`).
    pub fn set_widget_size(&self, size: (u32, u32)) {
        let changed = self
            .widget
            .lock()
            .map(|mut w| std::mem::replace(&mut *w, size) != size)
            .unwrap_or(false);
        if changed {
            self.poke(|w| w.resize = true);
        }
    }

    fn set_aspect(&self, a: Option<f64>) {
        let changed = self
            .aspect
            .lock()
            .map(|mut x| std::mem::replace(&mut *x, a) != a)
            .unwrap_or(false);
        if changed {
            self.poke(|w| w.resize = true);
        }
    }

    /// The UI handled the last `UiEv::Frame`.
    pub fn frame_seen(&self) {
        self.frame_msg_pending.store(false, Ordering::Relaxed);
    }

    fn send(&self, ev: UiEv) {
        if let Some(tx) = self.tx.lock().ok().and_then(|t| t.clone()) {
            let _ = tx.send(ev);
        }
    }
}

unsafe extern "C" fn on_render_update(ctx: *mut c_void) {
    // SAFETY: ctx is the Shared kept alive by the render thread until the
    // render context (and with it this callback) is freed
    let s = unsafe { &*(ctx as *const Shared) };
    s.poke(|w| w.update = true);
}

struct RenderCtxPtr(*mut RenderCtx);
// SAFETY: the render context is only used by the render thread
unsafe impl Send for RenderCtxPtr {}

fn render_loop(mpv: Arc<Mpv>, shared: Arc<Shared>, ctx: RenderCtxPtr) {
    let api = mpv.api;
    let ctx = ctx.0;
    let fmt = cs("rgb0");
    let mut back = Frame::default();
    loop {
        let (update, resize) = {
            let Ok(mut w) = shared.wake.lock() else { break };
            while !(w.update || w.resize || w.quit) {
                w = match shared.cv.wait(w) {
                    Ok(w) => w,
                    Err(_) => return,
                };
            }
            if w.quit {
                break;
            }
            (std::mem::take(&mut w.update), std::mem::take(&mut w.resize))
        };
        // SAFETY: render context owned by this thread
        let flags = if update {
            unsafe { (api.render_update)(ctx) }
        } else {
            0
        };
        if flags & RENDER_UPDATE_FRAME == 0 && !resize {
            continue;
        }
        let widget = shared.widget.lock().map(|w| *w).unwrap_or((0, 0));
        let aspect = shared.aspect.lock().ok().and_then(|a| *a);
        let (mut w, mut h) = target_size(widget, aspect);
        if w == 0 || h == 0 {
            // nothing on screen yet: still let mpv consume the frame
            if flags & RENDER_UPDATE_FRAME != 0 {
                (w, h) = (16, 16);
            } else {
                continue;
            }
        }
        let stride = stride_for(w);
        back.buf.resize(stride * h as usize);
        back.w = w;
        back.h = h;
        back.stride = stride;
        let mut size: [c_int; 2] = [w as c_int, h as c_int];
        let mut stride_v: usize = stride;
        let mut params = [
            RawRenderParam {
                ty: RENDER_PARAM_SW_SIZE,
                data: size.as_mut_ptr() as _,
            },
            RawRenderParam {
                ty: RENDER_PARAM_SW_FORMAT,
                data: fmt.as_ptr() as _,
            },
            RawRenderParam {
                ty: RENDER_PARAM_SW_STRIDE,
                data: &mut stride_v as *mut usize as _,
            },
            RawRenderParam {
                ty: RENDER_PARAM_SW_POINTER,
                data: back.buf.ptr(),
            },
            RawRenderParam {
                ty: RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: buffer holds stride × h bytes, 64-aligned; params
        // terminated by INVALID
        let r = unsafe { (api.render_render)(ctx, params.as_mut_ptr()) };
        if r < 0 || widget.0 == 0 {
            continue;
        }
        if let Ok(mut f) = shared.frame.lock() {
            std::mem::swap(&mut *f, &mut back);
        }
        shared.serial.fetch_add(1, Ordering::Release);
        if shared.paused.load(Ordering::Relaxed)
            && !shared.frame_msg_pending.swap(true, Ordering::Relaxed)
        {
            shared.send(UiEv::Frame);
        }
    }
    // SAFETY: free before the handle can be destroyed (we still hold an Arc)
    unsafe { (api.render_free)(ctx) };
}

// ───────────────────────────────────────────────────────────────── player

/// Properties observed for the UI (reply id = index).
pub const PROPS: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "eof-reached",
    "volume",
    "mute",
    "speed",
    "track-list",
    "chapter-list",
    "chapter",
    "hwdec-current",
    "video-params",
    "container-fps",
    "estimated-vf-fps",
    "frame-drop-count",
    "decoder-frame-drop-count",
    "sub-delay",
    "audio-delay",
    "ab-loop-a",
    "ab-loop-b",
    "loop-file",
    "sub-visibility",
    "video-dec-params",
    "video-zoom",
    "demuxer-cache-time",
    "video-out-params",
];

/// What the mpv threads tell the UI.
#[derive(Debug, Clone)]
pub enum UiEv {
    Prop(&'static str, Node),
    FileLoaded,
    /// playback of a file ended; Some(error) if it failed to play
    EndFile(Option<String>),
    /// a frame was rendered while paused (seek, frame step, resize)
    Frame,
    Shutdown,
}

/// Display aspect from a `video-out-params` node (dw/dh already include
/// rotation and the aspect override).
pub fn out_aspect(n: &Node) -> Option<f64> {
    let dw = n.get("dw")?.f64()?;
    let dh = n.get("dh")?.f64()?;
    (dw > 0.0 && dh > 0.0).then(|| dw / dh)
}

fn event_loop(mpv: Arc<Mpv>, shared: Arc<Shared>, quit: Arc<AtomicBool>) {
    let mut last_time = Instant::now() - Duration::from_secs(1);
    let mut held_time: Option<Node> = None;
    loop {
        // wake up at least every 100 ms to flush a throttled time-pos
        let ev = mpv.wait_event(if held_time.is_some() { 0.1 } else { -1.0 });
        if quit.load(Ordering::Relaxed) {
            break;
        }
        if let Some(t) = held_time.take() {
            if last_time.elapsed() >= Duration::from_millis(100) {
                last_time = Instant::now();
                shared.send(UiEv::Prop("time-pos", t));
            } else {
                held_time = Some(t);
            }
        }
        match ev {
            Event::Shutdown => {
                shared.send(UiEv::Shutdown);
                break;
            }
            Event::Prop(id, v) => {
                let Some(&name) = PROPS.get(id as usize) else {
                    continue;
                };
                match name {
                    "time-pos" => {
                        if last_time.elapsed() >= Duration::from_millis(100) {
                            last_time = Instant::now();
                            held_time = None;
                            shared.send(UiEv::Prop(name, v));
                        } else {
                            held_time = Some(v);
                        }
                        continue;
                    }
                    "pause" => {
                        shared
                            .paused
                            .store(v.flag().unwrap_or(true), Ordering::Relaxed);
                    }
                    "video-out-params" => shared.set_aspect(out_aspect(&v)),
                    _ => {}
                }
                shared.send(UiEv::Prop(name, v));
            }
            Event::FileLoaded => shared.send(UiEv::FileLoaded),
            Event::EndFile { reason, error } => {
                if reason == END_FILE_EOF || error.is_some() {
                    shared.send(UiEv::EndFile(error));
                }
            }
            _ => {}
        }
    }
}

/// A running mpv instance with its event and render threads. Dropping it
/// stops playback; mpv is destroyed in the background once both threads
/// have let go.
pub struct Player {
    pub mpv: Arc<Mpv>,
    pub shared: Arc<Shared>,
    quit: Arc<AtomicBool>,
}

impl Player {
    /// Create mpv with `options`, set up the SW render context and the
    /// threads. Events go to `tx`. Nothing is loaded yet.
    pub fn new(
        options: &[(String, String)],
        tx: Option<tmpsc::UnboundedSender<UiEv>>,
    ) -> Result<Player, String> {
        let mpv = Arc::new(Mpv::new(options)?);
        let shared = Arc::new(Shared::new());
        if let Ok(mut t) = shared.tx.lock() {
            *t = tx;
        }
        let api = mpv.api;
        let sw = cs("sw");
        let mut params = [
            RawRenderParam {
                ty: RENDER_PARAM_API_TYPE,
                data: sw.as_ptr() as _,
            },
            RawRenderParam {
                ty: RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        let mut ctx: *mut RenderCtx = std::ptr::null_mut();
        // SAFETY: valid handle; params terminated by INVALID
        let r = unsafe { (api.render_create)(&mut ctx, mpv.h, params.as_mut_ptr()) };
        if r < 0 || ctx.is_null() {
            return Err(format!("mpv render context: {}", api.err(r)));
        }
        // SAFETY: the Shared outlives the context (render thread holds it)
        unsafe {
            (api.render_set_update_callback)(
                ctx,
                Some(on_render_update),
                Arc::as_ptr(&shared) as *mut c_void,
            )
        };
        for (i, p) in PROPS.iter().enumerate() {
            mpv.observe(i as u64, p);
        }
        let quit = Arc::new(AtomicBool::new(false));
        let ctx = RenderCtxPtr(ctx);
        let (m, s) = (mpv.clone(), shared.clone());
        std::thread::Builder::new()
            .name("og-files-mpv-render".into())
            .spawn(move || render_loop(m, s, ctx))
            .map_err(|e| e.to_string())?;
        let (m, s, q) = (mpv.clone(), shared.clone(), quit.clone());
        std::thread::Builder::new()
            .name("og-files-mpv-events".into())
            .spawn(move || event_loop(m, s, q))
            .map_err(|e| e.to_string())?;
        Ok(Player { mpv, shared, quit })
    }

    /// Async command; errors only show up as replies we don't wait for.
    pub fn cmd(&self, args: &[&str]) {
        self.mpv.command_async(0, args);
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // silence at once, then unload
        let _ = self.mpv.set_flag("pause", true);
        self.mpv.command_async(0, &["stop"]);
        self.quit.store(true, Ordering::Relaxed);
        if let Ok(mut t) = self.shared.tx.lock() {
            *t = None;
        }
        self.shared.poke(|w| w.quit = true);
        self.mpv.wakeup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_mpv_target_size_letterbox() {
        // wide widget, 16:9 video → pillarbox
        assert_eq!(target_size((2000, 900), Some(16.0 / 9.0)), (1600, 900));
        // tall widget → letterbox
        assert_eq!(target_size((800, 900), Some(16.0 / 9.0)), (800, 450));
        // unknown aspect → whole widget
        assert_eq!(target_size((640, 480), None), (640, 480));
        assert_eq!(target_size((640, 480), Some(f64::NAN)), (640, 480));
        // capped at 4K, aspect kept
        assert_eq!(target_size((7680, 4320), Some(16.0 / 9.0)), (3840, 2160));
        assert_eq!(target_size((8000, 2000), None), (3840, 960));
        assert_eq!(target_size((0, 100), Some(1.0)), (0, 0));
        assert_eq!(stride_for(1), 256);
        assert_eq!(stride_for(64), 256);
        assert_eq!(stride_for(65), 512);
        assert_eq!(stride_for(1920), 7680);
        let mut b = AlignedBuf::default();
        b.resize(1000);
        assert_eq!(b.bytes().as_ptr() as usize % 64, 0);
        assert!(b.bytes().len() >= 1000);
    }

    #[test]
    fn video_mpv_node_helpers() {
        let n = Node::Map(vec![
            ("dw".into(), Node::Int(720)),
            ("dh".into(), Node::Int(360)),
            ("s".into(), Node::Str("2.5".into())),
            ("y".into(), Node::Str("yes".into())),
        ]);
        assert_eq!(out_aspect(&n), Some(2.0));
        assert_eq!(n.get("s").and_then(Node::f64), Some(2.5));
        assert_eq!(n.get("y").and_then(Node::flag), Some(true));
        assert_eq!(n.get("dw").and_then(Node::i64), Some(720));
        assert!(n.get("nope").is_none());
        assert!(Node::None.array().is_empty());
        assert_eq!(out_aspect(&Node::None), None);
    }

    /// Converting a real mpv_node built by hand.
    #[test]
    fn video_mpv_raw_node_conversion() {
        let mut k = [cs("a"), cs("b")];
        let s = cs("hi");
        let mut vals = [
            RawNode {
                u: RawNodeU { int64: 7 },
                format: FORMAT_INT64,
            },
            RawNode {
                u: RawNodeU {
                    string: s.as_ptr() as *mut c_char,
                },
                format: FORMAT_STRING,
            },
        ];
        let mut keys: Vec<*mut c_char> = k.iter_mut().map(|c| c.as_ptr() as *mut c_char).collect();
        let mut list = RawNodeList {
            num: 2,
            values: vals.as_mut_ptr(),
            keys: keys.as_mut_ptr(),
        };
        let map = RawNode {
            u: RawNodeU { list: &mut list },
            format: FORMAT_NODE_MAP,
        };
        // SAFETY: well-formed node tree on the stack
        let n = unsafe { Node::from_raw(&map) };
        assert_eq!(
            n,
            Node::Map(vec![
                ("a".into(), Node::Int(7)),
                ("b".into(), Node::Str("hi".into()))
            ])
        );
        list.keys = std::ptr::null_mut();
        let arr = RawNode {
            u: RawNodeU { list: &mut list },
            format: FORMAT_NODE_ARRAY,
        };
        let n = unsafe { Node::from_raw(&arr) };
        assert_eq!(n.array().len(), 2);
    }

    /// dlopen + symbol resolution; skipped when libmpv isn't installed.
    #[test]
    fn video_mpv_ffi_loads() {
        match api() {
            Ok(a) => {
                let (maj, min) = a.version();
                assert_eq!(maj, 2, "client API {maj}.{min}");
                let m = Mpv::new(&[("vo".into(), "null".into()), ("ao".into(), "null".into())])
                    .expect("mpv instance");
                m.set_str("volume", "42").unwrap();
                assert_eq!(m.get_f64("volume"), Some(42.0));
                m.set_flag("mute", true).unwrap();
                assert_eq!(m.get_str("mute").as_deref(), Some("yes"));
                m.set_f64("speed", 1.5).unwrap();
                assert_eq!(m.get_node("speed"), Some(Node::Double(1.5)));
                assert!(m.set_str("no-such-property", "1").is_err());
                assert!(m.command(&["no-such-command"]).is_err());
                let v = m.command_ret(&["expand-text", "${volume}"]).unwrap();
                assert_eq!(v.str().and_then(|s| s.parse::<f64>().ok()), Some(42.0), "{v:?}");
            }
            Err(e) => eprintln!("libmpv not available, skipping: {e}"),
        }
    }
}
