//! Video player window. libmpv decodes (hardware decoding where available) and draws with its OpenGL
//! render API straight into the window's framebuffer before Slint draws the controls on top
//! ("underlay"): one window, no child windows, works the same on Windows, X11 and Wayland.
//! Nothing redraws while paused: mpv only asks for a frame when there is a new one.

pub mod mpv;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_void, CStr};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, GraphicsAPI, ModelRc, RenderingState, VecModel};
use tvfs_core::content::Content;

use crate::{windows, PlayerWindow, Shared, StatRow, TrackItem};
use mpv::{Event, Format, Mpv, Value};

/// Controls hide after this long without mouse movement while playing.
const HIDE_AFTER: Duration = Duration::from_millis(2600);
/// Volume above this from the last video is confirmed before the next one plays (owner).
const LOUD: f64 = 150.0;
/// Samples kept for the statistics graphs (one per second).
const GRAPH_SAMPLES: usize = 60;

// observed property ids
const P_PAUSE: u64 = 1;
const P_TIME: u64 = 2;
const P_DURATION: u64 = 3;
const P_VOLUME: u64 = 4;
const P_MUTE: u64 = 5;
const P_SPEED: u64 = 6;
const P_TRACKS: u64 = 7;
const P_SID: u64 = 8;
const P_AID: u64 = 9;
const P_CACHE_WAIT: u64 = 10;
const P_CACHE_STATE: u64 = 11;
const P_CACHE_TIME: u64 = 12;
const P_VID: u64 = 13;
const P_PITCH: u64 = 14;
const LOADFILE: u64 = 100;

struct Player {
    id: u64,
    window: PlayerWindow,
    mpv: Arc<Mpv>,
    content: Content,
    playing: bool,
    position: f64,
    duration: f64,
    cache_wait: bool,
    cache_state: i64,
    loaded: bool,
    last_pos_push: Instant,
    visible: bool,
    sized: bool,
    hide_timer: slint::Timer,
    osd_timer: slint::Timer,
    stats_timer: Option<slint::Timer>,
    save_timer: slint::Timer,
    net: VecDeque<f64>,
    buf: VecDeque<f64>,
    /// Waiting for the "volume above 150 %" answer.
    asking: bool,
    /// A statistics snapshot is being read (off the UI thread).
    stats_busy: Arc<std::sync::atomic::AtomicBool>,
    /// Shared with the rendering notifier.
    playing_flag: Arc<std::sync::atomic::AtomicBool>,
}

thread_local! {
    static PLAYERS: RefCell<Vec<Rc<RefCell<Player>>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
}

fn find(id: u64) -> Option<Rc<RefCell<Player>>> {
    PLAYERS.with(|l| l.borrow().iter().find(|p| p.borrow().id == id).cloned())
}

/// mpv language list from the system locale (cs-CZ -> "cs,ces,cze"); mpv matches the track tags.
fn system_languages() -> String {
    let mut out: Vec<String> = Vec::new();
    for loc in sys_locale::get_locales() {
        let code = loc.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        if code.is_empty() || out.contains(&code) {
            continue;
        }
        out.push(code.clone());
        for alt in iso639_2(&code) {
            out.push((*alt).to_owned());
        }
    }
    out.join(",")
}

/// ISO 639-2 codes (both forms) for the common ISO 639-1 codes; container tags use either.
fn iso639_2(code: &str) -> &'static [&'static str] {
    match code {
        "cs" => &["ces", "cze"],
        "sk" => &["slk", "slo"],
        "en" => &["eng"],
        "de" => &["deu", "ger"],
        "fr" => &["fra", "fre"],
        "es" => &["spa"],
        "it" => &["ita"],
        "pl" => &["pol"],
        "ru" => &["rus"],
        "uk" => &["ukr"],
        "ja" => &["jpn"],
        "ko" => &["kor"],
        "zh" => &["zho", "chi"],
        "pt" => &["por"],
        "nl" => &["nld", "dut"],
        "hu" => &["hun"],
        _ => &[],
    }
}

fn language_name(code: &str) -> String {
    let n = match code.to_ascii_lowercase().as_str() {
        "cs" | "ces" | "cze" => "Czech",
        "sk" | "slk" | "slo" => "Slovak",
        "en" | "eng" => "English",
        "de" | "deu" | "ger" => "German",
        "fr" | "fra" | "fre" => "French",
        "es" | "spa" => "Spanish",
        "it" | "ita" => "Italian",
        "pl" | "pol" => "Polish",
        "ru" | "rus" => "Russian",
        "uk" | "ukr" => "Ukrainian",
        "ja" | "jpn" => "Japanese",
        "ko" | "kor" => "Korean",
        "zh" | "zho" | "chi" => "Chinese",
        "pt" | "por" => "Portuguese",
        "nl" | "nld" | "dut" => "Dutch",
        "hu" | "hun" => "Hungarian",
        "und" | "" => "",
        other => return other.to_ascii_uppercase(),
    };
    n.to_owned()
}

