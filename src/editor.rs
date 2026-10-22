//! Plain-text editor on top of DirectWrite. The text lives in one UTF-16 buffer (the unit
//! DirectWrite counts positions in) and is laid out paragraph by paragraph: an edit only
//! re-wraps the paragraph it touches, and only visible paragraphs are drawn. That keeps
//! typing fast no matter how long the note is.

use std::time::{Duration, Instant};
use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;

use crate::gfx::{rect, Color, Gfx};

const NEWLINE: u16 = 10;
const MAX_UNDO: usize = 300;
/// Layouts are kept for this many paragraphs around the visible ones; the rest only keep their height.
const KEEP_AROUND_VIEW: usize = 40;
/// With a fixed line height, the text baseline sits at this fraction of it.
pub const BASELINE: f32 = 0.8;
/// Font size factor of Markdown headings, level 1 to 6.
const HEADING_SCALE: [f32; 6] = [1.5, 1.3, 1.15, 1.0, 1.0, 1.0];
/// Font size that makes the ** of bold text vanish (DirectWrite does not accept zero).
const HIDDEN_SIZE: f32 = 0.01;

/// Consecutive edits of the same kind are merged into one undo step.
#[derive(Clone, Copy, PartialEq)]
pub enum Edit {
    None,
    Type,
    Delete,
    Other,
}

struct Snapshot {
    text: Vec<u16>,
    caret: usize,
    anchor: usize,
}

/// One paragraph: the text between two line breaks (the break itself is not part of it).
struct Para {
    start: usize,
    len: usize,
    layout: Option<IDWriteTextLayout>,
    /// Height of the wrapped paragraph; None until it has been laid out at the current width.
    height: Option<f32>,
    /// Web addresses (start, end) relative to the paragraph; filled together with `layout`.
    links: Vec<(usize, usize)>,
    /// Position of the "[" of a Markdown checkbox in this paragraph; filled together with `layout`.
    checkbox: Option<usize>,
    /// Styled spans with their syntax characters, relative to the paragraph; kept with `height`.
    spans: Vec<Span>,
    /// One bit per span whose syntax characters were visible when the paragraph was last laid out.
    shown: u64,
}

impl Para {
    fn new(start: usize, len: usize) -> Self {
        Self { start, len, layout: None, height: None, links: Vec::new(), checkbox: None, spans: Vec::new(), shown: 0 }
    }
}

/// A styled stretch of a paragraph including its syntax characters: `open` of them at the
/// start ("# ", "**", "`") and `close` at the end. They hide while the selection is elsewhere.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    from: usize,
    to: usize,
    open: usize,
    close: usize,
}

/// What the light Markdown styling found in one paragraph (all positions relative to it).
#[derive(Default, Debug, PartialEq)]
struct Markdown {
    /// Heading level 1–6, or 0.
    heading: usize,
    /// "[" of a checklist box and whether it is ticked.
    checkbox: Option<(usize, bool)>,
    bold: Vec<(usize, usize)>,
    code: Vec<(usize, usize)>,
    /// Syntax characters (#, **, `) that are shown dimmed.
    markers: Vec<(usize, usize)>,
    /// Heading, bold and code with their syntax characters, in the order they were found.
    spans: Vec<Span>,
}

/// For a paragraph that is a list item ("- text", "* text", "- [ ] task", optionally
/// indented): where its text starts, and the marker a following item begins with
/// (a new task always starts unticked).
fn list_prefix(text: &[u16]) -> Option<(usize, Vec<u16>)> {
    let at = |i: usize| text.get(i).copied().unwrap_or(0);
    let indent = text.iter().take_while(|&&c| c == 0x20 || c == 9).count();
    if !matches!(at(indent), 0x2D | 0x2A) || at(indent + 1) != 0x20 {
        return None;
    }
    let mut next: Vec<u16> = text[..indent + 2].to_vec();
    let mut content = indent + 2;
    if at(content) == 0x5B && matches!(at(content + 1), 0x20 | 0x78 | 0x58) && at(content + 2) == 0x5D && at(content + 3) == 0x20 {
        next.extend([0x5B, 0x20, 0x5D, 0x20]);
        content += 4;
    }
    Some((content, next))
}

/// Which spans (relative to a paragraph at `start`) the selection touches, one bit per span.
/// Those show their syntax characters so they can be edited; the others hide them.
/// Only the first 64 spans of a paragraph take part, any further ones always show theirs.
fn touched(spans: &[Span], start: usize, selection: (usize, usize)) -> u64 {
    let mut bits = 0;
    for (i, span) in spans.iter().take(64).enumerate() {
        if selection.0 <= start + span.to && selection.1 >= start + span.from {
            bits |= 1 << i;
        }
    }
    bits
}

