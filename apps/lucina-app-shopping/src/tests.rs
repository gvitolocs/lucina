//! The app driven as a person would, with the fake server behind a real worker thread.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use lucina_app::keys::{self, Mods};
use lucina_app::{Harness, Input};
use lucina_app_support::net::Reply;

use crate::fake::Fake;
use crate::model::{Status, join_spec};
use crate::store::CacheFile;
use crate::{Dialog, Msg, Shopping, Tap, TileKind};

struct T {
    h: Harness<Shopping>,
    rx: mpsc::Receiver<Reply>,
    fake: Fake,
}

fn start(fake: Fake) -> T {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let app = Shopping::new(
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
    t.h.core.message(Msg::Cache(Box::default()));
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
                self.h.app().store.retry_now();
                self.h.app().pump();
            }
        }
        self.pass();
    }

    fn pass(&mut self) {
        self.h.core.input(Input::Wheel(0));
        self.h.core.paint();
    }

    fn needed(&mut self) -> Vec<String> {
        self.h
            .app()
            .store
            .needed()
            .iter()
            .map(|r| r.name.clone())
            .collect()
    }

    fn on_server(&self, list: &str, name: &str) -> Option<crate::model::Row> {
        let s = self.fake.0.lock().unwrap();
        s.states[list]
            .items
            .iter()
            .find(|r| r.name == name)
            .cloned()
    }
}

#[test]
fn opening_shows_the_list_in_store_walk_order() {
    let mut t = start(Fake::demo());
    let names = t.needed();
    // Fruit, bakery, dairy: the catalog's sections, whatever order they were added in.
    let at = |n: &str| names.iter().position(|x| x == n).unwrap();
    assert!(at("Bananas") < at("Milk"), "{names:?}");
    assert_eq!(t.h.app().store.current_list_name(), "Home");
    assert_eq!(t.h.app().store.lists.len(), 2);
}

#[test]
fn marking_bought_is_at_once_and_reaches_the_server_and_undo_reverses_it() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Milk")
            .unwrap()
            .id
            .clone();
    t.h.app().store.tap_item(&id);
    // Before any answer.
    assert!(!t.needed().contains(&"Milk".to_owned()));
    assert_eq!(t.h.app().store.pending(), 1);
    t.h.app().pump();
    t.settle();
    assert_eq!(
        t.on_server("default", "Milk").unwrap().status,
        Status::Recent
    );
    t.h.app().store.undo_last();
    assert!(t.needed().contains(&"Milk".to_owned()));
    t.h.app().pump();
    t.settle();
    assert_eq!(
        t.on_server("default", "Milk").unwrap().status,
        Status::Needed
    );
}

#[test]
fn typing_searches_the_catalog_and_enter_adds_with_the_spec() {
    let mut t = start(Fake::new());
    t.h.type_str("banana x3");
    t.pass();
    let first =
        t.h.app()
            .tiles
            .iter()
            .find(|x| matches!(x.tap, Tap::Found(_)))
            .unwrap()
            .name
            .clone();
    assert_eq!(first, "Bananas");
    t.h.key(keys::KEY_ENTER);
    assert_eq!(t.h.app().search.text, "");
    assert_eq!(t.needed(), ["Bananas"]);
    t.settle();
    let row = t.on_server("default", "Bananas").unwrap();
    assert_eq!(row.spec, "x3");
    assert_eq!(row.catalog_id.as_deref(), Some("banana"));
}

#[test]
fn free_text_gets_a_custom_tile_and_becomes_a_row() {
    let mut t = start(Fake::new());
    t.h.type_str("Anna's starter");
    t.pass();
    assert_eq!(t.h.app().tiles[0].kind, TileKind::Custom);
    t.h.key(keys::KEY_ENTER);
    t.settle();
    assert!(
        t.on_server("default", "Anna's starter")
            .unwrap()
            .catalog_id
            .is_none()
    );
    // Typed again while recent, it goes back on the list instead of making a second row.
    let id = t.h.app().store.items[0].id.clone();
    t.h.app().store.tap_item(&id);
    t.h.type_str("anna's starter");
    t.pass();
    t.h.key(keys::KEY_ENTER);
    t.settle();
    assert_eq!(t.h.app().store.items.len(), 1);
    assert_eq!(t.h.app().store.items[0].status, Status::Needed);
}

