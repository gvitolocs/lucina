//! The app against the sample library, and against the real `lucina-media` through the real client.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lucina_app::time::Zone;
use lucina_app::{Harness, keys};
use lucina_files::engine::Engine;
use lucina_files::fake::Fake;
use lucina_files::media::Connect;
use lucina_files::pictures::{Done, Fit, Pictures};

use crate::app::{Dialog, Gallery, Link, Shelf};
use crate::server::{Job, Msg, Worker};

const NOW: i64 = 1_790_000_000_000;

struct Rig {
    h: Harness<Gallery>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
    pictures: Arc<Mutex<Vec<Done>>>,
}

impl Rig {
    fn on(connect: Connect, w: u32, h: u32) -> Rig {
        let media = connect().expect("a server");
        let worker = Worker::new(media, connect.clone(), Arc::new(|_| {}));
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let mut harness = Harness::new(Gallery::new(Zone::utc(), None, Some(link), None), w, h);
        let pictures: Arc<Mutex<Vec<Done>>> = Default::default();
        let sink = pictures.clone();
        harness.app().pictures = Some(Pictures::start(connect, None, move |d| sink.lock().unwrap().push(d)));
        harness.app().start();
        let mut r = Rig { h: harness, w: worker, rx, pictures };
        r.pump();
        r
    }

    fn new(n: u32) -> (Rig, Fake) {
        let fake = Fake::new(NOW, n);
        (Rig::on(fake.connect(), 1024, 768), fake)
    }

    fn pump(&mut self) {
        for _ in 0..3 {
            self.h.core.input(lucina_app::Input::Wheel(0));
            self.h.core.paint();
            while let Ok(job) = self.rx.try_recv() {
                for m in self.w.handle(job) {
                    self.h.core.message(m);
                }
            }
            for d in self.pictures.lock().unwrap().drain(..) {
                self.h.core.message(Msg::Picture(d));
            }
        }
        self.h.core.input(lucina_app::Input::Wheel(0));
        self.h.core.paint();
    }

    fn key(&mut self, k: u16) {
        self.h.key(k);
        self.pump();
    }

