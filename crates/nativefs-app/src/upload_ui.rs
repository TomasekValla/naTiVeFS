//! The upload part of the page (web: file selection, `uploadAll`, the progress
//! box, the result box, auto copy). The engine itself is `tvfs_core::upload`;
//! this module only connects it to the Slint UI.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, Model, ModelRc, VecModel, Weak};
use tvfs_core::config::AutoCopy;
use tvfs_core::fmt::{fmt_bytes, fmt_speed_mbps, fmt_speed_mbs, fmt_speed_mbs_parts, fmt_time};
use tvfs_core::models::{BatchCompleteResponse, ProtectedShareResponse, UploadCompleteResponse};
use tvfs_core::upload::plan::{commit_fixed, evaluate_fixed, ChunkMode, FixedWarning};
use tvfs_core::upload::stats::{ComparisonKind, StatsReport};
use tvfs_core::upload::{inspect, walk_folder, Control, Event, Outcome, Phase, Run, RunReport, RunStatus, UploadFile, UploadRequest};

use crate::{copy_to_clipboard, on_ui, push_recent, LocalShare, Shared};
use crate::{BatchRow, FileProgressRow, MainWindow, MetricTile, PillState, QueueRow};

/// What the result box currently shows (for the copy buttons and auto copy).
#[derive(Default)]
struct ResultState {
    link: String,
    styled: Option<String>,
    reader: String,
    reader_styled: Option<String>,
    player: String,
    player_styled: Option<String>,
    /// The styled link is still being made on the server; copy it as soon as it arrives.
    pending_auto_styled: bool,
    /// Increases with every new result, so late styled-link polls of an old one are ignored.
    generation: u64,
    base_link_label: String,
    base_styled_label: String,
}

#[derive(Default)]
struct UiState {
    queue: Vec<UploadFile>,
    run: Option<Arc<Run>>,
    timer: Option<slint::Timer>,
    result: ResultState,
    /// Text-mode files written to the temp folder (removed after the upload).
    temp_files: Vec<PathBuf>,
    text_md_manual: bool,
    /// The Cancel button paused the upload; remembers whether it was paused before.
    paused_before_cancel: bool,
    stats_timer: Option<slint::Timer>,
    chunk_msg_timer: Option<slint::Timer>,
}

thread_local! {
    static STATE: RefCell<UiState> = RefCell::new(UiState::default());
}

fn with_state<R>(f: impl FnOnce(&mut UiState) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

// ── queue ───────────────────────────────────────────────────────────────────

/// The web's extension badge text: upper case, shortened above 6 characters.
fn ext_label(ext: &str) -> String {
    let e: String = ext.trim_start_matches('.').to_uppercase();
    if e.chars().count() > 6 {
        format!("{}…", e.chars().take(5).collect::<String>())
    } else if e.is_empty() {
        "?".into()
    } else {
        e
    }
}

fn shown_base(f: &UploadFile) -> String {
    match &f.anon_name {
        Some(a) => a.strip_suffix(&f.ext).unwrap_or(a).to_owned(),
        None => f.base.clone(),
    }
}

pub fn push_queue(ui: &MainWindow) {
    with_state(|s| {
        let rows: Vec<QueueRow> = s
            .queue
            .iter()
            .map(|f| QueueRow {
                base: shown_base(f).into(),
                ext_label: ext_label(&f.ext).into(),
                size: fmt_bytes(f.size).into(),
                anon: f.anon_name.is_some(),
            })
            .collect();
        let total: u64 = s.queue.iter().map(|f| f.size).sum();
        ui.set_queue(ModelRc::new(VecModel::from(rows)));
        ui.set_queue_total(format!("Total: {}", fmt_bytes(total)).into());
    });
}

/// Add local paths (files or folders) to the queue, like the web's `addFiles` / folder drop.
/// Files are queued or uploading (an automatic update must wait).
pub fn busy() -> bool {
    with_state(|s| s.run.is_some() || !s.queue.is_empty())
}

pub fn add_paths(ui: &MainWindow, paths: Vec<PathBuf>) {
    let mut errors = Vec::new();
    let mut added = Vec::new();
    for p in paths {
        if p.is_dir() {
            let (files, rejected) = walk_folder(&p);
            added.extend(files);
            errors.extend(rejected.into_iter().map(|r| r.to_string()));
        } else {
            match inspect(&p, None) {
                Ok(f) => added.push(f),
                Err(r) => errors.push(r.to_string()),
            }
        }
    }
    if with_state(|s| s.run.is_some()) {
        // the web locks the file list while uploading
        return;
    }
    with_state(|s| s.queue.extend(added));
    ui.set_editing_index(-1);
    push_queue(ui);
    ui.set_upload_error(errors.join("\n").into());
}

pub fn queue_rename(ui: &MainWindow, i: usize, text: &str) {
    with_state(|s| {
        if let Some(f) = s.queue.get_mut(i) {
            f.rename(text);
        }
    });
    push_queue(ui);
}

pub fn queue_anon(ui: &MainWindow, i: usize) {
    with_state(|s| {
        if let Some(f) = s.queue.get_mut(i) {
            let on = f.anon_name.is_none();
            f.set_anonymized(on);
        }
    });
    push_queue(ui);
}

pub fn queue_remove(ui: &MainWindow, i: usize) {
    with_state(|s| {
        if i < s.queue.len() {
            s.queue.remove(i);
        }
    });
    push_queue(ui);
    ui.set_upload_error("".into());
}

pub fn queue_clear(ui: &MainWindow) {
    with_state(|s| s.queue.clear());
    ui.set_queue_scroll(0.0);
    push_queue(ui);
    ui.set_upload_error("".into());
}

/// Open the native file / folder picker (mode 1 = Folders). `filter`: photo / video pickers of Create mode.
pub fn pick(ui: &MainWindow, folders: bool, filter: Option<(&'static str, &'static [&'static str])>) {
    let build = |d: rfd::AsyncFileDialog| match filter {
        Some((name, exts)) => d.add_filter(name, exts),
        None => d,
    };
    let dialog = ui
        .window()
        .with_winit_window(|w| build(rfd::AsyncFileDialog::new().set_parent(w)))
        .unwrap_or_else(|| build(rfd::AsyncFileDialog::new()));
    let weak = ui.as_weak();
    let _ = slint::spawn_local(async move {
        let picked: Vec<PathBuf> = if folders {
            dialog.pick_folder().await.map(|h| vec![h.path().to_path_buf()]).unwrap_or_default()
        } else {
            dialog.pick_files().await.map(|v| v.iter().map(|h| h.path().to_path_buf()).collect()).unwrap_or_default()
        };
        if let Some(ui) = weak.upgrade() {
            if !picked.is_empty() {
                add_paths(&ui, picked);
            }
        }
    });
}

/// Ctrl+V outside a text field: files copied in the file manager, or an image (screenshot).
pub fn paste(ui: &MainWindow) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return };
    if let Ok(files) = cb.get().file_list() {
        if !files.is_empty() {
            add_paths(ui, files);
            return;
        }
    }
    if let Ok(img) = cb.get_image() {
        let Some(buf) = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()) else { return };
        // a pasted screenshot arrives in the browser as "image.png"
        let Some(dir) = temp_subdir() else { return };
        let path = dir.join("image.png");
        if buf.save_with_format(&path, image::ImageFormat::Png).is_ok() {
            with_state(|s| s.temp_files.push(path.clone()));
            add_paths(ui, vec![path]);
        }
    }
}

