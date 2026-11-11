//! `notebooksApi` in web/src/api/notebooks.ts: a request per function, and the worker that sends them. The screen never waits:
//! each answer arrives as a [`Msg`] with the [`Tag`] it was asked under, and the screen drops the ones about what it left.

use lucina_account::{ApiError, Client};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::wire::{Chat, ChatDetail, Chunk, Detail, Exchange, Hit, Insight, NoteLink, Notebook, Podcast, Profile, Source, Transformation, profiles};

/// A view and what was last seen of the notebooks, in RAM beside the token.
pub const CACHE_FILE: &str = "notebooks-cache.json";
const BASE: &str = "/v1/notebooks";
const ENDED_EARLY: &str = "The reply ended early. Try again.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// No answer, or 502/503/504.
    Offline(String),
    /// 401: the device token is revoked.
    SignedOut,
    /// The server looked at it and said no.
    Refused { status: u16, message: String },
}

impl Failure {
    pub fn text(&self) -> String {
        match self {
            Failure::Offline(m) | Failure::Refused { message: m, .. } => m.clone(),
            Failure::SignedOut => "This device is signed out.".into(),
        }
    }

    pub fn not_found(&self) -> bool {
        matches!(self, Failure::Refused { status: 404, .. })
    }
}

fn from_api(e: ApiError) -> Failure {
    match e {
        ApiError::Network(m) => Failure::Offline(m),
        ApiError::RateLimited(s) => Failure::Offline(format!("Too many requests; wait {s} s.")),
        ApiError::Http(502..=504, _, m) => Failure::Offline(if m.is_empty() { "The server is not answering.".into() } else { m }),
        ApiError::Unauthorized(_) => Failure::SignedOut,
        ApiError::Http(status, _, message) => Failure::Refused { status, message },
        ApiError::Protocol(message) => Failure::Refused { status: 0, message },
    }
}

/// `encodeURIComponent`.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'!' | b'*' | b'\'' | b'(' | b')') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    List(Vec<Notebook>),
    Notebook(Box<Notebook>),
    Detail(Box<Detail>),
    Source(Box<Source>),
    Chunks(Vec<Chunk>),
    Insights(Vec<Insight>),
    Insight(Box<Insight>),
    Transformations(Vec<Transformation>),
    Chats(Vec<Chat>),
    ChatRow(Box<Chat>),
    Chat(Box<ChatDetail>),
    Exchange(Box<Exchange>),
    Link(Box<NoteLink>),
    Hits(Vec<Hit>),
    Notes(Vec<NoteLink>),
    Profiles(Vec<Profile>, Vec<Profile>),
    Podcast(Box<Podcast>),
    /// What the screen does not read.
    Done,
}

/// Which earlier passages or later ones a chunks request was for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paging {
    Open,
    Earlier,
    Later,
}

/// What an answer is about.
#[derive(Clone, Debug, PartialEq)]
pub enum Tag {
    List { archived: bool },
    Detail { notebook: String },
    Created,
    SettingsSaved,
    Archived,
    Deleted,
    SourceAdded,
    SourceChanged,
    Source { source: String },
    Chunks { source: String, paging: Paging },
    Insights { source: String },
    InsightRun { source: String },
    InsightNote { insight: String },
    InsightDeleted { insight: String },
    Transformations,
    NoteSaved,
    NoteUnlinked,
    Search { query: String },
    PickNotes { query: String },
    Chats { notebook: String },
    ChatLoaded { chat: String },
    ChatCreated { notebook: String },
    ChatRenamed { chat: String },
    ChatDeleted { chat: String },
    Asked { chat: Option<String>, text: String },
    MessageNote { message: String },
    Profiles,
    PodcastMade,
    PodcastDeleted,
}

fn parse<T: DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("unexpected answer: {e}"))
}

fn field<T: DeserializeOwned>(v: &Value, key: &str) -> Result<T, String> {
    parse(v.get(key).cloned().unwrap_or_else(|| json!([])))
}

pub type Decode = fn(Value) -> Result<Answer, String>;

/// One request.
pub struct Call {
    pub tag: Tag,
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
    pub decode: Decode,
    /// Saving a reply to a note may wait on Keep and the embedder: the web gives it two minutes.
    pub long: bool,
}

