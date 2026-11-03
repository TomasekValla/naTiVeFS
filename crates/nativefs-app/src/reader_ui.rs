//! The text / Markdown reader window (web: `generateTextReaderHtml` / `generateMdReaderHtml`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use slint::private_unstable_api::re_exports::StyledText;
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use tvfs_core::content::{Content, ContentKind};
use tvfs_core::markdown::{self, Block, BlockKind, Colors, Mark};

use crate::{windows, ReaderBlock, ReaderRow, ReaderWindow, Shared};

/// The web's `READER_FONTS` (files_backend/upload.js): id, button label, font family ("" = system font).
/// Noto Serif JP and M PLUS Rounded 1c are left out (owner: one Japanese font is enough); their saved ids
/// fall back to Noto Sans JP.
pub const FONTS: &[(&str, &str, &str)] = &[
    ("default", "Default", ""),
    ("inter", "Inter", "Inter"),
    ("manrope", "Manrope", "Manrope"),
    ("sora", "Sora", "Sora"),
    ("spacegrotesk", "SpaceGrot.", "Space Grotesk"),
    ("outfit", "Outfit", "Outfit"),
    ("jakarta", "Jakarta", "Plus Jakarta Sans"),
    ("poppins", "Poppins", "Poppins"),
    ("lora", "Lora", "Lora"),
    ("merriweather", "Merri.", "Merriweather"),
    ("ptserif", "PT Serif", "PT Serif"),
    ("garamond", "Garamond", "EB Garamond"),
    ("atkinson", "Atkinson", "Atkinson Hyperlegible"),
    ("lexend", "Lexend", "Lexend"),
    ("opendyslexic", "OpenDys", "OpenDyslexic"),
    ("jetbrains", "JB Mono", "JetBrains Mono"),
    ("spacemono", "SpaceMono", "Space Mono"),
    ("quicksand", "Quicksand", "Quicksand"),
    ("notosansjp", "NotoJP", "Noto Sans JP"),
];

/// `SIZES` of the web reader in rem.
const SIZES: [f32; 5] = [0.85, 1.0, 1.15, 1.35, 1.6];
const SIZE_LABELS: [&str; 5] = ["(0.85rem)", "(1rem)", "(1.15rem)", "(1.35rem)", "(1.6rem)"];
/// The heading gradient (`linear-gradient(135deg, var(--accent), var(--accent2))`).
const ACCENT: u32 = 0x2fffc4;
const ACCENT2: u32 = 0x7c4dff;
/// Search matches (no background colour in Slint text: matches are coloured, the current one underlined too).
const MATCH: u32 = 0xffb02e;
const MATCH_CURRENT: u32 = 0xff6a00;
/// Plain text up to this many lines is shown one line per row (each row gets the web's line height 1.8);
/// longer files use chunks of `TXT_CHUNK_LINES` lines in one box each (tighter lines, far fewer elements).
const TXT_LINE_ROWS_MAX: usize = 8000;
const TXT_CHUNK_LINES: usize = 60;
const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 24 * 1024 * 1024;

fn font_index(id: &str) -> usize {
    let id = match id {
        "notoserifjp" | "mplusrounded" => "notosansjp",
        other => other,
    };
    FONTS.iter().position(|f| f.0 == id).unwrap_or(0)
}

/// One search hit: block, plain-text byte range, and for tables the cell (row, column).
#[derive(Clone, Copy, PartialEq)]
struct Hit {
    block: usize,
    cell: Option<(usize, usize)>,
    start: usize,
    end: usize,
}

struct Image {
    img: slint::Image,
    w: f32,
    h: f32,
}

struct Reader {
    window: ReaderWindow,
    content: Content,
    markdown: bool,
    raw: Vec<u8>,
    text: String,
    blocks: Vec<Block>,
    /// TXT: the rows (one line, or a chunk of lines in big files) and the byte offset of each in `text`.
    chunks: Vec<(usize, String)>,
    txt_line_rows: bool,
    font: usize,
    size: usize,
    light: bool,
    query: String,
    hits: Vec<Hit>,
    current: Option<usize>,
    /// Task ticks changed by the user (task index -> checked), remembered per file.
    ticks: HashMap<usize, bool>,
    images: HashMap<String, Option<Image>>,
    zoom: f32,
}