/// A fresh folder under `%TEMP%/naTiVeFS/` for generated files (text mode, pasted images).
fn temp_subdir() -> Option<PathBuf> {
    let base = std::env::temp_dir().join("naTiVeFS");
    let dir = base.join(format!("{}-{}", std::process::id(), fastrand_u64()));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn fastrand_u64() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
}

// ── text mode ───────────────────────────────────────────────────────────────

/// The web's Magix auto-detection: `(?:^|\n)(#{1,6} |[*_]{1,2}[^\s*_]|```|\|.+\||-{3,}|\* |\d+\. )`.
pub fn looks_like_markdown(text: &str) -> bool {
    text.split('\n').any(|line| {
        let l = line;
        let hashes = l.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && l[hashes..].starts_with(' ') {
            return true;
        }
        let marks = l.chars().take_while(|c| *c == '*' || *c == '_').count();
        if (1..=2).contains(&marks) {
            if let Some(c) = l[marks..].chars().next() {
                if !c.is_whitespace() && c != '*' && c != '_' {
                    return true;
                }
            }
        }
        if l.starts_with("```") || l.starts_with("---") || l.starts_with("* ") {
            return true;
        }
        if let Some(rest) = l.strip_prefix('|') {
            // `\|.+\|` : at least one character, then another pipe on the same line
            if rest.len() > 1 && rest[1..].contains('|') {
                return true;
            }
        }
        let digits = l.chars().take_while(|c| c.is_ascii_digit()).count();
        digits > 0 && l[digits..].starts_with(". ")
    })
}

pub fn text_edited(ui: &MainWindow, text: &str) {
    ui.set_text_size(fmt_bytes(text.len() as u64).into());
    if !with_state(|s| s.text_md_manual) {
        ui.set_text_md(looks_like_markdown(text));
    }
}

pub fn text_mode_pick(ui: &MainWindow, md: bool) {
    with_state(|s| s.text_md_manual = true);
    ui.set_text_md_manual(true);
    ui.set_text_md(md);
}

/// "Add as a file": write the text to a temp file and queue it.
pub fn add_text(ui: &MainWindow) {
    let text = ui.get_text_content().to_string();
    if text.trim().is_empty() {
        ui.set_upload_error("Text cannot be empty".into());
        return;
    }
    let raw = ui.get_text_name().to_string();
    let mut name = raw.trim().to_owned();
    for ext in [".txt", ".md", ".TXT", ".MD"] {
        if let Some(n) = name.strip_suffix(ext) {
            name = n.trim().to_owned();
        }
    }
    if name.is_empty() {
        const C: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut seed = fastrand_u64();
        name = (0..8)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                C[(seed >> 33) as usize % C.len()] as char
            })
            .collect();
    }
    let ext = if ui.get_text_md() { ".md" } else { ".txt" };
    let Some(dir) = temp_subdir() else { return };
    let path = dir.join(format!("{name}{ext}"));
    if let Err(e) = std::fs::write(&path, text.as_bytes()) {
        ui.set_upload_error(format!("Could not create the text file: {e}").into());
        return;
    }
    with_state(|s| {
        s.temp_files.push(path.clone());
        s.text_md_manual = false;
    });
    add_paths(ui, vec![path]);
    ui.set_text_name("".into());
    ui.set_text_content("".into());
    ui.set_text_size("0 B".into());
    ui.set_text_md(false);
    ui.set_text_md_manual(false);
    ui.set_text_scroll(0.0);
    ui.set_mode(0);
}

