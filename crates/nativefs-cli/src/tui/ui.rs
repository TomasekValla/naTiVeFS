//! Drawing. Every screen is a function of `App`; clickable areas are recorded in `app.hits`.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use tvfs_core::fmt::{fmt_bytes, fmt_speed_mbs, fmt_time};
use tvfs_core::upload::{Phase, RunStatus};

use super::app::*;
use super::picker::Kind;
use crate::expiry;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Block letters for the big shadowed logo: solid blocks get the TVFS gradient, the box-drawing edges are the drop
/// shadow. Lower case letters start one row lower, so "naTiVeFS" keeps its casing. Every glyph is 7 rows of equal width.
fn glyph(c: char) -> [&'static str; 7] {
    match c {
        'n' => ["        ", "██████╗ ", "██╔══██╗", "██║  ██║", "██║  ██║", "██║  ██║", "╚═╝  ╚═╝"],
        'a' => ["        ", "██████╗ ", "╚════██╗", " ██████║", "██╔══██║", "╚██████║", " ╚═════╝"],
        'T' => ["████████╗", "╚══██╔══╝", "   ██║   ", "   ██║   ", "   ██║   ", "   ██║   ", "   ╚═╝   "],
        'i' => ["██╗", "╚═╝", "██╗", "██║", "██║", "██║", "╚═╝"],
        'V' => ["██╗   ██╗", "██║   ██║", "██║   ██║", "██║   ██║", "╚██╗ ██╔╝", " ╚████╔╝ ", "  ╚═══╝  "],
        'e' => ["        ", " █████╗ ", "██╔══██╗", "███████║", "██╔════╝", "╚██████╗", " ╚═════╝"],
        'F' => ["███████╗", "██╔════╝", "█████╗  ", "██╔══╝  ", "██║     ", "██║     ", "╚═╝     "],
        'S' => ["███████╗", "██╔════╝", "███████╗", "╚════██║", "     ██║", "███████║", "╚══════╝"],
        '-' => ["      ", "      ", "      ", "█████╗", "╚════╝", "      ", "      "],
        'C' => [" ██████╗", "██╔════╝", "██║     ", "██║     ", "██║     ", "╚██████╗", " ╚═════╝"],
        'L' => ["██╗     ", "██║     ", "██║     ", "██║     ", "██║     ", "███████╗", "╚══════╝"],
        'I' => ["██╗", "██║", "██║", "██║", "██║", "██║", "╚═╝"],
        _ => [" "; 7],
    }
}

/// The seven rows of `word` in block letters.
fn big_rows(word: &str) -> [String; 7] {
    std::array::from_fn(|r| word.chars().map(|c| glyph(c)[r]).collect())
}

/// The big logo shrinks with the terminal: the full name, then "naTiVeFS", then "TVFS"; narrower than that, one line.
const BIG_WORDS: [&str; 3] = ["naTiVeFS-CLI", "naTiVeFS", "TVFS"];
const BIG_MIN_WIDTH: u16 = 4 + 34; // indent + "TVFS"