thread_local! {
    static READERS: RefCell<Vec<Rc<RefCell<Reader>>>> = const { RefCell::new(Vec::new()) };
}

/// Saved task ticks: file URL -> task index -> checked.
type TickStore = HashMap<String, HashMap<String, bool>>;

fn ticks_path() -> Option<std::path::PathBuf> {
    tvfs_core::config::Config::data_dir().ok().map(|d| d.join("reader-tasks.json"))
}

fn load_ticks(url: &str) -> HashMap<usize, bool> {
    let store: TickStore = ticks_path().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    store.get(url).map(|m| m.iter().filter_map(|(k, v)| Some((k.parse().ok()?, *v))).collect()).unwrap_or_default()
}

fn save_ticks(url: &str, ticks: &HashMap<usize, bool>) {
    let Some(path) = ticks_path() else { return };
    let mut store: TickStore = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if ticks.is_empty() {
        store.remove(url);
    } else {
        store.insert(url.to_owned(), ticks.iter().map(|(k, v)| (k.to_string(), *v)).collect());
    }
    if let Ok(bytes) = serde_json::to_vec(&store) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
            eprintln!("naTiVeFS: could not save {}", path.display());
        }
    }
}

fn styled(markup: &str, fallback: &str) -> StyledText {
    StyledText::parse_interpolated::<StyledText>(markup, &[]).unwrap_or_else(|_| {
        // should not happen (all text is escaped); show the plain text rather than nothing
        let mut esc = String::new();
        for c in fallback.chars() {
            if c.is_ascii_punctuation() {
                esc.push('\\');
            }
            esc.push(c);
        }
        StyledText::parse_interpolated::<StyledText>(&esc, &[]).unwrap_or_default()
    })
}

/// Open `content` in a reader window (an already open reader of the same file comes to the front).
pub fn open(shared: &Arc<Shared>, content: Content) {
    let existing = READERS.with(|r| r.borrow().iter().find(|x| x.borrow().content.url == content.url).cloned());
    if let Some(r) = existing {
        windows::raise(r.borrow().window.window());
        return;
    }
    let Ok(window) = ReaderWindow::new() else { return };
    let cfg = shared.config.lock().unwrap().clone();
    let markdown = content.kind == ContentKind::Markdown;
    window.set_file_name(content.name.clone().into());
    window.set_local(content.local_path().is_some());
    window.set_markdown(markdown);
    window.set_status("Loading…".into());
    window.set_smooth_scroll(cfg.smooth_scroll);
    let reader = Rc::new(RefCell::new(Reader {
        window: window.clone_strong(),
        ticks: load_ticks(&content.url),
        content: content.clone(),
        markdown,
        raw: Vec::new(),
        text: String::new(),
        blocks: Vec::new(),
        chunks: Vec::new(),
        txt_line_rows: true,
        font: font_index(&cfg.reader_font),
        size: (cfg.reader_size as usize).min(SIZES.len() - 1),
        light: cfg.reader_light,
        query: String::new(),
        hits: Vec::new(),
        current: None,
        images: HashMap::new(),
        zoom: cfg.reader_zoom,
    }));
    apply_look(&reader.borrow());
    wire(shared, &reader);
    READERS.with(|r| r.borrow_mut().push(reader.clone()));
    windows::place(window.window());
    let _ = window.show();
    let zoom = reader.borrow().zoom;
    let w2 = window.as_weak();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        if let Some(w) = w2.upgrade() {
            windows::apply_zoom(w.window(), zoom);
        }
    });

    // load the file (with the session cookie: "TVFS Users Only" files need it)
    let (shared2, weak) = (shared.clone(), window.as_weak());
    let url = content.url.clone();
    shared.rt.spawn(async move {
        let result = shared2.client.fetch_bytes(&url, MAX_TEXT_BYTES).await;
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            let Some(r) = find(&w) else { return };
            match result {
                Ok(bytes) => {
                    loaded(&r, bytes.to_vec());
                    let shared3 = shared2.clone();
                    load_images(&shared3, &r);
                }
                Err(e) => {
                    let msg = format!("Failed to load: {}", crate::open::describe(&e));
                    w.set_status(msg.into());
                    w.set_status_error(true);
                }
            }
        });
    });
}