// ── running an upload ───────────────────────────────────────────────────────

fn chunk_mode(shared: &Shared) -> ChunkMode {
    let cfg = shared.config.lock().unwrap();
    if cfg.chunk_auto {
        ChunkMode::Auto
    } else {
        ChunkMode::Fixed { count: cfg.chunk_count, size_mb: cfg.chunk_size_mb }
    }
}

pub fn start_upload(ui: &MainWindow, shared: &Arc<Shared>) {
    if with_state(|s| s.run.is_some()) {
        return;
    }
    let files = with_state(|s| s.queue.clone());
    if files.is_empty() {
        ui.set_upload_error("Select files first".into());
        return;
    }
    let protected = shared.config.lock().unwrap().tvfs_users_only;
    let expiration_minutes = tvfs_core::EXPIRATION_OPTIONS_MIN.get(ui.get_expiry().max(0) as usize).copied().unwrap_or(tvfs_core::DEFAULT_EXPIRATION_MIN);
    let req = UploadRequest {
        files: files.clone(),
        expiration_minutes,
        remove_timestamp: shared.tier.load(Ordering::Relaxed) == 2,
        chunking: chunk_mode(shared),
        protected,
    };
    let run = Run::new(files.iter().map(|f| (f.upload_name(), f.size)), Control::default());
    reset_result(ui);
    hide_stats(ui);
    ui.set_upload_error("".into());
    ui.set_editing_index(-1);
    ui.set_upload_cancelled(false);
    ui.set_uploading(true);
    ui.set_prog_status(if protected { format!("Uploading {} file{}…", files.len(), if files.len() == 1 { "" } else { "s" }) } else { "Uploading...".into() }.into());
    ui.set_prog_dots(false);
    ui.set_prog_frac(0.0);
    ui.set_prog_pct("0%".into());
    ui.set_prog_size("0 B / 0 B".into());
    ui.set_prog_eta("ETA: --".into());
    ui.set_prog_te("TE: 0s".into());
    ui.set_prog_mbps("-- Mbps".into());
    ui.set_prog_avg("AVG: -- MB/s".into());
    let multi = files.len() > 1 && !protected;
    let rows: Vec<FileProgressRow> = if multi {
        files.iter().map(|f| FileProgressRow { name: f.upload_name().into(), pct: 0.0, state: 0, status: "wait".into() }).collect()
    } else {
        Vec::new()
    };
    ui.set_per_file(ModelRc::new(VecModel::from(rows)));
    with_state(|s| s.run = Some(run.clone()));
    *shared.active_run.lock().unwrap() = Some(run.clone());
    start_render_timer(ui, shared);

    let (shared2, weak) = (shared.clone(), ui.as_weak());
    let events: Arc<dyn Fn(Event) + Send + Sync> = {
        let shared = shared.clone();
        Arc::new(move |e| {
            if let Event::FileDone(_, resp) = &e {
                shared.remember_tool_link(resp);
                // the server list changes: refresh the recent list (only happens while visible)
                shared.wake.notify_one();
            }
        })
    };
    shared.rt.spawn(async move {
        let report = shared2.uploader.run(req, run, events).await;
        let shared3 = shared2.clone();
        on_ui(&weak, move |ui| finish(ui, &shared3, report));
    });
}

/// Progress redraws: at the web's 300-600 ms cadence, only while the window is visible.
pub fn start_render_timer(ui: &MainWindow, shared: &Arc<Shared>) {
    let Some(run) = with_state(|s| s.run.clone()) else { return };
    if !shared.window_visible.load(Ordering::Relaxed) {
        return;
    }
    let timer = slint::Timer::default();
    let (weak, shared) = (ui.as_weak(), shared.clone());
    render(&weak, &shared, &run);
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(500), move || {
        render(&weak, &shared, &run);
    });
    with_state(|s| s.timer = Some(timer));
}

/// The window was hidden: stop redrawing (the upload itself continues).
pub fn stop_render_timer() {
    with_state(|s| s.timer = None);
}

/// Live speed -> progress bar colour (web `updateSpeedVisuals`: red at <= 1 Mbps, accent at >= 330 Mbps).
fn speed_factor(mbps: f64) -> f64 {
    if mbps <= 1.0 {
        0.0
    } else if mbps >= 330.0 {
        1.0
    } else {
        (mbps / 1.0).ln() / 330f64.ln()
    }
}

fn speed_color(mbs: f64, light: bool) -> slint::Color {
    let f = speed_factor(mbs * 8.0);
    let hi: [f64; 3] = if light { [0.0, 168.0, 134.0] } else { [47.0, 255.0, 196.0] };
    let lo = [255.0, 71.0, 87.0];
    let c = |i: usize| (lo[i] + (hi[i] - lo[i]) * f).round() as u8;
    slint::Color::from_rgb_u8(c(0), c(1), c(2))
}

