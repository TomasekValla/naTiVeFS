//! Single-file window (download with progress, the audio stub, errors) and the batch / share file list.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};
use tokio_util::sync::CancellationToken;
use tvfs_core::content::{BatchPage, Content, ContentKind, ShareMeta};
use tvfs_core::fmt::fmt_bytes;
use tvfs_core::markdown::{self, Span};

use crate::{windows, FileWindow, ListFile, ListWindow, Shared};

thread_local! {
    /// Open windows stay alive here until they are closed.
    static FILE_WINDOWS: RefCell<Vec<FileWindow>> = const { RefCell::new(Vec::new()) };
    static LIST_WINDOWS: RefCell<Vec<ListWindow>> = const { RefCell::new(Vec::new()) };
}

fn keep_file(w: &FileWindow) {
    FILE_WINDOWS.with(|l| l.borrow_mut().push(w.clone_strong()));
    let weak = w.as_weak();
    w.window().on_close_requested(move || {
        if let Some(w) = weak.upgrade() {
            w.invoke_cancel();
            FILE_WINDOWS.with(|l| l.borrow_mut().retain(|x| !std::ptr::eq(x.window(), w.window())));
        }
        crate::windows::closed();
            slint::CloseRequestResponse::HideWindow
    });
}

fn file_toast(w: &FileWindow, text: &str) {
    let weak = w.as_weak();
    windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, text);
}

fn new_file_window(shared: &Arc<Shared>) -> Option<FileWindow> {
    let w = FileWindow::new().ok()?;
    w.set_light(shared.config.lock().unwrap().light_theme);
    let mut hooks = windows::Hooks::new();
    {
        let weak = w.as_weak();
        hooks.paste_link = Some(crate::open::paste_hook(shared, move |m| {
            if let Some(w) = weak.upgrade() {
                file_toast(&w, m);
            }
        }));
    }
    windows::attach(w.window(), hooks);
    Some(w)
}

/// A file the app does not show itself (or audio until the stage-4 player): offer a download.
pub fn open_file(shared: &Arc<Shared>, c: Content) {
    let Some(w) = new_file_window(shared) else { return };
    w.set_file_name(c.name.clone().into());
    let kind = match c.kind {
        ContentKind::Audio => "Audio",
        ContentKind::Video => "Video",
        ContentKind::Image => "Image",
        ContentKind::Text | ContentKind::Markdown => "Text",
        ContentKind::File => "File",
    };
    w.set_info(format!("{kind}{}", if c.protected { " · TVFS Users Only" } else { "" }).into());
    if c.kind == ContentKind::Audio {
        w.set_mode(1);
        w.set_message("The native audio player (bit-perfect output) comes with the next naTiVeFS update. Until then you can download the file or play it in the TVFS web player.".into());
    }
    w.set_link(c.page.clone().into());
    wire_download(shared, &w, c);
    keep_file(&w);
    windows::place(w.window());
    let _ = w.show();
    // the size, without downloading anything
    let (s2, weak, url) = (shared.clone(), w.as_weak(), w.get_link().to_string());
    let _ = (s2, url);
    let _ = weak;
}

/// A link that could not be opened.
pub fn show_error(shared: &Arc<Shared>, link: &str, message: &str) {
    let Some(w) = new_file_window(shared) else { return };
    w.set_mode(2);
    w.set_message(message.into());
    w.set_link(link.into());
    w.set_info(link.into());
    let link2 = link.to_owned();
    let shared2 = shared.clone();
    w.on_open_browser(move || crate::open_external(&link2, &shared2.config.lock().unwrap()));
    {
        let (weak, link) = (w.as_weak(), link.to_owned());
        w.on_copy_link(move || {
            crate::copy_to_clipboard(&link);
            if let Some(w) = weak.upgrade() {
                file_toast(&w, "Link copied");
            }
        });
    }
    keep_file(&w);
    windows::place(w.window());
    let _ = w.show();
}

/// "Show in folder" for a local file: Explorer selects it (Windows), elsewhere its folder opens.
pub fn show_in_folder(path: &std::path::Path) {
    #[cfg(windows)]
    {
        // Explorer wants `/select,"path"` as is (Rust would quote the whole argument)
        use std::os::windows::process::CommandExt;
        if std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", path.display())).spawn().is_ok() {
            return;
        }
    }
    if let Some(dir) = path.parent() {
        let _ = opener::open(dir);
    }
}

