//! The text / Markdown reader window (web: `generateTextReaderHtml` / `generateMdReaderHtml`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use slint::private_unstable_api::re_exports::StyledText;
use slint::{ComponentHandle, Model, ModelRc, SharedPixelBuffer, VecModel};
use tvfs_core::content::{Content, ContentKind};
use tvfs_core::markdown::{self, Block, BlockKind, Colors, Mark};

use crate::{windows, ReaderBlock, ReaderChunk, ReaderRow, ReaderWindow, Shared};

/// Blocks per run of the page (see `ReaderChunk`): few enough that a run on screen is cheap to draw, enough that
/// a long document has only a few hundred runs.
const CHUNK: usize = 32;

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
/// Markdown images downloaded and decoded at the same time.
static IMAGE_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(3);

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
    blocks: Arc<Vec<Block>>,
    /// TXT: the rows (one line, or a chunk of lines in big files) and the byte offset of each in `text`.
    chunks: Arc<Vec<(usize, String)>>,
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
    /// Increases with every rebuild; a background build of an older one stops.
    build_gen: std::cell::Cell<u64>,
    /// Big documents: which runs have their blocks, and the height of every run.
    virt: RefCell<Virtual>,
    /// A viewport update is already queued for this event-loop turn.
    live_pending: std::cell::Cell<bool>,
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
        blocks: Arc::new(Vec::new()),
        chunks: Arc::new(Vec::new()),
        txt_line_rows: true,
        font: font_index(&cfg.reader_font),
        size: (cfg.reader_size as usize).min(SIZES.len() - 1),
        light: cfg.reader_light,
        query: String::new(),
        hits: Vec::new(),
        current: None,
        images: HashMap::new(),
        build_gen: std::cell::Cell::new(0),
        virt: RefCell::new(Virtual::default()),
        live_pending: std::cell::Cell::new(false),
        zoom: cfg.reader_zoom,
    }));
    apply_look(&reader.borrow());
    wire(shared, &reader);
    READERS.with(|r| r.borrow_mut().push(reader.clone()));
    windows::place(window.window());
    let _ = window.show();
    windows::raise(window.window());
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
        // decoding, Markdown parsing and splitting run off the UI thread (a big file took seconds and froze the window);
        // the window only gets the finished result
        let result = match shared2.client.fetch_bytes(&url, MAX_TEXT_BYTES).await {
            Ok(bytes) => {
                let w3 = weak.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w3.upgrade() {
                        w.set_status("Reading the document…".into());
                    }
                });
                tokio::task::spawn_blocking(move || prepare(bytes.to_vec(), markdown))
                .await
                .map_err(|_| tvfs_core::Error::Server { status: 0, message: "the file could not be read".into() })
            }
            Err(e) => Err(e),
        };
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = weak.upgrade() else { return };
            let Some(r) = find(&w) else { return };
            match result {
                Ok(prepared) => {
                    loaded(&r, prepared);
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

/// A loaded file, ready for the window (made on a worker thread).
struct Prepared {
    raw: Vec<u8>,
    text: String,
    blocks: Vec<Block>,
    chunks: Vec<(usize, String)>,
    txt_line_rows: bool,
}

fn prepare(bytes: Vec<u8>, markdown: bool) -> Prepared {
    let text = String::from_utf8_lossy(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes)).replace("\r\n", "\n");
    let mut p = Prepared { raw: Vec::new(), text: String::new(), blocks: Vec::new(), chunks: Vec::new(), txt_line_rows: false };
    if markdown {
        p.blocks = markdown::parse(&text);
    } else if text.split('\n').count() <= TXT_LINE_ROWS_MAX {
        let mut off = 0;
        p.chunks = text
            .split('\n')
            .map(|l| {
                let row = (off, l.to_owned());
                off += l.len() + 1;
                row
            })
            .collect();
        p.txt_line_rows = true;
    } else {
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
        p.chunks = chunks;
    }
    p.raw = bytes;
    p.text = text;
    p
}

fn loaded(r: &Rc<RefCell<Reader>>, p: Prepared) {
    let mut rd = r.borrow_mut();
    rd.raw = p.raw;
    rd.text = p.text;
    rd.blocks = Arc::new(p.blocks);
    rd.chunks = Arc::new(p.chunks);
    rd.txt_line_rows = p.txt_line_rows;
    // cleared when the rows are in the window (big documents are built in the background)
    rd.window.set_status("Building the page…".into());
    // debug builds: `NATIVEFS_READER_FIND=<text>` opens Find with that text (synthetic typing does not reach winit windows)
    #[cfg(debug_assertions)]
    if let Some(q) = crate::debug::var("NATIVEFS_READER_FIND") {
        rd.window.set_find_open(true);
        rd.window.set_find_text(q.as_str().into());
        rd.query = q;
        run_search(&mut rd);
        if let Some(n) = crate::debug::var("NATIVEFS_READER_FIND_STEP").and_then(|v| v.parse::<usize>().ok()) {
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
/// Above this many rows the page is built in the background (a big document froze every window of the app for
/// seconds: they all share the UI thread).
const ASYNC_ROWS: usize = 4_000;

/// One display row, made off the UI thread: everything except the table models and the image, which must be made on
/// the UI thread (`StyledText` and strings may cross threads, Slint models and images may not).
#[derive(Default)]
struct RowSpec {
    kind: i32,
    text: StyledText,
    plain: slint::SharedString,
    level: i32,
    font_size: f32,
    gap: f32,
    indent: f32,
    marker: slint::SharedString,
    task: i32,
    task_index: i32,
    quote: bool,
    quote_first: bool,
    quote_last: bool,
    col_w: Vec<f32>,
    rows: Vec<(Vec<StyledText>, bool, bool)>,
    img_src: String,
    alt: slint::SharedString,
    sel_start: i32,
    sel_end: i32,
    lh: f32,
    code_styled: bool,
    /// Estimated height including the gap above (a big document's runs use it until they are measured).
    est: f32,
}

/// What building the rows reads from the reader: a snapshot, so the work can run on other threads.
#[derive(Clone)]
struct RowInputs {
    markdown: bool,
    blocks: Arc<Vec<Block>>,
    chunks: Arc<Vec<(usize, String)>>,
    txt_line_rows: bool,
    size: usize,
    light: bool,
    hits: Vec<Hit>,
    current: Option<Hit>,
    ticks: HashMap<usize, bool>,
    /// Width of the text column (for the height estimate).
    col_w: f32,
}

fn row_inputs(rd: &Reader) -> RowInputs {
    RowInputs {
        markdown: rd.markdown,
        blocks: rd.blocks.clone(),
        chunks: rd.chunks.clone(),
        txt_line_rows: rd.txt_line_rows,
        size: rd.size,
        light: rd.light,
        hits: rd.hits.clone(),
        current: rd.current.and_then(|i| rd.hits.get(i)).copied(),
        ticks: rd.ticks.clone(),
        col_w: rd.window.get_column_w().max(200.0),
    }
}

/// Font size and the margins above / below a Markdown block (the web's CSS).
fn block_margins(b: &Block, base: f32) -> (f32, f32, f32) {
    let in_list = b.indent > 0;
    match b.kind {
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
    }
}

/// Search marks of one block: (block, table cell)
type MarkKey = (usize, Option<(usize, usize)>);

/// All rows of the page, made on every CPU core: each row depends only on its block and the block before it.
fn make_rows(inp: &RowInputs) -> Vec<RowSpec> {
    use rayon::prelude::*;
    let base = 16.0 * SIZES[inp.size];
    let (fg, bg) = if inp.light { (0x1a1a1a, 0xfafafa) } else { (0xe8e8e8, 0x111111) };
    let colors = Colors { strong: Some(if inp.light { 0x000000 } else { 0xffffff }), em: Some(rgb_mix(bg, fg, 0.85)) };
    // search marks grouped once by block / table cell (not filtered again for every block)
    let mut marks: HashMap<MarkKey, Vec<Mark>> = HashMap::new();
    for h in &inp.hits {
        let cur = Some(*h) == inp.current;
        marks
            .entry((h.block, h.cell))
            .or_default()
            .push(Mark { start: h.start, end: h.end, color: if cur { MATCH_CURRENT } else { MATCH }, underline: cur });
    }
    let marks_for = |block: usize, cell: Option<(usize, usize)>| -> Vec<Mark> { marks.get(&(block, cell)).cloned().unwrap_or_default() };
    if !inp.markdown {
        let lh = if inp.txt_line_rows { 1.8 } else { 0.0 };
        return inp
            .chunks
            .par_iter()
            .enumerate()
            .map(|(i, (_, chunk))| {
                let marks = marks_for(i, None);
                let est = (chunk.matches('\n').count() + 1) as f32 * base * if lh > 0.0 { lh } else { 1.45 };
                if marks.is_empty() {
                    RowSpec { kind: 6, plain: chunk.as_str().into(), lh, font_size: base, task: -1, sel_start: -1, sel_end: -1, est, ..Default::default() }
                } else {
                    // a row with search matches: styled text, so every match can be coloured
                    let spans = [markdown::Span { text: chunk.clone(), style: 0, link: None }];
                    let markup = markdown::to_markup(&spans, &marks, None, Colors::default());
                    RowSpec { kind: 0, text: styled(&markup, chunk), lh, font_size: base, task: -1, sel_start: -1, sel_end: -1, est, ..Default::default() }
                }
            })
            .collect();
    }
    let blocks = &inp.blocks;
    blocks
        .par_iter()
        .enumerate()
        .map(|(i, b)| {
            let in_list = b.indent > 0;
            let (fs, top, _bottom) = block_margins(b, base);
            let prev = i.checked_sub(1).map(|p| &blocks[p]);
            // a list as a whole has .6em margins around it
            let prev_in_list = prev.is_some_and(|p| p.indent > 0);
            let top = if in_list != prev_in_list { top.max(0.6 * base) } else { top };
            let quote = b.quote > 0;
            let prev_quote = prev.is_some_and(|p| p.quote > 0);
            let next_quote = blocks.get(i + 1).is_some_and(|x| x.quote > 0);
            let prev_bottom = prev.map(|p| block_margins(p, base).2);
            // a quote box has 1em margins outside and .4em padding + the paragraph's .8em inside
            let gap = match prev_bottom {
                // the next line of the same paragraph
                Some(_) if b.cont => 0.0,
                None => top,
                Some(pb) if quote && !prev_quote => pb.max(base),
                Some(pb) if !quote && prev_quote => pb.max(base).max(top),
                Some(pb) => pb.max(top),
            };
            let mut rs = RowSpec { font_size: fs, gap, indent: 25.6 / 16.0 * base * b.indent as f32, task: -1, ..Default::default() };
            rs.quote = quote;
            rs.quote_first = quote && !prev_quote && !b.cont;
            rs.quote_last = quote && !next_quote;
            rs.marker = b.marker.as_str().into();
            if let Some(done) = b.task {
                let done = inp.ticks.get(&b.task_index).copied().unwrap_or(done);
                rs.task = done as i32;
                rs.task_index = b.task_index as i32;
            }
            match b.kind {
                BlockKind::Paragraph => {
                    rs.kind = 0;
                    rs.lh = 1.75;
                    let markup = markdown::to_markup(&b.spans, &marks_for(i, None), None, colors);
                    rs.text = styled(&markup, &b.plain());
                }
                BlockKind::Heading => {
                    rs.kind = 1;
                    rs.lh = 1.25;
                    rs.level = b.level as i32;
                    let spans: Vec<_> = b
                        .spans
                        .iter()
                        .cloned()
                        .map(|mut s| {
                            s.style |= markdown::STRONG;
                            s
                        })
                        .collect();
                    // the web paints the gradient over the full column: short headings stay near the teal end
                    let chars = markdown::spans_plain(&spans).chars().count() as f32;
                    let reach = ((chars * fs * 0.56 + fs * 0.6) / (812.0 + fs * 1.25)).clamp(0.05, 1.0);
                    let markup = markdown::to_markup(&spans, &marks_for(i, None), Some((ACCENT, rgb_mix(ACCENT, ACCENT2, reach))), Colors::default());
                    rs.text = styled(&markup, &b.plain());
                }
                BlockKind::Code => {
                    rs.kind = 2;
                    rs.plain = b.code.as_str().into();
                    rs.sel_start = -1;
                    rs.sel_end = -1;
                    let marks = marks_for(i, None);
                    if !marks.is_empty() {
                        let spans = [markdown::Span { text: b.code.clone(), style: 0, link: None }];
                        rs.text = styled(&markdown::to_markup(&spans, &marks, None, Colors::default()), &b.code);
                        rs.code_styled = true;
                    }
                }
                BlockKind::Rule => rs.kind = 3,
                BlockKind::Table => {
                    rs.kind = 4;
                    let cols = b.rows.iter().map(|r| r.len()).max().unwrap_or(0);
                    let mut widths = vec![0f32; cols];
                    for row in &b.rows {
                        for (c, cell) in row.iter().enumerate() {
                            let chars = markdown::spans_plain(cell).chars().count() as f32;
                            widths[c] = widths[c].max(chars * fs * 0.55 + 28.0 + 2.0).max(44.0);
                        }
                    }
                    let avail = 812.0 - rs.indent;
                    let sum: f32 = widths.iter().sum();
                    if sum > avail {
                        for w in &mut widths {
                            *w *= avail / sum;
                        }
                    }
                    rs.col_w = widths;
                    rs.rows = b
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
                            (cells, ri == 0, ri > 0 && ri % 2 == 0)
                        })
                        .collect();
                }
                BlockKind::Image => {
                    rs.kind = 5;
                    rs.alt = b.alt.as_str().into();
                    rs.img_src = b.src.clone();
                }
            }
            rs.est = estimate_block(b, fs, rs.indent, inp.col_w, gap);
            rs
        })
        .collect()
}

/// Rough height of a Markdown block: text lines from its length and the column width (the real height replaces it as
/// soon as the run is shown).
fn estimate_block(b: &Block, fs: f32, indent: f32, col_w: f32, gap: f32) -> f32 {
    let per_line = ((col_w - indent).max(120.0) / (fs * 0.52)).max(10.0);
    let lines = |chars: usize| (chars as f32 / per_line).ceil().max(1.0);
    let body = match b.kind {
        BlockKind::Paragraph => lines(b.plain().chars().count()) * fs * 1.75,
        BlockKind::Heading => lines(b.plain().chars().count()) * fs * 1.25,
        BlockKind::Code => b.code.lines().count().max(1) as f32 * fs * 1.5 + 2.0 * fs,
        BlockKind::Table => b.rows.len() as f32 * (fs * 1.6 + 16.0),
        BlockKind::Rule => 2.0,
        BlockKind::Image => 240.0,
    };
    gap + body
}

/// A row for the window (UI thread: makes the table models and takes the image).
fn spec_to_block(s: RowSpec, images: &HashMap<String, Option<Image>>) -> ReaderBlock {
    let mut rb = ReaderBlock {
        kind: s.kind,
        text: s.text,
        plain: s.plain,
        level: s.level,
        font_size: s.font_size,
        gap: s.gap,
        indent: s.indent,
        marker: s.marker,
        task: s.task,
        task_index: s.task_index,
        quote: s.quote,
        quote_first: s.quote_first,
        quote_last: s.quote_last,
        alt: s.alt,
        sel_start: s.sel_start,
        sel_end: s.sel_end,
        lh: s.lh,
        code_styled: s.code_styled,
        ..Default::default()
    };
    if s.kind == 4 {
        rb.col_w = ModelRc::new(VecModel::from(s.col_w));
        let rows: Vec<ReaderRow> = s
            .rows
            .into_iter()
            .map(|(cells, header, even)| ReaderRow { cells: ModelRc::new(VecModel::from(cells)), header, even })
            .collect();
        rb.rows = ModelRc::new(VecModel::from(rows));
    }
    if s.kind == 5 {
        if let Some(Some(img)) = images.get(&s.img_src) {
            rb.img = img.img.clone();
            rb.has_img = true;
            rb.img_w = img.w;
            rb.img_h = img.h;
        }
    }
    rb
}

/// Build the page rows again (load, font size, theme, search marks, a task ticked, an image arrived). Small documents
/// are built at once (on all cores, waited for); big ones on all cores in the background, so no window waits for that.
fn rebuild(rd: &Reader) {
    let inp = row_inputs(rd);
    let gen = rd.build_gen.get() + 1;
    rd.build_gen.set(gen);
    let count = if inp.markdown { inp.blocks.len() } else { inp.chunks.len() };
    if count <= ASYNC_ROWS {
        let specs = make_rows(&inp);
        let est: Vec<f32> = specs.iter().map(|s| s.est).collect();
        let rows: Vec<ReaderBlock> = specs.into_iter().map(|s| spec_to_block(s, &rd.images)).collect();
        apply_rows(rd, rows, est);
        return;
    }
    let weak = rd.window.as_weak();
    std::thread::spawn(move || {
        let specs = make_rows(&inp);
        let _ = slint::invoke_from_event_loop(move || apply_built(weak, gen, specs));
    });
}

/// From this many rows on, the page is virtual: only the runs near the view get their blocks.
const VIRTUAL_MIN: usize = CHUNK * 64;

/// The virtual page of a big document. Slint gets only the runs near the view (`a..b`); the rest of the page is two empty
/// spaces of the runs' heights above and below them. Opening and scrolling an 8 MB document so no longer makes Slint
/// measure all its text (measured: over a minute of a frozen UI thread in a debug build), and a scroll step no longer
/// lays out thousands of empty runs either.
#[derive(Default)]
struct Virtual {
    on: bool,
    /// font size, font, column width (8 px steps): measured heights belong to these
    key: (usize, usize, i32),
    rows: Vec<Vec<ReaderBlock>>,
    /// estimated, or measured once the run was shown
    heights: Vec<f32>,
    /// the runs in the window's model
    a: usize,
    b: usize,
    /// the run of the search match the view was last brought to
    revealed: Option<usize>,
}

fn chunk_row(v: &Virtual, c: usize) -> ReaderChunk {
    ReaderChunk {
        first: (c * CHUNK) as i32,
        est_h: v.heights[c].max(1.0),
        blocks: ModelRc::new(VecModel::from(v.rows[c].clone())),
    }
}

/// Top of the view in column coordinates (the column starts below the page padding).
fn view_top(rd: &Reader) -> f32 {
    rd.window.get_view_top() - rd.window.get_page_pad()
}

/// Put the runs from one screen above the view to two below into the window; a search match outside them then moves
/// the view there (its reveal element places it exactly once its run is drawn).
fn update_live(rd: &Reader) {
    if !rd.virt.borrow().on || rd.virt.borrow().heights.is_empty() {
        return;
    }
    let vh = rd.window.get_view_height().max(200.0);
    fill_window(rd, vh);
    let cur = rd.current.and_then(|i| rd.hits.get(i)).map(|h| h.block / CHUNK);
    let target = {
        let mut v = rd.virt.borrow_mut();
        if cur == v.revealed {
            return;
        }
        v.revealed = cur;
        // the spaces are set first: the page has its full height, so the scroll is not cut short
        cur.filter(|&c| c < v.a || c >= v.b).map(|c| v.heights[..c].iter().sum::<f32>())
    };
    if let Some(y) = target {
        rd.window.invoke_scroll_to(y + rd.window.get_page_pad() - vh / 3.0, false);
        fill_window(rd, vh);
    }
}

fn fill_window(rd: &Reader, vh: f32) {
    let mut v = rd.virt.borrow_mut();
    let top = view_top(rd);
    let (lo, hi) = (top - vh, top + 2.0 * vh);
    let n = v.heights.len();
    let (mut a, mut b, mut y, mut above) = (n - 1, n, 0.0f32, 0.0f32);
    let mut found = false;
    for c in 0..n {
        let h = v.heights[c];
        if !found && (y + h >= lo || c == n - 1) {
            (a, above, found) = (c, y, true);
        }
        if found && y > hi {
            b = c;
            break;
        }
        y += h;
    }
    let below: f32 = v.heights[b..].iter().sum();
    if (a, b) != (v.a, v.b) {
        let model = rd.window.get_chunks();
        let vm = model.as_any().downcast_ref::<VecModel<ReaderChunk>>();
        match vm {
            // the window moved a little: runs leave at one end and come in at the other, the rest stays drawn
            Some(vm) if a < v.b && v.a < b => {
                for _ in b..v.b {
                    vm.remove(vm.row_count() - 1);
                }
                for _ in v.a..a {
                    vm.remove(0);
                }
                for c in (a..v.a).rev() {
                    vm.insert(0, chunk_row(&v, c));
                }
                for c in v.b..b {
                    vm.push(chunk_row(&v, c));
                }
            }
            _ => {
                let rows: Vec<ReaderChunk> = (a..b).map(|c| chunk_row(&v, c)).collect();
                rd.window.set_chunks(ModelRc::new(VecModel::from(rows)));
            }
        }
        (v.a, v.b) = (a, b);
    }
    rd.window.set_virt_above(above);
    rd.window.set_virt_below(below);
}

/// A run that just got drawn has its real height: remember it, and when the run is above the view, scroll by the
/// difference so the text being read stays where it was.
fn chunk_measured(rd: &Reader, first: i32, h: f32) {
    let mut v = rd.virt.borrow_mut();
    let c = first.max(0) as usize / CHUNK;
    if !v.on || c >= v.heights.len() || c < v.a || c >= v.b {
        return;
    }
    let old = v.heights[c];
    if (h - old).abs() < 0.5 {
        return;
    }
    v.heights[c] = h;
    let y: f32 = v.heights[..c].iter().sum();
    drop(v);
    if y + old <= view_top(rd) {
        rd.window.invoke_scroll_by(h - old, false);
    }
}

/// Rows into the window: changed rows in place when the shape stayed (a task ticked, search marks, theme), else a new
/// page. Big documents get the virtual page.
fn apply_rows(rd: &Reader, out: Vec<ReaderBlock>, est: Vec<f32>) {
    rd.window.set_status("".into());
    if out.len() >= VIRTUAL_MIN {
        apply_virtual(rd, out, est);
        return;
    }
    rd.virt.borrow_mut().on = false;
    rd.window.set_virt_above(0.0);
    rd.window.set_virt_below(0.0);
    let model = rd.window.get_chunks();
    let same_shape = model.row_count() == out.len().div_ceil(CHUNK)
        && (0..model.row_count()).all(|c| model.row_data(c).is_some_and(|ch| ch.blocks.row_count() == out[c * CHUNK..].len().min(CHUNK)));
    if same_shape && !out.is_empty() {
        for (c, part) in out.chunks(CHUNK).enumerate() {
            let Some(ch) = model.row_data(c) else { continue };
            for (j, b) in part.iter().enumerate() {
                if ch.blocks.row_data(j).as_ref() != Some(b) {
                    ch.blocks.set_row_data(j, b.clone());
                }
            }
        }
        return;
    }
    let chunks: Vec<ReaderChunk> = out
        .chunks(CHUNK)
        .enumerate()
        .map(|(c, part)| ReaderChunk { first: (c * CHUNK) as i32, blocks: ModelRc::new(VecModel::from(part.to_vec())), est_h: 0.0 })
        .collect();
    rd.window.set_chunks(ModelRc::new(VecModel::from(chunks)));
}

fn apply_virtual(rd: &Reader, out: Vec<ReaderBlock>, est: Vec<f32>) {
    let key = (rd.size, rd.font, (rd.window.get_column_w() / 8.0).round() as i32);
    let mut rows: Vec<Vec<ReaderBlock>> = Vec::with_capacity(out.len().div_ceil(CHUNK));
    let mut it = out.into_iter();
    loop {
        let part: Vec<ReaderBlock> = it.by_ref().take(CHUNK).collect();
        if part.is_empty() {
            break;
        }
        rows.push(part);
    }
    let n = rows.len();
    let mut v = rd.virt.borrow_mut();
    let same = v.on && v.key == key && v.rows.len() == n && v.rows.iter().zip(&rows).all(|(a, b)| a.len() == b.len());
    if same {
        // the same page (search marks, theme, a task ticked): only changed rows, drawn runs updated in place
        let model = rd.window.get_chunks();
        for (c, new) in rows.into_iter().enumerate() {
            if v.rows[c] == new {
                continue;
            }
            if c >= v.a && c < v.b {
                if let Some(ch) = model.row_data(c - v.a) {
                    for (j, b) in new.iter().enumerate() {
                        if ch.blocks.row_data(j).as_ref() != Some(b) {
                            ch.blocks.set_row_data(j, b.clone());
                        }
                    }
                }
            }
            v.rows[c] = new;
        }
    } else {
        let heights: Vec<f32> = est.chunks(CHUNK).map(|p| p.iter().sum()).collect();
        // a new page: the window is filled by update_live below (a, b = 0 means nothing drawn yet)
        *v = Virtual { on: true, key, rows, heights, ..Default::default() };
        rd.window.set_chunks(ModelRc::new(VecModel::from(Vec::<ReaderChunk>::new())));
    }
    drop(v);
    update_live(rd);
}

/// Rows built in the background into the window, all at once: adding them in slices made Slint lay out the whole
/// growing page again after every slice (measured: far slower than one change). A newer rebuild makes this one stop.
fn apply_built(weak: slint::Weak<ReaderWindow>, gen: u64, specs: Vec<RowSpec>) {
    let Some(w) = weak.upgrade() else { return };
    let Some(r) = find(&w) else { return };
    let rd = r.borrow();
    if rd.build_gen.get() != gen {
        return;
    }
    let est: Vec<f32> = specs.iter().map(|s| s.est).collect();
    let rows: Vec<ReaderBlock> = specs.into_iter().map(|s| spec_to_block(s, &rd.images)).collect();
    apply_rows(&rd, rows, est);
    if rd.current.is_some() {
        show_find_state(&rd);
    }
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
        // and never an address on this computer or its local network (a shared document must not probe the LAN)
        let Some(abs) = base
            .as_ref()
            .and_then(|b| b.join(&src).ok())
            .filter(|u| {
                let web = matches!(u.scheme(), "http" | "https")
                    && (!tvfs_core::hosts::is_local_network(u)
                        || tvfs_core::hosts::TrustedUrl::from_url(u.clone(), tvfs_core::hosts::Allow::AnyPath).is_some());
                web || (local_doc && u.scheme() == "file")
            })
        else {
            continue;
        };
        let (shared2, weak) = (shared.clone(), weak.clone());
        shared.rt.spawn(async move {
            let decoded = async {
                // a few downloads and decodes at a time, each with a size limit (a 24 MB file can decode to gigabytes)
                let _permit = IMAGE_SLOTS.acquire().await.ok()?;
                let bytes = shared2.client.fetch_bytes(abs.as_str(), MAX_IMAGE_BYTES).await.ok()?;
                tokio::task::spawn_blocking(move || {
                    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes[..])).with_guessed_format().ok()?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(16_384);
                    limits.max_image_height = Some(16_384);
                    limits.max_alloc = Some(256 * 1024 * 1024);
                    reader.limits(limits);
                    let img = reader.decode().ok()?.into_rgba8();
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
                // a big document's measured heights belong to the old font
                if rd.virt.borrow().on {
                    rebuild(&rd);
                }
            })
        });
    }
    {
        // scrolling a big document: blocks follow the view (once per event-loop turn)
        let weak_r = Rc::downgrade(r);
        w.on_viewport_changed(move || {
            let Some(r) = weak_r.upgrade() else { return };
            let rd = r.borrow();
            if !rd.virt.borrow().on || rd.live_pending.replace(true) {
                return;
            }
            let weak_r = Rc::downgrade(&r);
            slint::Timer::single_shot(std::time::Duration::ZERO, move || {
                if let Some(r) = weak_r.upgrade() {
                    let rd = r.borrow();
                    rd.live_pending.set(false);
                    update_live(&rd);
                }
            });
        });
    }
    {
        let weak_r = Rc::downgrade(r);
        w.on_chunk_measured(move |first, h| {
            if let Some(r) = weak_r.upgrade() {
                if let Ok(rd) = r.try_borrow() {
                    chunk_measured(&rd, first, h);
                }
            }
        });
    }
    {
        // the column width changed (window resized): a big document's heights are estimated again, a moment later
        let weak_r = Rc::downgrade(r);
        let timer = Rc::new(slint::Timer::default());
        w.on_layout_changed(move || {
            let weak_r = weak_r.clone();
            timer.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(200), move || {
                if let Some(r) = weak_r.upgrade() {
                    let rd = r.borrow();
                    if rd.virt.borrow().on {
                        rebuild(&rd);
                    }
                }
            });
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
