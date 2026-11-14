//! State and behaviour of the full-screen interface. Drawing lives in `ui.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tvfs_core::config::{AutoCopy, Config};
use tvfs_core::models::{RecentFiles, Status, SyncEntry};
use tvfs_core::upload::{Control, Run, UploadRequest};
use tvfs_core::{session, Client};

use super::picker::{Kind, Picker};
use super::theme::Theme;
use crate::common::*;
use crate::expiry;
use crate::links::{self, Finished, Prefs};
use crate::settings::{CliSettings, ThemeMode};

pub enum Msg {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    LoggedIn(Result<String, String>),
    Status(Result<(u32, Status), String>),
    Recent(Result<Vec<SyncEntry>, String>),
    Picked(Vec<PathBuf>),
    Finished(Box<Finished>, f64),
    Styled(String, Option<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Login,
    Menu,
    Picker,
    Options,
    Uploading,
    Result,
    Recent,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Menu(usize),
    PickerRow(usize),
    Recent(usize),
    ResultRow(usize),
    Setting(usize),
    Expiry(usize),
    OptRow(usize),
}

pub const MENU: [(&str, char); 5] = [("Upload files", 'u'), ("Recent uploads", 'r'), ("Settings", 's'), ("Log out", 'l'), ("Quit", 'q')];
pub const SETTINGS_ROWS: usize = 5;

#[derive(Default)]
pub struct LoginState {
    pub user: String,
    pub pass: String,
    pub field: usize,
    pub busy: bool,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct StatusInfo {
    pub checked: bool,
    pub online: bool,
    pub latency_ms: Option<u32>,
    pub used: String,
    pub total_gb: f64,
    pub tier: u8,
}

pub struct Options {
    pub expiry: usize,
    pub anonymize: bool,
    pub users_only: bool,
    pub row: usize,
    pub files: Vec<tvfs_core::upload::UploadFile>,
}

pub struct UploadState {
    pub run: Arc<Run>,
    pub control: Control,
    pub confirm_cancel: bool,
}

#[derive(Default)]
pub struct ResultState {
    pub fin: Finished,
    pub elapsed: f64,
    pub cursor: usize,
    pub copied: Option<usize>,
    /// The default link is still being computed by the server and gets copied when it arrives.
    pub copy_when_styled: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResultRow {
    #[allow(dead_code)]
    pub file: Option<usize>,
    pub label: &'static str,
    pub url: String,
    pub default: bool,
    pub header: Option<String>,
}

#[derive(Default)]
pub struct RecentState {
    pub entries: Vec<SyncEntry>,
    pub cursor: usize,
    pub scroll: usize,
    pub loading: bool,
    pub error: Option<String>,
}

pub struct App {
    pub theme: Theme,
    pub truecolor: bool,
    pub cfg: Config,
    pub cli: CliSettings,
    pub client: Client,
    pub rt: tokio::runtime::Handle,
    pub tx: std::sync::mpsc::Sender<Msg>,
    pub screen: Screen,
    pub user: Option<String>,
    pub status: StatusInfo,
    pub toast: Option<(String, Instant, bool)>,
    pub hits: Vec<(Rect, Hit)>,
    pub quit: bool,
    pub dirty: bool,
    pub frame: u64,
    pub login: LoginState,
    pub menu: usize,
    pub picker: Picker,
    pub opts: Options,
    pub up: Option<UploadState>,
    pub result: ResultState,
    pub recent: RecentState,
    pub settings_row: usize,
    pub next_status: Option<Instant>,
    clipboard: Option<arboard::Clipboard>,
}

fn home_dir() -> PathBuf {
    directories::BaseDirs::new().map(|d| d.home_dir().to_owned()).unwrap_or_else(|| PathBuf::from("."))
}

impl App {
    pub fn new(rt: tokio::runtime::Handle, tx: std::sync::mpsc::Sender<Msg>, theme_override: Option<ThemeMode>) -> Self {
        let cfg = Config::load();
        let mut cli = CliSettings::load();
        if let Some(t) = theme_override {
            cli.theme = t;
            cli.save();
        }
        let truecolor = super::theme::detect_truecolor();
        let theme = Theme::new(cli.theme.is_light(&cfg), truecolor);
        let (client, user) = match client_with_saved_session() {
            Ok(Some((c, u))) => (c, Some(u)),
            _ => (Client::new().expect("http client"), None),
        };
        let start_dir = cli.last_dir.as_deref().map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(home_dir);
        let expiry_idx = expiry::index(cli.expiry_min.unwrap_or(tvfs_core::DEFAULT_EXPIRATION_MIN));
        let mut app = Self {
            theme,
            truecolor,
            opts: Options { expiry: expiry_idx, anonymize: false, users_only: cfg.tvfs_users_only, row: 0, files: Vec::new() },
            cfg,
            cli,
            client,
            rt,
            tx,
            screen: if user.is_some() { Screen::Menu } else { Screen::Login },
            user,
            status: StatusInfo::default(),
            toast: None,
            hits: Vec::new(),
            quit: false,
            dirty: true,
            frame: 0,
            login: LoginState::default(),
            menu: 0,
            picker: Picker::open(&start_dir),
            up: None,
            result: ResultState::default(),
            recent: RecentState::default(),
            settings_row: 0,
            next_status: None,
            clipboard: None,
        };
        if app.user.is_some() {
            app.refresh_status();
        }
        app
    }

