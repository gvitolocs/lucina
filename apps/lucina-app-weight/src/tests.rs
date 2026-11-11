//! The app against the real `secretary-body` engine, through the same JSON as the wire.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};

use lucina_app::{Harness, keys};
use secretary_body::Engine;
use serde_json::json;

use crate::app::{Link, Weight, format_weight, parse_when};
use crate::server::{Fake, Job, Tag, Worker};

const NOW: u64 = 1_790_000_000_000;
const TZ: chrono_tz::Tz = chrono_tz::Europe::Copenhagen;

struct Rig {
    h: Harness<Weight>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
    engine: Arc<Mutex<Engine>>,
    offline: Arc<std::sync::atomic::AtomicBool>,
    lose: Arc<std::sync::atomic::AtomicBool>,
}

impl Rig {
    fn new() -> Rig {
        let fake = Fake::new(Engine::in_memory(), NOW);
        let (engine, offline, lose) = (fake.engine.clone(), fake.offline.clone(), fake.lose_answers.clone());
        let w = Worker::new(Box::new(fake), "Europe/Copenhagen".into());
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let h = Harness::new(Weight::new(TZ, Some(link)), 1920, 1080);
        let mut r = Rig { h, w, rx, engine, offline, lose };
        r.h.core.paint();
        r.pump();
        r
    }

    /// What the worker thread does, here on this thread.
    fn pump(&mut self) {
        while let Ok(job) = self.rx.try_recv() {
            for m in self.w.handle(job) {
                self.h.core.message(m);
            }
        }
        self.h.core.paint();
    }

    fn entries(&mut self) -> usize {
        self.engine.lock().unwrap().state().entries.len()
    }

    /// Another device changes the state, so this app's revision is old.
    fn elsewhere(&mut self, grams: u64) {
        let mut e = self.engine.lock().unwrap();
        let rev = e.state().revision;
        let r = json!({"op_id": format!("other-{rev}"), "expected_revision": rev, "timezone": "UTC",
            "command": {"type": "log_entry", "weight_grams": grams}});
        assert!(e.handle_at(&r.to_string(), NOW).ok);
    }

    fn log(&mut self, weight: &str) {
        self.h.app().weight.set(weight);
        self.h.app().submit_log();
        self.pump();
    }
}

#[test]
fn typing_a_weight_and_enter_logs_it_with_the_engines_label() {
    let mut r = Rig::new();
    r.h.type_str("80.5");
    r.h.key(keys::KEY_ENTER);
    r.pump();
    assert_eq!(r.entries(), 1);
    let v = r.h.app().view.clone().unwrap();
    assert_eq!(v.entries[0].weight_grams, 80_500);
    assert_eq!(v.labels[&v.entries[0].id], "80.5 kg");
    assert_eq!(r.h.app().weight.text, "", "the field empties once it is saved");
    assert!(!r.h.app().log_pending);
}

#[test]
fn empty_weight_is_refused_here_and_nothing_is_sent() {
    let mut r = Rig::new();
    r.h.app().submit_log();
    r.pump();
    assert_eq!(r.h.app().log_error.as_deref(), Some("Enter your weight."));
    assert_eq!(r.entries(), 0);
}

#[test]
fn the_engine_refuses_nonsense_and_the_text_stays() {
    let mut r = Rig::new();
    r.log("heavy");
    assert_eq!(r.entries(), 0);
    assert!(r.h.app().log_error.is_some());
    assert_eq!(r.h.app().weight.text, "heavy");
    assert!(!r.h.app().log_pending, "the form can be used again");
}

#[test]
fn the_other_unit_is_parsed_by_the_engine_first() {
    let mut r = Rig::new();
    r.h.app().choose_unit("lb");
    r.log("180");
    let v = r.h.app().view.clone().unwrap();
    assert_eq!(v.settings.unit, "kg", "the display unit does not change");
    assert_eq!(v.entries[0].weight_grams, 81_647);
}

