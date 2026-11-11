//! The app against the engine's own expansion through the fake server, with jobs run when the test
//! says, so that it can look at the screen before the server has answered.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::{NaiveDate, TimeZone};
use chrono_tz::Tz;
use lucina_app::Harness;
use lucina_app::keys;
use secretary_calendar::Source;

use crate::app::{Calendar, Kind, Mode};
use crate::sample;
use crate::server::{Draft, Server, ServerError};
use crate::store::{self, Form, FormError, Store};
use crate::worker::{self, Job, Worker};

const TZ: Tz = chrono_tz::Europe::Copenhagen;

/// A Wednesday.
fn now() -> i64 {
    TZ.with_ymd_and_hms(2026, 11, 4, 12, 0, 0)
        .unwrap()
        .timestamp_millis()
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

struct Rig {
    h: Harness<Calendar>,
    jobs: Rc<RefCell<VecDeque<Job>>>,
    w: Worker,
    offline: Arc<AtomicBool>,
    writes: Arc<AtomicU64>,
}

impl Rig {
    fn new() -> Rig {
        let fake = sample::server(TZ, day(2026, 11, 4));
        let offline = fake.offline.clone();
        let writes = fake.writes.clone();
        let jobs: Rc<RefCell<VecDeque<Job>>> = Rc::default();
        let j = jobs.clone();
        let mut st = Store::new(TZ);
        st.now = now;
        let app = Calendar::new(
            st,
            Mode::Sample,
            Some(Box::new(move |job| j.borrow_mut().push_back(job))),
        );
        let mut r = Rig {
            h: Harness::new(app, 1920, 1080),
            jobs,
            w: Worker::new(Box::new(fake), TZ),
            offline,
            writes,
        };
        r.pump();
        r
    }

    fn pump(&mut self) {
        loop {
            let job = self.jobs.borrow_mut().pop_front();
            let Some(job) = job else { break };
            for m in self.w.handle(job) {
                self.h.core.message(m);
            }
        }
        self.h.core.paint();
    }

    fn titles_on(&mut self, d: NaiveDate) -> Vec<String> {
        let a = self.h.app();
        a.store
            .on_day(d)
            .iter()
            .map(|&i| a.store.events[i].title.clone())
            .collect()
    }

    fn id_of(&mut self, title: &str) -> (String, i64) {
        let e = self
            .h
            .app()
            .store
            .events
            .iter()
            .find(|e| e.title == title)
            .unwrap();
        (e.id.clone(), e.start_ms)
    }

    fn fill(&mut self, title: &str, date: &str, from: &str, to: &str) {
        let ed = self.h.app().editor.as_mut().expect("the editor is open");
        ed.title.set(title);
        ed.start_date.set(date);
        ed.end_date.set(date);
        ed.start_time.set(from);
        ed.end_time.set(to);
    }
}

#[test]
fn the_week_shows_the_engines_expansion_of_recurring_events() {
    let mut r = Rig::new();
    let monday = r.titles_on(day(2026, 11, 2));
    assert!(monday.contains(&"Team stand-up".to_string()), "{monday:?}");
    let wed = r.titles_on(day(2026, 11, 4));
    assert!(wed.contains(&"Team stand-up".to_string()));
    assert!(wed.contains(&"Design review".to_string()));
    // A later week, loaded from the months around it.
    r.h.app().go(day(2026, 11, 25));
    r.pump();
    assert!(
        r.titles_on(day(2026, 11, 23))
            .contains(&"Team stand-up".to_string())
    );
    // All day, end exclusive: Saturday and Sunday and not Monday.
    let trip = "Weekend in the country".to_string();
    assert!(r.titles_on(day(2026, 11, 7)).contains(&trip));
    assert!(r.titles_on(day(2026, 11, 8)).contains(&trip));
    assert!(!r.titles_on(day(2026, 11, 9)).contains(&trip));
}

#[test]
fn iphone_events_never_offer_edits_and_nothing_can_reach_the_server() {
    let mut r = Rig::new();
    let (id, start) = r.id_of("Design review");
    assert!(id.starts_with("iphone:"));
    let i =
        r.h.app()
            .store
            .events
            .iter()
            .position(|e| e.id == id && e.start_ms == start)
            .unwrap();
    assert!(!r.h.app().store.events[i].editable);
    r.h.app().open_event(i);
    r.h.core.paint();
    assert!(r.h.app().detail.is_some());
    // Edit and Delete refuse, whatever calls them.
    r.h.app().edit_event(&id, start);
    assert!(r.h.app().editor.is_none());
    r.h.app().delete_event(&id, start);
    r.pump();
    assert_eq!(r.writes.load(Ordering::SeqCst), 0);
    assert!(r.h.app().event(&id, start).is_some());
    // The sheet stays until Esc.
    r.h.key(keys::KEY_ESC);
    assert!(r.h.app().detail.is_none());
}

#[test]
fn the_fake_server_refuses_iphone_writes_as_the_real_one_does() {
    let mut f = sample::server(TZ, day(2026, 11, 4));
    let d = Draft {
        title: "x".into(),
        start: "2026-11-04".into(),
        ..Draft::default()
    };
    let Err(ServerError::Refused { status, code, .. }) = f.update("iphone:a3", &d) else {
        panic!("not refused");
    };
    assert_eq!((status, code.as_str()), (422, "read_only_source"));
    assert!(matches!(
        f.delete("iphone:a3"),
        Err(ServerError::Refused { status: 422, .. })
    ));
}

#[test]
fn a_new_event_shows_at_once_goes_to_lucinas_calendar_and_leaves_the_iphones_alone() {
    let mut r = Rig::new();
    let iphone_before: Vec<String> =
        r.h.app()
            .store
            .events
            .iter()
            .filter(|e| e.source == Source::Iphone)
            .map(|e| e.id.clone())
            .collect();
    r.h.type_str("n");
    assert!(r.h.app().editor.is_some());
    r.fill("Coffee with Sam", "2026-11-05", "11:00", "11:30");
    r.h.app().save_editor();
    // Before any answer: closed, on the screen, and not sent.
    assert!(r.h.app().editor.is_none());
    assert_eq!(r.writes.load(Ordering::SeqCst), 0);
    assert!(
        r.titles_on(day(2026, 11, 5))
            .contains(&"Coffee with Sam".to_string())
    );
    r.pump();
    assert_eq!(r.writes.load(Ordering::SeqCst), 1);
    let e =
        r.h.app()
            .store
            .events
            .iter()
            .find(|e| e.title == "Coffee with Sam")
            .unwrap()
            .clone();
    assert!(e.id.starts_with("lucina:"), "{}", e.id);
    assert!(e.editable);
    assert_eq!(e.source, Source::Lucina);
    assert_eq!(
        store::local(e.start_ms, TZ).format("%H:%M").to_string(),
        "11:00"
    );
    // Exactly one copy after the reload.
    let n = r
        .titles_on(day(2026, 11, 5))
        .iter()
        .filter(|t| *t == "Coffee with Sam")
        .count();
    assert_eq!(n, 1);
    let iphone_after: Vec<String> =
        r.h.app()
            .store
            .events
            .iter()
            .filter(|e| e.source == Source::Iphone)
            .map(|e| e.id.clone())
            .collect();
    assert_eq!(iphone_before, iphone_after);
}

#[test]
fn creating_what_the_iphone_already_has_asks_first() {
    let mut r = Rig::new();
    r.h.type_str("n");
    r.fill("design review", "2026-11-04", "14:00", "15:30");
    r.h.app().save_editor();
    let err =
        r.h.app()
            .editor
            .as_ref()
            .expect("still open")
            .error
            .clone()
            .unwrap();
    assert!(err.contains("already in"), "{err}");
    assert_eq!(r.writes.load(Ordering::SeqCst), 0);
    r.h.app().save_editor();
    r.pump();
    assert_eq!(
        r.writes.load(Ordering::SeqCst),
        1,
        "a second Save means yes"
    );
}

#[test]
fn a_failed_create_is_never_sent_again_and_comes_back_for_the_owner() {
    let mut r = Rig::new();
    r.offline.store(true, Ordering::SeqCst);
    r.h.type_str("n");
    r.fill("Lost in the post", "2026-11-05", "11:00", "12:00");
    r.h.app().save_editor();
    assert!(
        r.titles_on(day(2026, 11, 5))
            .contains(&"Lost in the post".to_string())
    );
    r.pump();
    assert!(
        !r.titles_on(day(2026, 11, 5))
            .contains(&"Lost in the post".to_string())
    );
    let ed = r.h.app().editor.as_ref().expect("the dialog is back");
    assert_eq!(ed.title.text, "Lost in the post");
    assert!(ed.error.as_ref().unwrap().contains("Not saved"));
    assert!(r.h.app().store.changes.is_empty());
    // Back online: nothing goes by itself.
    r.offline.store(false, Ordering::SeqCst);
    r.jobs.borrow_mut().push_back(Job::Tick);
    r.pump();
    assert_eq!(r.writes.load(Ordering::SeqCst), 0);
}

#[test]
fn own_events_can_be_edited_and_deleted_and_show_it_at_once() {
    let mut r = Rig::new();
    let (id, start) = r.id_of("Focus: write the report");
    r.h.app().edit_event(&id, start);
    r.h.app()
        .editor
        .as_mut()
        .unwrap()
        .title
        .set("Focus: finish the report");
    r.h.app().save_editor();
    assert!(
        r.titles_on(day(2026, 11, 3))
            .contains(&"Focus: finish the report".to_string())
    );
    assert!(
        !r.titles_on(day(2026, 11, 3))
            .contains(&"Focus: write the report".to_string())
    );
    r.pump();
    assert!(
        r.titles_on(day(2026, 11, 3))
            .contains(&"Focus: finish the report".to_string())
    );
    let (id, start) = r.id_of("Focus: finish the report");
    r.h.app().delete_event(&id, start);
    assert!(
        !r.titles_on(day(2026, 11, 3))
            .contains(&"Focus: finish the report".to_string())
    );
    r.pump();
    assert!(
        !r.titles_on(day(2026, 11, 3))
            .contains(&"Focus: finish the report".to_string())
    );
    assert_eq!(r.writes.load(Ordering::SeqCst), 2);
}

#[test]
fn the_keys_move_through_views_and_days() {
    let mut r = Rig::new();
    assert_eq!(r.h.app().kind, Kind::Week);
    r.h.type_str("m");
    assert_eq!(r.h.app().kind, Kind::Month);
    r.h.core.paint();
    r.h.key(keys::KEY_PAGEDOWN);
    assert_eq!(r.h.app().cursor, day(2026, 12, 4));
    r.h.key(keys::KEY_DOWN);
    assert_eq!(r.h.app().cursor, day(2026, 12, 11));
    r.h.type_str("d");
    r.h.key(keys::KEY_RIGHT);
    assert_eq!(r.h.app().cursor, day(2026, 12, 12));
    r.h.type_str("t");
    assert_eq!(r.h.app().cursor, day(2026, 11, 4));
    r.pump();
    // e marks the day's events, Enter opens.
    r.h.type_str("e");
    assert!(r.h.app().marked.is_some());
    r.h.key(keys::KEY_ENTER);
    assert!(r.h.app().detail.is_some());
}

#[test]
fn a_month_end_page_stays_in_the_next_month() {
    let mut r = Rig::new();
    r.h.app().kind = Kind::Month;
    r.h.app().go(day(2027, 1, 31));
    r.h.app().page(1);
    assert_eq!(r.h.app().cursor, day(2027, 2, 28));
    r.pump();
}

#[test]
fn hiding_a_calendar_hides_its_events() {
    let mut r = Rig::new();
    assert!(
        r.titles_on(day(2026, 11, 4))
            .contains(&"Design review".to_string())
    );
    r.h.app().store.hidden.insert("Work".into());
    r.h.app().store.rebuild();
    assert!(
        !r.titles_on(day(2026, 11, 4))
            .contains(&"Design review".to_string())
    );
    let names: Vec<String> =
        r.h.app()
            .store
            .calendars
            .iter()
            .map(|c| c.0.clone())
            .collect();
    assert!(
        names.contains(&"Work".to_string()),
        "still listed: {names:?}"
    );
    assert_eq!(names[0], "Lucina");
}

#[test]
fn walking_weeks_keeps_loads_on_the_newest_only() {
    let mut q: VecDeque<Job> = VecDeque::new();
    q.push_back(Job::Prefetch(vec![day(2026, 10, 1)]));
    q.push_back(Job::Visible(vec![day(2026, 11, 1)]));
    q.push_back(Job::Visible(vec![day(2026, 12, 1)]));
    let Some(Job::Visible(m)) = worker::next_job(&mut q) else {
        panic!("visible first");
    };
    assert_eq!(m, vec![day(2026, 12, 1)]);
}

#[test]
fn a_write_is_never_dropped_for_a_load() {
    let mut q: VecDeque<Job> = VecDeque::new();
    q.push_back(Job::Delete(1, "lucina:a".into()));
    q.push_back(Job::Visible(vec![day(2026, 11, 1)]));
    q.push_back(Job::Visible(vec![day(2026, 12, 1)]));
    assert!(matches!(worker::next_job(&mut q), Some(Job::Delete(1, _))));
    assert!(matches!(worker::next_job(&mut q), Some(Job::Visible(_))));
    assert!(worker::next_job(&mut q).is_none());
}

// ---- lib/calendar.ts, ported: the cases of web/src/lib/calendar.test.ts's form tests ----

fn form() -> Form {
    Form {
        title: "T".into(),
        start_date: "2026-11-04".into(),
        start_time: "09:00".into(),
        end_date: "2026-11-04".into(),
        end_time: "10:00".into(),
        ..Form::default()
    }
}

#[test]
fn the_draft_is_utc_instants_or_dates_with_the_exclusive_end() {
    let d = store::draft_from_form(&form(), TZ).unwrap();
    assert_eq!(d.start, "2026-11-04T08:00:00.000Z");
    assert_eq!(d.end.as_deref(), Some("2026-11-04T09:00:00.000Z"));
    assert!(!d.all_day);
    let mut f = form();
    f.all_day = true;
    let d = store::draft_from_form(&f, TZ).unwrap();
    assert_eq!(d.start, "2026-11-04");
    assert_eq!(d.end.as_deref(), Some("2026-11-05"));
}

#[test]
fn the_form_refuses_what_the_web_form_refuses() {
    let mut f = form();
    f.title = "  ".into();
    assert_eq!(store::draft_from_form(&f, TZ), Err(FormError::NeedTitle));
    let mut f = form();
    f.end_time = "09:00".into();
    assert_eq!(
        store::draft_from_form(&f, TZ),
        Err(FormError::NeedEndAfterStart)
    );
    let mut f = form();
    f.start_date = "not a date".into();
    assert_eq!(
        store::draft_from_form(&f, TZ),
        Err(FormError::NeedEndAfterStart)
    );
}

#[test]
fn moving_the_start_keeps_the_length() {
    let mut next = form();
    next.start_time = "11:30".into();
    let moved = store::shift_end(&form(), &next, TZ);
    assert_eq!(moved.end_time, "12:30");
    // Over midnight.
    let mut next = form();
    next.start_time = "23:30".into();
    let moved = store::shift_end(&form(), &next, TZ);
    assert_eq!(
        (moved.end_date.as_str(), moved.end_time.as_str()),
        ("2026-11-05", "00:30")
    );
}

#[test]
fn the_time_zone_change_is_the_engines() {
    // 2026-10-25 02:30 in Copenhagen happens twice; a 01:30-03:30 event is three hours long.
    let f = Form {
        title: "T".into(),
        start_date: "2026-10-25".into(),
        start_time: "01:30".into(),
        end_date: "2026-10-25".into(),
        end_time: "03:30".into(),
        ..Form::default()
    };
    let d = store::draft_from_form(&f, TZ).unwrap();
    assert_eq!(d.start, "2026-10-24T23:30:00.000Z");
    assert_eq!(d.end.as_deref(), Some("2026-10-25T02:30:00.000Z"));
}

#[test]
fn labels_read_as_the_web_apps_do() {
    let f = sample::server(TZ, day(2026, 11, 4));
    let mut s = f;
    let (a, b) = store::month_range(day(2026, 11, 1), TZ);
    let events = s.events(a, b).unwrap();
    let review = events.iter().find(|e| e.title == "Design review").unwrap();
    assert_eq!(store::when_label(review, TZ), "Wed 4 Nov · 14:00–15:30");
    let trip = events
        .iter()
        .find(|e| e.title == "Weekend in the country")
        .unwrap();
    assert_eq!(store::when_label(trip, TZ), "Sat 7 Nov – Sun 8 Nov");
}

#[test]
fn notes_lose_their_html() {
    assert_eq!(store::plain_notes("a<br/>b &amp; c <b>d</b>"), "a\nb & c d");
    assert_eq!(store::plain_notes("<p>x</p><p>y</p>"), "x\ny");
}

#[test]
fn overlapping_events_share_the_width_and_others_do_not() {
    let cols = store::overlap_columns(&[(0, 10), (5, 15), (20, 30)]);
    assert_eq!(cols, vec![(0, 2), (1, 2), (0, 1)]);
    // A long one and two after each other under it.
    let cols = store::overlap_columns(&[(0, 100), (0, 10), (10, 20)]);
    assert_eq!(cols[0], (0, 2));
    assert_eq!(cols[1].0, 1);
    assert_eq!(cols[2].0, 1);
}

#[test]
fn a_click_on_an_hour_makes_a_new_event_there() {
    let mut r = Rig::new();
    r.h.app().kind = Kind::Day;
    r.h.core.paint();
    r.h.core.input(lucina_app::Input::Wheel(0));
    r.h.core.paint();
    // The day's grid, well to the right of everything else.
    r.h.click(1200, 700);
    let ed =
        r.h.app()
            .editor
            .as_ref()
            .expect("a slot click opens the editor");
    assert_eq!(ed.start_date.text, "2026-11-04");
    assert!(ed.start_time.text.ends_with(":00") || ed.start_time.text.ends_with(":30"));
}

#[test]
fn not_signed_in_shows_no_events_and_never_talks_to_a_server() {
    let mut st = Store::new(TZ);
    st.now = now;
    let mut h = Harness::new(Calendar::new(st, Mode::NotSignedIn, None), 1920, 1080);
    assert!(h.app().store.events.is_empty());
    h.key(keys::KEY_ENTER);
    assert_eq!(h.app().mode, Mode::NotSignedIn);
}
