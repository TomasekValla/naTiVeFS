#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod bg;
mod camera_ui;
mod capture;
mod audio_controls;
mod audio_eq;
mod audio_rack;
mod audio_ui;
mod autostart;
mod check_ui;
mod files_ui;
mod image_ui;
mod open;
#[cfg(windows)]
mod reg;
mod router;
mod shellmenu;
mod player;
mod reader_ui;
mod thumbs;
mod tray;
mod media_keys;
mod upload_ui;
mod voice;
mod windows;

mod about {
    include!(concat!(env!("OUT_DIR"), "/about.rs"));
}

slint::include_modules!();

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nativefs_ipc::Message;
use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel, Weak};
use tokio::sync::{Notify, Semaphore};
use tvfs_core::config::{Config, Motion, UpdateChannel};
use serde::{Deserialize, Serialize};
use tvfs_core::fmt::{fmt_countdown, truncate_filename};
use tvfs_core::link::parse_open_request;
use tvfs_core::models::{PendingUpload, RecentFiles, SyncEntry, UploadCompleteResponse};
use tvfs_core::upload::{HistoryStore, ResumeStore, Run, Uploader};
use tvfs_core::session::{self, StoredSession};
use tvfs_core::update::{self, CheckOutcome};
use tvfs_core::{Client, Error};

/// Status poll interval while the window is visible (hidden: no polling at all).
const POLL: Duration = Duration::from_secs(10);
/// Logical px scrolled per mouse-wheel notch (what browsers use).
const WHEEL_STEP: f32 = 100.0;
/// Images without a server thumbnail are previewed from the file itself up to this size.
const IMAGE_PREVIEW_MAX: u64 = 16 * 1024 * 1024;
const THUMB_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Logical size of the thumbnail box in a recent row.
const THUMB_BOX: f32 = 42.0;

enum Thumb {
    Loading,
    Ready(SharedPixelBuffer<Rgba8Pixel>),
    Failed(Instant),
}

struct Shared {
    client: Client,
    rt: tokio::runtime::Handle,
    recent: Mutex<RecentFiles>,
    config: Mutex<Config>,
    /// Window shown and not minimized / fully covered.
    window_visible: AtomicBool,
    /// The main window is open (shown, possibly minimized); false after it was closed.
    main_shown: AtomicBool,
    logged_in: AtomicBool,
    tray_ok: AtomicBool,
    /// Wakes the status poller (window shown, focused, login).
    wake: Notify,
    /// Thumbnails by URL (server thumbnail or image link).
    thumbs: Mutex<HashMap<String, Thumb>>,
    thumb_px: AtomicU32,
    fetch_limit: Semaphore,
    uploader: Uploader,
    /// Account tier from `/api/status` (tier 2 gets links without a timestamp).
    tier: AtomicU8,
    /// The upload running right now (its files are not shown as NOT FINISHED).
    active_run: Mutex<Option<Arc<Run>>>,
    /// Unfinished uploads of this account on any device (`/api/status`).
    /// Upload ids of runs that just ended: `/api/status` caches the pending list for a few
    /// seconds, so a finished upload would come back as "100% uploaded from another device".
    finished_ids: Mutex<Vec<(String, Instant)>>,
    pending: Mutex<Vec<PendingUpload>>,
    /// Things only this device knows: reader / player pages, TVFS Users Only shares.
    local: Mutex<LocalData>,
    local_path: Option<PathBuf>,
    /// "Show more" pressed on the recent list.
    recent_expanded: AtomicBool,
}

/// A "TVFS Users Only" share (the server does not list them in the recent list; the web
/// keeps them in localStorage too).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalShare {
    id: String,
    name: String,
    link: String,
    size: u64,
    uploaded_at: i64,
    expires_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct LocalData {
    /// file id -> (reader / player page, 1 = reader, 2 = player, expires at)
    tool_links: HashMap<String, (String, i32, i64)>,
    shares: Vec<LocalShare>,
}

/// Recent entries live at most 14 days (files cannot outlive that anyway).
const RECENT_MAX_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;

impl Shared {
    fn save_local(&self, data: &LocalData) {
        let Some(path) = &self.local_path else { return };
        if let Ok(bytes) = serde_json::to_vec(data) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
                eprintln!("naTiVeFS: could not save {}", path.display());
            }
        }
    }

    /// Keep the reader / player page of an upload from this device for its recent row.
    fn remember_tool_link(&self, r: &UploadCompleteResponse) {
        let Some(tool) = r.tool_link() else { return };
        if r.file_id.is_empty() {
            return;
        }
        let kind = if r.audio_player_link.as_deref() == Some(tool) { 2 } else { 1 };
        let mut data = self.local.lock().unwrap();
        let cutoff = now_ms() - RECENT_MAX_AGE_MS;
        data.tool_links.retain(|_, v| v.2 > cutoff);
        data.tool_links.insert(r.file_id.clone(), (tool.to_owned(), kind, now_ms() + RECENT_MAX_AGE_MS));
        self.save_local(&data);
    }

    fn add_local_share(&self, share: LocalShare) {
        let mut data = self.local.lock().unwrap();
        let cutoff = now_ms() - RECENT_MAX_AGE_MS;
        data.shares.retain(|s| s.uploaded_at > cutoff && s.id != share.id);
        data.shares.insert(0, share);
        self.save_local(&data);
    }

    fn remove_local_share(&self, id: &str) -> bool {
        let mut data = self.local.lock().unwrap();
        let before = data.shares.len();
        data.shares.retain(|s| s.id != id);
        let removed = data.shares.len() != before;
        if removed {
            self.save_local(&data);
        }
        removed
    }
}

thread_local! {
    static TRAY: RefCell<Option<Box<dyn std::any::Any>>> = const { RefCell::new(None) };
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    /// Decoded thumbnails as Slint images (UI thread only), by URL.
    static THUMB_IMAGES: RefCell<HashMap<String, slint::Image>> = RefCell::new(HashMap::new());
    static ANIM: RefCell<Option<Anim>> = const { RefCell::new(None) };
    static AUTOSCROLL: RefCell<AutoScroll> = RefCell::new(AutoScroll::default());
    static COUNTDOWN: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// The zoom modifier is held (Ctrl; Cmd on macOS) and the touchpad pixels not yet turned into a zoom step.
    static ZOOM_KEY: std::cell::Cell<(bool, f32)> = const { std::cell::Cell::new((false, 0.0)) };
    /// Ctrl and Alt are held (Ctrl+Alt+V opens the TVFS link on the clipboard).
    static CTRL_ALT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The main window, for viewers that bring it back (the audio player's back arrow).
    static MAIN_UI: RefCell<Option<Weak<MainWindow>>> = const { RefCell::new(None) };
}

/// Show the main window again (from a viewer window).
/// Show the reader / audio player / video player zoom on the settings page (they change in their own windows).
fn refresh_viewer_zooms(shared: &Shared) {
    let Some(ui) = MAIN_UI.with(|m| m.borrow().as_ref().and_then(|w| w.upgrade())) else { return };
    let cfg = shared.config.lock().unwrap();
    ui.set_reader_zoom(cfg.reader_zoom);
    ui.set_audio_zoom(cfg.audio_zoom);
    ui.set_video_zoom(cfg.player_zoom);
}

fn show_main(shared: &Arc<Shared>) {
    if let Some(ui) = MAIN_UI.with(|m| m.borrow().as_ref().and_then(|w| w.upgrade())) {
        show_window(&ui, shared);
        windows::raise(ui.window());
    }
}


/// Browser zoom levels (Chrome's, 50 - 300 %; the extremes make no sense in a fixed-width column).
const ZOOM_LEVELS: [f32; 13] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];
/// The window's minimum size in logical px at 100 % (`min-width` / `min-height` in app.slint): the width scales with the zoom,
/// the height does not (the page scrolls).
const MIN_WINDOW_W: f32 = 480.0;
const MIN_WINDOW_H: f32 = 560.0;
/// Touchpad pixels per zoom step (a mouse wheel notch is one step).
const ZOOM_PIXELS_PER_STEP: f32 = 100.0;

/// The zoom level after `cur`: +1 / -1 = next / previous level, 0 = 100 %. A value between levels
/// (edited config) goes to the nearest level in that direction.
fn next_zoom(cur: f32, dir: i32) -> f32 {
    match dir {
        0 => 1.0,
        d if d > 0 => ZOOM_LEVELS.iter().copied().find(|z| *z > cur + 0.001).unwrap_or(ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]),
        _ => ZOOM_LEVELS.iter().rev().copied().find(|z| *z < cur - 0.001).unwrap_or(ZOOM_LEVELS[0]),
    }
}

/// +1 / -1 = next / previous zoom level, 0 = back to 100 %. Saved in the config.
fn zoom_step(ui: &MainWindow, shared: &Arc<Shared>, dir: i32) {
    let cur = shared.config.lock().unwrap().zoom;
    let next = next_zoom(cur, dir);
    if (next - cur).abs() > 1e-4 {
        let mut cfg = shared.config.lock().unwrap();
        cfg.zoom = next;
        let _ = cfg.save();
    }
    apply_zoom(ui, shared);
}

/// Zoom = the window scale factor (system scale x zoom), so Slint lays the page out at a smaller logical size,
/// exactly like a browser zoom. Runs again whenever the window is shown or the system scale changes.
fn apply_zoom(ui: &MainWindow, shared: &Arc<Shared>) {
    let zoom = shared.config.lock().unwrap().zoom.clamp(ZOOM_LEVELS[0], ZOOM_LEVELS[ZOOM_LEVELS.len() - 1]);
    ui.set_zoom(zoom);
    let Some(system) = ui.window().with_winit_window(|w| w.scale_factor() as f32) else { return };
    let sf = system * zoom;
    if (ui.window().scale_factor() - sf).abs() > 1e-4 {
        let physical = ui.window().size();
        ui.window().dispatch_event(slint::platform::WindowEvent::ScaleFactorChanged { scale_factor: sf });
        if physical.width > 0 && physical.height > 0 {
            ui.window().dispatch_event(slint::platform::WindowEvent::Resized { size: physical.to_logical(sf) });
        }
    }
    // Slint sizes the new window and its minimum before the zoom is known and recomputes the minimum only when the
    // layout changes: keep the page at its 480 logical px here (zooming in widens the window instead of squeezing the
    // page). Runs on every resize; it only acts when the window is too small.
    let min = winit::dpi::PhysicalSize::new((MIN_WINDOW_W * sf).ceil() as u32, (MIN_WINDOW_H * system).ceil() as u32);
    ui.window().with_winit_window(|w| w.set_min_inner_size(Some(min)));
    let now = ui.window().size();
    if now.width > 0 && (now.width < min.width || now.height < min.height) {
        ui.window().set_size(slint::PhysicalSize::new(now.width.max(min.width), now.height.max(min.height)));
    }
    shared.thumb_px.store((THUMB_BOX * sf).ceil() as u32, Ordering::Relaxed);
    ui.window().request_redraw();
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Run `f` on the UI thread (no-op if the window is gone).
fn on_ui<F: FnOnce(&MainWindow) + Send + 'static>(weak: &Weak<MainWindow>, f: F) {
    let weak = weak.clone();
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = weak.upgrade() {
            f(&ui);
        }
    });
}

