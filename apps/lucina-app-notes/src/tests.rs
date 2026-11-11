//! The app driven as a person would, with the fake server behind a real worker thread.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use lucina_app::keys::{self, Mods};
use lucina_app::{Harness, Input as UiInput};
use lucina_app_support::net::{Method, Reply, Transport};
use serde_json::json;

use crate::editor::Editor;
use crate::fake::Fake;
use crate::model::{Entry, Input, Item, Note, View};
use crate::store::{CacheFile, Op};
use crate::{Mode, Msg, Notes};

struct T {
    h: Harness<Notes>,
    rx: mpsc::Receiver<Reply>,
    fake: Fake,
}

fn start(fake: Fake) -> T {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let app = Notes::new(
        Some(Box::new(fake.clone())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut t = T {
        h: Harness::new(app, 1920, 1080),
        rx,
        fake,
    };
    t.h.core.message(Msg::Cache(Box::default(), Vec::new()));
    t.settle();
    t
}

impl T {
    fn settle(&mut self) {
        let end = Instant::now() + Duration::from_secs(5);
        while !self.h.app().store.idle() && Instant::now() < end {
            if let Ok(r) = self.rx.recv_timeout(Duration::from_millis(50)) {
                self.h.core.message(Msg::Reply(r));
            } else if !self.h.app().store.in_flight() {
                // Waiting out a retry delay.
                self.h.app().store.retry_now();
                self.h.app().pump();
            }
        }
        self.pass();
    }

    fn pass(&mut self) {
        self.h.core.input(UiInput::Wheel(0));
        self.h.core.paint();
    }

    fn shown(&mut self) -> Vec<String> {
        self.pass();
        self.h.app().shown.iter().map(|e| title(&e.note)).collect()
    }

    fn server_note(&self, f: impl Fn(&Note) -> bool) -> Note {
        let s = self.fake.0.lock().unwrap();
        s.notes
            .iter()
            .find(|n| f(n))
            .cloned()
            .expect("on the server")
    }

    fn press_ctrl(&mut self, code: u16) {
        self.h.press(
            code,
            Mods {
                ctrl: true,
                ..Mods::default()
            },
        );
    }
}

fn title(n: &Note) -> String {
    n.title.clone()
}

fn seeded(titles: &[&str]) -> Fake {
    let f = Fake::new();
    for t in titles {
        f.0.lock().unwrap().insert(Note {
            title: (*t).into(),
            content: format!("about {t}"),
            ..Note::default()
        });
    }
    f
}

#[test]
fn opening_shows_the_servers_notes_with_the_newest_first() {
    let mut t = start(seeded(&["one", "two", "three"]));
    assert_eq!(t.shown(), ["three", "two", "one"]);
    assert!(t.h.app().store.loaded);
}

#[test]
fn every_key_of_a_search_filters_at_once_without_the_server() {
    let mut t = start(seeded(&["Apples", "Bread", "Apple pie"]));
    let asked = t.fake.0.lock().unwrap().requests.len();
    t.h.type_str("app");
    assert_eq!(t.shown(), ["Apple pie", "Apples"]);
    t.h.type_str("les");
    assert_eq!(t.shown(), ["Apples"]);
    t.h.key(keys::KEY_ESC);
    assert_eq!(t.shown().len(), 3);
    assert_eq!(
        t.fake.0.lock().unwrap().requests.len(),
        asked,
        "search is local"
    );
}

#[test]
fn a_new_note_is_typed_saved_and_reaches_the_server() {
    let mut t = start(seeded(&[]));
    t.press_ctrl(49); // Ctrl+N
    assert!(t.h.app().editor.is_some());
    t.h.type_str("hello there");
    assert_eq!(t.h.app().editor.as_ref().unwrap().body.text, "hello there");
    t.h.key(keys::KEY_ESC);
    assert!(t.h.app().editor.is_none());
    // On the screen before any answer.
    assert_eq!(t.h.app().store.all().count(), 1);
    t.settle();
    let n = t.server_note(|n| n.content == "hello there");
    assert_eq!(n.slug, "hello-there");
    // The temporary copy became the server's.
    assert_eq!(t.h.app().store.all().next().unwrap().note.id, n.id);
}

#[test]
fn an_empty_new_note_is_never_made() {
    let mut t = start(seeded(&[]));
    t.press_ctrl(49);
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert_eq!(t.h.app().store.all().count(), 0);
    assert!(t.fake.0.lock().unwrap().notes.is_empty());
}

#[test]
fn editing_saves_after_the_pause_and_as_one_patch() {
    let mut t = start(seeded(&["note"]));
    t.h.app().open(0);
    t.pass();
    t.h.type_str(" more");
    // Nothing yet: the pause has not passed.
    assert_eq!(t.h.app().store.waiting(), 0);
    t.h.app().editor.as_mut().unwrap().save_at = Some(Instant::now());
    t.pass();
    assert_eq!(t.h.app().store.waiting(), 1);
    t.settle();
    assert_eq!(
        t.server_note(|n| n.title == "note").content,
        "about note more"
    );
}

#[test]
fn a_title_heading_stays_in_the_note_but_not_in_the_field() {
    let f = Fake::new();
    f.0.lock().unwrap().insert(Note {
        title: "Plan".into(),
        content: "# Plan\n\nbody".into(),
        ..Note::default()
    });
    let mut t = start(f);
    t.h.app().open(0);
    t.pass();
    assert_eq!(t.h.app().editor.as_ref().unwrap().body.text, "body");
    t.h.type_str("!");
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert_eq!(
        t.server_note(|n| n.title == "Plan").content,
        "# Plan\n\nbody!"
    );
}

#[test]
fn a_checklist_is_edited_as_keep_edits_it() {
    let mut t = start(seeded(&[]));
    t.press_ctrl(38); // Ctrl+L
    t.pass();
    t.h.type_str("milk");
    t.h.key(keys::KEY_ENTER);
    t.pass();
    t.h.type_str("eggs");
    t.h.key(keys::KEY_ENTER);
    t.pass();
    t.h.type_str("bread");
    t.h.key(keys::KEY_TAB); // Indents.
    t.pass();
    t.h.key(keys::KEY_ESC);
    t.settle();
    let n = t.server_note(|n| n.items.is_some());
    let items = n.items.clone().unwrap();
    let got: Vec<(&str, u8)> = items.iter().map(|i| (i.text.as_str(), i.indent)).collect();
    assert_eq!(got, [("milk", 0), ("eggs", 0), ("bread", 1)]);
    assert_eq!(n.content, "- [ ] milk\n- [ ] eggs\n  - [ ] bread");
}

#[test]
fn enter_splits_an_item_at_the_cursor_and_backspace_on_empty_removes_it() {
    let f = Fake::new();
    f.0.lock().unwrap().insert(Note {
        title: "L".into(),
        items: Some(vec![Item {
            id: "i_a".into(),
            text: "ab".into(),
            ..Item::default()
        }]),
        content: "- [ ] ab".into(),
        ..Note::default()
    });
    let mut t = start(f);
    t.h.app().open(0);
    t.pass();
    {
        let ed = t.h.app().editor.as_mut().unwrap();
        ed.rows.as_mut().unwrap()[0].edit.cursor = 1;
        ed.rows.as_mut().unwrap()[0].edit.anchor = 1;
    }
    t.h.core.input(UiInput::Key(lucina_app::keys::Press {
        code: keys::KEY_TAB,
        mods: Mods::default(),
    }));
    t.h.core.paint();
    // The title has the keyboard first: walk to the first item's field.
    t.h.app().editor.as_mut().unwrap().rows.as_mut().unwrap()[0]
        .item
        .indent = 0;
    t.pass();
    let id = "i_a".to_owned();
    t.h.app().editor.as_mut().unwrap().focus_item(&id);
    t.pass();
    t.pass();
    t.h.key(keys::KEY_ENTER);
    t.pass();
    let texts = |t: &mut T| {
        t.h.app()
            .editor
            .as_ref()
            .unwrap()
            .rows
            .as_ref()
            .unwrap()
            .iter()
            .map(|r| r.edit.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(texts(&mut t), ["a", "b"]);
    t.h.key(keys::KEY_DELETE);
    t.h.key(keys::KEY_BACKSPACE); // The new item is now empty, and goes.
    t.pass();
    assert_eq!(texts(&mut t), ["a"]);
}

#[test]
fn trash_shows_at_once_and_undo_brings_it_back() {
    let mut t = start(seeded(&["keep", "bin"]));
    let id =
        t.h.app()
            .store
            .all()
            .find(|e| e.note.title == "bin")
            .unwrap()
            .note
            .id
            .clone();
    t.h.app().store.trash(std::slice::from_ref(&id));
    assert_eq!(t.shown(), ["keep"]);
    t.settle();
    assert!(t.server_note(|n| n.id == id).deleted_at.is_some());
    let undo = t.h.app().store.notice.clone().unwrap().undo.unwrap();
    t.h.app().store.undo(undo);
    assert_eq!(t.shown(), ["bin", "keep"]);
    t.settle();
    assert!(t.server_note(|n| n.id == id).deleted_at.is_none());
}

#[test]
fn views_show_what_keep_shows() {
    let f = Fake::demo();
    let mut t = start(f);
    assert!(t.shown().iter().any(|n| n == "Pasta night"));
    assert_eq!(t.shown()[..2], ["Pasta night", "Welcome to Notes"]); // Pinned first.
    t.h.app().go(View::Archive);
    assert_eq!(t.shown(), ["Old plans"]);
    t.h.app().go(View::Trash);
    assert_eq!(t.shown(), ["Scratch"]);
    t.h.app().go(View::Reminders);
    assert_eq!(t.shown(), ["Dentist"]);
    t.h.app().go(View::Label("cooking".into()));
    assert_eq!(t.shown(), ["Pasta night"]);
    let labels = t.h.app().store.labels();
    assert_eq!(
        labels.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["Cooking", "Home", "Ideas", "Work"]
    );
    assert_eq!(labels[3].count, 2);
}

#[test]
fn another_clients_note_arrives_with_the_next_sync() {
    let mut t = start(seeded(&["mine"]));
    t.fake.0.lock().unwrap().insert(Note {
        title: "from Lucina".into(),
        content: "x".into(),
        source: "lucina".into(),
        ..Note::default()
    });
    t.h.app().store.want_sync();
    t.h.app().pump();
    t.settle();
    assert_eq!(t.shown()[0], "from Lucina");
}

#[test]
fn a_write_without_an_answer_stays_queued_and_goes_when_the_network_is_back() {
    let mut t = start(seeded(&["note"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    t.fake.0.lock().unwrap().drop_next = 2;
    t.h.app().store.set_color(std::slice::from_ref(&id), "red");
    t.h.app().pump();
    let r = t.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    t.h.core.message(Msg::Reply(r));
    // Still red on the screen, and the status says so.
    assert_eq!(t.h.app().store.note(&id).unwrap().note.color, "red");
    assert!(!t.h.app().store.online);
    assert_eq!(t.h.app().store.waiting(), 1);
    t.settle();
    assert!(t.h.app().store.online);
    assert_eq!(t.server_note(|n| n.id == id).color, "red");
}

#[test]
fn a_create_that_arrived_but_got_no_answer_is_not_made_twice() {
    let mut t = start(seeded(&[]));
    {
        let mut s = t.fake.0.lock().unwrap();
        s.drop_next = 1;
        s.drop_after_arriving = true;
    }
    t.h.app().store.create(Input {
        title: Some("once".into()),
        content: Some("only".into()),
        ..Input::default()
    });
    t.h.app().pump();
    t.settle();
    assert_eq!(t.fake.0.lock().unwrap().notes.len(), 1);
    assert_eq!(t.h.app().store.all().count(), 1);
    assert_eq!(t.h.app().store.waiting(), 0);
}

#[test]
fn a_refused_write_goes_back_and_says_why() {
    let mut t = start(seeded(&["note"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    let huge = "x".repeat(300 * 1024);
    t.h.app().store.update(
        &id,
        Input {
            content: Some(huge),
            ..Input::default()
        },
        false,
    );
    t.settle();
    assert_eq!(
        t.h.app().store.note(&id).unwrap().note.content,
        "about note"
    );
    assert_eq!(
        t.h.app().store.notice.as_ref().unwrap().text,
        "The note is too large."
    );
}

#[test]
fn a_note_changed_elsewhere_is_not_overwritten_from_the_documents_view() {
    let mut t = start(seeded(&["doc"]));
    t.h.app().mode = Mode::Docs;
    t.pass();
    t.h.app().open(0);
    t.pass();
    let id = t.h.app().editor.as_ref().unwrap().id.clone().unwrap();
    // Lucina appends while the owner is typing.
    t.fake
        .clone()
        .call(
            Method::Patch,
            &format!("/v1/notes/{id}"),
            Some(&json!({"content": "from Lucina"})),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(2));
    t.h.type_str(" mine");
    t.h.key(keys::KEY_ESC);
    t.settle();
    assert_eq!(t.h.app().store.conflict.as_deref(), Some(id.as_str()));
    assert_eq!(t.server_note(|n| n.id == id).content, "from Lucina");
}

#[test]
fn a_write_in_flight_ignores_what_the_sync_saw() {
    let mut t = start(seeded(&["note"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    t.h.app().store.update(
        &id,
        Input {
            title: Some("edited".into()),
            ..Input::default()
        },
        false,
    );
    t.fake
        .clone()
        .call(
            Method::Patch,
            &format!("/v1/notes/{id}"),
            Some(&json!({"content": "elsewhere"})),
        )
        .unwrap();
    t.h.app().store.want_sync();
    t.h.app().pump();
    t.settle();
    assert_eq!(t.h.app().store.note(&id).unwrap().note.title, "edited");
    assert_eq!(t.server_note(|n| n.id == id).title, "edited");
}

#[test]
fn an_expired_cursor_drops_the_cache_and_loads_from_the_start() {
    let mut t = start(seeded(&["a"]));
    {
        let mut s = t.fake.0.lock().unwrap();
        s.notes.clear();
        s.insert(Note {
            title: "after a restore".into(),
            content: "x".into(),
            ..Note::default()
        });
        s.expire_cursor = true;
    }
    t.h.app().store.want_sync();
    t.h.app().pump();
    t.settle();
    assert_eq!(t.shown(), ["after a restore"]);
}

#[test]
fn a_long_sync_comes_in_pages() {
    let f = seeded(&["1", "2", "3", "4", "5"]);
    f.0.lock().unwrap().page = 2;
    let mut t = start(f);
    assert_eq!(t.shown().len(), 5);
    let pages = t
        .fake
        .0
        .lock()
        .unwrap()
        .requests
        .iter()
        .filter(|r| r.contains("changes"))
        .count();
    assert_eq!(pages, 3);
}

#[test]
fn the_cache_and_the_outbox_are_on_screen_before_the_first_answer() {
    let f = Fake::new();
    let app = Notes::new(Some(Box::new(f.clone())), |_| {}, None, None);
    let mut h = Harness::new(app, 1920, 1080);
    let cached = Note {
        id: "n_1".into(),
        title: "cached".into(),
        color: "default".into(),
        rev: 5,
        ..Note::default()
    };
    let cache = CacheFile {
        cursor: "5".into(),
        labels: vec![],
        notes: vec![cached],
    };
    let outbox = vec![Op::Patch {
        id: "n_1".into(),
        input: Input {
            title: Some("queued edit".into()),
            ..Input::default()
        },
        guard: false,
    }];
    h.core.message(Msg::Cache(Box::new(cache), outbox));
    h.core.input(UiInput::Wheel(0));
    h.core.paint();
    assert_eq!(h.app().shown[0].note.title, "queued edit");
    assert_eq!(h.app().store.waiting(), 1);
}

#[test]
fn a_reminder_cleared_in_the_outbox_stays_cleared_after_a_restart() {
    let op = Op::Patch {
        id: "n".into(),
        input: Input {
            reminder: Some(None),
            ..Input::default()
        },
        guard: false,
    };
    let back: Op = serde_json::from_slice(&serde_json::to_vec(&op).unwrap()).unwrap();
    assert_eq!(op, back);
    let plain = Op::Patch {
        id: "n".into(),
        input: Input {
            title: Some("t".into()),
            ..Input::default()
        },
        guard: false,
    };
    let back: Op = serde_json::from_slice(&serde_json::to_vec(&plain).unwrap()).unwrap();
    assert_eq!(plain, back);
}

#[test]
fn the_keyboard_moves_through_the_grid_while_search_keeps_the_keys() {
    let names: Vec<String> = (0..30).map(|i| format!("n{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut t = start(seeded(&refs));
    t.pass();
    t.h.key(keys::KEY_DOWN);
    assert_eq!(t.h.app().sel, Some(0));
    t.h.key(keys::KEY_DOWN);
    let cols = t.h.app().sel.unwrap();
    assert!(cols >= 2, "{cols} columns on this screen");
    t.h.key(keys::KEY_ENTER);
    t.pass();
    let opened = t.h.app().editor.as_ref().map(|e| e.title.text.clone());
    assert_eq!(opened.as_deref(), Some(format!("n{}", 29 - cols).as_str()));
}

#[test]
fn a_click_on_a_card_opens_it() {
    let mut t = start(seeded(&["only"]));
    t.pass();
    // A card sits to the right of the rail, under the header and the search field.
    t.h.click(400, 200);
    assert_eq!(
        t.h.app()
            .editor
            .as_ref()
            .map(|e| e.title.text.clone())
            .as_deref(),
        Some("only")
    );
}

#[test]
fn typing_only_redraws_what_changed() {
    let mut t = start(Fake::demo());
    t.pass();
    t.h.app().open(0);
    t.pass();
    t.pass();
    let dirty = t.h.press(30, Mods::default());
    let px: i32 = dirty.iter().map(|r| r.w * r.h).sum();
    assert!(px < 1920 * 1080 / 8, "{px} pixels for one key");
}

/// Owner's requirement: the feel must be instant. A search over the store's stated size of about 5,000 small notes, one
/// key's work, on a build without optimisation; the release build on the laptop is what the fluidity test measures.
#[test]
fn a_search_over_five_thousand_notes_is_quick() {
    let entries: Vec<std::sync::Arc<Entry>> = (0..5000)
        .map(|i| {
            Entry::new(Note {
                id: format!("n{i}"),
                title: format!("Note number {i}"),
                content: format!("Some ordinary text about things to remember, the number {i} among them.\n- a list\n- of things"),
                position: f64::from(i),
                ..Note::default()
            })
        })
        .collect();
    let t0 = Instant::now();
    let hits = crate::model::search_notes(entries.iter(), "remember 4242", None);
    let took = t0.elapsed();
    assert_eq!(hits.len(), 1);
    assert!(took < Duration::from_millis(400), "{took:?}");
    eprintln!("search over 5000 notes: {took:?}");
}

#[test]
fn a_dead_token_asks_for_sign_in_and_keeps_what_was_typed() {
    let mut t = start(seeded(&["note"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    t.fake.0.lock().unwrap().unauthorized = true;
    t.h.app().store.update(
        &id,
        Input {
            title: Some("edited".into()),
            ..Input::default()
        },
        false,
    );
    t.h.app().pump();
    let r = t.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // The account file is the test's own, so forgetting the token touches nothing real.
    unsafe {
        std::env::set_var(
            "LUCINA_SECRETARY_CREDENTIAL",
            std::env::temp_dir().join("lucina-notes-no-token"),
        )
    };
    t.h.core.message(Msg::Reply(r));
    t.pass();
    assert!(t.h.app().needs_signin);
    assert_eq!(t.h.app().store.waiting(), 1);
    assert_eq!(t.h.app().store.note(&id).unwrap().note.title, "edited");
}

#[test]
fn after_closing_a_note_the_keyboard_is_back_in_the_search() {
    let mut t = start(seeded(&["one", "two"]));
    t.press_ctrl(49);
    t.h.type_str("x");
    t.h.key(keys::KEY_ESC);
    t.settle();
    t.h.type_str("two");
    assert_eq!(t.h.app().search.text, "two");
    assert_eq!(t.shown(), ["two"]);
}

/// The evidence for one app (docs/research/juno-apps-notes.md): a note written in one view is in the other without a
/// request, because there is one store and no copy.
#[test]
fn a_note_from_either_view_is_the_same_note_in_the_other() {
    let mut t = start(seeded(&[]));
    t.press_ctrl(49);
    t.h.type_str("written in cards");
    t.h.key(keys::KEY_ESC);
    t.settle();
    let asked = t.fake.0.lock().unwrap().requests.len();
    let in_cards = t.h.app().shown[0].clone();
    t.h.app().mode = Mode::Docs;
    t.pass();
    assert!(std::sync::Arc::ptr_eq(&in_cards, &t.h.app().shown[0]));
    assert_eq!(t.fake.0.lock().unwrap().requests.len(), asked);
}

#[test]
fn opening_an_editor_for_a_blank_list_focuses_its_item() {
    let e = Editor::blank(true, None);
    assert_eq!(e.rows.as_ref().unwrap().len(), 1);
}

#[test]
fn the_parity_scenario_against_the_fake() {
    let f = Fake::new();
    crate::parity::scenario(&|| Box::new(f.clone()));
}

// ------------------------------------------------------------ intents

use lucina_app::intent::{Call, Entity, GONE, Reply as IntentReply, UndoEntry, WriteKind, rules, there};

/// Call an intent whose answer waits for the server, and drive the fake's answers until it comes.
fn intent_reply(t: &mut T, name: &str, args: serde_json::Value) -> IntentReply {
    assert!(t.h.intent(name, args).is_none(), "{name} answers late");
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(r) = t.h.reply() {
            return r;
        }
        assert!(Instant::now() < end, "no answer for {name}");
        match t.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => t.h.core.message(Msg::Reply(r)),
            Err(_) => {
                if !t.h.app().store.in_flight() {
                    t.h.app().store.retry_now();
                    t.h.app().pump();
                }
            }
        }
    }
}

#[test]
fn create_intent_makes_a_note_and_returns_its_real_id_with_an_undoable_write() {
    let mut t = start(seeded(&[]));
    let r = intent_reply(
        &mut t,
        "notes.create",
        json!({"title": "From Lucina", "body": "made by an intent"}),
    );
    assert!(r.ok, "{r:?}");
    let id = r.value["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("n_"), "{id}");
    assert_eq!(r.entities, vec![Entity::new("note", &id, "From Lucina")]);
    let w = r.write.unwrap();
    assert_eq!(w.kind, WriteKind::Create);
    assert_eq!(w.target, format!("note:{id}"));
    assert_eq!(w.entry().unwrap().op, rules::UndoOp::TakeAway);
    assert_eq!(t.h.app().store.note(&id).unwrap().note.title, "From Lucina");
    assert_eq!(t.server_note(|n| n.id == id).content, "made by an intent");
}

#[test]
fn the_undo_of_a_create_trashes_the_note_and_is_refused_once_the_note_changed() {
    let mut t = start(seeded(&[]));
    let r = intent_reply(
        &mut t,
        "notes.create",
        json!({"title": "undo me", "body": "text"}),
    );
    assert!(r.ok, "{r:?}");
    let id = r.value["id"].as_str().unwrap().to_owned();
    let entry: UndoEntry = r.write.unwrap().entry().unwrap().into();
    let undo = |t: &mut T| {
        t.h.intent_call(Call {
            call: "u".into(),
            name: "notes.trash".into(),
            args: json!({"id": id}),
            undo: Some(entry),
        })
    };
    // The undo of the create trashes the note.
    assert!(undo(&mut t).is_none());
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(r) = t.h.reply() {
            assert!(r.ok, "{r:?}");
            break;
        }
        assert!(Instant::now() < end, "no answer");
        if let Ok(r) = t.rx.recv_timeout(Duration::from_millis(50)) {
            t.h.core.message(Msg::Reply(r));
        }
    }
    assert!(t.h.app().store.note(&id).unwrap().note.deleted_at.is_some());
    // Once the note changed, the same undo is refused and the note stays.
    let r = intent_reply(&mut t, "notes.restore", json!({"id": id}));
    assert!(r.ok, "{r:?}");
    t.h.app().store.update(
        &id,
        Input {
            title: Some("edited".into()),
            ..Input::default()
        },
        false,
    );
    t.settle();
    let r = undo(&mut t).unwrap();
    assert!(!r.ok && r.text.contains("changed since"), "{r:?}");
    let n = &t.h.app().store.note(&id).unwrap().note;
    assert!(n.deleted_at.is_none());
    assert_eq!(n.title, "edited");
}

#[test]
fn trash_and_restore_intents_move_a_note_and_refuse_a_bad_target() {
    let mut t = start(seeded(&["keep"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    let r = intent_reply(&mut t, "notes.trash", json!({"id": id}));
    assert!(r.ok, "{r:?}");
    let w = r.write.unwrap();
    assert_eq!(w.kind, WriteKind::Remove);
    assert_eq!(w.target, format!("note:{id}"));
    assert!(t.h.app().store.note(&id).unwrap().note.deleted_at.is_some());
    assert!(t.server_note(|n| n.id == id).deleted_at.is_some());
    let r = intent_reply(&mut t, "notes.restore", json!({"id": id}));
    assert!(r.ok, "{r:?}");
    assert!(t.h.app().store.note(&id).unwrap().note.deleted_at.is_none());
    assert!(t.server_note(|n| n.id == id).deleted_at.is_none());
    let r = t.h.intent("notes.trash", json!({"id": "n_nope"})).unwrap();
    assert!(!r.ok, "{r:?}");
    let r = t.h.intent("notes.restore", json!({"id": id})).unwrap();
    assert!(!r.ok && r.text.contains("not in the trash"), "{r:?}");
}

#[test]
fn search_intent_finds_the_created_note() {
    let mut t = start(seeded(&["Apples"]));
    let r = intent_reply(
        &mut t,
        "notes.create",
        json!({"title": "Zebra plans", "body": "uniquely worded body"}),
    );
    assert!(r.ok, "{r:?}");
    let id = r.value["id"].as_str().unwrap().to_owned();
    let r =
        t.h.intent("notes.search", json!({"query": "uniquely"}))
            .unwrap();
    assert!(r.ok, "{r:?}");
    let notes = r.value["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["id"], json!(id));
    assert_eq!(notes[0]["title"], json!("Zebra plans"));
    assert!(notes[0]["snippet"].as_str().unwrap().len() <= 120);
    assert_eq!(r.entities, vec![Entity::new("note", &id, "Zebra plans")]);
    let r =
        t.h.intent("notes.search", json!({"query": "", "limit": 1}))
            .unwrap();
    assert_eq!(r.value["notes"].as_array().unwrap().len(), 1);
}

#[test]
fn open_intent_opens_the_note_and_refuses_an_unknown_id() {
    let mut t = start(seeded(&["only"]));
    let id = t.h.app().store.all().next().unwrap().note.id.clone();
    let r = t.h.intent("notes.open", json!({"id": id})).unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(t.h.app().editor.as_ref().unwrap().title.text, "only");
    let r = t.h.intent("notes.open", json!({"id": "n_nope"})).unwrap();
    assert!(!r.ok, "{r:?}");
}

/// A headless run never calls `view`: the intent is answered from the messages alone.
#[test]
fn a_headless_create_is_answered_without_any_paint_between_the_call_and_the_answer() {
    use lucina_app::App as _;
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let fake = seeded(&[]);
    let app = Notes::new(
        Some(Box::new(fake.clone())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut h = Harness::new(app, 1920, 1080);
    h.core.message(Msg::Cache(Box::default(), Vec::new()));
    let drive = |h: &mut Harness<Notes>| {
        let end = Instant::now() + Duration::from_secs(5);
        while !h.app().store.idle() && Instant::now() < end {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(r) => h.app().message(Msg::Reply(r)),
                Err(_) => {
                    if !h.app().store.in_flight() {
                        h.app().store.retry_now();
                        h.app().pump();
                    }
                }
            }
        }
    };
    drive(&mut h);
    let (responder, slot) = lucina_app::intent::Responder::slot("notes.create");
    lucina_app::intent::dispatch(
        h.app(),
        Call {
            call: "t".into(),
            name: "notes.create".into(),
            args: json!({"title": "headless", "body": "no screen was drawn"}),
            undo: None,
        },
        responder,
    );
    let end = Instant::now() + Duration::from_secs(5);
    let r = loop {
        if let Some(r) = slot.lock().unwrap().take() {
            break r;
        }
        assert!(Instant::now() < end, "no answer");
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => h.app().message(Msg::Reply(r)),
            Err(_) => {
                if !h.app().store.in_flight() {
                    h.app().store.retry_now();
                    h.app().pump();
                }
            }
        }
    };
    assert!(r.ok, "{r:?}");
    let id = r.value["id"].as_str().unwrap();
    assert!(id.starts_with("n_"), "{id}");
    assert_eq!(h.app().store.note(id).unwrap().note.title, "headless");
}

/// A start-up intent arrives before the cache: nothing is acted on, and the answer comes only once the store is loaded.
#[test]
fn an_intent_before_the_store_is_loaded_is_answered_after_the_cache_arrives() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let fake = seeded(&[]);
    let app = Notes::new(
        Some(Box::new(fake.clone())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut h = Harness::new(app, 1920, 1080);
    assert!(!h.app().store.loaded);
    assert!(h.intent("notes.search", json!({"query": "cached"})).is_none());
    assert!(h.reply().is_none(), "no answer before the store is loaded");
    let cached = Note {
        id: "n_1".into(),
        title: "cached".into(),
        content: "about cached".into(),
        color: "default".into(),
        rev: 5,
        ..Note::default()
    };
    h.core.message(Msg::Cache(
        Box::new(CacheFile {
            cursor: "5".into(),
            labels: vec![],
            notes: vec![cached],
        }),
        Vec::new(),
    ));
    let r = h.reply().expect("answered once the cache is in");
    assert!(r.ok, "{r:?}");
    let notes = r.value["notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["title"], json!("cached"));
    drop(rx);
}

/// A trash with an undo for a note that only exists on the server (in no cache) waits for the first list, then works.
#[test]
fn a_trash_with_undo_for_a_server_only_note_waits_for_the_first_list_and_works() {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let fake = seeded(&["server only"]);
    let server = fake.0.lock().unwrap().notes[0].clone();
    let app = Notes::new(
        Some(Box::new(fake.clone())),
        move |r| {
            let _ = tx.lock().map(|t| t.send(r));
        },
        None,
        None,
    );
    let mut h = Harness::new(app, 1920, 1080);
    // The undo of a create, as the journal would hold it for this note.
    let entry = UndoEntry::from(rules::Entry {
        before: GONE,
        after: there(server.updated_at.as_bytes()),
        op: rules::UndoOp::TakeAway,
    });
    assert!(
        h.intent_call(Call {
            call: "u".into(),
            name: "notes.trash".into(),
            args: json!({"id": server.id}),
            undo: Some(entry),
        })
        .is_none(),
        "waits while the store is not loaded"
    );
    assert!(h.reply().is_none());
    // No cache was ever delivered: the first list is what loads the store.
    h.app().pump();
    let end = Instant::now() + Duration::from_secs(5);
    let r = loop {
        if let Some(r) = h.reply() {
            break r;
        }
        assert!(Instant::now() < end, "no answer");
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => h.core.message(Msg::Reply(r)),
            Err(_) => {
                if !h.app().store.in_flight() {
                    h.app().store.retry_now();
                    h.app().pump();
                }
            }
        }
    };
    assert!(r.ok, "{r:?}");
    assert_eq!(r.write.unwrap().kind, WriteKind::Remove);
    assert!(h.app().store.note(&server.id).unwrap().note.deleted_at.is_some());
    // The trash reached the server.
    let end = Instant::now() + Duration::from_secs(5);
    while !h.app().store.idle() && Instant::now() < end {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(r) => h.core.message(Msg::Reply(r)),
            Err(_) => {
                if !h.app().store.in_flight() {
                    h.app().store.retry_now();
                    h.app().pump();
                }
            }
        }
    }
    assert!(
        fake.0
            .lock()
            .unwrap()
            .notes
            .iter()
            .find(|n| n.id == server.id)
            .unwrap()
            .deleted_at
            .is_some()
    );
}
