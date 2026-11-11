//! The app against an in-process copy of the routes, whose JSON is checked against `ink-library`'s own types.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use lucina_app::{Harness, keys};
use serde_json::json;

use crate::app::{Library, Link, Style, detail_rows, parse_tags, percent, tags_valid};
use crate::fake::{self, Data, Fake};
use crate::server::{Job, Msg, Progress, Worker, list_path, parse_detail, parse_highlight, parse_row, Filters};

const TZ: chrono_tz::Tz = chrono_tz::Europe::Copenhagen;

struct Rig {
    h: Harness<Library>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
    data: Arc<Mutex<Data>>,
    offline: Arc<AtomicBool>,
}

impl Rig {
    fn new() -> Rig {
        let fake = Fake::new(fake::sample());
        let (data, offline) = (fake.data.clone(), fake.offline.clone());
        let w = Worker::new(Box::new(fake));
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let h = Harness::new(Library::new(TZ, Some(link)), 1920, 1080);
        let mut r = Rig { h, w, rx, data, offline };
        r.pump();
        r
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
        }
        self.h.core.input(lucina_app::Input::Wheel(0));
        self.h.core.paint();
    }

    /// As if the selection had rested.
    fn settle(&mut self) {
        self.h.app().follow_selection();
        self.h.app().ask_if_settled(Instant::now() + Duration::from_secs(5));
        self.pump();
    }

    fn details_asked(&self) -> usize {
        self.data.lock().unwrap().requests.iter().filter(|r| r.starts_with("GET /v1/library/books/")).count()
    }

    fn book(&self, id: &str) -> serde_json::Value {
        self.data.lock().unwrap().books.iter().find(|b| b["id"] == json!(id)).unwrap().clone()
    }
}

#[test]
fn opening_lists_the_books_and_selects_the_first() {
    let mut r = Rig::new();
    assert_eq!(r.h.app().books.len(), 6);
    assert_eq!(r.h.app().selected().unwrap().id, "b1");
    assert_eq!(r.h.app().books[0].highlight_count, 2);
}

#[test]
fn details_wait_for_the_selection_to_rest_and_are_kept() {
    let mut r = Rig::new();
    assert_eq!(r.details_asked(), 0, "not before it has rested");
    // Arrows held down over every book ask for nothing.
    for _ in 0..4 {
        r.h.key(keys::KEY_DOWN);
    }
    r.pump();
    assert_eq!(r.details_asked(), 0);
    r.settle();
    assert_eq!(r.details_asked(), 1);
    assert_eq!(r.h.app().open().unwrap().row.id, "b5");
    // Coming back to it asks again for nothing.
    r.h.key(keys::KEY_UP);
    r.settle();
    assert_eq!(r.details_asked(), 2);
    r.h.key(keys::KEY_DOWN);
    r.settle();
    assert_eq!(r.details_asked(), 2);
}

#[test]
fn a_search_waits_for_the_typing_and_shows_the_passage() {
    let mut r = Rig::new();
    let before = r.data.lock().unwrap().requests.len();
    r.h.app().query.set("quiet machine");
    r.h.app().search_at = Some(Instant::now());
    r.h.core.paint();
    assert_eq!(r.data.lock().unwrap().requests.len(), before, "still waiting");
    r.h.app().reload();
    r.pump();
    assert_eq!(r.h.app().books.len(), 1);
    assert_eq!(r.h.app().books[0].id, "b2");
    assert!(r.h.app().hits[0].snippet.contains("quiet machine"));
}

#[test]
fn an_older_answer_never_replaces_a_newer_one() {
    let mut r = Rig::new();
    r.h.app().set_status_filter(Some("completed"));
    r.pump();
    assert_eq!(r.h.app().books.len(), 1);
    r.h.core.message(Msg::Listed { seq: 0, books: Vec::new(), hits: Vec::new(), capped: false });
    assert_eq!(r.h.app().books.len(), 1, "seq 0 is old");
}

#[test]
fn filters_narrow_and_saying_so_when_nothing_matches() {
    let mut r = Rig::new();
    r.h.app().toggle_kindle();
    r.pump();
    assert_eq!(r.h.app().books.len(), 3);
    r.h.app().set_status_filter(Some("abandoned"));
    r.pump();
    assert!(r.h.app().books.is_empty());
    assert!(r.h.app().selected().is_none());
    r.h.core.paint();
}

#[test]
fn dictionaries_are_left_out() {
    let mut r = Rig::new();
    let mut dictionary = fake::book("d1", "Oxford Dictionary", "OUP", "unknown", None, true);
    dictionary["kind"] = json!("dictionary");
    r.data.lock().unwrap().books.push(dictionary);
    r.h.app().reload();
    r.pump();
    assert!(!r.h.app().books.iter().any(|b| b.id == "d1"));
}