fn find(w: &ReaderWindow) -> Option<Rc<RefCell<Reader>>> {
    READERS.with(|r| r.borrow().iter().find(|x| std::ptr::eq(x.borrow().window.window(), w.window())).cloned())
}

fn loaded(r: &Rc<RefCell<Reader>>, bytes: Vec<u8>) {
    let mut rd = r.borrow_mut();
    let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).replace("\r\n", "\n");
    rd.raw = bytes;
    if rd.markdown {
        rd.blocks = markdown::parse(&text);
    } else if text.split('\n').count() <= TXT_LINE_ROWS_MAX {
        let mut off = 0;
        rd.chunks = text
            .split('\n')
            .map(|l| {
                let row = (off, l.to_owned());
                off += l.len() + 1;
                row
            })
            .collect();
        rd.txt_line_rows = true;
    } else {
        rd.txt_line_rows = false;
        let mut chunks = Vec::new();
        let mut start = 0;
        let mut lines = 0;
        for (i, c) in text.char_indices() {
            if c == '\n' {
                lines += 1;
                if lines == TXT_CHUNK_LINES {
                    chunks.push((start, text[start..i].to_owned()));
                    start = i + 1;
                    lines = 0;
                }
            }
        }
        if start < text.len() || chunks.is_empty() {
            chunks.push((start, text[start..].to_owned()));
        }
        rd.chunks = chunks;
    }
    rd.text = text;
    rd.window.set_status("".into());
    // debug builds: `NATIVEFS_READER_FIND=<text>` opens Find with that text (synthetic typing does not reach winit windows)
    #[cfg(debug_assertions)]
    if let Ok(q) = std::env::var("NATIVEFS_READER_FIND") {
        rd.window.set_find_open(true);
        rd.window.set_find_text(q.as_str().into());
        rd.query = q;
        run_search(&mut rd);
        if let Some(n) = std::env::var("NATIVEFS_READER_FIND_STEP").ok().and_then(|v| v.parse::<usize>().ok()) {
            rd.current = Some(n.min(rd.hits.len().saturating_sub(1)));
        }
    }
    rebuild(&rd);
    if rd.current.is_some() {
        show_find_state(&rd);
    }
}

/// Font size / theme / font into the window properties.
fn apply_look(rd: &Reader) {
    let w = &rd.window;
    let f = FONTS[rd.font];
    w.set_font_family(f.2.into());
    w.set_font_label(f.1.into());
    w.set_size_label(SIZE_LABELS[rd.size].into());
    w.set_base_size(16.0 * SIZES[rd.size]);
    w.set_light(rd.light);
}

fn rgb_mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |c: u32, s: u32| ((c >> s) & 0xff) as f32;
    let m = |s: u32| ((ch(a, s) + (ch(b, s) - ch(a, s)) * t).round() as u32) << s;
    m(16) | m(8) | m(0)
}

