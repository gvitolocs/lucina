//! Lucina message format v1 (`docs/lucina-message-format.md` in SecretaryIOS; `web/src/lib/rich.ts`). It comes from the
//! network, so it is checked again here, and what does not fit is dropped, never the whole message.
use serde_json::Value;

pub const MAX_BUBBLES: usize = 4;
pub const MAX_BLOCKS: usize = 8;
pub const MAX_ACTIONS: usize = 4;
/// A list shows this many items before "and n more".
pub const LIST_VISIBLE: usize = 5;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rich {
    pub bubbles: Vec<String>,
    pub blocks: Vec<Block>,
    pub actions: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Checklist { title: Option<String>, items: Vec<(String, bool)> },
    Card { emoji: Option<String>, title: String, subtitle: Option<String>, body: Option<String>, tone: Tone },
    Stat { items: Vec<(String, String, Option<String>)> },
    List { title: Option<String>, items: Vec<(Option<String>, String, Option<String>)> },
    Table { title: Option<String>, columns: Vec<String>, rows: Vec<Vec<String>> },
    Steps { title: Option<String>, items: Vec<String> },
    Quote { text: String, cite: Option<String> },
    Link { url: String, title: String },
    /// Juno shows where it is and what it is called; there is no map here.
    Place { title: String, address: Option<String>, when: Option<String> },
    /// Juno shows the caption; pictures come later.
    Image { alt: String, caption: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Success,
    Warning,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    pub id: String,
    pub label: String,
    pub emoji: Option<String>,
    pub kind: ActionKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionKind {
    /// Send `text`, or with `prefill` put it in the composer.
    Reply { text: String, prefill: bool },
    /// A page of the app. Juno has few of them: the app says where it opens.
    Route { path: String },
    /// `yes` and `no` send `text`; `edit` puts it in the composer.
    Confirm { answer: String, text: Option<String> },
    Dismiss,
    /// Names only. With `request`, the inline paste-and-OK card; a value never travels in a message.
    Secret { request: Option<String>, name: Option<String> },
}

impl Action {
    /// The words this button sends (`actionText`).
    pub fn text(&self) -> Option<&str> {
        match &self.kind {
            ActionKind::Reply { text, .. } => Some(text),
            ActionKind::Confirm { text, .. } => text.as_deref(),
            _ => None,
        }
    }
}

fn cut(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.chars().take(max).collect())
}

fn valid_action_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 48 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

fn safe_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

fn valid_route(s: &str) -> bool {
    s.starts_with('/') && !s.contains("..") && !s.contains("://") && s.len() <= 600
}

impl Block {
    fn parse(v: &Value) -> Option<Block> {
        let title = || cut(&v["title"], 120);
        let items = || v["items"].as_array().filter(|a| !a.is_empty());
        Some(match v["type"].as_str()? {
            "checklist" => Block::Checklist {
                title: title(),
                items: items()?.iter().take(12).filter_map(|i| Some((cut(&i["text"], 400)?, i["done"].as_bool() == Some(true)))).collect(),
            },
            "card" => Block::Card {
                emoji: cut(&v["emoji"], 8),
                title: title()?,
                subtitle: cut(&v["subtitle"], 120),
                body: cut(&v["body"], 600),
                tone: match v["tone"].as_str() {
                    Some("success") => Tone::Success,
                    Some("warning") => Tone::Warning,
                    _ => Tone::Info,
                },
            },
            "stat" => {
                let one = |i: &Value| Some((cut(&i["label"], 40)?, cut(&i["value"], 40)?, cut(&i["delta"], 40)));
                let items: Vec<_> = match v["items"].as_array() {
                    Some(a) => a.iter().take(4).filter_map(one).collect(),
                    None => one(v).into_iter().collect(),
                };
                if items.is_empty() {
                    return None;
                }
                Block::Stat { items }
            }
            "list" => Block::List {
                title: title(),
                items: items()?.iter().take(20).filter_map(|i| Some((cut(&i["emoji"], 8), cut(&i["text"], 400)?, cut(&i["meta"], 40)))).collect(),
            },
            "table" => {
                let columns: Vec<String> = v["columns"].as_array()?.iter().take(4).filter_map(|c| cut(c, 80)).collect();
                if columns.is_empty() {
                    return None;
                }
                let rows = v["rows"]
                    .as_array()?
                    .iter()
                    .take(6)
                    .filter_map(|r| r.as_array())
                    .map(|r| r.iter().take(columns.len()).map(|c| cut(c, 80).unwrap_or_default()).collect())
                    .collect();
                Block::Table { title: title(), columns, rows }
            }
            "steps" => Block::Steps { title: title(), items: items()?.iter().take(8).filter_map(|i| cut(i, 400)).collect() },
            "quote" => Block::Quote { text: cut(&v["text"], 600)?, cite: cut(&v["cite"], 120) },
            "link_preview" => {
                let url = v["url"].as_str().filter(|u| safe_url(u))?.to_owned();
                Block::Link { url, title: title()? }
            }
            "place" => {
                let address = cut(&v["address"], 200);
                let coords = v["lat"].is_number() && v["lng"].is_number();
                if address.is_none() && !coords {
                    return None;
                }
                Block::Place { title: title()?, address, when: cut(&v["when"], 40) }
            }
            "image" => {
                let src = v["src"].as_str()?;
                if !(src.starts_with("/v1/") || src.starts_with("https://getsecretary.tech/")) || src.contains("..") || src.len() > 300 {
                    return None;
                }
                Block::Image { alt: cut(&v["alt"], 200).unwrap_or_default(), caption: cut(&v["caption"], 200) }
            }
            _ => return None,
        })
    }
}

impl Action {
    fn parse(v: &Value) -> Option<Action> {
        let id = v["id"].as_str().filter(|s| valid_action_id(s))?.to_owned();
        let label = cut(&v["label"], 40)?;
        let p = &v["payload"];
        let kind = match v["kind"].as_str()? {
            "reply" => ActionKind::Reply { text: cut(&p["text"], 4000)?, prefill: p["prefill"].as_bool() == Some(true) },
            "route" => ActionKind::Route { path: p["path"].as_str().filter(|s| valid_route(s))?.to_owned() },
            "confirm" => {
                let answer = p["answer"].as_str().filter(|a| matches!(*a, "yes" | "no" | "edit"))?.to_owned();
                ActionKind::Confirm { answer, text: cut(&p["text"], 4000) }
            }
            "dismiss" => ActionKind::Dismiss,
            // Any other field is dropped by construction, a value above all.
            "secret" => ActionKind::Secret { request: cut(&p["request"], 128), name: cut(&p["name"], 256) },
            _ => return None,
        };
        Some(Action { id, label, emoji: cut(&v["emoji"], 8), kind })
    }
}

impl Rich {
    /// From `meta` of a timeline item or a chat response. `None` unless `format` is `v1` and something survives.
    pub fn parse(v: &Value) -> Option<Rich> {
        if v["format"].as_str() != Some("v1") {
            return None;
        }
        let bubbles: Vec<String> = v["bubbles"].as_array()?.iter().take(MAX_BUBBLES).filter_map(|b| cut(b, 1200)).collect();
        let blocks: Vec<Block> = v["blocks"].as_array().map(|a| a.iter().filter_map(Block::parse).take(MAX_BLOCKS).collect()).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let actions: Vec<Action> = v["actions"]
            .as_array()
            .map(|a| a.iter().filter_map(Action::parse).filter(|x| seen.insert(x.id.clone())).take(MAX_ACTIONS).collect())
            .unwrap_or_default();
        if bubbles.is_empty() && blocks.is_empty() {
            return None;
        }
        Some(Rich { bubbles, blocks, actions })
    }

    /// The queue entry this message draws a card for, if any.
    pub fn secret_request(&self) -> Option<&str> {
        self.actions.iter().find_map(|a| match &a.kind {
            ActionKind::Secret { request: Some(r), .. } => Some(r.as_str()),
            _ => None,
        })
    }
}

/// A run of inline-marked text. Raw HTML has no meaning here: it is just text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
}

