//! The app against the real `secretary-gym` engine, through the same JSON as the wire.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use lucina_app::{Harness, keys};
use secretary_gym::Engine;
use serde_json::json;

use crate::app::{Confirm, Link, PickFor, Row, Screen, Training, format_countdown, format_distance, format_duration};
use crate::server::{Fake, Job, Msg, Tag, Worker};

const NOW: u64 = 1_790_000_000_000;
const TZ: chrono_tz::Tz = chrono_tz::Europe::Copenhagen;

struct Rig {
    h: Harness<Training>,
    w: Worker,
    rx: mpsc::Receiver<Job>,
    engine: Arc<Mutex<Engine>>,
    offline: Arc<AtomicBool>,
    lose: Arc<AtomicBool>,
    body_down: Arc<AtomicBool>,
}

impl Rig {
    fn new() -> Rig {
        let fake = Fake::new(Engine::in_memory(), crate::sample_body(NOW), NOW);
        let (engine, offline, lose, body_down) = (
            fake.engine.clone(),
            fake.offline.clone(),
            fake.lose_answers.clone(),
            fake.body_down.clone(),
        );
        let w = Worker::new(Box::new(fake), "Europe/Copenhagen".into());
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let h = Harness::new(Training::new(TZ, Some(link)), 1920, 1080);
        let mut r = Rig { h, w, rx, engine, offline, lose, body_down };
        r.pump();
        r
    }

    fn pump(&mut self) {
        for _ in 0..4 {
            self.h.core.paint();
            while let Ok(job) = self.rx.try_recv() {
                for m in self.w.handle(job) {
                    self.h.core.message(m);
                }
            }
            self.h.core.input(lucina_app::Input::Wheel(0));
        }
        self.h.core.paint();
    }

    fn key(&mut self, code: u16) {
        self.h.key(code);
        self.pump();
    }

    fn exercise_id(&mut self, n: usize) -> String {
        self.engine
            .lock()
            .unwrap()
            .state()
            .exercises
            .iter()
            .filter(|e| e.kind == secretary_gym::ExerciseKind::WeightReps)
            .nth(n)
            .unwrap()
            .id
            .clone()
    }

    /// A workout with one exercise of two sets, the selection on the first set.
    fn workout(&mut self) {
        self.h.app().start_empty();
        self.pump();
        assert_eq!(self.h.app().screen, Screen::Workout);
        let id = self.exercise_id(0);
        self.h.app().run(Tag::Other, json!({"type": "add_exercise", "exercise_id": id}));
        self.pump();
        let item = self.h.app().active().unwrap().exercises[0].id.clone();
        let have = self.h.app().active().unwrap().exercises[0].sets.len();
        for _ in have..2 {
            self.h.app().add_set(&item);
            self.pump();
        }
        let first = self.h.app().rows.iter().position(|r| matches!(r, Row::Set { .. })).unwrap();
        self.h.app().workout_list.select(first);
        self.pump();
    }

    fn sets(&mut self) -> Vec<secretary_gym::WorkoutSet> {
        let e = self.engine.lock().unwrap();
        e.state().active.as_ref().map(|a| a.exercises[0].sets.clone()).unwrap_or_default()
    }

    fn elsewhere(&mut self) {
        let mut e = self.engine.lock().unwrap();
        let rev = e.state().revision;
        let r = json!({"op_id": format!("other-{rev}"), "expected_revision": rev, "timezone": "UTC",
            "command": {"type": "update_settings", "weight_unit": "kg", "rest_seconds": 75, "weekly_goal": 4}});
        assert!(e.handle_at(&r.to_string(), NOW).ok);
    }
}