fn render(weak: &Weak<MainWindow>, shared: &Arc<Shared>, run: &Arc<Run>) {
    let Some(ui) = weak.upgrade() else { return };
    if !shared.window_visible.load(Ordering::Relaxed) {
        stop_render_timer();
        return;
    }
    let s = run.snapshot();
    let n = s.files.len();
    let frac = if s.total > 0 { s.done as f64 / s.total as f64 } else { 0.0 };
    let pct = (frac * 100.0).round() as u32;
    let protected = shared.config.lock().unwrap().tvfs_users_only && n > 0;
    let (status, dots) = if s.cancelled {
        ("Cancelled".to_string(), false)
    } else if s.paused {
        (if s.requests > 0 { "Finishing current chunk…".to_string() } else { "Paused".to_string() }, false)
    } else {
        match s.status {
            RunStatus::Reconnecting => ("Reconnecting".to_string(), true),
            RunStatus::Busy => ("Server busy, slowing down".to_string(), true),
            RunStatus::Resuming { chunks, total } => (
                match total {
                    Some(t) => format!("Resuming — {chunks}/{t} chunks already uploaded"),
                    None => format!("Resuming — {chunks} chunks already uploaded"),
                },
                false,
            ),
            _ if ui.get_per_file().row_count() > 1 => (format!("Uploading file {}/{} — overall {pct}%", s.current_file + 1, n), false),
            _ if protected => (ui.get_prog_status().to_string(), false),
            _ => ("Uploading...".to_string(), false),
        }
    };
    ui.set_prog_status(status.into());
    ui.set_prog_dots(dots);
    ui.set_prog_frac(frac as f32);
    ui.set_prog_pct(format!("{pct}%").into());
    ui.set_prog_size(format!("{} / {}", fmt_bytes(s.done), fmt_bytes(s.total)).into());
    ui.set_prog_te(format!("TE: {}", fmt_time(s.elapsed_s)).into());
    let light = ui.global::<crate::Palette>().get_light();
    match s.speed_mbs {
        Some(mbs) => {
            let eta = s.eta_s.map_or("--".to_string(), fmt_time);
            ui.set_prog_eta(format!("{} • ETA: {eta}", fmt_speed_mbs(mbs)).into());
            ui.set_prog_mbps(fmt_speed_mbps(mbs).into());
            crate::set_anim_speed(0.08 + (3.2 - 0.08) * speed_factor(mbs * 8.0));
            if !s.resumed_flash && !matches!(s.status, RunStatus::Reconnecting | RunStatus::Busy) {
                ui.set_prog_color(speed_color(mbs, light));
            }
        }
        None => ui.set_prog_eta("Calculating…".into()),
    }
    ui.set_prog_avg(s.avg_mbs.map_or("AVG: -- MB/s".to_string(), |a| format!("AVG: {}", fmt_speed_mbs(a))).into());
    if matches!(s.status, RunStatus::Reconnecting | RunStatus::Busy) {
        ui.set_prog_color(slint::Color::from_rgb_u8(0xe1, 0x70, 0x55));
    } else if s.resumed_flash {
        ui.set_prog_color(slint::Color::from_rgb_u8(0xa2, 0x9b, 0xfe));
    }
    // per-file rows
    let model = ui.get_per_file();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<FileProgressRow>>() {
        for (i, (phase, bytes, size)) in s.files.iter().enumerate() {
            let Some(mut row) = vm.row_data(i) else { continue };
            let p = if *size > 0 { *bytes as f32 / *size as f32 } else { 1.0 };
            let (state, status) = match phase {
                Phase::Waiting => (0, "wait".to_string()),
                Phase::Active => (1, format!("{}%", (p * 100.0).round() as u32)),
                Phase::Done => (2, "✓".to_string()),
                Phase::Failed => (3, "✗".to_string()),
            };
            if row.state != state || row.status.as_str() != status || (row.pct - p).abs() > 0.001 {
                row.state = state;
                row.status = status.into();
                row.pct = p;
                vm.set_row_data(i, row);
            }
        }
    }
    // the window title shows the progress, like the web's tab title
    let eta = s.eta_s.map_or_else(|| if s.speed_mbs.is_some() { "--".to_string() } else { "Calculating…".to_string() }, fmt_time);
    ui.set_title_text(
        format!(
            "({pct}% | {:.1}/{:.1} MB | {} / {eta}) naTiVeFS",
            s.done as f64 / 1048576.0,
            s.total as f64 / 1048576.0,
            fmt_time(s.elapsed_s)
        )
        .into(),
    );
    // live plan on the settings card
    if let Some(plan) = s.plan {
        if shared.config.lock().unwrap().chunk_auto {
            ui.set_chunk_auto_text(plan.describe().into());
        }
    }
}

