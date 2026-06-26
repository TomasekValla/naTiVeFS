use crate::{
    auth::{self, AuthState},
    clipboard,
    config::{self, Config},
    history::{self, HistoryEntry},
    shortcuts::{self, ShortcutAction},
    upload::{self, UploadEvent, UploadTask},
};
use eframe::egui::{self, Color32, FontId, RichText, Stroke};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

// ── Theme ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Theme {
    Light,
    Dark,
    Amoled,
}

impl Theme {
    fn bg(&self) -> Color32 {
        match self {
            Theme::Light => Color32::WHITE,
            Theme::Dark => Color32::from_rgb(17, 17, 17),
            Theme::Amoled => Color32::BLACK,
        }
    }
    fn fg(&self) -> Color32 {
        match self {
            Theme::Light => Color32::BLACK,
            Theme::Dark => Color32::from_rgb(238, 238, 238),
            Theme::Amoled => Color32::WHITE,
        }
    }
    fn accent(&self) -> Color32 {
        match self {
            Theme::Light => Color32::BLACK,
            Theme::Dark | Theme::Amoled => Color32::WHITE,
        }
    }
    fn panel_bg(&self) -> Color32 {
        match self {
            Theme::Light => Color32::from_rgb(245, 245, 245),
            Theme::Dark => Color32::from_rgb(24, 24, 24),
            Theme::Amoled => Color32::from_rgb(10, 10, 10),
        }
    }
    fn border(&self) -> Color32 {
        match self {
            Theme::Light => Color32::from_rgb(200, 200, 200),
            Theme::Dark => Color32::from_rgb(50, 50, 50),
            Theme::Amoled => Color32::from_rgb(40, 40, 40),
        }
    }
    fn label(&self) -> &'static str {
        match self {
            Theme::Light => "Light",
            Theme::Dark => "Dark",
            Theme::Amoled => "AMOLED",
        }
    }
    fn cycle(&self) -> Theme {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Amoled,
            Theme::Amoled => Theme::Light,
        }
    }
    fn from_str(s: &str) -> Theme {
        match s {
            "light" => Theme::Light,
            "amoled" => Theme::Amoled,
            _ => Theme::Dark,
        }
    }
    fn to_str(&self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::Amoled => "amoled",
        }
    }
}

// ── Per-file upload state ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Queued,
    Uploading,
    Done { link: String, styled_link: String },
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub id: usize,
    pub path: PathBuf,
    pub name: String,
    pub anonymize: bool,
    pub status: FileStatus,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct NativeFS {
    // Runtime
    pub rt: Arc<tokio::runtime::Runtime>,

    // Config
    pub config: Config,

    // Auth
    pub auth: AuthState,
    pub login_password: String,
    pub login_error: String,
    pub login_loading: bool,
    pub auth_tx: mpsc::UnboundedSender<Result<(String, u8), String>>,
    pub auth_rx: mpsc::UnboundedReceiver<Result<(String, u8), String>>,

    // Files
    pub files: Vec<FileEntry>,
    pub next_id: usize,

    // Upload channel
    pub upload_tx: mpsc::UnboundedSender<UploadEvent>,
    pub upload_rx: mpsc::UnboundedReceiver<UploadEvent>,
    pub uploading: bool,

    // History
    pub history: Vec<HistoryEntry>,

    // UI state
    pub theme: Theme,
    pub show_settings: bool,
    pub show_close_dialog: bool,
    pub remember_close: bool,
    pub status_bar_msg: String,

    // Clipboard feedback
    pub clipboard_flash: f32, // countdown timer for "Copied!" flash

    // Drag-over highlight
    pub drag_over: bool,
}

impl NativeFS {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let cfg = config::load_config();
        let theme = Theme::from_str(&cfg.theme);
        let history = history::load_history();