#[test]
fn the_keyboard_alone_logs_a_set_and_moves_to_the_next() {
    let mut r = Rig::new();
    r.workout();
    // Enter on a set puts the cursor in its first field.
    r.h.app().focus_next = Some("rows");
    r.pump();
    r.key(keys::KEY_ENTER);
    // What is in a field is selected on the way in: typing replaces it.
    r.h.type_str("62.5");
    assert_eq!(r.h.app().draft.as_ref().unwrap().weight.text, "62.5");
    r.key(keys::KEY_ENTER);
    r.h.type_str("8");
    assert_eq!(r.h.app().draft.as_ref().unwrap().reps.text, "8");
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().action_error, None, "the set is sent once, not again on leaving it");
    let sets = r.sets();
    assert_eq!(sets[0].weight_grams, 62_500);
    assert_eq!(sets[0].reps, 8);
    assert!(sets[0].completed);
    assert!(!sets[1].completed);
    assert_eq!(r.h.app().selected_set().unwrap().1, sets[1].id, "on to the next set");
    assert_eq!(r.h.app().draft.as_ref().unwrap().set_id, sets[1].id);
}

#[test]
fn starting_a_routine_goes_to_the_workout_and_finishing_to_the_journal_entry() {
    let mut r = Rig::new();
    let ids = vec![r.exercise_id(0), r.exercise_id(1)];
    r.h.app().run(Tag::Other, json!({"type": "save_routine", "name": "My legs", "exercise_ids": ids}));
    r.pump();
    let id = r.h.app().view.as_ref().unwrap().state.routines.iter().find(|x| x.name == "My legs").unwrap().id.clone();
    r.h.app().start_routine(&id, "My legs");
    r.pump();
    assert_eq!(r.h.app().screen, Screen::Workout);
    assert_eq!(r.h.app().active().unwrap().title, "My legs");
    assert_eq!(r.h.app().active().unwrap().exercises.len(), 2);
    // A workout needs something done before it can finish.
    let first = r.h.app().rows.iter().position(|r| matches!(r, Row::Set { .. })).unwrap();
    r.h.app().workout_list.select(first);
    r.pump();
    r.h.app().toggle_done();
    r.pump();
    r.h.app().finish();
    r.pump();
    assert!(r.h.app().active().is_none());
    let newest = r.h.app().view.as_ref().unwrap().state.history[0].id.clone();
    assert_eq!(r.h.app().screen, Screen::Detail(newest));
    assert!(!r.h.app().busy);
}

#[test]
fn a_typed_value_goes_out_when_the_set_is_left() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().draft.as_mut().unwrap().weight.set("70");
    r.h.app().draft.as_mut().unwrap().reps.set("5");
    let at = r.h.app().workout_list.selected.unwrap();
    r.h.app().workout_list.select(at + 1);
    r.pump();
    let sets = r.sets();
    assert_eq!(sets[0].weight_grams, 70_000);
    assert_eq!(sets[0].reps, 5);
    assert!(!sets[0].completed);
}

#[test]
fn a_bad_number_is_refused_here_and_nothing_is_sent() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().draft.as_mut().unwrap().reps.set("many");
    let before = r.sets()[0].clone();
    r.h.app().toggle_done();
    r.pump();
    assert_eq!(r.sets()[0], before);
    assert_eq!(r.h.app().draft.as_ref().unwrap().error.as_deref(), Some("Reps must be a whole number."));
    assert_eq!(r.h.app().draft.as_ref().unwrap().reps.text, "many");
}

#[test]
fn the_engine_refuses_a_bad_weight_and_the_set_is_not_completed() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().draft.as_mut().unwrap().weight.set("heavy");
    r.h.app().toggle_done();
    r.pump();
    assert!(!r.sets()[0].completed, "the save failed, so it does not go on to complete");
    assert!(r.h.app().action_error.is_some());
    assert_eq!(r.h.app().draft.as_ref().unwrap().weight.text, "heavy", "what was typed stays");
    assert!(r.h.app().done_sent.is_empty());
}

#[test]
fn a_conflict_is_fetched_again_and_applied_once() {
    let mut r = Rig::new();
    r.workout();
    r.elsewhere();
    r.h.app().toggle_done();
    r.pump();
    assert!(r.sets()[0].completed);
    assert!(r.h.app().action_error.is_none());
    assert_eq!(r.h.app().view.as_ref().unwrap().state.settings.rest_seconds, 75, "and the other change shows");
}