/// Recognises "# heading", "- [ ] task", **bold** and `code`. Deliberately no single-asterisk
/// italics: in ordinary notes a lone * is usually something else ("64*64").
fn parse_markdown(text: &[u16]) -> Markdown {
    const HASH: u16 = 0x23;
    const SPACE: u16 = 0x20;
    const STAR: u16 = 0x2A;
    const TICK: u16 = 0x60;
    let mut md = Markdown::default();
    let at = |i: usize| text.get(i).copied().unwrap_or(0);

    let hashes = text.iter().take_while(|&&c| c == HASH).count();
    if (1..=6).contains(&hashes) && at(hashes) == SPACE {
        md.heading = hashes;
        md.markers.push((0, hashes + 1));
        // A heading without text keeps its #, otherwise the line would look empty.
        if text.len() > hashes + 1 {
            md.spans.push(Span { from: 0, to: text.len(), open: hashes + 1, close: 0 });
        }
    }

    // "- [ ] " or "* [x] ", optionally indented
    let indent = text.iter().take_while(|&&c| c == SPACE || c == 9).count();
    if matches!(at(indent), 0x2D | STAR) && at(indent + 1) == SPACE && at(indent + 2) == 0x5B && at(indent + 4) == 0x5D && at(indent + 5) == SPACE {
        match at(indent + 3) {
            SPACE => md.checkbox = Some((indent + 2, false)),
            0x78 | 0x58 => md.checkbox = Some((indent + 2, true)),
            _ => {}
        }
        if md.checkbox.is_some() {
            // The box stands for the list marker, so "- " hides. The span ends at "]": the caret
            // behind the box (where a task is typed) must not bring the marker back.
            md.spans.push(Span { from: indent, to: indent + 5, open: 2, close: 0 });
        }
    }

    let mut i = if md.heading > 0 { hashes + 1 } else { 0 };
    while i < text.len() {
        if text[i] == TICK {
            if let Some(end) = (i + 1..text.len()).find(|&j| text[j] == TICK) {
                if end > i + 1 {
                    md.code.push((i + 1, end));
                    md.markers.push((i, i + 1));
                    md.markers.push((end, end + 1));
                    md.spans.push(Span { from: i, to: end + 1, open: 1, close: 1 });
                }
                i = end + 1;
                continue;
            }
        } else if text[i] == STAR && at(i + 1) == STAR && at(i + 2) != SPACE && at(i + 2) != STAR && i + 2 < text.len() {
            // closing ** must directly follow a non-space
            let close = (i + 3..text.len().saturating_sub(1)).find(|&j| text[j] == STAR && text[j + 1] == STAR && text[j - 1] != SPACE);
            if let Some(end) = close {
                md.bold.push((i + 2, end));
                md.markers.push((i, i + 2));
                md.markers.push((end, end + 2));
                md.spans.push(Span { from: i, to: end + 2, open: 2, close: 2 });
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    md
}

pub struct Editor {
    pub text: Vec<u16>,
    /// Caret and selection anchor as UTF-16 offsets; the selection is the range between them.
    pub caret: usize,
    pub anchor: usize,
    pub scroll: f32,
    dwrite: IDWriteFactory,
    format: IDWriteTextFormat,
    width: f32,
    paras: Vec<Para>,
    /// Top edge of every paragraph (running sum of heights) and the total height; rebuilt on demand.
    tops: Vec<f32>,
    total_height: f32,
    tops_valid: bool,
    /// Column the caret tries to keep while moving up/down through shorter lines.
    desired_x: Option<f32>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Edit,
    last_edit_time: Instant,
    /// Brushes for links/code and for dimmed text; their colours follow the theme without a new layout.
    link_brush: Option<ID2D1SolidColorBrush>,
    muted_brush: Option<ID2D1SolidColorBrush>,
    /// Line height as a multiple of the font size (0 = the font's own), and whether Markdown is styled.
    line_spacing: f32,
    markdown: bool,
    /// Extra ranges to mark (search hits), as (start, end), sorted by position.
    pub highlights: Vec<(usize, usize)>,
    /// A closing bracket offered after a typed opening one: (position, character). It is only
    /// drawn, dimmed, and becomes text once something is typed between the pair.
    ghost: Option<(usize, u16)>,
    /// Positions of the closing brackets the editor added itself; typing one there steps over it.
    closers: Vec<usize>,
}

/// The closing partner of a bracket or quote that is completed automatically.
fn closer_of(c: u16) -> Option<u16> {
    match c {
        0x28 => Some(0x29), // ( )
        0x5B => Some(0x5D), // [ ]
        0x7B => Some(0x7D), // { }
        0x3C => Some(0x3E), // < >
        0x22 => Some(0x22), // " "
        _ => None,
    }
}

/// Splits text into paragraphs as (start, length). There is always at least one; a trailing
/// line break yields a final empty paragraph (the empty line the caret can sit on).
fn split_paragraphs(text: &[u16]) -> Vec<(usize, usize)> {
    let mut paragraphs = Vec::new();
    let mut start = 0;
    for (i, &c) in text.iter().enumerate() {
        if c == NEWLINE {
            paragraphs.push((start, i - start));
            start = i + 1;
        }
    }
    paragraphs.push((start, text.len() - start));
    paragraphs
}

/// After replacing `removed` units at `at` by `inserted` units: how many paragraphs at the
/// front and at the back are untouched and can keep their cached layout. Returns (0, 0)
/// whenever the old and new lists do not line up exactly, so a wrong reuse is impossible.
fn reusable(old: &[(usize, usize)], new: &[(usize, usize)], at: usize, removed: usize, inserted: usize) -> (usize, usize) {
    // A paragraph is untouched in front if it ends (line break included) before the edit …
    let prefix = old.iter().take_while(|(start, len)| start + len < at).count();
    // … and at the back if it starts after the removed range, with its preceding break intact.
    let suffix = old.iter().rev().take_while(|(start, _)| *start > at + removed).count();
    if prefix + suffix > old.len() || prefix + suffix > new.len() {
        return (0, 0);
    }
    let shift = inserted as isize - removed as isize;
    let front_matches = (0..prefix).all(|i| old[i] == new[i]);
    let back_matches = (1..=suffix).all(|k| {
        let (o, n) = (old[old.len() - k], new[new.len() - k]);
        o.0 as isize + shift == n.0 as isize && o.1 == n.1
    });
    if front_matches && back_matches { (prefix, suffix) } else { (0, 0) }
}

/// Finds http://, https:// and www. addresses.
fn find_links(text: &[u16]) -> Vec<(usize, usize)> {
    let starts_with = |at: usize, prefix: &str| {
        prefix.bytes().enumerate().all(|(i, b)| text.get(at + i).map(|&c| c == b as u16 || c == b.to_ascii_uppercase() as u16) == Some(true))
    };
    let is_space = |c: u16| matches!(c, 9 | 10 | 13 | 32 | 0xA0);
    let mut links = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let at_word_start = i == 0 || is_space(text[i - 1]) || matches!(text[i - 1], 0x28 | 0x5B | 0x3C | 0x22 | 0x27);
        let prefix = ["https://", "http://", "www."].into_iter().find(|p| at_word_start && starts_with(i, p));
        let Some(prefix) = prefix else {
            i += 1;
            continue;
        };
        let mut end = i;
        while end < text.len() && !is_space(text[end]) {
            end += 1;
        }
        // Punctuation right after an address belongs to the sentence, not the address.
        while end > i && matches!(text[end - 1], 0x2E | 0x2C | 0x3B | 0x3A | 0x21 | 0x3F | 0x29 | 0x5D | 0x7D | 0x3E | 0x22 | 0x27) {
            end -= 1;
        }
        if end > i + prefix.len() + 1 {
            links.push((i, end));
        }
        i = end.max(i + 1);
    }
    links
}

#[derive(Clone, Copy, PartialEq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn class_of(c: u16) -> CharClass {
    match c {
        9 | 10 | 13 | 32 | 0xA0 => CharClass::Space,
        0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x5F => CharClass::Word,
        c if c > 127 => CharClass::Word,
        _ => CharClass::Punctuation,
    }
}

fn is_low_surrogate(c: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&c)
}

impl Editor {
    pub fn new(dwrite: IDWriteFactory, format: IDWriteTextFormat, text: &str) -> Self {
        let text: Vec<u16> = text.encode_utf16().collect();
        let end = text.len();
        let mut editor = Self {
            text, caret: end, anchor: end, scroll: 0.0, dwrite, format, width: 100.0,
            paras: Vec::new(), tops: Vec::new(), total_height: 0.0, tops_valid: false,
            desired_x: None, undo: Vec::new(), redo: Vec::new(), last_edit: Edit::None, last_edit_time: Instant::now(),
            link_brush: None, muted_brush: None, line_spacing: 0.0, markdown: true, highlights: Vec::new(),
            ghost: None, closers: Vec::new(),
        };
        editor.rebuild_paragraphs(None);
        editor
    }

    pub fn text_string(&self) -> String {
        String::from_utf16_lossy(&self.text)
    }

    /// Replaces the whole text (note reloaded from disk); undo history starts over.
    pub fn replace_text(&mut self, text: &str) {
        self.text = text.encode_utf16().collect();
        self.caret = self.caret.min(self.text.len());
        self.anchor = self.caret;
        self.undo.clear();
        self.redo.clear();
        self.last_edit = Edit::None;
        self.changed(None);
    }

    // ---------- Paragraphs and their layouts ----------

    /// Brings the paragraph list in line with the text. `edit` = (position, removed, inserted)
    /// describes what just changed, so untouched paragraphs keep their layout and height.
    fn rebuild_paragraphs(&mut self, edit: Option<(usize, usize, usize)>) {
        let new = split_paragraphs(&self.text);
        let (prefix, suffix) = match edit {
            Some((at, removed, inserted)) => {
                let old: Vec<(usize, usize)> = self.paras.iter().map(|p| (p.start, p.len)).collect();
                reusable(&old, &new, at, removed, inserted)
            }
            None => (0, 0),
        };
        let mut old = std::mem::take(&mut self.paras);
        let mut back = old.split_off(old.len() - suffix);
        old.truncate(prefix);
        let middle = &new[prefix..new.len() - suffix];
        let back_starts = &new[new.len() - suffix..];
        for (para, &(start, _)) in back.iter_mut().zip(back_starts) {
            para.start = start;
        }
        old.extend(middle.iter().map(|&(start, len)| Para::new(start, len)));
        old.append(&mut back);
        self.paras = old;
        self.tops_valid = false;
    }

    /// Forgets every layout, e.g. after the wrap width or the font changed.
    fn invalidate_layouts(&mut self, heights_too: bool) {
        for para in &mut self.paras {
            para.layout = None;
            if heights_too {
                para.height = None;
            }
        }
        if heights_too {
            self.tops_valid = false;
        }
    }

    pub fn set_format(&mut self, format: IDWriteTextFormat) {
        self.format = format;
        self.invalidate_layouts(true);
    }

    pub fn set_width(&mut self, width: f32) {
        let width = width.max(20.0);
        if (width - self.width).abs() > 0.01 {
            self.width = width;
            self.invalidate_layouts(true);
        }
    }

