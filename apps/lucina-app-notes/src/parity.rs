//! One scenario through the store, over any transport: the in-process fake in the tests, and a real `secretary-server` with
//! `scripts/notes-parity.sh`. It checks the wire contract, and that what the screen shows before an answer is what the answer
//! says: `apply_input` must be what the server does.

use std::time::Instant;

use lucina_app_support::net::Transport;

use crate::model::{Input, Item, Note};
use crate::store::{CacheFile, Store};

struct Drive {
    store: Store,
    t: Box<dyn Transport>,
}

impl Drive {
    fn new(t: Box<dyn Transport>) -> Drive {
        let mut d = Drive {
            store: Store::new(),
            t,
        };
        d.store.load_cache(CacheFile::default(), Vec::new());
        d.run();
        d
    }

    /// Sends what the store asks for until it has nothing left, on this thread.
    fn run(&mut self) {
        for _ in 0..200 {
            let Some(j) = self.store.next_job(Instant::now()) else {
                return;
            };
            let result = self.t.call(j.method, &j.path, j.body.as_ref());
            self.store
                .on_reply(lucina_app_support::net::Reply { id: j.id, result });
        }
        panic!("the store never settled");
    }

    fn note(&self, id: &str) -> Note {
        self.store.note(id).expect("note").note.clone()
    }

    /// Applies a write, keeps what the screen showed before the answer, and checks the server agrees with it. This is
    /// what makes the app's optimistic copy honest: `apply_input` must be what the server does.
    fn same_after(&mut self, id: &str, what: &str, write: impl FnOnce(&mut Store)) -> Note {
        write(&mut self.store);
        let shown = self.note(id);
        self.run();
        let real = self.note(id);
        let view = |n: &Note| {
            (
                n.title.clone(),
                n.content.clone(),
                n.tags.clone(),
                n.color.clone(),
                n.pinned,
                n.archived,
                n.items.as_ref().map(|i| {
                    i.iter()
                        .map(|x| (x.text.clone(), x.checked, x.indent))
                        .collect::<Vec<_>>()
                }),
                n.reminder.is_some(),
                n.deleted_at.is_some(),
            )
        };
        assert_eq!(
            view(&shown),
            view(&real),
            "{what}: what the screen showed before the answer differs from the answer"
        );
        real
    }
}

fn item(text: &str, checked: bool, indent: u8) -> Item {
    Item {
        id: String::new(),
        text: text.into(),
        checked,
        indent,
    }
}