fn fmt_clock(s: f64) -> String {
    let s = s.max(0.0).floor() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn fmt_bitrate(bps: f64) -> String {
    if bps >= 1_000_000.0 {
        format!("{:.2} Mbit/s", bps / 1_000_000.0)
    } else {
        format!("{:.0} kbit/s", bps / 1000.0)
    }
}

/// Open a video in a new player window (or bring the one already playing it to the front).
pub fn open(shared: &Arc<Shared>, content: Content) {
    let existing = PLAYERS.with(|l| l.borrow().iter().find(|p| p.borrow().content.url == content.url).cloned());
    if let Some(p) = existing {
        windows::raise(p.borrow().window.window());
        return;
    }
    if let Err(e) = mpv::available() {
        eprintln!("naTiVeFS: libmpv: {e}");
        crate::files_ui::show_error(
            shared,
            &content.page,
            "The video player is not available: libmpv was not found. Reinstall naTiVeFS, or open the video in the browser.",
        );
        return;
    }
    let cfg = shared.config.lock().unwrap().clone();
    let Ok(window) = PlayerWindow::new() else { return };
    let ask = cfg.player_volume > LOUD;
    let langs = system_languages();
    let volume = cfg.player_volume.clamp(0.0, 200.0).to_string();
    let mut options: Vec<(&str, &str)> = vec![
        ("vo", "libmpv"),
        ("hwdec", if cfg.player_hwdec { "auto-safe" } else { "no" }),
        ("keep-open", "yes"),
        ("idle", "yes"),
        ("config", "no"),
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("terminal", "no"),
        ("volume-max", "200"),
        ("volume", &volume),
        ("audio-pitch-correction", if cfg.player_pitch_correction { "yes" } else { "no" }),
        ("sub-auto", "no"),
        ("cache", "yes"),
        ("demuxer-max-bytes", "256MiB"),
        ("demuxer-max-back-bytes", "64MiB"),
        ("user-agent", concat!("naTiVeFS/", env!("CARGO_PKG_VERSION"))),
        ("screenshot-directory", "~~desktop/"),
    ];
    if !langs.is_empty() {
        options.push(("slang", &langs));
        options.push(("alang", &langs));
    }
    if ask {
        options.push(("pause", "yes"));
    }
    // debug builds: `NATIVEFS_MPV_OPTS="k=v;k=v"` adds / overrides mpv options (performance experiments)
    let extra: Vec<(String, String)> = if cfg!(debug_assertions) {
        std::env::var("NATIVEFS_MPV_OPTS")
            .unwrap_or_default()
            .split(';')
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())))
            .collect()
    } else {
        Vec::new()
    };
    for (k, v) in &extra {
        options.push((k.as_str(), v.as_str()));
    }
    let mut player_mpv = match Mpv::new(&options) {
        Ok(m) => m,
        Err(e) => {
            crate::files_ui::show_error(shared, &content.page, &format!("The video player could not start: {e}"));
            return;
        }
    };
    // "TVFS Users Only" videos need the session cookie (only sent to the TVFS server)
    if content.protected {
        if let Some(token) = shared.client.session_token() {
            let _ = player_mpv.set_str("http-header-fields", &format!("Cookie: tvfs_token={token}"));
        }
    }
    let id = NEXT_ID.with(|n| {
        let id = n.get();
        n.set(id + 1);
        id
    });
    player_mpv.set_wakeup(move || {
        let _ = slint::invoke_from_event_loop(move || drain(id));
    });
    let mpv = Arc::new(player_mpv);
    for (pid, name, fmt) in [
        (P_PAUSE, "pause", Format::Flag),
        (P_TIME, "time-pos", Format::Float),
        (P_DURATION, "duration", Format::Float),
        (P_VOLUME, "volume", Format::Float),
        (P_MUTE, "mute", Format::Flag),
        (P_SPEED, "speed", Format::Float),
        (P_TRACKS, "track-list/count", Format::Int),
        (P_SID, "sid", Format::Str),
        (P_AID, "aid", Format::Str),
        (P_CACHE_WAIT, "paused-for-cache", Format::Flag),
        (P_CACHE_STATE, "cache-buffering-state", Format::Int),
        (P_CACHE_TIME, "demuxer-cache-time", Format::Float),
        (P_VID, "vid", Format::Str),
        (P_PITCH, "audio-pitch-correction", Format::Flag),
    ] {
        mpv.observe(pid, name, fmt);
    }

    window.set_file_name(content.name.clone().into());
    window.set_local(content.local_path().is_some());
    window.set_status("Loading…".into());
    window.set_volume(cfg.player_volume as f32);
    window.set_hwdec(cfg.player_hwdec);
    window.set_pitch_correction(cfg.player_pitch_correction);
    window.set_stats_level(0);
    window.set_ask_volume(ask);

    let player = Rc::new(RefCell::new(Player {
        id,
        window: window.clone_strong(),
        mpv: mpv.clone(),
        content: content.clone(),
        playing: false,
        position: 0.0,
        duration: 0.0,
        cache_wait: false,
        cache_state: 0,
        loaded: false,
        last_pos_push: Instant::now(),
        visible: true,
        sized: false,
        hide_timer: slint::Timer::default(),
        osd_timer: slint::Timer::default(),
        stats_timer: None,
        save_timer: slint::Timer::default(),
        net: VecDeque::new(),
        buf: VecDeque::new(),
        asking: ask,
        stats_busy: Arc::default(),
        playing_flag: Arc::default(),
    }));
    PLAYERS.with(|l| l.borrow_mut().push(player.clone()));
    // debug builds: `NATIVEFS_PLAYER_STATS=0..3` and `NATIVEFS_PLAYER_MENU=1..4` preset the overlay (screenshots)
    #[cfg(debug_assertions)]
    {
        if let Some(l) = std::env::var("NATIVEFS_PLAYER_STATS").ok().and_then(|v| v.parse::<i32>().ok()) {
            window.set_stats_level(l.clamp(0, 3));
        }
        if let Some(m) = std::env::var("NATIVEFS_PLAYER_MENU").ok().and_then(|v| v.parse::<i32>().ok()) {
            window.set_menu(m);
        }
        // `NATIVEFS_PLAYER_FULLSCREEN=1`: start in fullscreen (performance tests)
        if std::env::var("NATIVEFS_PLAYER_FULLSCREEN").is_ok() {
            let w = window.as_weak();
            slint::Timer::single_shot(Duration::from_millis(1500), move || {
                if let Some(w) = w.upgrade() {
                    w.invoke_toggle_fullscreen();
                }
            });
        }
        // `NATIVEFS_PLAYER_PAUSE_AFTER=<s>`: pause by itself (idle CPU / GPU measurements)
        if let Some(secs) = std::env::var("NATIVEFS_PLAYER_PAUSE_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) {
            let m = mpv.clone();
            slint::Timer::single_shot(Duration::from_secs(secs), move || {
                let _ = m.set_flag("pause", true);
            });
        }
    }

    // ── rendering: mpv draws into the window's framebuffer before Slint draws the controls ──
    let render: Rc<RefCell<Option<mpv::RenderContext>>> = Rc::default();
    let prof: Arc<std::sync::Mutex<Prof>> = Arc::default();
    let playing_flag: Arc<std::sync::atomic::AtomicBool> = Arc::default();
    let drive = cfg!(debug_assertions) && std::env::var("NATIVEFS_PLAYER_DRIVE").is_ok();
    #[cfg(debug_assertions)]
    if std::env::var("NATIVEFS_PLAYER_PROFILE").is_ok() {
        profile_timer(&mpv, &prof);
    }
    {
        let (mpv, render, weak, url, prof, playing_flag) = (mpv.clone(), render.clone(), window.as_weak(), content.url.clone(), prof.clone(), playing_flag.clone());
        let notifier = window.window().set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                    eprintln!("naTiVeFS: the player needs the OpenGL renderer");
                    return;
                };
                // debug: `NATIVEFS_SWAP_INTERVAL=<n>` changes the window's vsync (swap interval) for experiments
                #[cfg(windows)]
                if let Some(n) = std::env::var("NATIVEFS_SWAP_INTERVAL").ok().filter(|_| cfg!(debug_assertions)).and_then(|v| v.parse::<i32>().ok()) {
                    let f = get_proc_address(c"wglSwapIntervalEXT");
                    if !f.is_null() {
                        type SwapInterval = unsafe extern "system" fn(i32) -> i32;
                        let set: SwapInterval = unsafe { std::mem::transmute::<*const c_void, SwapInterval>(f) };
                        eprintln!("PROFILE swap interval {n}: {}", unsafe { set(n) });
                    }
                }
                match mpv.render_context(get_proc_address) {
                    Ok(mut ctx) => {
                        let weak = weak.clone();
                        let prof = prof.clone();
                        ctx.set_update_callback(move || {
                            {
                                let mut p = prof.lock().unwrap();
                                p.updates += 1;
                                p.pending.get_or_insert_with(Instant::now);
                            }
                            let weak = weak.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(w) = weak.upgrade() {
                                    w.window().request_redraw();
                                }
                            });
                        });
                        *render.borrow_mut() = Some(ctx);
                        // mpv needs its video output before the file starts
                        mpv.command_async(LOADFILE, &["loadfile", &url]);
                    }
                    Err(e) => eprintln!("naTiVeFS: {e}"),
                }
            }
            RenderingState::BeforeRendering => {
                let GraphicsAPI::NativeOpenGL { get_proc_address } = api else { return };
                let borrowed = render.borrow();
                let Some(ctx) = borrowed.as_ref() else { return };
                let Some(w) = weak.upgrade() else { return };
                let t0 = Instant::now();
                let frame = ctx.update();
                let size = w.window().size();
                let block = !(cfg!(debug_assertions) && std::env::var("NATIVEFS_MPV_NOBLOCK").is_ok());
                ctx.render(current_fbo(get_proc_address), size.width as i32, size.height as i32, true, block);
                let mut p = prof.lock().unwrap();
                let took = t0.elapsed();
                if frame {
                    let now = Instant::now();
                    if let Some(l) = p.last_frame.replace(now) {
                        p.intervals.push(now.duration_since(l).as_secs_f64() * 1000.0);
                    }
                }
                if let Some(a) = p.after.take() {
                    let gap = t0.duration_since(a);
                    p.gap_total += gap;
                    p.gap_max = p.gap_max.max(gap);
                }
                p.before = Some(Instant::now());
                p.renders += 1;
                p.frames += frame as u32;
                p.render_total += took;
                p.render_max = p.render_max.max(took);
                if let Some(since) = p.pending.take() {
                    let lat = t0.duration_since(since);
                    p.lat_total += lat;
                    p.lat_max = p.lat_max.max(lat);
                    p.lat_n += 1;
                }
            }
            RenderingState::AfterRendering => {
                let skip = cfg!(debug_assertions) && std::env::var("NATIVEFS_NO_REPORT_SWAP").is_ok();
                if let Some(ctx) = render.borrow().as_ref().filter(|_| !skip) {
                    ctx.report_swap();
                }
                let mut p = prof.lock().unwrap();
                if let Some(b) = p.before.take() {
                    let d = b.elapsed();
                    p.slint_total += d;
                    p.slint_max = p.slint_max.max(d);
                }
                p.after = Some(Instant::now());
                drop(p);
                // continuous presentation at the display rate while playing (VRR stays at the top rate)
                if drive && playing_flag.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(w) = weak.upgrade() {
                        w.window().request_redraw();
                    }
                }
            }
            RenderingState::RenderingTeardown => {
                render.borrow_mut().take();
            }
            _ => {}
        });
        if let Err(e) = notifier {
            eprintln!("naTiVeFS: player rendering: {e:?}");
        }
    }

    player.borrow_mut().playing_flag = playing_flag;
    wire(shared, &player);
    windows::place(window.window());
    let _ = window.show();
    // the controls start visible: subtitles start above them
    sub_margin(&player.borrow(), true);
    activity(&player);
}