fn finish(ui: &MainWindow, shared: &Arc<Shared>, report: RunReport) {
    with_state(|s| {
        s.timer = None;
        s.run = None;
    });
    if let Some(run) = shared.active_run.lock().unwrap().take() {
        let ids = run.upload_ids();
        shared.pending.lock().unwrap().retain(|p| !ids.contains(&p.upload_id));
        shared.finished_ids.lock().unwrap().extend(ids.into_iter().map(|id| (id, std::time::Instant::now())));
    }
    crate::set_anim_speed(1.0);
    ui.set_uploading(false);
    ui.set_upload_cancelled(false);
    ui.set_per_file(ModelRc::default());
    ui.set_title_text("naTiVeFS".into());
    let light = ui.global::<crate::Palette>().get_light();
    ui.set_prog_color(if light { slint::Color::from_rgb_u8(0, 168, 134) } else { slint::Color::from_rgb_u8(47, 255, 196) });
    match report.outcome {
        Outcome::Single(f) => match f.result {
            Ok(resp) => show_single(ui, shared, &resp),
            Err(e) => ui.set_upload_error(e.into()),
        },
        Outcome::Batch { files, batch } => match batch {
            Ok(b) => show_batch(ui, shared, &b, &files),
            Err(e) => ui.set_upload_error(e.into()),
        },
        Outcome::Protected(r) => match r {
            Ok(share) => show_protected(ui, shared, &share),
            Err(e) => ui.set_upload_error(e.into()),
        },
        Outcome::Failed(e) => ui.set_upload_error(e.into()),
    }
    if let Some(stats) = report.stats {
        show_stats(ui, &stats);
    }
    // the web empties the selection after every run
    let temps = with_state(|s| {
        s.queue.clear();
        std::mem::take(&mut s.temp_files)
    });
    for t in temps {
        let _ = std::fs::remove_file(&t);
        if let Some(dir) = t.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    ui.set_queue_scroll(0.0);
    push_queue(ui);
    push_recent(shared, &ui.as_weak());
    shared.wake.notify_one();
}

// ── cancel / pause ──────────────────────────────────────────────────────────

pub fn cancel_clicked(ui: &MainWindow, shift: bool) {
    let Some(run) = with_state(|s| s.run.clone()) else { return };
    if run.control.is_cancelled() {
        return;
    }
    if shift {
        kill(ui, &run);
        return;
    }
    // pause right away so nothing new starts while the question is open
    let was = run.control.is_paused();
    with_state(|s| s.paused_before_cancel = was);
    run.control.set_paused(true);
    ui.set_confirm_title("Cancel upload?".into());
    ui.set_confirm_text("This will stop the upload. Chunks already sent will be automatically deleted from the server shortly.".into());
    ui.set_confirm_note("Tip: hold Shift next time to cancel immediately, no prompt.".into());
    ui.set_confirm_id("".into());
    ui.set_confirm_kind(3);
}

pub fn cancel_answer(ui: &MainWindow, yes: bool) {
    let Some(run) = with_state(|s| s.run.clone()) else { return };
    if yes {
        kill(ui, &run);
    } else {
        let was = with_state(|s| s.paused_before_cancel);
        run.control.set_paused(was);
    }
}

fn kill(ui: &MainWindow, run: &Arc<Run>) {
    run.control.cancel();
    ui.set_upload_cancelled(true);
    ui.set_prog_status("Cancelled".into());
    ui.set_prog_dots(false);
}

// ── result box ──────────────────────────────────────────────────────────────

fn pill(icon: i32, label: &str) -> PillState {
    PillState { show: true, icon, label: label.into(), enabled: true }
}

fn hidden_pill() -> PillState {
    PillState { show: false, icon: 0, label: "".into(), enabled: false }
}

fn reset_result(ui: &MainWindow) {
    with_state(|s| {
        let g = s.result.generation + 1;
        s.result = ResultState { generation: g, ..Default::default() };
    });
    ui.set_result_visible(false);
    ui.set_reader_link("".into());
    ui.set_player_link("".into());
    ui.set_batch_rows(ModelRc::default());
    ui.set_batch_copied_row(-1);
}

fn show_single(ui: &MainWindow, shared: &Arc<Shared>, r: &UploadCompleteResponse) {
    shared.remember_tool_link(r);
    let reader = r.md_reader_link.clone().or(r.text_reader_link.clone()).unwrap_or_default();
    let reader_styled = r.md_reader_styled_link.clone().or(r.text_reader_styled_link.clone());
    let is_md = r.md_reader_link.is_some();
    let (raw_label, raw_style_label, read_label, read_style_label) = if is_md {
        ("Copy MD Link", "Copy MD Styled", "Copy MD Reader", "Copy MD Styled")
    } else {
        ("Copy TXT Link", "Copy TXT Styled", "Copy Reader Link", "Copy Reader Styled")
    };
    let has_reader = !reader.is_empty();
    let link_label = if has_reader { raw_label } else { "Copy Link" };
    let styled_label = if has_reader { raw_style_label } else { "Copy Styled" };
    ui.set_result_title("Upload Successful".into());
    ui.set_result_link(r.link.clone().into());
    ui.set_pill_link(pill(0, link_label));
    let pending = r.styled_link_pending && !r.file_id.is_empty();
    if pending {
        ui.set_pill_styled(PillState { show: true, icon: 5, label: "Processing…".into(), enabled: false });
    } else if r.styled_link.is_some() {
        ui.set_pill_styled(pill(1, styled_label));
    } else {
        ui.set_pill_styled(hidden_pill());
    }
    ui.set_reader_link(reader.clone().into());
    ui.set_pill_reader(pill(2, read_label));
    ui.set_pill_reader_styled(if reader_styled.is_some() { pill(1, read_style_label) } else { hidden_pill() });
    let player = r.audio_player_link.clone().unwrap_or_default();
    ui.set_player_link(player.clone().into());
    ui.set_pill_player(pill(3, "Copy Player Link"));
    ui.set_pill_player_styled(if r.audio_player_styled_link.is_some() { pill(1, "Copy Player Styled") } else { hidden_pill() });
    ui.set_batch_rows(ModelRc::default());
    ui.set_result_visible(true);
    let generation = with_state(|s| {
        s.result.link = r.link.clone();
        s.result.styled = r.styled_link.clone();
        s.result.reader = reader.clone();
        s.result.reader_styled = reader_styled.clone();
        s.result.player = player.clone();
        s.result.player_styled = r.audio_player_styled_link.clone();
        s.result.base_link_label = link_label.into();
        s.result.base_styled_label = styled_label.into();
        s.result.pending_auto_styled = false;
        s.result.generation
    });
    if pending {
        poll_styled(ui, shared, r.file_id.clone(), generation);
    }
    // auto copy (web `runAutoCopySingle`)
    let cfg = shared.config.lock().unwrap().clone();
    if cfg.auto_copy == AutoCopy::Off {
        return;
    }
    let tool_plain = r.tool_link().map(str::to_owned);
    let tool_styled = r.tool_styled_link().map(str::to_owned);
    if cfg.auto_copy_prefer_tool {
        if let Some(plain) = tool_plain {
            let text = if cfg.auto_copy == AutoCopy::Normal { plain.clone() } else { tool_styled.unwrap_or(plain) };
            copy_to_clipboard(&text);
            return;
        }
    }
    let styled_mode = if r.kind == "image" { cfg.auto_copy_image_styled } else { cfg.auto_copy == AutoCopy::Styled };
    if !styled_mode {
        copy_to_clipboard(&r.link);
        flash_pill(ui, 0, "Auto-copied");
    } else if pending {
        with_state(|s| s.result.pending_auto_styled = true);
    } else if let Some(st) = &r.styled_link {
        copy_to_clipboard(st);
        flash_pill(ui, 1, "Auto-copied");
    } else {
        copy_to_clipboard(&r.link);
        flash_pill(ui, 0, "Auto-copied");
    }
}

/// `pollStyledLink`: once a second, up to ~40 s, then give up quietly.
fn poll_styled(ui: &MainWindow, shared: &Arc<Shared>, file_id: String, generation: u64) {
    let (shared, weak) = (shared.clone(), ui.as_weak());
    shared.rt.clone().spawn(async move {
        for _ in 0..41 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if let Ok(st) = shared.client.styled_link(&file_id).await {
                if st.ready {
                    if let Some(link) = st.styled_link {
                        on_ui(&weak, move |ui| styled_ready(ui, Some(link), generation));
                        return;
                    }
                }
            }
        }
        on_ui(&weak, move |ui| styled_ready(ui, None, generation));
    });
}

