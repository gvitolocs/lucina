//! What `web/src/api/notebooks.ts` calls its types, as `crates/lucina-notebooks` writes them. `tests.rs` reads them from the
//! real engine. That crate is a server (axum, SQLite, tokio), so its types are not linked into this program.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    #[serde(default)]
    pub sources: u32,
    #[serde(default)]
    pub notes: u32,
    #[serde(default)]
    pub chats: u32,
    #[serde(default)]
    pub podcasts: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Notebook {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub counts: Counts,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub origin: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub chunks: u32,
    #[serde(default)]
    pub pages: Option<u32>,
    #[serde(default)]
    pub duration_s: Option<f64>,
    /// Only in a notebook's listing.
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NoteLink {
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub excerpt: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Citation {
    #[serde(rename = "ref")]
    pub reference: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub chunk: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub page: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub citations: Vec<Citation>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatDetail {
    pub id: String,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Exchange {
    pub user: Message,
    pub reply: Message,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub id: String,
    pub ordinal: u32,
    pub text: String,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub start_s: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Transformation {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Insight {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub speaker: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Podcast {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub episode_profile: String,
    #[serde(default)]
    pub speaker_profile: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub transcript: Vec<Line>,
    #[serde(default)]
    pub duration_s: Option<f64>,
    #[serde(default)]
    pub audio: bool,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Detail {
    #[serde(flatten)]
    pub notebook: Notebook,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub notes: Vec<NoteLink>,
    #[serde(default)]
    pub chats: Vec<Chat>,
    #[serde(default)]
    pub podcasts: Vec<Podcast>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Hit {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    #[serde(default)]
    pub source_id: Option<String>,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub excerpt: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Profile {
    pub id: String,
    pub name: String,
}

/// `profiles` in web/src/api/notebooks.ts: plain names, or objects with a name, and maybe an id.
pub fn profiles(list: Option<&Value>) -> Vec<Profile> {
    let mut out = Vec::new();
    for raw in list.and_then(Value::as_array).into_iter().flatten() {
        if let Some(s) = raw.as_str() {
            out.push(Profile { id: s.to_owned(), name: s.to_owned() });
            continue;
        }
        let first = |keys: &[&str]| keys.iter().find_map(|k| raw[*k].as_str().filter(|s| !s.is_empty())).map(str::to_owned);
        if let Some(id) = first(&["id", "name", "slug"]) {
            let name = first(&["name", "title", "id"]).unwrap_or_else(|| id.clone());
            out.push(Profile { id, name });
        }
    }
    out
}

pub fn source_pending(status: &str) -> bool {
    matches!(status, "queued" | "extracting" | "embedding")
}

pub fn insight_pending(status: &str) -> bool {
    matches!(status, "queued" | "running")
}

pub fn podcast_pending(status: &str) -> bool {
    matches!(status, "queued" | "outline" | "script" | "voicing" | "mixing")
}

impl Detail {
    /// `hasPending`: the page polls until nothing is being worked on.
    pub fn has_pending(&self) -> bool {
        self.sources.iter().any(|s| source_pending(&s.status)) || self.podcasts.iter().any(|p| podcast_pending(&p.status))
    }
}

/// The chunk ordinal of a `source:<source id>-<ordinal>` ref.
pub fn chunk_ordinal(reference: &str) -> Option<u32> {
    reference.rsplit_once('-')?.1.parse().ok()
}

/// The source id of a `source:<source id>-<ordinal>` ref.
pub fn ref_source_id(reference: &str) -> Option<String> {
    let rest = reference.strip_prefix("source:")?;
    let (id, ordinal) = rest.rsplit_once('-')?;
    ordinal.parse::<u32>().ok()?;
    Some(id.to_owned())
}

pub fn kind_label(kind: &str) -> &'static str {
    match kind {
        "pdf" => "PDF",
        "youtube" => "YouTube",
        "web" => "Web page",
        "text" => "Text",
        "audio" => "Audio",
        "note" => "Keep note",
        _ => "File",
    }
}

/// `SOURCE_STATUS_KEY`, `INSIGHT_STATUS_KEY` and `PODCAST_STATUS_KEY` in one: no status means two things.
pub fn status_label(status: &str) -> &'static str {
    match status {
        "queued" => "Queued",
        "extracting" => "Reading…",
        "embedding" => "Indexing…",
        "ready" => "Ready",
        "failed" => "Failed",
        "running" => "Writing…",
        "outline" => "Outlining…",
        "script" => "Writing the script…",
        "voicing" => "Voicing…",
        "mixing" => "Mixing…",
        _ => "Working…",
    }
}

pub fn context_label(context: &str) -> &'static str {
    match context {
        "summary" => "Summary",
        "off" => "Off",
        _ => "Full text",
    }
}

/// The next of full, summary, off, for a button that goes round them.
pub fn next_context(context: &str) -> &'static str {
    match context {
        "summary" => "off",
        "off" => "full",
        _ => "summary",
    }
}

/// `originLabel`: the host of a link without `www.`, or the origin without its `upload:` or `note:`.
pub fn origin_label(origin: &str) -> String {
    let lower = origin.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) else {
        return origin.strip_prefix("upload:").or_else(|| origin.strip_prefix("note:")).unwrap_or(origin).to_owned();
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host).to_owned()
}

/// `clock`: `h:mm:ss` or `m:ss`.
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0).round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// `{zero, one, other}` of the web's dictionary.
pub fn count(n: u32, zero: &str, one: &str, other: &str) -> String {
    match n {
        0 => zero.to_owned(),
        1 => format!("1 {one}"),
        _ => format!("{n} {other}"),
    }
}

pub fn counts_line(c: &Counts) -> String {
    [
        count(c.sources, "No sources", "source", "sources"),
        count(c.notes, "No notes", "note", "notes"),
        count(c.chats, "No chats", "chat", "chats"),
    ]
    .join(" · ")
}
