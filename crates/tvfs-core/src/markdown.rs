//! Markdown -> blocks for the native reader. Parsing follows the web reader (`marked` with
//! `gfm: true, breaks: true`): GitHub tables, task lists, strikethrough, and every line break is
//! a break. Inline text is kept as styled spans; the UI turns them into Slint `StyledText` markup
//! (`to_markup`), which only knows bold / italic / strikethrough / code / links / underline / colour.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub const STRONG: u8 = 1;
pub const EM: u8 = 2;
pub const STRIKE: u8 = 4;
pub const CODE: u8 = 8;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    pub style: u8,
    pub link: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlockKind {
    #[default]
    Paragraph,
    Heading,
    /// A fenced or indented code block (`code` holds the raw text).
    Code,
    Rule,
    Table,
    Image,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block {
    pub kind: BlockKind,
    pub spans: Vec<Span>,
    /// Heading level 1-6.
    pub level: u8,
    /// List nesting depth (0 = not in a list).
    pub indent: u8,
    /// Blockquote depth.
    pub quote: u8,
    /// "•", "◦", "▪", "3." for the first block of a list item, else empty.
    pub marker: String,
    /// Task list item: checked or not (as written in the file).
    pub task: Option<bool>,
    /// Running number of the task item in the document (for remembering ticks).
    pub task_index: usize,
    pub code: String,
    pub lang: String,
    /// Table rows (the first one is the header), each cell as spans.
    pub rows: Vec<Vec<Vec<Span>>>,
    /// Image source and alt text.
    pub src: String,
    pub alt: String,
    /// A later line of a paragraph (`breaks: true`): no gap above, no marker. Paragraphs are split per
    /// line so each line gets the web's line height (Slint text has no line-height).
    pub cont: bool,
}

impl Block {
    /// The visible text (search, copy).
    pub fn plain(&self) -> String {
        match self.kind {
            BlockKind::Code => self.code.clone(),
            BlockKind::Table => self
                .rows
                .iter()
                .map(|r| r.iter().map(|c| spans_plain(c)).collect::<Vec<_>>().join("\t"))
                .collect::<Vec<_>>()
                .join("\n"),
            BlockKind::Image => self.alt.clone(),
            BlockKind::Rule => String::new(),
            _ => spans_plain(&self.spans),
        }
    }
}

pub fn spans_plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    /// The block being filled with inline content.
    cur: Option<Block>,
    style: Vec<u8>,
    link: Vec<String>,
    quote: u8,
    /// Per open list: next number (ordered) or None (bullets).
    lists: Vec<Option<u64>>,
    /// The next paragraph opens a list item (gets the marker / task box).
    item_start: Option<(String, Option<bool>)>,
    tasks: usize,
    table: Option<Block>,
    row: Vec<Vec<Span>>,
    cell: Option<Vec<Span>>,
    image: Option<(String, String)>,
    in_code: bool,
}

impl Builder {
    fn style_bits(&self) -> u8 {
        self.style.iter().fold(0, |a, b| a | b)
    }

    fn open(&mut self, kind: BlockKind) {
        self.close();
        let mut b = Block { kind, quote: self.quote, indent: self.lists.len() as u8, ..Default::default() };
        if let Some((marker, task)) = self.item_start.take() {
            b.marker = marker;
            if task.is_some() {
                b.task = task;
                b.task_index = self.tasks;
                self.tasks += 1;
            }
        }
        self.cur = Some(b);
    }

    fn close(&mut self) {
        if let Some(b) = self.cur.take() {
            let empty = b.kind == BlockKind::Paragraph && b.spans.iter().all(|s| s.text.trim().is_empty()) && b.task.is_none() && b.marker.is_empty();
            if !empty {
                self.blocks.push(b);
            }
        }
    }

    fn push_text(&mut self, text: &str, extra: u8) {
        let span = Span { text: text.to_owned(), style: self.style_bits() | extra, link: self.link.last().cloned() };
        if let Some(cell) = self.cell.as_mut() {
            push_span(cell, span);
            return;
        }
        if self.cur.is_none() {
            // inline content outside a paragraph (tight list items)
            self.open(BlockKind::Paragraph);
        }
        if let Some(b) = self.cur.as_mut() {
            push_span(&mut b.spans, span);
        }
    }

    fn marker(&self) -> String {
        match self.lists.last() {
            Some(Some(n)) => format!("{n}."),
            _ => match self.lists.len().saturating_sub(1) % 3 {
                0 => "•".into(),
                1 => "◦".into(),
                _ => "▪".into(),
            },
        }
    }
}

