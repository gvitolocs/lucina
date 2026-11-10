//! The app on the sample log at a fixed clock, by keyboard and mouse, through the toolkit's harness.

use lucina_app::time::Zone;
use lucina_app::{Harness, keys};
use lucina_evlog::store::Meta;

use crate::app::{ReliabilityApp, View};
use crate::details::DLine;

/// 2026-11-07 18:00 UTC.
const NOW: i64 = 1_794_009_600_000 + 18 * 3_600_000;

fn start(events: Vec<lucina_evlog::Event>) -> Harness<ReliabilityApp> {
    let mut a = ReliabilityApp::new(Zone::utc(), "/data/lucina/events");
    a.fixed_now = Some(NOW);
    a.load(
        events,
        Meta {
            dropped_routine: 12,
            ..Meta::default()
        },
    );
    let mut h = Harness::new(a, 1920, 1080);
    settle(&mut h);
    h
}

fn settle(h: &mut Harness<ReliabilityApp>) {
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
}

fn sample() -> Harness<ReliabilityApp> {
    start(lucina_evlog::sample::events(NOW))
}

#[test]
fn the_timeline_shows_today_with_its_index() {
    let mut h = sample();
    let (big, status) = h.app().headline();
    assert!(big.ends_with(" / 10"), "{big}");
    let value: f64 = big.split(' ').next().unwrap().parse().expect("a number");
    assert!((1.0..=10.0).contains(&value), "{value}");
    // The morning's four resets and the panic: problems today, and one earlier in the week.
    assert!(status.contains("problems today"), "{status}");
    assert!(status.contains("in the 7 days before"), "{status}");
    assert_eq!(*h.app().view_now(), View::Timeline);
    let today = h.app().selected_day().expect("a day").clone();
    assert_eq!(today.date, "2026-11-07");
    assert!(today.juno_failures >= 4, "{today:?}");
    assert!(!h.app().rows.is_empty());
}

#[test]
fn left_goes_to_yesterday_and_right_back() {
    let mut h = sample();
    h.key(keys::KEY_LEFT);
    assert_eq!(h.app().selected_day().unwrap().date, "2026-11-06");
    h.key(keys::KEY_LEFT);
    assert_eq!(h.app().selected_day().unwrap().date, "2026-11-05");
    h.key(keys::KEY_RIGHT);
    h.key(keys::KEY_RIGHT);
    h.key(keys::KEY_RIGHT);
    assert_eq!(
        h.app().selected_day().unwrap().date,
        "2026-11-07",
        "it stops at today"
    );
}

#[test]
fn a_click_on_a_column_selects_that_day() {
    let mut h = sample();
    // The date row of the column in the middle of the chart.
    let before = h.app().back;
    h.click(1920 / 2, 300);
    settle(&mut h);
    assert_ne!(h.app().back, before, "the click landed on another day");
}

#[test]
fn enter_opens_the_first_event_and_esc_comes_back() {
    let mut h = sample();
    let first = h.app().rows[0].record;
    h.key(keys::KEY_ENTER);
    assert_eq!(*h.app().view_now(), View::Details(first));
    assert_eq!(h.app().detail.record, first);
    h.key(keys::KEY_ESC);
    assert_eq!(*h.app().view_now(), View::Timeline);
}

#[test]
fn enter_after_changing_the_day_opens_an_event_not_the_formula() {
    let mut h = sample();
    h.key(keys::KEY_LEFT);
    h.key(keys::KEY_RIGHT);
    h.key(keys::KEY_ENTER);
    assert!(
        matches!(h.app().view_now(), View::Details(_)),
        "{:?}",
        h.app().view_now()
    );
    assert!(!h.app().formula);
}

#[test]
fn a_today_without_events_opens_on_the_latest_day_with_some() {
    // The sample's events are on NOW's day; a day later, today is empty.
    let mut a = ReliabilityApp::new(Zone::utc(), "/data/lucina/events");
    a.fixed_now = Some(NOW + 86_400_000);
    a.load(lucina_evlog::sample::events(NOW), Meta::default());
    let mut h = Harness::new(a, 1920, 1080);
    settle(&mut h);
    assert_eq!(h.app().back, 1);
    h.key(keys::KEY_ENTER);
    assert!(
        matches!(h.app().view_now(), View::Details(_)),
        "{:?}",
        h.app().view_now()
    );
}

#[test]
fn an_evidence_item_with_a_line_highlights_it_in_the_kernel_log() {
    let mut h = sample();
    let rec = h.app().newest_critical_today().expect("a reset today");
    h.app().open_details(rec);
    settle(&mut h);
    assert_eq!(h.app().detail.hl, None);
    let row = h
        .app()
        .first_linked_row()
        .expect("the startup reset points at a log line");
    let want = h.app().detail.lines[row].log_line().unwrap();
    // Down walks the selectable rows (evidence first) until it lands on that one.
    let mut guard = 0;
    while h.app().detail.list.selected != Some(row) {
        h.key(keys::KEY_DOWN);
        guard += 1;
        assert!(guard < 30, "never reached the evidence item");
    }
    assert_eq!(h.app().detail.hl, Some(want));
    assert!(h.app().detail.log.selected == Some(want));
    // Esc back to the list.
    h.key(keys::KEY_ESC);
    assert_eq!(*h.app().view_now(), View::Timeline);
}