// ── recently uploaded ──────────────────────────────────────────────────────

/// A recent row without its image (built off the UI thread).
struct Row {
    id: String,
    name: String,
    category: String,
    sub_text: String,
    expired: bool,
    link: String,
    thumb_key: Option<String>,
    kind: i32,
    key: String,
    tool_link: String,
    tool_kind: i32,
    sort_at: i64,
}

fn thumb_key(f: &SyncEntry, expired: bool) -> Option<String> {
    if expired {
        return None;
    }
    if let Some(t) = f.thumbnail.as_deref().filter(|t| !t.is_empty()) {
        return Some(t.to_owned());
    }
    // same fallback as the web: an image is its own preview
    (f.category == "image" && !f.link.is_empty() && f.size <= IMAGE_PREVIEW_MAX).then(|| f.link.clone())
}

/// "Will expire in 06:23:59:59" / "Expired" (web `fmtCountdown`, ticks every second).
fn expiry_sub(expires_at: Option<i64>, now: i64) -> (bool, String) {
    let expired = expires_at.is_some_and(|e| e <= now);
    match expires_at.and_then(|e| fmt_countdown(e - now)) {
        Some(cd) if !expired => (false, format!("Will expire in {cd}")),
        _ => (expired, "Expired".to_string()),
    }
}

/// Everything the web's "Recently uploaded" merges, newest first: the server's list, this
/// device's TVFS Users Only shares, unfinished uploads of this device (NOT FINISHED) and of
/// other devices (OTHER DEVICE).
fn to_rows(shared: &Shared) -> Vec<Row> {
    let now = now_ms();
    let cutoff = now - RECENT_MAX_AGE_MS;
    let mut rows = Vec::new();
    let local = shared.local.lock().unwrap().clone();
    for f in shared.recent.lock().unwrap().files.iter() {
        if f.uploaded_at.is_some_and(|t| t <= cutoff) {
            continue;
        }
        let (expired, sub_text) = expiry_sub(f.expires_at, now);
        let (tool_link, tool_kind) = local.tool_links.get(&f.id).map_or((String::new(), 0), |t| (t.0.clone(), t.1));
        rows.push(Row {
            id: f.id.clone(),
            name: truncate_filename(&f.name, 26),
            category: f.category.clone(),
            sub_text,
            expired,
            link: f.link.clone(),
            thumb_key: thumb_key(f, expired),
            kind: 0,
            key: String::new(),
            tool_link,
            tool_kind,
            sort_at: f.uploaded_at.unwrap_or(0),
        });
    }
    for sh in local.shares.iter().filter(|s| s.uploaded_at > cutoff) {
        let (expired, sub_text) = expiry_sub(Some(sh.expires_at), now);
        rows.push(Row {
            id: sh.id.clone(),
            name: truncate_filename(&sh.name, 26),
            category: "protected".into(),
            sub_text,
            expired,
            link: sh.link.clone(),
            thumb_key: None,
            kind: 3,
            key: String::new(),
            tool_link: String::new(),
            tool_kind: 0,
            sort_at: sh.uploaded_at,
        });
    }
    let active = shared.active_run.lock().unwrap().clone();
    let records = shared.uploader.resume_store().list(now);
    let mut local_ids = std::collections::HashSet::new();
    for (key, rec) in &records {
        if let Some(id) = &rec.upload_id {
            local_ids.insert(id.clone());
        }
        if active.as_ref().is_some_and(|r| r.is_active_key(key)) {
            continue;
        }
        let cd = fmt_countdown(rec.expires_at() - now);
        let mut sub_text = format!("{}% uploaded — select the file again to resume", rec.percent());
        if let Some(cd) = cd {
            sub_text.push_str(&format!(" (chunks expire in {cd})"));
        }
        rows.push(Row {
            id: format!("resume:{key}"),
            name: truncate_filename(if rec.name.is_empty() { "Unfinished upload" } else { &rec.name }, 26),
            category: "file".into(),
            sub_text,
            expired: false,
            link: String::new(),
            thumb_key: None,
            kind: 1,
            key: key.clone(),
            tool_link: String::new(),
            tool_kind: 0,
            sort_at: rec.saved_at,
        });
    }
    let finished = {
        let mut f = shared.finished_ids.lock().unwrap();
        f.retain(|(_, t)| t.elapsed() < FINISHED_HIDE);
        f.clone()
    };
    for p in shared.pending.lock().unwrap().iter() {
        if local_ids.contains(&p.upload_id)
            || active.as_ref().is_some_and(|r| r.is_active_upload(&p.upload_id))
            || finished.iter().any(|(id, _)| *id == p.upload_id)
        {
            continue;
        }
        let pct = match p.total_chunks {
            Some(t) if t > 0 => ((p.received_chunks as f64 / t as f64) * 100.0).round() as u32,
            _ => 0,
        };
        let cd = p.expires_at.and_then(|e| fmt_countdown(e - now)).unwrap_or_else(|| "a moment".into());
        rows.push(Row {
            id: format!("remote:{}", p.upload_id),
            name: truncate_filename(if p.filename.is_empty() { "Unfinished upload" } else { &p.filename }, 26),
            category: "file".into(),
            sub_text: format!("{pct}% uploaded from another device — will clear itself in {cd}"),
            expired: false,
            link: String::new(),
            thumb_key: None,
            kind: 2,
            key: p.upload_id.clone(),
            tool_link: String::new(),
            tool_kind: 0,
            sort_at: p.created_at.unwrap_or(0),
        });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.sort_at));
    rows
}

/// How long the upload ids of a finished run stay hidden from the OTHER DEVICE cards.
const FINISHED_HIDE: Duration = Duration::from_secs(60);
/// Rows shown when the list is collapsed (web `RECENT_COLLAPSED_COUNT`).
const RECENT_COLLAPSED: usize = 3;

/// Push the recent list to the UI and start loading missing thumbnails.
fn push_recent(shared: &Arc<Shared>, weak: &Weak<MainWindow>) {
    let mut rows = to_rows(shared);
    let total = rows.len();
    let expanded = shared.recent_expanded.load(Ordering::Relaxed);
    if !expanded {
        rows.truncate(RECENT_COLLAPSED);
    }
    let hidden = total.saturating_sub(RECENT_COLLAPSED) as i32;
    {
        let weak = weak.clone();
        on_ui(&weak, move |ui| {
            ui.set_recent_hidden(hidden);
            ui.set_recent_expanded(expanded && hidden > 0);
        });
    }
    let mut ready = Vec::new();
    {
        let mut map = shared.thumbs.lock().unwrap();
        for key in rows.iter().filter_map(|r| r.thumb_key.as_ref()) {
            match map.get(key) {
                Some(Thumb::Ready(buf)) => ready.push((key.clone(), buf.clone())),
                Some(Thumb::Loading) => {}
                Some(Thumb::Failed(at)) if at.elapsed() < Duration::from_secs(60) => {}
                _ => {
                    map.insert(key.clone(), Thumb::Loading);
                    let (shared, weak, key) = (shared.clone(), weak.clone(), key.clone());
                    shared.rt.clone().spawn(async move { load_thumb(shared, weak, key).await });
                }
            }
        }
    }
    on_ui(weak, move |ui| apply_recent(ui, rows, ready));
}

async fn load_thumb(shared: Arc<Shared>, weak: Weak<MainWindow>, key: String) {
    let px = shared.thumb_px.load(Ordering::Relaxed);
    let max = if key.contains("/thumbnails/") { THUMB_MAX_BYTES } else { IMAGE_PREVIEW_MAX };
    let result = async {
        let _permit = shared.fetch_limit.acquire().await.ok()?;
        let bytes = shared.client.fetch_bytes(&key, max).await.ok()?;
        tokio::task::spawn_blocking(move || thumbs::decode_square(&bytes, px)).await.ok()?
    }
    .await;
    let ok = result.is_some();
    shared
        .thumbs
        .lock()
        .unwrap()
        .insert(key, result.map_or_else(|| Thumb::Failed(Instant::now()), Thumb::Ready));
    if ok {
        push_recent(&shared, &weak);
    }
}

/// UI thread: build the rows (with cached images) and update the model in place.
fn apply_recent(ui: &MainWindow, rows: Vec<Row>, ready: Vec<(String, SharedPixelBuffer<Rgba8Pixel>)>) {
    let items: Vec<RecentItem> = THUMB_IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        for (key, buf) in ready {
            cache.entry(key).or_insert_with(|| slint::Image::from_rgba8(buf));
        }
        cache.retain(|k, _| rows.iter().any(|r| r.thumb_key.as_deref() == Some(k)));
        rows.into_iter()
            .map(|r| {
                let thumb = r.thumb_key.as_ref().and_then(|k| cache.get(k)).cloned();
                RecentItem {
                    id: r.id.into(),
                    name: r.name.into(),
                    category: r.category.into(),
                    sub_text: r.sub_text.into(),
                    expired: r.expired,
                    link: r.link.into(),
                    has_thumb: thumb.is_some(),
                    thumb: thumb.unwrap_or_default(),
                    kind: r.kind,
                    key: r.key.into(),
                    tool_link: r.tool_link.into(),
                    tool_kind: r.tool_kind,
                }
            })
            .collect()
    });
    let model = ui.get_recent();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<RecentItem>>() {
        if vm.row_count() == items.len() {
            for (i, item) in items.into_iter().enumerate() {
                if vm.row_data(i).as_ref() != Some(&item) {
                    vm.set_row_data(i, item);
                }
            }
            return;
        }
    }
    ui.set_recent(ModelRc::new(VecModel::from(items)));
}

// ── misc helpers ───────────────────────────────────────────────────────────

/// "Files expire on: Fri, Oct 23, 2026" for the selected expiry preset; under one day the
/// web shows weekday + time instead ("Sat 07:05 PM").
fn expiry_date_text(index: i32) -> String {
    let mins = tvfs_core::EXPIRATION_OPTIONS_MIN.get(index.max(0) as usize).copied().unwrap_or(tvfs_core::DEFAULT_EXPIRATION_MIN);
    let at = chrono::Local::now() + chrono::Duration::minutes(mins as i64);
    if mins < 1440 {
        format!("Files expire on: {}", at.format("%a %I:%M %p"))
    } else {
        format!("Files expire on: {}", at.format("%a, %b %-d, %Y"))
    }
}

/// Text under "Chunk configuration" before the first upload of this session (web `setChunkAutoManage`).
const CHUNK_AUTO_START_TEXT: &str =
    "No real data yet — this device's first chunked upload uses the 30 MB × 3 default, then adapts from there.";

fn update_refresh_hz(ui: &MainWindow) {
    if let Some(hz) = monitor_hz(ui) {
        ui.set_refresh_hz(hz.round() as i32);
        ANIM.with(|a| {
            if let Some(a) = a.borrow_mut().as_mut() {
                a.refresh_hz = hz;
            }
        });
    }
}

