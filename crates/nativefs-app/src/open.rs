//! Where a TVFS link opens: video -> player, text / Markdown -> reader, image -> viewer, batch and
//! "TVFS Users Only" shares -> their file list, audio -> the audio player, other files -> the download
//! window. Links come from the command line,
//! IPC (link router, second launch), Ctrl+Alt+V, the recent list and links inside the reader.

use std::sync::Arc;

use tvfs_core::content::{self, Content, ContentKind, Resolved};
use tvfs_core::link::parse_open_request;

use crate::{audio_ui, files_ui, image_ui, player, reader_ui, Shared};

/// Open a TVFS link natively. Returns false when `raw` is not a TVFS link.
pub fn open_link(shared: &Arc<Shared>, raw: &str) -> bool {
    let Some(req) = parse_open_request(raw) else { return false };
    let url = req.url.clone();
    let s2 = shared.clone();
    shared.rt.spawn(async move {
        let result = content::resolve(&s2.client, &url).await;
        let s3 = s2.clone();
        let _ = slint::invoke_from_event_loop(move || match result {
            Ok(Resolved::Content(c)) => open_content(&s3, c),
            Ok(Resolved::Batch(b)) => files_ui::open_batch(&s3, b),
            Ok(Resolved::Share { meta, page }) => files_ui::open_share(&s3, meta, page),
            Ok(Resolved::Browser(u)) => {
                crate::open_external(&u, &s3.config.lock().unwrap());
                crate::quit_if_idle(&s3);
            }
            Err(e) => files_ui::show_error(&s3, url.as_str(), &describe(&e)),
        });
    });
    true
}

/// A clicked link: TVFS links open in the app, everything else in the browser.
pub fn open_or_browse(shared: &Arc<Shared>, url: &str) {
    if !open_link(shared, url) {
        crate::open_external(url, &shared.config.lock().unwrap());
    }
}

pub fn open_content(shared: &Arc<Shared>, c: Content) {
    match c.kind {
        // no audio output on macOS yet: the web player in the browser does the job meanwhile
        ContentKind::Audio if cfg!(target_os = "macos") => audio_elsewhere(shared, &c),
        ContentKind::Video => player::open(shared, c),
        ContentKind::Text | ContentKind::Markdown => reader_ui::open(shared, c),
        ContentKind::Image => image_ui::open(shared, c),
        ContentKind::Audio => audio_ui::open(shared, c),
        ContentKind::File => files_ui::open_file(shared, c),
    }
}

/// "Use TVFS tools locally": open a file from this computer in its viewer; nothing goes to the server.
/// Returns a message when no viewer takes it.
pub fn open_local(shared: &Arc<Shared>, path: &std::path::Path) -> Result<(), &'static str> {
    let c = Content::local(path).ok_or("This file cannot be opened")?;
    match c.kind {
        ContentKind::Audio if cfg!(target_os = "macos") => audio_elsewhere(shared, &c),
        ContentKind::Audio => audio_ui::open_local(shared, path.to_path_buf()),
        ContentKind::File => return Err("TVFS tools open audio, video, text, Markdown and images"),
        _ => open_content(shared, c),
    }
    Ok(())
}

/// macOS (owner, 2026-11-02): audio links open the web player in the browser, local audio files the system's
/// player, and a small note says the native player comes later (once per run).
fn audio_elsewhere(shared: &Arc<Shared>, c: &Content) {
    match c.local_path() {
        Some(path) => {
            let _ = opener::open(path);
        }
        None => crate::open_external(if c.page.is_empty() { &c.url } else { &c.page }, &shared.config.lock().unwrap()),
    }
    static SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !SHOWN.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let dialog = rfd::AsyncMessageDialog::new()
            .set_title("naTiVeFS")
            .set_description("The audio player is coming to Mac soon. Until then, audio opens in your browser (local files in your Mac's own player).")
            .set_level(rfd::MessageLevel::Info);
        let _ = slint::spawn_local(async move {
            dialog.show().await;
        });
    }
}

/// Ctrl+Alt+V: open the TVFS link on the clipboard. Returns a message when there is none.
pub fn open_clipboard_link(shared: &Arc<Shared>) -> Result<(), &'static str> {
    let text = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).unwrap_or_default();
    // the clipboard may hold a styled link "[name](https://...)" or text around the link
    let link = text
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '[' | ']' | '"' | '\''))
        .find(|w| parse_open_request(w).is_some());
    match link {
        Some(l) if open_link(shared, l) => Ok(()),
        _ => Err("There is no TVFS link on the clipboard."),
    }
}

/// A short, human error text for a link that could not be loaded.
pub fn describe(e: &tvfs_core::Error) -> String {
    match e {
        tvfs_core::Error::Server { status: 404, .. } => "This file does not exist any more (it expired or was deleted).".into(),
        e if e.is_auth() => "This file is for TVFS users only — log in in naTiVeFS first.".into(),
        tvfs_core::Error::Network(_) => "Network error: could not reach the server.".into(),
        e => e.to_string(),
    }
}

/// Ctrl+Alt+V for a window's input hooks.
pub fn paste_hook(shared: &Arc<Shared>, toast: impl Fn(&str) + 'static) -> Box<dyn Fn()> {
    let shared = shared.clone();
    Box::new(move || {
        if let Err(msg) = open_clipboard_link(&shared) {
            toast(msg);
        }
    })
}
