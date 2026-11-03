//! Audio player window (stage 4): TVFS audio links and local files, played by `nativefs-audio`
//! (Symphonia decoding, WASAPI exclusive / shared output). Streams TVFS files in `Range` pieces.
//! The window redraws continuously only while playing and visible (spinning disc); paused or
//! hidden it redraws nothing.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nativefs_audio::decode::{Decoder, SourceFormat};
use nativefs_audio::engine::{Event, OutputSettings, OutputStatus, PauseBehavior, Player};
use nativefs_audio::output::{DeviceInfo, Fidelity, ShareMode};
use nativefs_audio::remote::{Fetched, FetchFn, RemoteHandle, RemoteSource};
use slint::{ComponentHandle, ModelRc, VecModel};
use tvfs_core::content::{self, Content};

use crate::{windows, AudioWindow, BufferedRange, OutputDevice, Shared};

/// Loudness normalization target of the web player (Spotify / YouTube level).
const TARGET_LUFS: f64 = -14.0;

/// The web's BPM -> title font ladder (`audioPlayer.js`): < 70 BPM the oldest, > 160 the newest.
const BPM_FONTS: [&str; 10] =
    ["EB Garamond", "PT Serif", "Merriweather", "Lora", "Quicksand", "Poppins", "Plus Jakarta Sans", "Manrope", "Sora", "Space Grotesk"];

fn font_for_bpm(bpm: Option<f64>) -> &'static str {
    let Some(bpm) = bpm.filter(|b| b.is_finite() && *b > 0.0) else { return BPM_FONTS[4] };
    let (lo, hi) = (70.0, 160.0);
    if bpm <= lo {
        return BPM_FONTS[0];
    }
    if bpm >= hi {
        return BPM_FONTS[9];
    }
    let idx = (((bpm - lo) / (hi - lo)) * BPM_FONTS.len() as f64).floor() as usize;
    BPM_FONTS[idx.min(BPM_FONTS.len() - 1)]
}

/// "FLAC · Stereo · 811kbps · 16bit · 44.1kHz · 123 BPM" (the web's `formatMetaSummary`).
fn meta_summary(src: &SourceFormat, kbps: Option<u64>, bpm: Option<f64>) -> String {
    let ch = match src.channels {
        1 => "Mono".to_string(),
        2 => "Stereo".to_string(),
        n => format!("{n}ch"),
    };
    let mut parts = vec![src.codec.clone(), ch];
    if let Some(k) = kbps {
        parts.push(format!("{k}kbps"));
    }
    if let Some(b) = src.bits {
        parts.push(format!("{b}bit"));
    }
    parts.push(format!("{:.1}kHz", src.rate as f64 / 1000.0));
    if let Some(b) = bpm {
        parts.push(format!("{} BPM", b.round()));
    }
    parts.join(" · ")
}

/// Normalization gain: tracks louder than the target are turned down to it, quieter ones are left alone.
fn norm_db_for(lufs: Option<f64>) -> f64 {
    match lufs {
        Some(v) if v > TARGET_LUFS => TARGET_LUFS - v,
        _ => 0.0,
    }
}

fn fmt_time(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

enum Source {
    Remote(Content),
    Local(PathBuf),
}

impl Source {
    fn key(&self) -> String {
        match self {
            Source::Remote(c) => c.url.clone(),
            Source::Local(p) => p.to_string_lossy().into_owned(),
        }
    }
    fn name(&self) -> String {
        match self {
            Source::Remote(c) => c.name.clone(),
            Source::Local(p) => p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        }
    }
}

struct Loaded {
    player: Player,
    remote: Option<RemoteHandle>,
    bpm: Option<f64>,
    lufs: Option<f64>,
    bytes: Option<u64>,
}

struct State {
    window: AudioWindow,
    shared: Arc<Shared>,
    source: Source,
    player: Option<Player>,
    remote: Option<RemoteHandle>,
    rate: f64,
    total: f64,
    muted: bool,
    /// Normalization gain in dB (0 when the track is not louder than the target).
    norm_db: f64,
    output: Option<OutputStatus>,
    devices: Vec<DeviceInfo>,
    visible: bool,
    timer: Option<slint::Timer>,
    disc: f64,
    last_frame: Instant,
    last_buffered: Instant,
    /// Autoplay waits for a click (device without hardware volume, see `loud_device`).
    hold: bool,
    /// Sound controls (knobs, EQ) as the user set them; normalization and Bit-perfect mode are
    /// applied on top in `apply_gain`.
    params: nativefs_audio::dsp::Params,
    rack: crate::audio_rack::Rack,
    /// Preset glide (520 ms) and the delayed config save of the volume.
    preset_timer: Option<slint::Timer>,
    save_timer: Option<slint::Timer>,
    eq: crate::audio_eq::Eq,
    eq_timer: Option<slint::Timer>,
    spectrum: nativefs_audio::analysis::Spectrum,
    spec_view: crate::audio_eq::SpectrumView,
    spec_timer: Option<slint::Timer>,
    spec_last: Instant,
    /// Visualizer: the music analysis (also drives the disc pulse) and the particle field.
    analysis: Option<nativefs_audio::vis::Analysis>,
    field: Option<nativefs_audio::vis::Field>,
    /// Pointer in physical px (pushes particles aside).
    cursor: Option<(f32, f32)>,
    /// The system media overlay / media keys (Windows SMTC).
    media: Option<crate::media_keys::Session>,
}

thread_local! {
    static PLAYERS: RefCell<Vec<Rc<RefCell<State>>>> = const { RefCell::new(Vec::new()) };
}

fn find(w: &AudioWindow) -> Option<Rc<RefCell<State>>> {
    PLAYERS.with(|l| l.borrow().iter().find(|s| std::ptr::eq(s.borrow().window.window(), w.window())).cloned())
}

/// Open a TVFS audio link.
pub fn open(shared: &Arc<Shared>, content: Content) {
    open_source(shared, Source::Remote(content));
}

/// Open a local audio file ("Use TVFS tools locally").
pub fn open_local(shared: &Arc<Shared>, path: PathBuf) {
    open_source(shared, Source::Local(path));
}

fn open_source(shared: &Arc<Shared>, source: Source) {
    let key = source.key();
    if let Some(s) = PLAYERS.with(|l| l.borrow().iter().find(|s| s.borrow().source.key() == key).cloned()) {
        windows::raise(s.borrow().window.window());
        return;
    }
    let Ok(w) = AudioWindow::new() else { return };
    let name = source.name();
    let stem = std::path::Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name.clone());
    w.set_file_name(name.clone().into());
    w.set_track_title(stem.into());
    w.set_meta("Loading…".into());
    w.set_local(matches!(source, Source::Local(_)));
    {
        let cfg = shared.config.lock().unwrap();
        w.set_looping(cfg.audio_loop);
        w.set_vis_on(cfg.audio_visualizer);
        w.set_bit_perfect_mode(cfg.audio_bit_perfect);
        w.set_norm_on(cfg.audio_normalize);
        w.set_exclusive(cfg.audio_exclusive);
        w.set_release_on_pause(cfg.audio_release_on_pause);
    }
    let state = Rc::new(RefCell::new(State {
        window: w.clone_strong(),
        shared: shared.clone(),
        source,
        player: None,
        remote: None,
        rate: 44100.0,
        total: 0.0,
        muted: false,
        norm_db: 0.0,
        output: None,
        devices: Vec::new(),
        visible: true,
        timer: None,
        disc: 0.0,
        last_frame: Instant::now(),
        last_buffered: Instant::now(),
        hold: false,
        params: nativefs_audio::dsp::Params::default(),
        rack: crate::audio_rack::Rack::new(None, shared.config.lock().unwrap().audio_volume),
        preset_timer: None,
        save_timer: None,
        eq: crate::audio_eq::Eq::new(44100.0),
        eq_timer: None,
        spectrum: nativefs_audio::analysis::Spectrum::new(),
        spec_view: crate::audio_eq::SpectrumView::new(),
        spec_timer: None,
        spec_last: Instant::now(),
        analysis: None,
        field: None,
        cursor: None,
        media: None,
    }));
    crate::audio_eq::show_static(&w);
    w.set_presets_open(shared.config.lock().unwrap().audio_presets_open);
    show_rack(&state);
    PLAYERS.with(|l| l.borrow_mut().push(state.clone()));
    refresh_devices(&state);
    update_fidelity(&state);
    sync_hw_volume(&state);
    wire(&w, &state);

    let mut hooks = windows::Hooks::new();
    {
        let sw = Rc::downgrade(&state);
        hooks.visible = Box::new(move |on| {
            if let Some(s) = sw.upgrade() {
                s.borrow_mut().visible = on;
                schedule(&s);
                spectrum_schedule(&s);
            }
        });
    }
    {
        let sw = Rc::downgrade(&state);
        hooks.cursor = Some(Box::new(move |c| {
            if let Some(s) = sw.upgrade() {
                s.borrow_mut().cursor = c;
            }
        }));
    }
    {
        let weak = w.as_weak();
        hooks.paste_link = Some(crate::open::paste_hook(shared, move |m| {
            let weak = weak.clone();
            windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, m);
        }));
    }
    {
        let weak = w.as_weak();
        hooks.wheel = Box::new(move |dy, smooth| {
            if let Some(w) = weak.upgrade() {
                w.set_wheel_dy(dy);
                w.set_wheel_smooth(smooth);
            }
        });
    }
    w.set_smooth_scroll(shared.config.lock().unwrap().smooth_scroll);
    {
        let sw = Rc::downgrade(&state);
        hooks.resized = Some(Box::new(move |pw, ph| {
            if let Some(s) = sw.upgrade() {
                visualize_at(&s, Some((pw, ph)));
            }
        }));
    }
    windows::zoomable(&mut hooks, &w, shared, |c| &mut c.audio_zoom);
    windows::attach(w.window(), hooks);
    {
        let sw = Rc::downgrade(&state);
        w.window().on_close_requested(move || {
            if let Some(s) = sw.upgrade() {
                {
                    let mut sb = s.borrow_mut();
                    sb.timer = None;
                    sb.spec_timer = None;
                    // dropping the player stops playback and releases the device
                    sb.player = None;
                }
                PLAYERS.with(|l| l.borrow_mut().retain(|x| !Rc::ptr_eq(x, &s)));
                tray_refresh();
            }
            windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
    windows::place(w.window());
    let _ = w.show();
    // media keys and the system media overlay (needs the native window, so after show)
    {
        let weak = w.as_weak();
        let session = crate::media_keys::Session::new(
            w.window(),
            Box::new(move |b| {
                let weak = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(s) = weak.upgrade().and_then(|w| find(&w)) {
                        media_button(&s, b);
                    }
                });
            }),
        );
        state.borrow_mut().media = session;
        media_update(&state);
    }
    // debug builds: `NATIVEFS_AUDIO_EQ=1` opens the EQ panel right away (screenshots without clicking)
    // (`NATIVEFS_AUDIO_EQ=<n>` also applies EQ preset n)
    if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_EQ").is_some() {
        if let Some(n) = std::env::var("NATIVEFS_AUDIO_EQ").ok().and_then(|v| v.parse::<usize>().ok()).filter(|n| *n < crate::audio_eq::PRESETS.len()) {
            state.borrow_mut().eq.bands = crate::audio_eq::preset_bands(n);
            state.borrow_mut().eq.compute();
        }
        w.invoke_open_eq();
    }
    load(&state);
}