fn show_window(ui: &MainWindow, shared: &Arc<Shared>) {
    let first = !shared.main_shown.swap(true, Ordering::Relaxed) && !ui.window().is_visible();
    if first {
        // a window first shown later (started in the tray) may already exist at winit's default 800 x 600
        ui.window().set_size(slint::LogicalSize::new(ui.get_preferred_w(), ui.get_preferred_h()));
    }
    let _ = ui.show();
    ui.window().set_minimized(false);
    apply_zoom(ui, shared);
    if first {
        update_refresh_hz(ui);
    }
    // refresh right away, then keep polling while visible
    set_visible(ui, shared, true);
    shared.wake.notify_one();
    ui.window().request_redraw();
}

thread_local! {
    /// Browsers found on this computer (settings > Links), in the order shown.
    static BROWSERS: RefCell<Vec<router::Browser>> = const { RefCell::new(Vec::new()) };
}

/// Ready-made browser commands for this OS (settings > Links).
fn browser_examples() -> &'static str {
    if cfg!(windows) {
        concat!(
            "Examples (Windows). The link goes where %1 is, otherwise at the end; %LOCALAPPDATA% and other variables work. ",
            r#"Chrome: "C:\Program Files\Google\Chrome\Application\chrome.exe" · "#,
            r#"Edge: "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe" · "#,
            r#"Brave: "C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe" · "#,
            r#"Firefox: "C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url %1 · "#,
            r#"Zen: "C:\Program Files\Zen Browser\zen.exe" -osint -url %1 · "#,
            r#"Vivaldi: "%LOCALAPPDATA%\Vivaldi\Application\vivaldi.exe" · "#,
            r#"Opera: "%LOCALAPPDATA%\Programs\Opera\opera.exe" · "#,
            r#"LibreWolf: "C:\Program Files\LibreWolf\librewolf.exe" -osint -url %1 · "#,
            r#"Chromium / Ungoogled: "%LOCALAPPDATA%\Chromium\Application\chrome.exe" · "#,
            r#"Waterfox: "C:\Program Files\Waterfox\waterfox.exe" -osint -url %1 · "#,
            r#"Floorp: "C:\Program Files\Ablaze Floorp\floorp.exe" -osint -url %1. "#,
            "Links open as a new tab in the browser's open window. Tor Browser is left out on purpose: every ordinary ",
            "link would go through Tor. The list above already holds the exact commands of the browsers installed here."
        )
    } else if cfg!(target_os = "macos") {
        concat!(
            r#"Examples (macOS): open -a "Safari" · open -a "Google Chrome" · open -a "Microsoft Edge" · "#,
            r#"open -a "Brave Browser" · open -a "Firefox" · open -a "Zen" · open -a "Vivaldi" · open -a "Opera" · "#,
            r#"open -a "LibreWolf" · open -a "Chromium" · open -a "Waterfox" · open -a "Floorp". "#,
            "The link is added at the end and opens as a new tab in the browser's open window."
        )
    } else {
        concat!(
            "Examples (Linux). The link goes where %u / %U is, otherwise at the end. ",
            "Packages: google-chrome-stable · microsoft-edge-stable · brave-browser (Arch: brave) · firefox · zen-browser · ",
            "vivaldi-stable · opera · librewolf · chromium · waterfox · floorp. ",
            "Flatpak: flatpak run com.google.Chrome · flatpak run com.microsoft.Edge · flatpak run com.brave.Browser · ",
            "flatpak run org.mozilla.firefox · flatpak run app.zen_browser.zen · flatpak run com.vivaldi.Vivaldi · ",
            "flatpak run com.opera.Opera · flatpak run io.gitlab.librewolf-community · flatpak run org.chromium.Chromium · ",
            "flatpak run io.github.ungoogled_software.ungoogled_chromium · flatpak run net.waterfox.waterfox · ",
            "flatpak run one.ablaze.floorp. Snap: snap run firefox · snap run chromium · snap run brave. ",
            "Links open as a new tab in the browser's open window. The list above holds the browsers found on this computer."
        )
    }
}

/// The router's switch, status line, browser list and pick (settings > Links, the first-run offer).
fn refresh_router(ui: &MainWindow, shared: &Arc<Shared>) {
    let browsers = router::installed();
    let state = router::state();
    let cfg = shared.config.lock().unwrap().clone();
    let prev = cfg.router_browser_name.clone().or_else(|| router::current_default().map(|b| b.name)).unwrap_or_default();
    let custom = cfg.browser.as_deref().map(str::trim).unwrap_or("").to_owned();
    let pick = if custom.is_empty() { -1 } else { browsers.iter().position(|b| b.command == custom).map(|i| i as i32).unwrap_or(-2) };
    let other = match pick {
        -1 if !prev.is_empty() => prev.clone(),
        -1 => "your browser".into(),
        -2 => "the command below".into(),
        i => browsers[i as usize].name.clone(),
    };
    let status = match (cfg.router_on, state) {
        (true, router::State::Default) => format!("naTiVeFS is your default browser. Other links open in {other}."),
        (true, router::State::Registered) if cfg!(windows) => {
            "Almost there: in Windows Settings > Default apps > naTiVeFS, click \"Set default\".".into()
        }
        (true, router::State::Registered) => "naTiVeFS is not the default browser yet: pick it in your system settings.".into(),
        (true, _) => format!("On. Other links open in {other}."),
        (false, router::State::Default) => {
            "naTiVeFS is still the default browser: make your browser the default in the system settings.".into()
        }
        (false, _) => String::new(),
    };
    ui.set_router_on(cfg.router_on);
    ui.set_router_status(status.into());
    ui.set_router_prev_name(prev.into());
    ui.set_browser_pick(pick);
    ui.set_browser_examples(browser_examples().into());
    let rows: Vec<BrowserRow> =
        browsers.iter().map(|b| BrowserRow { name: b.name.clone().into(), command: b.command.clone().into() }).collect();
    ui.set_browsers(ModelRc::new(VecModel::from(rows)));
    BROWSERS.with(|b| *b.borrow_mut() = browsers);
}

/// Switch the router on / off (settings, the first-run offer).
fn router_switch(ui: &MainWindow, shared: &Arc<Shared>, on: bool) {
    let result = {
        let mut cfg = shared.config.lock().unwrap();
        let r = if on { router::switch_on(&mut cfg) } else { router::switch_off(&cfg) };
        if r.is_ok() {
            cfg.router_on = on;
        }
        if let Err(e) = cfg.save() {
            eprintln!("naTiVeFS: could not save settings: {e}");
        }
        r
    };
    match result {
        Ok(router::Next::Done) => {}
        Ok(router::Next::FinishInSettings(what)) => show_toast(ui, what),
        Err(msg) => show_toast(ui, &msg),
    }
    refresh_router(ui, shared);
}

/// A click in the tray menu (UI thread).
fn tray_action(ui: &MainWindow, shared: &Arc<Shared>, a: tray::Action) {
    use tray::Action;
    match a {
        Action::Show => show_window(ui, shared),
        Action::PlayPause | Action::Stop => audio_ui::tray_action(a),
        Action::OpenLocal => ui.invoke_pick_local(),
        Action::Upload => {
            show_window(ui, shared);
            ui.set_page(0);
            ui.invoke_pick_files();
        }
        Action::ClipboardLink => {
            if let Err(msg) = open::open_clipboard_link(shared) {
                show_window(ui, shared);
                show_toast(ui, msg);
            }
        }
        Action::VoiceNote => {
            show_window(ui, shared);
            ui.set_page(0);
            ui.set_mode(2);
            voice::open(ui, shared);
        }
        Action::Settings => {
            show_window(ui, shared);
            ui.set_page(1);
        }
        Action::Quit => {
            let _ = slint::quit_event_loop();
        }
    }
}

/// Without a tray the app quits once no window is left (the main window closed, no viewer open).
fn quit_if_idle(shared: &Shared) {
    if !shared.main_shown.load(Ordering::Relaxed) && windows::live() == 0 && !shared.tray_ok.load(Ordering::Relaxed) {
        let _ = slint::quit_event_loop();
    }
}

fn show_login(weak: &Weak<MainWindow>, shared: &Shared, message: String) {
    shared.logged_in.store(false, Ordering::Relaxed);
    on_ui(weak, move |ui| {
        ui.set_logged_in(false);
        ui.set_busy(false);
        ui.set_error_text(message.into());
    });
}

/// Fetch status, sync the recent list when the server says it changed, push to the UI.
async fn refresh(shared: &Arc<Shared>, weak: &Weak<MainWindow>) -> Result<(), Error> {
    let t0 = Instant::now();
    let status = match shared.client.status().await {
        Ok(s) => s,
        Err(e) => {
            if matches!(e, Error::Network(_)) {
                on_ui(weak, |ui| ui.set_backend_online(false));
            }
            return Err(e);
        }
    };
    let ms = t0.elapsed().as_millis() as i64;
    let current = shared.recent.lock().unwrap().version;
    if status.user_version.map_or(current == 0, |v| v != current) {
        let resp = shared.client.sync(if current == 0 { None } else { Some(current) }).await?;
        shared.recent.lock().unwrap().apply(resp);
    }
    // the web shows parseFloat(usedGB) with two decimals; the dot turns yellow at 24 GB and red at 28.8 GB (of 32)
    let used_gb = status.used_gb.trim().split(' ').next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(status.used_bytes as f64 / 1073741824.0);
    let used = format!("{} GB", tvfs_core::fmt::to_fixed(used_gb, 2));
    let used_level = if used_gb < 24.0 { 0 } else if used_gb < 28.8 { 1 } else { 2 };
    let level = if ms < 150 { 0 } else if ms < 400 { 1 } else { 2 };
    let tier = status.tier as i32;
    shared.tier.store(status.tier, Ordering::Relaxed);
    *shared.pending.lock().unwrap() = status.pending_uploads.clone();
    push_recent(shared, weak);
    on_ui(weak, move |ui| {
        ui.set_backend_online(true);
        ui.set_latency_text(format!("{ms}ms").into());
        ui.set_latency_level(level);
        ui.set_used_text(used.into());
        ui.set_used_level(used_level);
        ui.set_tier(tier);
    });
    Ok(())
}

async fn enter_app(shared: &Arc<Shared>, weak: &Weak<MainWindow>) -> Result<(), Error> {
    refresh(shared, weak).await?;
    shared.logged_in.store(true, Ordering::Relaxed);
    on_ui(weak, |ui| {
        ui.set_logged_in(true);
        ui.set_busy(false);
        ui.set_error_text("".into());
    });
    Ok(())
}

/// Delete a finished upload (or forget a TVFS Users Only share, which the server keeps until it expires).
fn delete_recent(shared: &Arc<Shared>, weak: &Weak<MainWindow>, handle: &tokio::runtime::Handle, id: String) {
    if shared.remove_local_share(&id) {
        push_recent(shared, weak);
        return;
    }
    let (shared, weak) = (shared.clone(), weak.clone());
    handle.spawn(async move {
        if shared.client.delete_file(&id).await.is_ok() {
            shared.recent.lock().unwrap().files.retain(|f| f.id != id);
            push_recent(&shared, &weak);
        }
    });
}

fn open_external(url: &str, cfg: &Config) {
    let ok = cfg.browser.as_deref().is_some_and(|b| !b.trim().is_empty()) && cfg.open_in_browser(url).is_ok();
    if !ok {
        if let Err(e) = opener::open_browser(url) {
            eprintln!("naTiVeFS: could not open {url}: {e}");
        }
    }
}

