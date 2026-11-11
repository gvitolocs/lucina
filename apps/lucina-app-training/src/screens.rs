//! What Training draws: the sections on the left, one screen beside them, the dialogs on top.

use lucina_app::{Face, ListState, Rect, Rgb, TextEdit, Ui, keys};
use secretary_gym::{ChartPoint, ExerciseKind, SetKind};

use crate::app::*;

pub fn view(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    if a.view.is_none() {
        ui.pad(20.0);
        let r = ui.area();
        let mut retry = false;
        ui.within(r, |ui| match &a.load_error {
            Some(e) => {
                ui.wrapped(Face::Sans, t.pt(16.0), t.danger, e);
                ui.space(8.0);
                retry = ui.button("Try again");
            }
            None => ui.dim("Loading your training…"),
        });
        if retry {
            a.refresh();
        }
        return;
    }
    let rail = ui.cut_left(t.px(210.0));
    sections(a, ui, rail);
    ui.pad(16.0);
    if let Some(e) = a.action_error.clone() {
        let r = ui.cut_top(t.px(34.0));
        ui.fill_round(r, t.px(6.0), t.panel_hi);
        ui.text_in(Rect::new(r.x + t.px(10.0), r.y, r.w - t.px(20.0), r.h), Face::Sans, t.pt(14.0), t.danger, &e);
        ui.cut_top(t.px(8.0));
    } else if let Some(e) = &a.load_error {
        let r = ui.cut_top(t.px(24.0));
        ui.text_in(r, Face::Sans, t.pt(13.0), t.dim, &format!("{e} Showing the last copy."));
    }
    // Before the screen is drawn, so that it is drawn with the focus.
    if let Some(id @ ("home" | "journal" | "records" | "back")) = a.focus_next {
        a.focus_next = None;
        ui.focus(id);
    }
    match a.screen.clone() {
        Screen::Home => home(a, ui),
        Screen::Workout => workout(a, ui),
        Screen::Journal => journal(a, ui),
        Screen::Detail(id) => detail(a, ui, &id),
        Screen::Progress => progress(a, ui),
        Screen::Settings => settings(a, ui),
    }
    builder_modal(a, ui);
    picker_modal(a, ui);
    measurement_modal(a, ui);
    confirm_modal(a, ui);
}

fn sections(a: &mut Training, ui: &mut Ui, r: Rect) {
    let t = ui.theme();
    ui.fill(r, t.bar);
    let mut go = None;
    ui.within(r.inset(t.px(12.0)), |ui| {
        if a.sample {
            ui.dim("Sample data");
            ui.space(6.0);
        }
        let active = a.active().is_some();
        let entries: &[(&str, Screen)] = &[
            ("Training", Screen::Home),
            ("Journal", Screen::Journal),
            ("Progress", Screen::Progress),
            ("Settings", Screen::Settings),
        ];
        if active {
            let on = a.screen == Screen::Workout;
            let hit = if on { ui.primary_button("Workout") } else { ui.button_id("nav-workout", "Workout") };
            if hit {
                go = Some(Screen::Workout);
            }
            ui.space(4.0);
        }
        for (i, (label, s)) in entries.iter().enumerate() {
            let on = a.screen == *s || (matches!(a.screen, Screen::Detail(_)) && *s == Screen::Journal);
            let hit = if on { ui.primary_button(label) } else { ui.button_id(("nav", i), label) };
            if hit {
                go = Some(s.clone());
            }
            ui.space(4.0);
        }
    });
    if let Some(s) = go {
        a.go(s);
    }
}

fn tile(ui: &mut Ui, r: Rect, value: &str, label: &str) {
    let t = ui.theme();
    ui.fill_round(r, t.px(10.0), t.panel);
    let inner = r.inset(t.px(10.0));
    let top = Rect::new(inner.x, inner.y, inner.w, inner.h * 3 / 5);
    let bottom = Rect::new(inner.x, top.bottom(), inner.w, inner.h - top.h);
    ui.text_in(top, Face::Bold, t.pt(19.0), t.text, value);
    ui.text_in(bottom, Face::Sans, t.pt(12.0), t.dim, label);
}

fn tiles(ui: &mut Ui, r: Rect, items: &[(String, &str)]) {
    let t = ui.theme();
    let gap = t.px(10.0);
    let n = items.len() as i32;
    let w = (r.w - gap * (n - 1)) / n;
    for (i, (value, label)) in items.iter().enumerate() {
        tile(ui, Rect::new(r.x + (w + gap) * i as i32, r.y, w, r.h), value, label);
    }
}

// Home.

fn body_stat(a: &Training, key: &str) -> String {
    a.body.as_ref().and_then(|v| v.stat_labels.get(key)).cloned().unwrap_or_else(|| "—".to_owned())
}

fn goal_distance(a: &Training) -> String {
    let Some(v) = &a.body else { return "—".into() };
    if v.stats.goal_grams.is_none() {
        return "No goal".into();
    }
    if v.stats.goal_reached == Some(true) {
        return "Goal reached".into();
    }
    body_stat(a, "goal_distance")
}