/// Opens the source and the decoder off the UI thread, then hands the player to the window.
fn load(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let shared = sb.shared.clone();
    let weak = sb.window.as_weak();
    let settings = output_settings(&shared);
    let source = match &sb.source {
        Source::Remote(c) => Err(c.clone()),
        Source::Local(p) => Ok(p.clone()),
    };
    let events = {
        let weak = weak.clone();
        Box::new(move |e: Event| {
            let _ = weak.upgrade_in_event_loop(move |w| {
                if let Some(s) = find(&w) {
                    on_event(&s, e);
                }
            });
        })
    };
    std::thread::Builder::new()
        .name("audio-open".into())
        .spawn(move || {
            let result: Result<Loaded, String> = (|| match source {
                Ok(path) => {
                    let bytes = std::fs::metadata(&path).ok().map(|m| m.len());
                    let dec = Decoder::open_path(&path).map_err(|e| e.to_string())?;
                    Ok(Loaded { player: Player::new(dec, settings, events), remote: None, bpm: None, lufs: None, bytes })
                }
                Err(c) => {
                    // the player page carries the server's BPM and loudness
                    let (mut bpm, mut lufs) = (None, None);
                    if c.page != c.url && c.page.to_ascii_lowercase().ends_with(".html") {
                        if let Ok(html) = shared.rt.block_on(shared.client.fetch_bytes(&c.page, 8 * 1024 * 1024)) {
                            if let Some(d) = content::parse_audio_data(&String::from_utf8_lossy(&html)) {
                                (bpm, lufs) = (d.bpm, d.lufs);
                            }
                        }
                    }
                    let (client, rt, url) = (shared.client.clone(), shared.rt.clone(), c.url.clone());
                    let fetch: FetchFn = Arc::new(move |start, len| {
                        let r = rt.block_on(client.fetch_range(&url, start, len)).map_err(|e| crate::open::describe(&e))?;
                        Ok(Fetched { offset: r.offset, total: r.total, data: r.data.to_vec() })
                    });
                    let (src, handle) = RemoteSource::open(fetch)?;
                    let bytes = Some(src_len(&handle));
                    let ext = std::path::Path::new(&c.name).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
                    let dec = Decoder::open(Box::new(src), ext.as_deref()).map_err(|e| e.to_string())?;
                    Ok(Loaded { player: Player::new(dec, settings, events), remote: Some(handle), bpm, lufs, bytes })
                }
            })();
            let _ = weak.upgrade_in_event_loop(move |w| {
                if let Some(s) = find(&w) {
                    loaded(&s, result);
                }
            });
        })
        .ok();
}

fn src_len(h: &RemoteHandle) -> u64 {
    h.progress().total
}

/// One player at a time: starting one pauses the others and releases their device (exclusive mode lets only one
/// app or window play; without this the second player stayed silent).
/// Returns whether another player had the device (then it needs a moment to let go of it).
fn pause_others(state: &Rc<RefCell<State>>) -> bool {
    let others: Vec<_> = PLAYERS.with(|l| l.borrow().iter().filter(|x| !Rc::ptr_eq(x, state)).cloned().collect());
    let mut held = false;
    for o in others {
        let ob = o.borrow();
        if let Some(p) = &ob.player {
            held |= ob.output.is_some();
            p.pause();
            // re-sending the output settings to a paused player closes its stream: the device is free
            p.set_output(output_settings(&ob.shared));
        }
    }
    held
}