fn styled_ready(ui: &MainWindow, link: Option<String>, generation: u64) {
    let (current, pending_copy, label, plain) = with_state(|s| {
        (s.result.generation == generation, s.result.pending_auto_styled, s.result.base_styled_label.clone(), s.result.link.clone())
    });
    if !current {
        return;
    }
    with_state(|s| {
        s.result.pending_auto_styled = false;
        s.result.styled = link.clone();
    });
    match link {
        Some(l) => {
            ui.set_pill_styled(pill(1, &label));
            if pending_copy {
                copy_to_clipboard(&l);
                flash_pill(ui, 1, "Auto-copied");
            }
        }
        None => {
            ui.set_pill_styled(hidden_pill());
            if pending_copy {
                copy_to_clipboard(&plain);
                flash_pill(ui, 0, "Auto-copied");
            }
        }
    }
}

fn show_batch(ui: &MainWindow, shared: &Arc<Shared>, b: &BatchCompleteResponse, files: &[tvfs_core::upload::FileResult]) {
    let link = if b.link.is_empty() { b.landing_page.clone().unwrap_or_default() } else { b.link.clone() };
    ui.set_result_title("Batch Upload Successful".into());
    ui.set_result_link(link.clone().into());
    ui.set_pill_link(pill(0, "Copy Batch Link"));
    ui.set_pill_styled(if b.styled_link.is_some() { pill(1, "Copy Styled") } else { hidden_pill() });
    ui.set_reader_link("".into());
    ui.set_player_link("".into());
    let rows: Vec<BatchRow> = files
        .iter()
        .filter_map(|f| f.result.as_ref().ok().map(|r| (f, r)))
        .map(|(f, r)| {
            shared.remember_tool_link(r);
            let styled = b.files.iter().find(|x| x.link == r.link).and_then(|x| x.styled_link.clone()).unwrap_or_default();
            BatchRow {
                name: f.name.clone().into(),
                link: r.link.clone().into(),
                styled: styled.into(),
                reader: r.text_reader_link.clone().or(r.md_reader_link.clone()).unwrap_or_default().into(),
                player: r.audio_player_link.clone().unwrap_or_default().into(),
            }
        })
        .collect();
    ui.set_batch_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_result_visible(true);
    with_state(|s| {
        s.result.link = link.clone();
        s.result.styled = b.styled_link.clone();
        s.result.base_link_label = "Copy Batch Link".into();
        s.result.base_styled_label = "Copy Styled".into();
    });
    let mode = shared.config.lock().unwrap().auto_copy;
    match (mode, &b.styled_link) {
        (AutoCopy::Off, _) => {}
        (AutoCopy::Styled, Some(st)) => copy_to_clipboard(st),
        _ => copy_to_clipboard(&link),
    }
}

