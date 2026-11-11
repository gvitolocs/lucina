//! The intents Gallery offers Lucina (docs/research/juno-intents.md): `gallery.show`, `gallery.latest`. Each calls the same
//! functions the grid's keys and taps call (`open_viewer`, `set_shelf`). An answer that waits for the server keeps its
//! [`Responder`] in [`Gallery::pending`] and is answered from `App::message`, so a headless run — which never calls `view` —
//! gets its answer too.

use lucina_app::intent::{ArgSpec, Call, Effect, Entity, IntentSpec, Reply, Responder};
use serde_json::{Value, json};

use crate::app::{Gallery, Shelf};
use crate::model::Item;

/// What Gallery offers, listed by `<program> --intents` and named in `manifest.app`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        name: "gallery.show",
        description: "Show a picture in the viewer, an album by name, or the newest pictures; returns what is shown.",
        args: &[
            ArgSpec::text("album", "The album's name.", false, 200),
            ArgSpec::id("id", "The picture's id.", false),
        ],
        effect: Effect::Read,
        shows: true,
        inverse: None,
    },
    IntentSpec {
        name: "gallery.latest",
        description: "The newest pictures, with their ids and when they were taken.",
        args: &[ArgSpec::int("limit", "How many at most.", false, 1, 50)],
        effect: Effect::Read,
        shows: false,
        inverse: None,
    },
];

/// An intent whose answer waits for the server.
pub enum Pending {
    /// A show by album name, while the albums load.
    ShowAlbum { album: String, reply: Responder },
    /// A latest, while the first page loads.
    Latest { limit: usize, reply: Responder },
}

/// One of [`INTENTS`], with its arguments already checked. The same door `dispatch` uses.
pub fn intent(app: &mut Gallery, call: Call, reply: Responder) {
    match call.name.as_str() {
        "gallery.show" => show(app, &call, reply),
        "gallery.latest" => latest(app, &call, reply),
        _ => reply.send(Reply::failed(format!("no intent {} here", call.name))),
    }
}

/// What a picture is called: its title, else its id.
fn picture_name(item: &Item) -> String {
    if item.title.is_empty() {
        item.id.clone()
    } else {
        item.title.clone()
    }
}

fn show(app: &mut Gallery, call: &Call, reply: Responder) {
    if let Some(id) = call.args["id"].as_str() {
        let Some(i) = app.items.iter().position(|x| x.id == id) else {
            return reply.send(Reply::failed(format!("no picture with id {id}")));
        };
        // As tapping the tile does.
        app.open_viewer(i);
        let name = picture_name(&app.items[i]);
        let r = Reply::done(format!("Showing {name}"))
            .with_value(json!({ "shown": name }))
            .with_entity(Entity::new("picture", id, &name));
        return reply.send(r);
    }
    if let Some(album) = call.args["album"].as_str() {
        if matches!(&app.albums, Some(Ok(_))) {
            return show_album(app, album, reply);
        }
        app.send(crate::server::Job::Albums);
        app.pending.push(Pending::ShowAlbum {
            album: album.to_owned(),
            reply,
        });
        return;
    }
    // The newest pictures view.
    app.set_shelf(Shelf::Everything);
    reply.send(
        Reply::done("Showing the newest pictures")
            .with_value(json!({ "shown": "the newest pictures" })),
    );
}

/// The album by name, case-insensitive, as the albums page opens it.
fn show_album(app: &mut Gallery, album: &str, reply: Responder) {
    let Some(Ok(list)) = &app.albums else {
        return reply.send(Reply::failed("the albums have not loaded".to_owned()));
    };
    let Some(a) = list.iter().find(|a| a.title.eq_ignore_ascii_case(album)) else {
        return reply.send(Reply::failed(format!("no album named {album}")));
    };
    let (id, title) = (a.id.clone(), a.title.clone());
    app.set_shelf(Shelf::Album {
        id: id.clone(),
        title: title.clone(),
    });
    let r = Reply::done(format!("Showing the album {title}"))
        .with_value(json!({ "shown": title }))
        .with_entity(Entity::new("album", &id, &title));
    reply.send(r);
}

fn latest(app: &mut Gallery, call: &Call, reply: Responder) {
    let limit = call.args["limit"].as_i64().unwrap_or(10).clamp(1, 50) as usize;
    if app.started {
        answer_latest(app, limit, reply);
    } else {
        app.pending.push(Pending::Latest { limit, reply });
    }
}

/// Answer the intents that were waiting, now that a message has moved the feed. Called from `App::message`.
pub fn answer_pending(app: &mut Gallery) {
    let mut i = 0;
    while i < app.pending.len() {
        let ready = match &app.pending[i] {
            Pending::ShowAlbum { .. } => matches!(&app.albums, Some(Ok(_))),
            Pending::Latest { .. } => app.started,
        };
        if ready {
            match app.pending.remove(i) {
                Pending::ShowAlbum { album, reply } => show_album(app, &album, reply),
                Pending::Latest { limit, reply } => answer_latest(app, limit, reply),
            }
        } else {
            i += 1;
        }
    }
}

/// The feed is newest first; the first `limit` of it.
fn answer_latest(app: &Gallery, limit: usize, reply: Responder) {
    let pictures: Vec<Value> = app
        .items
        .iter()
        .take(limit)
        .map(|it| {
            let mut v = json!({ "id": it.id });
            if let Some(t) = &it.taken_at {
                v["taken"] = json!(t);
            }
            let name = picture_name(it);
            if !name.is_empty() {
                v["name"] = json!(name);
            }
            v
        })
        .collect();
    let n = pictures.len();
    let mut r = Reply::done(format!("{n} picture{}", if n == 1 { "" } else { "s" }))
        .with_value(json!({ "pictures": pictures }));
    for it in app.items.iter().take(limit) {
        r = r.with_entity(Entity::new("picture", &it.id, &picture_name(it)));
    }
    reply.send(r);
}