/// Rendering timings (debug `NATIVEFS_PLAYER_PROFILE=1`, printed once a second).
#[derive(Default)]
struct Prof {
    updates: u32,
    renders: u32,
    frames: u32,
    render_total: Duration,
    render_max: Duration,
    pending: Option<Instant>,
    lat_total: Duration,
    lat_max: Duration,
    lat_n: u32,
    /// end of mpv's render -> end of Slint's drawing
    before: Option<Instant>,
    slint_total: Duration,
    slint_max: Duration,
    /// end of Slint's drawing -> next frame (buffer swap + waiting for the next request)
    after: Option<Instant>,
    gap_total: Duration,
    gap_max: Duration,
    /// intervals between frames that showed a new video frame (frame pacing, "1 % lows")
    last_frame: Option<Instant>,
    intervals: Vec<f64>,
}

#[cfg(debug_assertions)]
fn profile_timer(mpv: &Arc<Mpv>, prof: &Arc<std::sync::Mutex<Prof>>) {
    // its own thread: reading mpv properties on the UI thread would disturb what is measured
    let (mpv, prof) = (mpv.clone(), prof.clone());
    std::thread::spawn(move || {
        let mut last_drop = 0i64;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if Arc::strong_count(&prof) == 1 {
                return;
            }
            let p = {
                let mut g = prof.lock().unwrap();
                let last = g.last_frame;
                let taken = std::mem::take(&mut *g);
                g.last_frame = last;
                taken
            };
            let mut iv = p.intervals.clone();
            iv.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let pct = |q: f64| iv.get(((iv.len() as f64 - 1.0) * q).round().max(0.0) as usize).copied().unwrap_or(0.0);
            let drops = mpv.get_i64("frame-drop-count").unwrap_or(0);
            let ms = |d: Duration| d.as_secs_f64() * 1000.0;
            eprintln!(
                "PROFILE t={:.1} pacing p50={:.2} p99={:.2} max={:.2}ms avsync={:+.1}ms updates={} renders={} new_frames={} render_avg={:.2}ms max={:.2}ms slint_avg={:.2}ms max={:.2}ms gap_avg={:.2}ms max={:.2}ms latency_avg={:.2}ms max={:.2}ms drops+{} (total {}) hwdec={}",
                mpv.get_f64("time-pos").unwrap_or(0.0),
                pct(0.5),
                pct(0.99),
                iv.last().copied().unwrap_or(0.0),
                mpv.get_f64("avsync").unwrap_or(0.0) * 1000.0,
                p.updates,
                p.renders,
                p.frames,
                ms(p.render_total) / p.renders.max(1) as f64,
                ms(p.render_max),
                ms(p.slint_total) / p.renders.max(1) as f64,
                ms(p.slint_max),
                ms(p.gap_total) / p.renders.max(1) as f64,
                ms(p.gap_max),
                ms(p.lat_total) / p.lat_n.max(1) as f64,
                ms(p.lat_max),
                drops - last_drop,
                drops,
                mpv.get_str("hwdec-current").unwrap_or_default(),
            );
            last_drop = drops;
        }
    });
}

