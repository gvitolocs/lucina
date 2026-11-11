//! The app against the real engine through the fake server. Jobs are queued and run when the test
//! says, so a test can look at the screen before the server has answered: that is the "instant" part.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::TimeZone;
use chrono_tz::Tz;
use lucina_app::Harness;
use lucina_app::keys;
use secretary_tasks::{Command, ViewQuery};

use crate::app::{Mode, Row, Tasks};
use crate::sample;
use crate::server::Fake;
use crate::store::{Store, query};
use crate::worker::{self, Job, Worker};

const TZ: Tz = chrono_tz::Europe::Copenhagen;

fn now() -> u64 {
    TZ.with_ymd_and_hms(2026, 11, 2, 12, 0, 0)
        .unwrap()
        .timestamp_millis() as u64
}

struct Rig {
    h: Harness<Tasks>,
    jobs: Rc<RefCell<VecDeque<Job>>>,
    w: Worker,
    offline: Arc<AtomicBool>,
    seen: Arc<AtomicU64>,
}

impl Rig {
    fn new() -> Rig {
        let mut fake = Fake::new(sample::engine(TZ, now()));
        fake.now_ms = now();
        let offline = fake.offline.clone();
        let seen = fake.seen.clone();
        let jobs: Rc<RefCell<VecDeque<Job>>> = Rc::default();
        let j = jobs.clone();
        let mut st = Store::new(TZ);
        st.now = now;
        let app = Tasks::new(
            st,
            Mode::Sample,
            Some(Box::new(move |job| j.borrow_mut().push_back(job))),
        );
        let mut r = Rig {
            h: Harness::new(app, 1920, 1080),
            jobs,
            w: Worker::new(Box::new(fake), TZ.name().into(), query("today", None)),
            offline,
            seen,
        };
        r.pump();
        r
    }

    /// The server thread's turn: everything queued, answers delivered.
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

