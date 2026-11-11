//! The intents Notes offers Lucina (docs/research/juno-intents.md): `notes.create`, `notes.trash`, `notes.restore`,
//! `notes.search`, `notes.open`. Each calls the same store functions the buttons and keys call. A write that the server has
//! not answered yet keeps its [`Responder`] in [`Notes::pending`] and is answered from `App::message`, once the store has
//! the server's answer (the real id for a create, the accepted change for a trash or a restore, the loaded notes for a
//! search), so a headless run — which never calls `view` — gets its answer too.

use lucina_app::intent::{
    ArgSpec, Call, Effect, Entity, GONE, IntentSpec, Reply, Responder, Undo, Write, WriteKind,
    rules, there,
};
use serde_json::{Value, json};

use crate::editor::Editor;
use crate::model::{Entry, Input, search_notes};
use crate::store::Op;
use crate::{Mode, Notes};

/// What Notes offers, listed by `<program> --intents` and named in `manifest.app`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "notes.create",
        description: "Make a note with the given title and text, and return its id once the server has it.",
        args: &[
            ArgSpec::text("title", "Its title.", false, 1000),
            ArgSpec::text("body", "Its text.", true, 20_000),
        ],
        effect: Effect::Write,
        shows: false,
        inverse: Some("notes.trash"),
    },
    IntentSpec {
        name: "notes.trash",
        description: "Move a note to the trash; the undo of notes.create.",
        args: &[ArgSpec::id("id", "The note's id.", true)],
        effect: Effect::Destructive,
        shows: false,
        inverse: Some("notes.restore"),
    },
    IntentSpec {
        name: "notes.restore",
        description: "Take a note out of the trash.",
        args: &[ArgSpec::id("id", "The note's id.", true)],
        effect: Effect::Write,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "notes.search",
        description: "Search the notes that are not in the trash, and return their ids, titles and snippets.",
        args: &[
            ArgSpec::text("query", "What to look for.", true, 200),
            ArgSpec::int("limit", "How many at most.", false, 1, 50),
        ],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "notes.open",
        description: "Open a note in the editor.",
        args: &[ArgSpec::id("id", "The note's id.", true)],
        effect: Effect::Read,
        shows: true,
        inverse: None,
    },
];

/// An intent whose answer waits for the server (or for the notes to be loaded).
pub enum Pending {
    /// A create: answer once the temporary id has been replaced by the server's.
    Create { tmp: String, reply: Responder },
    /// A trash: answer once the server has accepted (or refused) the write.
    Trash {
        id: String,
        before: lucina_app::intent::State,
        reply: Responder,
    },
    /// A restore: answer once the server has accepted (or refused) the write.
    Restore { id: String, reply: Responder },
    /// Any intent that arrived before the store had the cache or the first list: run once `store.loaded`.
    Waiting { call: Call, reply: Responder },
}

