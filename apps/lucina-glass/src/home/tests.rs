use super::*;
use crate::canvas::Canvas;
use crate::layouts::fits;

fn screen() -> R {
    R::new(0.0, 0.0, W, H)
}

/// The fixture with nothing that moves forever.
fn still() -> HomeData {
    let mut d = fixture::full(0.0);
    d.build.as_mut().unwrap().live = false;
    d.device.as_mut().unwrap().busy = false;
    d.upload = None;
    d.countdown = None;
    d
}

#[test]
fn every_region_stays_on_the_screen() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    assert!(!h.regions.is_empty());
    for g in &h.regions {
        assert!(fits(g.r, screen()), "{:?} {:?}", g.id, g.r);
    }
}

#[test]
fn a_missing_card_is_left_out_and_the_rest_move_up() {
    let mut h = Home::new(0.0);
    let mut d = fixture::full(0.0);
    h.set_data(0.0, d.clone());
    let before = h.layout.seg.unwrap();
    d.build = None;
    d.deadline = None;
    d.upload = None;
    h.set_data(1.0, d);
    assert!(h.layout.build.is_none() && h.layout.deadline.is_none() && h.layout.upload.is_none());
    assert!(
        h.layout.seg.unwrap().y < before.y,
        "the segments moved up into the place Build had"
    );
    assert!(
        h.regions
            .iter()
            .all(|g| g.id != Rg::Build && g.id != Rg::Deadline)
    );
}

#[test]
fn nothing_at_all_draws_no_cards() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, HomeData::default());
    assert_eq!(h.layout.dock, None);
    assert_eq!(h.layout.panel, None);
    assert!(h.items.is_empty());
}

#[test]
fn at_rest_it_sleeps() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, still());
    assert_eq!(
        h.damage(0.0).len(),
        1,
        "the first frame is the whole screen"
    );
    // Long after every animation and the ambient budget: no frame is due, nothing is damaged.
    let late = 200.0;
    let _ = h.damage(late);
    assert_eq!(h.next_frame(late, 1.0 / 60.0), None);
    assert!(h.damage(late + 1.0).is_empty());
}

#[test]
fn an_animation_draws_only_its_region_at_the_panel_rate() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, still());
    let _ = h.damage(100.0);
    h.select_day(100.0, 2);
    let next = h.next_frame(100.0, 1.0 / 60.0).unwrap();
    assert!(next <= 100.0 + 1.0 / 60.0 + 1e-6, "{next}");
    let rects = h.damage(100.02);
    assert!(!rects.is_empty());
    let area: f32 = rects.iter().map(|r| r.w * r.h).sum();
    assert!(area < W * H * 0.4, "only the agenda is drawn: {rects:?}");
    // Once it is over, it rests.
    let _ = h.damage(110.0);
    assert!(h.damage(400.0).is_empty());
    assert_eq!(h.next_frame(400.0, 1.0 / 60.0), None);
}

#[test]
fn ambient_motion_stops_after_the_budget() {
    let mut h = Home::new(0.0);
    let mut d = still();
    d.build.as_mut().unwrap().live = true;
    h.set_data(0.0, d);
    let _ = h.damage(1.0);
    let n = h.next_frame(5.0, 1.0 / 60.0).unwrap();
    assert!(
        n >= 1.0 + AMBIENT_FRAME - 1e-3,
        "ambient runs at its own rate: {n}"
    );
    let _ = h.damage(AMBIENT_FOR + 10.0);
    assert_eq!(h.next_frame(AMBIENT_FOR + 10.0, 1.0 / 60.0), None);
    h.poke(AMBIENT_FOR + 20.0);
    assert!(h.next_frame(AMBIENT_FOR + 20.0, 1.0 / 60.0).is_some());
}

#[test]
fn arrows_walk_the_items_and_never_leave_them() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    let n = h.items.len();
    assert_eq!(h.key(1.0, Key::Down), Outcome::Redraw);
    let keys = [
        Key::Right,
        Key::Right,
        Key::Down,
        Key::Down,
        Key::Right,
        Key::Up,
        Key::Left,
        Key::Down,
        Key::Down,
    ];
    for (i, k) in keys.iter().enumerate() {
        let _ = h.key(2.0 + i as f64, *k);
        assert!(h.focus.unwrap() < n);
    }
}

#[test]
fn enter_on_an_action_opens_its_target() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    let i = h
        .items
        .iter()
        .position(|(it, _)| *it == Item::Action(1))
        .unwrap();
    h.focus = Some(i);
    assert_eq!(
        h.key(1.0, Key::Enter),
        Outcome::Open(Target::App("calendar".into()))
    );
}

#[test]
fn the_segments_switch_the_row_of_actions() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    assert_eq!(h.actions.len(), 4);
    let i = h
        .items
        .iter()
        .position(|(it, _)| *it == Item::Segment(1))
        .unwrap();
    h.focus = Some(i);
    h.key(1.0, Key::Enter);
    assert_eq!(h.actions.len(), 2);
    assert_eq!(h.actions[0].label, "Ask");
}

#[test]
fn clearing_a_notification_reports_it() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    let i = h
        .items
        .iter()
        .position(|(it, _)| *it == Item::Stack)
        .unwrap();
    h.focus = Some(i);
    assert_eq!(h.key(1.0, Key::Delete), Outcome::Dismissed("a".into()));
}

#[test]
fn a_new_notification_enters_and_an_old_one_does_not() {
    let mut h = Home::new(0.0);
    let mut d = fixture::full(0.0);
    h.set_data(0.0, d.clone());
    let _ = h.damage(50.0);
    d.notes.as_mut().unwrap()[0].line = "Edited".into();
    h.set_data(60.0, d.clone());
    assert!(h.stack_rise < 1.0, "an edited card does not enter again");
    d.notes.as_mut().unwrap().insert(
        0,
        Note {
            id: "c".into(),
            fact: "New".into(),
            line: "Now".into(),
            urgent: false,
            app: None,
            at: None,
        },
    );
    h.set_data(70.0, d);
    assert_eq!(h.stack_rise, 70.0);
}