fn home(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    let Some(summary) = a.view.as_ref().map(|v| {
        (
            v.summary.weekly_workouts,
            v.summary.weekly_goal,
            v.summary.workouts,
            v.summary.completed_sets,
            v.summary.volume.clone(),
            v.summary.active_completed_sets,
            v.summary.active_volume.clone(),
        )
    }) else {
        return;
    };
    // The owner wants Training and Weight together (`Training.tsx`): the weight on top. Logging is in Weight.
    if a.body.as_ref().is_some_and(|b| b.stats.count > 0) {
        let r = ui.cut_top(t.px(72.0));
        tiles(ui, r, &[
            (body_stat(a, "current"), "Weight"),
            (body_stat(a, "avg7"), "7-day average"),
            (goal_distance(a), "Goal distance"),
        ]);
        ui.cut_top(t.px(10.0));
    }
    let week = ui.cut_top(t.px(66.0));
    ui.fill_round(week, t.px(10.0), t.panel);
    let inner = week.inset(t.px(10.0));
    let head = format!("This week: {} / {} workouts", summary.0, summary.1);
    ui.text_in(Rect::new(inner.x, inner.y, inner.w, t.px(26.0)), Face::Bold, t.pt(16.0), t.text, &head);
    let bar = Rect::new(inner.x, inner.bottom() - t.px(10.0), inner.w, t.px(10.0));
    ui.fill_round(bar, t.px(5.0), t.panel_hi);
    let goal = summary.1.max(1);
    let filled = (bar.w as u64 * (summary.0 as u64).min(goal) / goal) as i32;
    if filled > 0 {
        ui.fill_round(Rect::new(bar.x, bar.y, filled, bar.h), t.px(5.0), t.accent);
    }
    ui.cut_top(t.px(10.0));

    let row = ui.cut_top(t.px(44.0));
    let active_title = a.active().map(|w| w.title.clone());
    let mut do_start = false;
    let mut do_continue = false;
    let mut do_build = false;
    ui.within(row, |ui| {
        ui.row(40.0, |ui| {
            if active_title.is_some() {
                do_continue = ui.primary_button("Continue workout");
            } else {
                do_start = ui.primary_button("Start empty workout");
            }
            do_build = ui.button("Create routine");
        });
    });
    if let Some(title) = &active_title {
        let line = ui.cut_top(t.px(26.0));
        let elapsed = a.clock.as_ref().map_or(0, ClockState::elapsed_now);
        let s = format!(
            "In progress: {title} · {} · {} · {}",
            format_duration(elapsed),
            sets_text(summary.5),
            summary.6
        );
        ui.text_in(line, Face::Sans, t.pt(14.0), t.accent, &s);
    }
    ui.cut_top(t.px(6.0));

    let totals = ui.cut_bottom(t.px(64.0));
    tiles(ui, totals, &[
        (summary.2.to_string(), "Sessions"),
        (summary.3.to_string(), "Sets logged"),
        (summary.4.clone(), "Total volume"),
    ]);
    ui.cut_bottom(t.px(8.0));

    let r = ui.area();
    let Some(v) = a.view.as_ref() else { return };
    let routines = &v.state.routines;
    let recent: Vec<_> = if active_title.is_none() { v.state.history.iter().take(3).collect() } else { Vec::new() };
    let n = routines.len() + recent.len();
    let mut activated = None;
    let mut secondary = None;
    if n == 0 {
        ui.within(r, |ui| ui.dim("Create a routine, or save a workout as one, to start it here."));
    } else {
        let exercises = &v.state.exercises;
        let labels = &v.workout_labels;
        let tz = a.tz;
        let res = ui.list("home", &mut a.home_list, r, n, 58.0, |ui, i, selected| {
            let row = ui.area();
            if selected {
                ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
            }
            let x = row.x + t.px(10.0);
            let top = Rect::new(x, row.y + t.px(6.0), row.w - t.px(20.0), t.px(24.0));
            let bottom = Rect::new(x, row.y + t.px(30.0), row.w - t.px(20.0), t.px(20.0));
            if let Some(routine) = routines.get(i) {
                ui.text_in(top, Face::Bold, t.pt(15.0), t.text, &format!("Start {}", routine.name));
                let names: Vec<&str> = routine
                    .exercises
                    .iter()
                    .filter_map(|re| exercises.iter().find(|e| e.id == re.exercise_id).map(|e| e.name.as_str()))
                    .collect();
                ui.text_in(bottom, Face::Sans, t.pt(13.0), t.dim, &names.join(" · "));
            } else if let Some(w) = recent.get(i - routines.len()) {
                ui.text_in(top, Face::Bold, t.pt(15.0), t.text, &format!("Repeat {}", w.title));
                let date = format_date(w.finished_ms.unwrap_or(w.started_ms), tz);
                let line = labels.get(&w.id).map_or(date.clone(), |l| {
                    format!("{date} · {} · {} · {}", l.duration, sets_text(l.sets), l.volume)
                });
                ui.text_in(bottom, Face::Sans, t.pt(13.0), t.dim, &line);
            }
        });
        activated = res.activated;
        secondary = res.secondary;
        // Delete acts on the selected routine, from the keyboard too.
        if ui.is_focused("home") && ui.key(keys::KEY_DELETE) {
            secondary = a.home_list.selected;
        }
    }
    let routine = activated.or(secondary).and_then(|i| routines.get(i)).map(|r| (r.id.clone(), r.name.clone()));
    let repeat = activated.and_then(|i| recent.get(i.wrapping_sub(routines.len())).map(|w| w.id.clone()));
    let was_active = active_title.is_some();
    if let (Some(_), Some((id, name))) = (secondary, routine.clone()) {
        a.confirm = Some(Confirm::DeleteRoutine(id, name));
    } else if let (Some(_), Some((id, name))) = (activated, routine) {
        if was_active {
            a.action_error = Some("Finish or discard your current workout before starting another.".into());
        } else {
            a.start_routine(&id, &name);
        }
    } else if let Some(id) = repeat {
        a.repeat(&id);
    }
    if do_start {
        a.start_empty();
    }
    if do_continue {
        a.go(Screen::Workout);
    }
    if do_build {
        a.open_builder();
    }
}

// The workout.