/// Rebuild the whole block model (after loading, a size / theme change, or a search).
fn rebuild(rd: &Reader) {
    let base = 16.0 * SIZES[rd.size];
    let (fg, bg) = if rd.light { (0x1a1a1a, 0xfafafa) } else { (0xe8e8e8, 0x111111) };
    let colors = Colors { strong: Some(if rd.light { 0x000000 } else { 0xffffff }), em: Some(rgb_mix(bg, fg, 0.85)) };
    let current = rd.current.and_then(|i| rd.hits.get(i)).copied();
    let marks_for = |block: usize, cell: Option<(usize, usize)>| -> Vec<Mark> {
        rd.hits
            .iter()
            .filter(|h| h.block == block && h.cell == cell)
            .map(|h| {
                let cur = Some(*h) == current;
                Mark { start: h.start, end: h.end, color: if cur { MATCH_CURRENT } else { MATCH }, underline: cur }
            })
            .collect()
    };
    let mut out: Vec<ReaderBlock> = Vec::new();
    if !rd.markdown {
        for (i, (_, chunk)) in rd.chunks.iter().enumerate() {
            let marks = marks_for(i, None);
            let lh = if rd.txt_line_rows { 1.8 } else { 0.0 };
            if marks.is_empty() {
                out.push(ReaderBlock { kind: 6, plain: chunk.as_str().into(), lh, font_size: base, task: -1, sel_start: -1, sel_end: -1, ..Default::default() });
            } else {
                // a row with search matches: styled text, so every match can be coloured
                let spans = [markdown::Span { text: chunk.clone(), style: 0, link: None }];
                let markup = markdown::to_markup(&spans, &marks, None, Colors::default());
                out.push(ReaderBlock { kind: 0, text: styled(&markup, chunk), lh, font_size: base, task: -1, sel_start: -1, sel_end: -1, ..Default::default() });
            }
        }
    } else {
        let mut prev_bottom: Option<f32> = None;
        let mut prev_quote = false;
        let n = rd.blocks.len();
        for (i, b) in rd.blocks.iter().enumerate() {
            let in_list = b.indent > 0;
            let (fs, top, bottom) = match b.kind {
                BlockKind::Heading => {
                    let em = [2.0, 1.5, 1.25, 1.05, 0.83, 0.67][(b.level.clamp(1, 6) - 1) as usize];
                    (base * em, 1.6 * base * em, 0.6 * base * em)
                }
                BlockKind::Code => (base * 0.9, base, base),
                BlockKind::Table => (base * 0.92, base * 0.92, base * 0.92),
                BlockKind::Rule => (base, 1.8 * base, 1.8 * base),
                BlockKind::Image => (base, 0.8 * base, 0.8 * base),
                BlockKind::Paragraph if in_list => (base, 0.3 * base, 0.3 * base),
                BlockKind::Paragraph => (base, 0.8 * base, 0.8 * base),
            };
            // a list as a whole has .6em margins around it
            let prev_in_list = i > 0 && rd.blocks[i - 1].indent > 0;
            let top = if in_list != prev_in_list { top.max(0.6 * base) } else { top };
            let quote = b.quote > 0;
            let next_quote = rd.blocks.get(i + 1).is_some_and(|x| x.quote > 0);
            // a quote box has 1em margins outside and .4em padding + the paragraph's .8em inside
            let gap = match prev_bottom {
                // the next line of the same paragraph
                Some(_) if b.cont => 0.0,
                None => top,
                Some(pb) if quote && !prev_quote => pb.max(base),
                Some(pb) if !quote && prev_quote => pb.max(base).max(top),
                Some(pb) => pb.max(top),
            };
            let mut rb = ReaderBlock { font_size: fs, gap, indent: 25.6 / 16.0 * base * b.indent as f32, task: -1, ..Default::default() };
            rb.quote = quote;
            rb.quote_first = quote && !prev_quote && !b.cont;
            rb.quote_last = quote && !next_quote;
            rb.marker = b.marker.as_str().into();
            if let Some(done) = b.task {
                let done = rd.ticks.get(&b.task_index).copied().unwrap_or(done);
                rb.task = done as i32;
                rb.task_index = b.task_index as i32;
            }
            match b.kind {
                BlockKind::Paragraph => {
                    rb.kind = 0;
                    rb.lh = 1.75;
                    let markup = markdown::to_markup(&b.spans, &marks_for(i, None), None, colors);
                    rb.text = styled(&markup, &b.plain());
                }
                BlockKind::Heading => {
                    rb.kind = 1;
                    rb.lh = 1.25;
                    rb.level = b.level as i32;
                    let spans: Vec<_> = b.spans.iter().cloned().map(|mut s| {
                        s.style |= markdown::STRONG;
                        s
                    }).collect();
                    // the web paints the gradient over the full column: short headings stay near the teal end
                    let chars = markdown::spans_plain(&spans).chars().count() as f32;
                    let reach = ((chars * fs * 0.56 + fs * 0.6) / (812.0 + fs * 1.25)).clamp(0.05, 1.0);
                    let markup = markdown::to_markup(&spans, &marks_for(i, None), Some((ACCENT, rgb_mix(ACCENT, ACCENT2, reach))), Colors::default());
                    rb.text = styled(&markup, &b.plain());
                }
                BlockKind::Code => {
                    rb.kind = 2;
                    rb.plain = b.code.as_str().into();
                    rb.sel_start = -1;
                    rb.sel_end = -1;
                    let marks = marks_for(i, None);
                    if !marks.is_empty() {
                        let spans = [markdown::Span { text: b.code.clone(), style: 0, link: None }];
                        rb.text = styled(&markdown::to_markup(&spans, &marks, None, Colors::default()), &b.code);
                        rb.code_styled = true;
                    }
                }
                BlockKind::Rule => rb.kind = 3,
                BlockKind::Table => {
                    rb.kind = 4;
                    let cols = b.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    let mut widths = vec![0f32; cols];
                    for row in &b.rows {
                        for (c, cell) in row.iter().enumerate() {
                            let chars = markdown::spans_plain(cell).chars().count() as f32;
                            widths[c] = widths[c].max(chars * fs * 0.55 + 28.0 + 2.0).max(44.0);
                        }
                    }
                    let avail = 812.0 - rb.indent;
                    let sum: f32 = widths.iter().sum();
                    if sum > avail {
                        for w in &mut widths {
                            *w *= avail / sum;
                        }
                    }
                    rb.col_w = ModelRc::new(VecModel::from(widths));
                    let rows: Vec<ReaderRow> = b
                        .rows
                        .iter()
                        .enumerate()
                        .map(|(ri, row)| {
                            let cells: Vec<StyledText> = row
                                .iter()
                                .enumerate()
                                .map(|(ci, cell)| {
                                    let mut spans = cell.clone();
                                    if ri == 0 {
                                        for s in &mut spans {
                                            s.style |= markdown::STRONG;
                                        }
                                    }
                                    let markup = markdown::to_markup(&spans, &marks_for(i, Some((ri, ci))), None, if ri == 0 { Colors::default() } else { colors });
                                    styled(&markup, &markdown::spans_plain(cell))
                                })
                                .collect();
                            ReaderRow { cells: ModelRc::new(VecModel::from(cells)), header: ri == 0, even: ri > 0 && ri % 2 == 0 }
                        })
                        .collect();
                    rb.rows = ModelRc::new(VecModel::from(rows));
                }
                BlockKind::Image => {
                    rb.kind = 5;
                    rb.alt = b.alt.as_str().into();
                    if let Some(Some(img)) = rd.images.get(&b.src) {
                        rb.img = img.img.clone();
                        rb.has_img = true;
                        rb.img_w = img.w;
                        rb.img_h = img.h;
                    }
                }
            }
            prev_bottom = Some(bottom);
            prev_quote = quote;
            out.push(rb);
            let _ = n;
        }
    }
    let model = rd.window.get_blocks();
    if let Some(vm) = model.as_any().downcast_ref::<VecModel<ReaderBlock>>() {
        if vm.row_count() == out.len() {
            for (i, b) in out.into_iter().enumerate() {
                if vm.row_data(i).as_ref() != Some(&b) {
                    vm.set_row_data(i, b);
                }
            }
            return;
        }
    }
    rd.window.set_blocks(ModelRc::new(VecModel::from(out)));
}