    // ── timing ────────────────────────────────────────────────────────────

    /// How long the loop may sleep; `None` = until something happens (no redraw at rest).
    pub fn next_tick(&self) -> Option<Duration> {
        let mut d: Option<Duration> = None;
        let mut want = |x: Duration| d = Some(d.map_or(x, |o| o.min(x)));
        if self.screen == Screen::Uploading || self.login.busy || self.recent.loading {
            want(Duration::from_millis(100));
        }
        if let Some((_, at, _)) = &self.toast {
            want((*at + Duration::from_secs(3)).saturating_duration_since(Instant::now()).max(Duration::from_millis(20)));
        }
        if let Some(at) = self.next_status {
            want(at.saturating_duration_since(Instant::now()).max(Duration::from_millis(20)));
        }
        d
    }

    pub fn on_tick(&mut self) {
        self.frame += 1;
        if self.screen == Screen::Uploading || self.login.busy || self.recent.loading {
            self.dirty = true;
        }
        if self.toast.as_ref().is_some_and(|(_, at, _)| at.elapsed() >= Duration::from_secs(3)) {
            self.toast = None;
            self.dirty = true;
        }
        if self.next_status.is_some_and(|at| Instant::now() >= at) {
            self.refresh_status();
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, bad: bool) {
        self.toast = Some((text.into(), Instant::now(), bad));
        self.dirty = true;
    }

    fn refresh_status(&mut self) {
        self.next_status = Some(Instant::now() + Duration::from_secs(30));
        let (client, tx) = (self.client.clone(), self.tx.clone());
        self.rt.spawn(async move {
            let t0 = Instant::now();
            let r = client.status().await.map(|s| (t0.elapsed().as_millis() as u32, s)).map_err(|e| if e.is_auth() { "auth".to_owned() } else { e.to_string() });
            let _ = tx.send(Msg::Status(r));
        });
    }

    pub fn prefs(&self) -> Prefs {
        Prefs::from_config(&self.cfg)
    }

    // ── messages ──────────────────────────────────────────────────────────

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Key(k) => self.on_key(k),
            Msg::Mouse(m) => self.on_mouse(m),
            Msg::Paste(s) => self.on_paste(&s),
            Msg::Resize => self.dirty = true,
            Msg::LoggedIn(r) => {
                self.login.busy = false;
                match r {
                    Ok(user) => {
                        self.user = Some(user);
                        self.login = LoginState::default();
                        self.screen = Screen::Menu;
                        self.refresh_status();
                    }
                    Err(e) => self.login.error = Some(e),
                }
                self.dirty = true;
            }
            Msg::Status(r) => {
                self.status.checked = true;
                match r {
                    Ok((ms, s)) => {
                        self.status.online = true;
                        self.status.latency_ms = Some(ms);
                        self.status.used = s.used_gb;
                        self.status.total_gb = s.total_gb;
                        self.status.tier = s.tier;
                    }
                    Err(e) if e == "auth" => {
                        self.status.online = true;
                        self.sign_out_local("Your session expired. Please log in again.");
                    }
                    Err(_) => {
                        self.status.online = false;
                        self.status.latency_ms = None;
                    }
                }
                self.dirty = true;
            }
            Msg::Recent(r) => {
                self.recent.loading = false;
                match r {
                    Ok(v) => {
                        self.recent.entries = v;
                        self.recent.error = None;
                        self.recent.cursor = self.recent.cursor.min(self.recent.entries.len().saturating_sub(1));
                    }
                    Err(e) => self.recent.error = Some(e),
                }
                self.dirty = true;
            }
            Msg::Picked(paths) => {
                for p in paths {
                    self.picker.selected.insert(p);
                }
                self.dirty = true;
            }
            Msg::Finished(fin, elapsed) => self.on_finished(*fin, elapsed),
            Msg::Styled(id, link) => self.on_styled(&id, link),
        }
    }