thread_local! {
    /// While a file is dragged over the window: where it is, for the local zone's highlight.
    static DRAG_TIMER: RefCell<Option<slint::Timer>> = const { RefCell::new(None) };
    /// When a dropped file last opened locally (several files dropped at once: only the first one opens).
    static LOCAL_OPENED: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// The pointer in the window's logical px while a file is dragged. winit reports no position during drag & drop:
/// Windows asks the system, elsewhere the last known position is used.
fn drag_position(ui: &MainWindow) -> (f32, f32) {
    #[cfg(windows)]
    {
        use ::windows::Win32::Foundation::POINT;
        use ::windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut p = POINT::default();
        // SAFETY: GetCursorPos only writes the POINT it is given.
        if unsafe { GetCursorPos(&mut p) }.is_ok() {
            if let Some(Ok(origin)) = ui.window().with_winit_window(|w| w.inner_position()) {
                let sf = ui.window().scale_factor();
                return ((p.x - origin.x) as f32 / sf, (p.y - origin.y) as f32 / sf);
            }
        }
    }
    AUTOSCROLL.with(|s| s.borrow().cursor)
}

/// Is the dragged file over the "Use TVFS tools locally" zone? A zero wheel event finds it (`drop-probe` in app.slint).
fn over_local_zone(ui: &MainWindow) -> bool {
    if ui.get_page() != 0 {
        return false;
    }
    let (x, y) = drag_position(ui);
    let target = ui.get_autoscroll_target();
    ui.set_drop_probe_hit(false);
    ui.set_drop_probe(true);
    ui.set_wheel_probe(true);
    ui.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled { position: slint::LogicalPosition::new(x, y), delta_x: 0.0, delta_y: 0.0 });
    ui.set_wheel_probe(false);
    ui.set_drop_probe(false);
    ui.set_autoscroll_target(target);
    ui.get_drop_probe_hit()
}

/// Open a file from this computer ("Use TVFS tools locally"), one at a time.
fn open_local_file(ui: &MainWindow, shared: &Arc<Shared>, path: &std::path::Path) {
    let now = Instant::now();
    if LOCAL_OPENED.with(|t| t.get()).is_some_and(|t| now.duration_since(t) < Duration::from_millis(800)) {
        show_toast(ui, "TVFS tools open one file at a time");
        return;
    }
    LOCAL_OPENED.with(|t| t.set(Some(now)));
    if let Err(msg) = open::open_local(shared, path) {
        show_toast(ui, msg);
    }
}

/// A short message at the bottom of the main window.
fn show_toast(ui: &MainWindow, text: &str) {
    let weak = ui.as_weak();
    windows::toast(move |t| { if let Some(ui) = weak.upgrade() { ui.set_toast(t.into()); } }, text);
}

fn copy_to_clipboard(text: &str) {
    CLIPBOARD.with(|cell| {
        let mut cb = cell.borrow_mut();
        if cb.is_none() {
            *cb = arboard::Clipboard::new().ok();
        }
        if let Some(c) = cb.as_mut() {
            let _ = c.set_text(text.to_owned());
        }
    });
}

fn platform_label() -> String {
    let t = about::TARGET;
    let os = if t.contains("windows") {
        "Windows"
    } else if t.contains("apple") {
        "macOS"
    } else if t.contains("linux") {
        "Linux"
    } else {
        t
    };
    let arch = if t.starts_with("x86_64") {
        "x64"
    } else if t.starts_with("aarch64") {
        "ARM64"
    } else {
        t.split('-').next().unwrap_or("")
    };
    format!("{os} {arch}")
}

fn build_info() -> String {
    let mut s = format!("{} · {} build · {}", platform_label(), about::PROFILE, about::BUILD_DATE);
    if !about::GIT_HASH.is_empty() {
        s.push_str(&format!(" · {}", about::GIT_HASH));
    }
    s
}

fn check_updates(shared: &Arc<Shared>, weak: &Weak<MainWindow>) {
    let beta = shared.config.lock().unwrap().update_channel == UpdateChannel::Beta;
    on_ui(weak, |ui| {
        ui.set_update_busy(true);
        ui.set_update_status("".into());
        ui.set_update_url("".into());
    });
    let weak = weak.clone();
    shared.rt.spawn(async move {
        let (status, url) = match update::check(env!("CARGO_PKG_VERSION"), beta).await {
            Ok(CheckOutcome::Available(info)) => (format!("naTiVeFS {} is available.", info.version), info.page_url),
            Ok(CheckOutcome::UpToDate { latest }) => (format!("You are up to date (newest release: {latest})."), String::new()),
            Ok(CheckOutcome::NoBuild { tag }) => (
                format!("The newest release on GitHub ({tag}) has no signed naTiVeFS build for {}, so there is nothing to install.", platform_label()),
                String::new(),
            ),
            Ok(CheckOutcome::NoReleases) => ("No naTiVeFS release has been published on this channel yet.".into(), String::new()),
            Ok(CheckOutcome::Unsupported) => ("There are no release builds for this platform.".into(), String::new()),
            Err(e) => (format!("Could not check for updates: {e}"), String::new()),
        };
        on_ui(&weak, move |ui| {
            ui.set_update_busy(false);
            ui.set_update_status(status.into());
            ui.set_update_url(url.into());
        });
    });
}

// ── animated background ────────────────────────────────────────────────────
// Driven by the renderer: after a frame is presented the next backdrop frame is
// scheduled, never faster than the chosen rate (with vsync the swap itself paces
// "Monitor"). A hidden or minimized window renders nothing, so the loop stops by
// itself and costs nothing; showing the window renders again and restarts it.

struct Anim {
    backdrop: bg::Backdrop,
    motion: Motion,
    last_tick: Instant,
    pending: bool,
    /// Size / theme of the last rendered frame (a still frame is re-rendered when they change).
    rendered: Option<(f32, f32, bool)>,
    refresh_hz: f32,
    refresh_checked: Option<Instant>,
    /// Frames counted for the live "now: N FPS" readout on the settings page.
    frames: u32,
    frames_since: Instant,
    /// Orb speed multiplier: follows the upload speed like the web (`TVFS_setAnimSpeed`), eased.
    speed_target: f64,
    speed_current: f64,
}

/// Speed the background orbs move at (1.0 normally; 0.08 - 3.2 during an upload).
fn set_anim_speed(target: f64) {
    ANIM.with(|a| {
        if let Some(a) = a.borrow_mut().as_mut() {
            a.speed_target = target.clamp(0.05, 3.5);
        }
    });
}

impl Anim {
    fn interval(&self) -> Duration {
        match self.motion {
            Motion::Off => Duration::ZERO,
            Motion::Fps30 => Duration::from_micros(33_333),
            Motion::Fps60 => Duration::from_micros(16_667),
            // a hair under the refresh period: vsync paces it, timer jitter never skips a frame
            Motion::Monitor => Duration::from_secs_f32(1.0 / self.refresh_hz.max(24.0)).saturating_sub(Duration::from_micros(500)),
        }
    }
}

fn monitor_hz(ui: &MainWindow) -> Option<f32> {
    ui.window()
        .with_winit_window(|w| w.current_monitor().and_then(|m| m.refresh_rate_millihertz()))
        .flatten()
        .map(|mhz| mhz as f32 / 1000.0)
        .filter(|hz| *hz >= 20.0)
}

fn logical_size(ui: &MainWindow) -> (f32, f32) {
    let s = ui.window().size().to_logical(ui.window().scale_factor());
    (s.width, s.height)
}

fn schedule_backdrop(weak: &Weak<MainWindow>, shared: &Arc<Shared>) {
    let Some(ui) = weak.upgrade() else { return };
    ANIM.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(a) = cell.as_mut() else { return };
        if a.pending {
            return;
        }
        let (w, h) = logical_size(&ui);
        if w < 1.0 || h < 1.0 {
            return;
        }
        let light = ui.global::<Palette>().get_light();
        let stale = a.rendered.is_none_or(|(rw, rh, rl)| (rw - w).abs() >= 1.0 || (rh - h).abs() >= 1.0 || rl != light);
        let moving = a.motion != Motion::Off && shared.window_visible.load(Ordering::Relaxed);
        if !moving && !stale {
            return;
        }
        let delay = if moving { (a.last_tick + a.interval()).saturating_duration_since(Instant::now()) } else { Duration::ZERO };
        a.pending = true;
        let (weak, shared) = (weak.clone(), shared.clone());
        slint::Timer::single_shot(delay, move || backdrop_tick(&weak, &shared));
    });
}

fn backdrop_tick(weak: &Weak<MainWindow>, shared: &Arc<Shared>) {
    let Some(ui) = weak.upgrade() else { return };
    ANIM.with(|cell| {
        let mut cell = cell.borrow_mut();
        let Some(a) = cell.as_mut() else { return };
        a.pending = false;
        let now = Instant::now();
        a.frames += 1;
        let window = now.duration_since(a.frames_since);
        if window >= Duration::from_secs(1) {
            let fps = if a.motion == Motion::Off { 0.0 } else { a.frames as f64 / window.as_secs_f64() };
            a.frames = 0;
            a.frames_since = now;
            // only while the settings page shows it: no extra redraws elsewhere
            if ui.get_page() == 1 {
                ui.set_bg_fps(fps.round() as i32);
            }
        }
        // after a pause, continue where it stopped instead of jumping ahead
        let dt = now.duration_since(a.last_tick).min(Duration::from_millis(100));
        a.last_tick = now;
        if a.motion != Motion::Off && shared.window_visible.load(Ordering::Relaxed) {
            a.speed_current += (a.speed_target - a.speed_current) * 0.03;
            a.backdrop.advance(dt.as_secs_f64() * 1000.0 * a.speed_current);
        }
        if a.refresh_checked.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            a.refresh_checked = Some(now);
            if let Some(hz) = monitor_hz(&ui) {
                if (hz - a.refresh_hz).abs() > 0.5 {
                    a.refresh_hz = hz;
                    ui.set_refresh_hz(hz.round() as i32);
                }
            }
        }
        let (w, h) = logical_size(&ui);
        let light = ui.global::<Palette>().get_light();
        a.backdrop.set_light(light);
        let (plain, glass) = a.backdrop.render(w, h);
        a.rendered = Some((w, h, light));
        let look = ui.global::<Look>();
        look.set_backdrop(slint::Image::from_rgb8(plain));
        look.set_backdrop_glass(slint::Image::from_rgb8(glass));
        look.set_has_backdrop(true);
    });
}

// ── middle-button autoscroll (like browsers) ───────────────────────────────

#[derive(Default)]
struct AutoScroll {
    active: bool,
    /// Logical cursor position (always tracked).
    cursor: (f32, f32),
    anchor: (f32, f32),
    started: Option<Instant>,
    /// The cursor left the dead zone while the button was held: release stops ("hold" mode).
    moved_far: bool,
    last_tick: Option<Instant>,
    timer: Option<slint::Timer>,
}

const AUTOSCROLL_DEAD_ZONE: f32 = 10.0;

fn autoscroll_speed(dy: f32) -> f32 {
    let d = dy.abs() - AUTOSCROLL_DEAD_ZONE;
    if d <= 0.0 {
        return 0.0;
    }
    dy.signum() * (6.0 * d + 0.035 * d * d) // px/s
}