    fn until(&mut self, what: &str, mut ok: impl FnMut(&mut Gallery) -> bool) {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            self.pump();
            if ok(self.h.app()) {
                return;
            }
            assert!(Instant::now() < end, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn the_first_page_comes_and_the_next_follows_as_the_grid_nears_its_end() {
    let (mut r, _) = Rig::new(450);
    assert_eq!(r.h.app().items.len(), 200);
    assert!(r.h.app().has_more());
    r.key(keys::KEY_END);
    r.pump();
    assert!(r.h.app().items.len() >= 400, "{}", r.h.app().items.len());
    r.key(keys::KEY_END);
    r.pump();
    r.key(keys::KEY_END);
    r.pump();
    assert_eq!(r.h.app().items.len(), 450);
    assert!(!r.h.app().has_more());
}

#[test]
fn the_grid_is_cut_by_day_with_the_webs_labels() {
    let (mut r, _) = Rig::new(30);
    let rows = r.h.app().rows.clone();
    let headers = rows.iter().filter(|x| matches!(x, crate::app::GridRow::Header { .. })).count();
    assert!(headers >= 3, "{headers}");
    let first = crate::model::section_key(r.h.app().items[0].at_ms, &Zone::utc());
    assert_eq!(first.len(), 10);
}

#[test]
fn arrows_move_by_row_and_stay_in_the_column() {
    let (mut r, _) = Rig::new(60);
    let cols = r.h.app().rows.iter().find_map(|x| if let crate::app::GridRow::Tiles { start, end } = x { Some(end - start) } else { None }).unwrap();
    assert!(cols >= 3, "{cols}");
    r.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().focus, 1);
    r.key(keys::KEY_DOWN);
    // A short day may end the row sooner: still the same column, or the last of a short row.
    let after = r.h.app().focus;
    assert!(after > 1, "{after}");
    r.key(keys::KEY_UP);
    assert_eq!(r.h.app().focus, 1);
    r.key(keys::KEY_LEFT);
    assert_eq!(r.h.app().focus, 0);
    r.key(keys::KEY_LEFT);
    assert_eq!(r.h.app().focus, 0);
}

#[test]
fn a_tile_arrives_as_a_small_picture_and_those_far_away_are_let_go() {
    let (mut r, _) = Rig::new(450);
    r.until("the first screen's tiles", |g| g.tiles.len() >= 12);
    let side = r.h.app().tiles.values().next().map(|t| t.image.w).unwrap();
    assert!(side <= 200, "a tile is small: {side}");
    // A long way down, a screen at a time: what was on the first screen goes, within the limit.
    for _ in 0..16 {
        r.h.key(keys::KEY_PAGEDOWN);
        r.until("tiles at the new place", |g| {
            let at = g.focus;
            g.tiles.contains_key(&g.items[at].id)
        });
    }
    let (kept, keep) = (r.h.app().tiles.len(), r.h.app().keep);
    assert!(kept <= keep + 8, "{kept} kept with room for {keep}");
    let id0 = r.h.app().items[0].id.clone();
    assert!(!r.h.app().tiles.contains_key(&id0), "the first tile was let go, {kept} kept");
    // What the keep costs: three screens at most of small pictures.
    let bytes: usize = r.h.app().tiles.values().map(|t| t.image.px.len() * 4).sum();
    assert!(bytes < 48 << 20, "{bytes}");
}

#[test]
fn enter_opens_the_picture_and_arrows_walk_and_escape_returns_to_it() {
    let (mut r, _) = Rig::new(40);
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().viewer.as_ref().map(|v| v.index), Some(1));
    r.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().viewer.as_ref().map(|v| v.index), Some(2));
    r.key(keys::KEY_LEFT);
    r.key(keys::KEY_LEFT);
    r.key(keys::KEY_LEFT);
    assert_eq!(r.h.app().viewer.as_ref().map(|v| v.index), Some(0));
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_ESC);
    assert!(r.h.app().viewer.is_none());
    assert_eq!(r.h.app().focus, 1);
}

#[test]
fn closing_the_viewer_gives_the_keyboard_back_to_the_grid() {
    // Found on a QEMU capture: the toolbar had it, so Delete did nothing and Space pressed Trash.
    let (mut r, _) = Rig::new(10);
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_ESC);
    r.key(keys::KEY_DELETE);
    assert!(matches!(r.h.app().dialog, Some(Dialog::Delete { .. })));
    r.key(keys::KEY_ESC);
    r.key(keys::KEY_SPACE);
    assert_eq!(r.h.app().selected.len(), 1);
    assert_eq!(r.h.app().shelf, Shelf::Everything);
}

#[test]
fn the_caption_field_does_not_take_the_viewers_keys() {
    let (mut r, _) = Rig::new(4);
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_I);
    r.key(keys::KEY_F);
    assert_eq!(r.h.app().viewer.as_ref().unwrap().caption.text, "", "f went to the viewer, not the field");
}

#[test]
fn enter_on_a_video_plays_it_and_without_a_player_says_so() {
    let (mut r, _) = Rig::new(20);
    let at = r.h.app().items.iter().position(|i| i.video).unwrap();
    r.h.app().open_viewer(at);
    r.pump();
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().toast, "The video player is not on this machine.");
    assert!(!r.h.app().viewer.as_ref().unwrap().is_zoomed());
}

#[test]
fn the_viewer_shows_a_picture_fitted_to_the_screen() {
    let (mut r, _) = Rig::new(4);
    r.key(keys::KEY_ENTER);
    r.until("a fitted picture", |g| g.viewer.as_ref().unwrap().wants(&g.items).iter().all(|w| w.url != g.items[0].preview_url().unwrap()));
}