#[test]
fn no_connection_keeps_what_was_typed_and_sending_again_works() {
    let mut r = Rig::new();
    r.workout();
    r.offline.store(true, Ordering::SeqCst);
    r.h.app().draft.as_mut().unwrap().weight.set("80");
    r.h.app().toggle_done();
    r.pump();
    assert!(r.h.app().action_error.as_deref().unwrap_or_default().contains("Not saved"));
    assert_eq!(r.h.app().draft.as_ref().unwrap().weight.text, "80");
    r.offline.store(false, Ordering::SeqCst);
    r.h.app().toggle_done();
    r.pump();
    assert_eq!(r.sets()[0].weight_grams, 80_000);
    assert!(r.sets()[0].completed);
}

#[test]
fn a_lost_answer_sent_again_adds_one_set() {
    let mut r = Rig::new();
    r.workout();
    let item = r.h.app().active().unwrap().exercises[0].id.clone();
    let before = r.sets().len();
    r.lose.store(true, Ordering::SeqCst);
    r.h.app().add_set(&item);
    r.pump();
    assert_eq!(r.sets().len(), before + 1, "it did arrive");
    assert!(r.h.app().action_error.is_some(), "this side cannot know that");
    r.lose.store(false, Ordering::SeqCst);
    r.h.app().add_set(&item);
    r.pump();
    assert_eq!(r.sets().len(), before + 1, "the engine answers with its receipt");
}

#[test]
fn a_dead_token_says_signed_out_once() {
    let count = |m: Vec<Msg>| m.iter().filter(|x| matches!(x, Msg::SignedOut)).count();
    let mut w = Worker::new(
        Box::new({
            let f = Fake::new(Engine::in_memory(), secretary_body::Engine::in_memory(), NOW);
            f.dead.store(true, Ordering::SeqCst);
            f
        }),
        "UTC".into(),
    );
    assert_eq!(count(w.handle(Job::Refresh)), 1);
    assert_eq!(count(w.handle(Job::Run(Tag::Other, json!({"type": "stop_rest"})))), 1);
}

#[test]
fn the_weight_summary_failing_does_not_block_training() {
    let mut r = Rig::new();
    assert!(r.h.app().body.is_some(), "it shows when it can");
    r.h.app().body = None;
    r.body_down.store(true, Ordering::SeqCst);
    r.h.app().refresh();
    r.pump();
    assert!(r.h.app().body.is_none());
    assert!(r.h.app().view.is_some());
    assert!(r.h.app().load_error.is_none());
    assert!(r.h.app().action_error.is_none());
}

#[test]
fn a_late_answer_on_a_closed_screen_does_nothing() {
    let mut r = Rig::new();
    r.h.core.message(Msg::Done(Tag::SaveRoutine, Err("late".into())));
    r.h.core.message(Msg::Done(Tag::Measurement, Err("late".into())));
    r.h.core.message(Msg::Done(Tag::Edit("gone".into()), Err("late".into())));
    assert!(r.h.app().builder.is_none());
    assert!(r.h.app().measurement.is_none());
    r.h.core.paint();
}

#[test]
fn discard_asks_first_and_cancel_keeps_it() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().confirm = Some(Confirm::Discard);
    r.pump();
    // Enter takes Cancel: nothing is destroyed by a stray key.
    r.key(keys::KEY_ENTER);
    assert!(r.h.app().confirm.is_none());
    assert!(r.h.app().active().is_some());
    r.h.app().confirm = Some(Confirm::Discard);
    r.h.app().confirmed();
    r.pump();
    assert!(r.h.app().active().is_none());
    assert_eq!(r.h.app().screen, Screen::Home);
}

#[test]
fn a_journal_entry_repeats_and_deletes() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().toggle_done();
    r.pump();
    r.h.app().finish();
    r.pump();
    let id = r.h.app().view.as_ref().unwrap().state.history[0].id.clone();
    r.h.app().repeat(&id);
    r.pump();
    assert_eq!(r.h.app().screen, Screen::Workout);
    assert!(r.h.app().active().is_some());
    r.h.app().repeat(&id);
    r.pump();
    assert!(r.h.app().action_error.is_none(), "with a workout on, repeat does nothing rather than fail");
    r.h.app().confirm = Some(Confirm::Discard);
    r.h.app().confirmed();
    r.pump();
    r.h.app().confirm = Some(Confirm::DeleteWorkout(id));
    r.h.app().confirmed();
    r.pump();
    assert!(r.h.app().view.as_ref().unwrap().state.history.is_empty());
    assert_eq!(r.h.app().screen, Screen::Journal);
}