fn autoscroll_start(ui: &MainWindow, refresh_hz: f32) {
    let weak = ui.as_weak();
    AUTOSCROLL.with(|s| {
        let mut s = s.borrow_mut();
        s.active = true;
        s.anchor = s.cursor;
        s.started = Some(Instant::now());
        s.moved_far = false;
        s.last_tick = Some(Instant::now());
        ui.set_autoscroll_x(s.anchor.0);
        ui.set_autoscroll_y(s.anchor.1);
        ui.set_autoscroll_dir(0);
        ui.set_autoscroll(true);
        let timer = slint::Timer::default();
        let period = Duration::from_secs_f32(1.0 / refresh_hz.max(30.0));
        timer.start(slint::TimerMode::Repeated, period, move || {
            let Some(ui) = weak.upgrade() else { return };
            let step = AUTOSCROLL.with(|s| {
                let mut s = s.borrow_mut();
                let now = Instant::now();
                let dt = s.last_tick.map_or(0.0, |t| now.duration_since(t).as_secs_f32()).min(0.05);
                s.last_tick = Some(now);
                autoscroll_speed(s.cursor.1 - s.anchor.1) * dt
            });
            if step != 0.0 {
                ui.invoke_autoscroll_by(step);
            }
        });
        s.timer = Some(timer);
    });
}

fn autoscroll_stop(ui: &MainWindow) {
    AUTOSCROLL.with(|s| {
        let mut s = s.borrow_mut();
        s.active = false;
        s.timer = None;
    });
    ui.set_autoscroll(false);
}

/// Raw winit events, before Slint sees them: wheel (smooth scroll), middle-button
/// autoscroll (works over buttons too), and window visibility.
fn on_window_event(ui: &MainWindow, shared: &Arc<Shared>, w: &slint::Window, ev: &winit::event::WindowEvent) -> EventResult {
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent as E};
    let scale = w.scale_factor();
    let active = AUTOSCROLL.with(|s| s.borrow().active);
    match ev {
        E::CursorMoved { position, .. } => {
            let pos = (position.x as f32 / scale, position.y as f32 / scale);
            let dir = AUTOSCROLL.with(|s| {
                let mut s = s.borrow_mut();
                s.cursor = pos;
                let dy = pos.1 - s.anchor.1;
                if s.active && dy.abs() > AUTOSCROLL_DEAD_ZONE * 1.5 {
                    s.moved_far = true;
                }
                if dy.abs() <= AUTOSCROLL_DEAD_ZONE { 0 } else { dy.signum() as i32 }
            });
            if active {
                ui.set_autoscroll_dir(dir);
            }
            EventResult::Propagate
        }
        E::ModifiersChanged(mods) => {
            let held = if cfg!(target_os = "macos") { mods.state().super_key() } else { mods.state().control_key() };
            ZOOM_KEY.with(|z| z.set((held, 0.0)));
            CTRL_ALT.with(|c| c.set(mods.state().control_key() && mods.state().alt_key()));
            EventResult::Propagate
        }
        E::KeyboardInput { event, .. } if !active && windows::is_paste_link(event, CTRL_ALT.with(|c| c.get())) => {
            if let Err(msg) = open::open_clipboard_link(shared) {
                show_toast(ui, msg);
            }
            EventResult::PreventDefault
        }
        E::ScaleFactorChanged { .. } => {
            // the system scale changed (other monitor / display settings): Slint applies it, then the zoom goes on top
            let (weak, shared) = (ui.as_weak(), shared.clone());
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    apply_zoom(&ui, &shared);
                }
            });
            EventResult::Propagate
        }
        E::MouseWheel { delta, .. } if ZOOM_KEY.with(|z| z.get().0) => {
            let step = match delta {
                MouseScrollDelta::LineDelta(_, y) => y.signum() as i32,
                MouseScrollDelta::PixelDelta(p) => ZOOM_KEY.with(|z| {
                    let (held, acc) = z.get();
                    let acc = acc + p.y as f32;
                    let step = (acc / ZOOM_PIXELS_PER_STEP).trunc();
                    z.set((held, acc - step * ZOOM_PIXELS_PER_STEP));
                    step as i32
                }),
            };
            if step != 0 {
                zoom_step(ui, shared, step.signum());
            }
            EventResult::PreventDefault
        }
        E::MouseWheel { delta, .. } => {
            if active {
                autoscroll_stop(ui);
            }
            let (dy, smooth) = match delta {
                MouseScrollDelta::LineDelta(_, y) => (-y * WHEEL_STEP, true),
                // touchpads already deliver smooth pixel deltas
                MouseScrollDelta::PixelDelta(p) => (-(p.y as f32) / scale, false),
            };
            // Slint delivers the event to what is under the cursor: a nested box (queue list, textarea, raw JSON)
            // scrolls itself, everything else scrolls the page with this step (`scroll-event` in app.slint)
            ui.set_wheel_dy(dy);
            ui.set_wheel_smooth(smooth);
            EventResult::Propagate
        }
        E::MouseInput { state, button, .. } => {
            if active {
                match (state, button) {
                    (ElementState::Released, MouseButton::Middle) => {
                        let held = AUTOSCROLL.with(|s| {
                            let s = s.borrow();
                            s.moved_far || s.started.is_some_and(|t| t.elapsed() > Duration::from_millis(400))
                        });
                        if held {
                            autoscroll_stop(ui);
                        }
                    }
                    (ElementState::Pressed, _) => autoscroll_stop(ui),
                    _ => {}
                }
                return EventResult::PreventDefault;
            }
            if *button == MouseButton::Middle && *state == ElementState::Pressed {
                // like a browser: autoscroll the box under the cursor when it can scroll, otherwise the page. A zero
                // wheel event finds it (Slint delivers it to what is under the cursor, see `wheel-probe` in app.slint);
                // the press itself never reaches Slint, so no TouchArea is left holding a mouse grab
                let (x, y) = AUTOSCROLL.with(|s| s.borrow().cursor);
                ui.set_autoscroll_target(0);
                ui.set_wheel_probe(true);
                ui.window().dispatch_event(slint::platform::WindowEvent::PointerScrolled {
                    position: slint::LogicalPosition::new(x, y),
                    delta_x: 0.0,
                    delta_y: 0.0,
                });
                ui.set_wheel_probe(false);
                if ui.get_autoscroll_target() != 0 || ui.get_can_scroll() {
                    let hz = ANIM.with(|a| a.borrow().as_ref().map_or(60.0, |a| a.refresh_hz));
                    autoscroll_start(ui, hz);
                    return EventResult::PreventDefault;
                }
            }
            EventResult::Propagate
        }
        E::KeyboardInput { event, .. } if active => {
            if event.state == ElementState::Pressed {
                autoscroll_stop(ui);
            }
            EventResult::PreventDefault
        }
        E::Focused(false) if active => {
            autoscroll_stop(ui);
            EventResult::Propagate
        }
        E::Focused(true) => {
            if shared.logged_in.load(Ordering::Relaxed) {
                shared.wake.notify_one();
            }
            // back from the system settings (Windows' "Set default"): show the router's new state
            if ui.get_page() == 1 {
                refresh_router(ui, shared);
            }
            EventResult::Propagate
        }
        E::Occluded(hidden) => {
            set_visible(ui, shared, !hidden);
            EventResult::Propagate
        }
        // Windows reports minimizing as a 0x0 resize rather than `Occluded`
        E::Resized(size) => {
            set_visible(ui, shared, size.width > 0 && size.height > 0);
            // the first resize comes right after winit created the window (before that there is no system scale
            // to multiply); afterwards this is a no-op while the scale factor already matches
            apply_zoom(ui, shared);
            EventResult::Propagate
        }
        // drag & drop of files / folders anywhere on the window (the web: on the drop zone)
        E::HoveredFile(_) => {
            ui.set_drop_hover(true);
            // the local zone lights up while the file is over it
            let timer = slint::Timer::default();
            let weak = ui.as_weak();
            timer.start(slint::TimerMode::Repeated, Duration::from_millis(60), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_drop_local(over_local_zone(&ui));
                }
            });
            DRAG_TIMER.with(|t| *t.borrow_mut() = Some(timer));
            EventResult::Propagate
        }
        E::HoveredFileCancelled => {
            DRAG_TIMER.with(|t| *t.borrow_mut() = None);
            ui.set_drop_hover(false);
            ui.set_drop_local(false);
            EventResult::Propagate
        }
        E::DroppedFile(path) => {
            DRAG_TIMER.with(|t| *t.borrow_mut() = None);
            let local = over_local_zone(ui);
            ui.set_drop_hover(false);
            ui.set_drop_local(false);
            if local {
                open_local_file(ui, shared, path);
                return EventResult::Propagate;
            }
            if ui.get_mode() == 3 || ui.get_mode() == 2 {
                ui.set_mode(0);
            }
            upload_ui::add_paths(ui, vec![path.clone()]);
            EventResult::Propagate
        }
        _ => EventResult::Propagate,
    }
}

fn set_visible(ui: &MainWindow, shared: &Arc<Shared>, visible: bool) {
    let was = shared.window_visible.swap(visible, Ordering::Relaxed);
    if visible && !was {
        shared.wake.notify_one();
        upload_ui::start_render_timer(ui, shared);
        start_countdown(ui, shared);
    }
    if !visible && was {
        // hidden: no redraws at all (an upload keeps running in the background)
        upload_ui::stop_render_timer();
        COUNTDOWN.with(|c| *c.borrow_mut() = None);
    }
}

/// The recent list's countdowns tick once a second, only while the window is visible.
fn start_countdown(ui: &MainWindow, shared: &Arc<Shared>) {
    let timer = slint::Timer::default();
    let (weak, shared) = (ui.as_weak(), shared.clone());
    timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
        if shared.window_visible.load(Ordering::Relaxed) && shared.logged_in.load(Ordering::Relaxed) {
            push_recent(&shared, &weak);
        }
    });
    COUNTDOWN.with(|c| *c.borrow_mut() = Some(timer));
}