#[test]
fn status_saves_with_the_version_it_was_based_on() {
    let mut r = Rig::new();
    r.settle();
    r.h.app().save_status("completed");
    r.pump();
    assert_eq!(r.book("b1")["status"], "completed");
    assert_eq!(r.h.app().open().unwrap().row.status, "completed");
    assert_eq!(r.h.app().books[0].status, "completed", "and the list follows");
    assert!(!r.h.app().busy);
}

#[test]
fn a_book_changed_elsewhere_is_reloaded_and_says_so() {
    let mut r = Rig::new();
    r.settle();
    r.data.lock().unwrap().books[0]["updatedAt"] = json!("2026-10-30T00:00:00Z");
    r.data.lock().unwrap().books[0]["tags"] = json!(["from elsewhere"]);
    r.h.app().tags.set("mine");
    r.h.app().save_tags();
    r.pump();
    assert!(r.h.app().message.as_deref().unwrap_or_default().contains("changed while you were editing"));
    assert_eq!(r.book("b1")["tags"], json!(["from elsewhere"]), "nothing overwritten");
    assert_eq!(r.h.app().open().unwrap().tags, ["from elsewhere"]);
    assert_eq!(r.h.app().tags.text, "from elsewhere");
}

#[test]
fn tags_are_checked_here_first() {
    let mut r = Rig::new();
    r.settle();
    r.h.app().tags.set(&"x".repeat(33));
    r.h.app().save_tags();
    r.pump();
    assert!(r.h.app().message.as_deref().unwrap_or_default().contains("at most 16 tags"));
    assert_eq!(r.book("b1")["tags"], json!([]));
    r.h.app().tags.set(" reread, reread ,, loved ");
    r.h.app().save_tags();
    r.pump();
    assert_eq!(r.book("b1")["tags"], json!(["reread", "loved"]));
}

#[test]
fn no_connection_keeps_the_tags_typed_and_the_book_as_it_was() {
    let mut r = Rig::new();
    r.settle();
    r.offline.store(true, Ordering::SeqCst);
    r.h.app().tags.set("reread");
    r.h.app().save_tags();
    r.pump();
    assert!(r.h.app().message.is_some());
    assert_eq!(r.h.app().tags.text, "reread");
    assert!(!r.h.app().busy, "and it can be tried again");
    r.offline.store(false, Ordering::SeqCst);
    r.h.app().save_tags();
    r.pump();
    assert_eq!(r.book("b1")["tags"], json!(["reread"]));
    assert!(r.h.app().message.is_none());
}

#[test]
fn a_failed_list_offers_trying_again() {
    let mut r = Rig::new();
    r.offline.store(true, Ordering::SeqCst);
    r.h.app().set_status_filter(Some("reading"));
    r.pump();
    assert!(r.h.app().error.is_some());
    assert!(!r.h.app().loading);
    r.offline.store(false, Ordering::SeqCst);
    r.h.app().reload();
    r.pump();
    assert!(r.h.app().error.is_none());
    assert_eq!(r.h.app().books.len(), 2);
}

#[test]
fn a_failed_detail_does_not_retry_by_itself_and_try_again_does() {
    let mut r = Rig::new();
    r.offline.store(true, Ordering::SeqCst);
    r.settle();
    assert!(r.h.app().detail_error.is_some());
    assert!(r.h.app().open().is_none());
    r.offline.store(false, Ordering::SeqCst);
    let asked = r.details_asked();
    r.settle();
    r.settle();
    assert_eq!(r.details_asked(), asked, "nothing asked until someone does");
    assert!(r.h.app().detail_error.is_some());
    r.h.app().retry_detail();
    r.settle();
    assert!(r.h.app().open().is_some());
    assert!(r.h.app().detail_error.is_none());
}

#[test]
fn a_brief_needs_highlights_and_shows_when_made() {
    let mut r = Rig::new();
    r.settle();
    r.h.app().make_brief();
    r.pump();
    let d = r.h.app().open().unwrap();
    assert_eq!(d.briefs.len(), 1);
    assert!(detail_rows(d, TZ).iter().any(|(_, s)| s.contains("passages stand out")));
    // b4 has none.
    let writes_before = r.data.lock().unwrap().requests.len();
    r.h.app().list.select(3);
    r.settle();
    r.h.app().make_brief();
    let asked: Vec<String> = r.data.lock().unwrap().requests[writes_before..].to_vec();
    r.pump();
    assert!(!asked.iter().any(|x| x.starts_with("POST")));
}

#[test]
fn activity_counts_and_says_what_a_stock_kindle_cannot() {
    let mut r = Rig::new();
    r.h.app().show_activity();
    r.pump();
    let a = r.h.app().activity.clone().unwrap();
    assert_eq!(a.week, 6);
    assert!(a.no_sessions);
    assert!(a.last_import.is_some());
    assert_eq!(a.by_status.iter().find(|(s, _)| s == "reading").unwrap().1, 2);
}