/// The framebuffer Slint renders into (0 for the window itself on most systems).
fn current_fbo(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> i32 {
    type GetIntegerv = unsafe extern "system" fn(u32, *mut i32);
    const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
    let f = get_proc_address(c"glGetIntegerv");
    if f.is_null() {
        return 0;
    }
    // SAFETY: glGetIntegerv from the current context, called on the rendering thread.
    let get: GetIntegerv = unsafe { std::mem::transmute::<*const c_void, GetIntegerv>(f) };
    let mut fbo = 0;
    unsafe { get(GL_FRAMEBUFFER_BINDING, &mut fbo) };
    fbo
}

/// Handle everything mpv queued (runs on the UI thread after a wakeup).
fn drain(id: u64) {
    let Some(p) = find(id) else { return };
    loop {
        let ev = p.borrow().mpv.next_event(0.0);
        let Some(ev) = ev else { break };
        handle(&p, ev);
    }
}

fn handle(p: &Rc<RefCell<Player>>, ev: Event) {
    let mut pl = p.borrow_mut();
    let w = pl.window.clone_strong();
    match ev {
        Event::Property { id, value, .. } => match (id, value) {
            (P_PAUSE, Value::Flag(paused)) => {
                pl.playing = !paused;
                pl.playing_flag.store(!paused, std::sync::atomic::Ordering::Relaxed);
                if !paused {
                    w.window().request_redraw();
                }
                w.set_playing(!paused);
                drop(pl);
                stats_schedule(p);
                activity(p);
            }
            (P_TIME, Value::Float(t)) => {
                pl.position = t;
                // the seek bar does not need every frame
                if !pl.playing || pl.last_pos_push.elapsed() >= Duration::from_millis(200) {
                    pl.last_pos_push = Instant::now();
                    w.set_position(t as f32);
                    w.set_pos_text(fmt_clock(t).into());
                }
            }
            (P_DURATION, Value::Float(d)) => {
                pl.duration = d;
                w.set_duration(d as f32);
                w.set_dur_text(fmt_clock(d).into());
            }
            (P_VOLUME, Value::Float(v)) => w.set_volume(v as f32),
            (P_MUTE, Value::Flag(m)) => w.set_muted(m),
            (P_SPEED, Value::Float(s)) => w.set_speed(s as f32),
            (P_PITCH, Value::Flag(on)) => w.set_pitch_correction(on),
            (P_TRACKS, _) | (P_SID, _) | (P_AID, _) | (P_VID, _) => refresh_tracks(&pl),
            (P_CACHE_WAIT, Value::Flag(waiting)) => {
                pl.cache_wait = waiting;
                status(&pl);
            }
            (P_CACHE_STATE, Value::Int(s)) => {
                pl.cache_state = s;
                status(&pl);
            }
            (P_CACHE_TIME, Value::Float(t)) => w.set_buffered(t as f32),
            _ => {}
        },
        Event::FileLoaded => {
            pl.loaded = true;
            // debug experiment: the exact refresh rate of the window's monitor for display-synced playback
            if cfg!(debug_assertions) && std::env::var("NATIVEFS_PLAYER_EXACT_HZ").is_ok() {
                if let Some(mhz) = w.window().with_winit_window(|x| x.current_monitor().and_then(|m| m.refresh_rate_millihertz())).flatten() {
                    eprintln!("PROFILE monitor {mhz} mHz");
                    let _ = pl.mpv.set_f64("display-fps-override", mhz as f64 / 1000.0);
                }
            }
            w.set_error("".into());
            status(&pl);
            refresh_tracks(&pl);
        }
        Event::VideoReconfig => {
            if !pl.sized {
                if let (Some(dw), Some(dh)) = (pl.mpv.get_i64("dwidth"), pl.mpv.get_i64("dheight")) {
                    pl.sized = true;
                    size_to_video(&w, dw as f32, dh as f32);
                }
            }
        }
        Event::EndFile { reason: 4, error } => {
            let msg = error.unwrap_or_else(|| "unknown error".into());
            let msg = if msg.contains("loading failed") || msg.contains("unrecognized") {
                if pl.content.protected {
                    "This video is for TVFS users only — log in in naTiVeFS first.".to_owned()
                } else {
                    format!("{msg}. The file may have expired, or its format is not supported.")
                }
            } else {
                msg
            };
            w.set_error(msg.into());
            w.set_status("".into());
        }
        Event::CommandReply { id: LOADFILE, error: Some(e) } => {
            w.set_error(format!("Could not open the video: {e}").into());
            w.set_status("".into());
        }
        _ => {}
    }
}

fn status(pl: &Player) {
    let text = if !pl.loaded {
        "Loading…".to_owned()
    } else if pl.cache_wait {
        format!("Buffering… {} %", pl.cache_state.clamp(0, 100))
    } else {
        String::new()
    };
    pl.window.set_status(text.into());
}

fn refresh_tracks(pl: &Player) {
    let m = &pl.mpv;
    let n = m.get_i64("track-list/count").unwrap_or(0);
    let mut subs = Vec::new();
    let mut audio = Vec::new();
    let mut has_video = false;
    for i in 0..n {
        let g = |k: &str| m.get_str(&format!("track-list/{i}/{k}"));
        let kind = g("type").unwrap_or_default();
        let id = g("id").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let selected = g("selected").as_deref() == Some("yes");
        let lang = g("lang").unwrap_or_default();
        let title = g("title").unwrap_or_default();
        let codec = g("codec").unwrap_or_default();
        let mut label = language_name(&lang);
        if !title.is_empty() && title != label {
            label = if label.is_empty() { title } else { format!("{label} — {title}") };
        }
        match kind.as_str() {
            "video" => {
                // a cover image in an audio file is not a video
                if g("albumart").as_deref() != Some("yes") {
                    has_video = true;
                }
            }
            "audio" => {
                let ch = g("demux-channel-count").unwrap_or_default();
                if label.is_empty() {
                    label = format!("Track {id}");
                }
                let extra = if ch.is_empty() { codec } else { format!("{codec}, {ch} ch") };
                audio.push(TrackItem { id, label: format!("{label} ({extra})").into(), selected });
            }
            "sub" => {
                if label.is_empty() {
                    label = format!("Track {id}");
                }
                subs.push(TrackItem { id, label: format!("{label} ({codec})").into(), selected });
            }
            _ => {}
        }
    }
    pl.window.set_sub_tracks_any_selected(subs.iter().any(|t| t.selected));
    pl.window.set_sub_tracks(ModelRc::new(VecModel::from(subs)));
    pl.window.set_audio_tracks(ModelRc::new(VecModel::from(audio)));
    pl.window.set_has_video(has_video || n == 0);
}

/// Fit the window to the video, at most 75 % of the monitor (like VLC opening a file).
fn size_to_video(w: &PlayerWindow, vw: f32, vh: f32) {
    if vw < 16.0 || vh < 16.0 || w.window().is_fullscreen() || w.window().is_maximized() {
        return;
    }
    let scale = w.window().scale_factor();
    let monitor = w.window().with_winit_window(|win| win.current_monitor().map(|m| m.size())).flatten();
    let (mw, mh) = monitor.map_or((1920.0, 1080.0), |s| (s.width as f32 / scale, s.height as f32 / scale));
    let k = (mw * 0.75 / vw).min(mh * 0.75 / vh).min(1.0);
    let (tw, th) = ((vw * k).max(480.0), (vh * k).max(270.0));
    w.window().set_size(slint::LogicalSize::new(tw, th));
}

/// Subtitles sit above the control panel while it is shown (mpv's margin is in 720p-scaled pixels).
fn sub_margin(pl: &Player, controls_shown: bool) {
    let h = pl.window.window().size().to_logical(pl.window.window().scale_factor()).height.max(1.0);
    // panel: 96 px + 14 px gap
    let margin = if controls_shown { 22.0 + 124.0 * 720.0 / h as f64 } else { 22.0 };
    let _ = pl.mpv.set_f64("sub-margin-y", margin.round());
}

/// Mouse moved / key pressed: show the controls, hide them again later while playing.
fn activity(p: &Rc<RefCell<Player>>) {
    let pl = p.borrow();
    if !pl.window.get_controls() {
        sub_margin(&pl, true);
    }
    pl.window.set_controls(true);
    let weak = Rc::downgrade(p);
    pl.hide_timer.start(slint::TimerMode::SingleShot, HIDE_AFTER, move || {
        if let Some(p) = weak.upgrade() {
            let pl = p.borrow();
            // debug builds: `NATIVEFS_PLAYER_UI=1` keeps the controls up (screenshots)
            let keep = cfg!(debug_assertions) && std::env::var("NATIVEFS_PLAYER_UI").is_ok();
            if pl.playing && pl.window.get_menu() == 0 && !keep {
                pl.window.set_controls(false);
                sub_margin(&pl, false);
            }
        }
    });
}

fn osd(p: &Rc<RefCell<Player>>, text: String) {
    let pl = p.borrow();
    pl.window.set_osd(text.into());
    let weak = pl.window.as_weak();
    pl.osd_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(900), move || {
        if let Some(w) = weak.upgrade() {
            w.set_osd("".into());
        }
    });
}

