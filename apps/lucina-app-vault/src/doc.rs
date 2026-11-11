//! A note's blocks as rows for one measured list: each row is a paragraph, a code line, a table row or a picture, wrapped by run so
//! that bold, links and tags stay what they are across lines.

use lucina_app::Face;
use lucina_vault::markdown::{Block, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Text {
        spans: Vec<Span>,
        level: u8,
    },
    Code(String),
    Rule,
    Embed {
        target: String,
        alt: String,
        external: bool,
    },
    Table {
        cells: Vec<Vec<Span>>,
        head: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub kind: Kind,
    /// Nesting, in steps.
    pub indent: u8,
    /// `•`, `3.`, a task box or a footnote's label.
    pub marker: Option<String>,
    /// Inside a quote or a callout: the bar beside it.
    pub bar: bool,
    /// A callout's title row.
    pub callout: bool,
}

fn text(spans: Vec<Span>, level: u8) -> Kind {
    Kind::Text { spans, level }
}

fn push(out: &mut Vec<Row>, kind: Kind, indent: u8, bar: bool) {
    out.push(Row {
        kind,
        indent,
        marker: None,
        bar,
        callout: false,
    });
}

fn walk(blocks: &[Block], indent: u8, bar: bool, out: &mut Vec<Row>) {
    for b in blocks {
        match b {
            Block::Heading { level, spans } => push(out, text(spans.clone(), *level), indent, bar),
            Block::Paragraph { spans } => push(out, text(spans.clone(), 0), indent, bar),
            Block::Code { text, .. } | Block::Math { text } | Block::Html { text } => {
                for l in text.trim_end_matches('\n').split('\n') {
                    push(out, Kind::Code(l.to_owned()), indent, bar);
                }
            }
            Block::Quote { blocks } => walk(blocks, indent, true, out),
            Block::Callout {
                kind,
                title,
                blocks,
                ..
            } => {
                let title = if title.is_empty() {
                    vec![Span {
                        text: kind.clone(),
                        ..Default::default()
                    }]
                } else {
                    title.clone()
                };
                out.push(Row {
                    kind: text(title, 0),
                    indent,
                    marker: None,
                    bar: true,
                    callout: true,
                });
                walk(blocks, indent, true, out);
            }
            Block::List {
                ordered,
                start,
                items,
            } => {
                for (i, item) in items.iter().enumerate() {
                    let marker = match item.task {
                        Some(true) => "[x]".to_owned(),
                        Some(false) => "[ ]".to_owned(),
                        None if *ordered => format!("{}.", *start + i as u64),
                        None => "•".to_owned(),
                    };
                    let first = out.len();
                    walk(&item.blocks, indent + 1, bar, out);
                    if first == out.len() {
                        push(out, text(Vec::new(), 0), indent + 1, bar);
                    }
                    out[first].marker = Some(marker);
                }
            }
            Block::Table { head, rows, .. } => {
                push(
                    out,
                    Kind::Table {
                        cells: head.clone(),
                        head: true,
                    },
                    indent,
                    bar,
                );
                for r in rows {
                    push(
                        out,
                        Kind::Table {
                            cells: r.clone(),
                            head: false,
                        },
                        indent,
                        bar,
                    );
                }
            }
            Block::Rule => push(out, Kind::Rule, indent, bar),
            Block::Embed {
                target,
                alt,
                external,
            } => push(
                out,
                Kind::Embed {
                    target: target.clone(),
                    alt: alt.clone(),
                    external: *external,
                },
                indent,
                bar,
            ),
            Block::FootnoteDefinition { label, blocks } => {
                let first = out.len();
                walk(blocks, indent + 1, bar, out);
                if first < out.len() {
                    out[first].marker = Some(format!("[{label}]"));
                }
            }
        }
    }
}

pub fn rows(blocks: &[Block]) -> Vec<Row> {
    let mut out = Vec::new();
    walk(blocks, 0, false, &mut out);
    out
}

/// One run of text on a line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub x: i32,
    pub w: i32,
    pub text: String,
    pub span: usize,
}

pub fn face_of(s: &Span, level: u8) -> Face {
    if s.code {
        Face::Mono
    } else if s.bold || level > 0 {
        Face::Bold
    } else {
        Face::Sans
    }
}

/// Word wrap across runs. `measure` is the toolkit's, so tests and the screen agree.
pub fn wrap(
    spans: &[Span],
    level: u8,
    width: i32,
    mut measure: impl FnMut(Face, &str) -> i32,
) -> Vec<Vec<Placed>> {
    let mut lines: Vec<Vec<Placed>> = vec![Vec::new()];
    let mut x = 0;
    let width = width.max(1);
    for (si, s) in spans.iter().enumerate() {
        let f = face_of(s, level);
        let mut rest = s.text.as_str();
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix('\n') {
                lines.push(Vec::new());
                x = 0;
                rest = r;
                continue;
            }
            // A word with the spaces before it.
            let spaces = rest.len()
                - rest
                    .trim_start_matches(|c: char| c.is_whitespace() && c != '\n')
                    .len();
            let body = &rest[spaces..];
            let word = body.find(char::is_whitespace).unwrap_or(body.len());
            let token = &rest[..spaces + word];
            let mut w = measure(f, token);
            let mut shown = token;
            if x + w > width && x > 0 {
                lines.push(Vec::new());
                x = 0;
                shown = token.trim_start();
                w = measure(f, shown);
                if shown.is_empty() {
                    rest = &rest[token.len()..];
                    continue;
                }
            }
            if w > width {
                // Longer than a line: as many characters as fit.
                let mut end = 0;
                for (i, c) in shown.char_indices() {
                    let e = i + c.len_utf8();
                    if measure(f, &shown[..e]) > width {
                        break;
                    }
                    end = e;
                }
                let end = if end == 0 {
                    shown.chars().next().map_or(0, char::len_utf8)
                } else {
                    end
                };
                place(
                    lines.last_mut(),
                    x,
                    measure(f, &shown[..end]),
                    &shown[..end],
                    si,
                );
                lines.push(Vec::new());
                x = 0;
                let used = token.len() - shown.len() + end;
                rest = &rest[used..];
                continue;
            }
            place(lines.last_mut(), x, w, shown, si);
            x += w;
            rest = &rest[token.len()..];
        }
    }
    lines
}

fn place(line: Option<&mut Vec<Placed>>, x: i32, w: i32, text: &str, span: usize) {
    let Some(line) = line else { return };
    if let Some(last) = line.last_mut() {
        if last.span == span {
            last.text.push_str(text);
            last.w = x + w - last.x;
            return;
        }
    }
    line.push(Placed {
        x,
        w,
        text: text.to_owned(),
        span,
    });
}