#[test]
fn a_dead_token_says_signed_out_once() {
    let f = Fake::new(fake::sample());
    f.dead.store(true, Ordering::SeqCst);
    let mut w = Worker::new(Box::new(f));
    let count = |m: Vec<Msg>| m.iter().filter(|x| matches!(x, Msg::SignedOut)).count();
    assert_eq!(count(w.handle(Job::List(1, Filters::default()))), 1);
    assert_eq!(count(w.handle(Job::Detail("b1".into()))), 1);
    assert_eq!(count(w.handle(Job::Brief("b1".into()))), 1);
    assert_eq!(count(w.handle(Job::Activity)), 1);
    assert_eq!(count(w.handle(Job::Patch { id: "b1".into(), patch: json!({}), base: String::new() })), 1);
}

#[test]
fn late_answers_on_nothing_do_nothing() {
    let mut r = Rig::new();
    r.h.core.message(Msg::SaveFailed("late".into()));
    r.h.core.message(Msg::DetailFailed("gone".into(), "late".into()));
    r.h.core.paint();
    assert!(r.h.app().detail_error.is_none());
}

#[test]
fn numbers_and_tags_read_as_on_the_web() {
    let p = |n| Some(Progress { percent: n, location: None, page: None });
    assert_eq!(percent(p(Some(42.5)).as_ref()), "42.5%");
    assert_eq!(percent(p(Some(100.0)).as_ref()), "100%");
    assert_eq!(percent(p(None).as_ref()), "—");
    assert_eq!(percent(None), "—");
    assert_eq!(parse_tags(" a, b ,a,,"), ["a", "b"]);
    assert!(tags_valid(&vec!["x".to_owned(); 16]));
    assert!(!tags_valid(&vec!["x".to_owned(); 17]));
    assert!(tags_valid(&["é".repeat(32)]));
    assert!(!tags_valid(&["é".repeat(33)]));
}

#[test]
fn queries_are_encoded() {
    let f = Filters { q: "a&b c".into(), status: Some("reading"), on_kindle: true };
    assert_eq!(list_path(&f), "/v1/library/books?limit=500&q=a%26b%20c&status=reading&onKindle=true");
}

#[test]
fn kindle_dates_stay_as_supplied() {
    let d = parse_detail(&json!({"book": fake::book("b", "T", "A", "reading", None, false),
        "highlights": [fake::highlight("h", "words", 10)]}));
    let rows = detail_rows(&d, TZ);
    let meta = rows.iter().find(|(s, t)| *s == Style::Dim && t.contains("Location")).unwrap();
    assert!(meta.1.contains("Monday, 5 October 2026 21:14:02"));
}

/// The contract: what the server's own types serialize to reads back here, field by field.
#[test]
fn the_servers_own_types_parse() {
    let b: ink_library::model::Book = serde_json::from_value(json!({
        "id": "bk", "title": "Real", "authors": ["Author"], "kind": "book", "status": "reading",
        "progress": {"percent": 12.5, "location": 40, "page": 3, "source": "kindle", "at": "2026-10-01T00:00:00Z"},
        "onKindle": {"path": "documents/x.mobi", "sizeBytes": 9, "lastSeenAt": "2026-10-01T00:00:00Z"},
        "owned": true, "formats": ["mobi"], "drm": "protected", "isbn": "123", "doi": null, "asin": null,
        "fingerprints": [], "sources": [], "removedFromKindleAt": null, "mentionedOnly": false,
        "statusSource": null, "lastOpenedAt": null, "tags": ["t"], "briefs": [], "relatedNotes": [], "memoryRefs": [],
        "createdAt": "2026-10-01T00:00:00Z", "updatedAt": "2026-10-02T00:00:00Z"
    }))
    .expect("the sample above is what the server stores");
    let wire = serde_json::to_value(&b).unwrap();
    let row = parse_row(&wire);
    assert_eq!(row.id, "bk");
    assert_eq!(row.status, "reading");
    assert!(row.on_kindle);
    assert!(row.protected);
    assert_eq!(row.progress.as_ref().unwrap().percent, Some(12.5));
    let d = crate::server::parse_book(&wire, Vec::new());
    assert_eq!(d.updated_at, "2026-10-02T00:00:00Z");
    assert_eq!(d.tags, ["t"]);
    assert_eq!(d.ids, [("ISBN", "123".to_owned())]);

    let h: ink_library::model::Highlight = serde_json::from_value(json!({
        "id": "h", "bookId": "bk", "kind": "highlight", "text": "words", "note": "mine",
        "location": {"start": 1, "end": 2}, "page": "9", "addedAt": null, "addedAtRaw": "raw date",
        "source": "kindle", "provenance": {"file": "f", "entryIndex": 0, "device": null}, "dedupKey": "k"
    }))
    .expect("the sample above is what the server stores");
    let h = parse_highlight(&serde_json::to_value(&h).unwrap());
    assert_eq!((h.text.as_str(), h.note.as_deref(), h.location), ("words", Some("mine"), Some((1, 2))));
    assert_eq!(h.added.as_deref(), Some("raw date"));
    assert_eq!(h.page.as_deref(), Some("9"));
}