/// Play `p` (the player of `state`); other players pause first. When one of them held the device, playback starts
/// 150 ms later, after its audio thread closed the stream (otherwise exclusive mode would fail and fall back to shared).
fn start(state: &Rc<RefCell<State>>, p: &Player) {
    if !pause_others(state) {
        p.play();
        return;
    }
    let sw = Rc::downgrade(state);
    slint::Timer::single_shot(Duration::from_millis(150), move || {
        if let Some(s) = sw.upgrade() {
            if let Some(p) = &s.borrow().player {
                p.play();
            }
        }
    });
}

fn output_settings(shared: &Shared) -> OutputSettings {
    let cfg = shared.config.lock().unwrap();
    OutputSettings {
        device: (!cfg.audio_device.is_empty()).then(|| cfg.audio_device.clone()),
        mode: if cfg.audio_exclusive { ShareMode::Exclusive } else { ShareMode::Shared },
        pause: if cfg.audio_release_on_pause { PauseBehavior::Release } else { PauseBehavior::Keep },
    }
}

fn loaded(state: &Rc<RefCell<State>>, result: Result<Loaded, String>) {
    let w = state.borrow().window.clone_strong();
    let l = match result {
        Ok(l) => l,
        Err(e) => {
            w.set_meta("".into());
            w.set_status(format!("Cannot play this file: {e}").into());
            w.set_status_error(true);
            return;
        }
    };
    let info = l.player.info().clone();
    let rate = info.rate as f64;
    let total = l.player.duration_frames().unwrap_or(0) as f64 / rate;
    let kbps = l.bytes.filter(|_| total > 0.0).map(|b| (b as f64 * 8.0 / total / 1000.0).round() as u64);
    w.set_meta(meta_summary(&info, kbps, l.bpm).into());
    w.set_track_font(font_for_bpm(l.bpm).into());
    if l.bpm.is_some() {
        let volume = state.borrow().rack.volume();
        state.borrow_mut().rack = crate::audio_rack::Rack::new(l.bpm, volume);
        show_rack(state);
    }
    let norm_db = norm_db_for(l.lufs);
    w.set_norm_available(norm_db < -0.1);
    // local files: the server never measured them, so measure the loudness here (off the UI thread,
    // playback does not wait for it)
    if l.lufs.is_none() {
        if let Source::Local(path) = &state.borrow().source {
            let (path, weak) = (path.clone(), w.as_weak());
            std::thread::Builder::new()
                .name("audio-loudness".into())
                .spawn(move || {
                    let lufs = Decoder::open_path(&path).ok().and_then(|mut d| nativefs_audio::analysis::integrated_loudness(&mut d));
                    let _ = weak.upgrade_in_event_loop(move |w| {
                        if let Some(s) = find(&w) {
                            let db = norm_db_for(lufs);
                            s.borrow_mut().norm_db = db;
                            w.set_norm_available(db < -0.1);
                            apply_gain(&s);
                            update_fidelity(&s);
                        }
                    });
                })
                .ok();
        }
    }
    w.set_remaining(format!("-{}", fmt_time(total)).into());
    w.set_ready(true);
    media_update(state);
    // first run: the tour starts once a track is open (web: `maybeStartTour` in `setSource`)
    let tour_done = state.borrow().shared.config.lock().unwrap().audio_tour_done;
    if !tour_done && w.get_tour_step() < 0 {
        w.invoke_tour_show(0);
    }
    // debug builds: `NATIVEFS_AUDIO_TOUR=<step>` opens the tour at that step (screenshots without clicking)
    if let Some(n) = std::env::var("NATIVEFS_AUDIO_TOUR").ok().and_then(|v| v.parse::<i32>().ok()).filter(|_| cfg!(debug_assertions)) {
        w.invoke_tour_show(n);
    }
    {
        let mut sb = state.borrow_mut();
        l.player.set_loop(w.get_looping());
        sb.player = Some(l.player);
        sb.remote = l.remote;
        sb.rate = rate;
        sb.total = total;
        sb.eq.set_rate(rate);
        sb.analysis = Some(nativefs_audio::vis::Analysis::new(rate as u32));
        sb.norm_db = norm_db;
        // debug builds: `NATIVEFS_AUDIO_MUTE=1` plays muted (silent tests of the moving parts). Set in the player's own
        // state, which apply_gain sends on: muting only the window and the engine made the button look muted while
        // the next gain update (any knob, the first M) played out loud
        if cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_MUTE").is_some() {
            sb.muted = true;
            w.set_muted(true);
        }
    }
    apply_gain(state);
    // the field is there before the music starts (like the web), so Play only brightens it
    visualize(state);
    // a device without hardware volume plays at full scale in exclusive mode: never start it by itself
    if loud_device(state) {
        state.borrow_mut().hold = true;
        w.set_status("This device has no hardware volume. In exclusive mode Windows' volume slider does not apply, so it plays at the level the player sends (up to full scale): set the volume on the device (TV, DAC, amplifier) and press play.".into());
        w.set_status_error(false);
    } else if let Some(p) = &state.borrow().player {
        // debug builds: `NATIVEFS_AUDIO_NOPLAY=1` opens without starting playback (silent UI tests)
        if !(cfg!(debug_assertions) && std::env::var_os("NATIVEFS_AUDIO_NOPLAY").is_some()) {
            start(state, p);
        }
    }
    update_fidelity(state);
}

/// The selected device has no hardware volume and exclusive mode is on.
fn loud_device(state: &Rc<RefCell<State>>) -> bool {
    let sb = state.borrow();
    let cfg = sb.shared.config.lock().unwrap();
    if !cfg.audio_exclusive {
        return false;
    }
    let dev = sb.devices.iter().find(|d| if cfg.audio_device.is_empty() { d.default } else { d.id == cfg.audio_device });
    dev.is_some_and(|d| !d.hw_volume && !d.bluetooth)
}

fn on_event(state: &Rc<RefCell<State>>, e: Event) {
    let w = state.borrow().window.clone_strong();
    match e {
        Event::Output(o) => {
            state.borrow_mut().output = Some(o);
            update_fidelity(state);
            sync_hw_volume(state);
        }
        Event::Released => {
            state.borrow_mut().output = None;
            update_fidelity(state);
        }
        Event::Playing(on) => {
            w.set_playing(on);
            media_update(state);
            if on {
                w.set_status("".into());
                state.borrow_mut().hold = false;
            }
            state.borrow_mut().last_frame = Instant::now();
            schedule(state);
            spectrum_schedule(state);
            tick(state);
        }
        Event::Ended => {
            w.set_playing(false);
            tick(state);
            media_update(state);
        }
        Event::Error(m) => {
            w.set_status(m.into());
            w.set_status_error(true);
        }
    }
}

/// The frame timer runs only while playing and visible (disc + time); otherwise nothing redraws.
fn schedule(state: &Rc<RefCell<State>>) {
    let run = {
        let sb = state.borrow();
        let settling = sb.analysis.as_ref().is_some_and(|a| !a.settled());
        sb.visible && (sb.player.as_ref().is_some_and(Player::is_playing) || settling)
    };
    if !run {
        state.borrow_mut().timer = None;
        return;
    }
    if state.borrow().timer.is_some() {
        return;
    }
    let sw = Rc::downgrade(state);
    let timer = slint::Timer::default();
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        if let Some(s) = sw.upgrade() {
            tick(&s);
            visualize(&s);
            // paused and faded out: stop redrawing
            let done = {
                let sb = s.borrow();
                !sb.player.as_ref().is_some_and(Player::is_playing) && sb.analysis.as_ref().is_none_or(|a| a.settled())
            };
            if done {
                s.borrow_mut().timer = None;
            }
        }
    });
    state.borrow_mut().timer = Some(timer);
}