#[test]
fn zooming_pans_with_the_arrows_and_decodes_the_larger_form() {
    let (mut r, _) = Rig::new(4);
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_ENTER);
    assert!(r.h.app().viewer.as_ref().unwrap().is_zoomed());
    let wants = {
        let g = r.h.app();
        g.viewer.as_ref().unwrap().wants(&g.items)
    };
    assert!(wants.iter().any(|w| matches!(w.fit, Fit::Within(8192, 8192))), "the zoom wants an unfitted picture: {wants:?}");
    let before = *r.h.app().viewer.as_ref().unwrap().picture.view().unwrap();
    r.key(keys::KEY_RIGHT);
    let after = *r.h.app().viewer.as_ref().unwrap().picture.view().unwrap();
    assert_ne!(before.offset_x, after.offset_x, "right pans rather than walks");
    assert_eq!(r.h.app().viewer.as_ref().unwrap().index, 0);
    r.key(keys::KEY_0);
    assert!(!r.h.app().viewer.as_ref().unwrap().is_zoomed());
}

#[test]
fn f_favourites_at_once_and_the_server_keeps_it() {
    let (mut r, fake) = Rig::new(4);
    let was = r.h.app().items[0].favorite;
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_F);
    assert_eq!(r.h.app().items[0].favorite, !was);
    let mut m = (fake.connect())().unwrap();
    let id = r.h.app().items[0].id.clone();
    let v = m.get(&format!("/v1/media/items/{id}")).unwrap();
    assert_eq!(v["favorite"], !was);
}

#[test]
fn a_change_the_server_refuses_is_put_back_and_said() {
    let (mut r, fake) = Rig::new(4);
    let was = r.h.app().items[0].favorite;
    r.key(keys::KEY_ENTER);
    fake.offline.store(true, Ordering::SeqCst);
    r.key(keys::KEY_F);
    assert_eq!(r.h.app().items[0].favorite, was);
    assert_eq!(r.h.app().toast, "Couldn't save the change. Try again.");
}

#[test]
fn r_rotates_clockwise_and_the_picture_is_fetched_turned() {
    let (mut r, _) = Rig::new(4);
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_R);
    assert_eq!(r.h.app().items[0].rotation, 90);
    let wants = {
        let g = r.h.app();
        g.viewer.as_ref().unwrap().wants(&g.items)
    };
    assert_eq!(wants[0].rotation, 90);
    r.key(keys::KEY_R);
    r.key(keys::KEY_R);
    r.key(keys::KEY_R);
    assert_eq!(r.h.app().items[0].rotation, 0);
}

#[test]
fn delete_asks_says_the_originals_stay_and_moves_to_the_trash() {
    let (mut r, fake) = Rig::new(6);
    let id = r.h.app().items[0].id.clone();
    r.key(keys::KEY_DELETE);
    assert!(matches!(r.h.app().dialog, Some(Dialog::Delete { purge: false, .. })));
    // Nothing has happened yet.
    assert_eq!(r.h.app().items.len(), 6);
    r.key(keys::KEY_ESC);
    assert!(r.h.app().dialog.is_none());
    assert_eq!(r.h.app().items.len(), 6);
    r.h.app().open_delete(vec![id.clone()]);
    r.h.app().run_batch("trash", vec![id.clone()], None, false);
    r.pump();
    assert_eq!(r.h.app().items.len(), 5);
    assert_eq!(r.h.app().toast, "1 item moved to the Trash.");
    let mut m = (fake.connect())().unwrap();
    assert!(!m.get(&format!("/v1/media/items/{id}")).unwrap()["trashed_at"].is_null());
    // In the Trash: there, and it goes back.
    r.key(keys::KEY_T);
    assert_eq!(r.h.app().shelf, Shelf::Trash);
    assert_eq!(r.h.app().items.iter().map(|i| i.id.clone()).collect::<Vec<_>>(), [id.clone()]);
    r.h.app().run_batch("restore", vec![id.clone()], None, false);
    r.pump();
    assert_eq!(r.h.app().items.len(), 0);
    r.key(keys::KEY_ESC);
    assert_eq!(r.h.app().shelf, Shelf::Everything);
    assert_eq!(r.h.app().items.len(), 6);
}

