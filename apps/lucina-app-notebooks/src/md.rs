//! Markdown for a reply or an insight, as far as the toolkit draws it: one face per run, so inline marks go and a wholly bold line
//! stays bold. Citations follow `web/src/lib/markdown.ts`: `[source:id]` and `[insight:id]` become numbered marks in the order they
//! first appear, `[note:slug]` becomes `note:slug`. A note cannot be opened from here, so that one is plain text.

use std::cell::RefCell;

use lucina_account::rich;
use lucina_app::gfx::Text;
use lucina_app::{Face, Rect, Theme, Ui};

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(String),
    Para { text: String, bold: bool },
    Item { mark: String, text: String, depth: usize },
    Code(String),
    Rule,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// The citations, each once, in the order of their numbers.
    pub refs: Vec<String>,
    /// The width last measured, and each block's height at it.
    heights: RefCell<(i32, Vec<i32>)>,
}

fn id_char(c: char, first: bool) -> bool {
    c.is_ascii_alphanumeric() || (!first && matches!(c, '_' | '-'))
}

/// `NOTEBOOK_CITE` and `NOTE_CITE`: how long the citation at the start of `rest` is, and what it refers to.
fn citation(rest: &str) -> Option<(usize, &str, &str)> {
    let inner = rest.strip_prefix('[')?;
    let end = inner.find(']')?;
    let body = &inner[..end];
    let (kind, id) = body.split_once(':')?;
    if id.is_empty() || id.chars().count() > 80 {
        return None;
    }
    if !id.chars().enumerate().all(|(i, c)| id_char(c, i == 0)) {
        return None;
    }
    if !matches!(kind, "source" | "insight" | "note") {
        return None;
    }
    // `[source:x](link)` is a link, not a citation.
    if inner[end + 1..].starts_with('(') {
        return None;
    }
    Some((end + 2, kind, body))
}

fn number_citations(line: &str, refs: &mut Vec<String>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut in_code = false;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            in_code = !in_code;
        }
        if c == '[' && !in_code {
            if let Some((len, kind, body)) = citation(rest) {
                if kind == "note" {
                    out.push_str(body);
                } else {
                    let n = refs.iter().position(|r| r == body).unwrap_or_else(|| {
                        refs.push(body.to_owned());
                        refs.len() - 1
                    });
                    out.push_str(&format!("[{}]", n + 1));
                }
                rest = &rest[len..];
                continue;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Marks gone, a link's address kept after its label: nothing opens links here. The bool: every word was bold.
fn prose(line: &str) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let spans = rich::inline(line);
    let bold = !spans.is_empty() && spans.iter().filter(|s| !s.text.trim().is_empty()).all(|s| s.bold);
    for s in spans {
        out.push_str(&s.text);
        if let Some(url) = s.link.filter(|u| *u != s.text) {
            out.push_str(&format!(" ({url})"));
        }
    }
    (out, bold)
}

fn ordered_mark(t: &str) -> Option<(&str, &str)> {
    let digits = t.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") "))?;
    Some((&t[..digits], rest))
}

pub fn parse(text: &str) -> Doc {
    let mut doc = Doc::default();
    let mut para = String::new();
    let mut code: Option<String> = None;
    let flush = |para: &mut String, doc: &mut Doc| {
        if !para.is_empty() {
            let cited = number_citations(para, &mut doc.refs);
            let (text, bold) = prose(&cited);
            doc.blocks.push(Block::Para { text, bold });
            para.clear();
        }
    };
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            match code.take() {
                Some(c) => doc.blocks.push(Block::Code(c.trim_end().to_owned())),
                None => {
                    flush(&mut para, &mut doc);
                    code = Some(String::new());
                }
            }
            continue;
        }
        if let Some(c) = &mut code {
            c.push_str(line);
            c.push('\n');
            continue;
        }
        let depth = (line.len() - t.len()) / 2;
        if t.is_empty() {
            flush(&mut para, &mut doc);
        } else if t.len() >= 3 && (t.bytes().all(|b| b == b'-') || t.bytes().all(|b| b == b'*')) {
            flush(&mut para, &mut doc);
            doc.blocks.push(Block::Rule);
        } else if let Some(rest) = t.strip_prefix('#').and_then(|_| t.trim_start_matches('#').strip_prefix(' ')) {
            flush(&mut para, &mut doc);
            let cited = number_citations(rest, &mut doc.refs);
            doc.blocks.push(Block::Heading(prose(&cited).0));
        } else if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("+ ")) {
            flush(&mut para, &mut doc);
            let cited = number_citations(rest, &mut doc.refs);
            doc.blocks.push(Block::Item { mark: "•".into(), text: prose(&cited).0, depth });
        } else if let Some((n, rest)) = ordered_mark(t) {
            flush(&mut para, &mut doc);
            let cited = number_citations(rest, &mut doc.refs);
            doc.blocks.push(Block::Item { mark: format!("{n}."), text: prose(&cited).0, depth });
        } else if t.starts_with('|') {
            flush(&mut para, &mut doc);
            // A table as lines: the cells of a row side by side. The |---| row has nothing to say.
            let cells: Vec<&str> = t.trim_matches('|').split('|').map(str::trim).collect();
            if !cells.iter().all(|c| c.trim_matches([':', '-']).is_empty()) {
                let cited = number_citations(&cells.join(" · "), &mut doc.refs);
                let (text, bold) = prose(&cited);
                doc.blocks.push(Block::Para { text, bold });
            }
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t.strip_prefix("> ").unwrap_or(t));
        }
    }
    if let Some(c) = code {
        doc.blocks.push(Block::Code(c.trim_end().to_owned()));
    }
    flush(&mut para, &mut doc);
    doc
}