    fn sign_out_local(&mut self, why: &str) {
        self.user = None;
        self.up = None;
        self.recent = RecentState::default();
        self.next_status = None;
        self.status = StatusInfo::default();
        self.client = Client::new().expect("http client");
        self.screen = Screen::Login;
        self.login = LoginState { error: (!why.is_empty()).then(|| why.to_owned()), ..Default::default() };
    }

    // ── keyboard ──────────────────────────────────────────────────────────

    fn on_paste(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        match self.screen {
            Screen::Login if !self.login.busy => {
                if self.login.field == 0 { self.login.user.push_str(&clean) } else { self.login.pass.push_str(&clean) }
            }
            Screen::Picker => self.picker.filter.push_str(&clean),
            _ => {}
        }
        self.dirty = true;
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        self.dirty = true;
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            if self.screen == Screen::Uploading {
                self.ask_cancel();
            } else {
                self.quit = true;
            }
            return;
        }
        match self.screen {
            Screen::Login => self.key_login(k),
            Screen::Menu => self.key_menu(k),
            Screen::Picker => self.key_picker(k),
            Screen::Options => self.key_options(k),
            Screen::Uploading => self.key_uploading(k),
            Screen::Result => self.key_result(k),
            Screen::Recent => self.key_recent(k),
            Screen::Settings => self.key_settings(k),
        }
    }

    fn key_login(&mut self, k: KeyEvent) {
        if self.login.busy {
            return;
        }
        let l = &mut self.login;
        match k.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Tab | KeyCode::Down | KeyCode::Up | KeyCode::BackTab => l.field = 1 - l.field,
            KeyCode::Enter => {
                if l.field == 0 && !l.user.is_empty() {
                    l.field = 1;
                } else {
                    self.submit_login();
                }
            }
            KeyCode::Backspace => {
                if l.field == 0 { l.user.pop() } else { l.pass.pop() };
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                if l.field == 0 { l.user.push(c) } else { l.pass.push(c) }
            }
            _ => {}
        }
    }

    fn submit_login(&mut self) {
        if self.login.user.trim().is_empty() || self.login.pass.is_empty() {
            self.login.error = Some("Enter your username and password.".into());
            return;
        }
        self.login.busy = true;
        self.login.error = None;
        let (client, tx) = (self.client.clone(), self.tx.clone());
        let (user, pass) = (self.login.user.trim().to_owned(), self.login.pass.clone());
        self.rt.spawn(async move {
            let r = async {
                client.login(&user, &pass, true).await.map_err(|e| e.to_string())?;
                let token = client.session_token().ok_or("The server did not start a session.")?;
                let s = session::StoredSession { username: user.clone(), token };
                tokio::task::spawn_blocking(move || session::save(&s)).await.map_err(|e| e.to_string())?.map_err(|e| format!("Logged in, but the login could not be saved: {e}"))?;
                Ok::<String, String>(user)
            }
            .await;
            let _ = tx.send(Msg::LoggedIn(r));
        });
    }