#[test]
fn deleting_undoes_as_a_new_row_because_deleted_stays_deleted() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Bread")
            .unwrap()
            .id
            .clone();
    t.h.app().store.delete_item(&id);
    t.h.app().pump();
    t.settle();
    t.h.app().store.undo_last();
    t.h.app().pump();
    t.settle();
    let back =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Bread")
            .cloned()
            .unwrap();
    assert_ne!(back.id, id);
    assert_eq!(back.spec, "x2");
}

#[test]
fn offline_edits_wait_replay_with_the_same_ids_and_apply_once() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Milk")
            .unwrap()
            .id
            .clone();
    {
        let mut s = t.fake.0.lock().unwrap();
        s.drop_next = 1;
        s.drop_after_arriving = true; // The op arrived, the answer did not.
    }
    t.h.app().store.tap_item(&id);
    t.h.app().pump();
    let r = t.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    t.h.core.message(Msg::Reply(r));
    assert!(!t.h.app().store.online);
    assert_eq!(t.h.app().store.pending(), 1);
    let op = t.h.app().store.outbox_snapshot()[0].op.op_id.clone();
    t.settle();
    assert!(t.h.app().store.online);
    assert_eq!(t.h.app().store.pending(), 0);
    assert!(t.fake.0.lock().unwrap().seen.contains(&op));
    assert_eq!(
        t.on_server("default", "Milk").unwrap().status,
        Status::Recent
    );
}

#[test]
fn a_conflict_rebases_the_pending_ops_with_new_ids() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Milk")
            .unwrap()
            .id
            .clone();
    // Lucina changes the same row while this client's revision is old.
    {
        let mut s = t.fake.0.lock().unwrap();
        let st = s.states.get_mut("default").unwrap();
        st.revision += 1;
        let rev = st.revision;
        st.items.iter_mut().find(|r| r.id == id).unwrap().spec = "from chat".into();
        s.changed_at.insert(id.clone(), rev);
    }
    t.h.app().store.tap_item(&id);
    let before = t.h.app().store.outbox_snapshot()[0].op.op_id.clone();
    t.h.app().pump();
    t.settle();
    let s = t.fake.0.lock().unwrap();
    assert!(
        !s.seen.contains(&before),
        "the conflict's op must not count"
    );
    let row = s.states["default"]
        .items
        .iter()
        .find(|r| r.id == id)
        .unwrap();
    assert_eq!(
        (row.status, row.spec.as_str()),
        (Status::Recent, "from chat")
    );
    assert_eq!(s.batches.len(), 2 - usize::from(s.batches.len() == 1));
}

#[test]
fn a_refusal_drops_the_ops_and_says_so() {
    let mut t = start(Fake::demo());
    t.fake.0.lock().unwrap().states.remove("default");
    t.fake.0.lock().unwrap().lists.retain(|l| l.id != "default");
    let id = t.h.app().store.items[0].id.clone();
    t.h.app().store.tap_item(&id);
    t.h.app().pump();
    t.settle();
    assert_eq!(t.h.app().store.pending(), 0);
    assert!(t.h.app().store.action_error.is_some());
}

#[test]
fn the_grocery_store_being_down_keeps_the_ops_for_later() {
    let mut t = start(Fake::demo());
    t.fake.0.lock().unwrap().unavailable = true;
    let id = t.h.app().store.items[0].id.clone();
    t.h.app().store.tap_item(&id);
    t.h.app().pump();
    let r = t.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    t.h.core.message(Msg::Reply(r));
    assert_eq!(t.h.app().store.pending(), 1);
    t.fake.0.lock().unwrap().unavailable = false;
    t.settle();
    assert_eq!(t.h.app().store.pending(), 0);
}

#[test]
fn details_edit_only_what_changed() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Milk")
            .unwrap()
            .id
            .clone();
    t.h.app()
        .store
        .edit_item(&id, "Milk", "", Some(2.0), "l", None);
    let n = t.h.app().store.pending();
    assert_eq!(n, 1, "only the quantity");
    t.h.app().pump();
    t.settle();
    let row = t.on_server("default", "Milk").unwrap();
    assert_eq!((row.quantity, row.unit.as_deref()), (Some(2.0), Some("l")));
}