fn tick(state: &Rc<RefCell<State>>) {
    let mut sb = state.borrow_mut();
    let Some(p) = &sb.player else { return };
    let pos = p.position() as f64 / sb.rate;
    let playing = p.is_playing();
    let total = sb.total;
    let w = sb.window.clone_strong();
    w.set_progress(if total > 0.0 { (pos / total) as f32 } else { 0.0 });
    w.set_elapsed(fmt_time(pos).into());
    w.set_remaining(format!("-{}", fmt_time(total - pos)).into());
    let now = Instant::now();
    if playing {
        let dt = now.duration_since(sb.last_frame).as_secs_f64().min(0.05);
        sb.disc = (sb.disc + dt * 60.0) % 360.0;
        w.set_disc_angle(sb.disc as f32);
    }
    sb.last_frame = now;
    if now.duration_since(sb.last_buffered) > Duration::from_millis(500) {
        sb.last_buffered = now;
        if let Some(r) = &sb.remote {
            let pr = r.progress();
            if pr.total > 0 {
                let ranges: Vec<BufferedRange> =
                    pr.ranges.iter().map(|(a, b)| BufferedRange { start: *a as f32 / pr.total as f32, end: *b as f32 / pr.total as f32 }).collect();
                w.set_buffered(ModelRc::new(VecModel::from(ranges)));
            }
        }
    }
}

/// What the engine should do with the samples: the sound controls plus normalization, or nothing
/// at all in Bit-perfect mode.
fn effective_params(sb: &State) -> nativefs_audio::dsp::Params {
    let w = &sb.window;
    if w.get_bit_perfect_mode() {
        return nativefs_audio::dsp::Params::default();
    }
    let mut p = sb.params.clone();
    sb.rack.params(&mut p);
    p.eq = sb.eq.bands;
    p.eq_bypass = sb.eq.bypass;
    p.norm_db = if w.get_norm_on() { sb.norm_db } else { 0.0 };
    p
}

/// One visualizer frame: analyse what the device plays, pulse the disc, draw the particle field
/// (the web's `frame`). Runs from the frame timer only (playing, or settling after a pause).
fn visualize(state: &Rc<RefCell<State>>) {
    visualize_at(state, None);
}

/// `size`: the window's new physical size during a resize (`window().size()` still has the old one then).
fn visualize_at(state: &Rc<RefCell<State>>, size: Option<(u32, u32)>) {
    let mut sb = state.borrow_mut();
    let sb = &mut *sb;
    let Some(a) = sb.analysis.as_mut() else { return };
    let playing = sb.player.as_ref().is_some_and(Player::is_playing);
    let samples = if playing { sb.player.as_ref().map(|p| p.scope(16384)) } else { None };
    let tempo = sb.rack.values[2] as f32;
    a.update(samples.as_deref(), 1.0 / 60.0, sb.rack.bpm, tempo);
    let w = &sb.window;
    w.set_disc_scale(a.disc_scale());
    if !w.get_vis_on() {
        sb.field = None;
        return;
    }
    let (sw, sh) = size.unwrap_or_else(|| {
        let s = w.window().size();
        (s.width, s.height)
    });
    let scale = w.window().scale_factor();
    let (pw, ph) = (sw as usize, sh as usize);
    if pw == 0 || ph == 0 {
        return;
    }
    if sb.field.as_ref().is_none_or(|f| f.width != pw || f.height != ph) {
        // centre on the player card like the web (narrow windows: the middle of the window)
        let (lw, lh) = (pw as f32 / scale, ph as f32 / scale);
        let shell = lw.min(1080.0);
        let (cx, cy) = if lw <= 900.0 { (lw / 2.0, (lh * 0.32).min(82.0 + 120.0)) } else { ((lw - shell) / 2.0 + 20.0 + 200.0, lh * 0.45) };
        let mut field = nativefs_audio::vis::Field::new(pw, ph, scale, cx, cy);
        if let Some(old) = &sb.field {
            field.carry_from(old);
        }
        sb.field = Some(field);
    }
    let Some(f) = sb.field.as_mut() else { return };
    f.draw(a, 1.0 / 60.0, scale, sb.cursor);
    let buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(&f.pixels, pw as u32, ph as u32);
    w.set_vis_frame(slint::Image::from_rgba8_premultiplied(buf));
}

fn apply_gain(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let Some(p) = &sb.player else { return };
    p.set_muted(sb.muted && !sb.window.get_bit_perfect_mode());
    p.set_dsp(effective_params(&sb));
}

fn format_line(o: &OutputStatus) -> String {
    let f = &o.format;
    let bits = if f.float {
        "32-bit float".to_string()
    } else if f.valid_bits as usize == f.container_bytes as usize * 8 {
        format!("{} bit", f.valid_bits)
    } else {
        format!("{} bit in {}", f.valid_bits, f.container_bytes * 8)
    };
    let mode = match o.mode {
        ShareMode::Exclusive => "Exclusive",
        ShareMode::Shared => "Shared",
    };
    format!("{} · {:.1} kHz · {bits} · {mode}", o.device.name, f.rate as f64 / 1000.0)
}

/// The indicator under the chips: what reaches the device and why it is (not) bit-perfect.
fn update_fidelity(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let w = &sb.window;
    let bit_perfect_mode = w.get_bit_perfect_mode();
    let norm = !bit_perfect_mode && w.get_norm_on() && sb.norm_db < -0.05;
    let Some(o) = &sb.output else {
        let cfg = sb.shared.config.lock().unwrap();
        let dev = sb.devices.iter().find(|d| if cfg.audio_device.is_empty() { d.default } else { d.id == cfg.audio_device });
        w.set_fidelity(0);
        w.set_fidelity_text(if bit_perfect_mode { "Bit-perfect mode · not playing".into() } else { "Output · not playing".into() });
        let mode = if cfg.audio_exclusive { "Exclusive" } else { "Shared" };
        w.set_output_line(format!("{} · {mode}", dev.map(|d| d.name.as_str()).unwrap_or("System default")).into());
        return;
    };
    let eff = effective_params(&sb);
    let mut reasons = Vec::new();
    if norm {
        reasons.push(format!("normalization {:.1} dB", sb.norm_db));
    }
    if !(nativefs_audio::dsp::Params { norm_db: 0.0, ..eff }).neutral() {
        reasons.push("sound effects".to_string());
    }
    let dsp = (!reasons.is_empty()).then(|| reasons.join(", "));
    let (level, text) = match (&o.fidelity, dsp) {
        (_, Some(d)) if !matches!(o.fidelity, Fidelity::NotBitPerfect(_)) => (3, format!("Not bit-perfect · {d}")),
        (Fidelity::BitPerfect, _) => (1, "BIT-PERFECT".to_string()),
        (Fidelity::LossySource, _) => (2, "Lossy file · decoded samples sent unchanged".to_string()),
        (Fidelity::NotBitPerfect(r), _) => (3, format!("Not bit-perfect · {r}")),
    };
    let text = match &o.fallback {
        Some(f) => format!("{text} (exclusive failed: {f})"),
        None => text,
    };
    w.set_fidelity(level);
    w.set_fidelity_text(text.into());
    w.set_output_line(format_line(o).into());
}