        let (upload_tx, upload_rx) = mpsc::unbounded_channel();
        let (auth_tx, auth_rx) = mpsc::unbounded_channel();

        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap(),
        );

        let auth = if cfg.token.is_empty() {
            AuthState::default()
        } else {
            AuthState {
                logged_in: true,
                tier: 1,
                token: cfg.token.clone(),
            }
        };

        let app = Self {
            rt,
            config: cfg,
            auth,
            login_password: String::new(),
            login_error: String::new(),
            login_loading: false,
            auth_tx,
            auth_rx,
            files: Vec::new(),
            next_id: 0,
            upload_tx,
            upload_rx,
            uploading: false,
            history,
            theme,
            show_settings: false,
            show_close_dialog: false,
            remember_close: false,
            status_bar_msg: String::new(),
            clipboard_flash: 0.0,
            drag_over: false,
        };

        app.apply_theme(&cc.egui_ctx);
        app
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let mut visuals = egui::Visuals::dark();

        let bg = self.theme.bg();
        let fg = self.theme.fg();
        let panel = self.theme.panel_bg();
        let border = self.theme.border();
        let accent = self.theme.accent();

        visuals.panel_fill = panel;
        visuals.window_fill = bg;
        visuals.override_text_color = Some(fg);
        visuals.extreme_bg_color = bg;
        visuals.faint_bg_color = panel;

        // Widgets
        let w = &mut visuals.widgets;
        for ws in [
            &mut w.noninteractive,
            &mut w.inactive,
            &mut w.hovered,
            &mut w.active,
            &mut w.open,
        ] {
            ws.bg_fill = panel;
            ws.bg_stroke = Stroke::new(1.0, border);
            ws.fg_stroke = Stroke::new(1.0, fg);
            ws.rounding = egui::Rounding::ZERO;
        }
        w.hovered.bg_fill = accent.linear_multiply(0.1);
        w.hovered.fg_stroke = Stroke::new(1.0, accent);
        w.active.bg_fill = accent.linear_multiply(0.2);

        visuals.selection.bg_fill = accent.linear_multiply(0.2);
        visuals.selection.stroke = Stroke::new(1.0, accent);

        visuals.window_stroke = Stroke::new(1.0, border);
        visuals.window_rounding = egui::Rounding::ZERO;

        ctx.set_visuals(visuals);
    }

    fn cycle_theme(&mut self, ctx: &egui::Context) {
        self.theme = self.theme.cycle();
        self.config.theme = self.theme.to_str().to_string();
        let _ = config::save_config(&self.config);
        self.apply_theme(ctx);
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();
                let size = std::fs::metadata(&path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                self.files.push(FileEntry {
                    id: self.next_id,
                    path,
                    name,
                    anonymize: false,
                    status: FileStatus::Queued,
                    bytes_done: 0,
                    bytes_total: size,
                });
                self.next_id += 1;
            }
        }
    }

    pub fn start_upload(&mut self) {
        if self.files.is_empty() || !self.auth.logged_in {
            return;
        }

        self.uploading = true;

        let queued: Vec<FileEntry> = self
            .files
            .iter()
            .filter(|f| f.status == FileStatus::Queued)
            .cloned()
            .collect();

        if queued.is_empty() {
            self.uploading = false;
            return;
        }

        let server_url = self.config.server_url.clone();
        let token = self.auth.token.clone();
        let chunk_size_mb = self.config.chunk_size_mb;
        let parallel = self.config.parallel_chunks;
        let exp_days = self.config.expiration_days;
        let tx = self.upload_tx.clone();

        // Mark all as uploading
        for f in self.files.iter_mut() {
            if f.status == FileStatus::Queued {
                f.status = FileStatus::Uploading;
            }
        }

        let rt = self.rt.clone();
        let files_clone = queued;

        std::thread::spawn(move || {
            rt.block_on(async {
                // Use batch if more than one file
                let batch_id = if files_clone.len() > 1 {
                    let client = reqwest::Client::new();
                    upload::batch_init(&client, &server_url, &token, exp_days)
                        .await
                        .ok()
                } else {
                    None
                };

                let mut handles = Vec::new();
                for f in files_clone {
                    let task = UploadTask {
                        file_id: f.id,
                        path: f.path.clone(),
                        anonymize: f.anonymize,
                        expiration_days: exp_days,
                        batch_id: batch_id.clone(),
                    };
                    let su = server_url.clone();
                    let tok = token.clone();
                    let tx2 = tx.clone();
                    handles.push(tokio::spawn(async move {
                        upload::upload_file(task, su, tok, chunk_size_mb, parallel, tx2).await;
                    }));
                }

                for h in handles {
                    let _ = h.await;
                }

                // Finalize batch
                if let Some(bid) = batch_id {
                    let client = reqwest::Client::new();
                    let _ = upload::batch_complete(&client, &server_url, &token, &bid).await;
                }
            });
        });
    }

    pub fn open_file_picker(&mut self) {
        if let Some(paths) = rfd::FileDialog::new().pick_files() {
            self.add_files(paths);
        }
    }

    pub fn copy_last_direct(&mut self) {
        if let Some(entry) = history::last_entry(&self.history) {
            let link = entry.link.clone();
            if clipboard::copy_to_clipboard(&link).is_ok() {
                self.status_bar_msg = "Copied direct link".to_string();
                self.clipboard_flash = 2.0;
            }
        }
    }

    pub fn copy_last_styled(&mut self) {
        if let Some(entry) = history::last_entry(&self.history) {
            let sl = entry.styled_link.clone();
            if clipboard::copy_to_clipboard(&sl).is_ok() {
                self.status_bar_msg = "Copied styled link".to_string();
                self.clipboard_flash = 2.0;
            }
        }
    }

    /// Handle a close request (e.g. from keyboard shortcut or close button).
    /// Uses viewport commands instead of the removed frame.close() / frame.set_visible().
    fn handle_close(&mut self, ctx: &egui::Context) {
        match self.config.close_behavior.as_str() {
            "tray" => ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false)),
            "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => self.show_close_dialog = true,
        }
    }

    fn send_notification(&self, summary: &str, body: &str) {
        let summary = summary.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            let _ = notify_rust::Notification::new()
                .summary(&summary)
                .body(&body)
                .appname("naTiVeFS")
                .show();
        });
    }
}