/// Fetch the images of a Markdown file (relative sources resolve against the file's URL).
fn load_images(shared: &Arc<Shared>, r: &Rc<RefCell<Reader>>) {
    let rd = r.borrow();
    let base = url::Url::parse(&rd.content.url).ok();
    let mut srcs: Vec<String> = rd.blocks.iter().filter(|b| b.kind == BlockKind::Image).map(|b| b.src.clone()).collect();
    srcs.sort();
    srcs.dedup();
    let weak = rd.window.as_weak();
    drop(rd);
    for src in srcs {
        // web pictures; local ones only for a local document (a TVFS file never reads this computer's files)
        let local_doc = base.as_ref().is_some_and(|b| b.scheme() == "file");
        let Some(abs) = base.as_ref().and_then(|b| b.join(&src).ok()).filter(|u| matches!(u.scheme(), "http" | "https") || (local_doc && u.scheme() == "file")) else { continue };
        let (shared2, weak) = (shared.clone(), weak.clone());
        shared.rt.spawn(async move {
            let decoded = async {
                let bytes = shared2.client.fetch_bytes(abs.as_str(), MAX_IMAGE_BYTES).await.ok()?;
                tokio::task::spawn_blocking(move || {
                    let img = image::load_from_memory(&bytes).ok()?.into_rgba8();
                    let (w, h) = img.dimensions();
                    Some(SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(img.as_raw(), w, h))
                })
                .await
                .ok()?
            }
            .await;
            let _ = slint::invoke_from_event_loop(move || {
                let Some(w) = weak.upgrade() else { return };
                let Some(r) = find(&w) else { return };
                let mut rd = r.borrow_mut();
                let entry = decoded.map(|buf| {
                    let (iw, ih) = (buf.width() as f32, buf.height() as f32);
                    Image { img: slint::Image::from_rgba8(buf), w: iw, h: ih }
                });
                rd.images.insert(src, entry);
                drop(rd);
                rebuild(&r.borrow());
            });
        });
    }
}