/// `plainExcerpt` in `web/src/api/notes.ts`: an excerpt without its Markdown, or the title it starts with.
pub fn plain_excerpt(excerpt: &str, title: &str) -> String {
    let unticked = excerpt.replace("[ ] ", "").replace("[x] ", "").replace("[X] ", "");
    let words: Vec<&str> = unticked
        .split_whitespace()
        .filter(|w| {
            let hashes = !w.is_empty() && w.len() <= 6 && w.bytes().all(|b| b == b'#');
            let rule = w.len() >= 3 && w.bytes().all(|b| b == b'-');
            let mark = matches!(*w, ">" | "-" | "*" | "+" | "|");
            !hashes && !rule && !mark && ordered_mark(&format!("{w} x")).is_none()
        })
        .collect();
    let (plain, _) = prose(&words.join(" "));
    let plain = plain.replace('|', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    let tt = title.trim();
    match plain.get(..tt.len()) {
        Some(head) if !tt.is_empty() && head.eq_ignore_ascii_case(tt) => plain[tt.len()..].trim().to_owned(),
        _ => plain,
    }
}

const BODY: f32 = 15.0;
const CODE: f32 = 14.0;
const HEADING: f32 = 17.0;
const GAP: f32 = 6.0;

fn indent(t: &Theme, depth: usize) -> i32 {
    t.px(18.0) * (depth.min(4) as i32 + 1)
}

fn face_of(b: &Block) -> (Face, f32) {
    match b {
        Block::Heading(_) => (Face::Bold, HEADING),
        Block::Para { bold: true, .. } => (Face::Bold, BODY),
        Block::Code(_) => (Face::Mono, CODE),
        _ => (Face::Sans, BODY),
    }
}

/// The text of a block, and how wide it wraps in a document `w` wide.
fn text_of<'a>(b: &'a Block, t: &Theme, w: i32) -> Option<(&'a str, i32)> {
    match b {
        Block::Heading(s) | Block::Para { text: s, .. } => Some((s, w)),
        Block::Item { text: s, depth, .. } => Some((s, w - indent(t, *depth))),
        Block::Code(s) => Some((s, w - 2 * t.px(8.0))),
        Block::Rule => None,
    }
}

/// How tall [`draw`] will be at width `w`. Remembers each block's height, so that drawing does not wrap the text again.
pub fn height(text: &mut Text, doc: &Doc, t: &Theme, w: i32) -> i32 {
    let gap = t.px(GAP);
    let mut heights = Vec::with_capacity(doc.blocks.len());
    for b in &doc.blocks {
        let (face, px) = face_of(b);
        heights.push(match (b, text_of(b, t, w)) {
            (Block::Code(_), Some((s, inner))) => Ui::wrapped_height(text, face, t.pt(px), s, inner) + 2 * t.px(4.0),
            (_, Some((s, inner))) => Ui::wrapped_height(text, face, t.pt(px), s, inner),
            _ => t.px(3.0),
        });
    }
    let total = heights.iter().map(|h| h + gap).sum();
    *doc.heights.borrow_mut() = (w, heights);
    total
}