/// The endpoint the sound goes to (or would go to): the open stream's, else the selected one.
fn current_device(sb: &State) -> Option<DeviceInfo> {
    if let Some(o) = &sb.output {
        return Some(o.device.clone());
    }
    let id = sb.shared.config.lock().unwrap().audio_device.clone();
    sb.devices.iter().find(|d| if id.is_empty() { d.default } else { d.id == id }).cloned()
}

/// Bit-perfect mode: the Volume knob becomes the device's hardware volume when it has one.
fn sync_hw_volume(state: &Rc<RefCell<State>>) {
    let (bp, dev) = {
        let sb = state.borrow();
        (sb.window.get_bit_perfect_mode(), current_device(&sb))
    };
    let hw = dev.as_ref().is_some_and(|d| d.hw_volume);
    state.borrow().window.set_hw_volume(hw);
    #[cfg(windows)]
    let level = if bp && hw { nativefs_audio::output::sys::endpoint_volume(dev.as_ref().map(|d| d.id.as_str())).map(|v| v as f64) } else { None };
    #[cfg(not(windows))]
    let level: Option<f64> = None;
    state.borrow_mut().rack.set_hw_volume(level);
}

/// Bit-perfect mode on a device with a hardware volume: the Volume knob moved to `n` (0..1).
/// Returns false when the knob is the app's Volume.
fn hw_volume_knob(state: &Rc<RefCell<State>>, n: f64) -> bool {
    if state.borrow().rack.hw_volume.is_none() {
        return false;
    }
    let n = n.clamp(0.0, 1.0);
    let id = current_device(&state.borrow()).map(|d| d.id);
    #[cfg(windows)]
    nativefs_audio::output::sys::set_endpoint_volume(id.as_deref(), n as f32);
    let _ = id;
    state.borrow_mut().rack.set_hw_volume(Some(n));
    true
}

fn refresh_devices(state: &Rc<RefCell<State>>) {
    #[cfg(windows)]
    let list = nativefs_audio::output::sys::devices().unwrap_or_default();
    #[cfg(not(windows))]
    let list: Vec<DeviceInfo> = Vec::new();
    let mut sb = state.borrow_mut();
    let selected = sb.shared.config.lock().unwrap().audio_device.clone();
    let default_name = list.iter().find(|d| d.default).map(|d| d.name.clone()).unwrap_or_default();
    let mut rows = vec![OutputDevice {
        name: "System default".into(),
        note: default_name.into(),
        selected: selected.is_empty(),
    }];
    for d in &list {
        let mut notes = Vec::new();
        if d.bluetooth {
            notes.push("Bluetooth: never bit-perfect");
        }
        notes.push(if d.hw_volume { "hardware volume" } else { "no hardware volume" });
        rows.push(OutputDevice { name: d.name.clone().into(), note: notes.join(" · ").into(), selected: d.id == selected });
    }
    sb.window.set_devices(ModelRc::new(VecModel::from(rows)));
    sb.devices = list;
}

fn save_config(state: &Rc<RefCell<State>>, f: impl FnOnce(&mut tvfs_core::config::Config)) {
    let shared = state.borrow().shared.clone();
    let mut cfg = shared.config.lock().unwrap();
    f(&mut cfg);
    let _ = cfg.save();
}

fn reapply_output(state: &Rc<RefCell<State>>) {
    let settings = output_settings(&state.borrow().shared);
    if let Some(p) = &state.borrow().player {
        p.set_output(settings);
    }
    update_fidelity(state);
}

/// A media key / overlay button.
fn media_button(state: &Rc<RefCell<State>>, b: crate::media_keys::Button) {
    use crate::media_keys::Button;
    let w = state.borrow().window.clone_strong();
    let playing = state.borrow().player.as_ref().is_some_and(Player::is_playing);
    match b {
        Button::Play if !playing => w.invoke_play_pause(),
        Button::Pause if playing => w.invoke_play_pause(),
        Button::Stop => w.invoke_stop(),
        Button::Seek(secs) => {
            let total = state.borrow().total;
            if total > 0.0 {
                w.invoke_seek((secs / total).clamp(0.0, 1.0) as f32);
            }
        }
        _ => {}
    }
}

/// The player the tray controls: the one that plays, else the newest.
fn tray_player() -> Option<Rc<RefCell<State>>> {
    PLAYERS.with(|l| {
        let l = l.borrow();
        l.iter().find(|s| s.borrow().player.as_ref().is_some_and(Player::is_playing)).or(l.last()).cloned()
    })
}

/// Refresh the tray's media block (title, Play / Pause); no player open = the block disappears.
pub fn tray_refresh() {
    let media = tray_player().map(|s| {
        let sb = s.borrow();
        crate::tray::Media {
            title: sb.window.get_track_title().to_string(),
            playing: sb.player.as_ref().is_some_and(Player::is_playing),
        }
    });
    crate::tray::set_media(media);
}

/// Play / Pause or Stop from the tray.
pub fn tray_action(a: crate::tray::Action) {
    let Some(s) = tray_player() else { return };
    let w = s.borrow().window.clone_strong();
    match a {
        crate::tray::Action::PlayPause => w.invoke_play_pause(),
        crate::tray::Action::Stop => w.invoke_stop(),
        _ => {}
    }
    tray_refresh();
}

/// Tell the system media overlay the title, play state and position.
fn media_update(state: &Rc<RefCell<State>>) {
    tray_refresh();
    let sb = state.borrow();
    let Some(m) = &sb.media else { return };
    m.set_track(&sb.window.get_track_title(), if sb.window.get_local() { "Local file" } else { "TVFS" });
    match &sb.player {
        Some(p) => {
            m.set_playing(Some(p.is_playing()));
            m.set_timeline(p.position() as f64 / sb.rate, sb.total);
        }
        None => m.set_playing(None),
    }
}

fn seek_by(state: &Rc<RefCell<State>>, secs: f64) {
    let sb = state.borrow();
    if let Some(p) = &sb.player {
        let pos = p.position() as f64 / sb.rate + secs;
        p.seek((pos.clamp(0.0, sb.total) * sb.rate) as u64);
    }
    drop(sb);
    tick(state);
    media_update(state);
}

/// Knob rows, preset chips (quick row and the full list).
fn show_rack(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    sb.window.set_knobs(sb.rack.model());
    show_presets(&sb);
}

fn show_presets(sb: &State) {
    sb.window.set_preset_quick(ModelRc::new(VecModel::from(sb.rack.quick_chips())));
    let hidden = sb.rack.active_preset.filter(|i| !crate::audio_controls::FEATURED.contains(&crate::audio_controls::PRESETS[*i].name));
    sb.window.set_preset_more_label(hidden.map_or("More".to_string(), |i| format!("More · {}", crate::audio_controls::PRESETS[i].name)).into());
    sb.window.set_preset_lines(ModelRc::new(VecModel::from(sb.rack.lines())));
}

/// After a knob moved: the sound, the indicator, the chips (only when the active preset changed:
/// rebuilding them on every mouse move made dragging lag); the volume is saved 400 ms later.
fn rack_changed(state: &Rc<RefCell<State>>, preset_before: Option<usize>) {
    apply_gain(state);
    update_fidelity(state);
    if state.borrow().rack.active_preset != preset_before {
        show_presets(&state.borrow());
    }
    let sw = Rc::downgrade(state);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::SingleShot, Duration::from_millis(400), move || {
        if let Some(s) = sw.upgrade() {
            let v = s.borrow().rack.volume().min(100.0);
            save_config(&s, |c| c.audio_volume = v);
        }
    });
    state.borrow_mut().save_timer = Some(t);
}