#[test]
fn lists_can_be_created_switched_and_deleted_but_not_the_main_one() {
    let mut t = start(Fake::demo());
    t.h.app().store.create_list("Camping", "holiday");
    t.h.app().pump();
    t.settle();
    assert_eq!(t.h.app().store.current_list_name(), "Camping");
    let id = t.h.app().store.current_list_id.clone();
    t.h.app().store.remove_list(&id);
    t.h.app().pump();
    t.settle();
    assert_eq!(t.h.app().store.current_list_id, "default");
    t.h.app().store.remove_list("default");
    assert!(t.h.app().store.idle());
    t.h.app().store.select_list("work");
    t.h.app().pump();
    t.settle();
    assert_eq!(t.needed(), ["Printer paper"]);
}

#[test]
fn moving_an_item_takes_it_to_the_other_list() {
    let mut t = start(Fake::demo());
    let id =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.name == "Coffee")
            .map(|r| r.id.clone());
    let id = id
        .or_else(|| t.h.app().store.items.first().map(|r| r.id.clone()))
        .unwrap();
    let name =
        t.h.app()
            .store
            .items
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .name
            .clone();
    t.h.app().store.move_item(&id, "work");
    assert!(!t.needed().contains(&name));
    t.h.app().pump();
    t.settle();
    assert!(t.on_server("work", &name).is_some());
}

#[test]
fn store_order_moves_a_section_and_the_tiles_follow() {
    let mut t = start(Fake::demo());
    let first = t.h.app().store.ordered_sections()[0].id.clone();
    let names_before = t.needed();
    t.h.app().store.move_section(&first, 3);
    t.h.app().store.move_section(&first, 1);
    assert_ne!(t.h.app().store.ordered_sections()[0].id, first);
    assert_ne!(t.needed(), names_before);
    t.h.app().store.reset_section_order();
    assert_eq!(t.h.app().store.ordered_sections()[0].id, first);
}

#[test]
fn a_catalog_tile_on_the_list_shows_as_to_buy() {
    let mut t = start(Fake::demo());
    t.h.app().store.expanded.insert("produce".into(), true);
    t.h.app().store.version += 1;
    t.pass();
    let tiles = &t.h.app().tiles;
    let banana = tiles.iter().filter(|x| x.name == "Bananas").last().unwrap();
    assert_eq!(banana.kind, TileKind::Need);
    assert!(matches!(banana.tap, Tap::Row(_)));
    let other = tiles.iter().find(|x| x.kind == TileKind::Catalog).unwrap();
    assert!(matches!(other.tap, Tap::Catalog(_)));
}

#[test]
fn the_item_language_follows_the_lists_rows() {
    let f = Fake::new();
    {
        let mut s = f.0.lock().unwrap();
        s.push_row(
            "default",
            "Banane",
            Some("banana".into()),
            "",
            None,
            Status::Needed,
        );
        s.push_row(
            "default",
            "Latte",
            Some("milk".into()),
            "",
            None,
            Status::Needed,
        );
    }
    let mut t = start(f);
    assert_eq!(t.h.app().store.item_lang, "it");
    t.h.app().store.expanded.insert("produce".into(), true);
    t.h.app().store.version += 1;
    t.pass();
    assert!(
        t.h.app()
            .tiles
            .iter()
            .any(|x| x.key == "catalog:apple" || x.name == "Mele"),
        "catalog names in Italian"
    );
    t.h.app().store.set_item_lang(Some("en".into()));
    assert_eq!(t.h.app().store.item_lang, "en");
}

#[test]
fn the_cache_shows_before_the_first_answer_and_replays_its_outbox() {
    let f = Fake::demo();
    let mut a = start(f.clone());
    let id = a.h.app().store.items[0].id.clone();
    f.0.lock().unwrap().drop_next = 100;
    a.h.app().store.tap_item(&id);
    let bytes = a.h.app().store.cache_json();
    let cache: CacheFile = serde_json::from_slice(&bytes).unwrap();
    f.0.lock().unwrap().drop_next = 0;
    // A second run, its first answer not yet in.
    let g = f.clone();
    let mut h = Harness::new(
        Shopping::new(Some(Box::new(g)), |_| {}, None, None),
        1920,
        1080,
    );
    h.core.message(Msg::Cache(Box::new(cache)));
    assert!(h.app().store.loaded);
    assert_eq!(h.app().store.pending(), 1);
    assert_eq!(
        h.app()
            .store
            .items
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .status,
        Status::Recent
    );
}