/// The document at the top of `r`. It only draws, so a pass that does not paint skips it.
pub fn draw(ui: &mut Ui, doc: &Doc, r: Rect) {
    if !ui.painting() {
        return;
    }
    let t = ui.theme();
    let gap = t.px(GAP);
    let known = doc.heights.borrow();
    let known = (known.0 == r.w).then_some(&known.1);
    let mut y = r.y;
    for (i, b) in doc.blocks.iter().enumerate() {
        y += gap;
        let (face, size) = face_of(b);
        let px = t.pt(size);
        let lines = |ui: &mut Ui, s: &str, w: i32| ui.wrap_count(face, px, s, w) * ui.line_height(face, px);
        let h = match (known.and_then(|k| k.get(i)), text_of(b, &t, r.w), b) {
            (Some(h), _, _) => *h,
            (None, Some((s, w)), Block::Code(_)) => lines(ui, s, w) + 2 * t.px(4.0),
            (None, Some((s, w)), _) => lines(ui, s, w),
            (None, None, _) => t.px(3.0),
        };
        match b {
            Block::Heading(s) => ui.within(Rect::new(r.x, y, r.w, r.bottom() - y), |ui| ui.wrapped(face, px, t.accent, s)),
            Block::Para { text, .. } => ui.within(Rect::new(r.x, y, r.w, r.bottom() - y), |ui| ui.wrapped(face, px, t.text, text)),
            Block::Item { mark, text, depth } => {
                let x = indent(&t, *depth);
                let mark_r = Rect::new(r.x + x - t.px(18.0), y, t.px(18.0), ui.line_height(face, px));
                ui.text_in(mark_r, face, px, t.dim, mark);
                ui.within(Rect::new(r.x + x, y, r.w - x, r.bottom() - y), |ui| ui.wrapped(face, px, t.text, text));
            }
            Block::Code(s) => {
                ui.fill_round(Rect::new(r.x, y, r.w, h), t.px(4.0), t.bar);
                ui.within(Rect::new(r.x + t.px(8.0), y + t.px(4.0), r.w - 2 * t.px(8.0), h), |ui| ui.wrapped(face, px, t.item, s));
            }
            Block::Rule => ui.fill(Rect::new(r.x, y + t.px(1.0), r.w, 1), t.panel_hi),
        }
        y += h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citations_are_numbered_once_in_order_of_first_use() {
        let d = parse("One [source:s1-0], two [insight:i2], again [source:s1-0], a note [note:lect-1].");
        assert_eq!(d.refs, ["source:s1-0", "insight:i2"]);
        assert_eq!(d.blocks, [Block::Para { text: "One [1], two [2], again [1], a note note:lect-1.".into(), bold: false }]);
    }

    #[test]
    fn what_only_looks_like_a_citation_stays_text() {
        assert_eq!(parse("`[source:a]` [source:bad id] [other:x]").refs, Vec::<String>::new());
        let d = parse("[source:a](https://example.com)");
        assert!(d.refs.is_empty(), "a link, not a citation");
        assert_eq!(d.blocks, [Block::Para { text: "source:a (https://example.com)".into(), bold: false }]);
    }

    #[test]
    fn blocks_come_out_of_the_usual_markdown() {
        let d = parse("## Title\n\n**All bold**\n\n- first\n  - inner\n3) third\n\n```\nlet x = 1;\n```\n---\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        assert_eq!(
            d.blocks,
            [
                Block::Heading("Title".into()),
                Block::Para { text: "All bold".into(), bold: true },
                Block::Item { mark: "•".into(), text: "first".into(), depth: 0 },
                Block::Item { mark: "•".into(), text: "inner".into(), depth: 1 },
                Block::Item { mark: "3.".into(), text: "third".into(), depth: 0 },
                Block::Code("let x = 1;".into()),
                Block::Rule,
                Block::Para { text: "a · b".into(), bold: false },
                Block::Para { text: "1 · 2".into(), bold: false },
            ]
        );
    }

    #[test]
    fn a_reply_cut_mid_stream_still_parses() {
        let d = parse("Some **bold and [source:s1");
        assert_eq!(d.refs, Vec::<String>::new());
        assert_eq!(parse("```\nopen").blocks, [Block::Code("open".into())]);
    }

    #[test]
    fn an_excerpt_loses_its_markdown_and_its_title() {
        assert_eq!(plain_excerpt("# Plan\n- [ ] **buy** milk", "Plan"), "buy milk");
        assert_eq!(plain_excerpt("1. one | two", ""), "one two");
    }
}