#[test]
fn delete_forever_needs_the_tick() {
    let (mut r, fake) = Rig::new(3);
    let id = r.h.app().items[0].id.clone();
    r.h.app().run_batch("trash", vec![id.clone()], None, false);
    r.pump();
    r.key(keys::KEY_T);
    r.key(keys::KEY_DELETE);
    assert!(matches!(r.h.app().dialog, Some(Dialog::Delete { purge: true, understood: false, .. })));
    // The primary button, with the box unticked: Tab to it is Enter on a disabled action.
    for _ in 0..4 {
        r.h.key(keys::KEY_TAB);
        r.h.key(keys::KEY_ENTER);
        r.pump();
        if r.h.app().dialog.is_none() {
            break;
        }
    }
    let mut m = (fake.connect())().unwrap();
    let still = m.get(&format!("/v1/media/items/{id}"));
    if let Some(Dialog::Delete { understood: true, .. }) = r.h.app().dialog {
        // Enter on the box ticked it first: that is the tick.
    } else {
        assert!(still.is_ok(), "gone with no tick");
    }
}

#[test]
fn space_selects_and_a_batch_goes_to_all_of_them() {
    let (mut r, fake) = Rig::new(10);
    r.key(keys::KEY_SPACE);
    r.key(keys::KEY_RIGHT);
    r.key(keys::KEY_SPACE);
    assert_eq!(r.h.app().selected.len(), 2);
    let ids = r.h.app().selected_ids();
    r.h.app().items.iter_mut().for_each(|i| i.favorite = false);
    r.key(keys::KEY_F);
    let mut m = (fake.connect())().unwrap();
    for id in &ids {
        assert_eq!(m.get(&format!("/v1/media/items/{id}")).unwrap()["favorite"], true);
    }
    assert!(r.h.app().selected.is_empty());
    assert_eq!(r.h.app().toast, "2 items added to favorites.");
}

#[test]
fn shift_arrows_select_a_range_and_escape_clears_it() {
    let (mut r, _) = Rig::new(10);
    r.h.press(keys::KEY_RIGHT, keys::Mods { shift: true, ..Default::default() });
    r.h.press(keys::KEY_RIGHT, keys::Mods { shift: true, ..Default::default() });
    r.pump();
    assert_eq!(r.h.app().selected.len(), 3);
    r.key(keys::KEY_ESC);
    assert!(r.h.app().selected.is_empty());
}

#[test]
fn albums_open_and_a_new_one_gets_the_photos() {
    let (mut r, fake) = Rig::new(8);
    r.key(keys::KEY_A);
    assert!(r.h.app().albums_page);
    r.key(keys::KEY_ENTER);
    assert!(matches!(r.h.app().shelf, Shelf::Album { .. }));
    let n = r.h.app().items.len();
    assert!(n > 0 && n < 8, "{n}");
    r.key(keys::KEY_ESC);
    assert_eq!(r.h.app().shelf, Shelf::Everything);
    let id = r.h.app().items[1].id.clone();
    r.h.app().open_album_picker(vec![id.clone()]);
    r.pump();
    assert!(matches!(r.h.app().dialog, Some(Dialog::AddToAlbum(_))));
    let job = Job::CreateAlbum { title: "New one".into(), add: vec![id.clone()] };
    r.h.app().dialog = None;
    let msgs = r.w.handle(job);
    for m in msgs {
        r.h.core.message(m);
    }
    r.pump();
    assert_eq!(r.h.app().toast, "1 item added to the album.");
    let mut m = (fake.connect())().unwrap();
    assert_eq!(m.get(&format!("/v1/media/items/{id}")).unwrap()["albums"].as_array().unwrap().len(), 1 + usize::from(r.h.app().items[1].albums.len() > 1));
}

#[test]
fn searching_filters_after_a_pause_and_escape_brings_everything_back() {
    let (mut r, _) = Rig::new(40);
    r.key(keys::KEY_SLASH);
    r.h.type_str("IMG_1003");
    std::thread::sleep(Duration::from_millis(320));
    r.pump();
    assert_eq!(r.h.app().items.len(), 1);
    r.key(keys::KEY_ESC);
    assert_eq!(r.h.app().items.len(), 40);
}