#[test]
fn the_picker_filters_marks_what_is_added_and_adds() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().open_picker(PickFor::Workout);
    r.pump();
    let all = r.h.app().picker_rows().len();
    assert!(all > 3);
    let first_name = r.h.app().exercise(&r.engine.lock().unwrap().state().exercises[0].id.clone()).unwrap().name.clone();
    r.h.app().picker.as_mut().unwrap().query.set(&first_name);
    assert!(r.h.app().picker_rows().len() < all);
    let added = r.h.app().active().unwrap().exercises[0].exercise_id.clone();
    assert!(r.h.app().picker_added(&added));
    let other = r.exercise_id(1);
    let e = r.h.app().exercise(&other).unwrap().clone();
    r.h.app().pick(&e);
    r.pump();
    assert_eq!(r.h.app().active().unwrap().exercises.len(), 2);
    assert!(r.h.app().picker.is_none());
    // One already there does nothing.
    r.h.app().open_picker(PickFor::Workout);
    let e = r.h.app().exercise(&added).unwrap().clone();
    r.h.app().pick(&e);
    r.pump();
    assert_eq!(r.h.app().active().unwrap().exercises.len(), 2);
}

#[test]
fn a_new_exercise_comes_back_in_the_search() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().open_picker(PickFor::Workout);
    r.h.app().picker.as_mut().unwrap().create = Some(crate::app::CreateExercise {
        name: lucina_app::TextEdit::with_text("Sled push"),
        muscle: lucina_app::TextEdit::with_text("Legs"),
        equipment: lucina_app::TextEdit::with_text("Sled"),
        kind: secretary_gym::ExerciseKind::Timed,
        error: None,
        first: false,
    });
    r.h.app().save_exercise();
    r.pump();
    let p = r.h.app().picker.as_ref().unwrap();
    assert!(p.create.is_none());
    assert_eq!(p.query.text, "Sled push");
    assert_eq!(r.h.app().picker_rows()[0].kind.clone(), secretary_gym::ExerciseKind::Timed);
}

#[test]
fn the_builder_needs_a_name_and_exercises_and_keeps_their_order() {
    let mut r = Rig::new();
    r.h.app().open_builder();
    r.h.app().save_builder();
    assert_eq!(r.h.app().builder.as_ref().unwrap().error.as_deref(), Some("Give the routine a name."));
    r.h.app().builder.as_mut().unwrap().name.set("My pull");
    r.h.app().save_builder();
    assert_eq!(r.h.app().builder.as_ref().unwrap().error.as_deref(), Some("Add at least one exercise."));
    let (a, b) = (r.exercise_id(0), r.exercise_id(1));
    {
        let builder = r.h.app().builder.as_mut().unwrap();
        builder.ids = vec![a.clone(), b.clone()];
        builder.list.select(1);
    }
    r.h.app().builder_move(-1);
    r.h.app().save_builder();
    r.pump();
    assert!(r.h.app().builder.is_none());
    let routine = r.h.app().view.as_ref().unwrap().state.routines.iter().find(|x| x.name == "My pull").unwrap().clone();
    assert_eq!(routine.exercises[0].exercise_id, b, "the order chosen");
    assert_eq!(routine.exercises[1].exercise_id, a);
}

#[test]
fn settings_save_and_a_rest_set_elsewhere_stays_selectable() {
    let mut r = Rig::new();
    r.elsewhere();
    r.h.app().refresh();
    r.pump();
    assert!(r.h.app().rest_options().contains(&75));
    r.h.app().go(Screen::Settings);
    {
        let s = r.h.app().settings.as_mut().unwrap();
        s.unit = "lb".into();
        s.rest = 120;
        s.goal = 5;
    }
    r.h.app().save_settings();
    r.pump();
    let v = r.engine.lock().unwrap().state().settings.clone();
    assert_eq!((v.weight_unit.as_str(), v.rest_seconds, v.weekly_goal), ("lb", 120, 5));
    assert!(r.h.app().settings.as_ref().unwrap().saved);
}