/// `**bold**`, `*italic*`/`_italic_`, `~~strike~~`, `` `code` ``, `[label](https://…)`; an unclosed mark stays as it is.
pub fn inline(text: &str) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut plain = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut rest = text;
    let flush = |plain: &mut String, out: &mut Vec<Span>, bold: bool, italic: bool, strike: bool| {
        if !plain.is_empty() {
            out.push(Span { text: std::mem::take(plain), bold, italic, strike, ..Span::default() });
        }
    };
    while let Some(c) = rest.chars().next() {
        if let Some(r) = rest.strip_prefix('`') {
            if let Some(end) = r.find('`') {
                flush(&mut plain, &mut out, bold, italic, strike);
                out.push(Span { text: r[..end].to_owned(), code: true, ..Span::default() });
                rest = &r[end + 1..];
                continue;
            }
        }
        if let Some(r) = rest.strip_prefix('[') {
            if let Some((label, after)) = r.split_once("](") {
                if !label.contains(']') {
                    if let Some((url, tail)) = after.split_once(')') {
                        if safe_url(url) || url.starts_with("mailto:") {
                            flush(&mut plain, &mut out, bold, italic, strike);
                            out.push(Span { text: label.to_owned(), link: Some(url.to_owned()), bold, italic, ..Span::default() });
                            rest = tail;
                            continue;
                        }
                    }
                }
            }
        }
        let closes = |mark: &str, r: &str| bold_close(mark, r);
        if let Some(r) = rest.strip_prefix("**") {
            if bold || closes("**", r) {
                flush(&mut plain, &mut out, bold, italic, strike);
                bold = !bold;
                rest = r;
                continue;
            }
        }
        if let Some(r) = rest.strip_prefix("~~") {
            if strike || closes("~~", r) {
                flush(&mut plain, &mut out, bold, italic, strike);
                strike = !strike;
                rest = r;
                continue;
            }
        }
        if c == '*' || c == '_' {
            let r = &rest[1..];
            let mark = &rest[..1];
            let word_inside = c == '_' && plain.chars().last().is_some_and(char::is_alphanumeric);
            let doubled = c == '*' && r.starts_with('*');
            if !word_inside && !doubled && (italic || closes(mark, r)) {
                flush(&mut plain, &mut out, bold, italic, strike);
                italic = !italic;
                rest = r;
                continue;
            }
        }
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut plain, &mut out, bold, italic, strike);
    out
}