#[test]
fn the_source_switcher_filters_and_favourites_too() {
    let (mut r, _) = Rig::new(40);
    r.h.app().origin = "lucina".into();
    r.h.app().reload();
    r.pump();
    assert!(r.h.app().items.iter().all(|i| i.source.starts_with("lucina")));
    assert!(!r.h.app().items.is_empty());
    r.h.app().origin = "all".into();
    r.h.app().favorites = true;
    r.h.app().reload();
    r.pump();
    assert!(r.h.app().items.iter().all(|i| i.favorite));
    assert!(!r.h.app().items.is_empty());
}

#[test]
fn a_month_far_down_is_reached_page_by_page() {
    let (mut r, _) = Rig::new(1200);
    assert_eq!(r.h.app().items.len(), 200);
    let oldest = {
        let last_ms = NOW - (1199 / 9) * 86_400_000 - 1199 * 1_800_000;
        let (y, m, _) = lucina_files::drive::civil(last_ms.div_euclid(86_400_000));
        format!("{y}-{m:02}")
    };
    r.h.app().seek(oldest.clone());
    for _ in 0..8 {
        r.pump();
    }
    let at = r.h.app().focus;
    assert!(at > 200, "{at}");
    let key = crate::model::section_key(r.h.app().items[at].at_ms, &Zone::utc());
    assert!(key.starts_with(&oldest), "{key} {oldest}");
}

#[test]
fn a_lost_connection_is_said_and_a_dead_token_signs_out() {
    let (mut r, fake) = Rig::new(4);
    fake.offline.store(true, Ordering::SeqCst);
    r.h.app().reload();
    r.pump();
    assert!(r.h.app().failed.as_deref().unwrap_or("").contains("no connection"));
    fake.offline.store(false, Ordering::SeqCst);
    r.key(keys::KEY_R);
    assert!(r.h.app().failed.is_none());
    assert_eq!(r.h.app().items.len(), 4);
    fake.signed_out.store(true, Ordering::SeqCst);
    r.key(keys::KEY_R);
    assert!(r.h.app().signed_out);
}

#[test]
fn an_answer_for_an_old_query_is_dropped() {
    let (mut r, _) = Rig::new(10);
    r.h.app().favorites = true;
    r.h.app().reload();
    r.h.app().favorites = false;
    r.h.app().reload();
    r.pump();
    assert_eq!(r.h.app().items.len(), 10);
}

#[test]
fn every_screen_draws_at_both_sizes_without_a_panic() {
    for (w, h) in [(800, 600), (1920, 1080)] {
        let fake = Fake::new(NOW, 30);
        let mut r = Rig::on(fake.connect(), w, h);
        r.key(keys::KEY_G);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_S);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_DELETE);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_ENTER);
        r.key(keys::KEY_I);
        r.key(keys::KEY_EQUAL);
        r.key(keys::KEY_EQUAL);
        r.key(keys::KEY_MINUS);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_A);
        r.key(keys::KEY_ESC);
        r.key(keys::KEY_T);
        r.key(keys::KEY_ESC);
    }
}

// ---- The intents.

#[test]
fn gallery_show_opens_a_picture_by_id_and_refuses_an_unknown_one() {
    use serde_json::json;
    let (mut r, _) = Rig::new(10);
    let id = r.h.app().items[2].id.clone();
    let reply = r.h.intent("gallery.show", json!({"id": id})).unwrap();
    assert!(reply.ok && reply.value["shown"] == r.h.app().items[2].title, "{reply:?}");
    assert_eq!(r.h.app().viewer.as_ref().map(|v| v.index), Some(2));
    assert!(reply.entities.iter().any(|e| e.kind == "picture" && e.id == id), "{reply:?}");
    let reply = r.h.intent("gallery.show", json!({"id": "itm_nope"})).unwrap();
    assert!(!reply.ok && reply.text.contains("no picture with id"), "{reply:?}");
}

