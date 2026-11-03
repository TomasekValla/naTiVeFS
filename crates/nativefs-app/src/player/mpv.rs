//! Minimal libmpv binding, loaded at runtime (no import library, no headers at build time).
//! Only the client API and the OpenGL render API are used. The library ships next to the
//! executable on Windows (`libmpv-2.dll`); on Linux / macOS the system or bundled `libmpv` is used.

use std::ffi::{c_char, c_double, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

use libloading::Library;

type Handle = *mut c_void;

#[repr(C)]
struct RawEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

#[repr(C)]
struct RawEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct RawEventEndFile {
    reason: c_int,
    error: c_int,
}

#[repr(C)]
struct RawLogMessage {
    prefix: *const c_char,
    level: *const c_char,
    text: *const c_char,
}

#[repr(C)]
struct RenderParam {
    kind: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct OpenGlInitParams {
    get_proc_address: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct OpenGlFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

const FORMAT_STRING: c_int = 1;
const FORMAT_FLAG: c_int = 3;
const FORMAT_INT64: c_int = 4;
const FORMAT_DOUBLE: c_int = 5;

const RENDER_PARAM_INVALID: c_int = 0;
const RENDER_PARAM_API_TYPE: c_int = 1;
const RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const RENDER_PARAM_OPENGL_FBO: c_int = 3;
const RENDER_PARAM_FLIP_Y: c_int = 4;
const RENDER_PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
const RENDER_UPDATE_FRAME: u64 = 1;

macro_rules! api {
    ($($name:ident : $sym:literal => fn($($arg:ty),*) $(-> $ret:ty)?;)*) => {
        struct Api {
            $($name: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
            _lib: Library,
        }

        impl Api {
            unsafe fn load(lib: Library) -> Result<Self, String> {
                unsafe {
                    Ok(Self {
                        $($name: *lib.get::<unsafe extern "C" fn($($arg),*) $(-> $ret)?>($sym)
                            .map_err(|e| format!("{}: {e}", String::from_utf8_lossy(&$sym[..$sym.len() - 1])))?,)*
                        _lib: lib,
                    })
                }
            }
        }
    };
}

api! {
    client_api_version: b"mpv_client_api_version\0" => fn() -> std::ffi::c_ulong;
    error_string: b"mpv_error_string\0" => fn(c_int) -> *const c_char;
    free: b"mpv_free\0" => fn(*mut c_void);
    create: b"mpv_create\0" => fn() -> Handle;
    initialize: b"mpv_initialize\0" => fn(Handle) -> c_int;
    terminate_destroy: b"mpv_terminate_destroy\0" => fn(Handle);
    set_option_string: b"mpv_set_option_string\0" => fn(Handle, *const c_char, *const c_char) -> c_int;
    command: b"mpv_command\0" => fn(Handle, *mut *const c_char) -> c_int;
    command_async: b"mpv_command_async\0" => fn(Handle, u64, *mut *const c_char) -> c_int;
    set_property: b"mpv_set_property\0" => fn(Handle, *const c_char, c_int, *mut c_void) -> c_int;
    set_property_string: b"mpv_set_property_string\0" => fn(Handle, *const c_char, *const c_char) -> c_int;
    get_property: b"mpv_get_property\0" => fn(Handle, *const c_char, c_int, *mut c_void) -> c_int;
    get_property_string: b"mpv_get_property_string\0" => fn(Handle, *const c_char) -> *mut c_char;
    observe_property: b"mpv_observe_property\0" => fn(Handle, u64, *const c_char, c_int) -> c_int;
    wait_event: b"mpv_wait_event\0" => fn(Handle, c_double) -> *mut RawEvent;
    set_wakeup_callback: b"mpv_set_wakeup_callback\0" => fn(Handle, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void);
    render_context_create: b"mpv_render_context_create\0" => fn(*mut *mut c_void, Handle, *mut RenderParam) -> c_int;
    render_context_set_update_callback: b"mpv_render_context_set_update_callback\0" => fn(*mut c_void, Option<unsafe extern "C" fn(*mut c_void)>, *mut c_void);
    render_context_update: b"mpv_render_context_update\0" => fn(*mut c_void) -> u64;
    render_context_render: b"mpv_render_context_render\0" => fn(*mut c_void, *mut RenderParam) -> c_int;
    render_context_report_swap: b"mpv_render_context_report_swap\0" => fn(*mut c_void);
    render_context_free: b"mpv_render_context_free\0" => fn(*mut c_void);
}

/// File names tried in order, next to the executable first.
fn candidates() -> Vec<std::path::PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &["libmpv.2.dylib", "libmpv.dylib"]
    } else {
        &["libmpv.so.2", "libmpv.so"]
    };
    let mut out = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())) {
        for n in names {
            out.push(dir.join(n));
            // AppImage / macOS bundle layouts
            out.push(dir.join("lib").join(n));
            out.push(dir.join("../lib").join(n));
            out.push(dir.join("../Frameworks").join(n));
        }
    }
    // the system search path (Linux distributions, Homebrew)
    for n in names {
        out.push(n.into());
    }
    if cfg!(target_os = "macos") {
        for n in names {
            out.push(std::path::Path::new("/opt/homebrew/lib").join(n));
        }
    }
    out
}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