fn workout(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    let Some(w) = a.active().cloned() else {
        let r = ui.area();
        let mut back = false;
        ui.within(r, |ui| {
            ui.dim("No workout in progress.");
            ui.space(8.0);
            back = ui.button("Back to Training");
        });
        if back {
            a.go(Screen::Home);
        }
        return;
    };
    let Some((volume, done_sets, rest_seconds)) =
        a.view.as_ref().map(|v| (v.summary.active_volume.clone(), v.summary.active_completed_sets, v.state.settings.rest_seconds))
    else {
        return;
    };
    let top = ui.cut_top(t.px(40.0));
    let mut do_rename = false;
    let mut do_rest = false;
    let mut do_skip = false;
    let rest = a.clock.as_ref().map_or(0, ClockState::rest_now);
    let elapsed = a.clock.as_ref().map_or(0, ClockState::elapsed_now);
    if a.title.is_some() {
        let mut save = false;
        let mut cancel = ui.is_focused("title") && ui.key(keys::KEY_ESC);
        ui.within(top, |ui| {
            let tw = ui.area().w - t.px(220.0);
            let field = Rect::new(ui.area().x, ui.area().y, tw, t.px(38.0));
            if let Some(title) = &mut a.title
                && ui.text_field_at(field, "title", title).submitted
            {
                save = true;
            }
            let b1 = Rect::new(field.right() + t.px(8.0), field.y, t.px(100.0), field.h);
            save |= ui.button_at(b1, "title-save", "Save");
            let b2 = Rect::new(b1.right() + t.px(8.0), field.y, t.px(100.0), field.h);
            cancel |= ui.button_at(b2, "title-cancel", "Cancel");
        });
        if save {
            a.rename();
        }
        if cancel {
            a.title = None;
        }
    } else {
        ui.text_in(top, Face::Bold, t.pt(20.0), t.text, &w.title);
        let b = Rect::new(top.right() - t.px(100.0), top.y, t.px(100.0), t.px(36.0));
        do_rename = ui.button_at(b, "rename", "Rename");
    }
    ui.cut_top(t.px(6.0));
    let stats = ui.cut_top(t.px(64.0));
    let rest_text = if rest > 0 { format_countdown(rest) } else { "—".to_owned() };
    tiles(ui, stats, &[
        (format_duration(elapsed), "Duration"),
        (volume, "Volume"),
        (done_sets.to_string(), "Sets"),
        (rest_text, "Rest"),
    ]);
    ui.cut_top(t.px(8.0));

    let bottom = ui.cut_bottom(t.px(46.0));
    let mut do_finish = false;
    let mut do_discard = false;
    let mut do_save_routine = false;
    ui.within(bottom, |ui| {
        ui.row(40.0, |ui| {
            do_finish = ui.primary_button(if a.busy { "Finishing…" } else { "Finish workout" });
            if rest > 0 {
                do_skip = ui.button_id("skip", "Skip rest");
            } else {
                do_rest = ui.button_id("rest", &format!("Start rest {}", format_countdown(rest_seconds)));
            }
            do_save_routine = ui.button_id("save-routine", "Save as routine");
            do_discard = ui.button_id("discard", "Discard workout");
        });
    });
    ui.cut_bottom(t.px(6.0));
    let area = ui.area();
    let left_w = area.w * 55 / 100;
    let list_r = Rect::new(area.x, area.y, left_w, area.h);
    let pane = Rect::new(area.x + left_w + t.px(12.0), area.y, area.w - left_w - t.px(12.0), area.h);

    if a.focus_next == Some("rows") {
        a.focus_next = None;
        ui.focus("rows");
    }
    let rows = std::mem::take(&mut a.rows);
    let done_sent = std::mem::take(&mut a.done_sent);
    let sent = std::mem::take(&mut a.sent);
    let draft_id = a.draft.as_ref().map(|d| d.set_id.clone());
    let Some(view) = a.view.as_ref() else { return };
    let res = ui.list("rows", &mut a.workout_list, list_r, rows.len(), 44.0, |ui, i, selected| {
        let row = ui.area();
        if selected {
            ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
        }
        let x = row.x + t.px(10.0);
        let text = Rect::new(x, row.y, row.w - t.px(20.0), row.h);
        match &rows[i] {
            Row::Exercise(id) => {
                let name = w
                    .exercises
                    .iter()
                    .find(|e| e.id == *id)
                    .and_then(|e| view.state.exercises.iter().find(|c| c.id == e.exercise_id))
                    .map_or("Exercise", |c| c.name.as_str());
                ui.text_in(text, Face::Bold, t.pt(15.0), t.text, name);
            }
            Row::AddSet(_) => ui.text_in(Rect::new(x + t.px(30.0), row.y, text.w, row.h), Face::Sans, t.pt(14.0), t.dim, "+ Add set"),
            Row::AddExercise => ui.text_in(text, Face::Sans, t.pt(14.0), t.accent, "+ Add exercise"),
            Row::Set { item, set, index } => {
                let Some(s) = w.exercises.iter().find(|e| e.id == *item).and_then(|e| e.sets.iter().find(|x| x.id == *set)) else {
                    return;
                };
                let kind = w
                    .exercises
                    .iter()
                    .find(|e| e.id == *item)
                    .and_then(|e| view.state.exercises.iter().find(|c| c.id == e.exercise_id))
                    .map_or(ExerciseKind::WeightReps, |c| c.kind.clone());
                let done = done_sent.get(set).copied().unwrap_or(s.completed);
                let badge = Rect::new(x + t.px(8.0), row.y, t.px(30.0), row.h);
                let c = if s.kind == SetKind::Normal { t.dim } else { t.accent };
                ui.text_in(badge, Face::Bold, t.pt(14.0), c, &set_badge(&s.kind, *index));
                let unit = &view.state.settings.weight_unit;
                let (weight, reps, seconds, distance) = match sent.get(set) {
                    Some(v) => v.clone(),
                    None => (
                        view.weight_labels.get(set).cloned().unwrap_or_else(|| "0".into()),
                        s.reps.to_string(),
                        s.seconds.to_string(),
                        s.distance_meters.to_string(),
                    ),
                };
                let line = match kind {
                    ExerciseKind::WeightReps => format!("{weight} {unit} × {reps}"),
                    ExerciseKind::RepsOnly => format!("{reps} reps"),
                    ExerciseKind::Timed => format!("{seconds} s"),
                    ExerciseKind::DistanceTime => format!("{distance} m in {seconds} s"),
                };
                let colour = if done { t.ok } else { t.text };
                let value = Rect::new(badge.right(), row.y, row.w - t.px(120.0), row.h);
                ui.text_in(value, Face::Sans, t.pt(15.0), colour, &line);
                if done {
                    let tick = Rect::new(row.right() - t.px(50.0), row.y, t.px(40.0), row.h);
                    ui.text_in(tick, Face::Bold, t.pt(15.0), t.ok, "✓");
                }
            }
        }
    });
    let _ = draft_id;
    a.rows = rows;
    a.done_sent = done_sent;
    a.sent = sent;

    ui.fill_round(pane, t.px(10.0), t.bar);
    let selected = a.workout_list.selected.and_then(|i| a.rows.get(i)).cloned();
    if let Some(i) = res.activated {
        match a.rows.get(i).cloned() {
            Some(Row::Set { set, .. }) => {
                if a.find_set(&set).is_some_and(|(s, _)| s.completed) {
                    a.toggle_done();
                } else {
                    a.enter_set();
                }
            }
            Some(Row::AddSet(item)) => a.add_set(&item),
            Some(Row::AddExercise) => a.open_picker(PickFor::Workout),
            Some(Row::Exercise(_)) => a.focus_next = Some("notes"),
            None => {}
        }
    }
    ui.within(pane.inset(t.px(14.0)), |ui| match selected {
        Some(Row::Set { index, .. }) => set_pane(a, ui, index),
        Some(Row::Exercise(id)) => exercise_pane(a, ui, &id),
        Some(Row::AddSet(id)) => exercise_pane(a, ui, &id),
        _ => workout_pane(a, ui),
    });
    // Escape leaves a field for the list.
    if !ui.is_focused("rows") && a.confirm.is_none() && a.picker.is_none() && a.title.is_none() && a.routine_name.is_none() && ui.key(keys::KEY_ESC) {
        ui.focus("rows");
    }

    if a.routine_name.is_some() {
        let mut save = false;
        let mut cancel = ui.key(keys::KEY_ESC);
        ui.modal(460.0, 190.0, |ui| {
            ui.heading("Save as routine");
            ui.space(6.0);
            ui.focus("routine-name");
            if let Some(n) = &mut a.routine_name
                && ui.text_field("routine-name", n).submitted
            {
                save = true;
            }
            ui.space(8.0);
            ui.row(40.0, |ui| {
                cancel |= ui.button_id("routine-cancel", "Cancel");
                save |= ui.primary_button("Save");
            });
        });
        if save {
            a.save_routine_from_active();
        }
        if cancel {
            a.routine_name = None;
        }
    }

    if do_rename {
        a.title = Some(TextEdit::with_text(&w.title));
        ui.focus("title");
    }
    if do_rest {
        a.start_rest();
    }
    if do_skip {
        a.stop_rest();
    }
    if do_finish {
        a.finish();
    }
    if do_discard {
        a.confirm = Some(Confirm::Discard);
    }
    if do_save_routine {
        a.routine_saved = false;
        a.routine_name = Some(TextEdit::with_text(&w.title));
    }
}