#[test]
fn a_measurement_needs_a_value_and_can_be_deleted() {
    let mut r = Rig::new();
    r.h.app().open_measurement();
    r.h.app().save_measurement();
    assert_eq!(r.h.app().measurement.as_ref().unwrap().error.as_deref(), Some("Enter a value."));
    r.h.app().measurement.as_mut().unwrap().value.set("82.5");
    r.h.app().save_measurement();
    r.pump();
    assert!(r.h.app().measurement.is_none());
    let m = r.h.app().view.as_ref().unwrap().state.measurements[0].clone();
    assert_eq!((m.kind.as_str(), m.value_milli), ("waist", 82_500));
    r.h.app().confirm = Some(Confirm::DeleteMeasurement(m.id, "Waist".into()));
    r.h.app().confirmed();
    r.pump();
    assert!(r.h.app().view.as_ref().unwrap().state.measurements.is_empty());
}

#[test]
fn rest_starts_counts_and_skips_and_only_then_is_there_a_clock() {
    let mut r = Rig::new();
    assert!(!r.h.app().counting(), "nothing counts on Home with nothing on");
    r.workout();
    assert!(r.h.app().counting(), "the workout's duration does");
    r.h.app().start_rest();
    r.pump();
    assert!(r.h.app().clock.as_ref().unwrap().rest_now() > 0);
    r.h.app().stop_rest();
    r.pump();
    assert_eq!(r.h.app().clock.as_ref().unwrap().rest_now(), 0);
}

#[test]
fn notes_go_out_when_the_row_is_left() {
    let mut r = Rig::new();
    r.workout();
    let item = r.h.app().active().unwrap().exercises[0].id.clone();
    r.h.app().sync_notes();
    r.h.app().notes.set("Slow on the way down");
    let last = r.h.app().rows.len() - 1;
    r.h.app().workout_list.select(last);
    r.pump();
    let e = r.engine.lock().unwrap();
    let saved = e.state().active.as_ref().unwrap().exercises.iter().find(|x| x.id == item).unwrap().notes.clone();
    assert_eq!(saved, "Slow on the way down");
}

#[test]
fn the_sample_has_routines_and_a_journal() {
    let e = crate::sample_engine(NOW);
    let start = Engine::in_memory().state().routines.len();
    assert_eq!(e.state().routines.len(), start + 2);
    assert_eq!(e.state().history[0].exercises.len(), 3, "from Push day");
    assert_eq!(e.state().history.len(), 8);
    assert!(e.state().active.is_none());
}

#[test]
fn numbers_read_as_on_the_web() {
    assert_eq!(format_countdown(90), "1:30");
    assert_eq!(format_duration(65), "1m 05s");
    assert_eq!(format_duration(3700), "1h 01m");
    assert_eq!(format_distance(800), "800 m");
    assert_eq!(format_distance(1500), "1.5 km");
    assert_eq!(format_distance(2000), "2 km");
}

#[test]
fn enter_on_opening_starts_the_first_routine_on_its_first_set() {
    let mut r = Rig::new();
    let first = r.h.app().view.as_ref().unwrap().state.routines[0].name.clone();
    r.key(keys::KEY_ENTER);
    assert_eq!(r.h.app().screen, Screen::Workout);
    assert_eq!(r.h.app().active().unwrap().title, first);
    assert!(r.h.app().selected_set().is_some(), "the selection is on a set, ready to log");
}

#[test]
fn leaving_a_set_while_its_completion_is_on_its_way_sends_nothing_more() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().draft.as_mut().unwrap().weight.set("70");
    // Nothing is answered until the pump: the screen moves on first, as it does on a real network.
    r.h.app().complete_and_next();
    r.h.core.paint();
    r.pump();
    assert_eq!(r.h.app().action_error, None);
    let sets = r.sets();
    assert_eq!(sets[0].weight_grams, 70_000);
    assert!(sets[0].completed);
}