/// The web's `applyPreset`: every knob except Volume glides to the preset in 520 ms (ease-out cubic).
fn apply_preset(state: &Rc<RefCell<State>>, p: usize) {
    if p == 0 {
        // "Original" really is the original, EQ included (like the web)
        state.borrow_mut().eq.set_bypass(false);
        apply_eq_preset(state, 0);
    }
    let targets = state.borrow().rack.preset_targets(p);
    state.borrow_mut().rack.active_preset = Some(p);
    let start = Instant::now();
    let sw = Rc::downgrade(state);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let Some(s) = sw.upgrade() else { return };
        let k = (start.elapsed().as_secs_f64() / 0.52).min(1.0);
        let e = 1.0 - (1.0 - k).powi(3);
        {
            let mut sb = s.borrow_mut();
            for (i, from, to) in &targets {
                sb.rack.set_norm(*i, from + (to - from) * e, false);
            }
            sb.rack.active_preset = Some(p);
        }
        apply_gain(&s);
        update_fidelity(&s);
        if k >= 1.0 {
            show_presets(&s.borrow());
            s.borrow_mut().preset_timer = None;
        }
    });
    state.borrow_mut().preset_timer = Some(t);
    show_presets(&state.borrow());
}

/// The EQ's live spectrum runs while the panel is open and visible, and playing (paused it sinks
/// away, then the timer stops: an idle window redraws nothing).
fn spectrum_schedule(state: &Rc<RefCell<State>>) {
    let run = {
        let sb = state.borrow();
        sb.visible && sb.window.get_eq_open() && (sb.player.as_ref().is_some_and(Player::is_playing) || !sb.window.get_eq_spectrum().is_empty())
    };
    if !run {
        state.borrow_mut().spec_timer = None;
        return;
    }
    if state.borrow().spec_timer.is_some() {
        return;
    }
    state.borrow_mut().spec_last = Instant::now();
    let sw = Rc::downgrade(state);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let Some(s) = sw.upgrade() else { return };
        let keep = {
            let mut sb = s.borrow_mut();
            let now = Instant::now();
            let dt = now.duration_since(sb.spec_last).as_secs_f64().min(0.05);
            sb.spec_last = now;
            let playing = sb.player.as_ref().is_some_and(Player::is_playing);
            let alive = if playing {
                let samples = sb.player.as_ref().map(|p| p.scope(nativefs_audio::analysis::FFT_SIZE)).unwrap_or_default();
                let sb = &mut *sb;
                sb.spectrum.update(&samples);
                sb.spec_view.update(&sb.spectrum.db, sb.rate, dt);
                true
            } else {
                sb.spec_view.fall(dt)
            };
            let (w, h) = sb.eq.view;
            sb.window.set_eq_spectrum(sb.spec_view.path(w, h).into());
            alive && sb.visible && sb.window.get_eq_open()
        };
        if !keep {
            s.borrow_mut().spec_timer = None;
        }
    });
    state.borrow_mut().spec_timer = Some(t);
}

/// The EQ changed: the sound, the panel, the dot on the EQ button.
fn eq_changed(state: &Rc<RefCell<State>>) {
    {
        let sb = state.borrow();
        crate::audio_eq::show(&sb.window, &sb.eq);
    }
    apply_gain(state);
    update_fidelity(state);
}

/// The web's `applyEqPreset`: shapes switch at once, frequency / gain / width glide over 420 ms.
fn apply_eq_preset(state: &Rc<RefCell<State>>, i: usize) {
    let to = crate::audio_eq::preset_bands(i);
    let from = state.borrow().eq.bands;
    {
        let mut sb = state.borrow_mut();
        for (b, t) in sb.eq.bands.iter_mut().zip(to.iter()) {
            b.kind = t.kind;
            b.on = true;
        }
    }
    let start = Instant::now();
    let sw = Rc::downgrade(state);
    let t = slint::Timer::default();
    t.start(slint::TimerMode::Repeated, Duration::from_millis(16), move || {
        let Some(s) = sw.upgrade() else { return };
        let k = (start.elapsed().as_secs_f64() / 0.42).min(1.0);
        let e = 1.0 - (1.0 - k).powi(3);
        {
            let mut sb = s.borrow_mut();
            for j in 0..7 {
                let (a, b) = (from[j], to[j]);
                sb.eq.bands[j].freq = (a.freq.ln() + (b.freq.ln() - a.freq.ln()) * e).exp();
                sb.eq.bands[j].gain = a.gain + (b.gain - a.gain) * e;
                sb.eq.bands[j].q = (a.q.ln() + (b.q.ln() - a.q.ln()) * e).exp();
            }
            sb.eq.compute();
            sb.eq.active_preset = Some(i);
        }
        eq_changed(&s);
        if k >= 1.0 {
            s.borrow_mut().eq_timer = None;
        }
    });
    state.borrow_mut().eq_timer = Some(t);
    state.borrow_mut().eq.active_preset = Some(i);
    crate::audio_eq::show_presets(&state.borrow().window, Some(i));
}

/// The preset chips are rebuilt only when the highlighted preset changes (not on every move).
fn eq_presets_if_changed(state: &Rc<RefCell<State>>) {
    let sb = state.borrow();
    let shown = sb.window.get_eq_quick();
    let shown_active = (0..slint::Model::row_count(&shown)).filter_map(|i| slint::Model::row_data(&shown, i)).find(|c| c.active).map(|c| c.index as usize);
    let more = sb.window.get_eq_more_label();
    let active = sb.eq.active_preset;
    let shown_hidden = more != "More";
    let quick_active = active.filter(|i| crate::audio_eq::FEATURED.contains(&crate::audio_eq::PRESETS[*i].name));
    if shown_active != quick_active || shown_hidden != (active.is_some() && quick_active.is_none()) {
        crate::audio_eq::show_presets(&sb.window, active);
    }
}