impl eframe::App for NativeFS {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // ── Handle window close request (replaces removed on_close_event) ──────
        if ctx.input(|i| i.viewport().close_requested()) {
            match self.config.close_behavior.as_str() {
                "quit" => {} // allow the close to proceed normally
                "tray" => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                _ => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.show_close_dialog = true;
                }
            }
        }

        // ── Poll upload events ────────────────────────────────────────────────
        while let Ok(event) = self.upload_rx.try_recv() {
            match event {
                UploadEvent::Progress {
                    file_id,
                    bytes_done,
                    bytes_total,
                } => {
                    if let Some(f) = self.files.iter_mut().find(|f| f.id == file_id) {
                        f.bytes_done = bytes_done;
                        f.bytes_total = bytes_total;
                    }
                }
                UploadEvent::Complete {
                    file_id,
                    link,
                    styled_link,
                    filename,
                } => {
                    if let Some(f) = self.files.iter_mut().find(|f| f.id == file_id) {
                        f.status = FileStatus::Done {
                            link: link.clone(),
                            styled_link: styled_link.clone(),
                        };
                    }
                    history::push_entry(
                        &mut self.history,
                        HistoryEntry {
                            filename: filename.clone(),
                            link: link.clone(),
                            styled_link: styled_link.clone(),
                            uploaded_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0),
                        },
                    );
                    let _ = clipboard::copy_to_clipboard(&link);
                    self.status_bar_msg = format!("✓ {} — link copied", filename);
                    self.clipboard_flash = 3.0;
                    self.send_notification("naTiVeFS", &format!("✓ {} — link copied", filename));

                    let all_done = self
                        .files
                        .iter()
                        .all(|f| !matches!(f.status, FileStatus::Uploading | FileStatus::Queued));
                    if all_done {
                        self.uploading = false;
                    }
                }
                UploadEvent::Failed { file_id, error } => {
                    if let Some(f) = self.files.iter_mut().find(|f| f.id == file_id) {
                        f.status = FileStatus::Failed(error.clone());
                    }
                    self.send_notification("naTiVeFS", &format!("✗ Upload failed: {}", error));
                    let all_done = self
                        .files
                        .iter()
                        .all(|f| !matches!(f.status, FileStatus::Uploading | FileStatus::Queued));
                    if all_done {
                        self.uploading = false;
                    }
                }
            }
        }

        // ── Poll auth events ──────────────────────────────────────────────────
        while let Ok(result) = self.auth_rx.try_recv() {
            self.login_loading = false;
            match result {
                Ok((token, tier)) => {
                    self.auth.logged_in = true;
                    self.auth.tier = tier;
                    self.auth.token = token.clone();
                    self.config.token = token;
                    let _ = config::save_config(&self.config);
                    self.login_error.clear();
                    self.login_password.clear();
                }
                Err(e) => {
                    self.login_error = e;
                }
            }
        }

        // ── Keyboard shortcuts ────────────────────────────────────────────────
        if let Some(action) = shortcuts::check_shortcuts(ctx) {
            match action {
                ShortcutAction::OpenFilePicker => self.open_file_picker(),
                ShortcutAction::CopyDirectLink => self.copy_last_direct(),
                ShortcutAction::CopyStyledLink => self.copy_last_styled(),
                ShortcutAction::ClearFiles => {
                    if !self.uploading {
                        self.files.clear();
                    }
                }
                ShortcutAction::CycleTheme => self.cycle_theme(ctx),
                ShortcutAction::OpenSettings => self.show_settings = !self.show_settings,
                ShortcutAction::HideWindow => self.handle_close(ctx),
            }
        }

        // ── Countdown for clipboard flash ─────────────────────────────────────
        if self.clipboard_flash > 0.0 {
            self.clipboard_flash -= ctx.input(|i| i.unstable_dt);
            ctx.request_repaint();
        }

        // ── Drag-and-drop ─────────────────────────────────────────────────────
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_files(dropped);
        }
        self.drag_over = ctx.input(|i| !i.raw.hovered_files.is_empty());

        // ── Login modal (blocks rest of UI) ──────────────────────────────────
        if !self.auth.logged_in {
            self.render_login(ctx);
            return;
        }

        // ── Settings panel ────────────────────────────────────────────────────
        if self.show_settings {
            self.render_settings(ctx);
        }

        // ── Close dialog ──────────────────────────────────────────────────────
        if self.show_close_dialog {
            self.render_close_dialog(ctx);
        }

        // ── Main window ───────────────────────────────────────────────────────
        // frame is unused after migration to viewport commands but kept in
        // signature because eframe::App requires it.
        let _ = frame;
        self.render_main(ctx);
    }
}