fn run_search(rd: &mut Reader) {
    rd.hits.clear();
    rd.current = None;
    let q = rd.query.clone();
    if q.is_empty() {
        return;
    }
    if rd.markdown {
        for (i, b) in rd.blocks.iter().enumerate() {
            match b.kind {
                BlockKind::Paragraph | BlockKind::Heading | BlockKind::Code => {
                    for (s, e) in markdown::find_all(&b.plain(), &q) {
                        rd.hits.push(Hit { block: i, cell: None, start: s, end: e });
                    }
                }
                BlockKind::Table => {
                    for (ri, row) in b.rows.iter().enumerate() {
                        for (ci, cell) in row.iter().enumerate() {
                            for (s, e) in markdown::find_all(&markdown::spans_plain(cell), &q) {
                                rd.hits.push(Hit { block: i, cell: Some((ri, ci)), start: s, end: e });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    } else {
        for (i, (_, chunk)) in rd.chunks.iter().enumerate() {
            for (s, e) in markdown::find_all(chunk, &q) {
                rd.hits.push(Hit { block: i, cell: None, start: s, end: e });
            }
        }
    }
    if !rd.hits.is_empty() {
        rd.current = Some(0);
    }
}

fn show_find_state(rd: &Reader) {
    let info = match (rd.query.is_empty(), rd.current) {
        (true, _) => String::new(),
        (false, None) => "No matches".into(),
        (false, Some(i)) => format!("{} / {}", i + 1, rd.hits.len()),
    };
    rd.window.set_find_info(info.into());
    match rd.current.and_then(|i| rd.hits.get(i)) {
        Some(h) => {
            rd.window.set_current_block(h.block as i32);
            rd.window.set_reveal_seq(rd.window.get_reveal_seq() + 1);
        }
        None => rd.window.set_current_block(-1),
    }
}

/// Settings page "Reset": every open reader back to 100 %, and the saved zoom.
pub fn reset_zoom(shared: &Arc<Shared>) {
    READERS.with(|r| {
        for rd in r.borrow().iter() {
            let mut rd = rd.borrow_mut();
            rd.zoom = 1.0;
            windows::apply_zoom(rd.window.window(), 1.0);
        }
    });
    let mut cfg = shared.config.lock().unwrap();
    cfg.reader_zoom = 1.0;
    let _ = cfg.save();
}

fn save_look(shared: &Arc<Shared>, rd: &Reader) {
    let mut cfg = shared.config.lock().unwrap();
    cfg.reader_font = FONTS[rd.font].0.to_owned();
    cfg.reader_size = rd.size as u8;
    cfg.reader_light = rd.light;
    cfg.reader_zoom = rd.zoom;
    let _ = cfg.save();
}

fn wire(shared: &Arc<Shared>, r: &Rc<RefCell<Reader>>) {
    let w = r.borrow().window.clone_strong();
    let weak_r = Rc::downgrade(r);
    let with = move |f: &dyn Fn(&Rc<RefCell<Reader>>)| {
        if let Some(r) = weak_r.upgrade() {
            f(&r);
        }
    };
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_cycle_font(move || {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.font = (rd.font + 1) % FONTS.len();
                apply_look(&rd);
                save_look(&shared, &rd);
            })
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_cycle_size(move || {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.size = (rd.size + 1) % SIZES.len();
                apply_look(&rd);
                save_look(&shared, &rd);
                rebuild(&rd);
            })
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_toggle_theme(move || {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.light = !rd.light;
                apply_look(&rd);
                save_look(&shared, &rd);
                rebuild(&rd);
            })
        });
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_download(move || {
            with(&|r| {
                let rd = r.borrow();
                if let Some(path) = rd.content.local_path() {
                    crate::files_ui::show_in_folder(&path);
                } else if rd.raw.is_empty() {
                    crate::files_ui::save_as(&shared, &rd.content);
                } else {
                    crate::files_ui::save_bytes(&rd.content.name, rd.raw.clone(), {
                        let weak = rd.window.as_weak();
                        move |msg: &str| {
                            if let Some(w) = weak.upgrade() {
                                let weak = w.as_weak();
                                windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, msg);
                            }
                        }
                    });
                }
            })
        });
    }
    {
        let with = with.clone();
        w.on_copy_all(move || {
            with(&|r| {
                let rd = r.borrow();
                crate::copy_to_clipboard(&rd.text);
                let weak = rd.window.as_weak();
                windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, "Copied the whole text");
            })
        });
    }
    {
        let with = with.clone();
        w.on_find_edited(move |q| {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.query = q.to_string();
                run_search(&mut rd);
                rebuild(&rd);
                show_find_state(&rd);
            })
        });
    }
    {
        let with = with.clone();
        w.on_find_step(move |dir| {
            with(&|r| {
                let mut rd = r.borrow_mut();
                if rd.hits.is_empty() {
                    return;
                }
                let n = rd.hits.len() as i32;
                let cur = rd.current.map_or(0, |c| c as i32);
                rd.current = Some(((cur + dir).rem_euclid(n)) as usize);
                rebuild(&rd);
                show_find_state(&rd);
            })
        });
    }
    {
        let with = with.clone();
        w.on_find_closed(move || {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.query.clear();
                rd.hits.clear();
                rd.current = None;
                rd.window.set_find_text("".into());
                rebuild(&rd);
                show_find_state(&rd);
            })
        });
    }
    {
        let with = with.clone();
        w.on_task_toggled(move |index| {
            with(&|r| {
                let mut rd = r.borrow_mut();
                let index = index.max(0) as usize;
                let original = rd.blocks.iter().find(|b| b.task.is_some() && b.task_index == index).and_then(|b| b.task).unwrap_or(false);
                let now = !rd.ticks.get(&index).copied().unwrap_or(original);
                if now == original {
                    rd.ticks.remove(&index);
                } else {
                    rd.ticks.insert(index, now);
                }
                let url = rd.content.url.clone();
                save_ticks(&url, &rd.ticks);
                rebuild(&rd);
            })
        });
    }
    {
        let shared = shared.clone();
        w.on_link_clicked(move |url| crate::open::open_or_browse(&shared, &url));
    }
    {
        let (with, shared) = (with.clone(), shared.clone());
        w.on_zoom_step(move |dir| {
            with(&|r| {
                let mut rd = r.borrow_mut();
                rd.zoom = windows::zoom_after(rd.zoom, dir);
                windows::apply_zoom(rd.window.window(), rd.zoom);
                save_look(&shared, &rd);
                crate::refresh_viewer_zooms(&shared);
            })
        });
    }
    // raw input: wheel, middle-button autoscroll, Ctrl + wheel zoom
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
    {
        let weak = w.as_weak();
        hooks.zoom = Some(Box::new(move |dir| {
            if let Some(w) = weak.upgrade() {
                w.invoke_zoom_step(dir);
            }
        }));
    }
    {
        let weak = Rc::downgrade(r);
        hooks.rezoom = Some(Box::new(move || {
            if let Some(r) = weak.upgrade() {
                let rd = r.borrow();
                windows::apply_zoom(rd.window.window(), rd.zoom);
            }
        }));
    }
    {
        let weak = w.as_weak();
        hooks.paste_link = Some(crate::open::paste_hook(shared, move |m| {
            let weak = weak.clone();
            windows::toast(move |t| { if let Some(w) = weak.upgrade() { w.set_toast(t.into()); } }, m);
        }));
    }
    windows::attach(w.window(), hooks);
    {
        let weak = Rc::downgrade(r);
        w.window().on_close_requested(move || {
            if let Some(r) = weak.upgrade() {
                READERS.with(|list| list.borrow_mut().retain(|x| !Rc::ptr_eq(x, &r)));
            }
            crate::windows::closed();
            slint::CloseRequestResponse::HideWindow
        });
    }
}