#[test]
fn a_conflict_is_fetched_again_and_sent_once() {
    let mut r = Rig::new();
    r.elsewhere(70_000);
    r.log("80");
    assert_eq!(r.entries(), 2, "both weigh-ins, the other device's and this one");
    assert!(r.h.app().log_error.is_none());
    assert_eq!(r.h.app().view.as_ref().unwrap().entries.len(), 2);
}

#[test]
fn no_connection_keeps_what_was_typed() {
    let mut r = Rig::new();
    r.offline.store(true, Ordering::SeqCst);
    r.log("80");
    assert_eq!(r.entries(), 0);
    assert!(r.h.app().log_error.as_deref().unwrap_or_default().contains("Not saved"));
    assert_eq!(r.h.app().weight.text, "80");
    r.offline.store(false, Ordering::SeqCst);
    r.h.app().submit_log();
    r.pump();
    assert_eq!(r.entries(), 1);
    assert_eq!(r.h.app().weight.text, "");
}

#[test]
fn a_lost_answer_sent_again_does_not_log_twice() {
    let mut r = Rig::new();
    r.lose.store(true, Ordering::SeqCst);
    r.log("80");
    assert_eq!(r.entries(), 1, "it did arrive");
    assert!(r.h.app().log_error.is_some(), "this side cannot know that");
    r.lose.store(false, Ordering::SeqCst);
    r.h.app().submit_log();
    r.pump();
    assert_eq!(r.entries(), 1, "the engine answers with its receipt");
    assert!(r.h.app().log_error.is_none());
    assert_eq!(r.h.app().weight.text, "");
}

#[test]
fn edit_sends_only_what_changed() {
    let mut r = Rig::new();
    r.log("80");
    let entry = r.h.app().view.as_ref().unwrap().entries[0].clone();
    r.h.app().open_edit(entry.clone());
    r.h.app().save_edit();
    r.pump();
    assert!(r.h.app().edit.is_none(), "nothing changed: it just closes");
    assert_eq!(r.engine.lock().unwrap().state().entries[0], entry);

    r.h.app().open_edit(entry.clone());
    r.h.app().edit.as_mut().unwrap().note.set("Morning");
    r.h.app().save_edit();
    r.pump();
    assert!(r.h.app().edit.is_none());
    let now = r.engine.lock().unwrap().state().entries[0].clone();
    assert_eq!(now.note, "Morning");
    assert_eq!(now.weight_grams, entry.weight_grams);
    assert_eq!(now.measured_ms, entry.measured_ms);
}

#[test]
fn a_bad_date_stays_in_the_form() {
    let mut r = Rig::new();
    r.log("80");
    let entry = r.h.app().view.as_ref().unwrap().entries[0].clone();
    r.h.app().open_edit(entry);
    r.h.app().edit.as_mut().unwrap().when.set("yesterday");
    r.h.app().save_edit();
    r.pump();
    let e = r.h.app().edit.as_ref().unwrap();
    assert_eq!(e.error.as_deref(), Some("Enter a valid date and time."));
}

#[test]
fn delete_removes_the_entry() {
    let mut r = Rig::new();
    r.log("80");
    let entry = r.h.app().view.as_ref().unwrap().entries[0].clone();
    r.h.app().open_edit(entry);
    r.h.app().delete_entry();
    r.pump();
    assert_eq!(r.entries(), 0);
    assert!(r.h.app().edit.is_none());
}