fn bold_close(mark: &str, rest: &str) -> bool {
    !rest.starts_with(char::is_whitespace) && rest.contains(mark)
}

/// The decision this message's buttons answer, if they do.
impl Rich {
    pub fn decision(&self) -> Option<&str> {
        self.actions.iter().find_map(|a| a.text().and_then(crate::decisions::answers_decision))
    }
}

/// The text without marks.
pub fn plain(text: &str) -> String {
    inline(text).into_iter().map(|s| s.text).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_does_not_fit_is_dropped_not_the_message() {
        let r = Rich::parse(&json!({
            "format": "v1",
            "bubbles": ["Ecco", "", 3],
            "blocks": [
                {"type": "checklist", "items": [{"text": "a", "done": true}, {"nope": 1}]},
                {"type": "unknown"},
                {"type": "link_preview", "url": "javascript:alert(1)", "title": "x"},
                {"type": "image", "src": "https://evil.example/x.png"},
                {"type": "stat", "label": "Peso", "value": "70 kg"},
            ],
            "actions": [
                {"id": "a", "label": "Sì", "kind": "reply", "payload": {"text": "sì"}},
                {"id": "a", "label": "Again", "kind": "dismiss"},
                {"id": "bad id!", "label": "x", "kind": "dismiss"},
                {"id": "r", "label": "x", "kind": "route", "payload": {"path": "https://evil.example"}},
                {"id": "s", "label": "Key", "kind": "secret", "payload": {"mode": "save", "value": "hunter2", "request": "req1"}},
            ],
        })).unwrap();
        assert_eq!(r.bubbles, ["Ecco"]);
        assert_eq!(r.blocks.len(), 2);
        assert_eq!(r.actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["a", "s"]);
        assert_eq!(r.secret_request(), Some("req1"));
        assert!(!format!("{r:?}").contains("hunter2"));
        assert!(Rich::parse(&json!({"format": "v2", "bubbles": ["x"]})).is_none());
        assert!(Rich::parse(&json!({"format": "v1", "bubbles": []})).is_none());
    }

    #[test]
    fn inline_marks() {
        let s = inline("Ecco **ora** e *qui* `x*y` [sito](https://a.b) ~~no~~ a*b 2 * 3 snake_case_name");
        let of = |t: &str| s.iter().find(|x| x.text == t).unwrap();
        assert!(of("ora").bold);
        assert!(of("qui").italic);
        assert!(of("x*y").code);
        assert_eq!(of("sito").link.as_deref(), Some("https://a.b"));
        assert!(of("no").strike);
        assert_eq!(plain("**a** <b>c</b>"), "a <b>c</b>");
        assert_eq!(plain("snake_case_name and **open"), "snake_case_name and **open");
        assert_eq!(plain("[x](javascript:y)"), "[x](javascript:y)");
    }
}