/// Download `c` through a file window (used by the viewers' Download buttons when the bytes are not loaded).
pub fn save_as(shared: &Arc<Shared>, c: &Content) {
    let Some(w) = new_file_window(shared) else { return };
    w.set_file_name(c.name.clone().into());
    w.set_link(c.page.clone().into());
    wire_download(shared, &w, c.clone());
    keep_file(&w);
    windows::place(w.window());
    let _ = w.show();
    w.invoke_download();
}

/// Save bytes the app already has (reader, image viewer) under a name the user picks.
pub fn save_bytes(name: &str, bytes: Vec<u8>, done: impl Fn(&str) + 'static) {
    let dialog = rfd::AsyncFileDialog::new().set_file_name(name).set_directory(download_dir());
    let _ = slint::spawn_local(async move {
        let Some(h) = dialog.save_file().await else { return };
        let path = h.path().to_path_buf();
        match std::fs::write(&path, bytes) {
            Ok(()) => done("Saved"),
            Err(e) => done(&format!("Could not save: {e}")),
        }
    });
}

fn download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|u| u.download_dir().map(Path::to_path_buf))
        .or_else(|| directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// "12.3 MB of 120 MB · 8.5 MB/s"
fn progress_text(done: u64, total: Option<u64>, started: Instant) -> String {
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let speed = done as f64 / secs;
    match total {
        Some(t) => format!("{} of {} · {}/s", fmt_bytes(done), fmt_bytes(t), fmt_bytes(speed as u64)),
        None => format!("{} · {}/s", fmt_bytes(done), fmt_bytes(speed as u64)),
    }
}

fn wire_download(shared: &Arc<Shared>, w: &FileWindow, c: Content) {
    let cancel: Rc<RefCell<Option<CancellationToken>>> = Rc::default();
    let saved: Rc<RefCell<Option<PathBuf>>> = Rc::default();
    {
        let (shared, weak, cancel, saved, c) = (shared.clone(), w.as_weak(), cancel.clone(), saved.clone(), c.clone());
        w.on_download(move || {
            let Some(w) = weak.upgrade() else { return };
            if w.get_busy() {
                return;
            }
            let dialog = w
                .window()
                .with_winit_window_parent(rfd::AsyncFileDialog::new())
                .set_file_name(&c.name)
                .set_directory(download_dir());
            let (shared, weak, cancel, saved, url) = (shared.clone(), weak.clone(), cancel.clone(), saved.clone(), c.url.clone());
            let _ = slint::spawn_local(async move {
                let Some(h) = dialog.save_file().await else { return };
                let dest = h.path().to_path_buf();
                let Some(w) = weak.upgrade() else { return };
                let token = CancellationToken::new();
                *cancel.borrow_mut() = Some(token.clone());
                w.set_busy(true);
                w.set_done(false);
                w.set_progress(0.0);
                w.set_progress_text("Starting…".into());
                w.set_message("".into());
                let started = Instant::now();
                let last = Arc::new(std::sync::Mutex::new(Instant::now() - Duration::from_secs(1)));
                let weak2 = weak.clone();
                let dest2 = dest.clone();
                let result = shared
                    .rt
                    .spawn({
                        let shared = shared.clone();
                        async move {
                            shared
                                .client
                                .download_to(&url, &dest2, token, move |done, total| {
                                    let mut l = last.lock().unwrap();
                                    if l.elapsed() < Duration::from_millis(150) && total != Some(done) {
                                        return;
                                    }
                                    *l = Instant::now();
                                    let weak = weak2.clone();
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(w) = weak.upgrade() {
                                            w.set_progress(total.map_or(0.0, |t| done as f32 / t.max(1) as f32));
                                            w.set_progress_text(progress_text(done, total, started).into());
                                        }
                                    });
                                })
                                .await
                        }
                    })
                    .await;
                let Some(w) = weak.upgrade() else { return };
                w.set_busy(false);
                match result {
                    Ok(Ok(bytes)) => {
                        w.set_progress(1.0);
                        w.set_progress_text(format!("Saved {} to {}", fmt_bytes(bytes), dest.display()).into());
                        w.set_done(true);
                        *saved.borrow_mut() = Some(dest);
                    }
                    Ok(Err(e)) => {
                        w.set_progress(-1.0);
                        let msg = if e.is_auth() { "This file is for TVFS users only — log in in naTiVeFS first.".to_owned() } else { format!("Download failed: {e}") };
                        w.set_message(if msg.contains("cancelled") { "Download cancelled.".into() } else { msg.into() });
                    }
                    Err(_) => w.set_progress(-1.0),
                }
            });
        });
    }
    {
        let cancel = cancel.clone();
        w.on_cancel(move || {
            if let Some(t) = cancel.borrow_mut().take() {
                t.cancel();
            }
        });
    }
    {
        let saved = saved.clone();
        w.on_open_folder(move || {
            if let Some(dir) = saved.borrow().as_ref().and_then(|p| p.parent()) {
                let _ = opener::open(dir);
            }
        });
    }
    {
        let (shared, page) = (shared.clone(), c.page.clone());
        w.on_open_browser(move || crate::open_external(&page, &shared.config.lock().unwrap()));
    }
    {
        let (weak, page) = (w.as_weak(), c.page.clone());
        w.on_copy_link(move || {
            crate::copy_to_clipboard(&page);
            if let Some(w) = weak.upgrade() {
                file_toast(&w, "Link copied");
            }
        });
    }
}