fn wire_eq(w: &AudioWindow, state: &Rc<RefCell<State>>) {
    use crate::audio_eq as eq;
    let sw = Rc::downgrade(state);
    let with = Rc::new(move |f: &dyn Fn(&Rc<RefCell<State>>)| {
        if let Some(s) = sw.upgrade() {
            f(&s);
        }
    });
    macro_rules! on {
        ($setter:ident, |$s:ident $(, $arg:ident)*| $body:block) => {{
            let with = with.clone();
            w.$setter(move |$($arg),*| with(&|$s: &Rc<RefCell<State>>| $body));
        }};
    }
    on!(on_open_eq, |s| {
        let sb = s.borrow();
        sb.window.set_eq_presets_open(sb.shared.config.lock().unwrap().audio_eq_presets_open);
        eq::show(&sb.window, &sb.eq);
        eq::show_presets(&sb.window, sb.eq.active_preset);
        sb.window.set_eq_open(true);
        drop(sb);
        spectrum_schedule(s);
    });
    on!(on_eq_close, |s| {
        s.borrow().window.set_eq_open(false);
        s.borrow().window.set_eq_menu_open(false);
        spectrum_schedule(s);
    });
    on!(on_eq_toggle_bypass, |s| {
        let on = !s.borrow().eq.bypass;
        s.borrow_mut().eq.set_bypass(on);
        eq_changed(s);
    });
    on!(on_eq_preset, |s, i| {
        if i == 0 {
            s.borrow_mut().eq.set_bypass(false);
        }
        apply_eq_preset(s, i.max(0) as usize);
    });
    on!(on_eq_toggle_presets, |s, open| {
        s.borrow().window.set_eq_presets_open(open);
        save_config(s, |c| c.audio_eq_presets_open = open);
    });
    on!(on_eq_pointer, |s, kind, x, y, w, h, shift| {
        let (x, y, w, h) = (x as f64, y as f64, w as f64, h as f64);
        let before = s.borrow().eq.active_preset;
        let hit = s.borrow().eq.hit(x, y, w, h);
        let mut sound = false;
        {
            let mut sb = s.borrow_mut();
            match kind {
                0 => {
                    if let Some(i) = hit {
                        sb.eq_timer = None;
                        sb.eq.press(i, x, y, shift);
                    }
                }
                1 => {
                    if sb.eq.dragging().is_some() {
                        sound = sb.eq.drag_to(x, y, w, h, shift);
                    } else {
                        sb.eq.hover = hit;
                    }
                }
                2 => sb.eq.release(),
                3 => {
                    if let Some(i) = hit {
                        sb.eq.sel = i;
                        sb.window.set_eq_menu_items(ModelRc::new(VecModel::from(eq::menu_items(&sb.eq, i))));
                        sb.window.set_eq_menu_color(eq::color(i));
                        sb.window.set_eq_menu_open(true);
                    }
                }
                4 => {
                    if let Some(i) = hit {
                        sb.eq.release();
                        sb.eq.reset_band(i);
                        sound = true;
                    }
                }
                _ => {
                    if sb.eq.dragging().is_none() {
                        sb.eq.hover = None;
                    }
                }
            }
        }
        if sound {
            eq_changed(s);
        } else {
            let sb = s.borrow();
            eq::show(&sb.window, &sb.eq);
        }
        if s.borrow().eq.active_preset != before {
            let sb = s.borrow();
            eq::show_presets(&sb.window, sb.eq.active_preset);
        }
    });
    {
        let with = with.clone();
        w.on_eq_wheel(move |x, y, w, h, dy, shift| {
            let done = std::cell::Cell::new(false);
            with(&|s: &Rc<RefCell<State>>| {
                let i = {
                    let sb = s.borrow();
                    sb.eq.hit(x as f64, y as f64, w as f64, h as f64).or(sb.eq.hover).unwrap_or(sb.eq.sel)
                };
                let changed = s.borrow_mut().eq.wheel_q(i, dy as f64, shift);
                if changed {
                    done.set(true);
                    eq_changed(s);
                    eq_presets_if_changed(s);
                }
            });
            done.get()
        });
    }
    on!(on_eq_type_menu, |s, i, x, y| {
        let mut sb = s.borrow_mut();
        let i = i.max(0) as usize;
        sb.eq.sel = i;
        sb.window.set_eq_menu_x(x);
        sb.window.set_eq_menu_y(y);
        sb.window.set_eq_menu_items(ModelRc::new(VecModel::from(eq::menu_items(&sb.eq, i))));
        sb.window.set_eq_menu_color(eq::color(i));
        sb.window.set_eq_menu_open(true);
    });
    on!(on_eq_menu_pick, |s, item| {
        {
            let mut sb = s.borrow_mut();
            let i = sb.eq.sel;
            match eq::TYPES.get(item as usize) {
                Some((kind, _, _)) => sb.eq.set_type(i, *kind),
                None => sb.eq.toggle_on(i),
            }
        }
        eq_changed(s);
        eq_presets_if_changed(s);
    });
    on!(on_eq_fader, |s, i, v| {
        // magnet at 0 dB (like the web's fader)
        let v = if (v as f64).abs() < 0.4 { 0.0 } else { v as f64 };
        s.borrow_mut().eq_timer = None;
        s.borrow_mut().eq.set_height(i as usize, v);
        eq_changed(s);
        eq_presets_if_changed(s);
    });
    on!(on_eq_knob, |s, i, which, n| {
        let (i, n) = (i as usize, (n as f64).clamp(0.0, 1.0));
        s.borrow_mut().eq_timer = None;
        if which == 0 {
            let def = eq::freq_norm(nativefs_audio::dsp::eq_defaults()[i].freq);
            let n = if (n - def).abs() < 0.018 { def } else { n };
            s.borrow_mut().eq.set_freq(i, eq::freq_from_norm(n));
        } else {
            let def = eq::q_norm(1.0);
            let n = if (n - def).abs() < 0.018 { def } else { n };
            s.borrow_mut().eq.set_q(i, eq::q_from_norm(n));
        }
        eq_changed(s);
        eq_presets_if_changed(s);
    });
    on!(on_eq_knob_reset, |s, i, which| {
        let i = i as usize;
        let d = nativefs_audio::dsp::eq_defaults()[i];
        if which == 0 {
            s.borrow_mut().eq.set_freq(i, d.freq);
        } else {
            s.borrow_mut().eq.set_q(i, 1.0);
        }
        eq_changed(s);
        eq_presets_if_changed(s);
    });
    on!(on_eq_show_tip, |s, i, which, x, y| {
        let sb = s.borrow();
        let Some(col) = slint::Model::row_data(&sb.window.get_eq_cols(), i as usize) else { return };
        sb.window.set_tip_data(if which == 0 { col.freq } else { col.q });
        sb.window.set_tip_x(x);
        sb.window.set_tip_y(y);
        sb.window.set_tip_on(true);
    });
    on!(on_eq_plot_size, |s, w, h| {
        if w > 0.0 && h > 0.0 {
            s.borrow_mut().eq.view = (w as f64, h as f64);
            let sb = s.borrow();
            eq::show(&sb.window, &sb.eq);
        }
    });
    on!(on_eq_col_hover, |s, i, on| {
        {
            let mut sb = s.borrow_mut();
            if sb.eq.dragging().is_some() {
                return;
            }
            sb.eq.hover = if on { Some(i as usize) } else { None };
        }
        let sb = s.borrow();
        eq::show(&sb.window, &sb.eq);
    });
}