fn set_pane(a: &mut Training, ui: &mut Ui, index: usize) {
    let t = ui.theme();
    let Some(id) = a.draft.as_ref().map(|d| d.set_id.clone()) else { return };
    let Some((set, kind)) = a.find_set(&id).map(|(s, k)| (s.clone(), k)) else { return };
    let unit = a.view.as_ref().map_or("kg".to_owned(), |v| v.state.settings.weight_unit.clone());
    ui.heading(&format!("Set {}", index + 1));
    let previous = a.view.as_ref().and_then(|v| {
        a.notes_target().and_then(|item| v.previous_labels.get(&item).cloned())
    });
    if let Some(p) = previous {
        ui.dim(&format!("Previous: {p}"));
    }
    ui.space(4.0);
    if set.completed {
        ui.wrapped(Face::Sans, t.pt(14.0), t.ok, "Done. Uncheck it to change it.");
        ui.space(6.0);
        if ui.primary_button("Uncheck set") {
            a.toggle_done();
            a.focus_next = Some("set-first");
        }
        return;
    }
    let fields = fields_for(&kind);
    if a.focus_next == Some("set-first") {
        a.focus_next = None;
        ui.focus(("set-field", 0));
    }
    if a.focus_next == Some("notes") {
        a.focus_next = None;
        ui.focus("notes");
    }
    let mut last_submitted = false;
    let mut next_field = None;
    if let Some(d) = &mut a.draft {
        for (i, f) in fields.iter().enumerate() {
            let (label, edit) = match f {
                Field::Weight => (format!("Weight ({unit})"), &mut d.weight),
                Field::Reps => ("Reps".to_owned(), &mut d.reps),
                Field::Seconds => ("Seconds".to_owned(), &mut d.seconds),
                Field::Distance => ("Meters".to_owned(), &mut d.distance),
            };
            ui.dim(&label);
            if ui.text_field(("set-field", i), edit).submitted {
                if i + 1 < fields.len() {
                    next_field = Some(i + 1);
                } else {
                    last_submitted = true;
                }
            }
            ui.space(2.0);
        }
        if let Some(e) = &d.error {
            ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
        }
        // On to the next field, with what is there selected: typing replaces it.
        if let Some(i) = next_field {
            match fields[i] {
                Field::Weight => d.weight.select_all(),
                Field::Reps => d.reps.select_all(),
                Field::Seconds => d.seconds.select_all(),
                Field::Distance => d.distance.select_all(),
            }
            ui.focus(("set-field", i));
        }
    }
    ui.space(6.0);
    let mut done = false;
    let mut remove = false;
    ui.row(40.0, |ui| {
        done = ui.primary_button("Complete set");
        remove = ui.button_id("remove-set", "Delete set");
    });
    ui.space(4.0);
    ui.dim("Type");
    let mut kind_pick = None;
    ui.row(36.0, |ui| {
        for (i, k) in [SetKind::Normal, SetKind::Warmup, SetKind::Drop, SetKind::Failure].into_iter().enumerate() {
            let label = set_kind_label(&k);
            let hit = if set.kind == k { ui.primary_button(label) } else { ui.button_id(("set-kind", i), label) };
            if hit && set.kind != k {
                kind_pick = Some(k);
            }
        }
    });
    if last_submitted || done {
        a.complete_and_next();
    }
    if remove {
        a.remove_set();
        ui.focus("rows");
    }
    if let Some(k) = kind_pick {
        a.change_set_kind(k);
    }
}

fn notes_field(a: &mut Training, ui: &mut Ui, label: &str) {
    ui.dim(label);
    ui.text_area("notes", &mut a.notes, 3);
}

fn exercise_pane(a: &mut Training, ui: &mut Ui, item: &str) {
    let Some(w) = a.active() else { return };
    let Some(e) = w.exercises.iter().find(|e| e.id == item) else { return };
    let exercise_id = e.exercise_id.clone();
    let name = a.exercise(&exercise_id).map_or("Exercise".to_owned(), |c| c.name.clone());
    let previous = a.view.as_ref().and_then(|v| v.previous_labels.get(item).cloned());
    ui.heading(&name);
    if let Some(p) = previous {
        ui.dim(&format!("Previous: {p}"));
    }
    ui.space(6.0);
    if a.focus_next == Some("notes") {
        a.focus_next = None;
        ui.focus("notes");
    }
    notes_field(a, ui, "Notes");
    ui.space(6.0);
    let mut add = false;
    let mut remove = false;
    ui.row(40.0, |ui| {
        add = ui.primary_button("Add set");
        remove = ui.button_id("remove-exercise", "Remove exercise");
    });
    if add {
        a.add_set(item);
    }
    if remove {
        a.confirm = Some(Confirm::RemoveExercise(item.to_owned(), name));
    }
}