/// Merge with the previous span when the style is the same.
fn push_span(spans: &mut Vec<Span>, span: Span) {
    if let Some(last) = spans.last_mut() {
        if last.style == span.style && last.link == span.link {
            last.text.push_str(&span.text);
            return;
        }
    }
    spans.push(span);
}

pub fn parse(md: &str) -> Vec<Block> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_GFM;
    let mut b = Builder::default();
    for ev in Parser::new_ext(md, opts) {
        match ev {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    if b.cur.as_ref().is_some_and(|c| c.kind == BlockKind::Paragraph && c.spans.is_empty() && (!c.marker.is_empty() || c.task.is_some())) {
                        // a list item that already opened its block (task marker came first)
                    } else {
                        b.open(BlockKind::Paragraph);
                    }
                }
                Tag::Heading { level, .. } => {
                    b.open(BlockKind::Heading);
                    if let Some(c) = b.cur.as_mut() {
                        c.level = heading_level(level);
                    }
                }
                Tag::BlockQuote(_) => {
                    b.close();
                    b.quote += 1;
                }
                Tag::CodeBlock(kind) => {
                    b.open(BlockKind::Code);
                    b.in_code = true;
                    if let (Some(c), CodeBlockKind::Fenced(lang)) = (b.cur.as_mut(), kind) {
                        c.lang = lang.split_whitespace().next().unwrap_or("").to_owned();
                    }
                }
                Tag::List(start) => {
                    b.close();
                    b.lists.push(start);
                }
                Tag::Item => {
                    b.close();
                    let m = b.marker();
                    if let Some(Some(n)) = b.lists.last_mut() {
                        *n += 1;
                    }
                    b.item_start = Some((m, None));
                }
                Tag::Emphasis => b.style.push(EM),
                Tag::Strong => b.style.push(STRONG),
                Tag::Strikethrough => b.style.push(STRIKE),
                Tag::Link { dest_url, .. } => b.link.push(dest_url.into_string()),
                Tag::Image { dest_url, .. } => b.image = Some((dest_url.into_string(), String::new())),
                Tag::Table(_) => {
                    b.close();
                    b.table = Some(Block { kind: BlockKind::Table, quote: b.quote, indent: b.lists.len() as u8, ..Default::default() });
                }
                Tag::TableHead | Tag::TableRow => b.row.clear(),
                Tag::TableCell => b.cell = Some(Vec::new()),
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) => b.close(),
                TagEnd::CodeBlock => {
                    b.in_code = false;
                    if let Some(c) = b.cur.as_mut() {
                        if c.code.ends_with('\n') {
                            c.code.pop();
                        }
                    }
                    b.close();
                }
                TagEnd::BlockQuote(_) => {
                    b.close();
                    b.quote = b.quote.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    b.close();
                    b.lists.pop();
                }
                TagEnd::Item => {
                    // an empty item ("- ") still shows its marker
                    if let Some((marker, task)) = b.item_start.take() {
                        b.item_start = Some((marker, task));
                        b.open(BlockKind::Paragraph);
                    }
                    b.close();
                }
                TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                    b.style.pop();
                }
                TagEnd::Link => {
                    b.link.pop();
                }
                TagEnd::Image => {
                    if let Some((src, alt)) = b.image.take() {
                        if b.cell.is_some() {
                            b.push_text(&alt, 0);
                        } else {
                            // images get a block of their own, after the text they sit in
                            let pending = b.cur.take();
                            let had_text = pending.as_ref().is_some_and(|p| !spans_plain(&p.spans).trim().is_empty() || !p.marker.is_empty());
                            if let Some(p) = pending {
                                b.cur = Some(p);
                            }
                            let img = Block { kind: BlockKind::Image, src, alt, quote: b.quote, indent: b.lists.len() as u8, ..Default::default() };
                            if had_text {
                                b.close();
                            } else {
                                b.cur = None;
                            }
                            b.blocks.push(img);
                        }
                    }
                }
                TagEnd::TableCell => {
                    if let Some(cell) = b.cell.take() {
                        b.row.push(cell);
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    let row = std::mem::take(&mut b.row);
                    if let Some(t) = b.table.as_mut() {
                        t.rows.push(row);
                    }
                }
                TagEnd::Table => {
                    if let Some(t) = b.table.take() {
                        b.blocks.push(t);
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(img) = b.image.as_mut() {
                    img.1.push_str(&t);
                } else if b.in_code {
                    if let Some(c) = b.cur.as_mut() {
                        c.code.push_str(&t);
                    }
                } else {
                    b.push_text(&t, 0);
                }
            }
            Event::Code(t) => {
                if let Some(img) = b.image.as_mut() {
                    img.1.push_str(&t);
                } else {
                    b.push_text(&t, CODE);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(img) = b.image.as_mut() {
                    img.1.push(' ');
                } else {
                    // `breaks: true`: every newline is a line break
                    b.push_text("\n", 0);
                }
            }
            Event::Html(h) | Event::InlineHtml(h) => {
                let tag = h.trim().to_ascii_lowercase();
                if tag.starts_with("<br") {
                    b.push_text("\n", 0);
                }
                // other raw HTML is not rendered (the web would render it; it is rare in notes)
            }
            Event::Rule => {
                b.close();
                b.blocks.push(Block { kind: BlockKind::Rule, quote: b.quote, ..Default::default() });
            }
            Event::TaskListMarker(done) => {
                if let Some((m, _)) = b.item_start.take() {
                    b.item_start = Some((m, Some(done)));
                    b.open(BlockKind::Paragraph);
                } else if let Some(c) = b.cur.as_mut() {
                    c.task = Some(done);
                    c.task_index = b.tasks;
                    b.tasks += 1;
                }
            }
            Event::FootnoteReference(r) => b.push_text(&format!("[{r}]"), 0),
            Event::InlineMath(t) | Event::DisplayMath(t) => b.push_text(&t, CODE),
        }
    }
    b.close();
    split_lines(b.blocks)
}