fn get(tag: Tag, path: String, decode: Decode) -> Call {
    Call { tag, method: "GET", path, body: None, decode, long: false }
}

fn send(tag: Tag, method: &'static str, path: String, body: Value, decode: Decode) -> Call {
    Call { tag, method, path, body: Some(body), decode, long: false }
}

fn done(_: Value) -> Result<Answer, String> {
    Ok(Answer::Done)
}

fn a_notebook(v: Value) -> Result<Answer, String> {
    Ok(Answer::Notebook(Box::new(parse(v)?)))
}

fn a_source(v: Value) -> Result<Answer, String> {
    Ok(Answer::Source(Box::new(parse(v)?)))
}

fn a_link(v: Value) -> Result<Answer, String> {
    Ok(Answer::Link(Box::new(parse(v)?)))
}

fn a_chat_row(v: Value) -> Result<Answer, String> {
    Ok(Answer::ChatRow(Box::new(parse(v)?)))
}

pub mod api {
    use super::*;

    pub fn list(archived: bool) -> Call {
        let path = if archived { format!("{BASE}?archived=only") } else { BASE.to_owned() };
        get(Tag::List { archived }, path, |v| Ok(Answer::List(field(&v, "notebooks")?)))
    }

    pub fn create(name: &str, description: &str) -> Call {
        send(Tag::Created, "POST", BASE.into(), json!({"name": name, "description": description}), a_notebook)
    }

    pub fn detail(id: &str) -> Call {
        get(Tag::Detail { notebook: id.into() }, format!("{BASE}/{}", enc(id)), |v| {
            Ok(Answer::Detail(Box::new(parse(v)?)))
        })
    }

    pub fn save_settings(id: &str, name: &str, description: &str) -> Call {
        let body = json!({"name": name, "description": description});
        send(Tag::SettingsSaved, "PATCH", format!("{BASE}/{}", enc(id)), body, a_notebook)
    }

    pub fn archive(id: &str, archived: bool) -> Call {
        send(Tag::Archived, "PATCH", format!("{BASE}/{}", enc(id)), json!({"archived": archived}), a_notebook)
    }

    pub fn delete(id: &str, delete_exclusive: bool) -> Call {
        let sources = if delete_exclusive { "delete-exclusive" } else { "keep" };
        send(Tag::Deleted, "DELETE", format!("{BASE}/{}?sources={sources}", enc(id)), Value::Null, done)
    }

    pub fn add_source(id: &str, body: Value) -> Call {
        send(Tag::SourceAdded, "POST", format!("{BASE}/{}/sources", enc(id)), body, a_source)
    }

    pub fn set_context(id: &str, source: &str, context: &str) -> Call {
        let path = format!("{BASE}/{}/sources/{}", enc(id), enc(source));
        send(Tag::SourceChanged, "PATCH", path, json!({"context": context}), done)
    }

    pub fn remove_source(id: &str, source: &str) -> Call {
        send(Tag::SourceChanged, "DELETE", format!("{BASE}/{}/sources/{}", enc(id), enc(source)), Value::Null, done)
    }

    pub fn retry_source(source: &str) -> Call {
        send(Tag::SourceChanged, "POST", format!("{BASE}/sources/{}/retry", enc(source)), json!({}), done)
    }

    pub fn source(source: &str) -> Call {
        get(Tag::Source { source: source.into() }, format!("{BASE}/sources/{}", enc(source)), a_source)
    }

    pub fn chunks(source: &str, query: &str, paging: Paging) -> Call {
        let tag = Tag::Chunks { source: source.into(), paging };
        get(tag, format!("{BASE}/sources/{}/chunks?{query}", enc(source)), |v| Ok(Answer::Chunks(field(&v, "chunks")?)))
    }

    pub fn insights(source: &str) -> Call {
        let tag = Tag::Insights { source: source.into() };
        get(tag, format!("{BASE}/sources/{}/insights", enc(source)), |v| Ok(Answer::Insights(field(&v, "insights")?)))
    }

    pub fn run_transformation(source: &str, transformation: &str) -> Call {
        let path = format!("{BASE}/sources/{}/insights", enc(source));
        send(Tag::InsightRun { source: source.into() }, "POST", path, json!({"transformation_id": transformation}), |v| {
            Ok(Answer::Insight(Box::new(parse(v)?)))
        })
    }