#[test]
fn leaving_a_set_twice_sends_its_edit_once() {
    let mut r = Rig::new();
    r.workout();
    r.h.app().draft.as_mut().unwrap().weight.set("70");
    r.h.app().commit_draft();
    r.h.app().commit_draft();
    let mut jobs = 0;
    while let Ok(job) = r.rx.try_recv() {
        jobs += 1;
        for m in r.w.handle(job) {
            r.h.core.message(m);
        }
    }
    assert_eq!(jobs, 1);
}

// Export and restore, through the file picker's answer, on the real engine.

fn backup_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("training-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn an_export_lands_in_the_chosen_file_and_restores_on_the_engine() {
    use crate::app::DataPick;
    use lucina_files::picker::Picked;
    let d = backup_dir("roundtrip");
    let dest = d.join("backup.json");
    let mut r = Rig::new();
    r.h.app().data_picked(DataPick::Export { json: true }, Picked::File(dest.clone()));
    r.pump();
    let status = r.h.app().data_status.clone().unwrap();
    assert_eq!(status, (true, "Exported backup.json.".to_owned()));
    let text = std::fs::read_to_string(&dest).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
    assert!(!dest.with_extension("part").exists());
    // Back in: it asks first.
    r.h.app().data_picked(DataPick::Restore, Picked::File(dest.clone()));
    r.pump();
    assert_eq!(r.h.app().pending_backup.as_ref().map(|b| b.0.as_str()), Some("backup.json"));
    r.h.app().restore_backup();
    r.pump();
    assert_eq!(r.h.app().data_status.clone().unwrap(), (true, "Backup restored from backup.json.".to_owned()));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn a_csv_export_is_the_engines_csv() {
    use crate::app::DataPick;
    use lucina_files::picker::Picked;
    let d = backup_dir("csv");
    let dest = d.join("workouts.csv");
    let mut r = Rig::new();
    r.h.app().data_picked(DataPick::Export { json: false }, Picked::File(dest.clone()));
    r.pump();
    assert!(r.h.app().data_status.clone().unwrap().0);
    assert!(std::fs::read_to_string(&dest).unwrap().contains(','));
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn what_is_not_a_backup_is_said_and_nothing_is_asked() {
    use crate::app::DataPick;
    use lucina_files::picker::Picked;
    let d = backup_dir("bad");
    let not_json = d.join("note.json");
    std::fs::write(&not_json, "not json").unwrap();
    let mut r = Rig::new();
    r.h.app().data_picked(DataPick::Restore, Picked::File(not_json));
    r.pump();
    assert_eq!(r.h.app().data_status.clone().unwrap(), (false, "That file is not a Lucina Gym JSON backup.".to_owned()));
    assert!(r.h.app().pending_backup.is_none());
    let large = d.join("large.json");
    std::fs::write(&large, vec![b' '; (16 << 20) + 1]).unwrap();
    r.h.app().data_picked(DataPick::Restore, Picked::File(large));
    r.pump();
    assert_eq!(r.h.app().data_status.clone().unwrap(), (false, "This backup exceeds the 16 MB import limit.".to_owned()));
    // The engine's own word on JSON that is not a backup.
    let other = d.join("other.json");
    std::fs::write(&other, "{\"a\": 1}").unwrap();
    r.h.app().data_picked(DataPick::Restore, Picked::File(other));
    r.pump();
    r.h.app().restore_backup();
    r.pump();
    let (ok, m) = r.h.app().data_status.clone().unwrap();
    assert!(!ok, "{m}");
    std::fs::remove_dir_all(&d).unwrap();
}

#[test]
fn the_export_name_is_the_webs() {
    let mut r = Rig::new();
    let n = r.h.app().backup_file_name(true);
    assert!(n.starts_with("Lucina-Gym-") && n.ends_with(".json"), "{n}");
    assert!(r.h.app().backup_file_name(false).ends_with(".csv"));
}