/// One block per line of a paragraph.
fn split_lines(blocks: Vec<Block>) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    for b in blocks {
        if b.kind != BlockKind::Paragraph || !b.spans.iter().any(|s| s.text.contains('\n')) {
            out.push(b);
            continue;
        }
        let mut lines: Vec<Vec<Span>> = vec![Vec::new()];
        for s in &b.spans {
            for (i, part) in s.text.split('\n').enumerate() {
                if i > 0 {
                    lines.push(Vec::new());
                }
                if !part.is_empty() {
                    lines.last_mut().unwrap().push(Span { text: part.to_owned(), style: s.style, link: s.link.clone() });
                }
            }
        }
        for (i, spans) in lines.into_iter().enumerate() {
            let mut line = Block { spans, cont: i > 0, ..b.clone() };
            if i > 0 {
                line.marker.clear();
                line.task = None;
            }
            out.push(line);
        }
    }
    out
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// Text made literal for Slint's markup parser (CommonMark + `<u>` / `<font color>`):
/// ASCII punctuation is backslash-escaped, lines lose their leading blanks (they would become
/// code blocks), the private placeholder character Slint uses for interpolation is dropped.
fn escape(text: &str, out: &mut String, mut line_start: bool) {
    for ch in text.chars() {
        match ch {
            '\u{e541}' | '\r' => {}
            '\n' => {
                out.push('\n');
                line_start = true;
            }
            ' ' | '\t' if line_start => {
                // keep the indentation visible without starting a code block
                out.push('\u{00a0}');
            }
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
                line_start = false;
            }
            c => {
                out.push(c);
                line_start = false;
            }
        }
    }
}

/// `color` as `#rrggbb`.
pub type Rgb = u32;

/// A coloured range of the plain text (byte offsets into `spans_plain`), e.g. search matches.
#[derive(Debug, Clone, PartialEq)]
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub color: Rgb,
    pub underline: bool,
}

/// Slint `StyledText` markup for `spans`, with `marks` coloured (and optionally underlined).
/// `gradient` colours every character along a horizontal gradient (the web's headings).
/// Text colours of bold / italic runs (the web: `strong { color: #fff }`, `em { opacity: .85 }`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Colors {
    pub strong: Option<Rgb>,
    pub em: Option<Rgb>,
}