#[test]
fn it_draws_something_everywhere_it_should() {
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    let mut cv = Canvas::new(1920, 1080, K);
    let bg = Canvas::new(1920, 1080, K);
    let mut ty = Type::new();
    h.draw(&mut cv, &mut ty, 5.0, &bg, &bg);
    assert!(cv.px.iter().filter(|&&p| p != 0).count() > 100_000);
}

#[test]
fn the_slide_lands_exactly() {
    let (w, hh) = (64usize, 4usize);
    let back = vec![0x00ff_ffffu32; w * hh];
    let front = vec![0x0012_3456u32; w * hh];
    let mut out = vec![0u32; w * hh];
    slide_compose(&mut out, &back, &front, w, hh, 1.0, 1);
    assert!(out.iter().all(|&p| p == 0x0012_3456));
    slide_compose(&mut out, &back, &front, w, hh, 0.0, -1);
    assert!(out.iter().all(|&p| p == 0x00ff_ffff));
}

#[test]
fn the_summary_count_lands_when_the_week_arrives_twice() {
    let mut h = Home::new(0.0);
    let mut d = still();
    h.set_data(
        0.0,
        HomeData {
            week: None,
            ..d.clone()
        },
    );
    let _ = h.damage(0.0);
    // The cache's week, then the server's same week a little later, as the feed sends them.
    h.set_data(0.3, d.clone());
    d.deadline.as_mut().unwrap().line = "Today 15:30".into();
    h.set_data(0.7, d.clone());
    let mut t = 0.3;
    while t < 30.0 {
        match h.next_frame(t, 1.0 / 60.0) {
            Some(n) if n <= 30.0 => {
                t = n.max(t);
                let _ = h.damage(t);
                t += 1.0 / 60.0;
            }
            _ => break,
        }
    }
    let shown = h.grid.tiles.iter().find_map(|x| match &x.kind {
        TileKind::Summary { percent, .. } => {
            Some((percent.value(t), percent.value(1000.0), percent.target))
        }
        _ => None,
    });
    let (at_rest, later, target) = shown.unwrap();
    assert_eq!(
        at_rest, target,
        "drawn at rest: {at_rest} of {target} (later {later}), last frame at {t}"
    );
}

#[test]
fn a_notification_says_its_age() {
    assert_eq!(age_line(20_000, "23:25"), "Just now");
    assert_eq!(age_line(5 * 60_000 + 10, "23:25"), "5 min ago");
    assert_eq!(age_line(90 * 60_000, "23:25"), "1 h ago");
    assert_eq!(age_line(13 * 3_600_000, "23:25"), "23:25");
}

#[test]
fn text_is_cut_to_fit_its_card() {
    let mut cv = Canvas::new(400, 100, 1.0);
    let mut ty = Type::new();
    let mut p = crate::Paint {
        cv: &mut cv,
        ty: &mut ty,
        now: 0.0,
        backdrop: None,
    };
    let long = "Juno reset itself without a crash report during startup";
    for max in [40.0f32, 120.0, 250.0] {
        let s = p.fit(Font::Regular, ty::HEADLINE, long, max);
        assert!(
            p.measure(Font::Regular, ty::HEADLINE, &s) <= max,
            "{s} wider than {max}"
        );
        assert!(s.ends_with('…'));
    }
    assert_eq!(p.fit(Font::Regular, ty::HEADLINE, "Short", 250.0), "Short");
}

#[test]
fn with_no_build_the_stat_cards_take_the_room() {
    let mut h = Home::new(0.0);
    let mut d = fixture::full(0.0);
    d.build = None;
    h.set_data(0.0, d);
    let (actions, stats) = (h.layout.actions.unwrap(), h.layout.stats_left.unwrap());
    assert!(
        stats.y - actions.bottom() < 40.0,
        "no hole under the actions: {actions:?} {stats:?}"
    );
    assert!(fits(stats, screen()));
}

#[test]
fn only_the_volume_makes_a_slim_strip() {
    let mut h = Home::new(0.0);
    let mut d = fixture::full(0.0);
    d.upload = None;
    d.countdown = None;
    d.media = None;
    h.set_data(0.0, d);
    let dock = h.layout.dock.unwrap();
    let panel = h.layout.panel.unwrap();
    assert!(dock.w < W / 3.0 && dock.h <= 40.0, "slim: {dock:?}");
    assert!(panel.bottom() <= dock.y, "the right column ends above it");
    assert!(
        h.layout.grid.unwrap().bottom() > 640.0,
        "the middle uses the height"
    );
}

#[test]
fn a_tall_stat_card_lists_its_tasks_and_opens_one() {
    let mut h = Home::new(0.0);
    let mut d = fixture::full(0.0);
    d.build = None;
    h.set_data(0.0, d);
    let lines: Vec<usize> = h
        .items
        .iter()
        .enumerate()
        .filter(|(_, (it, _))| matches!(it, Item::StatLine(0, _)))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(lines.len(), 3, "three lines on the tall card");
    h.focus = Some(lines[1]);
    assert_eq!(
        h.key(1.0, Key::Enter),
        Outcome::Open(Target::Task("t1".into()))
    );
    // With Build back, the card is short again and lists nothing.
    let mut h = Home::new(0.0);
    h.set_data(0.0, fixture::full(0.0));
    assert!(
        !h.items
            .iter()
            .any(|(it, _)| matches!(it, Item::StatLine(..)))
    );
}