fn workout_pane(a: &mut Training, ui: &mut Ui) {
    ui.heading("This workout");
    ui.space(6.0);
    notes_field(a, ui, "Workout notes");
    ui.space(6.0);
    if ui.primary_button("Add exercise") {
        a.open_picker(PickFor::Workout);
    }
    if a.routine_saved {
        ui.space(6.0);
        ui.dim("Saved to your routines.");
    }
}

// The journal.

fn journal(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    ui.heading("Journal");
    ui.space(6.0);
    let r = ui.area();
    let Some(v) = a.view.as_ref() else { return };
    let history = &v.state.history;
    if history.is_empty() {
        ui.within(r, |ui| {
            ui.label("Your story starts here");
            ui.dim("Finish a workout and it will be saved in your journal.");
        });
        return;
    }
    let (labels, tz) = (&v.workout_labels, a.tz);
    let res = ui.list("journal", &mut a.journal_list, r, history.len(), 62.0, |ui, i, selected| {
        let row = ui.area();
        if selected {
            ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
        }
        let w = &history[i];
        let x = row.x + t.px(10.0);
        ui.text_in(Rect::new(x, row.y + t.px(6.0), row.w, t.px(26.0)), Face::Bold, t.pt(15.0), t.text, &w.title);
        let date = format_date(w.finished_ms.unwrap_or(w.started_ms), tz);
        let line = labels.get(&w.id).map_or(date.clone(), |l| {
            format!("{date} · {} · {} · {}", l.duration, sets_text(l.sets), l.volume)
        });
        ui.text_in(Rect::new(x, row.y + t.px(32.0), row.w, t.px(22.0)), Face::Sans, t.pt(13.0), t.dim, &line);
    });
    if let Some(id) = res.activated.and_then(|i| history.get(i)).map(|w| w.id.clone()) {
        a.go(Screen::Detail(id));
    }
}

fn detail(a: &mut Training, ui: &mut Ui, id: &str) {
    let t = ui.theme();
    let Some(w) = a.view.as_ref().and_then(|v| v.state.history.iter().find(|w| w.id == id)).cloned() else {
        let r = ui.area();
        let mut back = false;
        ui.within(r, |ui| {
            ui.dim("Workout not found.");
            back = ui.button("Back to the journal");
        });
        if back {
            a.go(Screen::Journal);
        }
        return;
    };
    let mut lines: Vec<(Face, Rgb, String)> = Vec::new();
    if let Some(v) = &a.view {
        for item in &w.exercises {
            let ex = v.state.exercises.iter().find(|c| c.id == item.exercise_id);
            let kind = ex.map_or(ExerciseKind::WeightReps, |c| c.kind.clone());
            lines.push((Face::Bold, t.text, ex.map_or("Exercise".into(), |c| c.name.clone())));
            for (i, s) in item.sets.iter().enumerate() {
                let label = v.weight_labels.get(&s.id).cloned().unwrap_or_else(|| "0".into());
                let unit = &v.state.settings.weight_unit;
                let value = match kind {
                    ExerciseKind::WeightReps => format!("{label} {unit} × {}", s.reps),
                    ExerciseKind::RepsOnly => format!("{} reps", s.reps),
                    ExerciseKind::Timed => format_duration(s.seconds),
                    ExerciseKind::DistanceTime => {
                        format!("{} in {}", format_distance(s.distance_meters), format_duration(s.seconds))
                    }
                };
                let state = if s.completed { "Done" } else { "Skipped" };
                let c = if s.completed { t.item } else { t.dim };
                lines.push((Face::Sans, c, format!("   {}   {value}   {state}", set_badge(&s.kind, i))));
            }
            if !item.notes.is_empty() {
                lines.push((Face::Sans, t.dim, format!("   {}", item.notes)));
            }
        }
    }
    let label = a.view.as_ref().and_then(|v| v.workout_labels.get(&w.id));
    let mut meta = format_when(w.finished_ms.unwrap_or(w.started_ms), a.tz);
    if let Some(l) = label {
        meta += &format!(" · {} · {} · {}", l.duration, sets_text(l.sets), l.volume);
    }
    ui.heading(&w.title);
    ui.dim(&meta);
    if !w.notes.is_empty() {
        ui.paragraph(&w.notes);
    }
    ui.space(6.0);
    let blocked = a.active().is_some();
    let foot = ui.cut_bottom(t.px(if blocked { 70.0 } else { 46.0 }));
    let mut repeat = false;
    let mut delete = false;
    let mut back = false;
    ui.within(foot, |ui| {
        ui.row(40.0, |ui| {
            if !blocked {
                repeat = ui.primary_button("Repeat workout");
            }
            delete = ui.button_id("delete-workout", "Delete");
            back = ui.button_id("back", "Journal");
        });
        if blocked {
            ui.dim("Finish or discard your current workout before repeating this one.");
        }
    });
    let r = ui.area();
    let mut state = ListState::new();
    ui.list("detail", &mut state, r, lines.len(), 32.0, |ui, i, _| {
        let row = ui.area();
        let (f, c, s) = &lines[i];
        ui.text_in(Rect::new(row.x + t.px(6.0), row.y, row.w, row.h), *f, t.pt(14.0), *c, s);
    });
    if ui.key(keys::KEY_ESC) {
        back = true;
    }
    if repeat {
        a.repeat(&w.id);
    }
    if delete {
        a.confirm = Some(Confirm::DeleteWorkout(w.id.clone()));
    }
    if back {
        a.go(Screen::Journal);
    }
}

// Progress.

fn bars(ui: &mut Ui, r: Rect, title: &str, points: &[ChartPoint], unit: &str) {
    let t = ui.theme();
    ui.fill_round(r, t.px(10.0), t.panel);
    let inner = r.inset(t.px(12.0));
    ui.text_in(Rect::new(inner.x, inner.y, inner.w, t.px(24.0)), Face::Bold, t.pt(15.0), t.text, title);
    let plot = Rect::new(inner.x, inner.y + t.px(30.0), inner.w, inner.h - t.px(30.0) - t.px(22.0));
    if points.is_empty() || plot.h <= 0 {
        ui.text_in(plot, Face::Sans, t.pt(14.0), t.dim, "Your completed sessions will appear here.");
        return;
    }
    if !ui.painting() {
        return;
    }
    let top = points.iter().map(|p| p.volume).fold(0.0_f64, f64::max).max(1.0);
    let gap = t.px(6.0);
    let n = points.len() as i32;
    let w = ((plot.w - gap * (n - 1)) / n).clamp(2, t.px(60.0));
    for (i, p) in points.iter().enumerate() {
        let h = ((p.volume / top) * f64::from(plot.h)) as i32;
        let x = plot.x + (w + gap) * i as i32;
        ui.fill_round(Rect::new(x, plot.bottom() - h, w, h.max(2)), t.px(3.0), t.accent);
    }
    let foot = Rect::new(plot.x, plot.bottom() + t.px(2.0), plot.w, t.px(20.0));
    let last = &points[points.len() - 1];
    ui.text_in(foot, Face::Sans, t.pt(12.0), t.dim, &format!("Latest: {} · {:.0} {unit}", last.label, last.volume));
}