    fn key_menu(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.menu = (self.menu + MENU.len() - 1) % MENU.len(),
            KeyCode::Down | KeyCode::Char('j') => self.menu = (self.menu + 1) % MENU.len(),
            KeyCode::Enter | KeyCode::Right => self.menu_action(self.menu),
            KeyCode::Esc => self.quit = true,
            KeyCode::Char(c) => {
                if let Some(i) = MENU.iter().position(|(_, h)| *h == c.to_ascii_lowercase()) {
                    self.menu = i;
                    self.menu_action(i);
                }
            }
            _ => {}
        }
    }

    fn menu_action(&mut self, i: usize) {
        match i {
            0 => {
                self.picker.reload();
                self.screen = Screen::Picker;
            }
            1 => {
                self.screen = Screen::Recent;
                self.load_recent();
            }
            2 => self.screen = Screen::Settings,
            3 => {
                let client = self.client.clone();
                self.rt.spawn(async move {
                    let _ = client.logout().await;
                });
                let _ = session::clear();
                self.sign_out_local("");
            }
            _ => self.quit = true,
        }
    }

    fn key_picker(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let p = &mut self.picker;
        match k.code {
            KeyCode::Esc => {
                if p.filter.is_empty() {
                    self.screen = Screen::Menu;
                } else {
                    p.filter.clear();
                    p.cursor = 0;
                }
            }
            KeyCode::Up => p.move_by(-1),
            KeyCode::Down => p.move_by(1),
            KeyCode::PageUp => p.move_by(-10),
            KeyCode::PageDown => p.move_by(10),
            KeyCode::Home => p.jump(false),
            KeyCode::End => p.jump(true),
            KeyCode::Left => p.up(),
            KeyCode::Right => {
                if p.current().is_some_and(|e| matches!(e.kind, Kind::Dir | Kind::Drive)) {
                    p.activate();
                }
            }
            KeyCode::Enter => p.activate(),
            KeyCode::Backspace => p.backspace(),
            KeyCode::Tab => self.start_options(),
            KeyCode::Char('a') if ctrl => p.select_all_files(),
            KeyCode::Char('d') if ctrl => p.selected.clear(),
            KeyCode::Char('h') if ctrl => {
                p.show_hidden = !p.show_hidden;
                p.reload();
            }
            KeyCode::Char('o') if ctrl => self.system_dialog(),
            KeyCode::Char(' ') if p.filter.is_empty() => p.toggle_current(true),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => p.type_char(c),
            _ => {}
        }
    }

    fn system_dialog(&mut self) {
        let tx = self.tx.clone();
        let dir = self.picker.dir.clone();
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new().set_title("Choose files to upload");
            if let Some(dir) = dir {
                d = d.set_directory(dir);
            }
            if let Some(files) = d.pick_files() {
                let _ = tx.send(Msg::Picked(files));
            }
        });
        self.toast("System file dialog opened (it may be behind this window).", false);
    }

    fn start_options(&mut self) {
        if self.picker.selected.is_empty() {
            if let Some(e) = self.picker.current().cloned() {
                if e.kind == Kind::File {
                    self.picker.selected.insert(e.path);
                }
            }
        }
        if self.picker.selected.is_empty() {
            self.toast("Select at least one file first (Space or Enter).", true);
            return;
        }
        let paths: Vec<PathBuf> = self.picker.selected.iter().cloned().collect();
        let (files, problems) = collect_files(&paths);
        if let Some(p) = problems.first() {
            self.toast(p.clone(), true);
            if files.is_empty() {
                return;
            }
        }
        if files.is_empty() {
            self.toast("Nothing uploadable in this selection.", true);
            return;
        }
        if let Some(d) = &self.picker.dir {
            self.cli.last_dir = Some(d.display().to_string());
        }
        self.opts.files = files;
        self.opts.row = 0;
        self.opts.users_only = self.cfg.tvfs_users_only;
        self.screen = Screen::Options;
    }

    fn key_options(&mut self, k: KeyEvent) {
        let o = &mut self.opts;
        match k.code {
            KeyCode::Esc => self.screen = Screen::Picker,
            KeyCode::Up => o.row = o.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => o.row = (o.row + 1).min(2),
            KeyCode::Left if o.row == 0 => o.expiry = o.expiry.saturating_sub(1),
            KeyCode::Right if o.row == 0 => o.expiry = (o.expiry + 1).min(11),
            KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right => match o.row {
                1 => o.anonymize = !o.anonymize,
                2 => o.users_only = !o.users_only,
                _ => {}
            },
            KeyCode::Enter => self.start_upload(),
            _ => {}
        }
    }

    fn start_upload(&mut self) {
        let mut files = std::mem::take(&mut self.opts.files);
        if files.is_empty() {
            return;
        }
        for f in &mut files {
            f.set_anonymized(self.opts.anonymize);
        }
        let minutes = tvfs_core::EXPIRATION_OPTIONS_MIN[self.opts.expiry];
        self.cli.expiry_min = Some(minutes);
        self.cli.save();
        let names: Vec<(String, u64)> = files.iter().map(|f| (f.upload_name(), f.size)).collect();
        let control = Control::default();
        let run = Run::new(names.clone(), control.clone());
        let req = UploadRequest {
            files,
            expiration_minutes: minutes,
            remove_timestamp: self.status.tier == 2,
            chunking: chunk_mode(&self.cfg),
            protected: self.opts.users_only,
        };
        let uploader = new_uploader(&self.client);
        let (tx, run2) = (self.tx.clone(), run.clone());
        self.rt.spawn(async move {
            let t0 = Instant::now();
            let report = uploader.run(req, run2, Arc::new(|_| {})).await;
            let _ = tx.send(Msg::Finished(Box::new(Finished::from_report(report, &names)), t0.elapsed().as_secs_f64()));
        });
        self.up = Some(UploadState { run, control, confirm_cancel: false });
        self.screen = Screen::Uploading;
    }

    fn ask_cancel(&mut self) {
        if let Some(u) = &mut self.up {
            if u.confirm_cancel {
                u.control.cancel();
                self.toast("Cancelling...", false);
            } else {
                u.confirm_cancel = true;
            }
        }
    }

    fn key_uploading(&mut self, k: KeyEvent) {
        let Some(u) = &mut self.up else { return };
        match k.code {
            KeyCode::Char('p') | KeyCode::Char(' ') => u.control.set_paused(!u.control.is_paused()),
            KeyCode::Char('c') | KeyCode::Esc => {
                if u.confirm_cancel && k.code == KeyCode::Esc {
                    u.confirm_cancel = false;
                } else {
                    self.ask_cancel();
                }
            }
            KeyCode::Char('y') if u.confirm_cancel => self.ask_cancel(),
            KeyCode::Char('n') => u.confirm_cancel = false,
            _ => {}
        }
    }

    fn on_finished(&mut self, fin: Finished, elapsed: f64) {
        self.up = None;
        self.picker.selected.clear();
        self.result = ResultState { fin, elapsed, ..Default::default() };
        self.screen = Screen::Result;
        self.dirty = true;
        // wait for styled links the server is still computing
        let prefs = self.prefs();
        let mut waits = None;
        for f in &self.result.fin.files {
            if let Ok(r) = &f.result {
                if r.styled_link_pending && r.styled_link.is_none() && !r.file_id.is_empty() {
                    let (client, tx, id) = (self.client.clone(), self.tx.clone(), r.file_id.clone());
                    if links::default_waits_for_styled(r, prefs) && self.result.fin.files.len() == 1 {
                        waits = Some(id.clone());
                    }
                    self.rt.spawn(async move {
                        let l = links::poll_styled(&client, &id).await;
                        let _ = tx.send(Msg::Styled(id, l));
                    });
                }
            }
        }
        if waits.is_some() {
            self.result.copy_when_styled = waits;
            return;
        }
        self.auto_copy();
    }

    fn on_styled(&mut self, id: &str, link: Option<String>) {
        let mut idx = None;
        for (i, f) in self.result.fin.files.iter_mut().enumerate() {
            if let Ok(r) = &mut f.result {
                if r.file_id == id {
                    r.styled_link_pending = false;
                    if link.is_some() {
                        r.styled_link = link.clone();
                    }
                    idx = Some(i);
                }
            }
        }
        if idx.is_some() && self.result.copy_when_styled.as_deref() == Some(id) {
            self.result.copy_when_styled = None;
            self.auto_copy();
        }
        self.dirty = true;
    }

    /// The links of the result screen, flattened. In a batch the batch page is the default (what the app copies).
    pub fn result_rows(&self) -> Vec<ResultRow> {
        let prefs = self.prefs();
        let fin = &self.result.fin;
        let mut rows = Vec::new();
        let batch = fin.batch_link.is_some();
        if let Some(l) = &fin.batch_link {
            let styled_default = prefs.auto == AutoCopy::Styled && fin.batch_styled.is_some();
            rows.push(ResultRow { file: None, label: "Batch page", url: l.clone(), default: prefs.auto != AutoCopy::Off && !styled_default, header: Some(format!("All {} files", fin.files.len())) });
            if let Some(s) = &fin.batch_styled {
                rows.push(ResultRow { file: None, label: "Batch page (styled)", url: s.clone(), default: styled_default, header: None });
            }
        }
        if let Some(s) = &fin.share_link {
            rows.push(ResultRow { file: None, label: "Share link (TVFS users only)", url: s.clone(), default: prefs.auto != AutoCopy::Off, header: Some("TVFS Users Only share".into()) });
        }
        for (i, f) in fin.files.iter().enumerate() {
            match &f.result {
                Ok(r) if fin.share_link.is_none() => {
                    let items = links::collect(r, prefs);
                    for (n, l) in items.into_iter().enumerate() {
                        let header = (n == 0).then(|| format!("{}  ({})", f.name, tvfs_core::fmt::fmt_bytes(r.size)));
                        rows.push(ResultRow { file: Some(i), label: l.label, url: l.url, default: l.default && !batch, header });
                    }
                }
                Ok(_) => {}
                Err(e) => rows.push(ResultRow { file: Some(i), label: "FAILED", url: e.clone(), default: false, header: Some(f.name.clone()) }),
            }
        }
        rows
    }

    fn auto_copy(&mut self) {
        let rows = self.result_rows();
        if let Some(i) = rows.iter().position(|r| r.default && r.label != "FAILED") {
            self.result.cursor = i;
            if self.copy_text(&rows[i].url) {
                self.result.copied = Some(i);
                self.toast("Default link copied to the clipboard", false);
            }
        }
    }

    pub fn copy_text(&mut self, text: &str) -> bool {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        match self.clipboard.as_mut().map(|c| c.set_text(text.to_owned())) {
            Some(Ok(())) => true,
            _ => {
                self.toast("Clipboard is not available here.", true);
                false
            }
        }
    }

    fn key_result(&mut self, k: KeyEvent) {
        let n = self.result_rows().len();
        let r = &mut self.result;
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => r.cursor = r.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => r.cursor = (r.cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Enter | KeyCode::Char('c') => self.copy_result_row(),
            KeyCode::Char('o') => {
                if let Some(row) = self.result_rows().get(self.result.cursor).filter(|r| r.url.starts_with("http")) {
                    self.open(&row.url.clone());
                }
            }
            KeyCode::Char('n') => {
                self.picker.reload();
                self.screen = Screen::Picker;
            }
            KeyCode::Esc | KeyCode::Char('m') => self.screen = Screen::Menu,
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn copy_result_row(&mut self) {
        let rows = self.result_rows();
        let i = self.result.cursor;
        if let Some(row) = rows.get(i).filter(|r| r.label != "FAILED") {
            if self.copy_text(&row.url) {
                self.result.copied = Some(i);
                self.toast("Copied", false);
            }
        }
    }

    pub fn open(&mut self, url: &str) {
        let ok = self.cfg.open_in_browser(url).is_ok() || open_default(url);
        if !ok {
            self.toast("Could not open a browser.", true);
        }
    }

    fn load_recent(&mut self) {
        self.recent.loading = true;
        let (client, tx) = (self.client.clone(), self.tx.clone());
        self.rt.spawn(async move {
            let r = client.sync(None).await.map(|resp| {
                let mut rf = RecentFiles::default();
                rf.apply(resp);
                rf.files
            });
            let _ = tx.send(Msg::Recent(r.map_err(|e| e.to_string())));
        });
    }

    fn key_recent(&mut self, k: KeyEvent) {
        let n = self.recent.entries.len();
        let c = &mut self.recent.cursor;
        match k.code {
            KeyCode::Esc | KeyCode::Char('m') => self.screen = Screen::Menu,
            KeyCode::Up | KeyCode::Char('k') => *c = c.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *c = (*c + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => *c = c.saturating_sub(10),
            KeyCode::PageDown => *c = (*c + 10).min(n.saturating_sub(1)),
            KeyCode::Home => *c = 0,
            KeyCode::End => *c = n.saturating_sub(1),
            KeyCode::Char('r') => self.load_recent(),
            KeyCode::Enter | KeyCode::Char('c') | KeyCode::Char('l') | KeyCode::Char('s') => {
                let Some(e) = self.recent.entries.get(self.recent.cursor).cloned() else { return };
                let styled = match k.code {
                    KeyCode::Char('s') => true,
                    KeyCode::Char('l') => false,
                    _ => self.cfg.auto_copy == AutoCopy::Styled,
                };
                let text = if styled { format!("[{}]({})", e.name, e.link) } else { e.link.clone() };
                if self.copy_text(&text) {
                    self.toast(if styled { "Styled link copied" } else { "Link copied" }, false);
                }
            }
            KeyCode::Char('o') => {
                if let Some(e) = self.recent.entries.get(self.recent.cursor).cloned() {
                    self.open(&e.link);
                }
            }
            _ => {}
        }
    }

    fn key_settings(&mut self, k: KeyEvent) {
        match k.code {
            KeyCode::Esc | KeyCode::Char('m') => self.screen = Screen::Menu,
            KeyCode::Up | KeyCode::Char('k') => self.settings_row = self.settings_row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.settings_row = (self.settings_row + 1).min(SETTINGS_ROWS - 1),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char(' ') => self.change_setting(true),
            KeyCode::Left => self.change_setting(false),
            _ => {}
        }
    }

    fn change_setting(&mut self, forward: bool) {
        // the shared options are re-read from disk so a change made by the app meanwhile is not overwritten
        let mut shared = Config::load();
        match self.settings_row {
            0 => {
                self.cli.theme = self.cli.theme.next();
                self.cli.save();
                self.theme = Theme::new(self.cli.theme.is_light(&shared), self.truecolor);
                return;
            }
            1 => {
                let all = [AutoCopy::Off, AutoCopy::Normal, AutoCopy::Styled];
                let i = all.iter().position(|a| *a == shared.auto_copy).unwrap_or(2);
                shared.auto_copy = all[if forward { (i + 1) % 3 } else { (i + 2) % 3 }];
            }
            2 => shared.auto_copy_prefer_tool = !shared.auto_copy_prefer_tool,
            3 => shared.auto_copy_image_styled = !shared.auto_copy_image_styled,
            _ => shared.tvfs_users_only = !shared.tvfs_users_only,
        }
        if let Err(e) = shared.save() {
            self.toast(format!("Could not save: {e}"), true);
        }
        self.cfg = shared;
    }

    // ── mouse ─────────────────────────────────────────────────────────────

    fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let d: isize = if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 };
                match self.screen {
                    Screen::Picker => self.picker.move_by(d),
                    Screen::Recent => {
                        let n = self.recent.entries.len().saturating_sub(1) as isize;
                        self.recent.cursor = (self.recent.cursor as isize + d).clamp(0, n) as usize;
                    }
                    Screen::Result => {
                        let n = self.result_rows().len().saturating_sub(1) as isize;
                        self.result.cursor = (self.result.cursor as isize + d).clamp(0, n) as usize;
                    }
                    _ => return,
                }
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self.hits.iter().rev().find(|(r, _)| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height).map(|(_, h)| *h);
                if let Some(h) = hit {
                    self.click(h);
                    self.dirty = true;
                }
            }
            _ => {}
        }
    }

    fn click(&mut self, h: Hit) {
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        match h {
            Hit::Menu(i) => {
                self.menu = i;
                self.menu_action(i);
            }
            Hit::PickerRow(i) => {
                let again = self.picker.cursor == i;
                self.picker.cursor = i;
                if again {
                    self.picker.activate();
                }
            }
            Hit::Recent(i) => self.recent.cursor = i,
            Hit::ResultRow(i) => {
                let again = self.result.cursor == i;
                self.result.cursor = i;
                if again {
                    self.key_result(enter);
                }
            }
            Hit::Setting(i) => {
                let again = self.settings_row == i;
                self.settings_row = i;
                if again {
                    self.change_setting(true);
                }
            }
            Hit::Expiry(i) => {
                self.opts.expiry = i;
                self.opts.row = 0;
            }
            Hit::OptRow(i) => {
                self.opts.row = i;
                if i == 1 {
                    self.opts.anonymize = !self.opts.anonymize;
                } else if i == 2 {
                    self.opts.users_only = !self.opts.users_only;
                }
            }
        }
    }
}

/// Open a link with the system's default handler (used when no browser is configured).
fn open_default(url: &str) -> bool {
    use std::process::Command;
    let r = if cfg!(windows) {
        Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
    r.is_ok()
}