    pub fn insight_to_note(insight: &str) -> Call {
        let tag = Tag::InsightNote { insight: insight.into() };
        Call { long: true, ..send(tag, "POST", format!("{BASE}/insights/{}/note", enc(insight)), json!({}), a_link) }
    }

    pub fn remove_insight(insight: &str) -> Call {
        send(Tag::InsightDeleted { insight: insight.into() }, "DELETE", format!("{BASE}/insights/{}", enc(insight)), Value::Null, done)
    }

    pub fn transformations() -> Call {
        get(Tag::Transformations, format!("{BASE}/transformations"), |v| Ok(Answer::Transformations(field(&v, "transformations")?)))
    }

    pub fn link_note(id: &str, slug: &str) -> Call {
        Call { long: true, ..send(Tag::NoteSaved, "POST", format!("{BASE}/{}/notes", enc(id)), json!({"slug": slug}), a_link) }
    }

    pub fn write_note(id: &str, title: &str, text: &str) -> Call {
        let body = json!({"title": title, "text": text});
        Call { long: true, ..send(Tag::NoteSaved, "POST", format!("{BASE}/{}/notes", enc(id)), body, a_link) }
    }

    pub fn unlink_note(id: &str, slug: &str) -> Call {
        send(Tag::NoteUnlinked, "DELETE", format!("{BASE}/{}/notes/{}", enc(id), enc(slug)), Value::Null, done)
    }

    pub fn search(id: &str, query: &str) -> Call {
        let path = format!("{BASE}/{}/search?q={}&limit=20", enc(id), enc(query));
        get(Tag::Search { query: query.into() }, path, |v| Ok(Answer::Hits(field(&v, "hits")?)))
    }

    /// `notesApi.list`: Keep's notes, which live on p1 and not in the notebooks engine.
    pub fn pick_notes(query: &str) -> Call {
        let path = if query.trim().is_empty() { "/v1/notes".to_owned() } else { format!("/v1/notes?q={}", enc(query.trim())) };
        get(Tag::PickNotes { query: query.into() }, path, |v| {
            let list = v.get("notes").cloned().unwrap_or(v);
            Ok(Answer::Notes(parse(list)?))
        })
    }

    pub fn chats(id: &str) -> Call {
        get(Tag::Chats { notebook: id.into() }, format!("{BASE}/{}/chats", enc(id)), |v| Ok(Answer::Chats(field(&v, "chats")?)))
    }

    pub fn chat(chat: &str) -> Call {
        get(Tag::ChatLoaded { chat: chat.into() }, format!("{BASE}/chats/{}", enc(chat)), |v| {
            Ok(Answer::Chat(Box::new(parse(v)?)))
        })
    }

    pub fn rename_chat(chat: &str, title: &str) -> Call {
        send(Tag::ChatRenamed { chat: chat.into() }, "PATCH", format!("{BASE}/chats/{}", enc(chat)), json!({"title": title}), a_chat_row)
    }

    pub fn remove_chat(chat: &str) -> Call {
        send(Tag::ChatDeleted { chat: chat.into() }, "DELETE", format!("{BASE}/chats/{}", enc(chat)), Value::Null, done)
    }

    pub fn message_to_note(chat: &str, message: &str) -> Call {
        let path = format!("{BASE}/chats/{}/messages/{}/note", enc(chat), enc(message));
        Call { long: true, ..send(Tag::MessageNote { message: message.into() }, "POST", path, json!({}), a_link) }
    }

    pub fn profiles_of_podcasts() -> Call {
        get(Tag::Profiles, format!("{BASE}/podcast-profiles"), |v| {
            Ok(Answer::Profiles(profiles(v.get("episode_profiles")), profiles(v.get("speaker_profiles"))))
        })
    }

    pub fn create_podcast(id: &str, body: Value) -> Call {
        send(Tag::PodcastMade, "POST", format!("{BASE}/{}/podcasts", enc(id)), body, |v| {
            Ok(Answer::Podcast(Box::new(parse(v)?)))
        })
    }

    pub fn remove_podcast(podcast: &str) -> Call {
        send(Tag::PodcastDeleted, "DELETE", format!("{BASE}/podcasts/{}", enc(podcast)), Value::Null, done)
    }
}