    fn titles(&mut self) -> Vec<String> {
        self.h
            .app()
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Task { t, .. } => Some(t.content.clone()),
                _ => None,
            })
            .collect()
    }

    fn select(&mut self, content: &str) -> String {
        let (i, id) = self
            .h
            .app()
            .rows
            .iter()
            .enumerate()
            .find_map(|(i, r)| match r {
                Row::Task { t, .. } if t.content == content => Some((i, t.id.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no row {content}"));
        self.h.app().list.select(i);
        self.h.core.paint();
        id
    }
}

#[test]
fn today_is_the_engines_today_with_its_overdue_group() {
    let mut r = Rig::new();
    let titles = r.titles();
    assert!(
        titles.contains(&"Return the library books".to_string()),
        "{titles:?}"
    );
    assert!(titles.contains(&"Write the release notes".to_string()));
    assert!(!titles.contains(&"Plan the next sprint".to_string()));
    let v = r.h.app().shown.clone().unwrap();
    assert_eq!(v.counts.overdue, 2);
    assert_eq!(v.counts.today, 6);
    // Timed tasks first, as the engine sorts them.
    let first_today =
        r.h.app()
            .rows
            .iter()
            .filter_map(|x| match x {
                Row::Task { t, group, .. } if group != "overdue" => Some(t.content.clone()),
                _ => None,
            })
            .next();
    assert_eq!(first_today.as_deref(), Some("Write the release notes"));
}

#[test]
fn completing_shows_before_the_server_answers_and_survives_its_answer() {
    let mut r = Rig::new();
    let before = r.h.app().shown.as_ref().unwrap().counts.today;
    r.select("Oat milk and bread");
    r.h.key(keys::KEY_SPACE);
    // Nothing has reached the server, and the row has gone.
    assert_eq!(r.seen.load(Ordering::SeqCst), 0);
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    assert_eq!(r.h.app().shown.as_ref().unwrap().counts.today, before - 1);
    r.pump();
    assert_eq!(r.seen.load(Ordering::SeqCst), 1);
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    assert_eq!(r.h.app().shown.as_ref().unwrap().counts.today, before - 1);
    assert!(r.h.app().store.pending.is_empty());
    assert_eq!(r.h.app().shown.as_ref().unwrap().goal.completed_today, 1);
}

#[test]
fn undo_puts_it_back_at_once() {
    let mut r = Rig::new();
    r.select("Oat milk and bread");
    r.h.key(keys::KEY_SPACE);
    r.pump();
    r.h.app().undo();
    assert!(
        r.titles().contains(&"Oat milk and bread".to_string()),
        "before the answer"
    );
    r.pump();
    assert!(
        r.titles().contains(&"Oat milk and bread".to_string()),
        "after the answer"
    );
    assert_eq!(r.h.app().shown.as_ref().unwrap().goal.completed_today, 0);
}

#[test]
fn a_recurring_task_comes_back_on_its_next_date() {
    let mut r = Rig::new();
    r.select("Water the plants");
    r.h.key(keys::KEY_SPACE);
    r.pump();
    assert!(!r.titles().contains(&"Water the plants".to_string()));
    r.h.app().open_list(query("upcoming", None));
    r.pump();
    assert!(
        r.titles().contains(&"Water the plants".to_string()),
        "{:?}",
        r.titles()
    );
}

#[test]
fn quick_add_parses_here_shows_a_waiting_row_then_the_engines_task() {
    let mut r = Rig::new();
    r.h.key(keys::KEY_ESC);
    r.h.type_str("q");
    r.h.type_str("Call Marco tomorrow 9am #Work p1");
    let parsed = r.h.app().parsed.clone().unwrap();
    assert_eq!(parsed.content, "Call Marco");
    assert_eq!(parsed.project.as_deref(), Some("Work"));
    assert_eq!(parsed.priority, Some(1));
    assert_eq!(parsed.due_string.as_deref(), Some("tomorrow 9am"));
    r.h.key(keys::KEY_ENTER);
    assert_eq!(r.seen.load(Ordering::SeqCst), 0);
    assert!(
        r.titles().contains(&"Call Marco".to_string()),
        "a waiting row: {:?}",
        r.titles()
    );
    assert_eq!(r.h.app().add.text, "");
    r.pump();
    // Due tomorrow: not on Today any more, and in Work on the right day.
    assert!(!r.titles().contains(&"Call Marco".to_string()));
    r.h.app().open_list(query("project", Some("p2")));
    r.pump();
    let v = r.h.app().shown.clone().unwrap();
    let t = v
        .list
        .groups
        .iter()
        .flat_map(|g| &g.tasks)
        .find(|t| t.content == "Call Marco")
        .unwrap();
    assert_eq!(t.priority, 1);
    assert_eq!(t.due.as_ref().unwrap().date, "2026-11-03");
    assert_eq!(t.due.as_ref().unwrap().time.as_deref(), Some("09:00"));
}

#[test]
fn quick_add_on_today_defaults_to_today_as_the_web_app_does() {
    let mut r = Rig::new();
    r.h.app().add.set("Plain thing");
    r.h.app().quick_add();
    r.pump();
    assert!(r.titles().contains(&"Plain thing".to_string()));
}

#[test]
fn switching_lists_shows_the_cached_one_before_any_answer() {
    let mut r = Rig::new();
    // The prefetch has run: Inbox is in memory.
    assert!(r.h.app().store.has_list(&query("inbox", None)));
    r.h.app().open_list(query("inbox", None));
    assert!(
        r.titles()
            .contains(&"Sketch ideas for the garden shelf".to_string())
    );
    // The server hasn't been asked yet: a Load is still waiting.
    assert!(!r.jobs.borrow().is_empty());
    r.pump();
}

#[test]
fn every_list_kind_loads_from_the_engine() {
    let mut r = Rig::new();
    r.h.app().open_list(query("upcoming", None));
    r.pump();
    assert!(r.titles().contains(&"Book the dentist".to_string()));
    r.h.app().open_list(query("label", Some("errand")));
    r.pump();
    assert_eq!(r.titles().len(), 2);
    r.h.app().open_list(query("filter", Some("f1")));
    r.pump();
    assert!(r.titles().contains(&"Write the release notes".to_string()));
    r.h.app().open_list(query("project", Some("p4")));
    r.pump();
    let t = r.titles();
    assert!(
        t.contains(&"Exercise 1".to_string()),
        "sub-tasks nest in: {t:?}"
    );
}

#[test]
fn the_keyboard_search_runs_the_engines_search() {
    let mut r = Rig::new();
    r.h.key(keys::KEY_ESC);
    r.h.type_str("/");
    r.pump();
    r.h.type_str("dentist");
    r.pump();
    assert_eq!(r.titles(), vec!["Book the dentist".to_string()]);
}

#[test]
fn editing_saves_only_what_changed_and_shows_at_once() {
    let mut r = Rig::new();
    let id = r.select("Oat milk and bread");
    r.h.app().open_detail(&id);
    r.h.app()
        .detail
        .as_mut()
        .unwrap()
        .title
        .set("Oat milk, bread, eggs");
    r.h.app().save_detail();
    assert!(
        r.titles().contains(&"Oat milk, bread, eggs".to_string()),
        "before the answer"
    );
    let q = r.h.app().store.pending[0].command.clone();
    match q {
        Command::Update {
            content,
            description,
            labels,
            ..
        } => {
            assert!(content.is_some());
            assert!(description.is_none() && labels.is_none());
        }
        other => panic!("{other:?}"),
    }
    r.pump();
    assert!(r.titles().contains(&"Oat milk, bread, eggs".to_string()));
    assert!(r.h.app().store.pending.is_empty());
}

#[test]
fn priority_keys_and_the_date_picker_reach_the_engine() {
    let mut r = Rig::new();
    r.select("Oat milk and bread");
    r.h.type_str("1");
    r.pump();
    let v = r.h.app().shown.clone().unwrap();
    let t = v
        .list
        .groups
        .iter()
        .flat_map(|g| &g.tasks)
        .find(|t| t.content.starts_with("Oat"))
        .unwrap();
    assert_eq!(t.priority, 1);
    // d, then a phrase with a time, typed in the picker.
    r.select("Oat milk and bread");
    r.h.type_str("d");
    assert!(r.h.app().picker.is_some());
    r.h.core.paint();
    r.h.type_str("tomorrow 3pm");
    assert_eq!(
        r.h.app()
            .picker
            .as_ref()
            .unwrap()
            .preview
            .as_ref()
            .unwrap()
            .date,
        "2026-11-03"
    );
    r.h.key(keys::KEY_ENTER);
    assert!(
        !r.titles().contains(&"Oat milk and bread".to_string()),
        "moved off Today at once"
    );
    r.pump();
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    r.h.app().open_list(query("upcoming", None));
    r.pump();
    let v = r.h.app().shown.clone().unwrap();
    let t = v
        .list
        .groups
        .iter()
        .flat_map(|g| &g.tasks)
        .find(|t| t.content.starts_with("Oat"))
        .unwrap();
    assert_eq!(t.due.as_ref().unwrap().time.as_deref(), Some("15:00"));
}

#[test]
fn clearing_a_date_and_rescheduling_overdue_follow_the_engine() {
    let mut r = Rig::new();
    let id = r.select("Oat milk and bread");
    r.h.app().picker = Some(crate::app::DatePicker {
        target: crate::app::PickFor::Task(id),
        phrase: lucina_app::TextEdit::new(),
        preview: None,
    });
    r.h.app().apply_date(None);
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    r.pump();
    r.h.app().picker = Some(crate::app::DatePicker {
        target: crate::app::PickFor::Overdue,
        phrase: lucina_app::TextEdit::new(),
        preview: None,
    });
    r.h.app().apply_date(None);
    r.pump();
    assert_eq!(r.h.app().shown.as_ref().unwrap().counts.overdue, 0);
    assert!(r.titles().contains(&"Return the library books".to_string()));
}

#[test]
fn deleting_hides_at_once_and_can_be_undone() {
    let mut r = Rig::new();
    let id = r.select("Oat milk and bread");
    r.h.key(keys::KEY_DELETE);
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    r.pump();
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
    assert!(r.h.app().shown.as_ref().unwrap().undo.contains(&id));
    r.h.app().undo();
    assert!(r.titles().contains(&"Oat milk and bread".to_string()));
    r.pump();
    assert!(r.titles().contains(&"Oat milk and bread".to_string()));
}

#[test]
fn offline_commands_wait_in_order_and_go_once_when_it_comes_back() {
    let mut r = Rig::new();
    r.offline.store(true, Ordering::SeqCst);
    r.select("Oat milk and bread");
    r.h.key(keys::KEY_SPACE);
    r.pump();
    r.h.app().add.set("Added while offline #Work");
    r.h.app().quick_add();
    r.pump();
    assert_eq!(r.h.app().store.pending.len(), 2);
    assert!(r.h.app().offline.is_some());
    assert!(
        !r.titles().contains(&"Oat milk and bread".to_string()),
        "still hidden"
    );
    assert!(
        r.titles().contains(&"Added while offline".to_string()),
        "still waiting"
    );
    assert_eq!(r.w.waiting(), 2);
    // Back again: the clock retries.
    r.offline.store(false, Ordering::SeqCst);
    r.jobs.borrow_mut().push_back(Job::Tick);
    r.pump();
    assert!(r.h.app().store.pending.is_empty());
    assert!(r.h.app().offline.is_none());
    assert_eq!(r.seen.load(Ordering::SeqCst), 2);
    assert!(!r.titles().contains(&"Oat milk and bread".to_string()));
}

#[test]
fn a_repeat_of_a_command_is_harmless_through_its_op_id() {
    let mut fake = Fake::new(sample::engine(TZ, now()));
    fake.now_ms = now();
    use crate::server::Server;
    let c = Command::Complete {
        task_id: "t8".into(),
    };
    let q = ViewQuery::default();
    fake.command("same", TZ.name(), &c, &q).unwrap();
    let again = fake.command("same", TZ.name(), &c, &q).unwrap();
    assert!(again.replayed);
    assert_eq!(fake.engine.state().completed.len(), 1);
}

#[test]
fn a_refused_command_comes_off_the_screen_and_says_why() {
    let mut r = Rig::new();
    r.h.app().run(
        Command::Complete {
            task_id: "no-such-task".into(),
        },
        None,
    );
    r.pump();
    assert!(r.h.app().store.pending.is_empty());
    assert!(r.h.app().error.is_some());
}

#[test]
fn not_signed_in_shows_no_lists_and_never_talks_to_a_server() {
    let st = Store::new(TZ);
    let mut h = Harness::new(Tasks::new(st, Mode::NotSignedIn, None), 1920, 1080);
    assert!(h.app().rows.is_empty());
    assert!(h.app().shown.is_none());
    h.key(keys::KEY_ENTER);
    assert_eq!(h.app().mode, Mode::NotSignedIn);
}

#[test]
fn newer_loads_overtake_older_ones_and_prefetches_wait() {
    let mut q: VecDeque<Job> = VecDeque::new();
    q.push_back(Job::Prefetch(query("inbox", None)));
    q.push_back(Job::Load(query("today", None)));
    q.push_back(Job::Load(query("upcoming", None)));
    let Some(Job::Load(first)) = worker::next_job(&mut q) else {
        panic!("a load first");
    };
    assert_eq!(first.kind.as_deref(), Some("upcoming"));
    assert!(
        matches!(q.front(), Some(Job::Prefetch(_))),
        "the prefetch waits"
    );
}

#[test]
fn drawing_one_key_touches_little_of_the_screen() {
    let mut r = Rig::new();
    r.h.key(keys::KEY_ESC);
    r.h.type_str("q");
    r.h.type_str("Buy");
    let dirty = r.h.key(keys::KEY_BACKSPACE);
    let px: i32 = dirty.iter().map(|x| x.w * x.h).sum();
    assert!(px < 1920 * 1080 / 4, "{px} pixels for one key");
}

#[test]
fn a_click_on_the_round_box_completes_and_on_the_row_opens() {
    let mut r = Rig::new();
    // The first row at 1920x1080: its box, then its text.
    r.h.click(367, 293);
    assert!(!r.titles().contains(&"Return the library books".to_string()));
    r.pump();
    assert_eq!(r.h.app().shown.as_ref().unwrap().goal.completed_today, 1);
    assert!(r.h.app().toast.is_some());
    r.h.click(800, 293);
    assert_eq!(
        r.h.app().detail.as_ref().unwrap().title.text,
        "Send the invoice"
    );
    // Tab goes through the widgets, never to an invisible target: from the list round to the field.
    r.h.app().detail = None;
    r.h.key(keys::KEY_ESC);
    let mut reached = false;
    for _ in 0..8 {
        r.h.key(keys::KEY_TAB);
        r.h.type_str("x");
        reached |= r.h.app().add.text.contains('x');
        if reached {
            break;
        }
    }
    assert!(reached);
}

#[test]
fn the_keyboard_stays_on_the_list_when_it_opens_a_list_not_loaded_yet() {
    let mut r = Rig::new();
    r.h.app().open_list(query("completed", None));
    r.h.core.paint();
    assert!(r.h.app().rows.is_empty());
    r.pump();
    r.h.app().open_list(query("upcoming", None));
    r.pump();
    assert!(r.h.app().rows.len() > 1);
    r.h.key(keys::KEY_DOWN);
    r.h.key(keys::KEY_ENTER);
    assert!(
        r.h.app().detail.is_some(),
        "the list had the keys, not the sidebar"
    );
}

#[test]
fn the_cache_brings_back_lists_and_unsent_commands() {
    let dir = std::env::temp_dir().join(format!("tasks-cache-{}", std::process::id()));
    // SAFETY: no other test in this crate reads this variable.
    unsafe { std::env::set_var(lucina_account::credential::PATH_ENV, dir.join("credential")) };
    let mut r = Rig::new();
    r.h.app().add.set("Unsent");
    r.h.app().quick_add();
    crate::cache::save(&r.h.app().store.snapshot());
    let saved = crate::cache::load().expect("saved");
    let mut st = Store::new(TZ);
    st.now = now;
    st.restore(saved);
    assert_eq!(st.pending.len(), 1);
    let shown = st.shown(&query("today", None)).unwrap();
    assert!(
        shown
            .list
            .groups
            .iter()
            .flat_map(|g| &g.tasks)
            .any(|t| t.content == "Unsent")
    );
    let _ = std::fs::remove_dir_all(dir);
}
