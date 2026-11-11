//! Training's state and what each action sends: `web/src/views/gym/*.tsx`. The engine owns parsing, statistics and
//! labels. The screens are in `screens.rs`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::DateTime;
use chrono_tz::Tz;
use lucina_account::ui::SignInView;
use lucina_app::{App, ListState, TextEdit, Ui};
use lucina_files::media::Connect;
use lucina_files::picker::{self, FilePicker, Mode, Picked, Setup};
use lucina_files::places;
use secretary_gym::{Exercise, ExerciseKind, SetKind, View, WorkoutSet};
use serde_json::{Value, json};

use crate::server::{Job, Msg, Tag};

pub const REST_PRESETS: [u64; 7] = [30, 45, 60, 90, 120, 180, 300];
/// `CLOCK_POLL_MS`: the server's clock corrects the local tick this often.
pub const CLOCK_POLL: Duration = Duration::from_secs(15);
/// Refreshed on opening and after every command. Another device's changes are the only other source.
const REFRESH: Duration = Duration::from_secs(300);
pub const MAX_ROUTINE_EXERCISES: usize = 30;

pub type Link = Box<dyn FnMut(Job)>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    Workout,
    Journal,
    Detail(String),
    Progress,
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Weight,
    Reps,
    Seconds,
    Distance,
}

/// `fieldsFor`.
pub fn fields_for(kind: &ExerciseKind) -> &'static [Field] {
    match kind {
        ExerciseKind::RepsOnly => &[Field::Reps],
        ExerciseKind::Timed => &[Field::Seconds],
        ExerciseKind::DistanceTime => &[Field::Distance, Field::Seconds],
        ExerciseKind::WeightReps => &[Field::Weight, Field::Reps],
    }
}

pub fn set_kind_name(k: &SetKind) -> &'static str {
    match k {
        SetKind::Normal => "normal",
        SetKind::Warmup => "warmup",
        SetKind::Drop => "drop",
        SetKind::Failure => "failure",
    }
}

pub fn set_kind_label(k: &SetKind) -> &'static str {
    match k {
        SetKind::Normal => "Normal",
        SetKind::Warmup => "Warmup",
        SetKind::Drop => "Drop",
        SetKind::Failure => "Failure",
    }
}

/// `setBadge`: a normal set's number, else W, D or F.
pub fn set_badge(k: &SetKind, index: usize) -> String {
    match k {
        SetKind::Normal => (index + 1).to_string(),
        SetKind::Warmup => "W".into(),
        SetKind::Drop => "D".into(),
        SetKind::Failure => "F".into(),
    }
}

pub fn sets_text(n: usize) -> String {
    if n == 1 { "1 set".into() } else { format!("{n} sets") }
}

pub fn kind_label(k: &ExerciseKind) -> &'static str {
    match k {
        ExerciseKind::WeightReps => "Weight & reps",
        ExerciseKind::RepsOnly => "Reps only",
        ExerciseKind::Timed => "Time",
        ExerciseKind::DistanceTime => "Distance & time",
    }
}

fn kind_name(k: &ExerciseKind) -> &'static str {
    match k {
        ExerciseKind::WeightReps => "weight_reps",
        ExerciseKind::RepsOnly => "reps_only",
        ExerciseKind::Timed => "timed",
        ExerciseKind::DistanceTime => "distance_time",
    }
}

pub fn measurement_label(kind: &str) -> String {
    match kind {
        "body_fat" => "Body fat".into(),
        "waist" => "Waist".into(),
        "body_weight" => "Body weight".into(),
        other => {
            let t = other.replace('_', " ");
            let mut c = t.chars();
            c.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + c.as_str())
        }
    }
}

/// `formatCountdown`: 90 -> "1:30".
pub fn format_countdown(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// `formatDuration`.
pub fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60)
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

/// `formatDistance`.
pub fn format_distance(meters: u64) -> String {
    if meters >= 1000 {
        let s = format!("{:.1}", meters as f64 / 1000.0);
        format!("{} km", s.strip_suffix(".0").unwrap_or(&s))
    } else {
        format!("{meters} m")
    }
}

pub fn format_date(ms: u64, tz: Tz) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map_or_else(String::new, |d| d.with_timezone(&tz).format("%-d %b %Y").to_string())
}

pub fn format_when(ms: u64, tz: Tz) -> String {
    DateTime::from_timestamp_millis(ms as i64)
        .map_or_else(String::new, |d| d.with_timezone(&tz).format("%-d %b %Y %H:%M").to_string())
}

/// What the screen's clock shows between the server's answers: visual only, the server's clock is the source.
#[derive(Clone, Debug)]
pub struct ClockState {
    pub rest_remaining: u64,
    pub elapsed: u64,
    pub received: Instant,
}

impl ClockState {
    fn since(&self) -> u64 {
        self.received.elapsed().as_secs()
    }

    pub fn rest_now(&self) -> u64 {
        self.rest_remaining.saturating_sub(self.since())
    }

    pub fn elapsed_now(&self) -> u64 {
        self.elapsed + self.since()
    }
}