fn progress(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    let area = ui.area();
    let left_w = area.w * 58 / 100;
    let left = Rect::new(area.x, area.y, left_w, area.h);
    let right = Rect::new(left.right() + t.px(12.0), area.y, area.w - left_w - t.px(12.0), area.h);
    let Some(v) = a.view.as_ref() else { return };
    let unit = v.state.settings.weight_unit.clone();

    // Left: the chart of the record selected, else of all training.
    let sel = a.records_list.selected.and_then(|i| v.records.get(i));
    let (title, points): (String, &[ChartPoint]) = match sel.and_then(|r| v.exercise_charts.get(&r.exercise_id).map(|c| (r, c))) {
        Some((r, c)) if !c.is_empty() => (format!("{}: volume", r.name), c),
        _ => ("Training volume".to_owned(), &v.chart),
    };
    let chart = Rect::new(left.x, left.y, left.w, left.h * 45 / 100);
    bars(ui, chart, &title, points, &unit);
    let records_top = chart.bottom() + t.px(10.0);
    let records_head = Rect::new(left.x, records_top, left.w, t.px(26.0));
    ui.text_in(records_head, Face::Bold, t.pt(15.0), t.text, "Personal records");
    let records_r = Rect::new(left.x, records_head.bottom(), left.w, left.bottom() - records_head.bottom());
    let (records, exercises) = (&v.records, &v.state.exercises);
    if records.is_empty() {
        ui.text_in(records_r, Face::Sans, t.pt(14.0), t.dim, "Complete working sets to establish your personal records.");
    } else {
        ui.list("records", &mut a.records_list, records_r, records.len(), 50.0, |ui, i, selected| {
            let row = ui.area();
            if selected {
                ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
            }
            let r = &records[i];
            let kind = exercises.iter().find(|e| e.id == r.exercise_id).map_or(ExerciseKind::WeightReps, |e| e.kind.clone());
            let x = row.x + t.px(10.0);
            ui.text_in(Rect::new(x, row.y + t.px(4.0), row.w, t.px(22.0)), Face::Bold, t.pt(14.0), t.text, &r.name);
            let line = match kind {
                ExerciseKind::RepsOnly => format!("Most reps {}", r.reps),
                ExerciseKind::Timed => format!("Longest set {}", format_duration(r.seconds)),
                ExerciseKind::DistanceTime => {
                    format!("Longest {} · {}", format_distance(r.distance_meters), format_duration(r.seconds))
                }
                ExerciseKind::WeightReps => {
                    let one_rm = r.estimated_1rm.as_ref().map_or(String::new(), |e| format!(" · est. 1RM {e}"));
                    format!("Heaviest {} · most reps {}{one_rm}", r.weight, r.reps)
                }
            };
            ui.text_in(Rect::new(x, row.y + t.px(26.0), row.w, t.px(20.0)), Face::Sans, t.pt(13.0), t.dim, &line);
        });
    }

    // Right: measurements. Body weight is in Weight.
    ui.fill_round(right, t.px(10.0), t.bar);
    let inner = right.inset(t.px(12.0));
    let head = Rect::new(inner.x, inner.y, inner.w, t.px(26.0));
    ui.text_in(head, Face::Bold, t.pt(15.0), t.text, "Body measurements");
    let add_r = Rect::new(inner.x, head.bottom() + t.px(4.0), t.px(190.0), t.px(38.0));
    let add = ui.button_at(add_r, "add-measurement", "Add measurement");
    let list_r = Rect::new(inner.x, add_r.bottom() + t.px(8.0), inner.w, inner.bottom() - add_r.bottom() - t.px(8.0));
    let meas = &v.state.measurements;
    let (labels, tz) = (&v.measurement_labels, a.tz);
    let mut del = None;
    if meas.is_empty() {
        let hint = Rect::new(list_r.x, list_r.y, list_r.w, t.px(24.0));
        ui.text_in(hint, Face::Sans, t.pt(13.0), t.dim, "Keep a private record of waist and body fat.");
    } else {
        let res = ui.list("measurements", &mut a.measurements_list, list_r, meas.len(), 50.0, |ui, i, selected| {
            let row = ui.area();
            if selected {
                ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
            }
            let m = &meas[i];
            let x = row.x + t.px(8.0);
            let label = labels.get(&m.id).cloned().unwrap_or_default();
            let s = format!("{}: {label}", measurement_label(&m.kind));
            ui.text_in(Rect::new(x, row.y + t.px(4.0), row.w, t.px(22.0)), Face::Bold, t.pt(14.0), t.text, &s);
            ui.text_in(Rect::new(x, row.y + t.px(26.0), row.w, t.px(20.0)), Face::Sans, t.pt(12.0), t.dim, &format_date(m.timestamp_ms, tz));
        });
        if let Some(i) = res.activated.or(res.secondary) {
            del = meas.get(i).map(|m| (m.id.clone(), measurement_label(&m.kind)));
        }
    }
    if add {
        a.open_measurement();
    }
    if let Some((id, name)) = del {
        a.confirm = Some(Confirm::DeleteMeasurement(id, name));
    }
}

// Settings.