pub enum Job {
    Call(Call),
    /// `sendMessage`: with no chat yet, one is made first, named after the question, as the web does.
    Ask { notebook: String, chat: Option<String>, text: String, model: &'static str },
}

impl Job {
    /// A reply takes as long as the model does: it gets a thread of its own, so the rest of the app goes on.
    pub fn is_long(&self) -> bool {
        matches!(self, Job::Ask { .. })
    }
}

pub enum Msg {
    /// The sign-in screen changed.
    Changed,
    SignedIn,
    SignedOut,
    /// A piece of the reply streaming into `chat`.
    Delta(String),
    Done(Tag, Result<Answer, Failure>),
}

fn run_call(client: &Client, call: &Call) -> Result<Answer, Failure> {
    let value = match (call.method, &call.body) {
        ("GET", _) => client.get_json(&call.path),
        ("POST", Some(b)) if call.long => client.post_stream(&call.path, b, &mut |_, _| {}).map(|v| v.unwrap_or(Value::Null)),
        ("POST", Some(b)) => client.post_json(&call.path, b),
        (m, b) => client.send_json(m, &call.path, b.as_ref().unwrap_or(&Value::Null)),
    }
    .map_err(from_api)?;
    (call.decode)(value).map_err(|message| Failure::Refused { status: 0, message })
}

fn ask(client: &Client, chat: &str, text: &str, model: &str, out: &mut dyn FnMut(Msg)) -> Result<Answer, Failure> {
    let mut done: Option<Result<Exchange, Failure>> = None;
    let path = format!("{BASE}/chats/{}/messages", enc(chat));
    let plain = client
        .post_stream(&path, &json!({"text": text, "model": model}), &mut |event, data| match event {
            "delta" => {
                let piece: Value = serde_json::from_str(data).unwrap_or(Value::Null);
                if let Some(p) = piece["text"].as_str() {
                    out(Msg::Delta(p.to_owned()));
                }
            }
            "message" => {
                done = Some(serde_json::from_str(data).map_err(|e| Failure::Refused { status: 0, message: format!("unexpected answer: {e}") }));
            }
            "error" => {
                let v: Value = serde_json::from_str(data).unwrap_or(Value::Null);
                let e = if v["error"].is_object() { &v["error"] } else { &v };
                let message = e["message"].as_str().unwrap_or("The reply failed.").to_owned();
                done = Some(Err(Failure::Refused { status: 500, message }));
            }
            _ => {}
        })
        .map_err(from_api)?;
    // A server that answered plain JSON instead of streaming works too.
    if let Some(v) = plain {
        return Ok(Answer::Exchange(Box::new(parse(v).map_err(|message| Failure::Refused { status: 0, message })?)));
    }
    match done {
        Some(Ok(e)) => Ok(Answer::Exchange(Box::new(e))),
        Some(Err(e)) => Err(e),
        None => Err(Failure::Offline(ENDED_EARLY.into())),
    }
}

/// Carries out one job. Every answer goes to `out`, in order.
pub fn handle(client: &Client, job: Job, out: &mut dyn FnMut(Msg)) {
    match job {
        Job::Call(call) => {
            let r = run_call(client, &call);
            finish(call.tag, r, out);
        }
        Job::Ask { notebook, chat, text, model } => {
            let chat_id = match chat {
                Some(c) => c,
                None => {
                    let title: String = text.chars().take(60).collect();
                    let call = send(Tag::ChatCreated { notebook: notebook.clone() }, "POST", format!("{BASE}/{}/chats", enc(&notebook)), json!({"title": title}), a_chat_row);
                    let r = run_call(client, &call);
                    let id = match &r {
                        Ok(Answer::ChatRow(c)) => c.id.clone(),
                        Err(_) | Ok(_) => {
                            let failure = r.err().unwrap_or(Failure::Offline(ENDED_EARLY.into()));
                            finish(Tag::Asked { chat: None, text }, Err(failure), out);
                            return;
                        }
                    };
                    finish(call.tag, r, out);
                    id
                }
            };
            let r = ask(client, &chat_id, &text, model, out);
            finish(Tag::Asked { chat: Some(chat_id), text }, r, out);
        }
    }
}

fn finish(tag: Tag, r: Result<Answer, Failure>, out: &mut dyn FnMut(Msg)) {
    if r == Err(Failure::SignedOut) {
        out(Msg::SignedOut);
    } else {
        out(Msg::Done(tag, r));
    }
}