// ── batch / share list ────────────────────────────────────────────────────

struct ListState {
    window: ListWindow,
    contents: Vec<Content>,
    links: Vec<(String, String)>,
    page: (String, String),
    batch_id: Option<String>,
}

fn gradient_heading(text: &str) -> slint::private_unstable_api::re_exports::StyledText {
    let spans = vec![Span { text: text.to_owned(), style: markdown::STRONG, link: None }];
    let markup = markdown::to_markup(&spans, &[], Some((0x6c5ce7, 0x00b894)), markdown::Colors::default());
    slint::private_unstable_api::re_exports::StyledText::parse_interpolated::<slint::private_unstable_api::re_exports::StyledText>(&markup, &[])
        .unwrap_or_default()
}

pub fn open_batch(shared: &Arc<Shared>, b: BatchPage) {
    let base = url::Url::parse(tvfs_core::base_url()).ok();
    let contents: Vec<Content> = b
        .files
        .iter()
        .map(|f| Content {
            url: f.url.clone(),
            kind: tvfs_core::content::kind_from_name(&f.name),
            name: f.name.clone(),
            protected: false,
            page: f.url.clone(),
        })
        .collect();
    let files: Vec<ListFile> = b
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| ListFile { name: tvfs_core::fmt::truncate_filename(&f.name, 32).into(), size: b.sizes.get(i).cloned().unwrap_or_default().into(), state: 0, pct: 0.0 })
        .collect();
    let links = b.files.iter().map(|f| (f.url.clone(), f.styled.clone())).collect();
    let _ = base;
    open_list(shared, "TomasekValla", " Shared Files", &b.subtitle, &b.expire_note, files, contents, links, (b.page_link.clone(), b.page_styled.clone()), Some(b.id.clone()));
}

pub fn open_share(shared: &Arc<Shared>, meta: ShareMeta, page: String) {
    let Ok(base) = url::Url::parse(tvfs_core::base_url()) else { return };
    let contents: Vec<Content> = meta.files.iter().map(|f| meta.content(&base, f)).collect();
    let files: Vec<ListFile> = meta
        .files
        .iter()
        .map(|f| ListFile { name: tvfs_core::fmt::truncate_filename(&f.relative_path, 40).into(), size: fmt_bytes(f.size).into(), state: 0, pct: 0.0 })
        .collect();
    let links = meta.files.iter().zip(&contents).map(|(f, c)| (f.tool_link.clone().unwrap_or_else(|| c.url.clone()), String::new())).collect();
    let n = meta.files.len();
    let subtitle = format!("{n} file{} \u{2022} {}", if n == 1 { "" } else { "s" }, fmt_bytes(meta.total_size));
    let expire = chrono::DateTime::from_timestamp_millis(meta.expires_at)
        .map(|d| format!("These files expire on {}", d.with_timezone(&chrono::Local).format("%B %-d, %Y")))
        .unwrap_or_default();
    open_list(shared, "TVFS Users Only", " Shared Files", &subtitle, &expire, files, contents, links, (page, String::new()), None);
}