fn settings(a: &mut Training, ui: &mut Ui) {
    if a.settings.is_none() {
        a.open_settings();
    }
    let options = a.rest_options();
    let Some(s) = &mut a.settings else { return };
    ui.heading("Training preferences");
    ui.space(8.0);
    ui.dim("Weight unit");
    ui.row(40.0, |ui| {
        for (u, label) in [("kg", "Kilograms"), ("lb", "Pounds")] {
            let hit = if s.unit == u { ui.primary_button(label) } else { ui.button_id(("unit", usize::from(u == "lb")), label) };
            if hit {
                s.unit = u.to_owned();
            }
        }
    });
    ui.space(8.0);
    ui.dim("Rest timer");
    ui.row(40.0, |ui| {
        for (i, o) in options.iter().enumerate() {
            let label = format!("{o} s");
            let hit = if s.rest == *o { ui.primary_button(&label) } else { ui.button_id(("rest", i), &label) };
            if hit {
                s.rest = *o;
            }
        }
    });
    ui.space(8.0);
    let word = if s.goal == 1 { "workout" } else { "workouts" };
    ui.dim(&format!("Weekly goal: {} {word}", s.goal));
    ui.row(40.0, |ui| {
        if ui.button_id("goal-down", "−") {
            s.goal = s.goal.saturating_sub(1).max(1);
        }
        if ui.button_id("goal-up", "+") {
            s.goal = (s.goal + 1).min(14);
        }
    });
    ui.space(12.0);
    let saved = s.saved;
    let save = ui.primary_button("Save preferences");
    if saved {
        ui.space(6.0);
        ui.dim("Preferences saved.");
    }
    if save {
        a.save_settings();
    }
    // `YourData`.
    ui.space(12.0);
    ui.heading("Your data");
    ui.space(4.0);
    let (mut export_json, mut export_csv, mut restore) = (false, false, false);
    ui.row(40.0, |ui| {
        export_json = ui.button("Export JSON backup");
        export_csv = ui.button("Export workout CSV");
        restore = ui.button("Restore JSON backup");
    });
    if export_json {
        a.export_as(true);
    }
    if export_csv {
        a.export_as(false);
    }
    if restore {
        a.choose_backup();
    }
    if let Some((ok, m)) = &a.data_status {
        ui.space(6.0);
        if *ok {
            ui.dim(m);
        } else {
            ui.label(m);
        }
    }
    if let Some((name, _)) = a.pending_backup.clone() {
        // The web's question, as a dialog. Cancel comes first and takes Enter: nothing is replaced by a stray key.
        let (mut yes, mut no) = (false, ui.key(keys::KEY_ESC));
        ui.modal(520.0, 180.0, |ui| {
            ui.heading("Replace your current gym data with this backup?");
            ui.paragraph(&format!("{name} · your workouts, routines, measurements and settings are replaced."));
            ui.space(8.0);
            ui.row(40.0, |ui| {
                no |= ui.button("Cancel");
                yes = ui.primary_button("Restore backup");
            });
        });
        if yes {
            a.restore_backup();
        } else if no {
            a.pending_backup = None;
        }
    }
}

// The dialogs.

fn confirm_modal(a: &mut Training, ui: &mut Ui) {
    let Some(c) = &a.confirm else { return };
    let (question, yes): (String, &str) = match c {
        Confirm::Discard => ("Discard this workout for good?".into(), "Discard for good"),
        Confirm::DeleteWorkout(_) => ("Delete this workout from your journal?".into(), "Delete workout"),
        Confirm::DeleteRoutine(_, n) => (format!("Delete the routine {n}?"), "Delete routine"),
        Confirm::RemoveExercise(_, n) => (format!("Remove {n} from this workout?"), "Remove exercise"),
        Confirm::DeleteMeasurement(_, n) => (format!("Delete this {n} measurement?"), "Delete measurement"),
    };
    let mut no = ui.key(keys::KEY_ESC);
    let mut go = false;
    ui.modal(460.0, 160.0, |ui| {
        ui.heading(&question);
        ui.space(14.0);
        ui.row(40.0, |ui| {
            // Cancel comes first and takes Enter: nothing is destroyed by a stray key.
            ui.focus("confirm-no");
            no |= ui.button_id("confirm-no", "Cancel");
            go = ui.button_id("confirm-yes", yes);
        });
    });
    if no {
        a.confirm = None;
    } else if go {
        a.confirmed();
    }
}

fn picker_modal(a: &mut Training, ui: &mut Ui) {
    if a.picker.is_none() {
        return;
    }
    let t = ui.theme();
    if a.picker.as_ref().is_some_and(|p| p.create.is_some()) {
        create_form(a, ui);
        return;
    }
    let rows = a.picker_rows();
    let added: Vec<bool> = rows.iter().map(|e| a.picker_added(&e.id)).collect();
    let muscles = a.muscles();
    let mut close = ui.key(keys::KEY_ESC);
    let mut chosen = None;
    let mut create = false;
    let Some(p) = &mut a.picker else { return };
    ui.modal(720.0, 520.0, |ui| {
        ui.heading("Add exercise");
        ui.space(4.0);
        if std::mem::take(&mut p.first) {
            ui.focus("picker-search");
        }
        let response = ui.text_field("picker-search", &mut p.query);
        if response.changed {
            p.list = ListState::new();
            if !rows.is_empty() {
                p.list.select(0);
            }
        }
        if response.submitted {
            // Enter in the search takes the selected row.
            chosen = p.list.selected.or(Some(0)).and_then(|i| rows.get(i)).cloned();
        }
        ui.space(4.0);
        ui.row(32.0, |ui| {
            let all = ui.button_id("muscle-all", if p.muscle.is_none() { "• All" } else { "All" });
            if all {
                p.muscle = None;
            }
            for (i, m) in muscles.iter().take(7).enumerate() {
                let on = p.muscle.as_ref() == Some(m);
                let label = if on { format!("• {m}") } else { m.clone() };
                if ui.button_id(("muscle", i), &label) {
                    p.muscle = if on { None } else { Some(m.clone()) };
                    p.list = ListState::new();
                }
            }
        });
        ui.space(4.0);
        let foot = ui.cut_bottom(t.px(46.0));
        ui.within(foot, |ui| {
            ui.row(40.0, |ui| {
                close |= ui.button_id("picker-close", "Close");
                create = ui.button_id("picker-create", "Create exercise");
            });
        });
        let r = ui.area();
        if rows.is_empty() {
            ui.within(r, |ui| ui.dim("No exercises match."));
        } else {
            let res = ui.list("picker-list", &mut p.list, r, rows.len(), 46.0, |ui, i, selected| {
                let row = ui.area();
                if selected {
                    ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
                }
                let e = &rows[i];
                let x = row.x + t.px(10.0);
                let c = if added[i] { t.dim } else { t.text };
                ui.text_in(Rect::new(x, row.y + t.px(2.0), row.w - t.px(90.0), t.px(24.0)), Face::Bold, t.pt(14.0), c, &e.name);
                let sub = format!("{} · {} · {}", e.muscle, e.equipment, kind_label(&e.kind));
                ui.text_in(Rect::new(x, row.y + t.px(24.0), row.w - t.px(90.0), t.px(20.0)), Face::Sans, t.pt(12.0), t.dim, &sub);
                if added[i] {
                    ui.text_in(Rect::new(row.right() - t.px(80.0), row.y, t.px(70.0), row.h), Face::Sans, t.pt(13.0), t.dim, "Added");
                }
            });
            if let Some(i) = res.activated {
                chosen = rows.get(i).cloned();
            }
        }
    });
    if let Some(e) = chosen {
        a.pick(&e);
        return;
    }
    if create && let Some(p) = &mut a.picker {
        p.create = Some(CreateExercise {
            name: TextEdit::with_text(p.query.text.trim()),
            muscle: TextEdit::new(),
            equipment: TextEdit::new(),
            kind: ExerciseKind::WeightReps,
            error: None,
            first: true,
        });
    }
    if close {
        a.picker = None;
    }
}