fn toast(w: &PlayerWindow, text: &str) {
    let weak = w.as_weak();
    windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, text);
}

/// Statistics refresh once a second, only while shown, visible and playing (paused = no wake-ups).
fn stats_schedule(p: &Rc<RefCell<Player>>) {
    let mut pl = p.borrow_mut();
    let level = pl.window.get_stats_level();
    if level <= 0 {
        pl.stats_timer = None;
        return;
    }
    drop(pl);
    stats_update(p);
    let mut pl = p.borrow_mut();
    if !pl.playing || !pl.visible {
        pl.stats_timer = None;
        return;
    }
    if pl.stats_timer.is_none() {
        let weak = Rc::downgrade(p);
        let t = slint::Timer::default();
        t.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            if let Some(p) = weak.upgrade() {
                stats_update(&p);
            }
        });
        pl.stats_timer = Some(t);
    }
}

/// One statistics snapshot, read from mpv off the UI thread (each property read waits for mpv's core lock;
/// ~30 of them on the UI thread stalled the video for 25-40 ms once a second).
struct StatsSnapshot {
    rows: Vec<(String, String, bool)>,
    net: f64,
    ahead: f64,
}

/// `local`: a file from this computer ("Use TVFS tools locally"): the reads come from the disk, not the network.
fn collect_stats(m: &Mpv, level: i32, hz_mhz: Option<u32>, local: bool) -> StatsSnapshot {
    let s = |k: &str| m.get_str(k).filter(|v| !v.is_empty());
    let f = |k: &str| m.get_f64(k);
    let i = |k: &str| m.get_i64(k);
    let mut rows: Vec<(String, String, bool)> = Vec::new();
    let push = |rows: &mut Vec<(String, String, bool)>, k: &str, v: String| rows.push((k.to_owned(), v, false));
    let head = |rows: &mut Vec<(String, String, bool)>, k: &str| rows.push((k.to_owned(), String::new(), true));
    let hw = s("hwdec-current").filter(|h| h != "no").map_or("no (software)".to_owned(), |h| format!("yes ({h})"));
    let res = match (i("video-params/w"), i("video-params/h")) {
        (Some(w), Some(h)) => format!("{w} × {h}"),
        _ => "—".into(),
    };
    let fps = f("container-fps").map(|v| format!("{v:.3}").trim_end_matches('0').trim_end_matches('.').to_owned());
    let net = f("cache-speed").unwrap_or(0.0);
    let ahead = f("demuxer-cache-duration").unwrap_or(0.0);
    let vcodec = s("video-codec").or_else(|| s("video-format")).unwrap_or_else(|| "—".into());
    if level == 1 {
        push(&mut rows, "Video", vcodec);
        push(&mut rows, "Resolution", res);
        if let Some(fps) = fps {
            push(&mut rows, "Frame rate", format!("{fps} fps"));
        }
        if let Some(b) = f("video-bitrate") {
            push(&mut rows, "Bitrate", fmt_bitrate(b));
        }
        push(&mut rows, "HW decoding", hw);
    } else {
        head(&mut rows, "VIDEO");
        push(&mut rows, "Codec", vcodec);
        let dres = match (i("dwidth"), i("dheight")) {
            (Some(w), Some(h)) => format!("{w} × {h}"),
            _ => String::new(),
        };
        push(&mut rows, "Resolution", if dres.is_empty() || dres == res { res } else { format!("{res} (shown {dres})") });
        let est = f("estimated-vf-fps").map(|v| format!(" (real {v:.2})")).unwrap_or_default();
        push(&mut rows, "Frame rate", fps.map_or("—".into(), |v| format!("{v} fps{est}")));
        // mpv measures the video bitrate from packets; some files only report it later
        let avg = match (i("file-size"), f("duration")) {
            (Some(sz), Some(d)) if d > 0.0 => Some(sz as f64 * 8.0 / d),
            _ => None,
        };
        let vb = f("video-bitrate").map(fmt_bitrate);
        push(&mut rows, "Bitrate", match (vb, avg) {
            (Some(v), Some(a)) => format!("{v} (file avg {})", fmt_bitrate(a)),
            (Some(v), None) => v,
            (None, Some(a)) => format!("file avg {}", fmt_bitrate(a)),
            (None, None) => "—".into(),
        });
        let pix = s("video-params/pixelformat").unwrap_or_default();
        let hwpix = s("video-params/hw-pixelformat").map(|h| format!(" via {h}")).unwrap_or_default();
        push(&mut rows, "Pixel format", format!("{pix}{hwpix}"));
        push(&mut rows, "Colour",
            [s("video-params/colormatrix"), s("video-params/primaries"), s("video-params/gamma")].into_iter().flatten().collect::<Vec<_>>().join(" / "),
        );
        push(&mut rows, "HW decoding", hw);
        head(&mut rows, "AUDIO");
        push(&mut rows, "Codec", s("audio-codec-name").or_else(|| s("audio-codec")).unwrap_or_else(|| "—".into()));
        let rate = i("audio-params/samplerate").map(|r| format!("{} kHz", r as f64 / 1000.0)).unwrap_or_default();
        let ch = s("audio-params/hr-channels").or_else(|| i("audio-params/channel-count").map(|c| format!("{c} ch"))).unwrap_or_default();
        let fmt = s("audio-params/format").unwrap_or_default();
        push(&mut rows, "Format", [rate, ch, fmt].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · "));
        push(&mut rows, "Bitrate", f("audio-bitrate").map_or("—".into(), fmt_bitrate));
        head(&mut rows, "PLAYBACK");
        let dropped = i("frame-drop-count").unwrap_or(0);
        let dec_dropped = i("decoder-frame-drop-count").unwrap_or(0);
        push(&mut rows, "Dropped frames", format!("{dropped} output, {dec_dropped} decoder"));
        push(&mut rows, "A/V sync", f("avsync").map_or("—".into(), |v| format!("{:+.1} ms", v * 1000.0)));
        // the render API does not know the monitor; the window does
        push(&mut rows, "Display", hz_mhz.map_or("—".into(), |mhz| format!("{:.0} Hz", mhz as f64 / 1000.0)));
        push(&mut rows, "Speed", f("speed").map_or("—".into(), |v| format!("{v}×")));
        head(&mut rows, if local { "DISK & BUFFER" } else { "NETWORK & BUFFER" });
        push(&mut rows, if local { "Read speed" } else { "Network speed" }, fmt_bitrate(net * 8.0));
        push(&mut rows, "Buffered ahead", format!("{ahead:.1} s"));
        if let Some(b) = i("demuxer-cache-state/fw-bytes") {
            push(&mut rows, "Buffer size", tvfs_core::fmt::fmt_bytes(b.max(0) as u64));
        }
        if let Some(sz) = i("file-size") {
            push(&mut rows, "File size", tvfs_core::fmt::fmt_bytes(sz.max(0) as u64));
        }
        push(&mut rows, "Container", s("file-format").unwrap_or_else(|| "—".into()));
    }
    StatsSnapshot { rows, net, ahead }
}

fn stats_update(p: &Rc<RefCell<Player>>) {
    let pl = p.borrow();
    if pl.stats_busy.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let level = pl.window.get_stats_level();
    let hz = pl.window.window().with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz())).flatten();
    let (m, id, busy) = (pl.mpv.clone(), pl.id, pl.stats_busy.clone());
    let local = pl.content.local_path().is_some();
    drop(pl);
    std::thread::spawn(move || {
        // debug: `NATIVEFS_STATS_TEST=noread` fakes the snapshot, `=noapply` reads but does not touch the UI
        let test = if cfg!(debug_assertions) { std::env::var("NATIVEFS_STATS_TEST").unwrap_or_default() } else { String::new() };
        let snap = if test == "noread" {
            StatsSnapshot { rows: (0..24).map(|i| (format!("Key {i}"), format!("value {i}"), i % 6 == 0)).collect(), net: 1.0, ahead: 10.0 }
        } else {
            collect_stats(&m, level, hz, local)
        };
        if test == "noapply" {
            busy.store(false, std::sync::atomic::Ordering::Release);
            return;
        }
        let _ = slint::invoke_from_event_loop(move || {
            busy.store(false, std::sync::atomic::Ordering::Release);
            if let Some(p) = find(id) {
                apply_stats(&p, level, snap);
            }
        });
    });
}