#[test]
fn settings_write_the_goal_again_in_the_new_unit() {
    let mut r = Rig::new();
    r.log("80");
    r.h.app().open_settings();
    r.h.app().settings.as_mut().unwrap().goal.set("75");
    r.h.app().save_settings();
    r.pump();
    assert_eq!(r.engine.lock().unwrap().state().settings.goal_grams, Some(75_000));

    r.h.app().open_settings();
    r.h.app().settings_unit("lb");
    assert_eq!(r.h.app().settings.as_ref().unwrap().goal.text, format_weight(75_000, "lb"));
    r.h.app().save_settings();
    r.pump();
    let s = r.engine.lock().unwrap().state().settings.clone();
    assert_eq!(s.unit, "lb");
    // One decimal in pounds: the goal moves by less than a tenth of a pound.
    assert!(s.goal_grams.unwrap().abs_diff(75_000) < 50, "{:?}", s.goal_grams);
    assert_eq!(r.h.app().view_unit(), "lb");
}

#[test]
fn an_empty_goal_clears_it() {
    let mut r = Rig::new();
    r.h.app().open_settings();
    r.h.app().settings.as_mut().unwrap().goal.set("75");
    r.h.app().save_settings();
    r.pump();
    r.h.app().open_settings();
    r.h.app().settings.as_mut().unwrap().goal.clear();
    r.h.app().save_settings();
    r.pump();
    assert_eq!(r.engine.lock().unwrap().state().settings.goal_grams, None);
}

#[test]
fn local_dates_round_trip_through_the_zone() {
    let ms = parse_when("2026-11-02 08:30", TZ).unwrap();
    assert_eq!(crate::app::format_when(ms, TZ), "2026-11-02 08:30");
    // 07:30 UTC in winter.
    assert_eq!(ms % 86_400_000, (7 * 3600 + 30 * 60) * 1000);
    assert_eq!(parse_when("2026-13-02 08:30", TZ), None);
}

#[test]
fn weights_show_one_decimal_without_a_trailing_zero() {
    assert_eq!(format_weight(80_000, "kg"), "80");
    assert_eq!(format_weight(80_500, "kg"), "80.5");
    assert_eq!(format_weight(81_647, "lb"), "180");
}

#[test]
fn the_sample_has_a_goal_and_a_series() {
    let e = crate::sample_engine(NOW);
    assert_eq!(e.state().entries.len(), 30);
    assert_eq!(e.state().settings.goal_grams, Some(78_000));
}

#[test]
fn a_finished_tag_on_a_closed_form_does_nothing() {
    let mut r = Rig::new();
    r.h.core.message(crate::server::Msg::Done(Tag::Edit, Err("late".into())));
    r.h.core.message(crate::server::Msg::Done(Tag::Settings, Err("late".into())));
    assert!(r.h.app().edit.is_none());
    assert!(r.h.app().settings.is_none());
}

#[test]
fn a_dead_token_says_signed_out_once() {
    struct Dead;
    impl crate::server::Server for Dead {
        fn state(&mut self, _: &str) -> Result<secretary_body::View, crate::server::ServerError> {
            Err(crate::server::ServerError::SignedOut)
        }
        fn command(&mut self, _: &serde_json::Value) -> Result<secretary_body::Response, crate::server::ServerError> {
            Err(crate::server::ServerError::SignedOut)
        }
    }
    let count = |msgs: Vec<crate::server::Msg>| msgs.iter().filter(|m| matches!(m, crate::server::Msg::SignedOut)).count();
    let mut w = Worker::new(Box::new(Dead), "UTC".into());
    assert_eq!(count(w.handle(Job::Refresh)), 1);
    assert_eq!(count(w.handle(Job::Log { text: "80".into(), unit: "kg".into(), note: None })), 1);
}

#[test]
fn the_keyboard_alone_opens_a_weigh_in_and_escape_closes_it() {
    let mut r = Rig::new();
    r.log("80");
    for _ in 0..6 {
        r.h.key(keys::KEY_TAB);
    }
    r.h.key(keys::KEY_ENTER);
    r.h.core.paint();
    assert!(r.h.app().edit.is_some(), "Enter on the list opens the selected weigh-in");
    r.h.key(keys::KEY_ESC);
    r.h.core.paint();
    assert!(r.h.app().edit.is_none());
}