fn show_protected(ui: &MainWindow, shared: &Arc<Shared>, share: &ProtectedShareResponse) {
    let files = with_state(|s| s.queue.clone());
    let prefer_tool = shared.config.lock().unwrap().auto_copy_prefer_tool;
    let solo_tool = (share.files.len() == 1).then(|| share.files[0].tool_link.clone()).flatten();
    let link = match solo_tool {
        Some(t) if prefer_tool => t,
        _ => share.link.clone(),
    };
    let total: u64 = files.iter().map(|f| f.size).sum();
    let name_part = if files.len() == 1 { files[0].upload_name() } else { format!("{} files", files.len()) };
    let styled = format!("[{name_part} | {} | TomasekValla Filestream System Users Exclusive Sharing]({link})", fmt_bytes(total));
    ui.set_result_title("Shared (TVFS Users Only)".into());
    ui.set_result_link(link.clone().into());
    ui.set_pill_link(pill(0, "Copy Link"));
    ui.set_pill_styled(pill(1, "Copy Styled"));
    ui.set_reader_link("".into());
    ui.set_player_link("".into());
    ui.set_batch_rows(ModelRc::default());
    ui.set_result_visible(true);
    with_state(|s| {
        s.result.link = link.clone();
        s.result.styled = Some(styled.clone());
        s.result.base_link_label = "Copy Link".into();
        s.result.base_styled_label = "Copy Styled".into();
    });
    let mode = shared.config.lock().unwrap().auto_copy;
    match mode {
        AutoCopy::Styled => {
            copy_to_clipboard(&styled);
            flash_pill(ui, 1, "Auto-copied");
        }
        AutoCopy::Normal => {
            copy_to_clipboard(&link);
            flash_pill(ui, 0, "Auto-copied");
        }
        AutoCopy::Off => {}
    }
    let exp_min = tvfs_core::EXPIRATION_OPTIONS_MIN.get(ui.get_expiry().max(0) as usize).copied().unwrap_or(tvfs_core::DEFAULT_EXPIRATION_MIN);
    let now = crate::now_ms();
    shared.add_local_share(LocalShare {
        id: share.share_id.clone(),
        name: if files.len() == 1 { files[0].upload_name() } else { format!("{} files (protected share)", files.len()) },
        link,
        size: total,
        uploaded_at: now,
        expires_at: share.expires_at.unwrap_or(now + exp_min as i64 * 60_000),
    });
}

/// "✅ Copied" / "✅ Auto-copied" on a pill for 1.5 s. `which`: 0 link, 1 styled, 2 reader, 3 reader styled, 4 player, 5 player styled.
fn flash_pill(ui: &MainWindow, which: i32, label: &str) {
    let get = |ui: &MainWindow| match which {
        0 => ui.get_pill_link(),
        1 => ui.get_pill_styled(),
        2 => ui.get_pill_reader(),
        3 => ui.get_pill_reader_styled(),
        4 => ui.get_pill_player(),
        _ => ui.get_pill_player_styled(),
    };
    let set = move |ui: &MainWindow, p: PillState| match which {
        0 => ui.set_pill_link(p),
        1 => ui.set_pill_styled(p),
        2 => ui.set_pill_reader(p),
        3 => ui.set_pill_reader_styled(p),
        4 => ui.set_pill_player(p),
        _ => ui.set_pill_player_styled(p),
    };
    let orig = get(ui);
    if orig.icon == 4 {
        return;
    }
    set(ui, PillState { icon: 4, label: label.into(), ..orig.clone() });
    let weak = ui.as_weak();
    let generation = with_state(|s| s.result.generation);
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        if let Some(ui) = weak.upgrade() {
            if with_state(|s| s.result.generation) == generation {
                set(&ui, orig);
            }
        }
    });
}

pub fn copy_result(ui: &MainWindow, which: i32) {
    let text = with_state(|s| match which {
        0 => Some(s.result.link.clone()),
        1 => s.result.styled.clone(),
        2 => Some(s.result.reader.clone()),
        3 => s.result.reader_styled.clone(),
        4 => Some(s.result.player.clone()),
        _ => s.result.player_styled.clone(),
    });
    if let Some(t) = text.filter(|t| !t.is_empty()) {
        copy_to_clipboard(&t);
        flash_pill(ui, which, "Copied");
    }
}

pub fn copy_batch(ui: &MainWindow, row: i32, text: &str, kind: i32) {
    copy_to_clipboard(text);
    ui.set_batch_copied_row(row);
    ui.set_batch_copied_kind(kind);
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_batch_copied_row() == row && ui.get_batch_copied_kind() == kind {
                ui.set_batch_copied_row(-1);
            }
        }
    });
}

// ── stats card ──────────────────────────────────────────────────────────────

fn show_stats(ui: &MainWindow, st: &StatsReport) {
    ui.set_stats_elapsed(st.elapsed.clone().into());
    ui.set_stats_speed(st.speed_mbs.clone().into());
    ui.set_stats_speed_mbps(st.speed_mbps.clone().into());
    match &st.comparison {
        Some((kind, badge, headline, sub)) => {
            ui.set_stats_cmp_kind(match kind {
                ComparisonKind::Faster => 1,
                ComparisonKind::Slower => 2,
                ComparisonKind::Same => 3,
            });
            ui.set_stats_cmp_badge(badge.clone().into());
            ui.set_stats_cmp_headline(headline.clone().into());
            ui.set_stats_cmp_sub(sub.clone().into());
        }
        None => ui.set_stats_cmp_kind(0),
    }
    ui.set_stats_fun_prefix(st.fun.0.clone().into());
    ui.set_stats_fun_bold(st.fun.1.clone().into());
    let speed: Vec<MetricTile> = st
        .speed
        .map(|b| {
            [("MIN", b.min), ("MED", b.med), ("AVG", b.avg), ("MAX", b.max)]
                .into_iter()
                .map(|(k, v)| {
                    let (value, unit) = fmt_speed_mbs_parts(v);
                    MetricTile { key: k.into(), value: value.into(), unit: unit.into() }
                })
                .collect()
        })
        .unwrap_or_default();
    let ping: Vec<MetricTile> = match st.ping {
        Some(b) if st.ping_single => vec![MetricTile { key: "AVG".into(), value: format!("{} ms", b.avg.round()).into(), unit: "".into() }],
        Some(b) => [("MIN", b.min), ("MED", b.med), ("AVG", b.avg), ("MAX", b.max)]
            .into_iter()
            .map(|(k, v)| MetricTile { key: k.into(), value: format!("{} ms", v.round()).into(), unit: "".into() })
            .collect(),
        None => Vec::new(),
    };
    ui.set_stats_speed_tiles(ModelRc::new(VecModel::from(speed)));
    ui.set_stats_ping_tiles(ModelRc::new(VecModel::from(ping)));
    // timer bar: full, then a 20 s linear run down to empty, then the card goes away
    ui.set_stats_timer_anim(0);
    ui.set_stats_timer(1.0);
    ui.set_stats_visible(true);
    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(50), move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_stats_timer_anim(20_000);
            ui.set_stats_timer(0.0);
        }
    });
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(slint::TimerMode::SingleShot, Duration::from_secs(20), move || {
        if let Some(ui) = weak.upgrade() {
            hide_stats(&ui);
        }
    });
    with_state(|s| s.stats_timer = Some(timer));
}