fn apply_stats(p: &Rc<RefCell<Player>>, level: i32, snap: StatsSnapshot) {
    let mut pl = p.borrow_mut();
    let (net, ahead) = (snap.net, snap.ahead);
    let rows: Vec<StatRow> = snap.rows.into_iter().map(|(k, v, head)| StatRow { key: k.into(), value: v.into(), head }).collect();
    pl.net.push_back(net);
    pl.buf.push_back(ahead);
    while pl.net.len() > GRAPH_SAMPLES {
        pl.net.pop_front();
    }
    while pl.buf.len() > GRAPH_SAMPLES {
        pl.buf.pop_front();
    }
    let w = pl.window.clone_strong();
    w.set_stats(ModelRc::new(VecModel::from(rows)));
    if level >= 3 {
        let max_net = pl.net.iter().cloned().fold(1.0f64, f64::max);
        let max_buf = pl.buf.iter().cloned().fold(1.0f64, f64::max);
        w.set_graph_net(ModelRc::new(VecModel::from(pl.net.iter().map(|v| (v / max_net) as f32).collect::<Vec<_>>())));
        w.set_graph_buf(ModelRc::new(VecModel::from(pl.buf.iter().map(|v| (v / max_buf) as f32).collect::<Vec<_>>())));
        let what = if pl.content.local_path().is_some() { "Disk read" } else { "Network" };
        w.set_graph_net_label(format!("{what}: {} (peak {})", fmt_bitrate(net * 8.0), fmt_bitrate(max_net * 8.0)).into());
        w.set_graph_buf_label(format!("Buffered ahead: {ahead:.1} s").into());
    }
}

