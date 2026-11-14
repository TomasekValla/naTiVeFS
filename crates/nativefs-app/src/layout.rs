//! The main window's desktop layout (the web's "Desktop layout"): below 900 logical px one column as on a phone;
//! 900+ the upload card in the middle with Recently uploaded, File Check and the links on the right; 1200+ also
//! Settings on the left; 1800+ wider side columns. Column widths follow the web's grid tracks (the middle one stays
//! 440-480 px at every width), the gap is the web's `clamp(24px, 9vw - 84px, 128px)`.
//!
//! Computed here once per resize and handed to Slint as plain values instead of bindings on the window width:
//! Slint marks everything that depends on a property dirty when it changes, even when the result stays the same,
//! so with bindings every resize step re-wrapped the text of every card (measured: ~8 ms per step). A value set
//! from Rust that did not change invalidates nothing, so a resize that only moves the cards stays cheap.

use crate::MainWindow;

/// Window widths (logical px) where the layout switches.
pub const TWO_COLUMNS: f32 = 900.0;
pub const THREE_COLUMNS: f32 = 1200.0;
pub const WIDE: f32 = 1800.0;
/// Space left and right of the cards (each side).
const MARGIN: f32 = 20.0;
/// The phone column (the web's 440 px).
const PHONE_COL: f32 = 440.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Geometry {
    /// 0 = one column, 1 = two columns, 2 = three columns.
    pub lay: i32,
    pub big: bool,
    pub left_x: f32,
    pub left_w: f32,
    pub mid_x: f32,
    pub mid_w: f32,
    pub side_x: f32,
    pub side_w: f32,
    /// The settings card: a page of its own below 1200 px (560 px wide like the web's settings view at 900+).
    pub set_x: f32,
    pub set_w: f32,
}

pub fn compute(width: f32) -> Geometry {
    let lay = if width >= THREE_COLUMNS {
        2
    } else if width >= TWO_COLUMNS {
        1
    } else {
        0
    };
    let big = width >= WIDE;
    let inner = width - 2.0 * MARGIN;
    let col_w = inner.min(PHONE_COL);
    // the columns' minimum widths and the number of gaps between them
    let (mins, gaps) = match (lay, big) {
        (2, true) => (1120.0, 2.0),
        (2, false) => (1080.0, 2.0),
        _ => (760.0, 1.0),
    };
    let gap = (width * 0.09 - 84.0).clamp(24.0, 128.0).min((inner - mins) / gaps).max(18.0);
    // room left after the gaps and the minimum widths, shared out like CSS grid minmax() tracks (equal shares, a
    // track stops at its maximum: the middle one first, then the left, then the right)
    let free = (inner - mins - gap * gaps).max(0.0);
    // (whole px: a column then changes width every few px of a resize instead of on every step, and each change
    // re-wraps its text)
    let e_mid = (free / if lay == 2 { 3.0 } else { 2.0 }).min(40.0).floor();
    let e_left = ((free - e_mid) / 2.0).min(if big { 90.0 } else { 60.0 }).floor();
    let e_side = (free - e_mid - if lay == 2 { e_left } else { 0.0 })
        .min(match (lay, big) {
            (2, true) => 110.0,
            _ => 80.0,
        })
        .floor();
    let left_w = if big { 340.0 } else { 320.0 } + e_left;
    let mid_w = if lay == 0 { col_w } else { PHONE_COL + e_mid };
    let side_w = if lay == 2 && big { 340.0 } else { 320.0 } + e_side;
    let group_w = mid_w + side_w + gap + if lay == 2 { left_w + gap } else { 0.0 };
    let left_x = (width - group_w) / 2.0;
    let mid_x = if lay == 0 { (width - col_w) / 2.0 } else { left_x + if lay == 2 { left_w + gap } else { 0.0 } };
    let side_x = mid_x + mid_w + gap;
    let set_w = match lay {
        2 => left_w,
        1 => inner.min(560.0),
        _ => col_w,
    };
    let set_x = if lay == 2 { left_x } else { (width - set_w) / 2.0 };
    Geometry { lay, big, left_x, left_w, mid_x, mid_w, side_x, side_w, set_x, set_w }
}

/// Lay the main window out for a logical width (on every resize, zoom change and at start).
pub fn apply(ui: &MainWindow, width: f32) {
    if width < 1.0 {
        return;
    }
    let g = compute(width);
    // switch first: Slint starts the glide and fixes the page before the new places arrive
    ui.invoke_set_layout(g.lay, g.big);
    ui.set_left_x(g.left_x);
    ui.set_left_w(g.left_w);
    ui.set_mid_x(g.mid_x);
    ui.set_mid_w(g.mid_w);
    ui.set_side_x(g.side_x);
    ui.set_side_w(g.side_w);
    ui.set_set_x(g.set_x);
    ui.set_set_w(g.set_w);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoints() {
        assert_eq!(compute(899.0).lay, 0);
        assert_eq!(compute(900.0).lay, 1);
        assert_eq!(compute(1199.0).lay, 1);
        assert_eq!(compute(1200.0).lay, 2);
        assert!(!compute(1799.0).big && compute(1800.0).big);
    }

    #[test]
    fn phone_column_is_centred() {
        let g = compute(520.0);
        assert_eq!(g.mid_w, 440.0);
        assert_eq!(g.mid_x, 40.0);
        // narrower than the column + margins: the column shrinks
        assert_eq!(compute(470.0).mid_w, 430.0);
    }

    #[test]
    fn columns_stay_inside_the_window_and_do_not_overlap() {
        let mut w = TWO_COLUMNS;
        while w <= 2600.0 {
            let g = compute(w);
            let right = g.side_x + g.side_w;
            assert!(g.mid_x + g.mid_w < g.side_x, "{w}");
            assert!(right <= w - MARGIN + 0.01, "{w}: {right}");
            if g.lay == 2 {
                assert!(g.left_x >= MARGIN - 0.01, "{w}");
                assert!(g.left_x + g.left_w < g.mid_x, "{w}");
                assert_eq!((g.set_x, g.set_w), (g.left_x, g.left_w));
            }
            assert!((440.0..=480.0).contains(&g.mid_w), "{w}");
            w += 7.0;
        }
    }

    /// Once the tracks reach their maximum a wider window only moves the cards: the widths stay bit for bit the same
    /// (this is what keeps resizing cheap).
    #[test]
    fn widths_saturate() {
        let a = compute(1500.0);
        let b = compute(1700.0);
        assert_eq!((a.left_w, a.mid_w, a.side_w), (b.left_w, b.mid_w, b.side_w));
        assert_eq!((a.left_w, a.mid_w, a.side_w), (380.0, 480.0, 400.0));
        let c = compute(2000.0);
        let d = compute(2400.0);
        assert_eq!((c.left_w, c.mid_w, c.side_w), (430.0, 480.0, 450.0));
        assert_eq!((c.mid_w, c.side_w), (d.mid_w, d.side_w));
        assert_eq!(compute(1000.0).mid_w, compute(1150.0).mid_w);
    }
}