fn api() -> Result<&'static Api, String> {
    API.get_or_init(|| {
        let mut last = String::from("libmpv was not found");
        for path in candidates() {
            if path.is_absolute() && !path.exists() {
                continue;
            }
            // SAFETY: loading libmpv runs its (trusted) initialisers only.
            match unsafe { Library::new(&path) } {
                Ok(lib) => {
                    let api = unsafe { Api::load(lib) }?;
                    let version = unsafe { (api.client_api_version)() } as u64;
                    if version >> 16 < 2 {
                        return Err(format!("libmpv {}.{} is too old (2.0 or newer is needed)", version >> 16, version & 0xffff));
                    }
                    return Ok(api);
                }
                Err(e) => last = format!("{}: {e}", path.display()),
            }
        }
        Err(last)
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// Whether libmpv can be loaded (the error explains why not).
pub fn available() -> Result<(), String> {
    api().map(|_| ())
}

fn cstr(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

fn err_text(api: &Api, code: c_int) -> String {
    // SAFETY: mpv_error_string returns a static string for any code.
    unsafe { CStr::from_ptr((api.error_string)(code)) }.to_string_lossy().into_owned()
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Str(String),
    Flag(bool),
    Int(i64),
    Float(f64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Str,
    Flag,
    Int,
    Float,
}

impl Format {
    fn raw(self) -> c_int {
        match self {
            Self::Str => FORMAT_STRING,
            Self::Flag => FORMAT_FLAG,
            Self::Int => FORMAT_INT64,
            Self::Float => FORMAT_DOUBLE,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Shutdown,
    StartFile,
    /// 0 = EOF, 2 = stop, 3 = quit, 4 = error (with mpv's error text), 5 = redirect.
    EndFile { reason: i32, error: Option<String> },
    FileLoaded,
    VideoReconfig,
    AudioReconfig,
    Seek,
    PlaybackRestart,
    Property { id: u64, name: String, value: Value },
    Log { prefix: String, level: String, text: String },
    CommandReply { id: u64, error: Option<String> },
    Other(i32),
}

type Callback = Box<dyn Fn() + Send + Sync>;

unsafe extern "C" fn trampoline(data: *mut c_void) {
    // SAFETY: `data` is the `Callback` boxed in `Mpv` / `RenderContext`, alive until they are dropped.
    let cb = unsafe { &*(data as *const Callback) };
    cb();
}

/// An mpv player instance. The client API is thread-safe.
pub struct Mpv {
    api: &'static Api,
    h: Handle,
    wakeup: Option<Box<Callback>>,
}

unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

impl Mpv {
    /// Create and initialise mpv with options that must be set before `mpv_initialize`.
    pub fn new(options: &[(&str, &str)]) -> Result<Self, String> {
        let api = api()?;
        // SAFETY: plain libmpv calls on a handle we own.
        let h = unsafe { (api.create)() };
        if h.is_null() {
            return Err("mpv_create failed".into());
        }
        let mpv = Self { api, h, wakeup: None };
        for (k, v) in options {
            let (k, v) = (cstr(k), cstr(v));
            let r = unsafe { (api.set_option_string)(h, k.as_ptr(), v.as_ptr()) };
            if r < 0 {
                eprintln!("naTiVeFS: mpv option {k:?}={v:?}: {}", err_text(api, r));
            }
        }
        let r = unsafe { (api.initialize)(h) };
        if r < 0 {
            return Err(format!("mpv_initialize: {}", err_text(api, r)));
        }
        Ok(mpv)
    }

    pub fn set_wakeup(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let cb: Box<Callback> = Box::new(Box::new(f));
        let ptr = &*cb as *const Callback as *mut c_void;
        unsafe { (self.api.set_wakeup_callback)(self.h, Some(trampoline), ptr) };
        self.wakeup = Some(cb);
    }

    fn with_args<R>(args: &[&str], f: impl FnOnce(*mut *const c_char) -> R) -> R {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        f(ptrs.as_mut_ptr())
    }

    pub fn command(&self, args: &[&str]) -> Result<(), String> {
        let r = Self::with_args(args, |p| unsafe { (self.api.command)(self.h, p) });
        if r < 0 { Err(err_text(self.api, r)) } else { Ok(()) }
    }

    /// Runs without blocking the caller; the result arrives as `Event::CommandReply { id }`.
    pub fn command_async(&self, id: u64, args: &[&str]) {
        Self::with_args(args, |p| unsafe { (self.api.command_async)(self.h, id, p) });
    }

    pub fn set_str(&self, name: &str, value: &str) -> Result<(), String> {
        let (n, v) = (cstr(name), cstr(value));
        let r = unsafe { (self.api.set_property_string)(self.h, n.as_ptr(), v.as_ptr()) };
        if r < 0 { Err(err_text(self.api, r)) } else { Ok(()) }
    }

    pub fn set_f64(&self, name: &str, mut value: f64) -> Result<(), String> {
        let n = cstr(name);
        let r = unsafe { (self.api.set_property)(self.h, n.as_ptr(), FORMAT_DOUBLE, &mut value as *mut f64 as *mut c_void) };
        if r < 0 { Err(err_text(self.api, r)) } else { Ok(()) }
    }

    pub fn set_flag(&self, name: &str, value: bool) -> Result<(), String> {
        let n = cstr(name);
        let mut v: c_int = value as c_int;
        let r = unsafe { (self.api.set_property)(self.h, n.as_ptr(), FORMAT_FLAG, &mut v as *mut c_int as *mut c_void) };
        if r < 0 { Err(err_text(self.api, r)) } else { Ok(()) }
    }

    pub fn get_f64(&self, name: &str) -> Option<f64> {
        let n = cstr(name);
        let mut v = 0f64;
        let r = unsafe { (self.api.get_property)(self.h, n.as_ptr(), FORMAT_DOUBLE, &mut v as *mut f64 as *mut c_void) };
        (r >= 0).then_some(v)
    }

    pub fn get_i64(&self, name: &str) -> Option<i64> {
        let n = cstr(name);
        let mut v = 0i64;
        let r = unsafe { (self.api.get_property)(self.h, n.as_ptr(), FORMAT_INT64, &mut v as *mut i64 as *mut c_void) };
        (r >= 0).then_some(v)
    }

    pub fn get_flag(&self, name: &str) -> Option<bool> {
        let n = cstr(name);
        let mut v: c_int = 0;
        let r = unsafe { (self.api.get_property)(self.h, n.as_ptr(), FORMAT_FLAG, &mut v as *mut c_int as *mut c_void) };
        (r >= 0).then_some(v != 0)
    }

    pub fn get_str(&self, name: &str) -> Option<String> {
        let n = cstr(name);
        let p = unsafe { (self.api.get_property_string)(self.h, n.as_ptr()) };
        if p.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        unsafe { (self.api.free)(p as *mut c_void) };
        Some(s)
    }

    pub fn observe(&self, id: u64, name: &str, format: Format) {
        let n = cstr(name);
        unsafe { (self.api.observe_property)(self.h, id, n.as_ptr(), format.raw()) };
    }

    /// Next queued event, `None` when the queue is empty (`timeout` 0 = do not wait).
    pub fn next_event(&self, timeout: f64) -> Option<Event> {
        // SAFETY: the event stays valid until the next mpv_wait_event call on this handle; it is copied out here.
        let ev = unsafe { &*(self.api.wait_event)(self.h, timeout) };
        let read = |p: *const c_char| if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() };
        Some(match ev.event_id {
            0 => return None,
            1 => Event::Shutdown,
            2 => {
                let m = unsafe { &*(ev.data as *const RawLogMessage) };
                Event::Log { prefix: read(m.prefix), level: read(m.level), text: read(m.text) }
            }
            5 => Event::CommandReply { id: ev.reply_userdata, error: (ev.error < 0).then(|| err_text(self.api, ev.error)) },
            6 => Event::StartFile,
            7 => {
                let e = unsafe { &*(ev.data as *const RawEventEndFile) };
                Event::EndFile { reason: e.reason, error: (e.error < 0).then(|| err_text(self.api, e.error)) }
            }
            8 => Event::FileLoaded,
            17 => Event::VideoReconfig,
            18 => Event::AudioReconfig,
            20 => Event::Seek,
            21 => Event::PlaybackRestart,
            22 => {
                let p = unsafe { &*(ev.data as *const RawEventProperty) };
                let value = unsafe {
                    match p.format {
                        _ if p.data.is_null() => Value::None,
                        FORMAT_STRING | 2 => Value::Str(read(*(p.data as *const *const c_char))),
                        FORMAT_FLAG => Value::Flag(*(p.data as *const c_int) != 0),
                        FORMAT_INT64 => Value::Int(*(p.data as *const i64)),
                        FORMAT_DOUBLE => Value::Float(*(p.data as *const f64)),
                        _ => Value::None,
                    }
                };
                Event::Property { id: ev.reply_userdata, name: read(p.name), value }
            }
            other => Event::Other(other),
        })
    }

    /// The OpenGL render context. Must be created, used and dropped on the thread that owns the GL context.
    ///
    /// `get_proc` resolves GL functions; mpv only calls it while this function runs.
    pub fn render_context(&self, get_proc: &dyn Fn(&CStr) -> *const c_void) -> Result<RenderContext, String> {
        unsafe extern "C" fn resolve(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
            // SAFETY: `ctx` points at the `&dyn Fn` below, which outlives mpv_render_context_create.
            let f = unsafe { &*(ctx as *const &dyn Fn(&CStr) -> *const c_void) };
            f(unsafe { CStr::from_ptr(name) }) as *mut c_void
        }
        let mut get_proc_ref: &dyn Fn(&CStr) -> *const c_void = get_proc;
        let mut init = OpenGlInitParams {
            get_proc_address: resolve,
            get_proc_address_ctx: &mut get_proc_ref as *mut &dyn Fn(&CStr) -> *const c_void as *mut c_void,
        };
        let api_type = c"opengl";
        let mut params = [
            RenderParam { kind: RENDER_PARAM_API_TYPE, data: api_type.as_ptr() as *mut c_void },
            RenderParam { kind: RENDER_PARAM_OPENGL_INIT_PARAMS, data: &mut init as *mut OpenGlInitParams as *mut c_void },
            RenderParam { kind: RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        let mut ctx: *mut c_void = std::ptr::null_mut();
        let r = unsafe { (self.api.render_context_create)(&mut ctx, self.h, params.as_mut_ptr()) };
        if r < 0 || ctx.is_null() {
            return Err(format!("mpv_render_context_create: {}", err_text(self.api, r)));
        }
        Ok(RenderContext { api: self.api, ctx, update: None })
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        unsafe {
            (self.api.set_wakeup_callback)(self.h, None, std::ptr::null_mut());
            (self.api.terminate_destroy)(self.h);
        }
    }
}

/// mpv's OpenGL renderer bound to one GL context (UI thread only).
pub struct RenderContext {
    api: &'static Api,
    ctx: *mut c_void,
    update: Option<Box<Callback>>,
}

impl RenderContext {
    /// Called from an mpv thread whenever a new frame should be drawn.
    pub fn set_update_callback(&mut self, f: impl Fn() + Send + Sync + 'static) {
        let cb: Box<Callback> = Box::new(Box::new(f));
        let ptr = &*cb as *const Callback as *mut c_void;
        unsafe { (self.api.render_context_set_update_callback)(self.ctx, Some(trampoline), ptr) };
        self.update = Some(cb);
    }

    /// True when a new video frame is waiting.
    pub fn update(&self) -> bool {
        unsafe { (self.api.render_context_update)(self.ctx) & RENDER_UPDATE_FRAME != 0 }
    }

    /// Draw the current frame into framebuffer `fbo` (0 = the window), `w` x `h` physical px.
    /// `block`: wait until the frame's display time (mpv's default) or return at once.
    pub fn render(&self, fbo: i32, w: i32, h: i32, flip_y: bool, block: bool) {
        let mut target = OpenGlFbo { fbo, w, h, internal_format: 0 };
        let mut flip: c_int = flip_y as c_int;
        let mut wait: c_int = block as c_int;
        let mut params = [
            RenderParam { kind: RENDER_PARAM_OPENGL_FBO, data: &mut target as *mut OpenGlFbo as *mut c_void },
            RenderParam { kind: RENDER_PARAM_FLIP_Y, data: &mut flip as *mut c_int as *mut c_void },
            RenderParam { kind: RENDER_PARAM_BLOCK_FOR_TARGET_TIME, data: &mut wait as *mut c_int as *mut c_void },
            RenderParam { kind: RENDER_PARAM_INVALID, data: std::ptr::null_mut() },
        ];
        unsafe { (self.api.render_context_render)(self.ctx, params.as_mut_ptr()) };
    }

    pub fn report_swap(&self) {
        unsafe { (self.api.render_context_report_swap)(self.ctx) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe {
            (self.api.render_context_set_update_callback)(self.ctx, None, std::ptr::null_mut());
            (self.api.render_context_free)(self.ctx);
        }
    }
}
