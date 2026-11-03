//! The File Check page (page 2, the web's `check.html`): paste a files.tomasekvalla.cz link,
//! `POST /api/probe`, show the container, the streams and the raw ffprobe JSON.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use slint::{ComponentHandle, ModelRc, VecModel};
use tvfs_core::probe::{self, ProbeView, Stat, StreamKind};
use tvfs_core::Error;

use crate::{on_ui, Shared};
use crate::{CheckStat, CheckStatPair, CheckStream, MainWindow};

/// Links the paste shortcut accepts (check.html: `text.startsWith('https://files.tomasekvalla.cz/')`).
const LINK_PREFIX: &str = "https://files.tomasekvalla.cz/";

/// Bumped every time the page opens; a probe that started before is ignored when it returns.
static VISIT: AtomicU64 = AtomicU64::new(0);

/// The page was opened: empty field, no result, no error (the web reloads check.html).
pub fn opened(ui: &MainWindow) {
    VISIT.fetch_add(1, Ordering::Relaxed);
    ui.set_check_busy(false);
    ui.set_check_error("".into());
    ui.set_check_has_result(false);
    ui.set_check_raw_open(false);
    ui.set_raw_scroll(0.0);
    ui.set_check_url("".into());
    // debug builds: `NATIVEFS_CHECK_URL=<link>` pre-fills the field (synthetic typing does not reach winit windows)
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var("NATIVEFS_CHECK_URL") {
        ui.set_check_url(url.into());
    }
}

pub fn probe_clicked(ui: &MainWindow, shared: &Arc<Shared>, url: &str) {
    let url = url.trim().to_string();
    // the web disables the button while probing; Enter in the field must not start a second probe
    if url.is_empty() || ui.get_check_busy() {
        return;
    }
    ui.set_check_error("".into());
    ui.set_check_busy(true);
    ui.set_check_has_result(false);
    ui.set_check_raw_open(false);
    ui.set_raw_scroll(0.0);
    let (weak, client) = (ui.as_weak(), shared.client.clone());
    let visit = VISIT.load(Ordering::Relaxed);
    shared.rt.spawn(async move {
        let result = match client.probe(&url).await {
            Ok(body) => probe::parse(&body).ok_or_else(|| "Network error: the server sent an invalid response".to_string()),
            Err(e) => Err(error_text(&e)),
        };
        on_ui(&weak, move |ui| {
            if VISIT.load(Ordering::Relaxed) != visit {
                return;
            }
            ui.set_check_busy(false);
            match result {
                Ok(view) => show(ui, view),
                Err(msg) => ui.set_check_error(msg.into()),
            }
        });
    });
}

/// check.html: `data.error || 'Error ' + status`, or `'Network error: ' + message`.
fn error_text(e: &Error) -> String {
    match e {
        Error::Server { status, message } if message.is_empty() => format!("Error {status}"),
        Error::Server { message, .. } => message.clone(),
        Error::Network(n) if n.is_timeout() => "Network error: the request timed out".into(),
        Error::Network(_) => "Network error: could not reach the server".into(),
        other => format!("Network error: {other}"),
    }
}

fn show(ui: &MainWindow, view: ProbeView) {
    ui.set_check_has_format(view.format.is_some());
    ui.set_check_format(pairs(view.format.unwrap_or_default()));
    let streams: Vec<CheckStream> = view
        .streams
        .into_iter()
        .map(|s| CheckStream {
            kind: match s.kind {
                StreamKind::Video => 0,
                StreamKind::Audio => 1,
                StreamKind::Other => 2,
            },
            badge: s.badge.into(),
            codec: s.codec.into(),
            codec_long: s.codec_long.into(),
            rows: pairs(s.stats),
        })
        .collect();
    ui.set_check_streams(ModelRc::new(VecModel::from(streams)));
    ui.set_check_raw(view.raw.into());
    ui.set_check_has_result(true);
}

/// Two stats per grid row.
fn pairs(stats: Vec<Stat>) -> ModelRc<CheckStatPair> {
    // `.stat-label { text-transform: uppercase }`
    let cell = |s: &Stat| CheckStat { label: s.label.to_uppercase().into(), value: s.value.as_str().into(), accent: s.accent };
    let rows: Vec<CheckStatPair> = stats
        .chunks(2)
        .map(|c| CheckStatPair { a: cell(&c[0]), b: c.get(1).map(cell).unwrap_or_default(), has_b: c.len() == 2 })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// Ctrl+V outside the URL field: a TVFS link from the clipboard fills it (it does not start the probe).
pub fn paste(ui: &MainWindow) {
    let Ok(mut cb) = arboard::Clipboard::new() else { return };
    let Ok(text) = cb.get_text() else { return };
    let text = text.trim();
    if text.starts_with(LINK_PREFIX) {
        ui.set_check_url(text.into());
    }
}
