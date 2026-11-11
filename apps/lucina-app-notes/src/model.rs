//! A note and what can be asked of one. The behaviour is SecretaryIOS's: `web/src/api/keep.ts`, `web/src/lib/keepFilter.ts`,
//! `web/src/lib/keepText.ts` and `web/src/state/keep.ts`, and `docs/keep.md` for the contract. The tests at the end repeat
//! the cases of `web/src/lib/keepFilter.test.ts` and of the server's `keep_tests.rs`.

use std::cmp::Ordering;
use std::sync::Arc;

use lucina_app_support::dates::{parse_iso, random};
use lucina_app_support::text::{fold_note, word_match};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const COLORS: [&str; 12] = [
    "default", "red", "orange", "yellow", "green", "teal", "blue", "cerulean", "purple", "pink",
    "brown", "gray",
];

/// The server's limits (`docs/keep.md`).
pub const NOTE_MAX_BYTES: usize = 256 * 1024;

fn default_color() -> String {
    "default".into()
}

fn default_source() -> String {
    "owner".into()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Item {
    pub id: String,
    pub text: String,
    pub checked: bool,
    pub indent: u8,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Attachment {
    pub id: String,
    pub file: String,
    pub mimetype: String,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Annotation {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Reminder {
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
}

/// A full note as the server stores it: live, archived or in the trash.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Note {
    pub id: String,
    pub slug: String,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    #[serde(default = "default_color")]
    pub color: String,
    pub pinned: bool,
    pub archived: bool,
    /// Present, even empty, makes it a list note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Item>>,
    pub attachments: Vec<Attachment>,
    pub annotations: Vec<Annotation>,
    pub collaborators: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reminder: Option<Reminder>,
    pub position: f64,
    pub rev: u64,
    #[serde(default = "default_source")]
    pub source: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

/// `parseKeepNote`: a stored 0 position means "not set", an unknown colour is the default one, an item without an id gets
/// one. `None` for what is not a note at all.
pub fn parse_note(v: &Value) -> Option<Note> {
    let mut n: Note = serde_json::from_value(v.clone()).ok()?;
    if n.id.is_empty() {
        return None;
    }
    if !COLORS.contains(&n.color.as_str()) {
        n.color = default_color();
    }
    if n.position == 0.0 {
        n.position = parse_iso(&n.created_at).unwrap_or(0) as f64;
    }
    if n.source.is_empty() {
        n.source = default_source();
    }
    for i in n.items.iter_mut().flatten() {
        if i.id.is_empty() {
            i.id = new_item_id();
        }
        i.indent = u8::from(i.indent == 1);
    }
    Some(n)
}

/// `newItemId`: `i_` and 12 hex digits.
pub fn new_item_id() -> String {
    let mut b = [0u8; 6];
    random(&mut b);
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("i_{hex}")
}

/// `normLabel`: trim, drop a leading `#`, collapse whitespace, lower case.
pub fn norm_label(name: &str) -> String {
    clean_label(name).to_lowercase()
}

/// `cleanLabelName`: as typed, but trimmed, without a leading `#`, whitespace collapsed.
pub fn clean_label(name: &str) -> String {
    let t = name.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unique_tags(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let t = norm_label(n);
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

fn some_or_null<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<Reminder>>, D::Error> {
    Ok(Some(Option::<Reminder>::deserialize(d)?))
}

/// What the client may send in `POST` and `PATCH`: absent fields keep their value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Input {
    pub title: Option<String>,
    pub content: Option<String>,
    pub color: Option<String>,
    pub pinned: Option<bool>,
    pub archived: Option<bool>,
    pub items: Option<Vec<Item>>,
    pub labels: Option<Vec<String>>,
    /// `Some(None)` clears the reminder, also after a trip through the outbox file.
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "some_or_null"
    )]
    pub reminder: Option<Option<Reminder>>,
    pub position: Option<f64>,
    /// `text` or `list`.
    pub convert: Option<String>,
}

impl Input {
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        if let Some(v) = &self.title {
            m.insert("title".into(), json!(v));
        }
        if let Some(v) = &self.content {
            m.insert("content".into(), json!(v));
        }
        if let Some(v) = &self.color {
            m.insert("color".into(), json!(v));
        }
        if let Some(v) = self.pinned {
            m.insert("pinned".into(), json!(v));
        }
        if let Some(v) = self.archived {
            m.insert("archived".into(), json!(v));
        }
        if let Some(v) = &self.items {
            m.insert("items".into(), json!(v));
        }
        if let Some(v) = &self.labels {
            m.insert("labels".into(), json!(v));
        }
        if let Some(v) = &self.reminder {
            m.insert("reminder".into(), json!(v));
        }
        if let Some(v) = self.position {
            m.insert("position".into(), json!(v));
        }
        if let Some(v) = &self.convert {
            m.insert("convert".into(), json!(v));
        }
        Value::Object(m)
    }

    /// Later edits to the same note, on top of these: what one queued `PATCH` can carry instead of two. A later `content`
    /// or `items` replaces the earlier body whichever it was.
    pub fn merge(&mut self, later: Input) {
        if later.content.is_some() {
            self.items = None;
        }
        if later.items.is_some() {
            self.content = None;
        }
        macro_rules! take {
            ($($f:ident),*) => { $(if later.$f.is_some() { self.$f = later.$f; })* };
        }
        take!(
            title, content, color, pinned, archived, items, labels, reminder, position, convert
        );
    }
}

/// `itemsToContent`: the checklist as markdown lines, the way the server derives `content`.
pub fn items_to_content(items: &[Item]) -> String {
    items
        .iter()
        .map(|i| {
            format!(
                "{}- [{}] {}",
                if i.indent == 1 { "  " } else { "" },
                if i.checked { "x" } else { " " },
                i.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `contentToItems`: the lines of a text as checklist items. Checkbox syntax keeps its state.
pub fn content_to_items(content: &str) -> Vec<Item> {
    let mut out = Vec::new();
    for raw in content.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.trim().is_empty() {
            continue;
        }
        let spaces = raw.len() - raw.trim_start().len();
        let body = raw.trim_start();
        let marked = body
            .strip_prefix("- [")
            .or_else(|| body.strip_prefix("* ["))
            .and_then(|r| {
                let mut c = r.chars();
                let mark = c.next()?;
                (matches!(mark, ' ' | 'x' | 'X') && c.next() == Some(']'))
                    .then_some((mark != ' ', c.as_str()))
            });
        // docs/keep.md, as the server parses it: two or more leading spaces or a tab are indent 1 on any line, and a plain
        // bullet loses its marker.
        let indent = u8::from(spaces >= 2 || raw.starts_with('\t'));
        let (text, checked) = match marked {
            Some((checked, rest)) => (rest.trim().to_owned(), checked),
            None => {
                let plain = body
                    .strip_prefix("- ")
                    .or_else(|| body.strip_prefix("* "))
                    .unwrap_or(body);
                (plain.trim().to_owned(), false)
            }
        };
        out.push(Item {
            id: new_item_id(),
            text,
            checked,
            indent,
        });
    }
    out
}

/// `applyInput`: a note with a patch applied locally, the way the server will apply it.
pub fn apply_input(note: &Note, input: &Input, now: &str) -> Note {
    let mut n = note.clone();
    n.updated_at = now.to_owned();
    if let Some(v) = &input.title {
        n.title = v.clone();
    }
    if let Some(v) = &input.content {
        n.content = v.clone();
    }
    if let Some(v) = &input.color {
        n.color = v.clone();
    }
    if let Some(v) = input.pinned {
        n.pinned = v;
    }
    if let Some(v) = input.archived {
        n.archived = v;
    }
    if let Some(v) = input.position {
        n.position = v;
    }
    if let Some(v) = &input.labels {
        n.tags = unique_tags(v);
    }
    if let Some(v) = &input.reminder {
        n.reminder = v.clone();
    }
    if let Some(v) = &input.items {
        n.content = items_to_content(v);
        n.items = Some(v.clone());
    }
    match input.convert.as_deref() {
        Some("text") => {
            if let Some(items) = n.items.take() {
                // The server keeps an indented item's two spaces (the web's local copy drops them until its answer).
                n.content = items
                    .iter()
                    .map(|i| format!("{}{}", if i.indent == 1 { "  " } else { "" }, i.text))
                    .collect::<Vec<_>>()
                    .join("\n");
            }
        }
        Some("list") if n.items.is_none() => {
            let items = content_to_items(&n.content);
            n.content = items_to_content(&items);
            n.items = Some(items);
        }
        _ => {}
    }
    n
}

fn fold_title(t: &str) -> String {
    fold_note(t)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `splitTitleHeading`: a leading `# <title>` only repeats the title. It is not shown, and is kept when saving.
pub fn split_title_heading<'a>(md: &'a str, title: &str) -> (&'a str, &'a str) {
    let t = fold_title(title);
    if t.is_empty() {
        return ("", md);
    }
    let mut at = 0;
    let mut rest = md;
    while let Some((line, next)) = rest.split_once('\n') {
        if !line.trim().is_empty() {
            break;
        }
        at += line.len() + 1;
        rest = next;
    }
    let line = rest.split('\n').next().unwrap_or("").trim_end_matches('\r');
    let heading = line
        .strip_prefix("## ")
        .or_else(|| line.strip_prefix("# "))
        .map(|h| h.trim().trim_end_matches('#').trim());
    if heading.map(fold_title).as_deref() != Some(t.as_str()) {
        return ("", md);
    }
    let mut end = at + line.len();
    let bytes = md.as_bytes();
    while end < md.len() {
        let line_end = md[end..].find('\n').map_or(md.len(), |i| end + i);
        if bytes.get(end) == Some(&b'\n') {
            end += 1;
            continue;
        }
        if md[end..line_end].trim().is_empty() {
            end = (line_end + 1).min(md.len());
        } else {
            break;
        }
    }
    md.split_at(end)
}

/// `plainText`: markdown marks removed, for search and for card previews.
pub fn plain_text(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for (i, line) in md.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        let heading = body
            .strip_prefix("## ")
            .or_else(|| body.strip_prefix("# "))
            .map(str::trim_start);
        let task = [
            "- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "* [X] ", "+ [ ] ", "+ [x] ",
            "+ [X] ",
        ]
        .iter()
        .find_map(|p| body.strip_prefix(p));
        match (heading, task) {
            (Some(h), _) => inline_plain(h, &mut out),
            (_, Some(t)) => {
                out.push_str(indent);
                inline_plain(t, &mut out);
            }
            _ => inline_plain(line, &mut out),
        }
    }
    out
}

fn inline_plain(s: &str, out: &mut String) {
    let cs: Vec<char> = s.chars().collect();
    let mut i = 0;
    let word = |c: Option<&char>| c.is_some_and(|c| c.is_alphanumeric() || *c == '_');
    while i < cs.len() {
        match cs[i] {
            '<' if s_at(&cs, i, "<u>") => i += 3,
            '<' if s_at(&cs, i, "</u>") => i += 4,
            '[' => {
                // [text](target): the text.
                let close = cs[i + 1..].iter().position(|&c| c == ']' || c == '\n');
                let done = close.and_then(|c| {
                    let c = i + 1 + c;
                    if cs[c] != ']' || c == i + 1 || cs.get(c + 1) != Some(&'(') {
                        return None;
                    }
                    let end = cs[c + 2..]
                        .iter()
                        .position(|&x| x == ')' || x.is_whitespace())?;
                    let end = c + 2 + end;
                    (cs[end] == ')' && end > c + 2).then_some((c, end))
                });
                if let Some((c, end)) = done {
                    out.extend(&cs[i + 1..c]);
                    i = end + 1;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            '*' if cs.get(i + 1) == Some(&'*') => i += 2,
            '*' => {
                let prev = i.checked_sub(1).and_then(|p| cs.get(p));
                let next = cs.get(i + 1);
                let opens =
                    !word(prev) && prev != Some(&'*') && next.is_some_and(|c| !c.is_whitespace());
                let closes =
                    prev.is_some_and(|c| !c.is_whitespace()) && !word(next) && next != Some(&'*');
                if !opens && !closes {
                    out.push('*');
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
}

fn s_at(cs: &[char], i: usize, s: &str) -> bool {
    s.chars()
        .enumerate()
        .all(|(k, c)| cs.get(i + k) == Some(&c))
}

/// One block of a note read as a document (`parseKeepText`, its line-level part).
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(u8, String),
    Task(bool, String),
    Bullet(String, u8, String),
    Code(String),
    Para(String),
    Blank,
}

/// Notes from Lucina and ChatGPT are real markdown documents: fenced code and `###` on top of Keep's own subset.
pub fn parse_blocks(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut code: Option<String> = None;
    for raw in md.split('\n') {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            match code.take() {
                Some(c) => out.push(Block::Code(c)),
                None => code = Some(String::new()),
            }
            continue;
        }
        if let Some(c) = code.as_mut() {
            if !c.is_empty() {
                c.push('\n');
            }
            c.push_str(line);
            continue;
        }
        if line.trim().is_empty() {
            if !matches!(out.last(), Some(Block::Blank)) {
                out.push(Block::Blank);
            }
            continue;
        }
        let hashes = line.bytes().take_while(|&b| b == b'#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            out.push(Block::Heading(
                hashes as u8,
                plain_inline(line[hashes..].trim()),
            ));
            continue;
        }
        let body = line.trim_start();
        let spaces = line.len() - body.len();
        if let Some(r) = body
            .strip_prefix("- [")
            .or_else(|| body.strip_prefix("* ["))
            .or_else(|| body.strip_prefix("+ ["))
        {
            let mut c = r.chars();
            if let (Some(m @ (' ' | 'x' | 'X')), Some(']'), Some(' ')) =
                (c.next(), c.next(), c.next())
            {
                out.push(Block::Task(m != ' ', plain_inline(c.as_str())));
                continue;
            }
        }
        if let Some(r) = body
            .strip_prefix("- ")
            .or_else(|| body.strip_prefix("* "))
            .or_else(|| body.strip_prefix("+ "))
        {
            out.push(Block::Bullet(
                "•".into(),
                (spaces / 2).min(2) as u8,
                plain_inline(r),
            ));
            continue;
        }
        let digits = body.bytes().take_while(u8::is_ascii_digit).count();
        if (1..=3).contains(&digits)
            && matches!(body.as_bytes().get(digits), Some(b'.' | b')'))
            && body.as_bytes().get(digits + 1) == Some(&b' ')
        {
            out.push(Block::Bullet(
                body[..=digits].to_owned(),
                (spaces / 2).min(2) as u8,
                plain_inline(&body[digits + 2..]),
            ));
            continue;
        }
        if let Some(q) = body.strip_prefix("> ") {
            out.push(Block::Para(plain_inline(q)));
            continue;
        }
        out.push(Block::Para(plain_inline(line)));
    }
    if let Some(c) = code {
        out.push(Block::Code(c));
    }
    while matches!(out.last(), Some(Block::Blank)) {
        out.pop();
    }
    while matches!(out.first(), Some(Block::Blank)) {
        out.remove(0);
    }
    out
}

fn plain_inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    inline_plain(s, &mut out);
    out.replace('`', "")
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

/// `hasUrl` looks for links with a markdown parser; a link in a note always holds one of these.
fn has_link(s: &str) -> bool {
    s.contains("https://") || s.contains("http://")
}

/// A note and what the screen and the search need from it, worked out once.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub note: Note,
    /// `searchText`, folded: title, text, items, labels, link titles and hosts, file names.
    pub hay: String,
    pub title_fold: String,
    pub labels_fold: String,
    /// The start of the text for a card, without the marks.
    pub preview: String,
    pub reminder_ms: Option<i64>,
}

/// About what a card can show: wrapping more on every frame only costs time.
const PREVIEW_CHARS: usize = 260;

impl Entry {
    pub fn new(note: Note) -> Arc<Entry> {
        let (_, body) = split_title_heading(&note.content, &note.title);
        let plain = plain_text(body);
        let mut parts: Vec<&str> = vec![&note.title, &plain];
        for i in note.items.iter().flatten() {
            parts.push(&i.text);
        }
        for t in &note.tags {
            parts.push(t);
        }
        for a in &note.annotations {
            parts.push(a.title.as_deref().unwrap_or(""));
            parts.push(a.description.as_deref().unwrap_or(""));
            parts.push(host_of(&a.url));
        }
        for a in &note.attachments {
            parts.push(a.name.as_deref().unwrap_or(""));
        }
        let hay = fold_note(&parts.join("\n"));
        let title_fold = fold_note(&note.title);
        let labels_fold = fold_note(&note.tags.join("\n"));
        let preview: String = plain.trim().chars().take(PREVIEW_CHARS).collect();
        let reminder_ms = note.reminder.as_ref().and_then(|r| parse_iso(&r.at));
        Arc::new(Entry {
            hay,
            title_fold,
            labels_fold,
            preview,
            reminder_ms,
            note,
        })
    }
}

/// Which notes the screen shows (`KeepView`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Notes,
    Reminders,
    Archive,
    Trash,
    Label(String),
}

/// `NOTE_TYPES` on the search page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteType {
    Reminders,
    Lists,
    Images,
    Audio,
    Urls,
}

pub fn matches_type(n: &Note, t: NoteType) -> bool {
    match t {
        NoteType::Reminders => n.reminder.is_some(),
        NoteType::Lists => n.items.is_some(),
        NoteType::Images => n
            .attachments
            .iter()
            .any(|a| a.mimetype.starts_with("image/")),
        NoteType::Audio => n
            .attachments
            .iter()
            .any(|a| a.mimetype.starts_with("audio/")),
        NoteType::Urls => {
            !n.annotations.is_empty()
                || has_link(&n.content)
                || n.items.iter().flatten().any(|i| has_link(&i.text))
        }
    }
}

/// `byPosition`: higher position first; ties fall back to the newest, then the id.
pub fn by_position(a: &Note, b: &Note) -> Ordering {
    b.position
        .partial_cmp(&a.position)
        .unwrap_or(Ordering::Equal)
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.id.cmp(&b.id))
}

/// `viewNotes`: Notes, Archive and labels by position; Reminders by time; Trash by deletion.
pub fn view_notes<'a>(all: impl Iterator<Item = &'a Arc<Entry>>, view: &View) -> Vec<Arc<Entry>> {
    let mut list: Vec<Arc<Entry>> = all
        .filter(|e| {
            let n = &e.note;
            let trashed = n.deleted_at.is_some();
            match view {
                View::Notes => !n.archived && !trashed,
                View::Archive => n.archived && !trashed,
                View::Trash => trashed,
                View::Reminders => n.reminder.is_some() && !trashed,
                View::Label(t) => n.tags.contains(t) && !trashed,
            }
        })
        .cloned()
        .collect();
    match view {
        View::Reminders => list.sort_by(|a, b| {
            a.reminder_ms
                .cmp(&b.reminder_ms)
                .then_with(|| by_position(&a.note, &b.note))
        }),
        View::Trash => list.sort_by(|a, b| b.note.deleted_at.cmp(&a.note.deleted_at)),
        _ => list.sort_by(|a, b| by_position(&a.note, &b.note)),
    }
    list
}

/// `queryScore`: 0 when some word is missing, else higher for title and label hits and for whole words.
pub fn query_score(e: &Entry, q: &str) -> u32 {
    let folded = fold_note(q);
    let mut score = 0;
    let mut any = false;
    for w in folded.split_whitespace() {
        any = true;
        let in_all = word_match(&e.hay, w);
        if in_all == 0 {
            return 0;
        }
        score += u32::from(in_all)
            + 6 * u32::from(word_match(&e.title_fold, w))
            + 3 * u32::from(word_match(&e.labels_fold, w));
    }
    if any { score } else { 1 }
}

/// `searchNotes`: across Notes, Archive and labels, never the trash. With words, the best matches first, pinned or not;
/// filters alone keep the grid order.
pub fn search_notes<'a>(
    all: impl Iterator<Item = &'a Arc<Entry>>,
    q: &str,
    ty: Option<NoteType>,
) -> Vec<Arc<Entry>> {
    let q = q.trim();
    let mut out: Vec<(u32, Arc<Entry>)> = all
        .filter(|e| e.note.deleted_at.is_none() && ty.is_none_or(|t| matches_type(&e.note, t)))
        .filter_map(|e| {
            let s = if q.is_empty() { 1 } else { query_score(e, q) };
            (s > 0).then(|| (s, e.clone()))
        })
        .collect();
    out.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| by_position(&a.1.note, &b.1.note))
    });
    out.into_iter().map(|(_, e)| e).collect()
}