/// Cut a string to a display width, with an ellipsis.
pub fn fit(s: &str, w: usize) -> String {
    let width = |t: &str| Line::raw(t.to_owned()).width();
    if width(s) <= w {
        return s.to_owned();
    }
    let mut out = String::new();
    for c in s.chars() {
        if width(&out) + width(&c.to_string()) + 1 > w {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn width(s: &str) -> usize {
    Line::raw(s.to_owned()).width()
}

fn pad_right(s: &str, w: usize) -> String {
    let n = width(s);
    format!("{s}{}", " ".repeat(w.saturating_sub(n)))
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

pub fn draw(app: &mut App, f: &mut Frame) {
    app.hits.clear();
    app.marquee_active = false;
    let area = f.area();
    f.render_widget(Block::new().style(Style::new().bg(app.theme.bg).fg(app.theme.text)), area);
    // below this nothing useful fits; anything larger just lays out at whatever size the terminal has
    if area.width < 16 || area.height < 8 {
        let p = Paragraph::new("Window too small.\nMake the terminal bigger.").alignment(Alignment::Center);
        f.render_widget(p, centered(area, area.width, 2));
        return;
    }
    let tall = area.height >= 26;
    // the big shadowed logo when there is room for it (and the screens below it), else one line
    let head_h = if tall && area.width >= BIG_MIN_WIDTH { 8 } else { 1 };
    let [head, status, body, foot] = Layout::vertical([Constraint::Length(head_h), Constraint::Length(1), Constraint::Min(5), Constraint::Length(1)]).areas(area);
    draw_header(app, f, head);
    draw_status(app, f, status);
    let body = Rect::new(body.x + 1, body.y, body.width.saturating_sub(2), body.height);
    let body = centered(body, 110, body.height);
    // "‹ Back" row on top of the screens that have a way back (Up from the first item reaches it)
    let body = if app.screen.has_back() && body.height > 6 {
        draw_back(app, f, Rect::new(body.x, body.y, body.width, 1));
        Rect::new(body.x, body.y + 1, body.width, body.height - 1)
    } else {
        body
    };
    match app.screen {
        Screen::Login => draw_login(app, f, body),
        Screen::Menu => draw_menu(app, f, body),
        Screen::Picker => draw_picker(app, f, body),
        Screen::Options => draw_options(app, f, body),
        Screen::Uploading => draw_uploading(app, f, body),
        Screen::Result => draw_result(app, f, body),
        Screen::Recent => draw_recent(app, f, body),
        Screen::Settings => draw_settings(app, f, body),
        Screen::SystemPick => draw_system_pick(app, f, body),
    }
    draw_footer(app, f, foot);
    if let Some((text, _, bad)) = &app.toast {
        let w = (width(text) as u16 + 4).min(area.width);
        let r = Rect::new(area.x + (area.width - w) / 2, foot.y.saturating_sub(2), w, 1);
        f.render_widget(Clear, r);
        let color = if *bad { app.theme.bad } else { app.theme.good };
        f.render_widget(Paragraph::new(format!(" {} ", fit(text, (w as usize).saturating_sub(2)))).alignment(Alignment::Center).style(Style::new().fg(app.theme.bg).bg(color).add_modifier(Modifier::BOLD)), r);
    }
}

fn draw_back(app: &mut App, f: &mut Frame, r: Rect) {
    let focus = app.back_focus == Some(app.screen);
    let style = if focus { Style::new().bg(app.theme.sel_bg).fg(app.theme.text).add_modifier(Modifier::BOLD) } else { Style::new().fg(app.theme.dim) };
    f.render_widget(Paragraph::new(Line::styled(pad_right("  ‹ Back", r.width as usize), style)), r);
    app.hits.push((r, Hit::Back));
}

fn draw_header(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let version = Line::styled(format!("v{}  ·  TVFS", env!("CARGO_PKG_VERSION")), Style::new().fg(t.dim));
    if r.height >= 8 {
        // one empty row above, so the logo never touches the terminal's top edge
        let r = Rect::new(r.x, r.y + 1, r.width, r.height - 1);
        let fits = |w: &str| big_rows(w)[0].chars().count() as u16 + 4 <= r.width;
        let word = BIG_WORDS.iter().copied().find(|w| fits(w)).unwrap_or("TVFS");
        let rows = big_rows(word);
        let logo_w = rows[0].chars().count();
        let total = logo_w.max(1) as f32;
        let lines: Vec<Line> = rows
            .iter()
            .map(|row| {
                let mut v = vec![Span::raw("  ")];
                v.extend(row.chars().enumerate().map(|(i, c)| {
                    let x = i as f32 / total;
                    let style = if c == '█' {
                        Style::new().fg(t.grad(x)).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(t.grad_shade(x, if t.light { 0.45 } else { 0.38 }))
                    };
                    Span::styled(c.to_string(), style)
                }));
                Line::from(v)
            })
            .collect();
        f.render_widget(Paragraph::new(lines), r);
        let mut tag = vec![Line::raw(""); 6];
        // the version sits right of the logo's bottom row, only when it does not touch it
        if (r.width as usize) >= 2 + logo_w + 3 + version.width() + 2 {
            tag.push(version);
        }
        f.render_widget(Paragraph::new(tag).alignment(Alignment::Right), Rect::new(r.x, r.y, r.width.saturating_sub(2), r.height));
    } else {
        let name = "naTiVeFS-CLI";
        let total = name.chars().count() as f32;
        let mut spans = vec![Span::raw("  ")];
        for (i, c) in name.chars().enumerate() {
            spans.push(Span::styled(c.to_string(), Style::new().fg(t.grad(i as f32 / total)).add_modifier(Modifier::BOLD)));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), r);
        if (r.width as usize) >= 2 + name.chars().count() + 3 + version.width() + 2 {
            f.render_widget(Paragraph::new(version).alignment(Alignment::Right), Rect::new(r.x, r.y, r.width.saturating_sub(2), 1));
        }
    }
}

fn draw_status(app: &App, f: &mut Frame, r: Rect) {
    let t = &app.theme;
    let s = &app.status;
    let mut spans = vec![Span::raw("  ")];
    if app.user.is_none() {
        spans.push(Span::styled("not logged in", Style::new().fg(t.dim)));
    } else if !s.checked {
        spans.push(Span::styled("connecting…", Style::new().fg(t.dim)));
    } else if s.online {
        spans.push(Span::styled("● online", Style::new().fg(t.good)));
        if let Some(ms) = s.latency_ms {
            spans.push(Span::styled(format!("  {ms} ms"), Style::new().fg(t.dim)));
        }
        if !s.used.is_empty() {
            // only the used space, like the app and the web: the storage has no fixed size to show "of N GB" against
            spans.push(Span::styled(format!("  ·  used {}", s.used), Style::new().fg(t.dim)));
        }
    } else {
        spans.push(Span::styled("● offline", Style::new().fg(t.bad)));
    }
    if let Some(u) = &app.user {
        spans.push(Span::styled(format!("  ·  {u}"), Style::new().fg(t.accent)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn draw_footer(app: &App, f: &mut Frame, r: Rect) {
    let hints: &[(&str, &str)] = match app.screen {
        Screen::Login => &[("Tab", "switch field"), ("Enter", "log in"), ("Esc", "quit")],
        Screen::Menu => &[("↑↓", "move"), ("Enter", "open"), ("u r s l q", "shortcuts")],
        Screen::Picker => &[("type", "filter"), ("Space", "select"), ("Enter", "open"), ("Tab", "continue"), ("^O", "system dialog"), ("Esc/←", "back")],
        Screen::Options => &[("←→", "change"), ("↑↓", "row"), ("Enter", "start upload"), ("Esc/⌫", "back")],
        Screen::Uploading => &[("p", "pause"), ("c", "cancel")],
        Screen::Result => &[("↑↓", "move"), ("Enter", "copy"), ("o", "open"), ("n", "new upload"), ("m/←", "menu")],
        Screen::Recent => &[("↑↓", "move"), ("Enter", "copy"), ("l/s", "plain/styled"), ("o", "open"), ("r", "refresh"), ("d", "delete"), ("Esc/←", "back")],
        Screen::Settings => &[("↑↓", "move"), ("Enter/←→", "change"), ("Esc/⌫", "back")],
        Screen::SystemPick => &[("Enter", "choose files"), ("f", "choose a folder"), ("t", "terminal browser"), ("Esc/←", "back")],
    };
    let t = &app.theme;
    if let (Screen::Recent, Some((_, name))) = (app.screen, &app.recent.pending_delete) {
        let q = format!(" Delete {}? ", fit(name, (r.width as usize).saturating_sub(14)));
        let spans = vec![
            Span::styled(q, Style::new().fg(t.bad).add_modifier(Modifier::BOLD)),
            Span::styled("y", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" yes   ", Style::new().fg(t.dim)),
            Span::styled("n", Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(" no", Style::new().fg(t.dim)),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), r);
        return;
    }
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in hints {
        spans.push(Span::styled(*k, Style::new().fg(t.accent).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {d}   "), Style::new().fg(t.dim)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), r);
}

fn panel<'a>(app: &App, title: &'a str) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(app.theme.border))
        .title(Span::styled(format!(" {title} "), Style::new().fg(app.theme.accent).add_modifier(Modifier::BOLD)))
}

fn spinner(app: &App) -> &'static str {
    SPINNER[(app.frame as usize) % SPINNER.len()]
}

// ── screens ─────────────────────────────────────────────────────────────

fn draw_login(app: &mut App, f: &mut Frame, body: Rect) {
    let r = centered(body, 56, 12.min(body.height));
    let t = app.theme.clone();
    f.render_widget(panel(app, "Log in to TVFS"), r);
    let inner = Rect::new(r.x + 3, r.y + 2, r.width.saturating_sub(6), r.height.saturating_sub(3));
    let field = |label: &str, value: String, active: bool| -> Vec<Line<'static>> {
        let style = if active { Style::new().fg(t.accent2) } else { Style::new().fg(t.dim) };
        let cursor = if active { "▏" } else { "" };
        vec![
            Line::styled(label.to_owned(), style),
            Line::from(vec![Span::styled(if active { "› " } else { "  " }, style), Span::styled(format!("{value}{cursor}"), Style::new().fg(t.text))]),
        ]
    };
    let mut lines = field("Username", app.login.user.clone(), app.login.field == 0);
    lines.push(Line::raw(""));
    lines.extend(field("Password", "•".repeat(app.login.pass.chars().count()), app.login.field == 1));
    lines.push(Line::raw(""));
    if app.login.busy {
        lines.push(Line::styled(format!("{} Logging in…", spinner(app)), Style::new().fg(t.accent)));
    } else if let Some(e) = &app.login.error {
        lines.push(Line::styled(fit(e, inner.width as usize), Style::new().fg(t.bad)));
    } else {
        lines.push(Line::styled("The login is shared with the naTiVeFS app.", Style::new().fg(t.dim)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_menu(app: &mut App, f: &mut Frame, body: Rect) {
    let r = centered(body, 44, (MENU.len() as u16 * 2 + 3).min(body.height));
    f.render_widget(panel(app, "Menu"), r);
    for (i, (label, key)) in MENU.iter().enumerate() {
        let y = r.y + 2 + i as u16 * 2;
        if y >= r.y + r.height - 1 {
            break;
        }
        let row = Rect::new(r.x + 2, y, r.width.saturating_sub(4), 1);
        let sel = app.menu == i;
        let style = if sel { Style::new().bg(app.theme.sel_bg).fg(app.theme.text).add_modifier(Modifier::BOLD) } else { Style::new().fg(app.theme.text) };
        let line = Line::from(vec![
            Span::styled(if sel { " ▸ " } else { "   " }, Style::new().fg(app.theme.accent2)),
            Span::styled(pad_right(label, (row.width as usize).saturating_sub(6)), style),
            Span::styled(format!("{key} "), Style::new().fg(app.theme.dim)),
        ]);
        f.render_widget(Paragraph::new(line).style(if sel { Style::new().bg(app.theme.sel_bg) } else { Style::new() }), row);
        app.hits.push((row, Hit::Menu(i)));
    }
}

fn draw_picker(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    let title = match &app.picker.dir {
        Some(d) => d.display().to_string().trim_start_matches(r"\\?\").to_owned(),
        None => "Drives".into(),
    };
    let [list_area, info] = Layout::vertical([Constraint::Min(5), Constraint::Length(3)]).areas(body);
    f.render_widget(panel(app, &fit(&title, list_area.width.saturating_sub(6) as usize)), list_area);
    let inner = Rect::new(list_area.x + 2, list_area.y + 1, list_area.width.saturating_sub(4), list_area.height.saturating_sub(2));
    let h = inner.height as usize;
    app.picker.keep_visible(h);
    let vis = app.picker.visible();
    let scroll = app.picker.scroll;
    for (row, vi) in vis.iter().enumerate().skip(scroll).take(h) {
        let e = &app.picker.entries[*vi];
        let y = inner.y + (row - scroll) as u16;
        let rect = Rect::new(inner.x, y, inner.width, 1);
        let cur = row == app.picker.cursor;
        let sel = app.picker.selected.contains(&e.path);
        let mark = if sel { "[x] " } else if matches!(e.kind, Kind::File | Kind::Dir) { "[ ] " } else { "    " };
        let (icon, color) = match e.kind {
            Kind::Parent => ("↰ ", t.dim),
            Kind::Drive => ("▣ ", t.accent),
            Kind::Dir => ("▸ ", t.accent),
            Kind::File => ("  ", t.text),
        };
        let size = if e.kind == Kind::File { fmt_bytes(e.size) } else { String::new() };
        let name_w = (inner.width as usize).saturating_sub(4 + 2 + 12);
        let line = Line::from(vec![
            Span::styled(mark, Style::new().fg(if sel { t.good } else { t.dim })),
            Span::styled(icon, Style::new().fg(color)),
            Span::styled(pad_right(&fit(&e.name, name_w), name_w), Style::new().fg(color).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() })),
            Span::styled(format!("{size:>12}"), Style::new().fg(t.dim)),
        ]);
        f.render_widget(Paragraph::new(line).style(if cur { Style::new().bg(t.sel_bg) } else { Style::new() }), rect);
        app.hits.push((rect, Hit::PickerRow(row)));
    }
    if vis.is_empty() || app.picker.error.is_some() {
        let msg = app.picker.error.clone().unwrap_or_else(|| "Nothing here.".into());
        f.render_widget(Paragraph::new(Span::styled(msg, Style::new().fg(t.warn))), inner);
    }
    let (files, bytes, folders) = app.picker.selection_summary();
    let mut sel = format!("{files} file(s), {}", fmt_bytes(bytes));
    if folders > 0 {
        sel.push_str(&format!(" + {folders} folder(s)"));
    }
    let filter = if app.picker.filter.is_empty() { Span::styled("type to filter", Style::new().fg(t.dim)) } else { Span::styled(format!("filter: {}▏", app.picker.filter), Style::new().fg(t.accent2)) };
    f.render_widget(
        Paragraph::new(vec![Line::from(vec![Span::styled("Selected: ", Style::new().fg(t.dim)), Span::styled(sel, Style::new().fg(t.text).add_modifier(Modifier::BOLD))]), Line::from(filter)]).block(Block::new().padding(ratatui::widgets::Padding::horizontal(2))),
        Rect::new(info.x, info.y + 1, info.width, 2),
    );
}

fn draw_options(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    let r = centered(body, 90, body.height.min(24));
    f.render_widget(panel(app, "Upload options"), r);
    let inner = Rect::new(r.x + 3, r.y + 2, r.width.saturating_sub(6), r.height.saturating_sub(3));
    let w = inner.width as usize;
    let n_files = app.opts.files.len();
    let total: u64 = app.opts.files.iter().map(|x| x.size).sum();
    let audio = app.opts.has_audio();
    let users_only = app.opts.users_only;
    let too_many = users_only && n_files > tvfs_core::upload::PROTECTED_MAX_FILES;
    // the 12 expiry chips take one row when they fit, else two
    let chip_total: usize = expiry::LABELS.iter().map(|l| l.chars().count() + 3).sum();
    let chip_rows = if chip_total <= w { 1 } else { 2 };
    // everything below the file list is fixed: label, chips, blank, toggles, warning(s), Enter row
    let fixed = 1 + 1 + 1 + chip_rows + 1 + app.opts.rows() + 1 + usize::from(too_many) + 1;
    let room = (inner.height as usize).saturating_sub(fixed).min(5);
    let mut lines = vec![Line::from(vec![Span::styled(format!("{n_files} file(s), {}", fmt_bytes(total)), Style::new().fg(t.text).add_modifier(Modifier::BOLD))])];
    let shown = if n_files <= room { n_files } else { room.saturating_sub(1) };
    for fl in app.opts.files.iter().take(shown) {
        lines.push(Line::styled(format!("  {}", fit(&fl.share_path(), w.saturating_sub(4))), Style::new().fg(t.dim)));
    }
    if n_files > shown && room > 0 {
        lines.push(Line::styled(format!("  … and {} more", n_files - shown), Style::new().fg(t.dim)));
    }
    lines.push(Line::raw(""));
    let label_style = |row: usize| if app.opts.row == row { Style::new().fg(t.accent2).add_modifier(Modifier::BOLD) } else { Style::new().fg(t.dim) };
    lines.push(Line::styled("Link expires after", label_style(0)));
    let per_row = expiry::LABELS.len().div_ceil(chip_rows);
    let mut hits: Vec<(Rect, Hit)> = Vec::new();
    for (row_i, chunk) in expiry::LABELS.chunks(per_row).enumerate() {
        let y = inner.y + lines.len() as u16;
        let mut spans = vec![];
        let mut x = inner.x;
        for (j, l) in chunk.iter().enumerate() {
            let i = row_i * per_row + j;
            let text = format!(" {l} ");
            let cw = text.chars().count() as u16;
            let sel = app.opts.expiry == i;
            spans.push(Span::styled(text, if sel { Style::new().bg(t.accent).fg(t.bg).add_modifier(Modifier::BOLD) } else { Style::new().fg(t.text) }));
            spans.push(Span::raw(" "));
            if x + cw <= inner.x + inner.width {
                hits.push((Rect::new(x, y, cw, 1), Hit::Expiry(i)));
            }
            x += cw + 1;
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    let toggle = |on: bool| if on { Span::styled("[x] ", Style::new().fg(t.good)) } else { Span::styled("[ ] ", Style::new().fg(t.dim)) };
    let y_opt = inner.y + lines.len() as u16;
    lines.push(Line::from(vec![toggle(app.opts.anonymize), Span::styled("Anonymize file names (random name, same extension)", label_style(1))]));
    lines.push(Line::from(vec![toggle(users_only), Span::styled("TVFS Users Only (only logged-in users can open the link)", label_style(2))]));
    if audio {
        let mut v = vec![toggle(app.opts.voice_note && !users_only), Span::styled("Upload as voice note", label_style(3))];
        if users_only {
            v.push(Span::styled("  (public uploads only)", Style::new().fg(t.dim)));
        }
        lines.push(Line::from(v));
    }
    for i in 0..app.opts.rows() - 1 {
        hits.push((Rect::new(inner.x, y_opt + i as u16, inner.width, 1), Hit::OptRow(i + 1)));
    }
    // the warnings come before the Enter row, so they are never the part that gets cut off on a small terminal
    if too_many {
        lines.push(Line::styled(fit(&tvfs_core::upload::protected_limit_message(n_files), w), Style::new().fg(t.bad)));
    }
    if !users_only {
        lines.push(Line::styled(fit("The link will be public: anyone with it can open the file.", w), Style::new().fg(t.warn)));
    } else {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(vec![Span::styled(" Enter ", Style::new().bg(t.accent2).fg(t.bg).add_modifier(Modifier::BOLD)), Span::styled("  start the upload", Style::new().fg(t.text))]));
    // clickable areas only where something is drawn
    let bottom = inner.y + inner.height;
    app.hits.extend(hits.into_iter().filter(|(r, _)| r.y < bottom));
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_system_pick(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    let r = centered(body, 70, body.height.min(11));
    f.render_widget(panel(app, "Choose files"), r);
    let inner = Rect::new(r.x + 3, r.y + 2, r.width.saturating_sub(6), r.height.saturating_sub(3));
    let state = if app.dialog_open { "The system file dialog is open (it may be behind this window)." } else { "The system file dialog is closed." };
    let key = |k: &'static str| Span::styled(format!(" {k} "), Style::new().bg(t.accent2).fg(t.bg).add_modifier(Modifier::BOLD));
    let lines = vec![
        Line::styled(fit(state, inner.width as usize), Style::new().fg(t.accent)),
        Line::raw(""),
        Line::from(vec![key("Enter"), Span::styled("  choose files (several at once)", Style::new().fg(t.text))]),
        Line::from(vec![key("f"), Span::styled("  choose a folder instead", Style::new().fg(t.text))]),
        Line::from(vec![key("t"), Span::styled("  use the terminal file browser this time", Style::new().fg(t.text))]),
        Line::raw(""),
        Line::styled(fit("Change this under Settings: Pick files with.", inner.width as usize), Style::new().fg(t.dim)),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn progress_bar(app: &App, width: usize, frac: f64, flash: bool) -> Line<'static> {
    const PART: [&str; 8] = [" ", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let cells = (frac.clamp(0.0, 1.0) * width as f64 * 8.0) as usize;
    let (full, part) = (cells / 8, cells % 8);
    let mut spans = Vec::new();
    for i in 0..width {
        let t = i as f32 / width.max(1) as f32;
        if i < full {
            spans.push(Span::styled("█", Style::new().fg(if flash { app.theme.warn } else { app.theme.grad(t) })));
        } else if i == full && part > 0 {
            spans.push(Span::styled(PART[part], Style::new().fg(app.theme.grad(t)).bg(app.theme.border)));
        } else {
            spans.push(Span::styled(" ", Style::new().bg(app.theme.border)));
        }
    }
    Line::from(spans)
}

fn draw_uploading(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    let r = centered(body, 90, body.height.min(22));
    f.render_widget(panel(app, "Uploading"), r);
    let inner = Rect::new(r.x + 3, r.y + 2, r.width.saturating_sub(6), r.height.saturating_sub(3));
    let Some(up) = &app.up else { return };
    let s = up.run.snapshot();
    let frac = s.done as f64 / s.total.max(1) as f64;
    let status = match s.status {
        _ if s.cancelled => "Cancelling…".to_owned(),
        _ if s.paused => "Paused".to_owned(),
        RunStatus::Uploading => format!("{} Uploading", spinner(app)),
        RunStatus::Resuming { chunks, total } => format!("Resuming - {chunks}/{} chunks already uploaded", total.map_or("?".into(), |t| t.to_string())),
        RunStatus::Reconnecting => "Connection lost - waiting for the network…".into(),
        RunStatus::Busy => "Server busy, slowing down".into(),
        RunStatus::Finishing => format!("{} Finishing on the server…", spinner(app)),
    };
    let mut lines = vec![Line::styled(status, Style::new().fg(t.accent).add_modifier(Modifier::BOLD)), Line::raw("")];
    lines.push(progress_bar(app, inner.width as usize, frac, s.resumed_flash));
    lines.push(Line::raw(""));
    let dash = "--".to_owned();
    lines.push(Line::from(vec![
        Span::styled(format!("{:>5.1}%", frac * 100.0), Style::new().fg(t.text).add_modifier(Modifier::BOLD)),
        Span::styled(format!("   {} / {}", fmt_bytes(s.done), fmt_bytes(s.total)), Style::new().fg(t.text)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("speed ", Style::new().fg(t.dim)),
        Span::styled(s.speed_mbs.map_or(dash.clone(), fmt_speed_mbs), Style::new().fg(t.accent2)),
        Span::styled("   average ", Style::new().fg(t.dim)),
        Span::styled(s.avg_mbs.map_or(dash.clone(), fmt_speed_mbs), Style::new().fg(t.text)),
        Span::styled("   time left ", Style::new().fg(t.dim)),
        Span::styled(s.eta_s.map_or(dash.clone(), fmt_time), Style::new().fg(t.text)),
        Span::styled("   elapsed ", Style::new().fg(t.dim)),
        Span::styled(fmt_time(s.elapsed_s), Style::new().fg(t.text)),
    ]));
    if let Some(p) = &s.plan {
        lines.push(Line::styled(p.describe(), Style::new().fg(t.dim)));
    }
    lines.push(Line::raw(""));
    let room = (inner.height as usize).saturating_sub(lines.len() + 2);
    for (name, (phase, bytes, size)) in up.run.files.iter().map(|f| &f.name).zip(s.files.iter()).take(room) {
        let (icon, color) = match phase {
            Phase::Waiting => ("·", t.dim),
            Phase::Active => (spinner(app), t.accent),
            Phase::Done => ("✔", t.good),
            Phase::Failed => ("✘", t.bad),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(color)),
            Span::styled(pad_right(&fit(name, (inner.width as usize).saturating_sub(24)), (inner.width as usize).saturating_sub(24)), Style::new().fg(t.text)),
            Span::styled(format!("{:>10} / {}", fmt_bytes(*bytes), fmt_bytes(*size)), Style::new().fg(t.dim)),
        ]));
    }
    if up.run.files.len() > room && room > 0 {
        lines.push(Line::styled(format!("… {} more", up.run.files.len() - room), Style::new().fg(t.dim)));
    }
    if up.confirm_cancel {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Cancel this upload?  y / c = yes, n / Esc = keep going", Style::new().fg(t.warn).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_result(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    let rows = app.result_rows();
    let fin = &app.result.fin;
    let failed_all = !fin.any_ok();
    let title = if failed_all { "Upload failed" } else if fin.any_failed() { "Uploaded (some files failed)" } else { "Upload successful" };
    f.render_widget(panel(app, title), body);
    let inner = Rect::new(body.x + 3, body.y + 2, body.width.saturating_sub(6), body.height.saturating_sub(3));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(e) = &app.result.fin.failed {
        lines.push(Line::styled(format!("Error: {e}"), Style::new().fg(t.bad)));
        lines.push(Line::raw(""));
    }
    let avg = if app.result.elapsed > 0.0 { app.result.fin.ok_bytes() as f64 / 1_048_576.0 / app.result.elapsed } else { 0.0 };
    if !failed_all {
        lines.push(Line::from(vec![
            Span::styled(format!("{} in ", fmt_bytes(app.result.fin.ok_bytes())), Style::new().fg(t.dim)),
            Span::styled(fmt_time(app.result.elapsed), Style::new().fg(t.text)),
            Span::styled(format!("   average {}", fmt_speed_mbs(avg)), Style::new().fg(t.accent2)),
        ]));
        lines.push(Line::raw(""));
    }
    let top = lines.len() as u16;
    let avail = inner.height.saturating_sub(top) as usize;
    // keep the cursor row on screen: rows (with headers) are laid out first, then windowed
    let mut flat: Vec<(Option<usize>, Line)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if let Some(h) = &row.header {
            flat.push((None, Line::styled(h.clone(), Style::new().fg(t.accent).add_modifier(Modifier::BOLD))));
        }
        let cur = app.result.cursor == i;
        let url = fit(&row.url, (inner.width as usize).saturating_sub(34 + 20));
        let mut spans = vec![
            Span::styled(if cur { "▸ " } else { "  " }, Style::new().fg(t.accent2)),
            Span::styled(pad_right(row.label, 30), Style::new().fg(if row.label == "FAILED" { t.bad } else { t.dim })),
            Span::styled(url, Style::new().fg(if row.label == "FAILED" { t.bad } else { t.text })),
        ];
        if row.default {
            spans.push(Span::styled(if app.result.copied == Some(i) { "  (default, copied)" } else { "  (default)" }, Style::new().fg(t.good).add_modifier(Modifier::BOLD)));
        } else if app.result.copied == Some(i) {
            spans.push(Span::styled("  (copied)", Style::new().fg(t.good)));
        }
        let mut line = Line::from(spans);
        if cur {
            line = line.style(Style::new().bg(t.sel_bg));
        }
        flat.push((Some(i), line));
    }
    let cur_pos = flat.iter().position(|(i, _)| *i == Some(app.result.cursor)).unwrap_or(0);
    let start = if avail == 0 { 0 } else { (cur_pos + 1).saturating_sub(avail) };
    for (y, (idx, line)) in (inner.y + top..).zip(flat.into_iter().skip(start).take(avail)) {
        if let Some(i) = idx {
            app.hits.push((Rect::new(inner.x, y, inner.width, 1), Hit::ResultRow(i)));
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_recent(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    f.render_widget(panel(app, "Recent uploads"), body);
    let inner = Rect::new(body.x + 2, body.y + 1, body.width.saturating_sub(4), body.height.saturating_sub(2));
    if app.recent.loading && app.recent.entries.is_empty() {
        f.render_widget(Paragraph::new(Span::styled(format!("{} Loading…", spinner(app)), Style::new().fg(t.accent))), inner);
        return;
    }
    if let Some(e) = &app.recent.error {
        f.render_widget(Paragraph::new(Span::styled(fit(e, inner.width as usize), Style::new().fg(t.bad))), inner);
        return;
    }
    if app.recent.entries.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("No uploads yet.", Style::new().fg(t.dim))), inner);
        return;
    }
    let h = inner.height as usize;
    let rc = &mut app.recent;
    if rc.cursor < rc.scroll {
        rc.scroll = rc.cursor;
    } else if rc.cursor >= rc.scroll + h {
        rc.scroll = rc.cursor + 1 - h;
    }
    let now = crate::common::now_ms();
    let (scroll, cursor) = (rc.scroll, rc.cursor);
    let entries: Vec<_> = rc.entries.iter().enumerate().skip(scroll).take(h).map(|(i, e)| (i, e.clone())).collect();
    for (i, e) in entries {
        let rect = Rect::new(inner.x, inner.y + (i - scroll) as u16, inner.width, 1);
        let name_w = (inner.width as usize).saturating_sub(2 + 10 + 12 + 24);
        let line = Line::from(vec![
            Span::styled(if i == cursor { "▸ " } else { "  " }, Style::new().fg(t.accent2)),
            Span::styled(pad_right(&fit(&e.name, name_w), name_w), Style::new().fg(t.text)),
            Span::styled(format!("{:<10}", fit(&e.category, 9)), Style::new().fg(t.accent)),
            Span::styled(format!("{:>12}", fmt_bytes(e.size)), Style::new().fg(t.dim)),
            Span::styled(format!("  {}", tvfs_core::fmt::describe_expiry(e.expires_at, now)), Style::new().fg(t.dim)),
        ]);
        f.render_widget(Paragraph::new(line).style(if i == cursor { Style::new().bg(t.sel_bg) } else { Style::new() }), rect);
        app.hits.push((rect, Hit::Recent(i)));
    }
}

/// `text` cut to `w` columns; when it does not fit it scrolls in a loop after a short pause, and the app keeps ticking
/// while any such text is on screen (nothing ticks when everything fits).
fn marquee(app: &mut App, text: &str, w: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if width(text) <= w || w == 0 {
        return text.to_owned();
    }
    app.marquee_active = true;
    const GAP: &str = "     ·     ";
    let cycle: Vec<char> = chars.iter().copied().chain(GAP.chars()).collect();
    let pause = 8; // steps standing still at the start of every round
    let step = (app.marquee_start.elapsed().as_millis() / crate::tui::app::MARQUEE_STEP_MS as u128) as usize;
    let pos = step % (cycle.len() + pause);
    let off = pos.saturating_sub(pause);
    (0..w).map(|i| cycle[(off + i) % cycle.len()]).collect()
}

fn draw_settings(app: &mut App, f: &mut Frame, body: Rect) {
    let t = app.theme.clone();
    // wide enough for the notes on a normal terminal; on narrower ones a note that does not fit scrolls (marquee)
    let r = centered(body, 112, body.height.min(16));
    f.render_widget(panel(app, "Settings"), r);
    let c = &app.cfg;
    let copy = match c.auto_copy {
        tvfs_core::config::AutoCopy::Off => "off",
        tvfs_core::config::AutoCopy::Normal => "normal link",
        tvfs_core::config::AutoCopy::Styled => "styled link",
    };
    let rows: [(&str, String, &str); SETTINGS_ROWS] = [
        ("Theme", app.theme_override.unwrap_or(app.cli.theme).name().to_owned(), "auto follows the naTiVeFS app"),
        ("Pick files with", app.cli.picker.name().to_owned(), "what Upload opens: terminal browser or the system file dialog"),
        ("Auto copy after upload", copy.to_owned(), "shared with the app"),
        ("When a reader / player page exists", if c.auto_copy_prefer_tool { "copy the tool link" } else { "copy the file link" }.to_owned(), "shared with the app"),
        ("If an image was uploaded", if c.auto_copy_image_styled { "styled link" } else { "normal link" }.to_owned(), "shared with the app"),
        ("TVFS Users Only uploads", if c.tvfs_users_only { "on" } else { "off" }.to_owned(), "shared with the app"),
    ];
    for (i, (label, value, note)) in rows.iter().enumerate() {
        let rect = Rect::new(r.x + 2, r.y + 2 + i as u16 * 2, r.width.saturating_sub(4), 1);
        if rect.y >= r.y + r.height - 1 {
            break;
        }
        let sel = app.settings_row == i;
        let note_w = (rect.width as usize).saturating_sub(2 + 38 + 26);
        let line = Line::from(vec![
            Span::styled(if sel { "▸ " } else { "  " }, Style::new().fg(t.accent2)),
            Span::styled(pad_right(label, 38), Style::new().fg(t.text)),
            Span::styled(pad_right(&format!("‹ {value} ›"), 26), Style::new().fg(t.accent).add_modifier(Modifier::BOLD)),
            Span::styled(if sel { marquee(app, note, note_w) } else { fit(note, note_w) }, Style::new().fg(t.dim)),
        ]);
        f.render_widget(Paragraph::new(line).style(if sel { Style::new().bg(t.sel_bg) } else { Style::new() }), rect);
        app.hits.push((rect, Hit::Setting(i)));
    }
}