/// Debug builds only: `NATIVEFS_DEMO=1` shows the logged-in screen with fake data
/// (no server needed), `NATIVEFS_DEMO=0` the logged-out screen with fake status chips.
#[cfg(debug_assertions)]
fn apply_demo(ui: &MainWindow) {
    let Ok(mode) = std::env::var("NATIVEFS_DEMO") else { return };
    let item = |id: &str, name: &str, cat: &str, sub: &str, expired: bool| RecentItem {
        id: id.into(),
        name: name.into(),
        category: cat.into(),
        sub_text: sub.into(),
        expired,
        link: format!("https://files.tomasekvalla.cz/files/{cat}/{name}").into(),
        has_thumb: false,
        thumb: Default::default(),
        kind: if sub.contains("another device") { 2 } else if sub.contains("resume") { 1 } else { 0 },
        key: "k".into(),
        tool_link: if cat == "text" { "https://files.tomasekvalla.cz/files/text/r.html".into() } else { "".into() },
        tool_kind: 1,
    };
    ui.set_backend_online(true);
    ui.set_latency_text("23ms".into());
    ui.set_used_text("12.4 GB".into());
    ui.set_tier(3);
    if mode != "0" {
        let items = vec![
            item("1", "sample-video.mkv", "video", "Will expire in 06:23:59:59", false),
            item("2", "sample-track.flac", "audio", "Will expire in 02:04:00:00", false),
            item("3", "notes.md", "text", "Will expire in 00:23:12:05", false),
            item("5", "big-movie-file.mkv", "file", "30% uploaded — select the file again to resume (chunks expire in 00:22:59:59)", false),
            item("6", "holiday-4k.mov", "file", "42% uploaded from another device — will clear itself in 00:21:59:59", false),
            item("4", "wallpaper.png", "image", "Expired", true),
        ];
        ui.set_recent(ModelRc::new(VecModel::from(items)));
        ui.set_logged_in(true);
    }
    if std::env::var("NATIVEFS_PAGE").as_deref() == Ok("settings") {
        ui.set_page(1);
    }
    // `NATIVEFS_SCROLL=<px>` scrolls the page (screenshots of its lower parts without touching the mouse)
    if let Some(y) = std::env::var("NATIVEFS_SCROLL").ok().and_then(|v| v.parse::<f32>().ok()) {
        let weak = ui.as_weak();
        slint::Timer::single_shot(Duration::from_millis(400), move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_scroll_to(y, false);
            }
        });
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let autostarted = args.iter().any(|a| a == autostart::ARG);
    let mut start_minimized = args.iter().any(|a| a == "--minimized");
    // a TVFS link opens in the app; existing local paths are queued for upload (OS menus, "Open with")
    let url_arg = args.iter().filter(|a| !a.starts_with("--")).find(|a| parse_open_request(a).is_some()).cloned();
    // "Open locally in naTiVeFS" (file manager menu): `--local <path>`
    let local_arg = args
        .windows(2)
        .find(|w| w[0] == shellmenu::LOCAL_ARG)
        .map(|w| std::path::absolute(&w[1]).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| w[1].clone()));
    let upload_args: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--") && parse_open_request(a).is_none() && std::path::Path::new(a).exists())
        .filter(|a| local_arg.is_none() || args.windows(2).all(|w| !(w[0] == shellmenu::LOCAL_ARG && &w[1] == *a)))
        .map(|a| std::path::absolute(a).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| a.clone()))
        .collect();

    // Single instance: hand over to the running one and exit.
    let handover = match (&url_arg, start_minimized) {
        _ if local_arg.is_some() => Message::OpenLocal(local_arg.clone().unwrap_or_default()),
        _ if !upload_args.is_empty() => Message::Upload(upload_args.clone()),
        (Some(u), _) => Message::Open(u.clone()),
        (None, true) => Message::Ping,
        (None, false) if autostarted => Message::Ping,
        (None, false) => Message::Show,
    };
    if nativefs_ipc::send(&handover).is_ok() {
        return Ok(());
    }
    // Claim the instance right away; when another process started at the same moment won (several files from a file
    // manager menu), hand over to it as soon as it listens.
    let listener = match nativefs_ipc::listen() {
        Ok(l) => l,
        Err(_) => {
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(100));
                if nativefs_ipc::send(&handover).is_ok() {
                    return Ok(());
                }
            }
            eprintln!("naTiVeFS: another instance holds the IPC socket but does not answer");
            return Ok(());
        }
    };

    // The winit backend is required (raw wheel / middle-button events, monitor refresh rate).
    slint::BackendSelector::new().backend_name("winit".into()).select()?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("nativefs-io")
        .enable_all()
        .build()?;
    let handle = rt.handle().clone();

    let config = Config::load();
    // "Start with the computer" + "Start in the background": the autostart entry passes `--autostart`
    start_minimized |= autostarted && config.start_minimized;
    // keep `tvfs://` pointing at this installation; drop a router registration the user already left (off the UI thread)
    {
        let on = config.router_on;
        let menus = config.file_menus;
        std::thread::spawn(move || {
            router::tidy(on);
            shellmenu::sync(menus);
        });
    }
    let client = Client::new()?;
    let data_dir = Config::data_dir().ok();
    let resume = match &data_dir {
        Some(d) => ResumeStore::open(d.join("upload-resume.json")),
        None => ResumeStore::in_memory(),
    };
    let history = match &data_dir {
        Some(d) => HistoryStore::open(d.join("upload-stats.json")),
        None => HistoryStore::in_memory(),
    };
    let local_path = data_dir.as_ref().map(|d| d.join("uploads-local.json"));
    let local = local_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<LocalData>(&t).ok())
        .unwrap_or_default();
    let shared = Arc::new(Shared {
        uploader: Uploader::new(client.clone(), Arc::new(resume), Arc::new(history)),
        client,
        rt: handle.clone(),
        recent: Mutex::new(RecentFiles::default()),
        config: Mutex::new(config.clone()),
        window_visible: AtomicBool::new(false),
        main_shown: AtomicBool::new(false),
        logged_in: AtomicBool::new(false),
        tray_ok: AtomicBool::new(false),
        wake: Notify::new(),
        thumbs: Mutex::new(HashMap::new()),
        thumb_px: AtomicU32::new(84),
        fetch_limit: Semaphore::new(4),
        tier: AtomicU8::new(0),
        active_run: Mutex::new(None),
        finished_ids: Mutex::new(Vec::new()),
        pending: Mutex::new(Vec::new()),
        local: Mutex::new(local),
        local_path,
        recent_expanded: AtomicBool::new(false),
    });

    let ui = MainWindow::new()?;
    MAIN_UI.with(|m| *m.borrow_mut() = Some(ui.as_weak()));
    let weak = ui.as_weak();
    ui.global::<Palette>().set_light(config.light_theme);
    let look = ui.global::<Look>();
    look.set_glass(config.glass);
    look.set_smooth_scroll(config.smooth_scroll);
    ui.set_motion(config.motion.index() as i32);
    ui.set_close_to_tray(config.close_to_tray);
    ui.set_router_offer(!config.router_offered && !config.router_on);
    if ui.get_router_offer() {
        // the offer names the browser that gets the other links
        refresh_router(&ui, &shared);
    }
    ui.set_file_menus(config.file_menus);
    ui.set_file_menus_available(cfg!(any(windows, target_os = "linux")));
    ui.set_autostart(autostart::enabled());
    ui.set_start_background(config.start_minimized);
    ui.set_browser_cmd(config.browser.clone().unwrap_or_default().into());
    ui.set_update_beta(config.update_channel == UpdateChannel::Beta);
    ui.set_update_auto(config.check_updates);
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_build_info(build_info().into());
    let libs: Vec<LibInfo> = about::LIBS
        .iter()
        .map(|(name, version, license, url)| LibInfo {
            name: (*name).into(),
            version: (*version).into(),
            license: (*license).into(),
            url: (*url).into(),
        })
        .collect();
    ui.set_libs(ModelRc::new(VecModel::from(libs)));

    ui.set_expiry_date_text(expiry_date_text(ui.get_expiry()).into());
    // TVFS settings card
    ui.set_users_only(config.tvfs_users_only);
    ui.set_chunk_auto(config.chunk_auto);
    ui.set_chunk_count_text(config.chunk_count.to_string().into());
    ui.set_chunk_size_text(config.chunk_size_mb.to_string().into());
    ui.set_chunk_auto_text(CHUNK_AUTO_START_TEXT.into());
    ui.set_auto_copy(match config.auto_copy {
        tvfs_core::config::AutoCopy::Off => 0,
        tvfs_core::config::AutoCopy::Normal => 1,
        tvfs_core::config::AutoCopy::Styled => 2,
    });
    ui.set_prefer_tool(config.auto_copy_prefer_tool);
    ui.set_image_styled(config.auto_copy_image_styled);
    if !upload_args.is_empty() {
        upload_ui::add_paths(&ui, upload_args.iter().map(PathBuf::from).collect());
    }
    #[cfg(debug_assertions)]
    apply_demo(&ui);
    // debug builds: `NATIVEFS_WINDOW_POS=x,y` opens the window at that screen position (testing on a second monitor)
    #[cfg(debug_assertions)]
    if let Some((x, y)) = std::env::var("NATIVEFS_WINDOW_POS").ok().and_then(|v| {
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?))
    }) {
        ui.window().set_position(slint::PhysicalPosition::new(x, y));
    }

    // ── callbacks ───────────────────────────────────────────────────────────
    {
        let (shared, weak, handle) = (shared.clone(), weak.clone(), handle.clone());
        ui.on_login(move |user, pass| {
            let remember = true;
            let (shared, weak) = (shared.clone(), weak.clone());
            let (user, pass) = (user.to_string(), pass.to_string());
            on_ui(&weak, |ui| {
                ui.set_busy(true);
                ui.set_error_text("".into());
            });
            handle.spawn(async move {
                match shared.client.login(&user, &pass, remember).await {
                    Ok(_) => {
                        if remember {
                            if let Some(token) = shared.client.session_token() {
                                let s = StoredSession { username: user.clone(), token };
                                let _ = tokio::task::spawn_blocking(move || session::save(&s)).await;
                            }
                        } else {
                            let _ = tokio::task::spawn_blocking(session::clear).await;
                        }
                        if let Err(e) = enter_app(&shared, &weak).await {
                            show_login(&weak, &shared, e.to_string());
                        }
                    }
                    Err(e) => show_login(&weak, &shared, e.to_string()),
                }
            });
        });
    }
    {
        let (shared, weak, handle) = (shared.clone(), weak.clone(), handle.clone());
        ui.on_logout(move || {
            let (shared, weak) = (shared.clone(), weak.clone());
            handle.spawn(async move {
                let _ = shared.client.logout().await;
                let _ = tokio::task::spawn_blocking(session::clear).await;
                *shared.recent.lock().unwrap() = RecentFiles::default();
                shared.thumbs.lock().unwrap().clear();
                push_recent(&shared, &weak);
                show_login(&weak, &shared, String::new());
            });
        });
    }
    {
        let shared = shared.clone();
        ui.on_open_link(move |url| open_external(url.as_str(), &shared.config.lock().unwrap()));
    }
    ui.on_copy_link(|link| copy_to_clipboard(link.as_str()));
    ui.on_copy_styled(|name, link| copy_to_clipboard(&format!("[{name}]({link})")));
    {
        let weak = weak.clone();
        ui.on_pick_expiry(move |i| {
            if let Some(ui) = weak.upgrade() {
                ui.set_expiry(i);
                ui.set_expiry_date_text(expiry_date_text(i).into());
            }
        });
    }
    {
        let (shared, weak, handle) = (shared.clone(), weak.clone(), handle.clone());
        ui.on_delete_file(move |id| delete_recent(&shared, &weak, &handle, id.to_string()));
    }
    {
        let (shared, weak, handle) = (shared.clone(), weak.clone(), handle.clone());
        ui.on_confirm_answer(move |kind, id, yes| {
            let id = id.to_string();
            match kind {
                1 if yes => delete_recent(&shared, &weak, &handle, id),
                2 if yes => {
                    // a local resume key looks like "size|mtime"; anything else is another device's upload id
                    let upload_id = if id.contains('|') {
                        let rec = shared.uploader.resume_store().get(&id, now_ms());
                        shared.uploader.resume_store().remove(&id);
                        rec.and_then(|r| r.upload_id)
                    } else {
                        shared.pending.lock().unwrap().retain(|p| p.upload_id != id);
                        Some(id)
                    };
                    push_recent(&shared, &weak);
                    if let Some(uid) = upload_id {
                        let (shared, weak) = (shared.clone(), weak.clone());
                        handle.spawn(async move {
                            let _ = shared.client.delete_upload_chunks(&uid).await;
                            push_recent(&shared, &weak);
                        });
                    }
                }
                3 => {
                    if let Some(ui) = weak.upgrade() {
                        upload_ui::cancel_answer(&ui, yes);
                    }
                }
                _ => {}
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_forget_unfinished(move |key| {
            shared.uploader.resume_store().remove(&key);
            push_recent(&shared, &weak);
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_recent_toggle_more(move || {
            let v = !shared.recent_expanded.load(Ordering::Relaxed);
            shared.recent_expanded.store(v, Ordering::Relaxed);
            push_recent(&shared, &weak);
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_recent_open(move |item| {
            let Some(ui) = weak.upgrade() else { return };
            match item.kind {
                // NOT FINISHED: native extra, the file is still where it was, so queue it again (it resumes)
                1 => {
                    let rec = shared.uploader.resume_store().get(&item.key, now_ms());
                    if let Some(path) = rec.and_then(|r| r.path).filter(|p| p.is_file()) {
                        ui.set_mode(0);
                        upload_ui::add_paths(&ui, vec![path]);
                    }
                }
                2 => {}
                // the file itself in the native viewer (reader / player pages resolve to the same file)
                _ => {
                    let url = if item.link.is_empty() { item.tool_link.to_string() } else { item.link.to_string() };
                    open::open_or_browse(&shared, &url);
                }
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_copy_recent(move |id, text, kind| {
            copy_to_clipboard(&text);
            let Some(ui) = weak.upgrade() else { return };
            ui.set_recent_copied_id(id.clone());
            ui.set_recent_copied_kind(kind);
            let weak = weak.clone();
            slint::Timer::single_shot(Duration::from_millis(1200), move || {
                if let Some(ui) = weak.upgrade() {
                    if ui.get_recent_copied_id() == id && ui.get_recent_copied_kind() == kind {
                        ui.set_recent_copied_id("".into());
                    }
                }
            });
        });
    }
    // ── upload ──────────────────────────────────────────────────────────────
    {
        let weak = weak.clone();
        ui.on_pick_files(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::pick(&ui, ui.get_mode() == 1, None);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_pick_local(move || {
            let Some(ui) = weak.upgrade() else { return };
            let build = |d: rfd::AsyncFileDialog| d.set_title("Open locally").add_filter("Audio, video, text and images", &tvfs_core::content::viewer_extensions());
            // from the tray the main window may be hidden: then the dialog has no parent (and the window stays unmade)
            let dialog = if ui.window().is_visible() {
                ui.window().with_winit_window(|w| build(rfd::AsyncFileDialog::new().set_parent(w))).unwrap_or_else(|| build(rfd::AsyncFileDialog::new()))
            } else {
                build(rfd::AsyncFileDialog::new())
            };
            let (shared, weak) = (shared.clone(), weak.clone());
            let _ = slint::spawn_local(async move {
                let Some(file) = dialog.pick_file().await else { return };
                if let (Some(ui), Err(msg)) = (weak.upgrade(), open::open_local(&shared, file.path())) {
                    show_toast(&ui, msg);
                }
            });
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_capture(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            match i {
                // the device camera (Windows: Media Foundation, Linux: V4L2 + ffmpeg)
                0 | 1 if cfg!(any(windows, target_os = "linux")) => camera_ui::open(&shared, &ui, i == 1),
                // elsewhere what a desktop browser does for the web's capture inputs: a file picker
                0 => upload_ui::pick(&ui, false, Some(("Images", &["jpg", "jpeg", "png", "gif", "webp", "heic", "avif", "bmp"]))),
                1 => upload_ui::pick(&ui, false, Some(("Videos", &["mp4", "mov", "mkv", "webm", "avi", "m4v"]))),
                _ => voice::open(&ui, &shared),
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_voice(move |what| {
            if let Some(ui) = weak.upgrade() {
                voice::action(&ui, &shared, what);
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_voice_format_picked(move |i| voice::format_picked(&shared, i));
    }
    {
        let weak = weak.clone();
        ui.on_voice_seek(move |at| {
            if let Some(ui) = weak.upgrade() {
                voice::seek(&ui, at);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_queue_rename(move |i, t| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::queue_rename(&ui, i.max(0) as usize, &t);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_queue_anon(move |i| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::queue_anon(&ui, i.max(0) as usize);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_queue_remove(move |i| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::queue_remove(&ui, i.max(0) as usize);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_queue_clear(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::queue_clear(&ui);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_start_upload(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::start_upload(&ui, &shared);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_cancel_clicked(move |shift| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::cancel_clicked(&ui, shift);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_stats_dismiss(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::hide_stats(&ui);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_copy_result(move |i| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::copy_result(&ui, i);
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_open_result(move |url| open::open_or_browse(&shared, &url));
    }
    {
        let weak = weak.clone();
        ui.on_copy_batch(move |row, text, kind| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::copy_batch(&ui, row, &text, kind);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_text_edited(move |t| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::text_edited(&ui, &t);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_text_mode_pick(move |md| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::text_mode_pick(&ui, md);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_add_text(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::add_text(&ui);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_paste_files(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::paste(&ui);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_check_probe(move |url| {
            if let Some(ui) = weak.upgrade() {
                check_ui::probe_clicked(&ui, &shared, &url);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_check_opened(move || {
            if let Some(ui) = weak.upgrade() {
                check_ui::opened(&ui);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_check_paste(move || {
            if let Some(ui) = weak.upgrade() {
                check_ui::paste(&ui);
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_users_only_changed(move |on| {
            let mut cfg = shared.config.lock().unwrap();
            cfg.tvfs_users_only = on;
            let _ = cfg.save();
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_chunk_mode_changed(move |auto| {
            if let Some(ui) = weak.upgrade() {
                upload_ui::chunk_mode_changed(&ui, &shared, auto);
            }
        });
    }
    {
        let weak = weak.clone();
        ui.on_chunk_inputs_edited(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::chunk_inputs_edited(&ui);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_chunk_inputs_commit(move || {
            if let Some(ui) = weak.upgrade() {
                upload_ui::chunk_inputs_commit(&ui, &shared);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_auto_copy_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut cfg = shared.config.lock().unwrap();
            cfg.auto_copy = match ui.get_auto_copy() {
                0 => tvfs_core::config::AutoCopy::Off,
                1 => tvfs_core::config::AutoCopy::Normal,
                _ => tvfs_core::config::AutoCopy::Styled,
            };
            cfg.auto_copy_prefer_tool = ui.get_prefer_tool();
            cfg.auto_copy_image_styled = ui.get_image_styled();
            let _ = cfg.save();
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_zoom_step(move |dir| {
            if let Some(ui) = weak.upgrade() {
                zoom_step(&ui, &shared, dir);
            }
        });
    }
    {
        let shared = shared.clone();
        ui.on_viewer_zoom_reset(move |i| {
            match i {
                0 => reader_ui::reset_zoom(&shared),
                1 => windows::reset_zoom(&shared, |c| &mut c.audio_zoom),
                _ => windows::reset_zoom(&shared, |c| &mut c.player_zoom),
            }
            refresh_viewer_zooms(&shared);
        });
    }
    refresh_viewer_zooms(&shared);
    {
        let shared = shared.clone();
        ui.on_theme_changed(move |light| {
            let mut cfg = shared.config.lock().unwrap();
            cfg.light_theme = light;
            if let Err(e) = cfg.save() {
                eprintln!("naTiVeFS: could not save theme: {e}");
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_save_settings(move |close_to_tray, browser| {
            let mut cfg = shared.config.lock().unwrap();
            cfg.close_to_tray = close_to_tray;
            let browser = browser.trim().to_owned();
            cfg.browser = if browser.is_empty() { None } else { Some(browser) };
            if let Err(e) = cfg.save() {
                eprintln!("naTiVeFS: could not save settings: {e}");
            }
            drop(cfg);
            if let Some(ui) = weak.upgrade() {
                refresh_router(&ui, &shared);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_file_menus_changed(move |on| {
            let Some(ui) = weak.upgrade() else { return };
            let result = if on { shellmenu::install() } else { shellmenu::uninstall(); Ok(()) };
            if let Err(e) = &result {
                eprintln!("naTiVeFS: file manager menus: {e}");
                show_toast(&ui, "Could not change the file manager menu.");
            }
            ui.set_file_menus(on && result.is_ok());
            let mut cfg = shared.config.lock().unwrap();
            cfg.file_menus = on && result.is_ok();
            if let Err(e) = cfg.save() {
                eprintln!("naTiVeFS: could not save settings: {e}");
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_router_toggle(move |on| {
            if let Some(ui) = weak.upgrade() {
                router_switch(&ui, &shared, on);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_settings_opened(move || {
            if let Some(ui) = weak.upgrade() {
                refresh_router(&ui, &shared);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_pick_browser(move |i| {
            let Some(ui) = weak.upgrade() else { return };
            let cmd = BROWSERS.with(|b| if i >= 0 { b.borrow().get(i as usize).map(|b| b.command.clone()) } else { None });
            {
                let mut cfg = shared.config.lock().unwrap();
                cfg.browser = cmd.clone();
                if let Err(e) = cfg.save() {
                    eprintln!("naTiVeFS: could not save settings: {e}");
                }
            }
            ui.set_browser_cmd(cmd.unwrap_or_default().into());
            refresh_router(&ui, &shared);
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_router_offer_answer(move |yes| {
            let Some(ui) = weak.upgrade() else { return };
            ui.set_router_offer(false);
            {
                let mut cfg = shared.config.lock().unwrap();
                cfg.router_offered = true;
                if let Err(e) = cfg.save() {
                    eprintln!("naTiVeFS: could not save settings: {e}");
                }
            }
            if yes {
                router_switch(&ui, &shared, true);
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_startup_changed(move |on, background| {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = autostart::set(on) {
                eprintln!("naTiVeFS: autostart: {e}");
                show_toast(&ui, "Could not change the start with the computer.");
            }
            ui.set_autostart(autostart::enabled());
            ui.set_start_background(background);
            let mut cfg = shared.config.lock().unwrap();
            cfg.start_minimized = background;
            if let Err(e) = cfg.save() {
                eprintln!("naTiVeFS: could not save settings: {e}");
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_appearance_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let look = ui.global::<Look>();
            let motion = Motion::from_index(ui.get_motion().max(0) as usize);
            {
                let mut cfg = shared.config.lock().unwrap();
                cfg.glass = look.get_glass();
                cfg.smooth_scroll = look.get_smooth_scroll();
                cfg.motion = motion;
                if let Err(e) = cfg.save() {
                    eprintln!("naTiVeFS: could not save appearance: {e}");
                }
            }
            ANIM.with(|a| {
                if let Some(a) = a.borrow_mut().as_mut() {
                    a.motion = motion;
                }
            });
            ui.window().request_redraw();
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_update_prefs_changed(move || {
            let Some(ui) = weak.upgrade() else { return };
            let mut cfg = shared.config.lock().unwrap();
            cfg.update_channel = if ui.get_update_beta() { UpdateChannel::Beta } else { UpdateChannel::Stable };
            cfg.check_updates = ui.get_update_auto();
            if let Err(e) = cfg.save() {
                eprintln!("naTiVeFS: could not save update settings: {e}");
            }
        });
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.on_check_updates(move || check_updates(&shared, &weak));
    }
    {
        let shared = shared.clone();
        ui.window().on_close_requested(move || {
            shared.window_visible.store(false, Ordering::Relaxed);
            shared.main_shown.store(false, Ordering::Relaxed);
            let keep_running = shared.config.lock().unwrap().close_to_tray && shared.tray_ok.load(Ordering::Relaxed);
            // viewers (a video playing, a reader) stay open; the app quits with the last window
            if !keep_running && windows::live() == 0 {
                let _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }
    {
        let shared = shared.clone();
        windows::on_last_closed(move || quit_if_idle(&shared));
    }

    // ── animated background + raw input ────────────────────────────────────
    ANIM.with(|a| {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(7);
        *a.borrow_mut() = Some(Anim {
            backdrop: bg::Backdrop::new(config.light_theme, seed),
            motion: config.motion,
            last_tick: Instant::now(),
            pending: false,
            rendered: None,
            refresh_hz: 60.0,
            refresh_checked: None,
            frames: 0,
            frames_since: Instant::now(),
            speed_target: 1.0,
            speed_current: 1.0,
        });
    });
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.window().set_rendering_notifier(move |state, _| {
            if let slint::RenderingState::AfterRendering = state {
                schedule_backdrop(&weak, &shared);
            }
        })?;
    }
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        ui.window().on_winit_window_event(move |w, ev| match weak.upgrade() {
            Some(ui) => on_window_event(&ui, &shared, w, ev),
            None => EventResult::Propagate,
        });
    }

    // ── IPC (second launches, link router, OS menus) ───────────────────────
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        let _ipc = nativefs_ipc::serve(listener, move |msg| {
            let shared = shared.clone();
            on_ui(&weak, move |ui| match msg {
                Message::Ping => {}
                Message::Show => show_window(ui, &shared),
                Message::Open(url) => {
                    // links open in their own window; the main window stays as it is
                    let _ = ui;
                    if !open::open_link(&shared, &url) {
                        show_window(ui, &shared);
                    }
                }
                Message::Upload(paths) => {
                    show_window(ui, &shared);
                    if ui.get_mode() == 3 || ui.get_mode() == 2 {
                        ui.set_mode(0);
                    }
                    upload_ui::add_paths(ui, paths.into_iter().map(PathBuf::from).collect());
                }
                Message::OpenLocal(path) => {
                    if let Err(msg) = open::open_local(&shared, std::path::Path::new(&path)) {
                        show_window(ui, &shared);
                        show_toast(ui, msg);
                    }
                }
                Message::Quit => {
                    let _ = slint::quit_event_loop();
                }
            });
        });
    }

    // ── tray: created inside the event loop (required on macOS/Windows) ────
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        slint::Timer::single_shot(Duration::ZERO, move || {
            let on_tray: tray::Callback = {
                let (shared, weak) = (shared.clone(), weak.clone());
                Arc::new(move |a| {
                    let shared = shared.clone();
                    on_ui(&weak, move |ui| tray_action(ui, &shared, a));
                })
            };
            let guard = tray::start(on_tray);
            shared.tray_ok.store(guard.is_some(), Ordering::Relaxed);
            let has_tray = guard.is_some();
            TRAY.with(|t| *t.borrow_mut() = guard);
            if let Some(ui) = weak.upgrade() {
                ui.set_tray_enabled(has_tray);
                // debug builds: `NATIVEFS_STARTUP=<autostart 0|1>,<background 0|1>` sets the startup toggles like a
                // click; `NATIVEFS_TRAY=<action>[,<action>...]` runs tray menu actions one second apart
                #[cfg(debug_assertions)]
                if let Some((a, b)) = std::env::var("NATIVEFS_STARTUP").ok().and_then(|v| v.split_once(',').map(|(a, b)| (a == "1", b == "1"))) {
                    ui.invoke_startup_changed(a, b);
                }
                // `NATIVEFS_ROUTER=0|1` switches the link router like the settings toggle
                #[cfg(debug_assertions)]
                if let Ok(v) = std::env::var("NATIVEFS_ROUTER") {
                    ui.invoke_router_toggle(v == "1");
                }
                #[cfg(debug_assertions)]
                for (n, name) in std::env::var("NATIVEFS_TRAY").unwrap_or_default().split(',').filter(|s| !s.is_empty()).enumerate() {
                    use tray::Action::*;
                    let a = match name {
                        "show" => Show, "play" => PlayPause, "stop" => Stop, "local" => OpenLocal, "upload" => Upload,
                        "clip" => ClipboardLink, "voice" => VoiceNote, "settings" => Settings, "quit" => Quit,
                        _ => continue,
                    };
                    let (weak, shared) = (weak.clone(), shared.clone());
                    slint::Timer::single_shot(Duration::from_millis(1500 + 1000 * n as u64), move || {
                        if let Some(ui) = weak.upgrade() {
                            tray_action(&ui, &shared, a);
                        }
                    });
                }
            }
            // debug builds: `NATIVEFS_CAMERA=photo|video` opens the camera window (tests without clicking)
            #[cfg(debug_assertions)]
            if let Ok(kind) = std::env::var("NATIVEFS_CAMERA") {
                if let Some(ui) = weak.upgrade() {
                    camera_ui::open(&shared, &ui, kind == "video");
                }
            }
            // debug builds: `NATIVEFS_LOCAL_FILE=<path>` (or the older `NATIVEFS_AUDIO_FILE`) opens a local file like the
            // "Use TVFS tools locally" card
            #[cfg(debug_assertions)]
            // (several paths separated by `;` open one after another)
            if let Some(paths) = std::env::var("NATIVEFS_LOCAL_FILE").ok().or_else(|| std::env::var("NATIVEFS_AUDIO_FILE").ok()).filter(|p| !p.is_empty()) {
                for path in paths.split(';').filter(|p| !p.is_empty()) {
                    if let Err(msg) = open::open_local(&shared, std::path::Path::new(path)) {
                        eprintln!("NATIVEFS_LOCAL_FILE: {msg}");
                    }
                }
            }
            // debug builds: `NATIVEFS_VOICE=<seconds>` opens the voice recorder in Create mode, records that long, then
            // uses the recording (it lands in the upload queue; nothing is uploaded) - screenshots without clicking
            #[cfg(debug_assertions)]
            // (`NATIVEFS_VOICE=<seconds>,listen` plays it back instead of using it)
            if let Some(secs) = std::env::var("NATIVEFS_VOICE").ok().and_then(|v| v.split(',').next()?.parse::<u64>().ok()) {
                if let Some(ui) = weak.upgrade() {
                    ui.set_mode(2);
                    voice::open(&ui, &shared);
                    let last = if std::env::var("NATIVEFS_VOICE").is_ok_and(|v| v.ends_with(",listen")) { 5 } else { 2 };
                    let steps: [(u64, i32); 3] = [(1500, 0), (1500 + secs * 1000, 1), (4500 + secs * 1000, last)];
                    for (ms, what) in steps {
                        let (weak, shared) = (weak.clone(), shared.clone());
                        slint::Timer::single_shot(Duration::from_millis(ms), move || {
                            if let Some(ui) = weak.upgrade() {
                                voice::action(&ui, &shared, what);
                            }
                        });
                    }
                }
            }
            // a link on the command line (link router, "Open with") opens only its own window, like a browser tab
            if let Some(path) = &local_arg {
                if let Err(msg) = open::open_local(&shared, std::path::Path::new(path)) {
                    if let Some(ui) = weak.upgrade() {
                        show_window(&ui, &shared);
                        show_toast(&ui, msg);
                    }
                }
            }
            if let Some(url) = &url_arg {
                open::open_link(&shared, url);
            }
            // No tray available: never leave the app invisible.
            if url_arg.is_none() && local_arg.is_none() && (!start_minimized || !has_tray) && !shared.window_visible.load(Ordering::Relaxed) {
                if let Some(ui) = weak.upgrade() {
                    show_window(&ui, &shared);
                }
            }
            // a window started hidden (autostart in the tray) is measured when it is first shown: touching the native
            // window here would create it at winit's default 800 x 600 instead of the page's preferred size
            if let Some(ui) = weak.upgrade().filter(|_| shared.main_shown.load(Ordering::Relaxed)) {
                apply_zoom(&ui, &shared);
                update_refresh_hz(&ui);
            }
        });
    }

    // ── restore the saved session ──────────────────────────────────────────
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        handle.spawn(async move {
            let saved = tokio::task::spawn_blocking(session::load).await.ok().and_then(|r| r.ok()).flatten();
            let Some(saved) = saved else { return };
            let name = saved.username.clone();
            on_ui(&weak, move |ui| ui.set_username(name.into()));
            shared.client.restore_session(&saved.token);
            if enter_app(&shared, &weak).await.is_err() {
                show_login(&weak, &shared, String::new());
            }
        });
    }

    // ── status poll: every POLL while the window is visible, immediately when it
    //    is shown or focused; a hidden window means no wake-ups at all ──────
    {
        let (shared, weak) = (shared.clone(), weak.clone());
        handle.spawn(async move {
            loop {
                if shared.window_visible.load(Ordering::Relaxed) {
                    tokio::select! {
                        _ = tokio::time::sleep(POLL) => {}
                        _ = shared.wake.notified() => {}
                    }
                } else {
                    shared.wake.notified().await;
                }
                if shared.window_visible.load(Ordering::Relaxed) && shared.logged_in.load(Ordering::Relaxed) {
                    if let Err(e) = refresh(&shared, &weak).await {
                        if e.is_auth() {
                            show_login(&weak, &shared, "Session expired, please log in again.".into());
                        }
                    }
                } else if shared.window_visible.load(Ordering::Relaxed) {
                    // logged out: the "System" chip still shows whether the server answers ("not logged in" is an answer)
                    let online = !matches!(shared.client.status().await, Err(Error::Network(_)));
                    on_ui(&weak, move |ui| ui.set_backend_online(online));
                }
            }
        });
    }

    if config.check_updates {
        let (shared, weak) = (shared.clone(), weak.clone());
        slint::Timer::single_shot(Duration::from_secs(3), move || check_updates(&shared, &weak));
    }

    slint::run_event_loop_until_quit()?;
    rt.shutdown_background();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::next_zoom;

    #[test]
    fn zoom_steps_like_a_browser() {
        assert_eq!(next_zoom(1.0, 1), 1.1);
        assert_eq!(next_zoom(1.0, -1), 0.9);
        assert_eq!(next_zoom(1.1, 1), 1.25);
        assert_eq!(next_zoom(2.5, 0), 1.0);
        // the ends stay put
        assert_eq!(next_zoom(3.0, 1), 3.0);
        assert_eq!(next_zoom(0.5, -1), 0.5);
        // a hand-edited value between levels goes to the next level in that direction
        assert_eq!(next_zoom(1.2, 1), 1.25);
        assert_eq!(next_zoom(1.2, -1), 1.1);
    }
}