/// The selected set's fields, typed but not yet sent.
#[derive(Debug)]
pub struct Draft {
    pub set_id: String,
    pub weight: TextEdit,
    pub reps: TextEdit,
    pub seconds: TextEdit,
    pub distance: TextEdit,
    pub error: Option<String>,
}

/// One row of the workout's list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Exercise(String),
    Set { item: String, set: String, index: usize },
    AddSet(String),
    AddExercise,
}

pub enum Confirm {
    Discard,
    DeleteWorkout(String),
    DeleteRoutine(String, String),
    RemoveExercise(String, String),
    DeleteMeasurement(String, String),
}

/// What an exercise picked in the picker is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickFor {
    Workout,
    Routine,
}

pub struct Picker {
    pub purpose: PickFor,
    pub query: TextEdit,
    pub muscle: Option<String>,
    pub list: ListState,
    pub first: bool,
    pub create: Option<CreateExercise>,
}

pub struct CreateExercise {
    pub name: TextEdit,
    pub muscle: TextEdit,
    pub equipment: TextEdit,
    pub kind: ExerciseKind,
    pub error: Option<String>,
    pub first: bool,
}

pub struct Builder {
    pub name: TextEdit,
    pub ids: Vec<String>,
    pub list: ListState,
    pub error: Option<String>,
    pub first: bool,
}

pub struct SettingsForm {
    pub unit: String,
    pub rest: u64,
    pub goal: u64,
    pub saved: bool,
}

pub struct MeasurementForm {
    pub kind: &'static str,
    pub value: TextEdit,
    pub error: Option<String>,
    pub first: bool,
}

/// What the file picker is open for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataPick {
    Export { json: bool },
    Restore,
}

pub struct Training {
    pub tz: Tz,
    pub data_picker: Option<(FilePicker, DataPick)>,
    /// A backup read and waiting for "Restore backup": its name and text.
    pub pending_backup: Option<(String, String)>,
    /// How the last export or restore went: true when well.
    pub data_status: Option<(bool, String)>,
    wake: Option<Arc<dyn Fn() + Send + Sync>>,
    drive: Option<Connect>,
    link: Option<Link>,
    pub sign_in: Option<SignInView>,
    start_sign_in: Option<Box<dyn FnMut() -> Option<SignInView>>>,
    start_worker: Option<Box<dyn FnMut() -> Option<Link>>>,
    save_cache: Option<Box<dyn FnMut(&View)>>,
    pub sample: bool,

    pub view: Option<View>,
    pub body: Option<secretary_body::View>,
    pub clock: Option<ClockState>,
    pub screen: Screen,
    pub load_error: Option<String>,
    pub action_error: Option<String>,
    pub busy: bool,

    pub home_list: ListState,
    pub workout_list: ListState,
    pub rows: Vec<Row>,
    pub draft: Option<Draft>,
    /// What was sent for a set and not yet confirmed: the list shows it at once, and a failure keeps it on screen.
    pub sent: HashMap<String, (String, String, String, String)>,
    /// A set's new done state, shown before the answer.
    pub done_sent: HashMap<String, bool>,
    pub focus_next: Option<&'static str>,
    pub journal_list: ListState,
    pub records_list: ListState,
    pub measurements_list: ListState,
    pub title: Option<TextEdit>,
    pub routine_name: Option<TextEdit>,
    pub routine_saved: bool,
    pub notes: TextEdit,
    pub notes_for: Option<String>,

    pub confirm: Option<Confirm>,
    pub picker: Option<Picker>,
    pub builder: Option<Builder>,
    pub settings: Option<SettingsForm>,
    pub measurement: Option<MeasurementForm>,

    last_load: Option<Instant>,
    last_clock: Option<Instant>,
}

impl Training {
    pub fn new(tz: Tz, link: Option<Link>) -> Training {
        Training {
            tz,
            data_picker: None,
            pending_backup: None,
            data_status: None,
            wake: None,
            drive: None,
            link,
            sign_in: None,
            start_sign_in: None,
            start_worker: None,
            save_cache: None,
            sample: false,
            view: None,
            body: None,
            clock: None,
            screen: Screen::Home,
            load_error: None,
            action_error: None,
            busy: false,
            home_list: ListState::new(),
            workout_list: ListState::new(),
            rows: Vec::new(),
            draft: None,
            sent: HashMap::new(),
            done_sent: HashMap::new(),
            // The keyboard starts on the list, not on the sections.
            focus_next: Some("home"),
            journal_list: ListState::new(),
            records_list: ListState::new(),
            measurements_list: ListState::new(),
            title: None,
            routine_name: None,
            routine_saved: false,
            notes: TextEdit::multiline(),
            notes_for: None,
            confirm: None,
            picker: None,
            builder: None,
            settings: None,
            measurement: None,
            last_load: None,
            last_clock: None,
        }
    }

    pub fn sample(mut self) -> Training {
        self.sample = true;
        self
    }

    pub fn with_sign_in(
        mut self,
        start_sign_in: Box<dyn FnMut() -> Option<SignInView>>,
        start_worker: Box<dyn FnMut() -> Option<Link>>,
    ) -> Training {
        self.start_sign_in = Some(start_sign_in);
        self.start_worker = Some(start_worker);
        if self.link.is_none() {
            self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
        }
        self
    }