// ── UI rendering ──────────────────────────────────────────────────────────────

impl NativeFS {
    fn render_login(&mut self, ctx: &egui::Context) {
        let bg = self.theme.bg();
        let fg = self.theme.fg();
        let _border = self.theme.border();
        let _accent = self.theme.accent();

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(bg))
            .show(ctx, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(80.0);

                        ui.label(
                            RichText::new("naTiVeFS")
                                .font(FontId::proportional(28.0))
                                .color(fg),
                        );
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("Enter your TVFS password")
                                .font(FontId::proportional(13.0))
                                .color(fg.linear_multiply(0.6)),
                        );

                        ui.add_space(24.0);

                        let pw_response = ui.add(
                            egui::TextEdit::singleline(&mut self.login_password)
                                .password(true)
                                .hint_text("Password")
                                .desired_width(260.0)
                                .font(FontId::proportional(14.0)),
                        );

                        let enter_pressed = pw_response.lost_focus()
                            && ctx.input(|i| i.key_pressed(egui::Key::Enter));

                        ui.add_space(10.0);

                        let btn = ui.add_sized(
                            [260.0, 34.0],
                            egui::Button::new(
                                RichText::new(if self.login_loading {
                                    "Logging in…"
                                } else {
                                    "Log in"
                                })
                                .font(FontId::proportional(13.0))
                                .color(bg),
                            )
                            .fill(fg),
                        );