fn create_form(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    let mut close = ui.key(keys::KEY_ESC);
    let mut save = false;
    let Some(c) = a.picker.as_mut().and_then(|p| p.create.as_mut()) else { return };
    ui.modal(620.0, 400.0, |ui| {
        ui.heading("New exercise");
        ui.space(4.0);
        if std::mem::take(&mut c.first) {
            ui.focus("new-name");
        }
        ui.dim("Exercise name");
        save |= ui.text_field("new-name", &mut c.name).submitted;
        ui.dim("Muscle group");
        save |= ui.text_field("new-muscle", &mut c.muscle).submitted;
        ui.dim("Equipment");
        save |= ui.text_field("new-equipment", &mut c.equipment).submitted;
        ui.dim("Tracking");
        ui.row(36.0, |ui| {
            for (i, k) in [ExerciseKind::WeightReps, ExerciseKind::RepsOnly, ExerciseKind::Timed, ExerciseKind::DistanceTime]
                .into_iter()
                .enumerate()
            {
                let hit = if c.kind == k { ui.primary_button(kind_label(&k)) } else { ui.button_id(("new-kind", i), kind_label(&k)) };
                if hit {
                    c.kind = k;
                }
            }
        });
        if let Some(e) = &c.error {
            ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
        }
        ui.space(6.0);
        ui.row(40.0, |ui| {
            save |= ui.primary_button("Save exercise");
            close |= ui.button_id("new-cancel", "Cancel");
        });
    });
    if save {
        a.save_exercise();
    } else if close && let Some(p) = &mut a.picker {
        p.create = None;
        p.first = true;
    }
}

fn builder_modal(a: &mut Training, ui: &mut Ui) {
    if a.builder.is_none() {
        return;
    }
    let t = ui.theme();
    let names: Vec<String> = a
        .builder
        .as_ref()
        .map(|b| b.ids.iter().map(|id| a.exercise(id).map_or(id.clone(), |e| e.name.clone())).collect())
        .unwrap_or_default();
    let mut close = a.picker.is_none() && ui.key(keys::KEY_ESC);
    let mut save = false;
    let mut add = false;
    let (mut up, mut down, mut remove) = (false, false, false);
    let Some(b) = &mut a.builder else { return };
    ui.modal(640.0, 500.0, |ui| {
        ui.heading("New routine");
        ui.space(4.0);
        if std::mem::take(&mut b.first) {
            ui.focus("routine-new-name");
        }
        ui.dim("Routine name");
        save |= ui.text_field("routine-new-name", &mut b.name).submitted;
        ui.space(4.0);
        ui.dim("Exercises in order");
        let foot = ui.cut_bottom(t.px(138.0));
        let r = ui.area();
        if names.is_empty() {
            ui.within(r, |ui| ui.dim("No exercises yet."));
        } else {
            ui.list("routine-list", &mut b.list, r, names.len(), 38.0, |ui, i, selected| {
                let row = ui.area();
                if selected {
                    ui.fill_round(row.inset(t.px(1.0)), t.px(6.0), t.panel_hi);
                }
                let tr = Rect::new(row.x + t.px(10.0), row.y, row.w, row.h);
                ui.text_in(tr, Face::Sans, t.pt(14.0), t.text, &format!("{}. {}", i + 1, names[i]));
            });
        }
        ui.within(foot, |ui| {
            ui.row(36.0, |ui| {
                add = ui.button_id("routine-add", "Add exercise");
                up = ui.button_id("routine-up", "Move up");
                down = ui.button_id("routine-down", "Move down");
                remove = ui.button_id("routine-remove", "Remove");
            });
            ui.space(2.0);
            ui.dim("New routines start with three sets per exercise.");
            if let Some(e) = &b.error {
                ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
            }
            ui.space(2.0);
            ui.row(40.0, |ui| {
                save |= ui.primary_button("Save routine");
                close |= ui.button_id("routine-close", "Cancel");
            });
        });
    });
    if up {
        a.builder_move(-1);
    }
    if down {
        a.builder_move(1);
    }
    if remove {
        a.builder_remove();
    }
    if add {
        a.open_picker(PickFor::Routine);
    }
    if save {
        a.save_builder();
    }
    if close {
        a.builder = None;
    }
}

fn measurement_modal(a: &mut Training, ui: &mut Ui) {
    let t = ui.theme();
    let mut close = a.measurement.is_some() && ui.key(keys::KEY_ESC);
    let mut save = false;
    let Some(m) = &mut a.measurement else { return };
    ui.modal(480.0, 300.0, |ui| {
        ui.heading("New measurement");
        ui.space(6.0);
        if std::mem::take(&mut m.first) {
            ui.focus("measurement-value");
        }
        ui.row(38.0, |ui| {
            for (i, (k, label)) in [("waist", "Waist"), ("body_fat", "Body fat")].into_iter().enumerate() {
                let hit = if m.kind == k { ui.primary_button(label) } else { ui.button_id(("measurement-kind", i), label) };
                if hit {
                    m.kind = k;
                }
            }
        });
        ui.space(6.0);
        let unit = if m.kind == "waist" { "cm" } else { "%" };
        ui.dim(&format!("Value ({unit})"));
        save |= ui.text_field("measurement-value", &mut m.value).submitted;
        if let Some(e) = &m.error {
            ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
        }
        ui.space(8.0);
        ui.row(40.0, |ui| {
            save |= ui.primary_button("Save measurement");
            close |= ui.button_id("measurement-cancel", "Cancel");
        });
    });
    if save {
        a.save_measurement();
    }
    if close {
        a.measurement = None;
    }
}