/// The title a note is known by: its title, or the first words of its text.
fn display_title(e: &Entry) -> String {
    if !e.note.title.is_empty() {
        e.note.title.clone()
    } else {
        e.preview
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// One of [`INTENTS`], with its arguments already checked. The same door `dispatch` uses.
pub fn intent(app: &mut Notes, call: Call, reply: Responder) {
    // A start-up intent can arrive before the cache or the first list: act on nothing until the store is loaded, or a
    // create would be overwritten by the cached outbox and a trash would find no note.
    if !app.store.loaded {
        app.pending.push(Pending::Waiting { call, reply });
        return;
    }
    match call.name.as_str() {
        "notes.create" => create(app, &call, reply),
        "notes.trash" => trash(app, &call, reply),
        "notes.restore" => restore(app, &call, reply),
        "notes.search" => search(app, &call, reply),
        "notes.open" => open(app, &call, reply),
        _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
    }
    // A headless run never calls `view`, so the outbox goes out from here too.
    app.pump();
}

fn create(app: &mut Notes, call: &Call, reply: Responder) {
    // The editor's "new note" path: the title trimmed, the body as typed.
    let input = Input {
        title: Some(call.args["title"].as_str().unwrap_or("").trim().to_owned()),
        content: Some(
            call.args["body"]
                .as_str()
                .unwrap_or("")
                .trim_end()
                .to_owned(),
        ),
        ..Input::default()
    };
    let tmp = app.store.create(input);
    app.pending.push(Pending::Create { tmp, reply });
}

fn trash(app: &mut Notes, call: &Call, reply: Responder) {
    let id = app
        .store
        .resolve(call.args["id"].as_str().unwrap_or(""))
        .to_owned();
    let Some(e) = app.store.note(&id).cloned() else {
        return reply.send(Reply::failed("no note with that id"));
    };
    if e.note.deleted_at.is_some() {
        return reply.send(Reply::failed("the note is already in the trash"));
    }
    if let Some(entry) = &call.undo {
        // The undo of a create: only while the note is still what that write left.
        let now = there(e.note.updated_at.as_bytes());
        if rules::undo_to(now, (*entry).into()).is_none() {
            return reply.send(Reply::failed("the note changed since Lucina made it"));
        }
    }
    let before = there(e.note.updated_at.as_bytes());
    app.store.trash(std::slice::from_ref(&id));
    app.pending.push(Pending::Trash { id, before, reply });
}

fn restore(app: &mut Notes, call: &Call, reply: Responder) {
    let id = app
        .store
        .resolve(call.args["id"].as_str().unwrap_or(""))
        .to_owned();
    let Some(e) = app.store.note(&id).cloned() else {
        return reply.send(Reply::failed("no note with that id"));
    };
    if e.note.deleted_at.is_none() {
        return reply.send(Reply::failed("the note is not in the trash"));
    }
    if let Some(entry) = &call.undo {
        // The undo of a trash: only while the note is still in the trash that write made.
        if rules::undo_to(GONE, (*entry).into()).is_none() {
            return reply.send(Reply::failed("the note changed since Lucina trashed it"));
        }
    }
    app.store.restore(std::slice::from_ref(&id));
    app.pending.push(Pending::Restore { id, reply });
}

fn search(app: &mut Notes, call: &Call, reply: Responder) {
    let query = call.args["query"].as_str().unwrap_or("").to_owned();
    let limit = call.args["limit"].as_i64().unwrap_or(10).clamp(1, 50) as usize;
    answer_search(app, &query, limit, reply);
}

fn open(app: &mut Notes, call: &Call, reply: Responder) {
    let id = app
        .store
        .resolve(call.args["id"].as_str().unwrap_or(""))
        .to_owned();
    let Some(e) = app.store.note(&id).cloned() else {
        return reply.send(Reply::failed("no note with that id"));
    };
    // As tapping its row does.
    match app.shown.iter().position(|s| s.note.id == id) {
        Some(i) => app.open(i),
        None => {
            app.editor = Some(Editor::open(&e.note, app.mode == Mode::Docs));
        }
    }
    let title = display_title(&e);
    reply
        .send(Reply::done(format!("Opened {title}")).with_entity(Entity::new("note", &id, &title)));
}

/// Whether a queued write on `id` has left the outbox (answered, one way or the other).
fn write_settled(app: &Notes, id: &str, trash: bool) -> bool {
    !app.store.outbox().any(|op| {
        let ids = match op {
            Op::Trash { ids } if trash => ids,
            Op::Restore { ids } if !trash => ids,
            _ => return false,
        };
        ids.iter().any(|i| app.store.resolve(i) == id)
    })
}

/// Answer the intents that were waiting, now that a message has moved the store. Called from `App::message`.
pub fn answer_pending(app: &mut Notes) {
    let mut i = 0;
    while i < app.pending.len() {
        let ready = match &app.pending[i] {
            Pending::Create { tmp, .. } => {
                let real = app.store.resolve(tmp);
                (real != tmp && app.store.note(real).is_some())
                    || (real == tmp && app.store.note(tmp).is_none())
            }
            Pending::Trash { id, .. } => {
                write_settled(app, id, true) && app.store.note(id).is_some()
            }
            Pending::Restore { id, .. } => {
                write_settled(app, id, false) && app.store.note(id).is_some()
            }
            Pending::Waiting { .. } => app.store.loaded,
        };
        if !ready {
            i += 1;
            continue;
        }
        match app.pending.remove(i) {
            Pending::Create { tmp, reply } => answer_create(app, &tmp, reply),
            Pending::Trash { id, before, reply } => answer_trash(app, &id, before, reply),
            Pending::Restore { id, reply } => answer_restore(app, &id, reply),
            Pending::Waiting { call, reply } => intent(app, call, reply),
        }
    }
}

/// A create is answered once the server has given the note its real id (the temporary one is aliased to it), or has
/// refused it (the temporary note is gone and no alias was made).
fn answer_create(app: &mut Notes, tmp: &str, reply: Responder) {
    let real = app.store.resolve(tmp);
    if real != tmp
        && let Some(e) = app.store.note(real).cloned()
    {
        let title = display_title(&e);
        let w = Write {
            kind: WriteKind::Create,
            target: format!("note:{real}"),
            before: GONE.into(),
            after: there(e.note.updated_at.as_bytes()).into(),
            undo: Undo::Inverse {
                intent: "notes.trash".into(),
                args: json!({ "id": real }),
            },
        };
        reply.send(
            Reply::done(format!("Made the note {title}"))
                .with_entity(Entity::new("note", real, &title))
                .with_value(json!({ "id": real }))
                .with_write(w),
        );
        return;
    }
    let why = app
        .store
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_else(|| "the server refused the note".to_owned());
    reply.send(Reply::failed(why));
}

/// A trash is answered once its write has left the outbox: accepted when the note is in the trash, refused otherwise.
fn answer_trash(app: &mut Notes, id: &str, before: lucina_app::intent::State, reply: Responder) {
    let Some(e) = app.store.note(id).cloned() else {
        return reply.send(Reply::failed("the note is gone"));
    };
    if e.note.deleted_at.is_none() {
        let why = app
            .store
            .notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_else(|| "the server refused the trash".to_owned());
        reply.send(Reply::failed(why));
        return;
    }
    let title = display_title(&e);
    let w = Write {
        kind: WriteKind::Remove,
        target: format!("note:{id}"),
        before: before.into(),
        after: GONE.into(),
        undo: Undo::Inverse {
            intent: "notes.restore".into(),
            args: json!({ "id": id }),
        },
    };
    reply.send(
        Reply::done(format!("Moved {title} to the trash"))
            .with_entity(Entity::new("note", id, &title))
            .with_write(w),
    );
}

/// A restore is answered once its write has left the outbox: accepted when the note is out of the trash, refused
/// otherwise.
fn answer_restore(app: &mut Notes, id: &str, reply: Responder) {
    let Some(e) = app.store.note(id).cloned() else {
        return reply.send(Reply::failed("the note is gone"));
    };
    if e.note.deleted_at.is_some() {
        let why = app
            .store
            .notice
            .as_ref()
            .map(|n| n.text.clone())
            .unwrap_or_else(|| "the server refused the restore".to_owned());
        reply.send(Reply::failed(why));
        return;
    }
    let title = display_title(&e);
    reply.send(
        Reply::done(format!("Restored {title}")).with_entity(Entity::new("note", id, &title)),
    );
}

/// The same search the search field runs, answered with the hits up to `limit`.
fn answer_search(app: &mut Notes, query: &str, limit: usize, reply: Responder) {
    let hits = search_notes(app.store.all(), query, None);
    let hits: Vec<_> = hits.into_iter().take(limit).collect();
    let notes: Vec<Value> = hits
        .iter()
        .map(|e| {
            let snippet: String = e.preview.chars().take(120).collect();
            json!({ "id": e.note.id, "title": display_title(e), "snippet": snippet })
        })
        .collect();
    let n = notes.len();
    let mut r = Reply::done(format!("{n} note{}", if n == 1 { "" } else { "s" }))
        .with_value(json!({ "notes": notes }));
    for e in &hits {
        r = r.with_entity(Entity::new("note", &e.note.id, &display_title(e)));
    }
    reply.send(r);
}