fn wire(w: &AudioWindow, state: &Rc<RefCell<State>>) {
    wire_eq(w, state);
    let sw = Rc::downgrade(state);
    let with = move |f: &dyn Fn(&Rc<RefCell<State>>)| {
        if let Some(s) = sw.upgrade() {
            f(&s);
        }
    };
    let with = Rc::new(with);
    macro_rules! on {
        ($setter:ident, |$s:ident $(, $arg:ident)*| $body:block) => {{
            let with = with.clone();
            w.$setter(move |$($arg),*| with(&|$s: &Rc<RefCell<State>>| $body));
        }};
    }
    on!(on_play_pause, |s| {
        let sb = s.borrow();
        if let Some(p) = &sb.player {
            if p.is_playing() {
                p.pause();
            } else {
                start(s, p);
            }
        }
    });
    on!(on_restart, |s| {
        if let Some(p) = &s.borrow().player {
            p.seek(0);
        }
        tick(s);
    });
    on!(on_stop, |s| {
        if let Some(p) = &s.borrow().player {
            p.pause();
            p.seek(0);
        }
        tick(s);
        media_update(s);
    });
    on!(on_toggle_mute, |s| {
        let m = !s.borrow().muted;
        s.borrow_mut().muted = m;
        s.borrow().window.set_muted(m);
        apply_gain(s);
    });
    on!(on_toggle_loop, |s| {
        let on = !s.borrow().window.get_looping();
        s.borrow().window.set_looping(on);
        if let Some(p) = &s.borrow().player {
            p.set_loop(on);
        }
        save_config(s, |c| c.audio_loop = on);
    });
    on!(on_toggle_vis, |s| {
        let on = !s.borrow().window.get_vis_on();
        s.borrow().window.set_vis_on(on);
        save_config(s, |c| c.audio_visualizer = on);
        visualize(s);
    });
    on!(on_toggle_norm, |s| {
        let on = !s.borrow().window.get_norm_on();
        s.borrow().window.set_norm_on(on);
        save_config(s, |c| c.audio_normalize = on);
        apply_gain(s);
        update_fidelity(s);
    });
    on!(on_toggle_bit_perfect, |s| {
        let on = !s.borrow().window.get_bit_perfect_mode();
        s.borrow().window.set_bit_perfect_mode(on);
        save_config(s, |c| c.audio_bit_perfect = on);
        apply_gain(s);
        update_fidelity(s);
        sync_hw_volume(s);
    });
    on!(on_seek, |s, v| {
        let sb = s.borrow();
        if let Some(p) = &sb.player {
            p.seek((v as f64 * sb.total * sb.rate) as u64);
        }
        drop(sb);
        tick(s);
        media_update(s);
    });
    on!(on_open_menu, |s| { refresh_devices(s); });
    on!(on_knob_drag, |s, i, n| {
        if i == 1 && hw_volume_knob(s, n as f64) {
            return;
        }
        s.borrow_mut().preset_timer = None;
        let before = s.borrow().rack.active_preset;
        s.borrow_mut().rack.drag(i as usize, n as f64);
        rack_changed(s, before);
    });
    on!(on_knob_reset, |s, i| {
        if i == 1 && s.borrow().rack.hw_volume.is_some() {
            // the device volume has no "default" to go back to
            return;
        }
        s.borrow_mut().preset_timer = None;
        let before = s.borrow().rack.active_preset;
        s.borrow_mut().rack.reset(i as usize);
        rack_changed(s, before);
    });
    on!(on_knob_wheel, |s, i, d| {
        let hw = s.borrow().rack.hw_volume;
        if i == 1 && hw.is_some_and(|v| hw_volume_knob(s, v + d as f64 * 0.0011)) {
            return;
        }
        s.borrow_mut().preset_timer = None;
        let before = s.borrow().rack.active_preset;
        s.borrow_mut().rack.wheel(i as usize, d as f64);
        rack_changed(s, before);
    });
    on!(on_knob_edit, |s, i, text| {
        // the keys work again after typing a value
        s.borrow().window.invoke_focus_keys();
        if i == 1 && s.borrow().rack.hw_volume.is_some() {
            if let Ok(v) = text.trim().trim_start_matches("Device").trim().trim_end_matches('%').trim().replace(',', ".").parse::<f64>() {
                hw_volume_knob(s, v / 100.0);
            }
            return;
        }
        let before = s.borrow().rack.active_preset;
        s.borrow_mut().rack.edit(i as usize, &text);
        rack_changed(s, before);
    });
    on!(on_apply_preset, |s, p| { apply_preset(s, p.max(0) as usize); });
    on!(on_toggle_presets, |s, open| {
        s.borrow().window.set_presets_open(open);
        save_config(s, |c| c.audio_presets_open = open);
    });
    on!(on_seek_by, |s, secs| { seek_by(s, secs as f64); });
    on!(on_resized, |s| {
        // paused: redraw the field for the new size once (playing, the frame timer does it)
        if s.borrow().timer.is_none() {
            visualize(s);
        }
    });
    on!(on_select_device, |s, i| {
        let id = if i <= 0 { String::new() } else { s.borrow().devices.get(i as usize - 1).map(|d| d.id.clone()).unwrap_or_default() };
        save_config(s, |c| c.audio_device = id);
        refresh_devices(s);
        s.borrow().window.set_menu_open(false);
        reapply_output(s);
        sync_hw_volume(s);
    });
    on!(on_set_exclusive, |s, on| {
        s.borrow().window.set_exclusive(on);
        save_config(s, |c| c.audio_exclusive = on);
        reapply_output(s);
    });
    on!(on_set_release_on_pause, |s, on| {
        s.borrow().window.set_release_on_pause(on);
        save_config(s, |c| c.audio_release_on_pause = on);
        reapply_output(s);
    });
    on!(on_download, |s| {
        let sb = s.borrow();
        match &sb.source {
            Source::Local(p) => crate::files_ui::show_in_folder(p),
            Source::Remote(c) => match sb.remote.as_ref().and_then(RemoteHandle::bytes) {
                // already in memory: save without downloading again
                Some(bytes) => {
                    let weak = sb.window.as_weak();
                    crate::files_ui::save_bytes(&c.name, bytes, move |m| {
                        let weak = weak.clone();
                        windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, m);
                    });
                }
                None => crate::files_ui::save_as(&sb.shared, c),
            },
        }
    });
    on!(on_tour_end, |s, remember| {
        if remember {
            save_config(s, |c| c.audio_tour_done = true);
        }
    });
    on!(on_back, |s| {
        let shared = s.borrow().shared.clone();
        crate::show_main(&shared);
    });
    {
        let with = with.clone();
        w.on_key(move |text, ctrl, _shift| {
            let handled = std::cell::Cell::new(false);
            with(&|s: &Rc<RefCell<State>>| {
                let w = s.borrow().window.clone_strong();
                handled.set(true);
                // Ctrl (Cmd on macOS) + plus / minus / 0 zoom like a browser
                let zoom = match text.as_str() {
                    "=" | "+" => Some(1),
                    "-" | "_" => Some(-1),
                    // "é" = the 0 key on a Czech keyboard (browsers go by the physical key)
                    "0" | "é" => Some(0),
                    _ => None,
                };
                if let (true, Some(dir)) = (ctrl, zoom) {
                    let shared = s.borrow().shared.clone();
                    windows::zoom_step(w.window(), &shared, |c| &mut c.audio_zoom, dir);
                    return;
                }
                match text.as_str() {
                    " " | "k" | "K" => w.invoke_play_pause(),
                    "m" | "M" => w.invoke_toggle_mute(),
                    "l" | "L" => w.invoke_toggle_loop(),
                    _ => handled.set(false),
                }
            });
            handled.get()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bpm_ladder_matches_the_web() {
        assert_eq!(font_for_bpm(None), "Quicksand");
        assert_eq!(font_for_bpm(Some(60.0)), "EB Garamond");
        assert_eq!(font_for_bpm(Some(70.0)), "EB Garamond");
        assert_eq!(font_for_bpm(Some(123.0)), "Poppins");
        assert_eq!(font_for_bpm(Some(159.9)), "Space Grotesk");
        assert_eq!(font_for_bpm(Some(200.0)), "Space Grotesk");
    }

    #[test]
    fn meta_line() {
        let src = SourceFormat { rate: 44100, channels: 2, bits: Some(16), codec: "FLAC".into() };
        assert_eq!(meta_summary(&src, Some(811), Some(123.0)), "FLAC · Stereo · 811kbps · 16bit · 44.1kHz · 123 BPM");
    }
}