    /// The file picker redraws through this; a backup may come from the Drive through that.
    pub fn with_files(mut self, wake: Arc<dyn Fn() + Send + Sync>, drive: Option<Connect>) -> Training {
        self.wake = Some(wake);
        self.drive = drive;
        self
    }

    /// `backupFileName`, by the local calendar date.
    pub fn backup_file_name(&self, json: bool) -> String {
        let today = chrono::Utc::now().with_timezone(&self.tz).format("%Y-%m-%d");
        format!("Lucina-Gym-{today}.{}", if json { "json" } else { "csv" })
    }

    fn open_file_picker(&mut self, title: &str, mode: Mode, pick: DataPick) {
        let mut list = places::standard();
        list.rotate_left(3); // Documents first.
        let opening = matches!(mode, Mode::Open { .. });
        let setup = Setup {
            title: title.to_owned(),
            mode,
            places: list,
            drive: self.drive.clone().filter(|_| opening),
            fetch_to: picker::fetch_dir("training"),
        };
        let wake = self.wake.clone().unwrap_or_else(|| Arc::new(|| {}));
        self.data_status = None;
        self.data_picker = Some((FilePicker::start(setup, wake), pick));
    }

    /// `exportAs`: where to, then the engine's text.
    pub fn export_as(&mut self, json: bool) {
        let name = self.backup_file_name(json);
        let title = if json { "Export JSON backup" } else { "Export workout CSV" };
        self.open_file_picker(title, Mode::Save { name }, DataPick::Export { json });
    }

    /// The Restore button: a file, checked, then a question.
    pub fn choose_backup(&mut self) {
        self.pending_backup = None;
        self.open_file_picker("Restore JSON backup", Mode::Open { extensions: vec!["json".to_owned()] }, DataPick::Restore);
    }

    pub fn data_picked(&mut self, pick: DataPick, picked: Picked) {
        self.data_picker = None;
        let Picked::File(path) = picked else { return };
        match pick {
            DataPick::Export { json } => self.send(Job::Export { json, dest: path }),
            DataPick::Restore => self.send(Job::ReadBackup { path }),
        }
    }

    /// `restore`.
    pub fn restore_backup(&mut self) {
        if let Some((name, text)) = self.pending_backup.take() {
            self.send(Job::Restore { name, text });
        }
    }

    pub fn with_cache(mut self, save: Box<dyn FnMut(&View)>) -> Training {
        self.save_cache = Some(save);
        self
    }

    /// What was fetched last time, shown before any network.
    pub fn restore(&mut self, view: View) {
        self.view = Some(view);
        self.after_view();
    }

    pub fn send(&mut self, job: Job) {
        if let Some(l) = &mut self.link {
            l(job);
        }
    }

    pub fn refresh(&mut self) {
        self.last_load = Some(Instant::now());
        self.send(Job::Refresh);
        self.send(Job::Body);
    }

    /// `runGym`: clears the last error, sends.
    pub fn run(&mut self, tag: Tag, command: Value) {
        self.action_error = None;
        if matches!(tag, Tag::Start | Tag::Finish | Tag::Discard | Tag::DeleteWorkout) {
            self.busy = true;
        }
        self.send(Job::Run(tag, command));
    }

    pub fn exercise(&self, id: &str) -> Option<&Exercise> {
        self.view.as_ref()?.state.exercises.iter().find(|e| e.id == id)
    }

    pub fn active(&self) -> Option<&secretary_gym::Workout> {
        self.view.as_ref()?.state.active.as_ref()
    }

    pub fn go(&mut self, s: Screen) {
        self.commit_draft();
        self.commit_notes();
        if s == Screen::Settings {
            self.open_settings();
        }
        self.focus_next = match s {
            Screen::Home => Some("home"),
            Screen::Workout => Some("rows"),
            Screen::Journal => Some("journal"),
            Screen::Progress => Some("records"),
            Screen::Detail(_) => Some("back"),
            Screen::Settings => None,
        };
        self.screen = s;
        self.after_view();
    }

    // Starting.

    pub fn start_empty(&mut self) {
        if self.busy {
            return;
        }
        self.run(Tag::Start, json!({"type": "start_workout", "title": "Workout"}));
    }

    pub fn start_routine(&mut self, id: &str, name: &str) {
        if self.busy || self.active().is_some() {
            return;
        }
        self.run(Tag::Start, json!({"type": "start_workout", "title": name, "routine_id": id}));
    }

    pub fn repeat(&mut self, id: &str) {
        if self.busy || self.active().is_some() {
            return;
        }
        self.run(Tag::Start, json!({"type": "repeat_workout", "workout_id": id}));
    }