#[test]
fn gallery_show_by_album_name_is_case_insensitive_and_the_newest_view_needs_no_argument() {
    use serde_json::json;
    let (mut r, _) = Rig::new(10);
    let reply = r.h.intent("gallery.show", json!({"album": "HOLIDAY"})).unwrap();
    assert!(reply.ok, "{reply:?}");
    assert!(matches!(r.h.app().shelf, Shelf::Album { .. }));
    assert!(reply.entities.iter().any(|e| e.kind == "album" && e.title == "Holiday"), "{reply:?}");
    let reply = r.h.intent("gallery.show", json!({"album": "Nope"})).unwrap();
    assert!(!reply.ok && reply.text.contains("no album named"), "{reply:?}");
    let reply = r.h.intent("gallery.show", json!({})).unwrap();
    assert!(reply.ok && r.h.app().shelf == Shelf::Everything, "{reply:?}");
}

#[test]
fn gallery_latest_answers_with_the_newest_pictures() {
    use serde_json::json;
    let (mut r, _) = Rig::new(30);
    let reply = r.h.intent("gallery.latest", json!({"limit": 3})).unwrap();
    assert!(reply.ok, "{reply:?}");
    let pictures = reply.value["pictures"].as_array().unwrap();
    assert_eq!(pictures.len(), 3);
    assert_eq!(pictures[0]["id"], r.h.app().items[0].id);
    assert!(pictures[0]["taken"].is_string(), "{pictures:?}");
    assert_eq!(reply.entities.iter().filter(|e| e.kind == "picture").count(), 3);
}

// ---- The real engine: the deployed lucina-media on temporary folders, through the real client.

#[test]
fn the_real_servers_photos_reach_the_screen() {
    let Some(engine) = Engine::start("gallery-app") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let mut r = Rig::on(engine.connect(), 1024, 768);
    r.until("six photos", |g| g.items.len() == 6);
    assert!(!r.h.app().has_more());
    assert!(r.h.app().items.iter().all(|i| i.thumbhash.is_some() && i.tile_url().is_some()));
    // Real JPEG thumbnails through the real decoder.
    r.until("six tiles", |g| g.tiles.values().filter(|t| !t.placeholder).count() == 6);
    // A real ThumbHash makes a tile.
    let hash = r.h.app().items[0].thumbhash.clone().unwrap();
    let ph = crate::server::placeholder_tile(&hash, 64).unwrap();
    assert_eq!((ph.w, ph.h), (64, 64));
    // The albums: the import's own and the camera roll.
    let albums: Vec<String> = match &r.h.app().albums {
        Some(Ok(a)) => a.iter().map(|x| x.title.clone()).collect(),
        other => panic!("{:?}", other.as_ref().map(|_| ())),
    };
    assert!(albums.iter().any(|a| a == "Holiday"), "{albums:?}");
    // Months.
    r.key(keys::KEY_G);
    r.until("months", |g| matches!(&g.months, Some(Ok(m)) if !m.is_empty()));
    r.key(keys::KEY_ESC);
    // The viewer: a real 1600 px rendition, the details route, a favourite and a rotation the server keeps.
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_I);
    r.until("details", |g| matches!(&g.viewer.as_ref().unwrap().details, Some((_, Ok(_)))));
    let body = {
        let v = r.h.app().viewer.as_ref().unwrap();
        v.details.as_ref().unwrap().1.as_ref().unwrap().clone()
    };
    let item = r.h.app().items[0].clone();
    let rows = crate::model::details_rows(&body, &item, &Zone::utc(), &[]);
    assert!(rows.iter().any(|(k, _)| k == "Original file"), "{rows:?}");
    r.key(keys::KEY_F);
    r.key(keys::KEY_R);
    let mut m = (engine.connect())().unwrap();
    let id = item.id.clone();
    let v = m.get(&format!("/v1/media/items/{id}")).unwrap();
    assert_eq!((v["favorite"].as_bool(), v["rotation"].as_u64()), (Some(true), Some(90)));
    // The Trash, on the real engine: there, restorable, and the delete dialog's route.
    r.key(keys::KEY_ESC);
    r.h.app().run_batch("trash", vec![id.clone()], None, false);
    r.until("moved to the Trash", |g| g.items.len() == 5);
    r.h.app().set_shelf(Shelf::Trash);
    r.until("the Trash", |g| g.items.len() == 1);
    assert_eq!(r.h.app().items[0].id, id);
    assert!(r.h.app().items[0].trash_expires_at.is_some());
    r.h.app().run_batch("restore", vec![id.clone()], None, false);
    r.until("restored", |g| g.items.is_empty());
    r.h.app().set_shelf(Shelf::Everything);
    r.until("back", |g| g.items.len() == 6);
    // Words: the real index.
    r.h.app().words = "p3".into();
    r.h.app().reload();
    r.until("one found", |g| g.items.len() == 1);
    // A new album, on the real server, gets a photo.
    let add = r.h.app().items[0].id.clone();
    for msg in r.w.handle(Job::CreateAlbum { title: "Made here".into(), add: vec![add.clone()] }) {
        r.h.core.message(msg);
    }
    r.pump();
    assert_eq!(r.h.app().toast, "1 item added to the album.");
    let v = m.get(&format!("/v1/media/items/{add}")).unwrap();
    assert!(v["albums"].as_array().unwrap().len() >= 2);
}

