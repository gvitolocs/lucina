//! The intents Drive offers Lucina (docs/research/juno-intents.md): `drive.find`, `drive.open`. The find runs the same search the
//! search field runs (from the roots, so it covers everything) and keeps its [`Responder`] in [`Drive::pending`], answered from
//! `App::message` once the page arrives, so a headless run — which never calls `view` — gets its answer too.

use lucina_app::intent::{ArgSpec, Call, Effect, Entity, IntentSpec, Reply, Responder};
use serde_json::{Value, json};

use crate::app::{Context, Drive};

/// What Drive offers, listed by `<program> --intents` and named in `manifest.app`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "drive.find",
        description: "Search the Drive and return the files that match, with their ids, names and kinds.",
        args: &[
            ArgSpec::text("query", "What to look for.", true, 200),
            ArgSpec::int("limit", "How many at most.", false, 1, 50),
        ],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
    IntentSpec {
        name: "drive.open",
        description: "Open a Drive file in the preview, as tapping its row does.",
        args: &[ArgSpec::id("id", "The file's id.", true)],
        effect: Effect::Read,
        shows: true,
        inverse: None,
    },
];

/// An intent whose answer waits for the server.
pub enum Pending {
    /// A find: answer once the page for its generation has arrived.
    Find {
        generation: u64,
        limit: usize,
        reply: Responder,
    },
}

/// One of [`INTENTS`], with its arguments already checked. The same door `dispatch` uses.
pub fn intent(app: &mut Drive, call: Call, reply: Responder) {
    match call.name.as_str() {
        "drive.find" => find(app, &call, reply),
        "drive.open" => open(app, &call, reply),
        _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
    }
}

/// The roots search, which covers everything, as the search field at the roots does.
fn find(app: &mut Drive, call: &Call, reply: Responder) {
    let query = call.args["query"].as_str().unwrap_or("").trim().to_owned();
    let limit = call.args["limit"].as_i64().unwrap_or(10).clamp(1, 50) as usize;
    app.context = Context::Roots;
    app.filters.q = query.clone();
    app.search.set(&query);
    app.reload();
    app.pending.push(Pending::Find {
        generation: app.generation,
        limit,
        reply,
    });
}

/// As tapping the row does: `open_preview`.
fn open(app: &mut Drive, call: &Call, reply: Responder) {
    let id = call.args["id"].as_str().unwrap_or("");
    let Some(item) = app.files.iter().find(|f| f.id == id).cloned() else {
        return reply.send(Reply::failed(format!("no file with id {id}")));
    };
    app.open_preview(item.clone());
    reply.send(
        Reply::done(format!("Opened {}", item.name())).with_entity(Entity::new(
            "drive_file",
            &item.id,
            item.name(),
        )),
    );
}

/// Answer the intents that were waiting, now that a message has moved the listing. Called from `App::message`.
pub fn answer_pending(app: &mut Drive) {
    let mut i = 0;
    while i < app.pending.len() {
        let ready = match &app.pending[i] {
            Pending::Find { generation, .. } => *generation == app.generation && !app.loading,
        };
        if ready {
            match app.pending.remove(i) {
                Pending::Find { limit, reply, .. } => answer_find(app, limit, reply),
            }
        } else {
            i += 1;
        }
    }
}

/// The same listing the screen shows, answered with the hits up to `limit`.
fn answer_find(app: &Drive, limit: usize, reply: Responder) {
    let files: Vec<Value> = app
        .files
        .iter()
        .take(limit)
        .map(|f| {
            let mut v = json!({ "id": f.id, "name": f.name(), "kind": f.file_type().label() });
            if let Some(s) = f.size {
                v["size"] = json!(s);
            }
            if let Some(m) = f.modified() {
                v["modified"] = json!(m);
            }
            v
        })
        .collect();
    let n = files.len();
    let mut r = Reply::done(format!("{n} file{}", if n == 1 { "" } else { "s" }))
        .with_value(json!({ "files": files }));
    for f in app.files.iter().take(limit) {
        r = r.with_entity(Entity::new("drive_file", &f.id, f.name()));
    }
    reply.send(r);
}