pub fn hide_stats(ui: &MainWindow) {
    with_state(|s| s.stats_timer = None);
    ui.set_stats_visible(false);
}

// ── TVFS settings card ──────────────────────────────────────────────────────

fn parse_num(s: &str) -> Option<u32> {
    s.trim().parse::<u32>().ok()
}

/// `evaluateChunkConfig` while typing (the message is typed out at 14 ms per character).
pub fn chunk_inputs_edited(ui: &MainWindow) {
    let count = parse_num(&ui.get_chunk_count_text());
    let size = parse_num(&ui.get_chunk_size_text());
    let warning = evaluate_fixed(count, size);
    ui.set_chunk_size_invalid(warning == Some(FixedWarning::Prevented));
    if warning == Some(FixedWarning::Prevented) {
        ui.set_chunk_size_text("".into());
    }
    set_chunk_message(ui, warning);
}

fn set_chunk_message(ui: &MainWindow, warning: Option<FixedWarning>) {
    let kind = match warning {
        None => 0,
        Some(FixedWarning::Warn) => 1,
        Some(FixedWarning::Caution) => 2,
        Some(FixedWarning::Prevented) => 3,
    };
    if kind == ui.get_chunk_msg_kind() && (kind == 0 || ui.get_chunk_msg_typing() || !ui.get_chunk_msg_text().is_empty()) {
        return;
    }
    ui.set_chunk_msg_kind(kind);
    let Some(w) = warning else {
        with_state(|s| s.chunk_msg_timer = None);
        ui.set_chunk_msg_text("".into());
        ui.set_chunk_msg_typing(false);
        return;
    };
    let text: Vec<char> = w.text().chars().collect();
    ui.set_chunk_msg_text("".into());
    ui.set_chunk_msg_typing(true);
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let mut i = 0usize;
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(14), move || {
        let Some(ui) = weak.upgrade() else { return };
        i += 1;
        ui.set_chunk_msg_text(text[..i.min(text.len())].iter().collect::<String>().into());
        if i >= text.len() {
            ui.set_chunk_msg_typing(false);
            with_state(|s| s.chunk_msg_timer = None);
        }
    });
    with_state(|s| s.chunk_msg_timer = Some(timer));
}

/// `commitChunkConfig` on Enter / leaving the field: clamp and save.
pub fn chunk_inputs_commit(ui: &MainWindow, shared: &Shared) {
    let mut cfg = shared.config.lock().unwrap();
    let (count, size) = commit_fixed(parse_num(&ui.get_chunk_count_text()), parse_num(&ui.get_chunk_size_text()), cfg.chunk_count, cfg.chunk_size_mb);
    cfg.chunk_count = count;
    cfg.chunk_size_mb = size;
    if let Err(e) = cfg.save() {
        eprintln!("naTiVeFS: could not save chunk settings: {e}");
    }
    drop(cfg);
    ui.set_chunk_count_text(count.to_string().into());
    ui.set_chunk_size_text(size.to_string().into());
    ui.set_chunk_size_invalid(false);
    set_chunk_message(ui, evaluate_fixed(Some(count), Some(size)));
}

pub fn chunk_mode_changed(ui: &MainWindow, shared: &Shared, auto: bool) {
    {
        let mut cfg = shared.config.lock().unwrap();
        cfg.chunk_auto = auto;
        let _ = cfg.save();
    }
    if !auto {
        let (c, s) = (parse_num(&ui.get_chunk_count_text()), parse_num(&ui.get_chunk_size_text()));
        set_chunk_message(ui, evaluate_fixed(c, s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_detection_like_the_web() {
        for md in ["# Title", "x\n## sub", "**bold**", "_it_", "```rust", "| a | b |", "---", "* item", "1. first"] {
            assert!(looks_like_markdown(md), "{md}");
        }
        for txt in ["plain words", "#hashtag", "** spaced", "1.5 litres", "-- dash", "|x"] {
            assert!(!looks_like_markdown(txt), "{txt}");
        }
    }

    #[test]
    fn ext_badges() {
        assert_eq!(ext_label(".mp4"), "MP4");
        assert_eq!(ext_label(".markdown"), "MARKD…");
        assert_eq!(ext_label(""), "?");
    }

    #[test]
    fn speed_colours() {
        assert_eq!(speed_color(0.05, false), slint::Color::from_rgb_u8(255, 71, 87));
        assert_eq!(speed_color(100.0, false), slint::Color::from_rgb_u8(47, 255, 196));
    }
}