#[test]
fn tab_walks_the_evidence_that_has_lines() {
    let mut h = sample();
    let rec = h.app().newest_critical_today().unwrap();
    h.app().open_details(rec);
    settle(&mut h);
    h.key(keys::KEY_TAB);
    let first = h.app().detail.list.selected.expect("tab selects");
    assert!(h.app().detail.lines[first].log_line().is_some());
    let want = h.app().detail.lines[first].log_line();
    assert_eq!(h.app().detail.hl, want);
}

#[test]
fn t_shows_the_technical_details() {
    let mut h = sample();
    h.key(keys::KEY_ENTER);
    let mono = |h: &mut Harness<ReliabilityApp>| {
        h.app()
            .detail
            .lines
            .iter()
            .filter(|l| matches!(l, DLine::Mono(_)))
            .count()
    };
    let closed = mono(&mut h);
    h.key(keys::KEY_T);
    assert!(h.app().detail.tech);
    let open = mono(&mut h);
    assert!(open > closed + 4, "{open} vs {closed}");
    assert!(
        h.app()
            .detail
            .lines
            .iter()
            .any(|l| matches!(l, DLine::Mono(s) if s.starts_with("provider")))
    );
    h.key(keys::KEY_T);
    assert!(!h.app().detail.tech);
}

#[test]
fn problems_lists_groups_and_enter_goes_down_to_the_events() {
    let mut h = sample();
    h.key(keys::KEY_2);
    assert_eq!(*h.app().view_now(), View::Problems);
    let data = h.app().data.clone().unwrap();
    assert!(
        data.groups.iter().any(|g| g.count >= 2),
        "the venus resets repeat"
    );
    let at = data.groups.iter().position(|g| g.count >= 2).unwrap();
    for _ in 0..at {
        h.key(keys::KEY_DOWN);
    }
    h.key(keys::KEY_ENTER);
    let View::Group(id) = h.app().view_now().clone() else {
        panic!("a group")
    };
    assert!(h.app().rows.len() >= 2);
    assert!(
        h.app()
            .rows
            .iter()
            .all(|r| data.event(r.record).unwrap().bucket.as_ref().unwrap().id == id)
    );
    h.key(keys::KEY_ENTER);
    assert!(matches!(h.app().view_now(), View::Details(_)));
    h.key(keys::KEY_ESC);
    assert!(matches!(h.app().view_now(), View::Group(_)));
    h.key(keys::KEY_ESC);
    assert_eq!(*h.app().view_now(), View::Problems);
    h.key(keys::KEY_ESC);
    assert_eq!(
        *h.app().view_now(),
        View::Problems,
        "Esc on a root page stays"
    );
    h.key(keys::KEY_1);
    assert_eq!(*h.app().view_now(), View::Timeline);
}

#[test]
fn the_same_problem_row_lists_its_events() {
    let mut h = sample();
    let rec = h.app().newest_critical_today().unwrap();
    h.app().open_details(rec);
    settle(&mut h);
    let same = h
        .app()
        .detail
        .lines
        .iter()
        .position(|l| matches!(l, DLine::Same { .. }))
        .expect("it has a bucket");
    let DLine::Same { text, .. } = h.app().detail.lines[same].clone() else {
        unreachable!()
    };
    assert!(text.starts_with("seen "), "{text}");
    h.app().select_row(same);
    h.key(keys::KEY_ENTER);
    assert!(matches!(h.app().view_now(), View::Group(_)));
}

#[test]
fn question_mark_toggles_the_formula() {
    let mut h = sample();
    h.press(
        keys::KEY_SLASH,
        keys::Mods {
            shift: true,
            ..keys::Mods::default()
        },
    );
    assert!(h.app().formula);
    h.key(keys::KEY_ESC);
    assert!(!h.app().formula);
}

#[test]
fn an_empty_store_says_so_and_does_not_panic() {
    let mut h = start(Vec::new());
    assert_eq!(h.app().empty_notice(), Some("No events yet"));
    let (big, _) = h.app().headline();
    assert_eq!(big, "— / 10");
    h.key(keys::KEY_ENTER);
    h.key(keys::KEY_LEFT);
    h.key(keys::KEY_2);
    h.key(keys::KEY_ENTER);
    h.key(keys::KEY_1);
    h.press(
        keys::KEY_SLASH,
        keys::Mods {
            shift: true,
            ..keys::Mods::default()
        },
    );
    settle(&mut h);
}

#[test]
fn events_without_a_time_come_last_today() {
    let mut ev = lucina_evlog::sample::events(NOW);
    let mut u = lucina_evlog::Event::new(
        "Juno-Net",
        4201,
        lucina_evlog::Level::Warning,
        lucina_evlog::Channel::System,
        "Link stopped, time not known.",
    );
    u.record = 999;
    ev.push(u);
    let mut h = start(ev);
    let last = h.app().rows.last().unwrap().clone();
    assert_eq!(last.record, 999);
    assert_eq!(last.head, Some("Time unknown"));
}