/// A label with how many live notes use it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Label {
    pub tag: String,
    pub name: String,
    pub count: u32,
}

/// Who wrote a note, as `web/src/views/Notes.tsx` `SOURCE_KEYS` says it.
pub fn source_label(source: &str) -> &str {
    match source {
        "owner" | "web" | "ios" | "juno" => "You",
        "lucina" => "Lucina",
        "chatgpt" => "ChatGPT",
        "grok" => "Grok",
        "claude-code" | "claude" => "Claude",
        s => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, f: impl FnOnce(&mut Note)) -> Arc<Entry> {
        let mut n = Note {
            id: id.into(),
            color: "default".into(),
            source: "owner".into(),
            position: 100.0,
            created_at: "2026-10-01T00:00:00.000Z".into(),
            updated_at: "2026-10-01T00:00:00.000Z".into(),
            ..Note::default()
        };
        f(&mut n);
        Entry::new(n)
    }

    // The note of docs/server-api.md, "Notes".
    #[test]
    fn parses_the_contracts_note() {
        let n = parse_note(&json!({
            "id": "n_3f9c0a7b12de45a8c901", "slug": "craftit-meeting",
            "title": "CRAFT-IT4SD — meeting with Marianne & Jonas", "content": "# Briefing\n\n…markdown…",
            "tags": ["work", "aarhus"], "source": "chatgpt", "pinned": true,
            "createdAt": "2026-10-22T11:30:00.123Z", "updatedAt": "2026-10-22T11:30:00.123Z",
            "color": "default", "archived": false, "attachments": [], "annotations": [], "collaborators": [],
            "position": 1761132600123.0, "rev": 17,
            "import": {"source": "google-keep"}, "unknownLater": true
        }))
        .unwrap();
        assert_eq!(n.slug, "craftit-meeting");
        assert!(n.pinned);
        assert_eq!(n.rev, 17);
        assert_eq!(n.items, None);
    }

    #[test]
    fn parse_is_lenient_like_parse_keep_note() {
        let n = parse_note(
            &json!({"id": "a", "createdAt": "1970-01-01T00:00:01.000Z", "color": "mauve",
            "items": [{"text": "milk", "indent": 7}]}),
        )
        .unwrap();
        assert_eq!(n.color, "default");
        assert_eq!(n.position, 1000.0);
        assert_eq!(n.source, "owner");
        let items = n.items.unwrap();
        assert!(items[0].id.starts_with("i_"));
        assert_eq!(items[0].indent, 0);
        assert!(parse_note(&json!({"title": "no id"})).is_none());
    }

    // docs/keep.md, "Checklists".
    #[test]
    fn checklists_become_content_and_back() {
        let items = vec![
            Item {
                id: "a".into(),
                text: "milk".into(),
                checked: false,
                indent: 0,
            },
            Item {
                id: "b".into(),
                text: "whole".into(),
                checked: true,
                indent: 1,
            },
        ];
        assert_eq!(items_to_content(&items), "- [ ] milk\n  - [x] whole");
        let back = content_to_items(
            "- [ ] milk\n  - [x] whole\n\nplain line\n* [X] star\n- bullet\n  deep",
        );
        let got: Vec<(&str, bool, u8)> = back
            .iter()
            .map(|i| (i.text.as_str(), i.checked, i.indent))
            .collect();
        assert_eq!(
            got,
            [
                ("milk", false, 0),
                ("whole", true, 1),
                ("plain line", false, 0),
                ("star", true, 0),
                ("bullet", false, 0),
                ("deep", false, 1)
            ]
        );
    }

    #[test]
    fn apply_input_converts_like_the_server() {
        let n = note("a", |n| n.content = "- [x] one\ntwo".into())
            .note
            .clone();
        let list = apply_input(
            &n,
            &Input {
                convert: Some("list".into()),
                ..Input::default()
            },
            "T",
        );
        assert_eq!(list.items.as_ref().unwrap().len(), 2);
        assert_eq!(list.content, "- [x] one\n- [ ] two");
        assert_eq!(list.updated_at, "T");
        let text = apply_input(
            &list,
            &Input {
                convert: Some("text".into()),
                ..Input::default()
            },
            "T",
        );
        assert_eq!(text.items, None);
        assert_eq!(text.content, "one\ntwo");
        let labelled = apply_input(
            &n,
            &Input {
                labels: Some(vec!["Work".into(), "#work ".into(), "Big  Plan".into()]),
                ..Input::default()
            },
            "T",
        );
        assert_eq!(labelled.tags, ["work", "big plan"]);
    }

    #[test]
    fn merge_keeps_the_last_body() {
        let mut a = Input {
            title: Some("t".into()),
            content: Some("one".into()),
            ..Input::default()
        };
        a.merge(Input {
            items: Some(vec![]),
            ..Input::default()
        });
        assert_eq!(a.content, None);
        assert_eq!(a.items, Some(vec![]));
        assert_eq!(a.title.as_deref(), Some("t"));
        a.merge(Input {
            content: Some("two".into()),
            ..Input::default()
        });
        assert_eq!(a.items, None);
        assert_eq!(a.content.as_deref(), Some("two"));
        // A reminder cleared stays cleared.
        a.merge(Input {
            reminder: Some(None),
            ..Input::default()
        });
        assert_eq!(a.to_json()["reminder"], Value::Null);
        assert!(a.to_json().as_object().unwrap().contains_key("reminder"));
    }

    #[test]
    fn a_title_heading_is_hidden_and_kept() {
        let (head, body) = split_title_heading("# Hello World\n\nbody here\nmore", "hello  world");
        assert_eq!(head, "# Hello World\n\n");
        assert_eq!(body, "body here\nmore");
        assert_eq!(
            split_title_heading("# Other\nbody", "Hello"),
            ("", "# Other\nbody")
        );
        assert_eq!(split_title_heading("anything", ""), ("", "anything"));
    }

    #[test]
    fn plain_text_drops_the_marks() {
        assert_eq!(
            plain_text(
                "# Head\n**bold** and *italic* <u>under</u> [link](https://a.b)\n- [x] done\n2 * 3 * 4"
            ),
            "Head\nbold and italic under link\ndone\n2 * 3 * 4"
        );
    }

    #[test]
    fn blocks_read_a_document() {
        let b = parse_blocks(
            "# H\n\ntext **b**\n- one\n  - two\n1. first\n- [x] t\n```\ncode\n  more\n```\n",
        );
        assert_eq!(
            b,
            [
                Block::Heading(1, "H".into()),
                Block::Blank,
                Block::Para("text b".into()),
                Block::Bullet("•".into(), 0, "one".into()),
                Block::Bullet("•".into(), 1, "two".into()),
                Block::Bullet("1.".into(), 0, "first".into()),
                Block::Task(true, "t".into()),
                Block::Code("code\n  more".into()),
            ]
        );
    }

    // web/src/lib/keepFilter.test.ts.
    #[test]
    fn views_sort_like_keep() {
        let all = [
            note("old", |n| n.position = 1.0),
            note("new", |n| n.position = 9.0),
            note("arch", |n| n.archived = true),
            note("bin", |n| {
                n.deleted_at = Some("2026-10-02T00:00:00Z".into())
            }),
            note("bin2", |n| {
                n.deleted_at = Some("2026-10-03T00:00:00Z".into())
            }),
            note("soon", |n| {
                n.position = 2.0;
                n.tags = vec!["work".into()];
                n.reminder = Some(Reminder {
                    at: "2026-11-01T09:00:00Z".into(),
                    recurrence: None,
                });
            }),
            note("sooner", |n| {
                n.position = 3.0;
                n.reminder = Some(Reminder {
                    at: "2026-10-01T09:00:00Z".into(),
                    recurrence: None,
                });
            }),
        ];
        let ids = |v: &View| {
            view_notes(all.iter(), v)
                .iter()
                .map(|e| e.note.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&View::Notes), ["new", "sooner", "soon", "old"]);
        assert_eq!(ids(&View::Archive), ["arch"]);
        assert_eq!(ids(&View::Trash), ["bin2", "bin"]);
        assert_eq!(ids(&View::Reminders), ["sooner", "soon"]);
        assert_eq!(ids(&View::Label("work".into())), ["soon"]);
    }

    #[test]
    fn search_ranks_title_and_label_hits_and_skips_the_trash() {
        let all = [
            note("body", |n| {
                n.content = "rust in the text".into();
                n.position = 9.0;
            }),
            note("title", |n| n.title = "Rust notes".into()),
            note("label", |n| n.tags = vec!["rust".into()]),
            note("trust", |n| n.title = "Trust and frustration".into()),
            note("gone", |n| {
                n.title = "rust".into();
                n.deleted_at = Some("x".into());
            }),
            note("caffe", |n| n.title = "Caffè".into()),
            note("two", |n| n.content = "- alpha\n- beta".into()),
        ];
        let ids = |q: &str| {
            search_notes(all.iter(), q, None)
                .iter()
                .map(|e| e.note.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("rust"), ["title", "label", "body"]);
        assert_eq!(ids("CAFFE"), ["caffe"]);
        assert_eq!(ids("alpha bet"), ["two"]);
        assert_eq!(ids("alpha missing"), Vec::<String>::new());
    }

    #[test]
    fn type_filters_work_without_words() {
        let all = [
            note("list", |n| n.items = Some(vec![])),
            note("link", |n| n.content = "see https://x.y".into()),
            note("pic", |n| {
                n.attachments = vec![Attachment {
                    mimetype: "image/png".into(),
                    ..Attachment::default()
                }]
            }),
        ];
        let ids = |t| {
            search_notes(all.iter(), "", Some(t))
                .iter()
                .map(|e| e.note.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(NoteType::Lists), ["list"]);
        assert_eq!(ids(NoteType::Urls), ["link"]);
        assert_eq!(ids(NoteType::Images), ["pic"]);
        assert!(ids(NoteType::Audio).is_empty());
    }

    #[test]
    fn labels_normalise_like_the_server() {
        assert_eq!(norm_label("  #Big   Plan "), "big plan");
        assert_eq!(clean_label("  #Big   Plan "), "Big Plan");
    }
}