    pub fn set_brushes(&mut self, link: ID2D1SolidColorBrush, muted: ID2D1SolidColorBrush) {
        self.link_brush = Some(link);
        self.muted_brush = Some(muted);
        self.invalidate_layouts(false);
    }

    /// `line_spacing` must match the format given to set_format().
    pub fn set_style(&mut self, line_spacing: f32, markdown: bool) {
        self.line_spacing = line_spacing;
        self.markdown = markdown;
        self.invalidate_layouts(true);
    }

    /// If `index` is on a Markdown checkbox ("[ ]" / "[x]"): the text offset of its inner character.
    pub fn checkbox_at(&mut self, index: usize) -> Option<usize> {
        let para = self.para_of(index);
        self.layout(para); // fills the paragraph's checkbox position
        let start = self.paras[para].start;
        let bracket = self.paras[para].checkbox?;
        (bracket..bracket + 3).contains(&(index - start)).then_some(start + bracket + 1)
    }

    /// The checkbox of the paragraph the caret is in, as the text offset of its inner character.
    pub fn checkbox_in_caret_line(&mut self) -> Option<usize> {
        let para = self.para_of(self.caret);
        self.layout(para); // fills the paragraph's checkbox position
        self.paras[para].checkbox.map(|bracket| self.paras[para].start + bracket + 1)
    }

    /// Enter. In a list item (when `lists` is on) the next line starts with the same marker;
    /// Enter on an item that is still empty ends the list instead.
    pub fn enter(&mut self, lists: bool) {
        let para = self.para_of(self.caret);
        let (start, len) = (self.paras[para].start, self.paras[para].len);
        let item = if lists && !self.has_selection() { list_prefix(&self.text[start..start + len]) } else { None };
        match item {
            Some((content, _)) if len == content => {
                self.select_range(start, start + len);
                self.insert(&[], Edit::Other);
            }
            Some((content, marker)) if self.caret >= start + content => {
                let mut inserted = vec![NEWLINE];
                inserted.extend(marker);
                self.insert(&inserted, Edit::Other);
            }
            _ => self.insert(&[NEWLINE], Edit::Other),
        }
    }

    /// Ctrl+B: puts ** around the selection, or removes them if they are already there.
    /// Without a selection it inserts an empty pair and leaves the caret between them.
    pub fn toggle_bold(&mut self) {
        const STARS: [u16; 2] = [0x2A, 0x2A];
        let (start, end) = self.selection();
        let is_stars = |from: usize, text: &[u16]| text.get(from..from + 2) == Some(&STARS[..]);
        if start == end {
            self.insert(&[0x2A; 4], Edit::Other);
            self.select_range(start + 2, start + 2);
        } else if start >= 2 && is_stars(start - 2, &self.text) && is_stars(end, &self.text) {
            // the stars sit just outside the selection
            let inner = self.text[start..end].to_vec();
            self.select_range(start - 2, end + 2);
            self.insert(&inner, Edit::Other);
            self.select_range(start - 2, end - 2);
        } else if end - start >= 4 && is_stars(start, &self.text) && is_stars(end - 2, &self.text) {
            // the stars are part of the selection
            let inner = self.text[start + 2..end - 2].to_vec();
            self.insert(&inner, Edit::Other);
            self.select_range(start, end - 4);
        } else {
            let mut wrapped = STARS.to_vec();
            wrapped.extend_from_slice(&self.text[start..end]);
            wrapped.extend(STARS);
            self.insert(&wrapped, Edit::Other);
            self.select_range(start + 2, end + 2);
        }
    }

    /// Ticks or unticks the checkbox whose inner character is at `inner`; the caret stays put.
    pub fn toggle_checkbox(&mut self, inner: usize) {
        let (caret, anchor) = (self.caret, self.anchor);
        let ticked = self.text[inner] != 0x20;
        self.select_range(inner, inner + 1);
        self.insert(&[if ticked { 0x20 } else { 0x78 }], Edit::Other);
        self.select_range(anchor, caret);
    }