pub fn scenario(make: &dyn Fn() -> Box<dyn Transport>) {
    let mut d = Drive::new(make());

    // A text note with a label in display case, and a colour.
    let tmp = d.store.create(Input {
        title: Some("Parity note".into()),
        content: Some("first line\n\nsecond".into()),
        labels: Some(vec!["#Work  Stuff".into()]),
        color: Some("teal".into()),
        ..Input::default()
    });
    let shown_tags = d.store.note(&tmp).unwrap().note.tags.clone();
    d.run();
    let id = d.store.resolve(&tmp).to_owned();
    assert!(
        !id.starts_with("tmp_"),
        "the server's id replaced the temporary one"
    );
    let n = d.note(&id);
    assert_eq!(n.tags, shown_tags);
    assert_eq!(n.tags, ["work stuff"]);
    assert_eq!(n.color, "teal");
    assert_eq!(n.slug, "parity-note");
    assert_eq!(d.store.label_name("work stuff"), "Work Stuff");

    // Edits: each the screen must have right before the answer.
    d.same_after(&id, "title", |s| {
        s.update(
            &id,
            Input {
                title: Some("Parity note 2".into()),
                ..Input::default()
            },
            false,
        )
    });
    d.same_after(&id, "content", |s| {
        s.update(
            &id,
            Input {
                content: Some("# Parity note 2\n\nbody **bold**".into()),
                ..Input::default()
            },
            false,
        )
    });
    let ids = [id.clone()];
    d.same_after(&id, "pin", |s| s.set_pinned(&ids, true));
    d.same_after(&id, "colour", |s| s.set_color(&ids, "purple"));
    d.same_after(&id, "add label", |s| s.add_label(&id, "Second"));
    d.same_after(&id, "remove label", |s| s.remove_label(&id, "work stuff"));
    let archived = d.same_after(&id, "archive", |s| s.archive(&ids));
    assert!(
        archived.archived && !archived.pinned,
        "Keep unpins what it archives"
    );
    d.same_after(&id, "unarchive", |s| s.unarchive(&ids));

    // A list note.
    let tmp = d.store.create(Input {
        items: Some(vec![item("milk", false, 0), item("whole", true, 1)]),
        ..Input::default()
    });
    d.run();
    let list = d.store.resolve(&tmp).to_owned();
    let n = d.note(&list);
    assert_eq!(
        n.content, "- [ ] milk\n  - [x] whole",
        "the server derives the content"
    );
    assert_eq!(n.title, "");
    assert!(
        n.items
            .as_ref()
            .unwrap()
            .iter()
            .all(|i| i.id.starts_with("i_"))
    );
    let mut items = n.items.clone().unwrap();
    items[0].checked = true;
    items.push(item("eggs", false, 0));
    d.same_after(&list, "items", |s| {
        s.update(
            &list,
            Input {
                items: Some(items),
                ..Input::default()
            },
            false,
        )
    });
    d.same_after(&list, "to text", |s| {
        s.update(
            &list,
            Input {
                convert: Some("text".into()),
                ..Input::default()
            },
            false,
        )
    });
    d.same_after(&list, "to list", |s| {
        s.update(
            &list,
            Input {
                convert: Some("list".into()),
                ..Input::default()
            },
            false,
        )
    });

    // Another client changes the note: a guarded save is refused and the note goes back to what the server has.
    let mut other = make();
    let path = format!("/v1/notes/{id}");
    let before = d.note(&id);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let r = other
        .call(
            lucina_app_support::net::Method::Patch,
            &path,
            Some(&serde_json::json!({"content": "from elsewhere"})),
        )
        .unwrap();
    assert_eq!(r.status, 200);
    d.store.update(
        &id,
        Input {
            content: Some("mine".into()),
            ..Input::default()
        },
        true,
    );
    let job = d.store.next_job(Instant::now()).unwrap();
    assert_eq!(
        job.body.as_ref().unwrap()["baseUpdatedAt"],
        serde_json::json!(before.updated_at)
    );
    let result = d.t.call(job.method, &job.path, job.body.as_ref());
    d.store
        .on_reply(lucina_app_support::net::Reply { id: job.id, result });
    assert_eq!(d.store.conflict.as_deref(), Some(id.as_str()));
    d.store.clear_conflict();
    d.store.want_sync();
    d.run();
    assert_eq!(d.note(&id).content, "from elsewhere");

    // A refusal the server words itself.
    let huge = "x".repeat(300 * 1024);
    d.store.update(
        &id,
        Input {
            content: Some(huge),
            ..Input::default()
        },
        false,
    );
    d.run();
    assert_eq!(
        d.note(&id).content,
        "from elsewhere",
        "a refused write goes back"
    );
    assert!(d.store.notice.is_some());

    // Labels.
    d.store.create_label("Fresh One");
    d.run();
    assert_eq!(d.store.label_name("fresh one"), "Fresh One");
    d.store.create_label("fresh one"); // label_exists: silent.
    d.store.dismiss_notice();
    d.run();
    assert!(d.store.notice.is_none());
    d.store.rename_label("second", "Renamed");
    d.run();
    assert_eq!(d.note(&id).tags, ["renamed"]);
    d.store.delete_label("renamed");
    d.run();
    assert!(d.note(&id).tags.is_empty());

    // The trash.
    d.store.trash(&ids);
    assert!(d.note(&id).deleted_at.is_some());
    d.run();
    assert!(d.note(&id).deleted_at.is_some());
    d.store.restore(&ids);
    d.run();
    assert!(d.note(&id).deleted_at.is_none());
    let lists = [list.clone()];
    d.store.trash(&lists);
    d.run();
    d.store.delete_forever(&lists);
    d.run();
    assert!(d.store.note(&list).is_none());
    d.store.trash(&ids);
    d.run();
    d.store.empty_trash();
    d.run();
    assert!(d.store.note(&id).is_none());

    // What is left arrives the same way on a client that starts from nothing.
    let tmp = d.store.create(Input {
        title: Some("Stays".into()),
        content: Some("x".into()),
        ..Input::default()
    });
    d.run();
    let stays = d.store.resolve(&tmp).to_owned();
    let fresh = Drive::new(make());
    assert_eq!(fresh.note(&stays), d.note(&stays));
    assert!(fresh.store.note(&id).is_none(), "a tombstone is not a note");
    assert!(fresh.store.all().all(|e| e.note.id != list));

    // A cursor from another store: load again from 0.
    let mut d2 = Drive {
        store: Store::new(),
        t: make(),
    };
    d2.store.load_cache(
        CacheFile {
            cursor: "99999999".into(),
            ..CacheFile::default()
        },
        Vec::new(),
    );
    d2.run();
    assert!(d2.store.note(&stays).is_some());
}