fn save_later(p: &Rc<RefCell<Player>>, shared: &Arc<Shared>) {
    let pl = p.borrow();
    let w = pl.window.as_weak();
    let shared = shared.clone();
    pl.save_timer.start(slint::TimerMode::SingleShot, Duration::from_millis(600), move || {
        let Some(w) = w.upgrade() else { return };
        let mut cfg = shared.config.lock().unwrap();
        cfg.player_volume = w.get_volume() as f64;
        cfg.player_pitch_correction = w.get_pitch_correction();
        cfg.player_hwdec = w.get_hwdec();
        cfg.player_stats = w.get_stats_level().clamp(0, 3) as u8;
        let _ = cfg.save();
    });
}

fn wire(shared: &Arc<Shared>, p: &Rc<RefCell<Player>>) {
    let w = p.borrow().window.clone_strong();
    let mpv = p.borrow().mpv.clone();
    let weak = Rc::downgrade(p);
    let with = move |f: &dyn Fn(&Rc<RefCell<Player>>)| {
        if let Some(p) = weak.upgrade() {
            f(&p);
        }
    };
    {
        let mpv = mpv.clone();
        w.on_toggle_pause(move || {
            let _ = mpv.command(&["cycle", "pause"]);
        });
    }
    {
        let mpv = mpv.clone();
        w.on_seek_to(move |t| {
            mpv.command_async(0, &["seek", &format!("{:.3}", t.max(0.0)), "absolute"]);
        });
    }
    {
        let (mpv, with) = (mpv.clone(), with.clone());
        w.on_seek_by(move |d| {
            mpv.command_async(0, &["seek", &format!("{d}"), "relative"]);
            with(&|p| osd(p, format!("{}{} s", if d > 0.0 { "+" } else { "" }, d)));
        });
    }
    {
        let (mpv, with, shared) = (mpv.clone(), with.clone(), shared.clone());
        w.on_set_volume(move |v| {
            let v = v.clamp(0.0, 200.0).round();
            let _ = mpv.set_f64("volume", v as f64);
            let _ = mpv.set_flag("mute", false);
            with(&|p| {
                p.borrow().window.set_volume(v);
                osd(p, format!("Volume {v} %"));
                save_later(p, &shared);
            });
        });
    }
    {
        let mpv = mpv.clone();
        w.on_toggle_mute(move || {
            let _ = mpv.command(&["cycle", "mute"]);
        });
    }
    {
        let (mpv, with) = (mpv.clone(), with.clone());
        w.on_set_speed(move |s| {
            let s = (s.clamp(0.1, 8.0) * 100.0).round() / 100.0;
            let _ = mpv.set_f64("speed", s as f64);
            with(&|p| osd(p, format!("Speed {s}×")));
        });
    }
    {
        let (mpv, with) = (mpv.clone(), with.clone());
        w.on_custom_speed_accepted(move |text| {
            let v = text.trim().replace(',', ".").trim_end_matches(['x', '×']).parse::<f64>().ok().filter(|v| v.is_finite());
            with(&|p| {
                let pl = p.borrow();
                match v {
                    Some(v) if (0.1..=8.0).contains(&v) => {
                        let _ = mpv.set_f64("speed", (v * 100.0).round() / 100.0);
                        pl.window.set_custom_speed("".into());
                    }
                    _ => toast(&pl.window, "Enter a speed between 0.1 and 8"),
                }
            });
        });
    }
    {
        let (mpv, with, shared) = (mpv.clone(), with.clone(), shared.clone());
        w.on_toggle_pitch(move || {
            let on = !mpv.get_flag("audio-pitch-correction").unwrap_or(true);
            let _ = mpv.set_flag("audio-pitch-correction", on);
            with(&|p| {
                p.borrow().window.set_pitch_correction(on);
                save_later(p, &shared);
            });
        });
    }
    {
        let (mpv, with, shared) = (mpv.clone(), with.clone(), shared.clone());
        w.on_toggle_hwdec(move || {
            with(&|p| {
                let on = !p.borrow().window.get_hwdec();
                let _ = mpv.set_str("hwdec", if on { "auto-safe" } else { "no" });
                p.borrow().window.set_hwdec(on);
                osd(p, format!("Hardware decoding {}", if on { "on" } else { "off" }));
                save_later(p, &shared);
            });
        });
    }
    {
        let mpv = mpv.clone();
        w.on_pick_sub(move |id| {
            let _ = mpv.set_str("sid", &if id <= 0 { "no".to_owned() } else { id.to_string() });
        });
    }
    {
        let mpv = mpv.clone();
        w.on_pick_audio(move |id| {
            let _ = mpv.set_str("aid", &id.to_string());
        });
    }
    {
        let (mpv, with) = (mpv.clone(), with.clone());
        w.on_cycle_sub(move || {
            let _ = mpv.command(&["cycle", "sub"]);
            let label = match mpv.get_str("sid").as_deref() {
                Some("no") | None => "Subtitles off".to_owned(),
                Some(_) => format!("Subtitles: {}", language_name(&mpv.get_str("current-tracks/sub/lang").unwrap_or_default())),
            };
            with(&|p| osd(p, label.clone()));
        });
    }
    {
        let (mpv, with) = (mpv.clone(), with.clone());
        w.on_cycle_audio(move || {
            let _ = mpv.command(&["cycle", "audio"]);
            let lang = language_name(&mpv.get_str("current-tracks/audio/lang").unwrap_or_default());
            let id = mpv.get_str("aid").unwrap_or_default();
            with(&|p| osd(p, if lang.is_empty() { format!("Audio track {id}") } else { format!("Audio: {lang}") }));
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_cycle_stats(move || {
            with(&|p| {
                {
                    let pl = p.borrow();
                    let next = (pl.window.get_stats_level() + 1) % 4;
                    pl.window.set_stats_level(next);
                }
                stats_schedule(p);
                save_later(p, &shared);
            });
        });
    }
    {
        let with = with.clone();
        w.on_toggle_fullscreen(move || {
            with(&|p| {
                let w = p.borrow().window.clone_strong();
                let on = !w.window().is_fullscreen();
                w.window().set_fullscreen(on);
                w.set_fullscreen_on(on);
            });
        });
    }
    {
        let mpv = mpv.clone();
        w.on_frame_step(move || {
            let _ = mpv.command(&["frame-step"]);
        });
    }
    {
        let with = with.clone();
        w.on_activity(move || with(&activity));
    }
    {
        let (mpv, with, shared) = (mpv.clone(), with.clone(), shared.clone());
        w.on_volume_answer(move |keep| {
            with(&|p| {
                let w = p.borrow().window.clone_strong();
                w.set_ask_volume(false);
                p.borrow_mut().asking = false;
                if !keep {
                    let _ = mpv.set_f64("volume", 100.0);
                    w.set_volume(100.0);
                    save_later(p, &shared);
                }
                let _ = mpv.set_flag("pause", false);
            });
        });
    }
    {
        let (with, page) = (with.clone(), p.borrow().content.page.clone());
        w.on_copy_link(move || {
            crate::copy_to_clipboard(&page);
            with(&|p| toast(&p.borrow().window, "Link copied"));
        });
    }
    {
        let (shared, content) = (shared.clone(), p.borrow().content.clone());
        w.on_download(move || match content.local_path() {
            Some(path) => crate::files_ui::show_in_folder(&path),
            None => crate::files_ui::save_as(&shared, &content),
        });
    }
    {
        let (shared, page) = (shared.clone(), p.borrow().content.page.clone());
        w.on_open_browser(move || crate::open_external(&page, &shared.config.lock().unwrap()));
    }

    let mut hooks = windows::Hooks::new();
    {
        let with = with.clone();
        hooks.moved = Some(Box::new(move || with(&activity)));
    }
    {
        let with = with.clone();
        hooks.visible = Box::new(move |on| {
            with(&|p| {
                p.borrow_mut().visible = on;
                stats_schedule(p);
            })
        });
    }
    {
        let (shared, w2) = (shared.clone(), w.as_weak());
        hooks.paste_link = Some(crate::open::paste_hook(&shared, move |m| {
            if let Some(w) = w2.upgrade() {
                toast(&w, m);
            }
        }));
    }
    windows::zoomable(&mut hooks, &w, &shared, |c| &mut c.player_zoom);
    {
        let (shared, weak) = (shared.clone(), w.as_weak());
        w.on_zoom_step(move |dir| {
            if let Some(w) = weak.upgrade() {
                windows::zoom_step(w.window(), &shared, |c| &mut c.player_zoom, dir);
            }
        });
    }
    windows::attach(w.window(), hooks);
    {
        let weak = Rc::downgrade(p);
        w.window().on_close_requested(move || {
            if let Some(p) = weak.upgrade() {
                {
                    let mut pl = p.borrow_mut();
                    // stop decoding and downloading right away; mpv itself goes away with the window
                    let _ = pl.mpv.command(&["stop"]);
                    pl.stats_timer = None;
                }
                let id = p.borrow().id;
                PLAYERS.with(|l| l.borrow_mut().retain(|x| x.borrow().id != id));
            }
            windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
}