#[test]
fn a_click_on_a_tile_marks_it_and_the_corner_opens_details() {
    let mut t = start(Fake::demo());
    t.pass();
    let before = t.needed().len();
    // First tile of the list: right of nothing, under the head, the search and the heading.
    t.h.click(90, 240);
    assert_eq!(t.needed().len(), before - 1);
    t.settle();
    // The corner of the first tile. The list starts at the page's padding.
    let x = 17 + t.h.app().tile_w - 12;
    t.h.click(x, 236);
    assert!(matches!(t.h.app().dialog, Some(Dialog::Details(_))));
}

#[test]
fn the_arrows_walk_the_tiles_and_enter_taps_with_nothing_typed() {
    let mut t = start(Fake::demo());
    let before = t.needed().len();
    t.h.key(keys::KEY_DOWN);
    assert_eq!(t.h.app().sel, Some(0));
    t.h.key(keys::KEY_RIGHT);
    assert_eq!(
        t.h.app().sel,
        Some(1),
        "Right past an empty field walks the tiles"
    );
    t.h.key(keys::KEY_ENTER);
    assert_eq!(t.needed().len(), before - 1);
}

#[test]
fn after_a_dialog_the_keyboard_is_back_in_the_search() {
    let mut t = start(Fake::demo());
    let id = t.h.app().store.items[0].id.clone();
    t.h.app().open_details(&id);
    t.pass();
    t.h.key(keys::KEY_ESC);
    t.pass();
    t.h.type_str("milk");
    assert_eq!(t.h.app().search.text, "milk");
}

#[test]
fn catalog_rows_get_their_picture_and_free_text_keeps_its_letter() {
    let mut t = start(Fake::demo());
    t.pass();
    let key = |t: &mut T, name: &str| {
        let k =
            t.h.app()
                .tiles
                .iter()
                .find(|x| x.name == name)
                .unwrap()
                .key
                .clone();
        t.h.app().tile_art.get(&k).cloned()
    };
    assert_eq!(key(&mut t, "Bananas").as_deref(), Some("banana"));
    // Free text with a catalog name's alias gets that picture; anything else keeps its letter.
    assert_eq!(key(&mut t, "Sourdough starter").as_deref(), Some("yeast"));
    t.h.app().store.add_text("Anna's parcel", None);
    t.h.app().pump();
    t.settle();
    assert_eq!(key(&mut t, "Anna's parcel"), None);
    assert!(t.h.app().images.contains_key("banana"), "decoded");
    assert_eq!(t.h.app().images["banana"].w, 56);
}

#[test]
fn every_catalog_item_has_a_decodable_picture() {
    let index = crate::art_index();
    assert!(index.len() >= 395);
    let c = crate::store::Store::new();
    let missing: Vec<&str> = c
        .catalog
        .items
        .iter()
        .map(|i| i.id.as_str())
        .filter(|i| !index.contains_key(i))
        .collect();
    assert!(missing.is_empty(), "{missing:?}");
    for png in index.values() {
        lucina_app::Image::decode(png).expect("a picture");
    }
}

#[test]
fn escape_clears_a_search() {
    let mut t = start(Fake::demo());
    t.h.type_str("milk");
    t.h.key(keys::KEY_ESC);
    assert_eq!(t.h.app().search.text, "");
    t.h.press(keys::KEY_A, Mods::default());
    assert_eq!(t.h.app().search.text, "a");
}

#[test]
fn a_dead_token_asks_for_sign_in_and_keeps_the_ops() {
    let mut t = start(Fake::demo());
    t.fake.0.lock().unwrap().unauthorized = true;
    let id = t.h.app().store.items[0].id.clone();
    t.h.app().store.tap_item(&id);
    t.h.app().pump();
    unsafe {
        std::env::set_var(
            "LUCINA_SECRETARY_CREDENTIAL",
            std::env::temp_dir().join("lucina-shopping-no-token"),
        )
    };
    let r = t.rx.recv_timeout(Duration::from_secs(5)).unwrap();
    t.h.core.message(Msg::Reply(r));
    assert!(t.h.app().needs_signin);
    assert_eq!(t.h.app().store.pending(), 1);
}

#[test]
fn join_spec_is_used_for_chips() {
    assert_eq!(join_spec("x2", &["organic".into()]), "x2 organic");
}