#[allow(clippy::too_many_arguments)]
fn open_list(
    shared: &Arc<Shared>,
    heading_a: &str,
    heading_b: &str,
    subtitle: &str,
    expire: &str,
    files: Vec<ListFile>,
    contents: Vec<Content>,
    links: Vec<(String, String)>,
    page: (String, String),
    batch_id: Option<String>,
) {
    let Ok(w) = ListWindow::new() else { return };
    w.set_heading_styled(gradient_heading(heading_a));
    w.set_heading_a(heading_a.into());
    w.set_heading_b(heading_b.into());
    w.set_subtitle(subtitle.into());
    w.set_expire_note(expire.into());
    w.set_files(ModelRc::new(VecModel::from(files)));
    w.set_show_zip(batch_id.is_some());
    w.set_show_styled(batch_id.is_some());
    let state = Rc::new(RefCell::new(ListState { window: w.clone_strong(), contents, links, page, batch_id }));
    {
        let (shared, st) = (shared.clone(), Rc::downgrade(&state));
        w.on_open_file(move |i| {
            if let Some(st) = st.upgrade() {
                if let Some(c) = st.borrow().contents.get(i.max(0) as usize).cloned() {
                    crate::open::open_content(&shared, c);
                }
            }
        });
    }
    let copied = |w: &ListWindow, row: i32, kind: i32| {
        w.set_copied_row(row);
        w.set_copied_kind(kind);
        let weak = w.as_weak();
        slint::Timer::single_shot(Duration::from_millis(1200), move || {
            if let Some(w) = weak.upgrade() {
                if w.get_copied_row() == row && w.get_copied_kind() == kind {
                    w.set_copied_row(-2);
                }
            }
        });
    };
    {
        let st = Rc::downgrade(&state);
        w.on_copy_file(move |i, kind| {
            let Some(st) = st.upgrade() else { return };
            let st = st.borrow();
            if let Some((link, styled)) = st.links.get(i.max(0) as usize) {
                crate::copy_to_clipboard(if kind == 1 && !styled.is_empty() { styled } else { link });
                copied(&st.window, i, kind);
            }
        });
    }
    {
        let st = Rc::downgrade(&state);
        w.on_copy_page(move |kind| {
            let Some(st) = st.upgrade() else { return };
            let st = st.borrow();
            crate::copy_to_clipboard(if kind == 1 && !st.page.1.is_empty() { &st.page.1 } else { &st.page.0 });
            copied(&st.window, -1, kind);
        });
    }
    {
        let (shared, st) = (shared.clone(), Rc::downgrade(&state));
        w.on_download_all(move || {
            let Some(st) = st.upgrade() else { return };
            download_all(&shared, &st);
        });
    }
    {
        let (shared, st) = (shared.clone(), Rc::downgrade(&state));
        w.on_download_zip(move || {
            let Some(st) = st.upgrade() else { return };
            let Some(id) = st.borrow().batch_id.clone() else { return };
            let w = st.borrow().window.clone_strong();
            w.set_zip_busy(true);
            w.set_zip_label("Creating ZIP...".into());
            let (s2, weak) = (shared.clone(), w.as_weak());
            shared.rt.spawn(async move {
                let r = s2.client.batch_zip(&id).await;
                let s3 = s2.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = weak.upgrade() else { return };
                    w.set_zip_busy(false);
                    match r {
                        Ok(link) => {
                            w.set_zip_label("Download ZIP".into());
                            let name = link.rsplit('/').next().unwrap_or("batch.zip").to_owned();
                            save_as(&s3, &Content { url: link.clone(), name, kind: ContentKind::File, protected: false, page: link });
                        }
                        Err(e) => {
                            w.set_zip_label("Failed".into());
                            w.set_note(format!("Could not create the ZIP: {e}").into());
                            let weak = w.as_weak();
                            slint::Timer::single_shot(Duration::from_secs(3), move || {
                                if let Some(w) = weak.upgrade() {
                                    w.set_zip_label("Download ZIP".into());
                                }
                            });
                        }
                    }
                });
            });
        });
    }
    let mut hooks = windows::Hooks::new();
    {
        let weak = w.as_weak();
        hooks.wheel = Box::new(move |dy, smooth| {
            if let Some(w) = weak.upgrade() {
                w.set_wheel_dy(dy);
                w.set_wheel_smooth(smooth);
            }
        });
    }
    {
        let weak = w.as_weak();
        hooks.autoscroll_by = Box::new(move |dy| {
            if let Some(w) = weak.upgrade() {
                w.invoke_autoscroll_by(dy);
            }
        });
    }
    {
        let weak = w.as_weak();
        hooks.can_scroll = Box::new(move || weak.upgrade().is_some_and(|w| w.get_can_scroll()));
    }
    hooks.paste_link = Some(crate::open::paste_hook(shared, |_| {}));
    windows::attach(w.window(), hooks);
    LIST_WINDOWS.with(|l| l.borrow_mut().push(w.clone_strong()));
    {
        let (weak, keep) = (w.as_weak(), RefCell::new(Some(state)));
        w.window().on_close_requested(move || {
            keep.borrow_mut().take();
            if let Some(w) = weak.upgrade() {
                LIST_WINDOWS.with(|l| l.borrow_mut().retain(|x| !std::ptr::eq(x.window(), w.window())));
            }
            crate::windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
    windows::place(w.window());
    let _ = w.show();
}

/// "Download All": pick a folder, then download the files one after another (row progress).
fn download_all(shared: &Arc<Shared>, st: &Rc<RefCell<ListState>>) {
    let w = st.borrow().window.clone_strong();
    let contents = st.borrow().contents.clone();
    let dialog = w.window().with_winit_window_parent(rfd::AsyncFileDialog::new()).set_directory(download_dir());
    let (shared, weak) = (shared.clone(), w.as_weak());
    let _ = slint::spawn_local(async move {
        let Some(dir) = dialog.pick_folder().await else { return };
        let dir = dir.path().to_path_buf();
        let Some(w) = weak.upgrade() else { return };
        w.set_all_busy(true);
        w.set_note("".into());
        let set_row = |w: &ListWindow, i: usize, state: i32, pct: f32| {
            let m = w.get_files();
            if let Some(mut row) = m.row_data(i) {
                row.state = state;
                row.pct = pct;
                m.set_row_data(i, row);
            }
        };
        let mut failed = 0;
        for (i, c) in contents.iter().enumerate() {
            let Some(w) = weak.upgrade() else { return };
            set_row(&w, i, 1, 0.0);
            w.set_all_label(format!("Downloading {}/{}…", i + 1, contents.len()).into());
            let dest = unique_path(&dir, &c.name);
            let (s2, weak2, url) = (shared.clone(), weak.clone(), c.url.clone());
            let last = Arc::new(std::sync::Mutex::new(Instant::now()));
            let r = shared
                .rt
                .spawn(async move {
                    s2.client
                        .download_to(&url, &dest, CancellationToken::new(), move |done, total| {
                            let mut l = last.lock().unwrap();
                            if l.elapsed() < Duration::from_millis(150) {
                                return;
                            }
                            *l = Instant::now();
                            let weak = weak2.clone();
                            let pct = total.map_or(0.0, |t| done as f32 / t.max(1) as f32);
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(w) = weak.upgrade() {
                                    let m = w.get_files();
                                    if let Some(mut row) = m.row_data(i) {
                                        row.pct = pct;
                                        m.set_row_data(i, row);
                                    }
                                }
                            });
                        })
                        .await
                })
                .await;
            let Some(w) = weak.upgrade() else { return };
            let ok = matches!(r, Ok(Ok(_)));
            if !ok {
                failed += 1;
            }
            set_row(&w, i, if ok { 2 } else { 3 }, 1.0);
        }
        let Some(w) = weak.upgrade() else { return };
        w.set_all_busy(false);
        w.set_all_label("Download All".into());
        w.set_note(if failed == 0 { format!("Saved to {}", dir.display()) } else { format!("{failed} file(s) failed. The rest is in {}", dir.display()) }.into());
    });
}

/// `dir/name`, or `name (2).ext` ... when it exists.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe: String = name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let p = dir.join(&safe);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(p)
}

/// rfd dialogs parented to a Slint window.
trait DialogParent {
    fn with_winit_window_parent(&self, d: rfd::AsyncFileDialog) -> rfd::AsyncFileDialog;
}

impl DialogParent for slint::Window {
    fn with_winit_window_parent(&self, d: rfd::AsyncFileDialog) -> rfd::AsyncFileDialog {
        use slint::winit_030::WinitWindowAccessor;
        let mut d = Some(d);
        self.with_winit_window(|w| d.take().map(|d| d.set_parent(w))).flatten().or(d).unwrap_or_default()
    }
}