#[test]
fn the_real_servers_download_writes_the_original() {
    let Some(engine) = Engine::start("gallery-dl") else {
        eprintln!("skipped: no lucina-media here");
        return;
    };
    let mut m = (engine.connect())().unwrap();
    let page = m.get("/v1/media/items?scope=gallery&origin=all&kind=photo%2Cvideo&limit=1").unwrap();
    let item = crate::model::parse_item(&page["items"][0]).unwrap();
    let dir = std::env::temp_dir().join(format!("gallery-dl-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join("photo.png");
    crate::server::download(&engine.connect(), &item.download_url(), &dest, &|_| {}).unwrap();
    assert_eq!(std::fs::read(&dest).unwrap().len() as u64, item.size);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_viewer_stops_at_both_ends_jumps_with_home_and_end_and_alt_left_closes() {
    let (mut r, _) = Rig::new(12);
    let n = r.h.app().items.len();
    r.key(keys::KEY_ENTER);
    let at = |r: &mut Rig| r.h.app().viewer.as_ref().map(|v| v.index);
    assert_eq!(at(&mut r), Some(0));
    // Left on the first stays, and the key is taken: the next Right still walks.
    r.key(keys::KEY_LEFT);
    assert_eq!(at(&mut r), Some(0), "no wrap to the last");
    r.key(keys::KEY_RIGHT);
    assert_eq!(at(&mut r), Some(1));
    r.key(keys::KEY_END);
    assert_eq!(at(&mut r), Some(n - 1));
    r.key(keys::KEY_RIGHT);
    assert_eq!(at(&mut r), Some(n - 1), "no wrap to the first");
    r.key(keys::KEY_LEFT);
    assert_eq!(at(&mut r), Some(n - 2));
    r.key(keys::KEY_HOME);
    assert_eq!(at(&mut r), Some(0));
    r.key(keys::KEY_END);
    r.h.press(keys::KEY_LEFT, keys::Mods { alt: true, ..Default::default() });
    assert!(r.h.app().viewer.is_none(), "Alt+Left is back");
    assert_eq!(r.h.app().focus, n - 1, "the grid has the picture we left on");
}

#[test]
fn space_plays_a_video_in_the_viewer() {
    let (mut r, _) = Rig::new(20);
    let at = r.h.app().items.iter().position(|i| i.video).unwrap();
    r.h.app().open_viewer(at);
    r.pump();
    r.key(keys::KEY_SPACE);
    assert_eq!(r.h.app().toast, "The video player is not on this machine.");
}