                        if (btn.clicked() || enter_pressed) && !self.login_loading {
                            let pw = self.login_password.clone();
                            let server = self.config.server_url.clone();
                            let tx = self.auth_tx.clone();
                            self.login_loading = true;
                            self.login_error.clear();

                            let rt = self.rt.clone();
                            std::thread::spawn(move || {
                                rt.block_on(async {
                                    let client = reqwest::Client::new();
                                    let res = auth::login(&client, &server, &pw).await;
                                    let _ = tx.send(res);
                                });
                            });
                        }

                        if !self.login_error.is_empty() {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(&self.login_error)
                                    .font(FontId::proportional(12.0))
                                    .color(Color32::from_rgb(220, 80, 80)),
                            );
                        }

                        ui.add_space(16.0);
                        ui.label(
                            RichText::new(&self.config.server_url)
                                .font(FontId::monospace(11.0))
                                .color(fg.linear_multiply(0.4)),
                        );
                    });
                });
            });
    }

    fn render_main(&mut self, ctx: &egui::Context) {
        let bg = self.theme.bg();
        let fg = self.theme.fg();
        let border = self.theme.border();
        let accent = self.theme.accent();
        let panel_bg = self.theme.panel_bg();

        // ── Bottom bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(
                egui::Frame::none()
                    .fill(panel_bg)
                    .inner_margin(egui::Margin::symmetric(12.0, 6.0))
                    .stroke(Stroke::new(1.0, border)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Settings button
                    let settings_btn = ui.add(
                        egui::Button::new(
                            RichText::new("⚙ Settings")
                                .font(FontId::proportional(12.0))
                                .color(fg),
                        )
                        .frame(false),
                    );
                    if settings_btn.clicked() {
                        self.show_settings = !self.show_settings;
                    }

                    ui.separator();

                    // Theme toggle
                    let theme_btn = ui.add(
                        egui::Button::new(
                            RichText::new(format!("◐ {}", self.theme.label()))
                                .font(FontId::proportional(12.0))
                                .color(fg),
                        )
                        .frame(false),
                    );
                    if theme_btn.clicked() {
                        self.cycle_theme(ctx);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(concat!("v", env!("CARGO_PKG_VERSION")))
                                .font(FontId::proportional(11.0))
                                .color(fg.linear_multiply(0.4)),
                        );

                        if self.clipboard_flash > 0.0 {
                            ui.separator();
                            ui.label(
                                RichText::new(&self.status_bar_msg)
                                    .font(FontId::proportional(11.0))
                                    .color(fg.linear_multiply(0.7)),
                            );
                        }
                    });
                });
            });

        // ── Central panel ─────────────────────────────────────────────────────
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(bg).inner_margin(egui::Margin::same(12.0)))
            .show(ctx, |ui| {
                // Title bar row
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("naTiVeFS")
                            .font(FontId::proportional(18.0))
                            .color(fg),
                    );
                    if self.auth.logged_in {
                        ui.label(
                            RichText::new(format!("tier {}", self.auth.tier))
                                .font(FontId::proportional(10.0))
                                .color(fg.linear_multiply(0.35)),
                        );
                    }
                });

                ui.add_space(8.0);

                // ── Drop zone ────────────────────────────────────────────────
                let drop_height = if self.files.is_empty() { 120.0 } else { 60.0 };
                let drop_rect = ui.available_rect_before_wrap();
                let drop_rect = egui::Rect::from_min_size(
                    drop_rect.min,
                    egui::vec2(drop_rect.width(), drop_height),
                );

                let drop_resp = ui.allocate_rect(drop_rect, egui::Sense::click());

                // Draw drop zone border
                let dz_color = if self.drag_over { accent } else { border };
                ui.painter().rect_stroke(
                    drop_rect,
                    egui::Rounding::ZERO,
                    Stroke::new(1.0, dz_color),
                );

                // Drop zone text
                ui.painter().text(
                    drop_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    if self.drag_over {
                        "Drop to add"
                    } else if self.files.is_empty() {
                        "Drop files here\nor click to browse"
                    } else {
                        "+ Drop more files"
                    },
                    FontId::proportional(13.0),
                    fg.linear_multiply(if self.drag_over { 1.0 } else { 0.45 }),
                );

                if drop_resp.clicked() {
                    self.open_file_picker();
                }

                ui.add_space(drop_height + 4.0);

                // ── File list ─────────────────────────────────────────────────
                let available = ui.available_rect_before_wrap();
                let list_max_height = available.height() - 120.0;
                egui::ScrollArea::vertical()
                    .max_height(list_max_height)
                    .show(ui, |ui| {
                        let mut remove_ids: Vec<usize> = Vec::new();

                        for f in &mut self.files {
                            ui.horizontal(|ui| {
                                // Filename (truncated)
                                let max_name_width = ui.available_width() - 180.0;
                                ui.add_sized(
                                    [max_name_width, 20.0],
                                    egui::Label::new(
                                        RichText::new(&f.name)
                                            .font(FontId::proportional(12.0))
                                            .color(fg),
                                    )
                                    .truncate(), // FIX: was .truncate(true)
                                );

                                // Anonymize toggle
                                let anon_color = if f.anonymize { accent } else { border };
                                let anon_btn = ui.add(
                                    egui::Button::new(
                                        RichText::new("🕶")
                                            .font(FontId::proportional(13.0))
                                            .color(anon_color),
                                    )
                                    .frame(false),
                                );
                                if anon_btn.clicked()
                                    && !matches!(f.status, FileStatus::Uploading)
                                {
                                    f.anonymize = !f.anonymize;
                                }
                                anon_btn.on_hover_text("Anonymize filename");

                                // Progress bar or status
                                match &f.status {
                                    FileStatus::Queued => {
                                        ui.label(
                                            RichText::new("queued")
                                                .font(FontId::proportional(11.0))
                                                .color(fg.linear_multiply(0.4)),
                                        );
                                    }
                                    FileStatus::Uploading => {
                                        let progress = if f.bytes_total > 0 {
                                            f.bytes_done as f32 / f.bytes_total as f32
                                        } else {
                                            0.0
                                        };
                                        ui.add(
                                            egui::ProgressBar::new(progress)
                                                .desired_width(80.0)
                                                .text(format!("{:.0}%", progress * 100.0)),
                                        );
                                    }
                                    FileStatus::Done { link, .. } => {
                                        ui.label(
                                            RichText::new("✓")
                                                .font(FontId::proportional(13.0))
                                                .color(fg),
                                        );
                                        let copy_btn = ui.add(
                                            egui::Button::new(
                                                RichText::new("Copy")
                                                    .font(FontId::proportional(11.0))
                                                    .color(fg),
                                            )
                                            .frame(false),
                                        );
                                        let link_clone = link.clone();
                                        if copy_btn.clicked() {
                                            let _ = clipboard::copy_to_clipboard(&link_clone);
                                            self.status_bar_msg =
                                                "Copied direct link".to_string();
                                            self.clipboard_flash = 2.0;
                                        }
                                    }
                                    FileStatus::Failed(err) => {
                                        ui.label(
                                            RichText::new("✗")
                                                .font(FontId::proportional(13.0))
                                                .color(Color32::from_rgb(200, 60, 60)),
                                        )
                                        .on_hover_text(err);
                                        if ui
                                            .small_button(
                                                RichText::new("remove").color(fg.linear_multiply(0.5)),
                                            )
                                            .clicked()
                                        {
                                            remove_ids.push(f.id);
                                        }
                                    }
                                }
                            });
                            ui.add_space(2.0);
                        }

                        self.files.retain(|f| !remove_ids.contains(&f.id));
                    });

                ui.add_space(8.0);

                // ── Upload / Clear buttons ────────────────────────────────────
                ui.horizontal(|ui| {
                    let has_queued =
                        self.files.iter().any(|f| f.status == FileStatus::Queued);

                    let upload_btn = ui.add_enabled(
                        has_queued && !self.uploading,
                        egui::Button::new(
                            RichText::new(if self.uploading { "Uploading…" } else { "Upload" })
                                .font(FontId::proportional(13.0))
                                .color(bg),
                        )
                        .fill(fg)
                        .min_size(egui::vec2(90.0, 28.0)),
                    );
                    if upload_btn.clicked() {
                        self.start_upload();
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clear_btn = ui.add_enabled(
                            !self.uploading && !self.files.is_empty(),
                            egui::Button::new(
                                RichText::new("Clear")
                                    .font(FontId::proportional(12.0))
                                    .color(fg),
                            )
                            .min_size(egui::vec2(60.0, 28.0)),
                        );
                        if clear_btn.clicked() {
                            self.files.clear();
                        }
                    });
                });

                // ── Last upload result ────────────────────────────────────────
                if let Some(last) = history::last_entry(&self.history) {
                    ui.add_space(12.0);
                    ui.add(egui::Separator::default());
                    ui.add_space(4.0);

                    ui.label(
                        RichText::new("Last upload")
                            .font(FontId::proportional(11.0))
                            .color(fg.linear_multiply(0.4)),
                    );
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Direct link:")
                                .font(FontId::proportional(11.0))
                                .color(fg.linear_multiply(0.5)),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new(&last.link)
                                    .font(FontId::monospace(10.0))
                                    .color(fg),
                            )
                            .truncate(), // FIX: was .truncate(true)
                        );
                    });

                    ui.add_space(6.0);

                    ui.horizontal(|ui| {
                        let last_link = last.link.clone();
                        let last_styled = last.styled_link.clone();

                        let direct_btn = ui.add(
                            egui::Button::new(
                                RichText::new("Copy direct")
                                    .font(FontId::proportional(12.0))
                                    .color(bg),
                            )
                            .fill(fg)
                            .min_size(egui::vec2(100.0, 26.0)),
                        );
                        if direct_btn.clicked() {
                            let _ = clipboard::copy_to_clipboard(&last_link);
                            self.status_bar_msg = "Copied direct link".to_string();
                            self.clipboard_flash = 2.0;
                        }

                        let styled_btn = ui.add(
                            egui::Button::new(
                                RichText::new("Copy styled")
                                    .font(FontId::proportional(12.0))
                                    .color(fg),
                            )
                            .min_size(egui::vec2(100.0, 26.0)),
                        );
                        if styled_btn.clicked() {
                            let _ = clipboard::copy_to_clipboard(&last_styled);
                            self.status_bar_msg = "Copied styled link".to_string();
                            self.clipboard_flash = 2.0;
                        }
                    });
                }
            });
    }

    fn render_settings(&mut self, ctx: &egui::Context) {
        let bg = self.theme.bg();
        let fg = self.theme.fg();
        let border = self.theme.border();

        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .min_width(320.0)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(bg)
                    .stroke(Stroke::new(1.0, border))
                    .rounding(egui::Rounding::ZERO),
            )
            .show(ctx, |ui| {
                egui::Grid::new("settings_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Server URL").font(FontId::proportional(12.0)).color(fg));
                        let server_edit = ui.add(
                            egui::TextEdit::singleline(&mut self.config.server_url)
                                .desired_width(220.0)
                                .font(FontId::monospace(11.0)),
                        );
                        if server_edit.changed() {
                            let _ = config::save_config(&self.config);
                        }
                        ui.end_row();

                        ui.label(RichText::new("Chunk size (MB)").font(FontId::proportional(12.0)).color(fg));
                        let mut cs = self.config.chunk_size_mb as f32;
                        if ui.add(egui::Slider::new(&mut cs, 5.0..=100.0).integer()).changed() {
                            self.config.chunk_size_mb = cs as u64;
                            let _ = config::save_config(&self.config);
                        }
                        ui.end_row();

                        ui.label(RichText::new("Parallel chunks").font(FontId::proportional(12.0)).color(fg));
                        let mut pc = self.config.parallel_chunks as f32;
                        if ui.add(egui::Slider::new(&mut pc, 1.0..=5.0).integer()).changed() {
                            self.config.parallel_chunks = pc as usize;
                            let _ = config::save_config(&self.config);
                        }
                        ui.end_row();

                        ui.label(RichText::new("Expiration (days)").font(FontId::proportional(12.0)).color(fg));
                        let mut ed = self.config.expiration_days as f32;
                        if ui.add(egui::Slider::new(&mut ed, 1.0..=14.0).integer()).changed() {
                            self.config.expiration_days = ed as u32;
                            let _ = config::save_config(&self.config);
                        }
                        ui.end_row();

                        ui.label(RichText::new("On close").font(FontId::proportional(12.0)).color(fg));
                        ui.horizontal(|ui| {
                            for (label, val) in [("Ask", "ask"), ("Tray", "tray"), ("Quit", "quit")] {
                                let selected = self.config.close_behavior == val;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.config.close_behavior = val.to_string();
                                    let _ = config::save_config(&self.config);
                                }
                            }
                        });
                        ui.end_row();

                        ui.label(RichText::new("Theme").font(FontId::proportional(12.0)).color(fg));
                        ui.horizontal(|ui| {
                            for (label, t) in [("Light", Theme::Light), ("Dark", Theme::Dark), ("AMOLED", Theme::Amoled)] {
                                let selected = self.theme == t;
                                if ui.selectable_label(selected, label).clicked() {
                                    self.theme = t;
                                    self.config.theme = self.theme.to_str().to_string();
                                    let _ = config::save_config(&self.config);
                                    self.apply_theme(ctx);
                                }
                            }
                        });
                        ui.end_row();
                    });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Logout").font(FontId::proportional(12.0)).color(fg)).clicked() {
                        let server = self.config.server_url.clone();
                        let token = self.auth.token.clone();
                        let rt = self.rt.clone();
                        std::thread::spawn(move || {
                            rt.block_on(async {
                                let client = reqwest::Client::new();
                                auth::logout(&client, &server, &token).await;
                            });
                        });
                        self.auth = AuthState::default();
                        self.config.token = String::new();
                        let _ = config::save_config(&self.config);
                        self.show_settings = false;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_settings = false;
                        }
                    });
                });
            });
    }

    fn render_close_dialog(&mut self, ctx: &egui::Context) {
        let bg = self.theme.bg();
        let fg = self.theme.fg();
        let border = self.theme.border();

        egui::Window::new("Close naTiVeFS")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(bg)
                    .stroke(Stroke::new(1.0, border))
                    .rounding(egui::Rounding::ZERO),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new("What should happen when you close the window?").font(FontId::proportional(13.0)).color(fg));
                ui.add_space(8.0);

                ui.checkbox(
                    &mut self.remember_close,
                    RichText::new("Remember my choice").font(FontId::proportional(12.0)).color(fg),
                );

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    if ui.add_sized([120.0, 30.0], egui::Button::new(
                        RichText::new("Hide to tray").font(FontId::proportional(12.0)).color(bg)
                    ).fill(fg)).clicked() {
                        if self.remember_close {
                            self.config.close_behavior = "tray".to_string();
                            let _ = config::save_config(&self.config);
                        }
                        self.show_close_dialog = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    }

                    if ui.add_sized([80.0, 30.0], egui::Button::new(
                        RichText::new("Quit").font(FontId::proportional(12.0)).color(fg)
                    )).clicked() {
                        if self.remember_close {
                            self.config.close_behavior = "quit".to_string();
                            let _ = config::save_config(&self.config);
                        }
                        self.show_close_dialog = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    if ui.button(RichText::new("Cancel").font(FontId::proportional(12.0)).color(fg)).clicked() {
                        self.show_close_dialog = false;
                    }
                });
            });
    }
}