    // The workout's list.

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        if let Some(w) = self.active() {
            for item in &w.exercises {
                rows.push(Row::Exercise(item.id.clone()));
                for (index, s) in item.sets.iter().enumerate() {
                    rows.push(Row::Set { item: item.id.clone(), set: s.id.clone(), index });
                }
                rows.push(Row::AddSet(item.id.clone()));
            }
            rows.push(Row::AddExercise);
        }
        if rows != self.rows {
            self.workout_list.invalidate();
            // The selection stays on the same row where it can.
            let was = self.workout_list.selected.and_then(|i| self.rows.get(i)).cloned();
            let at = was.and_then(|r| rows.iter().position(|x| *x == r));
            let n = rows.len();
            self.rows = rows;
            match at {
                Some(i) => self.workout_list.select(i),
                None if n > 0 => {
                    // A row gone: the one now there. With none selected yet, the first set below.
                    if let Some(i) = self.workout_list.selected {
                        self.workout_list.select(i.min(n - 1));
                    }
                }
                None => {}
            }
        }
        if self.workout_list.selected.is_none() && !self.rows.is_empty() {
            let first = self.rows.iter().position(|r| matches!(r, Row::Set { .. })).unwrap_or(0);
            self.workout_list.select(first);
        }
    }

    fn after_view(&mut self) {
        self.rebuild_rows();
        let (n_home, n_journal, n_records, n_meas) = match &self.view {
            Some(v) => (
                v.state.routines.len() + v.state.history.len().min(3),
                v.state.history.len(),
                v.records.len(),
                v.state.measurements.len(),
            ),
            None => (0, 0, 0, 0),
        };
        for (l, n) in [
            (&mut self.home_list, n_home),
            (&mut self.journal_list, n_journal),
            (&mut self.records_list, n_records),
            (&mut self.measurements_list, n_meas),
        ] {
            // Enter acts on the selected row, so a list with rows keeps one.
            l.invalidate();
            match l.selected {
                None if n > 0 => l.select(0),
                Some(i) if i >= n && n > 0 => l.select(n - 1),
                _ => {}
            }
        }
        self.sync_draft();
        self.sync_notes();
    }

    pub fn selected_set(&self) -> Option<(String, String, usize)> {
        match self.workout_list.selected.and_then(|i| self.rows.get(i)) {
            Some(Row::Set { item, set, index }) => Some((item.clone(), set.clone(), *index)),
            _ => None,
        }
    }

    pub fn find_set(&self, id: &str) -> Option<(&WorkoutSet, ExerciseKind)> {
        let w = self.active()?;
        for item in &w.exercises {
            if let Some(s) = item.sets.iter().find(|s| s.id == id) {
                let kind = self.exercise(&item.exercise_id).map_or(ExerciseKind::WeightReps, |e| e.kind.clone());
                return Some((s, kind));
            }
        }
        None
    }

    /// The server's text for one of a set's fields.
    pub fn server_field(&self, s: &WorkoutSet, f: Field) -> String {
        match f {
            Field::Weight => {
                self.view.as_ref().and_then(|v| v.weight_labels.get(&s.id)).cloned().unwrap_or_else(|| "0".into())
            }
            Field::Reps => s.reps.to_string(),
            Field::Seconds => s.seconds.to_string(),
            Field::Distance => s.distance_meters.to_string(),
        }
    }

    /// The draft follows the selected set, and the server's copy unless something is typed.
    pub fn sync_draft(&mut self) {
        let Some((_, id, _)) = self.selected_set() else {
            self.commit_draft();
            self.draft = None;
            return;
        };
        if self.draft.as_ref().is_some_and(|d| d.set_id != id) {
            self.commit_draft();
            self.draft = None;
        }
        let Some((set, _)) = self.find_set(&id).map(|(s, k)| (s.clone(), k)) else {
            self.draft = None;
            return;
        };
        let values = [Field::Weight, Field::Reps, Field::Seconds, Field::Distance].map(|f| self.server_field(&set, f));
        match &mut self.draft {
            None => {
                self.draft = Some(Draft {
                    set_id: id,
                    weight: TextEdit::with_text(&values[0]),
                    reps: TextEdit::with_text(&values[1]),
                    seconds: TextEdit::with_text(&values[2]),
                    distance: TextEdit::with_text(&values[3]),
                    error: None,
                });
            }
            Some(_) => {
                let sent = self.sent.contains_key(&id);
                if !self.draft_dirty() && !sent {
                    if let Some(d) = &mut self.draft {
                        // Only what differs: setting text drops the selection made on the way in.
                        for (e, v) in [
                            (&mut d.weight, &values[0]),
                            (&mut d.reps, &values[1]),
                            (&mut d.seconds, &values[2]),
                            (&mut d.distance, &values[3]),
                        ] {
                            if e.text != *v {
                                e.set(v);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn draft_dirty(&self) -> bool {
        let Some(d) = &self.draft else { return false };
        let Some((set, kind)) = self.find_set(&d.set_id) else { return false };
        fields_for(&kind).iter().any(|f| {
            let text = match f {
                Field::Weight => &d.weight.text,
                Field::Reps => &d.reps.text,
                Field::Seconds => &d.seconds.text,
                Field::Distance => &d.distance.text,
            };
            text.trim() != self.server_field(set, *f)
        })
    }

    /// `buildEdit`: the draft as an `edit_set`, or the reason it is not one.
    fn build_edit(&mut self, new_kind: Option<SetKind>) -> Option<Value> {
        let d = self.draft.as_ref()?;
        let id = d.set_id.clone();
        let (set, kind) = self.find_set(&id).map(|(s, k)| (s.clone(), k))?;
        let d = self.draft.as_ref()?;
        let fields = fields_for(&kind);
        let whole = |t: &TextEdit, name: &str| -> Result<u64, String> {
            t.text.trim().parse::<u64>().map_err(|_| format!("{name} must be a whole number."))
        };
        let has = |f: Field| fields.contains(&f);
        let mut err = None;
        let mut value = |f: Field, t: &TextEdit, name: &str, old: u64| {
            if !has(f) {
                return old;
            }
            whole(t, name).unwrap_or_else(|e| {
                err = Some(e);
                old
            })
        };
        let reps = value(Field::Reps, &d.reps, "Reps", set.reps);
        let seconds = value(Field::Seconds, &d.seconds, "Seconds", set.seconds);
        let distance = value(Field::Distance, &d.distance, "Meters", set.distance_meters);
        let weight = if has(Field::Weight) { d.weight.text.trim().to_owned() } else { "0".into() };
        if let Some(e) = err {
            if let Some(d) = &mut self.draft {
                d.error = Some(e);
            }
            return None;
        }
        self.sent.insert(id.clone(), (weight.clone(), reps.to_string(), seconds.to_string(), distance.to_string()));
        if let Some(d) = &mut self.draft {
            d.error = None;
        }
        Some(json!({
            "type": "edit_set",
            "set_id": id,
            "weight": weight,
            "reps": reps,
            "seconds": seconds,
            "distance_meters": distance,
            "kind": set_kind_name(&new_kind.unwrap_or(set.kind.clone())),
        }))
    }

    fn draft_matches(&self, v: &(String, String, String, String)) -> bool {
        self.draft.as_ref().is_some_and(|d| {
            d.weight.text.trim() == v.0
                && d.reps.text.trim() == v.1
                && d.seconds.text.trim() == v.2
                && d.distance.text.trim() == v.3
        })
    }

    /// `commit`: what was typed goes out when the set is left. A completed set is not edited.
    pub fn commit_draft(&mut self) {
        let Some(id) = self.draft.as_ref().map(|d| d.set_id.clone()) else { return };
        if self.find_set(&id).is_none_or(|(s, _)| s.completed) || !self.draft_dirty() {
            return;
        }
        // What is on its way already, or a completion that carries it: never twice, since the set may be done by then.
        if self.done_sent.contains_key(&id) || self.sent.get(&id).is_some_and(|v| self.draft_matches(v)) {
            return;
        }
        if let Some(edit) = self.build_edit(None) {
            self.run(Tag::Edit(id), edit);
        }
    }

    pub fn change_set_kind(&mut self, k: SetKind) {
        let Some(id) = self.draft.as_ref().map(|d| d.set_id.clone()) else { return };
        if let Some(edit) = self.build_edit(Some(k)) {
            self.run(Tag::Edit(id), edit);
        }
    }

    /// `toggleDone`: what was typed is saved first, and a failure there stops it.
    pub fn toggle_done(&mut self) {
        let Some(id) = self.draft.as_ref().map(|d| d.set_id.clone()) else { return };
        let Some(completed) = self.find_set(&id).map(|(s, _)| s.completed) else { return };
        let complete = json!({"type": "complete_set", "set_id": id, "completed": !completed});
        self.done_sent.insert(id.clone(), !completed);
        self.action_error = None;
        if !completed && self.draft_dirty() {
            let Some(edit) = self.build_edit(None) else {
                self.done_sent.remove(&id);
                return;
            };
            self.send(Job::Sequence(Tag::Complete(id), vec![edit, complete]));
        } else {
            self.send(Job::Run(Tag::Complete(id), complete));
        }
    }

    /// Enter on the last field: done, and on to the next set of the workout.
    pub fn complete_and_next(&mut self) {
        let Some((_, id, _)) = self.selected_set() else { return };
        let completed = self.find_set(&id).is_some_and(|(s, _)| s.completed);
        if !completed {
            self.toggle_done();
        }
        if self.draft.as_ref().is_some_and(|d| d.error.is_some()) {
            return;
        }
        let at = self.workout_list.selected.unwrap_or(0);
        let next = self.rows.iter().enumerate().skip(at + 1).find_map(|(i, r)| match r {
            Row::Set { set, .. } if self.find_set(set).is_some_and(|(s, _)| !s.completed) => Some(i),
            _ => None,
        });
        match next {
            Some(i) => {
                self.workout_list.select(i);
                self.sync_draft();
                self.enter_set();
            }
            None => self.focus_next = Some("rows"),
        }
    }

    /// The cursor to the set's first field, with what is there selected: typing replaces it.
    pub fn enter_set(&mut self) {
        if let Some(d) = &mut self.draft {
            for e in [&mut d.weight, &mut d.reps, &mut d.seconds, &mut d.distance] {
                e.select_all();
            }
        }
        self.focus_next = Some("set-first");
    }

    pub fn remove_set(&mut self) {
        let Some((_, id, _)) = self.selected_set() else { return };
        self.draft = None;
        self.run(Tag::Other, json!({"type": "remove_set", "set_id": id}));
    }

    pub fn add_set(&mut self, item: &str) {
        self.run(Tag::Other, json!({"type": "add_set", "exercise_id": item}));
    }

    // Notes: the selected exercise's, or the workout's.

    pub fn notes_target(&self) -> Option<String> {
        match self.workout_list.selected.and_then(|i| self.rows.get(i)) {
            Some(Row::Exercise(id) | Row::AddSet(id)) => Some(id.clone()),
            Some(Row::Set { item, .. }) => Some(item.clone()),
            _ => None,
        }
    }

    fn server_notes(&self, target: Option<&str>) -> String {
        let Some(w) = self.active() else { return String::new() };
        match target {
            Some(id) => w.exercises.iter().find(|e| e.id == id).map_or_else(String::new, |e| e.notes.clone()),
            None => w.notes.clone(),
        }
    }

    pub fn commit_notes(&mut self) {
        let Some(target) = self.notes_for.clone() else { return };
        let target = (!target.is_empty()).then_some(target);
        if self.active().is_none() {
            return;
        }
        let text = self.notes.text.clone();
        if text == self.server_notes(target.as_deref()) {
            return;
        }
        let mut cmd = json!({"type": "set_notes", "notes": text});
        if let Some(id) = target {
            cmd["exercise_id"] = json!(id);
        }
        self.run(Tag::Other, cmd);
    }

    pub fn sync_notes(&mut self) {
        let target = self.notes_target().unwrap_or_default();
        if self.notes_for.as_deref() != Some(target.as_str()) {
            self.commit_notes();
            let t = (!target.is_empty()).then_some(target.as_str());
            let text = self.server_notes(t);
            self.notes.set(&text);
            self.notes_for = Some(target);
        }
    }

    // The workout as a whole.

    pub fn start_rest(&mut self) {
        let Some(seconds) = self.view.as_ref().map(|v| v.state.settings.rest_seconds) else { return };
        self.run(Tag::Other, json!({"type": "start_rest", "seconds": seconds}));
    }

    pub fn stop_rest(&mut self) {
        self.run(Tag::Other, json!({"type": "stop_rest"}));
    }

    pub fn rename(&mut self) {
        let Some(t) = &self.title else { return };
        let title = t.text.clone();
        self.run(Tag::Rename, json!({"type": "rename_workout", "title": title}));
    }

    pub fn finish(&mut self) {
        if self.busy {
            return;
        }
        self.commit_draft();
        self.commit_notes();
        self.run(Tag::Finish, json!({"type": "finish_workout"}));
    }

    pub fn save_routine_from_active(&mut self) {
        let Some(n) = &self.routine_name else { return };
        let name = n.text.clone();
        self.run(Tag::SaveRoutineFromActive, json!({"type": "save_routine", "name": name, "from_active": true}));
    }

    pub fn confirmed(&mut self) {
        match self.confirm.take() {
            Some(Confirm::Discard) => self.run(Tag::Discard, json!({"type": "cancel_workout"})),
            Some(Confirm::DeleteWorkout(id)) => {
                self.run(Tag::DeleteWorkout, json!({"type": "delete_workout", "workout_id": id}));
            }
            Some(Confirm::DeleteRoutine(id, _)) => {
                self.run(Tag::Other, json!({"type": "delete_routine", "routine_id": id}));
            }
            Some(Confirm::RemoveExercise(id, _)) => {
                self.run(Tag::Other, json!({"type": "remove_exercise", "exercise_id": id}));
            }
            Some(Confirm::DeleteMeasurement(id, _)) => {
                self.run(Tag::Other, json!({"type": "delete_measurement", "measurement_id": id}));
            }
            None => {}
        }
    }

    // The picker.

    pub fn open_picker(&mut self, purpose: PickFor) {
        self.picker = Some(Picker {
            purpose,
            query: TextEdit::new().placeholder("Name, muscle, or equipment"),
            muscle: None,
            list: ListState::new(),
            first: true,
            create: None,
        });
    }

    /// `catalog` filtered by the search and the muscle.
    pub fn picker_rows(&self) -> Vec<Exercise> {
        let (Some(p), Some(v)) = (&self.picker, &self.view) else { return Vec::new() };
        let q = p.query.text.trim().to_lowercase();
        let mut rows: Vec<Exercise> = v
            .state
            .exercises
            .iter()
            .filter(|e| {
                (q.is_empty() || format!("{} {} {}", e.name, e.muscle, e.equipment).to_lowercase().contains(&q))
                    && p.muscle.as_ref().is_none_or(|m| *m == e.muscle)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|e| e.name.to_lowercase());
        rows
    }

    pub fn muscles(&self) -> Vec<String> {
        let Some(v) = &self.view else { return Vec::new() };
        let mut m: Vec<String> = v.state.exercises.iter().map(|e| e.muscle.clone()).collect();
        m.sort_by_key(|s| s.to_lowercase());
        m.dedup();
        m
    }

    /// In use already: the workout's exercises, or those in the routine being built.
    pub fn picker_added(&self, id: &str) -> bool {
        match self.picker.as_ref().map(|p| p.purpose) {
            Some(PickFor::Routine) => self.builder.as_ref().is_some_and(|b| b.ids.iter().any(|x| x == id)),
            _ => self.active().is_some_and(|w| w.exercises.iter().any(|e| e.exercise_id == id)),
        }
    }

    pub fn pick(&mut self, e: &Exercise) {
        if self.picker_added(&e.id) {
            return;
        }
        match self.picker.as_ref().map(|p| p.purpose) {
            Some(PickFor::Routine) => {
                if let Some(b) = &mut self.builder
                    && b.ids.len() < MAX_ROUTINE_EXERCISES
                {
                    b.ids.push(e.id.clone());
                    b.list.invalidate();
                    b.list.select(b.ids.len() - 1);
                    b.error = None;
                }
            }
            _ => self.run(Tag::Other, json!({"type": "add_exercise", "exercise_id": e.id})),
        }
        self.picker = None;
        if self.screen == Screen::Workout {
            self.focus_next = Some("rows");
        }
    }

    pub fn save_exercise(&mut self) {
        let Some(c) = self.picker.as_mut().and_then(|p| p.create.as_mut()) else { return };
        let name = c.name.text.trim().to_owned();
        let cmd = json!({"type": "create_exercise", "name": name, "muscle": c.muscle.text.trim(),
            "equipment": c.equipment.text.trim(), "kind": kind_name(&c.kind)});
        self.run(Tag::CreateExercise(name), cmd);
    }

    // The routine builder.

    pub fn open_builder(&mut self) {
        self.builder = Some(Builder {
            name: TextEdit::new(),
            ids: Vec::new(),
            list: ListState::new(),
            error: None,
            first: true,
        });
    }

    pub fn builder_move(&mut self, by: i32) {
        let Some(b) = &mut self.builder else { return };
        let Some(i) = b.list.selected else { return };
        let to = i as i32 + by;
        if to < 0 || to as usize >= b.ids.len() {
            return;
        }
        b.ids.swap(i, to as usize);
        b.list.invalidate();
        b.list.select(to as usize);
    }

    pub fn builder_remove(&mut self) {
        let Some(b) = &mut self.builder else { return };
        let Some(i) = b.list.selected else { return };
        if i < b.ids.len() {
            b.ids.remove(i);
            b.list.invalidate();
            if !b.ids.is_empty() {
                b.list.select(i.min(b.ids.len() - 1));
            }
        }
    }

    pub fn save_builder(&mut self) {
        let Some(b) = &mut self.builder else { return };
        let name = b.name.text.trim().to_owned();
        if name.is_empty() {
            b.error = Some("Give the routine a name.".into());
            return;
        }
        if b.ids.is_empty() {
            b.error = Some("Add at least one exercise.".into());
            return;
        }
        b.error = None;
        let ids = b.ids.clone();
        self.run(Tag::SaveRoutine, json!({"type": "save_routine", "name": name, "exercise_ids": ids}));
    }

    // Settings and measurements.

    pub fn open_settings(&mut self) {
        let Some(v) = &self.view else { return };
        let s = &v.state.settings;
        self.settings = Some(SettingsForm {
            unit: s.weight_unit.clone(),
            rest: s.rest_seconds,
            goal: s.weekly_goal,
            saved: false,
        });
    }

    /// A rest time set elsewhere (75 s, say) stays selectable.
    pub fn rest_options(&self) -> Vec<u64> {
        let mut r = REST_PRESETS.to_vec();
        if let Some(c) = self.view.as_ref().map(|v| v.state.settings.rest_seconds)
            && !r.contains(&c)
        {
            r.push(c);
            r.sort_unstable();
        }
        r
    }

    pub fn save_settings(&mut self) {
        let Some(s) = &mut self.settings else { return };
        s.saved = false;
        let cmd = json!({"type": "update_settings", "weight_unit": s.unit, "rest_seconds": s.rest, "weekly_goal": s.goal});
        self.run(Tag::Settings, cmd);
    }

    pub fn open_measurement(&mut self) {
        self.measurement =
            Some(MeasurementForm { kind: "waist", value: TextEdit::new(), error: None, first: true });
    }

    pub fn save_measurement(&mut self) {
        let Some(m) = &mut self.measurement else { return };
        let value = m.value.text.trim().to_owned();
        if value.is_empty() {
            m.error = Some("Enter a value.".into());
            return;
        }
        m.error = None;
        let kind = m.kind;
        self.run(Tag::Measurement, json!({"type": "record_measurement", "kind": kind, "value": value}));
    }

    // The clock.

    /// Whether anything on screen counts seconds, so that nothing wakes the machine when nothing does.
    pub fn counting(&self) -> bool {
        (self.screen == Screen::Workout && self.active().is_some())
            || self.clock.as_ref().is_some_and(|c| c.rest_now() > 0)
    }

    pub fn tick(&mut self) {
        if self.counting() && self.last_clock.is_none_or(|t| t.elapsed() >= CLOCK_POLL) {
            self.last_clock = Some(Instant::now());
            self.send(Job::Clock);
        }
    }

    fn signed_out(&mut self) {
        crate::account::rejected();
        self.link = None;
        self.view = None;
        self.body = None;
        self.clock = None;
        self.draft = None;
        self.confirm = None;
        self.picker = None;
        self.builder = None;
        self.measurement = None;
        self.busy = false;
        self.sign_in = self.start_sign_in.as_mut().and_then(|f| f());
    }

    fn signed_in(&mut self) {
        self.sign_in = None;
        self.link = self.start_worker.as_mut().and_then(|f| f());
        self.refresh();
    }

    fn done(&mut self, tag: Tag, result: Result<(), String>) {
        let ok = result.is_ok();
        if let Err(m) = &result {
            // A form still open says it there; everything else in the banner.
            if let Tag::CreateExercise(_) = tag
                && let Some(c) = self.picker.as_mut().and_then(|p| p.create.as_mut())
            {
                c.error = Some(m.clone());
            } else if let (Tag::SaveRoutine, Some(b)) = (&tag, &mut self.builder) {
                b.error = Some(m.clone());
            } else if let (Tag::Measurement, Some(f)) = (&tag, &mut self.measurement) {
                f.error = Some(m.clone());
            } else {
                self.action_error = Some(m.clone());
            }
        }
        match tag {
            Tag::Start => {
                self.busy = false;
                if ok {
                    self.go(Screen::Workout);
                    self.workout_list = ListState::new();
                    self.rows.clear();
                    self.after_view();
                    self.focus_next = Some("rows");
                }
            }
            Tag::Finish => {
                self.busy = false;
                if ok {
                    self.draft = None;
                    self.notes_for = None;
                    // `finish`: to the journal entry just made.
                    let newest = self.view.as_ref().and_then(|v| v.state.history.first()).map(|w| w.id.clone());
                    self.focus_next = Some(if newest.is_some() { "back" } else { "journal" });
                    self.screen = newest.map_or(Screen::Journal, Screen::Detail);
                    self.after_view();
                }
            }
            Tag::Discard => {
                self.busy = false;
                if ok {
                    self.draft = None;
                    self.notes_for = None;
                    self.screen = Screen::Home;
                    self.focus_next = Some("home");
                    self.after_view();
                }
            }
            Tag::DeleteWorkout => {
                self.busy = false;
                if ok {
                    self.screen = Screen::Journal;
                    self.focus_next = Some("journal");
                }
            }
            Tag::SaveRoutine => {
                if ok {
                    self.builder = None;
                }
            }
            Tag::SaveRoutineFromActive => {
                if ok {
                    self.routine_name = None;
                    self.routine_saved = true;
                }
            }
            Tag::CreateExercise(name) => {
                if ok {
                    // `onDone`: back to the list, searching for the new one.
                    if let Some(p) = &mut self.picker {
                        p.create = None;
                        p.query.set(&name);
                        p.first = true;
                        p.list = ListState::new();
                        p.list.select(0);
                    }
                }
            }
            Tag::Edit(id) => {
                if ok {
                    self.sent.remove(&id);
                }
            }
            Tag::Complete(id) => {
                self.done_sent.remove(&id);
                if ok {
                    self.sent.remove(&id);
                }
            }
            Tag::Settings => {
                if ok && let Some(s) = &mut self.settings {
                    s.saved = true;
                }
            }
            Tag::Measurement => {
                if ok {
                    self.measurement = None;
                }
            }
            Tag::Rename => {
                if ok {
                    self.title = None;
                }
            }
            Tag::Other => {}
        }
    }
}

impl App for Training {
    type Msg = Msg;

    fn view(&mut self, ui: &mut Ui) {
        ui.header("Training");
        if let Some(v) = &mut self.sign_in {
            v.view(ui, "Your training waits in your account. This laptop asks to be added as a device.");
            return;
        }
        if let Some((picker, pick)) = &mut self.data_picker {
            let pick = *pick;
            ui.pad(14.0);
            if let Some(p) = picker.view(ui) {
                self.data_picked(pick, p);
            }
            return;
        }
        crate::screens::view(self, ui);
        self.tick();
        if self.last_load.is_none_or(|t| t.elapsed() >= REFRESH) {
            self.refresh();
        }
        // The list's selection may have moved: the draft and the notes follow it.
        self.sync_draft();
        self.sync_notes();
        ui.wake_after(if self.counting() { Duration::from_secs(1) } else { REFRESH });
    }

    fn message(&mut self, msg: Msg) {
        match msg {
            Msg::Changed => {}
            Msg::SignedIn => self.signed_in(),
            Msg::SignedOut => self.signed_out(),
            Msg::LoadFailed(e) => self.load_error = Some(e),
            Msg::Done(tag, result) => self.done(tag, result),
            Msg::Data(r) => {
                self.data_status = Some(match r {
                    Ok(m) => (true, m),
                    Err(m) => (false, m),
                });
            }
            Msg::BackupRead(r) => match r {
                Ok(backup) => self.pending_backup = Some(backup),
                Err(m) => self.data_status = Some((false, m)),
            },
            Msg::Body(v) => self.body = Some(*v),
            Msg::Clock(c) => {
                self.clock = Some(ClockState {
                    rest_remaining: c.rest_remaining,
                    elapsed: c.active_elapsed_seconds,
                    received: Instant::now(),
                });
            }
            Msg::View(v) => {
                self.load_error = None;
                self.clock = Some(ClockState {
                    rest_remaining: v.rest_remaining,
                    elapsed: v.summary.active_elapsed_seconds,
                    received: Instant::now(),
                });
                self.view = Some(*v);
                if let (Some(save), Some(v)) = (&mut self.save_cache, &self.view) {
                    save(v);
                }
                self.after_view();
            }
        }
    }
}
