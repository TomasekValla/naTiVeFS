//! The expiry picker of the upload page, from the account's upload rules (`GET /api/config`): options the login can
//! never use are left out, options blocked only for now (file size, free space, the host's time to midnight) are
//! greyed, and a choice that is not possible any more moves to the longest one that is. Lives on the UI thread.

use std::cell::RefCell;

use slint::{ModelRc, VecModel};
use tvfs_core::limits::{self, PillState, UploadConfig};

use crate::{ExpiryPill, MainWindow};

const LABELS: [&str; 13] = ["5m", "30m", "1h", "6h", "12h", "1d", "2d", "3d", "5d", "7d", "10d", "14d", "30d"];

thread_local! {
    static CONFIG: RefCell<Option<UploadConfig>> = const { RefCell::new(None) };
}

/// The rules of the current login (`None` after a logout: every option is shown).
pub fn set_config(ui: &MainWindow, cfg: Option<UploadConfig>) {
    CONFIG.with(|c| *c.borrow_mut() = cfg);
    apply(ui);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn states(biggest: u64) -> Vec<PillState> {
    CONFIG.with(|c| match c.borrow().as_ref() {
        Some(cfg) => limits::pill_states(cfg, biggest, now_ms()),
        None => vec![PillState::Normal; LABELS.len()],
    })
}

/// Recompute the options (after a login, a change of the selected files, and on every status refresh, so the greyed
/// options and the "Files expire on" date follow the clock).
pub fn apply(ui: &MainWindow) {
    let st = states(crate::upload_ui::biggest_selected());
    let shown: Vec<ExpiryPill> = st
        .iter()
        .enumerate()
        .filter(|(_, s)| **s != PillState::Hidden)
        .map(|(i, s)| ExpiryPill { label: LABELS[i].into(), idx: i as i32, grey: *s == PillState::Grey })
        .collect();
    // first row: up to 1 day (as on the web), second row: the rest
    let (a, b): (Vec<ExpiryPill>, Vec<ExpiryPill>) = shown.into_iter().partition(|p| p.idx <= 5);
    ui.set_pills_a(ModelRc::new(VecModel::from(a)));
    ui.set_pills_b(ModelRc::new(VecModel::from(b)));
    let current = ui.get_expiry().max(0) as usize;
    let usable = limits::usable_index(&st, current);
    if usable != current {
        ui.set_expiry(usable as i32);
    }
    ui.set_expiry_date_text(crate::expiry_date_text(usable as i32).into());
}

/// A click on an option: greyed and hidden ones do nothing.
pub fn pick(ui: &MainWindow, i: i32) {
    let st = states(crate::upload_ui::biggest_selected());
    if st.get(i.max(0) as usize) == Some(&PillState::Normal) {
        ui.set_expiry(i);
        ui.set_expiry_date_text(crate::expiry_date_text(i).into());
    }
}