pub fn to_markup(spans: &[Span], marks: &[Mark], gradient: Option<(Rgb, Rgb)>, colors: Colors) -> String {
    // cut the spans at every mark boundary so each piece has one colour
    let total: usize = spans.iter().map(|s| s.text.len()).sum();
    let chars_total = spans.iter().map(|s| s.text.chars().count()).sum::<usize>().max(1);
    let mut out = String::new();
    let mut offset = 0usize;
    let mut char_pos = 0usize;
    for span in spans {
        let mut cuts = vec![0, span.text.len()];
        for m in marks {
            for p in [m.start, m.end] {
                if p > offset && p < offset + span.text.len() && span.text.is_char_boundary(p - offset) {
                    cuts.push(p - offset);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let piece = &span.text[w[0]..w[1]];
            if piece.is_empty() {
                continue;
            }
            let at = offset + w[0];
            let mark = marks.iter().find(|m| m.start <= at && at < m.end);
            match gradient {
                Some((a, b)) if mark.is_none() && span.style & CODE == 0 => {
                    // one colour per character (Slint has no gradient text)
                    let mut styled = String::new();
                    for ch in piece.chars() {
                        let t = char_pos as f32 / (chars_total.saturating_sub(1).max(1)) as f32;
                        char_pos += 1;
                        let mut one = String::new();
                        let at_line_start = (out.is_empty() && styled.is_empty()) || out.ends_with('\n') || styled.ends_with('\n');
                        escape(&ch.to_string(), &mut one, at_line_start);
                        if ch == '\n' || ch.is_whitespace() {
                            styled.push_str(&one);
                        } else {
                            styled.push_str(&format!("<font color=\"#{:06x}\">{one}</font>", lerp_rgb(a, b, t)));
                        }
                    }
                    wrap_style(&mut out, &styled, span, Colors::default());
                }
                _ => {
                    char_pos += piece.chars().count();
                    let mut text = String::new();
                    if span.style & CODE != 0 {
                        text = code_span(piece);
                    } else {
                        escape(piece, &mut text, out.is_empty() || out.ends_with('\n'));
                    }
                    let text = match mark {
                        Some(m) => {
                            let inner = format!("<font color=\"#{:06x}\">{text}</font>", m.color);
                            if m.underline { format!("<u>{inner}</u>") } else { inner }
                        }
                        None => text,
                    };
                    wrap_style(&mut out, &text, span, if mark.is_some() { Colors::default() } else { colors });
                }
            }
        }
        offset += span.text.len();
    }
    debug_assert!(offset == total);
    out
}

fn lerp_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let ch = |c: Rgb, s: u32| ((c >> s) & 0xff) as f32;
    let mix = |s: u32| ((ch(a, s) + (ch(b, s) - ch(a, s)) * t.clamp(0.0, 1.0)).round() as u32) << s;
    mix(16) | mix(8) | mix(0)
}

/// Backtick code span that can hold any text (fence longer than any backtick run inside).
fn code_span(text: &str) -> String {
    let text: String = text.chars().filter(|c| *c != '\u{e541}' && *c != '\n').collect();
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') || text.starts_with(' ') { " " } else { "" };
    format!("{fence}{pad}{text}{pad}{fence}")
}

/// Wrap already-escaped `text` in the span's emphasis / strong / strike / link markers. Markers
/// must touch non-blank text on the inside to count as emphasis, so blanks stay outside.
fn wrap_style(out: &mut String, text: &str, span: &Span, colors: Colors) {
    let core = text.trim_matches(|c: char| c == ' ' || c == '\u{00a0}');
    if core.is_empty() || (span.style & (STRONG | EM | STRIKE) == 0 && span.link.is_none()) {
        out.push_str(text);
        return;
    }
    let lead = &text[..text.len() - text.trim_start_matches([' ', '\u{00a0}']).len()];
    let trail = &text[text.trim_end_matches([' ', '\u{00a0}']).len()..];
    let mut s = core.to_owned();
    // line breaks inside a styled run: style each line separately
    if s.contains('\n') {
        let lines: Vec<String> = s.split('\n').map(|l| {
            let mut one = String::new();
            wrap_style(&mut one, l, span, colors);
            one
        }).collect();
        out.push_str(lead);
        out.push_str(&lines.join("\n"));
        out.push_str(trail);
        return;
    }
    let tint = if span.style & STRONG != 0 { colors.strong } else if span.style & EM != 0 { colors.em } else { None };
    if let Some(c) = tint.filter(|_| span.link.is_none()) {
        s = format!("<font color=\"#{c:06x}\">{s}</font>");
    }
    if span.style & STRIKE != 0 {
        s = format!("~~{s}~~");
    }
    if span.style & EM != 0 {
        s = format!("*{s}*");
    }
    if span.style & STRONG != 0 {
        s = format!("**{s}**");
    }
    if let Some(link) = &span.link {
        let url: String = link.chars().filter(|c| !matches!(c, '<' | '>' | '\n' | '\u{e541}')).collect();
        s = format!("[{s}](<{url}>)");
    }
    out.push_str(lead);
    out.push_str(&s);
    out.push_str(trail);
}

/// Byte ranges of `needle` in `hay`, case-insensitive (Unicode simple case folding by lowercase).
pub fn find_all(hay: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle_l = needle.to_lowercase();
    let mut out = Vec::new();
    // compare on lowercased text but report positions in the original
    let idx: Vec<(usize, char)> = hay.char_indices().collect();
    let lower: Vec<String> = idx.iter().map(|(_, c)| c.to_lowercase().collect()).collect();
    let nchars: Vec<char> = needle_l.chars().collect();
    let mut i = 0;
    while i < idx.len() {
        // match char by char over lowercase expansions
        let mut j = 0;
        let mut k = i;
        let mut ok = true;
        while j < nchars.len() {
            if k >= idx.len() {
                ok = false;
                break;
            }
            for lc in lower[k].chars() {
                if j < nchars.len() && nchars[j] == lc {
                    j += 1;
                } else {
                    ok = false;
                    break;
                }
            }
            if !ok {
                break;
            }
            k += 1;
        }
        if ok && j == nchars.len() {
            let start = idx[i].0;
            let end = if k < idx.len() { idx[k].0 } else { hay.len() };
            out.push((start, end));
            i = k.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_paragraphs_breaks() {
        let b = parse("# Title\n\nline one\nline **two**\n\n---\n");
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].kind, BlockKind::Heading);
        assert_eq!(b[0].level, 1);
        // one block per line; the second continues the paragraph
        assert_eq!(b[1].plain(), "line one");
        assert!(!b[1].cont);
        assert_eq!(b[2].plain(), "line two");
        assert!(b[2].cont);
        assert_eq!(b[2].spans[1], Span { text: "two".into(), style: STRONG, link: None });
        assert_eq!(b[3].kind, BlockKind::Rule);
    }

    #[test]
    fn lists_and_tasks() {
        let b = parse("- a\n  - b\n1. one\n2. two\n\n- [ ] todo\n- [x] done\n");
        let m: Vec<(&str, u8, String)> = b.iter().map(|x| (x.marker.as_str(), x.indent, x.plain())).collect();
        assert_eq!(m[0], ("•", 1, "a".into()));
        assert_eq!(m[1], ("◦", 2, "b".into()));
        assert_eq!(m[2], ("1.", 1, "one".into()));
        assert_eq!(m[3], ("2.", 1, "two".into()));
        assert_eq!(b[4].task, Some(false));
        assert_eq!(b[5].task, Some(true));
        assert_eq!((b[4].task_index, b[5].task_index), (0, 1));
        assert_eq!(b[5].plain(), "done");
    }

    #[test]
    fn code_quote_table_image() {
        let b = parse("> quoted *text*\n\n```rust\nfn x() {}\n```\n\n| a | b |\n|---|---|\n| 1 | **2** |\n\n![alt text](https://x/y.png)\n");
        assert_eq!(b[0].quote, 1);
        assert_eq!(b[0].plain(), "quoted text");
        assert_eq!(b[1].kind, BlockKind::Code);
        assert_eq!(b[1].code, "fn x() {}");
        assert_eq!(b[1].lang, "rust");
        assert_eq!(b[2].kind, BlockKind::Table);
        assert_eq!(b[2].rows.len(), 2);
        assert_eq!(spans_plain(&b[2].rows[1][1]), "2");
        assert_eq!(b[3].kind, BlockKind::Image);
        assert_eq!(b[3].src, "https://x/y.png");
        assert_eq!(b[3].alt, "alt text");
    }

    #[test]
    fn markup_escapes_and_styles() {
        let spans = vec![
            Span { text: "1. *not* a list ".into(), style: 0, link: None },
            Span { text: "bold".into(), style: STRONG, link: None },
            Span { text: " a`b ".into(), style: CODE, link: None },
            Span { text: "site".into(), style: 0, link: Some("https://e.x/a b".into()) },
        ];
        let m = to_markup(&spans, &[], None, Colors::default());
        // CommonMark strips one blank on each side of a padded code span, so " a`b " survives intact
        assert_eq!(m, "1\\. \\*not\\* a list **bold**``  a`b  ``[site](<https://e.x/a b>)");
    }

    #[test]
    fn marks_split_spans() {
        let spans = vec![Span { text: "find me here".into(), style: 0, link: None }];
        let m = to_markup(&spans, &[Mark { start: 5, end: 7, color: 0xff0000, underline: false }], None, Colors::default());
        assert_eq!(m, "find <font color=\"#ff0000\">me</font> here");
    }

    #[test]
    fn search_is_case_insensitive() {
        assert_eq!(find_all("Hello hello HÉLLO", "héllo"), vec![(12, 18)]);
        assert_eq!(find_all("aaa", "aa"), vec![(0, 2)]);
        assert_eq!(find_all("Hello hello", "HELLO"), vec![(0, 5), (6, 11)]);
    }
}