    /// The wrapped layout of one paragraph, created on first use.
    fn layout(&mut self, index: usize) -> IDWriteTextLayout {
        if self.paras[index].layout.is_none() {
            let (start, len) = (self.paras[index].start, self.paras[index].len);
            let slice = &self.text[start..start + len];
            let layout = unsafe { self.dwrite.CreateTextLayout(slice, &self.format, self.width, 100_000.0) }
                .expect("CreateTextLayout");
            let links = find_links(slice);
            let markdown = if self.markdown { parse_markdown(slice) } else { Markdown::default() };
            let shown = touched(&markdown.spans, start, self.selection());
            let mut metrics = DWRITE_TEXT_METRICS::default();
            let range =|from: usize, to: usize| DWRITE_TEXT_RANGE { startPosition: from as u32, length: (to - from) as u32 };
            unsafe {
                if markdown.heading > 0 {
                    let size = self.format.GetFontSize() * HEADING_SCALE[markdown.heading - 1];
                    let _ = layout.SetFontSize(size, range(0, len));
                    let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range(0, len));
                    if self.line_spacing > 0.0 {
                        // The fixed line height comes from the format; a bigger heading needs a bigger one.
                        let _ = layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, size * self.line_spacing, size * self.line_spacing * BASELINE);
                    }
                }
                for &(from, to) in &markdown.bold {
                    let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range(from, to));
                }
                for (i, span) in markdown.spans.iter().enumerate().take(64) {
                    if shown & (1 << i) == 0 {
                        let _ = layout.SetFontSize(HIDDEN_SIZE, range(span.from, span.from + span.open));
                        if span.close > 0 {
                            let _ = layout.SetFontSize(HIDDEN_SIZE, range(span.to - span.close, span.to));
                        }
                    }
                }
                if let Some(muted) = &self.muted_brush {
                    for &(from, to) in &markdown.markers {
                        let _ = layout.SetDrawingEffect(muted, range(from, to));
                    }
                    match markdown.checkbox {
                        // A ticked task is done: dim it and strike it through.
                        Some((bracket, true)) => {
                            let _ = layout.SetDrawingEffect(muted, range(bracket + 3, len));
                            let _ = layout.SetStrikethrough(true, range(bracket + 4, len));
                        }
                        Some((bracket, false)) => {
                            let _ = layout.SetDrawingEffect(muted, range(bracket, bracket + 3));
                        }
                        None => {}
                    }
                }
                if let Some(accent) = &self.link_brush {
                    for &(from, to) in &markdown.code {
                        let _ = layout.SetDrawingEffect(accent, range(from, to));
                    }
                    if let Some((bracket, true)) = markdown.checkbox {
                        let _ = layout.SetDrawingEffect(accent, range(bracket, bracket + 3));
                    }
                    for &(from, to) in &links {
                        let _ = layout.SetDrawingEffect(accent, range(from, to));
                    }
                }
                for &(from, to) in &links {
                    let _ = layout.SetUnderline(true, range(from, to));
                }
                let _ = layout.GetMetrics(&mut metrics);
            }
            let para = &mut self.paras[index];
            para.checkbox = markdown.checkbox.map(|(bracket, _)| bracket);
            if para.height != Some(metrics.height) {
                para.height = Some(metrics.height);
                self.tops_valid = false;
            }
            para.links = links;
            para.spans = markdown.spans;
            para.shown = shown;
            para.layout = Some(layout);
        }
        self.paras[index].layout.clone().unwrap()
    }

    /// Lays paragraphs out again where the selection has moved onto or off styled text, so its
    /// syntax characters appear and vanish. Cheap enough to call before every use of the caret position.
    fn sync_markers(&mut self) {
        let selection = self.selection();
        let range = self.para_of(selection.0)..=self.para_of(selection.1);
        for (index, para) in self.paras.iter_mut().enumerate() {
            if (para.shown != 0 || range.contains(&index)) && para.height.is_some() && touched(&para.spans, para.start, selection) != para.shown {
                para.layout = None;
                para.height = None;
                self.tops_valid = false;
            }
        }
    }

    /// Makes sure every paragraph's top edge is known (lays out paragraphs whose height is not).
    fn ensure_tops(&mut self) {
        if self.tops_valid {
            return;
        }
        for index in 0..self.paras.len() {
            if self.paras[index].height.is_none() {
                self.layout(index);
            }
        }
        self.tops.clear();
        let mut top = 0.0;
        for para in &self.paras {
            self.tops.push(top);
            top += para.height.unwrap_or(0.0);
        }
        self.total_height = top;
        self.tops_valid = true;
    }

    pub fn content_height(&mut self) -> f32 {
        self.ensure_tops();
        self.total_height
    }

    fn para_of(&self, index: usize) -> usize {
        self.paras.partition_point(|p| p.start <= index).saturating_sub(1)
    }

    fn para_at_y(&mut self, y: f32) -> usize {
        self.ensure_tops();
        self.tops.partition_point(|top| *top <= y).saturating_sub(1)
    }

    // ---------- Positions ----------

    /// x, y (top) and height of the caret at a text offset, relative to the top of the text.
    fn position_of(&mut self, index: usize) -> (f32, f32, f32) {
        let para = self.para_of(index);
        let local = (index - self.paras[para].start).min(self.paras[para].len);
        let layout = self.layout(para);
        self.ensure_tops();
        let (mut x, mut y) = (0.0, 0.0);
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = layout.HitTestTextPosition(local as u32, false, &mut x, &mut y, &mut metrics);
        }
        (x, self.tops[para] + y, metrics.height.max(1.0))
    }

    /// Hit test in text coordinates: (paragraph, offset in it, on the right half?, directly on a character?).
    fn hit(&mut self, x: f32, y: f32) -> (usize, usize, bool, bool) {
        let para = self.para_at_y(y.max(0.0));
        let layout = self.layout(para);
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut metrics = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = layout.HitTestPoint(x, y.max(0.0) - self.tops[para], &mut trailing, &mut inside, &mut metrics);
        }
        (para, metrics.textPosition as usize, trailing.as_bool(), inside.as_bool())
    }

    /// Text offset nearest to a point.
    pub fn index_at(&mut self, x: f32, y: f32) -> usize {
        let (para, mut local, trailing, _) = self.hit(x, y);
        let (start, len) = (self.paras[para].start, self.paras[para].len);
        if trailing {
            local = self.next(start + local).min(start + len) - start;
        }
        start + local.min(len)
    }

    /// The character directly under a point, or None over empty space.
    pub fn char_at(&mut self, x: f32, y: f32) -> Option<usize> {
        if y < 0.0 || y >= self.content_height() {
            return None;
        }
        let (para, local, _, inside) = self.hit(x, y);
        (inside && local < self.paras[para].len).then_some(self.paras[para].start + local)
    }

    /// Start and end of the visual (wrapped) line a text offset is on.
    fn line_bounds(&mut self, index: usize) -> (usize, usize) {
        let para = self.para_of(index);
        let (start, len) = (self.paras[para].start, self.paras[para].len);
        let layout = self.layout(para);
        let mut count = 0u32;
        let mut metrics;
        unsafe {
            let _ = layout.GetLineMetrics(None, &mut count); // first call only reports the count
            metrics = vec![DWRITE_LINE_METRICS::default(); count as usize];
            if count > 0 {
                let _ = layout.GetLineMetrics(Some(&mut metrics), &mut count);
            }
        }
        let local = (index - start).min(len);
        let mut line_start = 0usize;
        for (i, line) in metrics.iter().enumerate() {
            let line_len = line.length as usize;
            let last = i == metrics.len() - 1;
            // A caret on a wrap boundary belongs to the line that starts there.
            if last || local < line_start + line_len {
                let end = if last {
                    len
                } else {
                    // Soft-wrapped line: stay in front of the whitespace that caused the wrap.
                    line_start + line_len - (line.trailingWhitespaceLength as usize).max(1).min(line_len)
                };
                return (start + line_start, start + end.max(line_start));
            }
            line_start += line_len;
        }
        (start, start + len)
    }

    fn prev(&self, index: usize) -> usize {
        if index == 0 {
            return 0;
        }
        if index >= 2 && is_low_surrogate(self.text[index - 1]) { index - 2 } else { index - 1 }
    }

    fn next(&self, index: usize) -> usize {
        let len = self.text.len();
        if index >= len {
            return len;
        }
        if index + 1 < len && is_low_surrogate(self.text[index + 1]) { index + 2 } else { index + 1 }
    }

    fn word_left(&self, mut index: usize) -> usize {
        while index > 0 && class_of(self.text[index - 1]) == CharClass::Space {
            index -= 1;
        }
        if index > 0 {
            let class = class_of(self.text[index - 1]);
            while index > 0 && class_of(self.text[index - 1]) == class {
                index -= 1;
            }
        }
        index
    }

    fn word_right(&self, mut index: usize) -> usize {
        let len = self.text.len();
        if index < len {
            let class = class_of(self.text[index]);
            if class != CharClass::Space {
                while index < len && class_of(self.text[index]) == class {
                    index += 1;
                }
            }
        }
        while index < len && class_of(self.text[index]) == CharClass::Space {
            index += 1;
        }
        index
    }

    /// The address under a text offset, ready to open (always http/https).
    pub fn link_at(&mut self, index: usize) -> Option<String> {
        let para = self.para_of(index);
        self.layout(para); // fills the paragraph's link list
        let start = self.paras[para].start;
        let local = index - start;
        let &(from, to) = self.paras[para].links.iter().find(|(from, to)| (*from..*to).contains(&local))?;
        let link = String::from_utf16_lossy(&self.text[start + from..start + to]);
        Some(if link.to_ascii_lowercase().starts_with("www.") { format!("https://{link}") } else { link })
    }

    // ---------- Caret movement ----------

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn select_range(&mut self, start: usize, end: usize) {
        self.anchor = start.min(self.text.len());
        self.caret = end.min(self.text.len());
        self.desired_x = None;
        self.last_edit = Edit::None;
        self.ghost = None;
    }

    fn move_to(&mut self, index: usize, extend: bool) {
        self.ghost = None;
        self.caret = index.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
        self.last_edit = Edit::None;
    }

    pub fn set_caret(&mut self, index: usize, extend: bool) {
        self.desired_x = None;
        self.move_to(index, extend);
    }

    pub fn move_left(&mut self, extend: bool, word: bool) {
        self.desired_x = None;
        let target = if !extend && self.has_selection() {
            self.selection().0
        } else if word {
            self.word_left(self.caret)
        } else {
            self.prev(self.caret)
        };
        self.move_to(target, extend);
    }

    pub fn move_right(&mut self, extend: bool, word: bool) {
        self.desired_x = None;
        let target = if !extend && self.has_selection() {
            self.selection().1
        } else if word {
            self.word_right(self.caret)
        } else {
            self.next(self.caret)
        };
        self.move_to(target, extend);
    }

    /// Moves the caret by a number of visual lines (negative = up).
    pub fn move_lines(&mut self, lines: f32, extend: bool) {
        let (x, y, height) = self.position_of(self.caret);
        let x = *self.desired_x.get_or_insert(x);
        let target_y = y + height * 0.5 + lines * height;
        let total = self.content_height();
        let target = if target_y < 0.0 {
            0
        } else if target_y > total {
            self.text.len()
        } else {
            self.index_at(x, target_y)
        };
        self.move_to(target, extend);
    }

    pub fn move_page(&mut self, view_height: f32, down: bool, extend: bool) {
        let (_, _, height) = self.position_of(self.caret);
        let lines = (view_height / height).floor().max(1.0);
        self.move_lines(if down { lines } else { -lines }, extend);
    }

    pub fn move_home(&mut self, extend: bool, document: bool) {
        self.desired_x = None;
        let target = if document { 0 } else { self.line_bounds(self.caret).0 };
        self.move_to(target, extend);
    }

    pub fn move_end(&mut self, extend: bool, document: bool) {
        self.desired_x = None;
        let target = if document { self.text.len() } else { self.line_bounds(self.caret).1 };
        self.move_to(target, extend);
    }

    pub fn select_all(&mut self) {
        self.ghost = None;
        self.anchor = 0;
        self.caret = self.text.len();
        self.last_edit = Edit::None;
    }

    pub fn select_word_at(&mut self, index: usize) {
        let len = self.text.len();
        if len == 0 {
            return;
        }
        let pivot = index.min(len - 1);
        let class = class_of(self.text[pivot]);
        let (mut start, mut end) = (pivot, pivot + 1);
        while start > 0 && class_of(self.text[start - 1]) == class && self.text[start - 1] != NEWLINE {
            start -= 1;
        }
        while end < len && class_of(self.text[end]) == class && self.text[end] != NEWLINE {
            end += 1;
        }
        self.anchor = start;
        self.caret = end;
        self.desired_x = None;
        self.ghost = None;
    }

    // ---------- Editing ----------

    fn checkpoint(&mut self, kind: Edit) {
        let now = Instant::now();
        let merge = kind == self.last_edit
            && kind != Edit::Other
            && now.duration_since(self.last_edit_time) < Duration::from_secs(1)
            && !self.undo.is_empty();
        if !merge {
            self.undo.push(Snapshot { text: self.text.clone(), caret: self.caret, anchor: self.anchor });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = kind;
        self.last_edit_time = now;
    }

    /// Call after the text changed; `edit` = (position, removed, inserted), or None if everything may differ.
    fn changed(&mut self, edit: Option<(usize, usize, usize)>) {
        self.rebuild_paragraphs(edit);
        self.desired_x = None;
        self.ghost = None;
        // Added closing brackets move with the text; one inside a changed stretch is forgotten.
        match edit {
            Some((pos, removed, inserted)) => self.closers.retain_mut(|at| {
                let behind = pos + removed <= *at;
                if behind {
                    *at = *at - removed + inserted;
                }
                behind || pos > *at
            }),
            None => self.closers.clear(),
        }
    }

    fn remove_range(&mut self, start: usize, end: usize) {
        self.text.drain(start..end);
        self.caret = start;
        self.anchor = start;
        self.changed(Some((start, end - start, 0)));
    }

    pub fn insert(&mut self, s: &[u16], kind: Edit) {
        self.checkpoint(kind);
        let (start, end) = self.selection();
        self.text.splice(start..end, s.iter().copied());
        self.caret = start + s.len();
        self.anchor = self.caret;
        self.changed(Some((start, end - start, s.len())));
    }

    /// A typed character. With `pairs` on, an opening bracket or quote offers its closing
    /// partner as a ghost, which turns into text with the next character typed in between.
    pub fn type_char(&mut self, c: u16, pairs: bool) {
        if !pairs {
            self.insert(&[c], Edit::Type);
            return;
        }
        let selected = self.has_selection();
        if !selected && self.closers.contains(&self.caret) && self.text.get(self.caret) == Some(&c) {
            self.move_to(self.caret + 1, false);
            self.closers.retain(|&at| at != self.caret - 1);
            return;
        }
        if let Some((at, close)) = self.ghost.filter(|g| g.0 == self.caret && !selected) {
            // A space or the partner itself right after the opening one keeps the plain text.
            if c == close || c == 0x20 {
                self.insert(&[c], Edit::Type);
                return;
            }
            self.insert(&[close], Edit::Type);
            self.caret = at;
            self.anchor = at;
            self.closers.push(at);
            if closer_of(c).is_none() {
                self.insert(&[c], Edit::Type);
                return;
            }
            // Another opening bracket goes on below and offers its own partner.
        }
        let Some(close) = closer_of(c) else {
            self.insert(&[c], Edit::Type);
            return;
        };
        let (start, end) = self.selection();
        if selected {
            let mut wrapped = vec![c];
            wrapped.extend_from_slice(&self.text[start..end]);
            wrapped.push(close);
            self.insert(&wrapped, Edit::Other);
            self.select_range(start + 1, end + 1);
            return;
        }
        // Only where a pair makes sense: not in front of a word, and no quote or "<" right
        // after one (inches, "a<b").
        let free = self.text.get(start).map_or(true, |&n| matches!(n, 9 | NEWLINE | 0x20 | 0x29 | 0x5D | 0x7D | 0x3E | 0x22));
        let after_word = matches!(c, 0x22 | 0x3C) && start > 0 && class_of(self.text[start - 1]) == CharClass::Word;
        self.insert(&[c], Edit::Type);
        if free && !after_word {
            self.ghost = Some((self.caret, close));
        }
    }

    /// The closing bracket offered after the caret, if any.
    pub fn ghost(&self) -> Option<u16> {
        self.ghost.filter(|g| g.0 == self.caret && !self.has_selection()).map(|g| g.1)
    }

    /// Ctrl+D: copies the caret's line below itself and moves the caret into the copy. With a
    /// selection, the selected text is copied right behind it and the copy gets selected.
    pub fn duplicate(&mut self) {
        let (start, end) = self.selection();
        let (at, copy, target) = if start != end {
            (end, self.text[start..end].to_vec(), (end, end + end - start))
        } else {
            let para = self.para_of(self.caret);
            let (from, len) = (self.paras[para].start, self.paras[para].len);
            let mut copy = vec![NEWLINE];
            copy.extend_from_slice(&self.text[from..from + len]);
            let caret = self.caret + len + 1;
            (from + len, copy, (caret, caret))
        };
        self.checkpoint(Edit::Other);
        self.text.splice(at..at, copy.iter().copied());
        self.changed(Some((at, 0, copy.len())));
        self.select_range(target.0, target.1);
    }

    /// First and last paragraph the selection covers. A selection ending at the very start of a
    /// line does not take that line along.
    fn selected_paras(&self) -> (usize, usize) {
        let (start, end) = self.selection();
        let first = self.para_of(start);
        let mut last = self.para_of(end);
        if last > first && self.paras[last].start == end {
            last -= 1;
        }
        (first, last)
    }

    /// Alt+Up / Alt+Down: swaps the selected lines with the line above or below them.
    pub fn move_selected_lines(&mut self, up: bool) -> bool {
        let (first, last) = self.selected_paras();
        if (up && first == 0) || (!up && last + 1 >= self.paras.len()) {
            return false;
        }
        let bounds = |p: &Para| (p.start, p.start + p.len);
        let (from, _) = bounds(&self.paras[first]);
        let (_, to) = bounds(&self.paras[last]);
        let (other_from, other_to) = bounds(&self.paras[if up { first - 1 } else { last + 1 }]);
        let (block, other) = (&self.text[from..to], &self.text[other_from..other_to]);
        let (range, parts) = if up { (other_from..to, [block, other]) } else { (from..other_to, [other, block]) };
        let swapped = [parts[0], &[NEWLINE], parts[1]].concat();
        let shift = other.len() + 1;
        let (anchor, caret) = if up { (self.anchor - shift, self.caret - shift) } else { (self.anchor + shift, self.caret + shift) };
        self.checkpoint(Edit::Other);
        let removed = range.len();
        self.text.splice(range.clone(), swapped.iter().copied());
        self.changed(Some((range.start, removed, swapped.len())));
        self.anchor = anchor;
        self.caret = caret;
        true
    }

    /// Ctrl+Shift+K: removes the selected lines (or the caret's line) with their line break.
    pub fn delete_lines(&mut self) -> bool {
        let (first, last) = self.selected_paras();
        let from = self.paras[first].start;
        let to = self.paras[last].start + self.paras[last].len;
        let (a, b) = if last + 1 < self.paras.len() { (from, to + 1) } else if first > 0 { (from - 1, to) } else { (from, to) };
        if a == b {
            return false;
        }
        self.checkpoint(Edit::Other);
        self.remove_range(a, b);
        let line = self.paras[self.para_of(a)].start;
        self.anchor = line;
        self.caret = line;
        true
    }

    /// Changes the start of every selected line in one undo step. `f` gets a line and returns
    /// how many characters to remove at its start and what to put there instead.
    fn edit_line_starts(&mut self, f: impl Fn(&[u16]) -> (usize, &'static [u16])) -> bool {
        let (first, last) = self.selected_paras();
        let from = self.paras[first].start;
        let to = self.paras[last].start + self.paras[last].len;
        let mut block = Vec::with_capacity(to - from + last - first + 1);
        let mut edits = Vec::new();
        for para in &self.paras[first..=last] {
            let line = &self.text[para.start..para.start + para.len];
            if para.start > from {
                block.push(NEWLINE);
            }
            let (removed, inserted) = f(line);
            block.extend_from_slice(inserted);
            block.extend_from_slice(&line[removed..]);
            if removed > 0 || !inserted.is_empty() {
                edits.push((para.start, removed, inserted.len()));
            }
        }
        if edits.is_empty() {
            return false;
        }
        // A position moves by what changed in front of it; one at a line start stays there.
        let map = |p: usize| edits.iter().filter(|e| e.0 < p).fold(p, |p2, &(at, removed, inserted)| p2 + inserted - removed.min(p - at));
        let (anchor, caret) = (map(self.anchor), map(self.caret));
        self.checkpoint(Edit::Other);
        self.text.splice(from..to, block.iter().copied());
        self.changed(Some((from, to - from, block.len())));
        self.anchor = anchor;
        self.caret = caret;
        true
    }

    /// Tab with a selection over several lines: puts a tab in front of each of them.
    pub fn indent(&mut self) -> bool {
        let (first, last) = self.selected_paras();
        if first == last {
            return false;
        }
        self.edit_line_starts(|line| (0, if line.is_empty() { &[] } else { &[9] }))
    }

    /// Shift+Tab: takes one tab, or up to four spaces, from the start of the selected lines.
    pub fn outdent(&mut self) -> bool {
        self.edit_line_starts(|line| match line.first() {
            Some(9) => (1, &[]),
            _ => (line.iter().take(4).take_while(|&&c| c == 0x20).count(), &[]),
        })
    }

    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.checkpoint(Edit::Other);
        let (start, end) = self.selection();
        self.remove_range(start, end);
        true
    }

    pub fn backspace(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let start = if word { self.word_left(self.caret) } else { self.prev(self.caret) };
        if start == self.caret {
            return false;
        }
        self.checkpoint(Edit::Delete);
        // An empty pair whose closing half the editor added goes as a whole.
        let pair = !word && self.closers.contains(&self.caret) && closer_of(self.text[start]).is_some_and(|c| self.text.get(self.caret) == Some(&c));
        if pair {
            self.remove_range(start, self.caret + 1);
            return true;
        }
        self.remove_range(start, self.caret);
        true
    }

    pub fn delete(&mut self, word: bool) -> bool {
        if self.delete_selection() {
            return true;
        }
        let end = if word { self.word_right(self.caret) } else { self.next(self.caret) };
        if end == self.caret {
            return false;
        }
        self.checkpoint(Edit::Delete);
        self.remove_range(self.caret, end);
        true
    }

    pub fn selected_text(&self) -> Vec<u16> {
        let (start, end) = self.selection();
        self.text[start..end].to_vec()
    }

    fn swap_snapshot(&mut self, from_undo: bool) -> bool {
        let source = if from_undo { &mut self.undo } else { &mut self.redo };
        let Some(snapshot) = source.pop() else { return false };
        let current = Snapshot { text: std::mem::replace(&mut self.text, snapshot.text), caret: self.caret, anchor: self.anchor };
        if from_undo { self.redo.push(current) } else { self.undo.push(current) }
        self.caret = snapshot.caret.min(self.text.len());
        self.anchor = snapshot.anchor.min(self.text.len());
        self.last_edit = Edit::None;
        self.changed(None);
        true
    }

    pub fn undo(&mut self) -> bool {
        self.swap_snapshot(true)
    }

    pub fn redo(&mut self) -> bool {
        self.swap_snapshot(false)
    }

    // ---------- Scrolling ----------

    pub fn max_scroll(&mut self, view_height: f32) -> f32 {
        (self.content_height() - view_height).max(0.0)
    }

    pub fn scroll_by(&mut self, delta: f32, view_height: f32) {
        let max = self.max_scroll(view_height);
        self.scroll = (self.scroll + delta).clamp(0.0, max);
    }

    pub fn scroll_to_caret(&mut self, view_height: f32) {
        self.sync_markers();
        let (_, y, height) = self.position_of(self.caret);
        if y < self.scroll {
            self.scroll = y;
        } else if y + height > self.scroll + view_height {
            self.scroll = y + height - view_height;
        }
        let max = self.max_scroll(view_height);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// x, y (top) and height of the caret, relative to the top of the text.
    pub fn caret_position(&mut self) -> (f32, f32, f32) {
        self.sync_markers();
        self.position_of(self.caret)
    }

    pub fn line_height(&mut self) -> f32 {
        self.position_of(self.caret).2
    }

    // ---------- Drawing ----------

    unsafe fn fill_range(g: &Gfx, layout: &IDWriteTextLayout, start: usize, end: usize, x: f32, top: f32, color: Color) {
        let mut count = 0u32;
        // The first call only reports how many rectangles the range needs.
        let _ = layout.HitTestTextRange(start as u32, (end - start) as u32, x, top, None, &mut count);
        let mut ranges = vec![DWRITE_HIT_TEST_METRICS::default(); count as usize];
        if count > 0 && layout.HitTestTextRange(start as u32, (end - start) as u32, x, top, Some(&mut ranges), &mut count).is_ok() {
            for r in &ranges[..count as usize] {
                g.fill_rect(rect(r.left, r.top, r.width, r.height), color);
            }
        }
    }

    /// Baseline of the wrapped line holding a paragraph offset, from the line's top.
    unsafe fn baseline_at(layout: &IDWriteTextLayout, local: usize) -> f32 {
        let mut count = 0u32;
        let _ = layout.GetLineMetrics(None, &mut count);
        let mut metrics = vec![DWRITE_LINE_METRICS::default(); count as usize];
        if count == 0 || layout.GetLineMetrics(Some(&mut metrics), &mut count).is_err() {
            return 0.0;
        }
        let mut line_start = 0;
        for line in &metrics {
            line_start += line.length as usize;
            if local < line_start {
                return line.baseline;
            }
        }
        metrics[metrics.len() - 1].baseline
    }

    /// The offered closing bracket, dimmed, right behind the caret in the size of the opening one.
    unsafe fn draw_ghost(&mut self, g: &Gfx, close: u16, x: f32, origin: f32, brush: &ID2D1SolidColorBrush) {
        let (cx, cy, _) = self.position_of(self.caret);
        let para = self.para_of(self.caret);
        let layout = self.layout(para);
        let local = self.caret - self.paras[para].start;
        let Ok(ghost) = self.dwrite.CreateTextLayout(&[close], &self.format, 1000.0, 1000.0) else { return };
        let whole = DWRITE_TEXT_RANGE { startPosition: 0, length: 1 };
        let mut size = 0.0;
        let at = local.saturating_sub(1) as u32;
        if layout.GetFontSize(at, &mut size, None).is_ok() && size > 0.0 {
            let _ = ghost.SetFontSize(size, whole);
        }
        let mut weight = DWRITE_FONT_WEIGHT_NORMAL;
        if layout.GetFontWeight(at, &mut weight, None).is_ok() {
            let _ = ghost.SetFontWeight(weight, whole);
        }
        let y = cy + Self::baseline_at(&layout, local) - Self::baseline_at(&ghost, 0);
        g.dc.DrawTextLayout(D2D_POINT_2F { x: x + cx, y: origin + y }, &ghost, brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
    }

    /// Draws highlights, selection, text and caret. (x, y) is where the top of the text goes
    /// when not scrolled; only paragraphs between `view_top` and `view_bottom` are drawn.
    pub unsafe fn draw(&mut self, g: &Gfx, x: f32, y: f32, view_top: f32, view_bottom: f32,
        text_color: Color, selection_color: Color, highlight_color: Color, caret_color: Option<Color>)
    {
        self.sync_markers();
        self.ensure_tops();
        let origin = y - self.scroll;
        let (selection_start, selection_end) = self.selection();
        let first = self.para_at_y(view_top - origin);
        let mut last = first;

        for index in first..self.paras.len() {
            let top = origin + self.tops[index];
            if top >= view_bottom {
                break;
            }
            last = index;
            let layout = self.layout(index);
            let (start, len) = (self.paras[index].start, self.paras[index].len);
            let end = start + len;

            // Search hits that overlap this paragraph (the list is sorted).
            let from = self.highlights.partition_point(|h| h.1 <= start);
            for &(a, b) in self.highlights[from..].iter().take_while(|h| h.0 < end) {
                Self::fill_range(g, &layout, a.max(start) - start, b.min(end) - start, x, top, highlight_color);
            }

            if selection_start != selection_end && selection_start <= end && selection_end > start {
                let (a, b) = (selection_start.max(start) - start, selection_end.min(end) - start);
                if b > a {
                    Self::fill_range(g, &layout, a, b, x, top, selection_color);
                }
                // The line break after this paragraph is selected too: show a small sliver for it.
                if selection_end > end {
                    let (mut cx, mut cy) = (0.0, 0.0);
                    let mut metrics = DWRITE_HIT_TEST_METRICS::default();
                    let _ = layout.HitTestTextPosition(len as u32, false, &mut cx, &mut cy, &mut metrics);
                    g.fill_rect(rect(x + cx, top + cy, 5.0, metrics.height), selection_color);
                }
            }

            g.brush.SetColor(&text_color);
            g.dc.DrawTextLayout(D2D_POINT_2F { x, y: top }, &layout, &g.brush, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT);
        }

        if let (Some(close), Some(brush)) = (self.ghost(), self.muted_brush.clone()) {
            self.draw_ghost(g, close, x, origin, &brush);
        }

        if let Some(color) = caret_color {
            let (cx, mut cy, mut height) = self.position_of(self.caret);
            if self.line_spacing > 0.0 {
                // With a fixed line height the line is taller than the letters: keep the caret
                // at letter height, sitting on the baseline.
                let font_size = height / self.line_spacing;
                cy += height * BASELINE - font_size * 0.97;
                height = font_size * 1.25;
            }
            g.fill_rect(rect(x + cx - 0.5, origin + cy, 1.5, height), color);
        }

        // Far-away paragraphs keep only their height; their layouts are rebuilt when scrolled to.
        let keep = first.saturating_sub(KEEP_AROUND_VIEW)..=last + KEEP_AROUND_VIEW;
        let caret_para = self.para_of(self.caret);
        for (index, para) in self.paras.iter_mut().enumerate() {
            if !keep.contains(&index) && index != caret_para {
                para.layout = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic random generator, so the test needs no extra crate.
    struct Random(u64);
    impl Random {
        fn below(&mut self, limit: usize) -> usize {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 33) as usize) % limit.max(1)
        }
    }

    fn slice<'a>(text: &'a [u16], para: (usize, usize)) -> &'a [u16] {
        &text[para.0..para.0 + para.1]
    }

    #[test]
    fn split_covers_the_whole_text() {
        let text: Vec<u16> = "eins\nzwei\n\ndrei\n".encode_utf16().collect();
        let paras = split_paragraphs(&text);
        assert_eq!(paras, vec![(0, 4), (5, 4), (10, 0), (11, 4), (16, 0)]);
        assert_eq!(split_paragraphs(&[]), vec![(0, 0)]);
    }

    /// Thousands of random edits: every paragraph the editor would reuse must really be unchanged.
    #[test]
    fn reused_paragraphs_are_unchanged() {
        let mut random = Random(42);
        let alphabet: Vec<u16> = "ab \n\n".encode_utf16().collect();
        let mut text: Vec<u16> = Vec::new();
        let mut reused_total = 0;
        for _ in 0..20_000 {
            let old_text = text.clone();
            let old = split_paragraphs(&old_text);
            let at = random.below(text.len() + 1);
            let removed = random.below(6).min(text.len() - at);
            let inserted: Vec<u16> = (0..random.below(6)).map(|_| alphabet[random.below(alphabet.len())]).collect();
            text.splice(at..at + removed, inserted.iter().copied());
            let new = split_paragraphs(&text);

            let (prefix, suffix) = reusable(&old, &new, at, removed, inserted.len());
            assert!(prefix + suffix <= new.len() && prefix + suffix <= old.len());
            for i in 0..prefix {
                assert_eq!(slice(&old_text, old[i]), slice(&text, new[i]));
            }
            for k in 1..=suffix {
                assert_eq!(slice(&old_text, old[old.len() - k]), slice(&text, new[new.len() - k]));
            }
            reused_total += prefix + suffix;
            if text.len() > 400 {
                text.truncate(200);
            }
        }
        assert!(reused_total > 100_000, "reuse should be the normal case, got {reused_total}");
    }

    fn md(text: &str) -> Markdown {
        parse_markdown(&text.encode_utf16().collect::<Vec<u16>>())
    }

    #[test]
    fn markdown_headings_and_checkboxes() {
        assert_eq!(md("## Titel").heading, 2);
        assert_eq!(md("## Titel").markers, vec![(0, 3)]);
        assert_eq!(md("#kein Titel").heading, 0);
        assert_eq!(md("####### zu viele").heading, 0);
        assert_eq!(md("- [ ] offen").checkbox, Some((2, false)));
        assert_eq!(md("  - [x] erledigt").checkbox, Some((4, true)));
        assert_eq!(md("- [y] nichts").checkbox, None);
        assert_eq!(md("- []kaputt").checkbox, None);
    }

    #[test]
    fn list_items_continue_with_their_marker() {
        let prefix = |text: &str| list_prefix(&text.encode_utf16().collect::<Vec<u16>>()).map(|(content, next)| (content, String::from_utf16_lossy(&next)));
        assert_eq!(prefix("- Punkt"), Some((2, "- ".to_string())));
        assert_eq!(prefix("  * eingerückt"), Some((4, "  * ".to_string())));
        assert_eq!(prefix("- [x] erledigt"), Some((6, "- [ ] ".to_string())));
        assert_eq!(prefix("- [ ] "), Some((6, "- [ ] ".to_string())));
        assert_eq!(prefix("**fett** am Anfang"), None);
        assert_eq!(prefix("-kein Punkt"), None);
        assert_eq!(prefix("normaler Text"), None);
    }

    #[test]
    fn markdown_bold_and_code() {
        let found = md("ein **fettes** Wort und `Code` hier");
        assert_eq!(found.bold, vec![(6, 12)]);
        assert_eq!(found.code, vec![(25, 29)]);
        // arithmetic and stray markers are left alone
        assert_eq!(md("64*64 und 2**8 und ** allein"), Markdown::default());
        assert_eq!(md("`**kein fett in Code**`").bold, vec![]);
        assert_eq!(md("offenes ` ohne Ende").code, vec![]);
    }

    #[test]
    fn bold_markers_show_only_where_the_selection_touches() {
        // "**a** und **b**" in a paragraph starting at 10: spans 0..5 and 10..15
        let spans = md("**a** und **b**").spans;
        assert_eq!(spans, vec![Span { from: 0, to: 5, open: 2, close: 2 }, Span { from: 10, to: 15, open: 2, close: 2 }]);
        assert_eq!(touched(&spans, 10, (0, 0)), 0);
        assert_eq!(touched(&spans, 10, (10, 10)), 0b01); // caret right before the first **
        assert_eq!(touched(&spans, 10, (15, 15)), 0b01); // right after its closing **
        assert_eq!(touched(&spans, 10, (16, 16)), 0);
        assert_eq!(touched(&spans, 10, (17, 19)), 0);
        assert_eq!(touched(&spans, 10, (22, 22)), 0b10);
        assert_eq!(touched(&spans, 10, (12, 21)), 0b11);
        assert_eq!(touched(&spans, 10, (26, 40)), 0);
    }

    #[test]
    fn heading_and_code_markers_hide_like_bold_ones() {
        // the heading spans its whole paragraph, so its # show while the caret is anywhere in it
        let spans = md("## Titel mit `Code`").spans;
        assert_eq!(spans, vec![Span { from: 0, to: 19, open: 3, close: 0 }, Span { from: 13, to: 19, open: 1, close: 1 }]);
        assert_eq!(touched(&spans, 0, (5, 5)), 0b01);
        assert_eq!(touched(&spans, 0, (14, 14)), 0b11);
        assert_eq!(touched(&spans, 0, (30, 30)), 0);
        assert_eq!(md("# ").spans, vec![]); // nothing to show but the #
    }

    #[test]
    fn the_list_marker_of_a_checklist_item_hides() {
        // "  - [x] da": the "- " hides unless the selection is on the marker or the box
        let spans = md("  - [x] da").spans;
        assert_eq!(spans, vec![Span { from: 2, to: 7, open: 2, close: 0 }]);
        assert_eq!(touched(&spans, 0, (8, 8)), 0); // behind the box, where the task is typed
        assert_eq!(touched(&spans, 0, (10, 10)), 0);
        assert_eq!(touched(&spans, 0, (7, 7)), 1);
        assert_eq!(touched(&spans, 0, (0, 2)), 1);
        assert_eq!(md("- Punkt").spans, vec![]); // a plain list item keeps its marker
    }

    #[test]
    fn links_are_found_without_trailing_punctuation() {
        let text: Vec<u16> = "siehe https://example.com/a?b=1, und (www.test.at).".encode_utf16().collect();
        let found: Vec<String> = find_links(&text).iter().map(|&(a, b)| String::from_utf16_lossy(&text[a..b])).collect();
        assert_eq!(found, vec!["https://example.com/a?b=1", "www.test.at"]);
    }

    fn editor(text: &str) -> Editor {
        unsafe {
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).unwrap();
            let format = dwrite.CreateTextFormat(windows::core::w!("Consolas"), None, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STRETCH_NORMAL, 14.0, windows::core::w!("de-de")).unwrap();
            Editor::new(dwrite, format, text)
        }
    }

    fn type_text(e: &mut Editor, text: &str) {
        for c in text.encode_utf16() {
            e.type_char(c, true);
        }
    }

    #[test]
    fn a_bracket_closes_once_something_is_typed_inside() {
        let mut e = editor("");
        type_text(&mut e, "(");
        assert_eq!((e.text_string().as_str(), e.ghost()), ("(", Some(0x29))); // only offered
        type_text(&mut e, "ab");
        assert_eq!((e.text_string().as_str(), e.caret), ("(ab)", 3));
        type_text(&mut e, ")"); // steps over the added one
        assert_eq!((e.text_string().as_str(), e.caret), ("(ab)", 4));

        let mut e = editor("");
        type_text(&mut e, "[{x");
        assert_eq!(e.text_string(), "[{x}]");
        type_text(&mut e, "}]!");
        assert_eq!(e.text_string(), "[{x}]!");

        let mut e = editor("");
        type_text(&mut e, "()"); // typed by hand: no doubled partner
        assert_eq!(e.text_string(), "()");
        let mut e = editor("");
        type_text(&mut e, "\"zitat\"");
        assert_eq!(e.text_string(), "\"zitat\"");
    }

    #[test]
    fn no_pair_where_it_would_get_in_the_way() {
        let mut e = editor("wort");
        e.set_caret(0, false);
        type_text(&mut e, "(x"); // in front of a word
        assert_eq!(e.text_string(), "(xwort");
        let mut e = editor("");
        type_text(&mut e, "a < b, 5\" x<y, ( c");
        assert_eq!(e.text_string(), "a < b, 5\" x<y, ( c");
        let mut e = editor("");
        e.type_char(0x28, true);
        e.move_left(false, false); // moving away drops the offer
        assert_eq!(e.ghost(), None);
    }

    #[test]
    fn brackets_wrap_a_selection_and_go_as_a_pair() {
        let mut e = editor("wort");
        e.select_all();
        type_text(&mut e, "(");
        assert_eq!((e.text_string().as_str(), e.selection()), ("(wort)", (1, 5)));
        let mut e = editor("");
        type_text(&mut e, "(a");
        assert!(e.backspace(false));
        assert_eq!(e.text_string(), "()");
        assert!(e.backspace(false)); // the empty pair goes as a whole
        assert_eq!(e.text_string(), "");
        let mut e = editor("");
        type_text(&mut e, "(a");
        assert!(e.undo());
        assert_eq!(e.text_string(), "");
    }

    #[test]
    fn duplicate_copies_the_line_or_the_selection() {
        let mut e = editor("eins\nzwei\ndrei");
        e.set_caret(7, false); // "zw|ei"
        e.duplicate();
        assert_eq!((e.text_string().as_str(), e.caret), ("eins\nzwei\nzwei\ndrei", 12));
        let mut e = editor("ab");
        e.duplicate(); // last line without a line break
        assert_eq!((e.text_string().as_str(), e.caret), ("ab\nab", 5));
        let mut e = editor("abc");
        e.select_range(0, 2);
        e.duplicate();
        assert_eq!((e.text_string().as_str(), e.selection()), ("ababc", (2, 4)));
        assert!(e.undo());
        assert_eq!(e.text_string(), "abc");
    }

    #[test]
    fn lines_move_up_and_down_with_their_selection() {
        let mut e = editor("eins\nzwei\ndrei");
        e.set_caret(7, false); // "zw|ei"
        assert!(e.move_selected_lines(true));
        assert_eq!((e.text_string().as_str(), e.caret), ("zwei\neins\ndrei", 2));
        assert!(!e.move_selected_lines(true)); // already at the top
        e.select_range(0, 10); // "zwei\neins\n": the third line is not part of it
        assert!(e.move_selected_lines(false));
        assert_eq!((e.text_string().as_str(), e.selection()), ("drei\nzwei\neins", (5, 15)));
        assert!(e.undo());
        assert_eq!(e.text_string(), "zwei\neins\ndrei");
    }

    #[test]
    fn delete_lines_takes_the_line_break_along() {
        let mut e = editor("eins\nzwei\ndrei");
        e.set_caret(6, false);
        assert!(e.delete_lines());
        assert_eq!((e.text_string().as_str(), e.caret), ("eins\ndrei", 5));
        e.set_caret(7, false); // the last line takes the break in front of it
        assert!(e.delete_lines());
        assert_eq!((e.text_string().as_str(), e.caret), ("eins", 0));
        assert!(e.delete_lines());
        assert_eq!(e.text_string(), "");
        assert!(!e.delete_lines());
    }

    #[test]
    fn tab_indents_and_shift_tab_outdents_selected_lines() {
        let mut e = editor("- a\n\n- b\nc");
        e.select_range(0, 9); // up to the start of "c"
        assert!(e.indent());
        assert_eq!((e.text_string().as_str(), e.selection()), ("\t- a\n\n\t- b\nc", (0, 11)));
        assert!(e.outdent());
        assert_eq!((e.text_string().as_str(), e.selection()), ("- a\n\n- b\nc", (0, 9)));
        let mut e = editor("      x");
        e.set_caret(7, false);
        assert!(e.outdent()); // four of the six spaces
        assert_eq!((e.text_string().as_str(), e.caret), ("  x", 3));
        let mut e = editor("ab");
        e.select_range(0, 1);
        assert!(!e.indent()); // within one line Tab stays a normal tab
    }
}
