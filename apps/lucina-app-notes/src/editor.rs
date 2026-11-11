//! Open one note: `components/keep/NoteDialog.tsx`, `ChecklistEditor.tsx` and `NoteToolbar.tsx`. The text lives here in
//! memory and every key lands at once; the store hears about it 600 ms after the last key, or on closing.

use std::time::{Duration, Instant};

use lucina_app::keys;
use lucina_app::time::Zone;
use lucina_app::{Face, ListState, Rect, Rgb, TextEdit, Ui};
use lucina_app_support::dates::{edited_label, now_ms, reminder_label, reminder_presets, to_iso};

use crate::model::{
    COLORS, Input, Item, Label, Note, Reminder, content_to_items, new_item_id, norm_label,
    split_title_heading,
};
use crate::store::Store;
use crate::{hid, palette};

const SAVE_DELAY: Duration = Duration::from_millis(600);

pub struct Row {
    pub item: Item,
    pub edit: TextEdit,
}

impl Row {
    fn new(item: Item) -> Row {
        let edit = TextEdit::with_text(&item.text);
        Row { item, edit }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    None,
    Colors,
    Labels,
    Remind,
}

pub enum Outcome {
    Stay,
    Close,
    Deleted,
}

pub struct Editor {
    /// `None` until a new note has something in it: an empty note is never made.
    pub id: Option<String>,
    pub title: TextEdit,
    pub body: TextEdit,
    /// The `# <title>` line that is hidden but kept.
    head: String,
    pub rows: Option<Vec<Row>>,
    dirty_title: bool,
    dirty_body: bool,
    pub save_at: Option<Instant>,
    panel: Panel,
    checked_open: bool,
    /// The item whose field has the keyboard, and the one that should get it next.
    focused: Option<String>,
    focus_next: Option<String>,
    focus_body: bool,
    list: ListState,
    label_new: TextEdit,
    labels_list: ListState,
    /// `Notes.tsx`: a note changed elsewhere answers 409 instead of being overwritten.
    guard: bool,
    new_label: Option<String>,
    color: String,
}

/// What the row list asked for, done after the list has drawn.
enum Act {
    Toggle(usize),
    Remove(usize),
    Split(usize),
    Indent(usize, u8),
    Add,
    FlipChecked,
    Focus(String),
}

impl Editor {
    pub fn open(n: &Note, guard: bool) -> Editor {
        let (head, body) = split_title_heading(&n.content, &n.title);
        let mut e = Editor::empty();
        e.id = Some(n.id.clone());
        e.title = TextEdit::with_text(&n.title).placeholder("Title");
        e.head = head.to_owned();
        e.rows = n
            .items
            .as_ref()
            .map(|items| items.iter().cloned().map(Row::new).collect());
        e.body = TextEdit::with_text(if n.items.is_some() { "" } else { body })
            .placeholder("Take a note…");
        e.body.multiline = true;
        e.guard = guard;
        e.color = n.color.clone();
        match &e.rows {
            Some(rows) => {
                e.focus_next = rows
                    .iter()
                    .find(|r| !r.item.checked)
                    .map(|r| r.item.id.clone())
            }
            None => e.focus_body = true,
        }
        e
    }

    pub fn blank(list: bool, label: Option<String>) -> Editor {
        let mut e = Editor::empty();
        e.new_label = label;
        if list {
            let item = Item {
                id: new_item_id(),
                ..Item::default()
            };
            e.focus_next = Some(item.id.clone());
            e.rows = Some(vec![Row::new(item)]);
        } else {
            e.focus_body = true;
        }
        e
    }

    fn empty() -> Editor {
        let mut body = TextEdit::multiline().placeholder("Take a note…");
        body.multiline = true;
        Editor {
            id: None,
            title: TextEdit::new().placeholder("Title"),
            body,
            head: String::new(),
            rows: None,
            dirty_title: false,
            dirty_body: false,
            save_at: None,
            panel: Panel::None,
            checked_open: true,
            focused: None,
            focus_next: None,
            focus_body: false,
            list: ListState::new(),
            label_new: TextEdit::new().placeholder("Label name"),
            labels_list: ListState::new(),
            guard: false,
            new_label: None,
            color: "default".into(),
        }
    }

    /// Give the keyboard to this item at the next pass.
    pub fn focus_item(&mut self, id: &str) {
        self.focus_next = Some(id.to_owned());
    }

    fn touch_body(&mut self) {
        self.dirty_body = true;
        self.save_at = Some(Instant::now() + SAVE_DELAY);
    }

    fn items(&self) -> Option<Vec<Item>> {
        self.rows.as_ref().map(|rows| {
            rows.iter()
                .map(|r| Item {
                    text: r.edit.text.clone(),
                    ..r.item.clone()
                })
                .collect()
        })
    }

    pub fn autosave(&mut self, store: &mut Store, now: Instant) {
        if self.save_at.is_some_and(|t| t <= now) {
            self.flush(store);
        }
    }

    /// `flush`: what changed goes to the store. While a conflict is open nothing is sent over the other client's edit.
    pub fn flush(&mut self, store: &mut Store) {
        self.save_at = None;
        if !self.dirty_title && !self.dirty_body {
            return;
        }
        if let Some(id) = &self.id
            && store.conflict.as_deref() == Some(store.resolve(id))
        {
            return;
        }
        let Some(id) = self.id.clone() else {
            // A new note: it exists from the first text on.
            let title = self.title.text.trim().to_owned();
            let items = self.items().map(|v| {
                v.into_iter()
                    .filter(|i| !i.text.trim().is_empty())
                    .collect::<Vec<_>>()
            });
            let content = self.body.text.trim_end().to_owned();
            let empty =
                title.is_empty() && content.is_empty() && items.as_ref().is_none_or(Vec::is_empty);
            if empty {
                return;
            }
            let input = Input {
                title: Some(title),
                content: items.is_none().then_some(content),
                items: items.clone(),
                labels: self.new_label.take().map(|l| vec![l]),
                color: (self.color != "default").then(|| self.color.clone()),
                ..Input::default()
            };
            self.id = Some(store.create(input));
            self.dirty_title = false;
            self.dirty_body = false;
            return;
        };
        let mut input = Input::default();
        if self.dirty_title {
            input.title = Some(self.title.text.trim().to_owned());
        }
        if self.dirty_body {
            match self.items() {
                Some(items) => input.items = Some(items),
                None => {
                    input.content = Some(
                        format!("{}{}", self.head, self.body.text)
                            .trim_end()
                            .to_owned(),
                    )
                }
            }
        }
        self.dirty_title = false;
        self.dirty_body = false;
        store.update(&id, input, self.guard);
    }

    /// `toggleCheckboxes`: a text note becomes a list and back.
    fn toggle_checkboxes(&mut self, store: &mut Store) {
        self.flush(store);
        if let Some(rows) = self.rows.take() {
            let text: Vec<String> = rows
                .iter()
                .map(|r| {
                    format!(
                        "{}{}",
                        if r.item.indent == 1 { "  " } else { "" },
                        r.edit.text
                    )
                })
                .collect();
            self.body.set(&text.join("\n"));
            self.head.clear();
            if let Some(id) = &self.id {
                store.update(
                    id,
                    Input {
                        convert: Some("text".into()),
                        ..Input::default()
                    },
                    false,
                );
            }
            self.focus_body = true;
        } else {
            let mut items = content_to_items(&format!("{}{}", self.head, self.body.text));
            self.head.clear();
            if items.is_empty() {
                items.push(Item {
                    id: new_item_id(),
                    ..Item::default()
                });
            }
            self.focus_next = items.first().map(|i| i.id.clone());
            self.body.clear();
            self.rows = Some(items.iter().cloned().map(Row::new).collect());
            if let Some(id) = &self.id {
                store.update(
                    id,
                    Input {
                        items: Some(items),
                        ..Input::default()
                    },
                    false,
                );
            }
        }
    }

    fn draw_rows(&mut self, ui: &mut Ui, r: Rect, read_only: bool) {
        let t = ui.theme();
        let Some(rows) = self.rows.as_mut() else {
            return;
        };
        // The order on the screen: open items, the add row, the checked ones.
        enum Line {
            Item(usize),
            Add,
            Head(usize),
        }
        let mut lines: Vec<Line> = rows
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.item.checked)
            .map(|(i, _)| Line::Item(i))
            .collect();
        if !read_only {
            lines.push(Line::Add);
        }
        let done: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, x)| x.item.checked)
            .map(|(i, _)| i)
            .collect();
        if !done.is_empty() {
            lines.push(Line::Head(done.len()));
            if self.checked_open {
                lines.extend(done.iter().map(|&i| Line::Item(i)));
            }
        }
        let want = self.focus_next.take();
        if let Some(w) = &want
            && let Some(at) = lines
                .iter()
                .position(|l| matches!(l, Line::Item(i) if rows[*i].item.id == *w))
        {
            self.list.select(at);
        }
        let mut act = None;
        let focused = self.focused.clone();
        let mut now_focused = None;
        let open_ids: Vec<String> = rows
            .iter()
            .filter(|x| !x.item.checked)
            .map(|x| x.item.id.clone())
            .collect();
        let bg = palette(&self.color).unwrap_or(t.panel);
        let checked_open = self.checked_open;
        ui.list("rows", &mut self.list, r, lines.len(), 38.0, |ui, n, _| {
            let a = ui.area();
            ui.fill(a, bg);
            match lines[n] {
                Line::Add => {
                    if ui.button_at(
                        Rect::new(a.x, a.y + t.px(3.0), t.px(120.0), a.h - t.px(6.0)),
                        "add-item",
                        "+ List item",
                    ) {
                        act = Some(Act::Add);
                    }
                }
                Line::Head(count) => {
                    let label = format!("{} {count} checked", if checked_open { "▾" } else { "▸" });
                    if ui.button_at(
                        Rect::new(a.x, a.y + t.px(3.0), t.px(140.0), a.h - t.px(6.0)),
                        "checked-head",
                        &label,
                    ) {
                        act = Some(Act::FlipChecked);
                    }
                }
                Line::Item(i) => {
                    let row = &mut rows[i];
                    let h = hid(&row.item.id);
                    let indent = t.px(24.0) * i32::from(row.item.indent);
                    let size = t.px(28.0);
                    let y = a.y + (a.h - size) / 2;
                    let bx = Rect::new(a.x + indent, y, size, size);
                    if !read_only
                        && ui.button_at(bx, ("box", h), if row.item.checked { "☑" } else { "☐" })
                    {
                        act = Some(Act::Toggle(i));
                    } else if read_only {
                        ui.text_in(
                            bx,
                            Face::Sans,
                            t.pt(15.0),
                            t.item,
                            if row.item.checked { "☑" } else { "☐" },
                        );
                    }
                    let del = Rect::new(a.right() - size - t.px(4.0), y, size, size);
                    let fld = Rect::new(
                        bx.right() + t.px(6.0),
                        a.y + t.px(2.0),
                        del.x - bx.right() - t.px(10.0),
                        a.h - t.px(4.0),
                    );
                    if read_only {
                        ui.text_in(fld, Face::Sans, t.pt(15.0), t.item, &row.edit.text);
                        return;
                    }
                    if want.as_deref() == Some(row.item.id.as_str()) {
                        ui.focus(("text", h));
                    }
                    // The keys a list item treats differently from a plain field.
                    if focused.as_deref() == Some(row.item.id.as_str())
                        && let Some(p) = ui.press()
                        && !p.mods.ctrl
                    {
                        let at = open_ids.iter().position(|o| *o == row.item.id);
                        match p.code {
                            keys::KEY_TAB => {
                                ui.take_press();
                                act = Some(Act::Indent(i, u8::from(!p.mods.shift)));
                            }
                            keys::KEY_BACKSPACE if row.edit.text.is_empty() => {
                                ui.take_press();
                                act = Some(Act::Remove(i));
                            }
                            keys::KEY_UP => {
                                if let Some(prev) = at.and_then(|x| x.checked_sub(1)) {
                                    ui.take_press();
                                    act = Some(Act::Focus(open_ids[prev].clone()));
                                }
                            }
                            keys::KEY_DOWN => {
                                if let Some(next) = at.and_then(|x| open_ids.get(x + 1)) {
                                    ui.take_press();
                                    act = Some(Act::Focus(next.clone()));
                                }
                            }
                            _ => {}
                        }
                    }
                    let res = ui.text_field_at(fld, ("text", h), &mut row.edit);
                    if res.focused {
                        now_focused = Some(row.item.id.clone());
                    }
                    if res.submitted {
                        act = Some(Act::Split(i));
                    }
                    if res.changed {
                        act = act.take().or(Some(Act::Focus(String::new())));
                    }
                    if ui.button_at(del, ("del", h), "×") {
                        act = Some(Act::Remove(i));
                    }
                }
            }
        });
        if now_focused.is_some() {
            self.focused = now_focused;
        }
        // A row's text changed: only remember to save (an empty Focus says so).
        match act {
            None => {}
            Some(Act::Focus(id)) if id.is_empty() => self.touch_body(),
            Some(Act::Focus(id)) => self.focus_next = Some(id),
            Some(Act::Toggle(i)) => {
                let rows = self.rows.as_mut().expect("rows");
                rows[i].item.checked = !rows[i].item.checked;
                self.touch_body();
            }
            Some(Act::Remove(i)) => {
                let rows = self.rows.as_mut().expect("rows");
                let open: Vec<usize> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| !x.item.checked)
                    .map(|(k, _)| k)
                    .collect();
                let prev = open
                    .iter()
                    .rposition(|&k| k == i)
                    .and_then(|p| p.checked_sub(1))
                    .map(|p| rows[open[p]].item.id.clone());
                rows.remove(i);
                self.focus_next = prev;
                self.touch_body();
            }
            Some(Act::Split(i)) => {
                let rows = self.rows.as_mut().expect("rows");
                // Enter splits the item at the cursor, like Keep.
                let at = rows[i].edit.cursor.min(rows[i].edit.text.len());
                let tail = rows[i].edit.text.split_off(at);
                rows[i].edit.cursor = at;
                rows[i].edit.anchor = at;
                let item = Item {
                    id: new_item_id(),
                    text: tail,
                    checked: false,
                    indent: rows[i].item.indent,
                };
                let id = item.id.clone();
                let mut row = Row::new(item);
                row.edit.cursor = 0;
                row.edit.anchor = 0;
                rows.insert(i + 1, row);
                self.focus_next = Some(id);
                self.touch_body();
            }
            Some(Act::Indent(i, level)) => {
                self.rows.as_mut().expect("rows")[i].item.indent = level;
                self.touch_body();
            }
            Some(Act::Add) => {
                let item = Item {
                    id: new_item_id(),
                    ..Item::default()
                };
                self.focus_next = Some(item.id.clone());
                self.rows.as_mut().expect("rows").push(Row::new(item));
            }
            Some(Act::FlipChecked) => self.checked_open = !self.checked_open,
        }
    }

    fn draw_panel(
        &mut self,
        ui: &mut Ui,
        store: &mut Store,
        labels: &[Label],
        tags: &[String],
        zone: &Zone,
    ) {
        let t = ui.theme();
        let id = self.id.clone();
        match self.panel {
            Panel::None => {}
            Panel::Colors => {
                ui.row(38.0, |ui| {
                    for c in COLORS {
                        let r = ui.area();
                        let size = t.px(34.0);
                        let rect = Rect::new(r.x, r.y, size, size);
                        ui.cut_left(size + t.px(6.0));
                        if ui.button_at(rect, ("swatch", hid(c)), "") {
                            self.color = c.to_owned();
                            if let Some(id) = &id {
                                store.set_color(std::slice::from_ref(id), c);
                            }
                        }
                        if ui.painting() {
                            let fill = palette(c).unwrap_or(Rgb::hex(0x202124));
                            ui.fill_round(rect.inset(t.px(3.0)), size / 2, fill);
                            if self.color == c {
                                ui.text_in(
                                    rect.inset(t.px(8.0)),
                                    Face::Sans,
                                    t.pt(13.0),
                                    t.text,
                                    "✓",
                                );
                            }
                        }
                    }
                });
            }
            Panel::Labels => {
                let a = ui.cut_top(t.px(170.0));
                ui.within(a, |ui| {
                    let f = ui.cut_top(t.px(34.0));
                    let res = ui.text_field_at(f, "note-label-new", &mut self.label_new);
                    ui.space(4.0);
                    if res.submitted && !self.label_new.text.trim().is_empty() {
                        if let Some(id) = &id {
                            store.add_label(id, &self.label_new.text);
                        } else {
                            self.new_label = Some(self.label_new.text.clone());
                        }
                        self.label_new.clear();
                    }
                    let q = norm_label(&self.label_new.text);
                    let hits: Vec<&Label> = labels
                        .iter()
                        .filter(|l| q.is_empty() || l.tag.contains(&q))
                        .collect();
                    let r = ui.area();
                    let res = ui.list(
                        "note-labels",
                        &mut self.labels_list,
                        r,
                        hits.len(),
                        28.0,
                        |ui, i, _| {
                            let a = ui.area();
                            let on = tags.contains(&hits[i].tag);
                            ui.text_in(
                                Rect::new(a.x + t.px(4.0), a.y, a.w, a.h),
                                Face::Sans,
                                t.pt(14.0),
                                t.item,
                                &format!("{} {}", if on { "☑" } else { "☐" }, hits[i].name),
                            );
                        },
                    );
                    if let Some(i) = res.activated
                        && let Some(id) = &id
                    {
                        if tags.contains(&hits[i].tag) {
                            store.remove_label(id, &hits[i].tag);
                        } else {
                            store.add_label(id, &hits[i].name);
                        }
                    }
                });
            }
            Panel::Remind => {
                let presets = reminder_presets(now_ms(), zone);
                ui.row(34.0, |ui| {
                    for (label, at) in &presets {
                        let shown =
                            format!("{label}: {}", reminder_label(&to_iso(*at), now_ms(), zone));
                        if ui.button_id(("remind", hid(label)), &shown)
                            && let Some(id) = &id
                        {
                            let reminder = Reminder {
                                at: to_iso(*at),
                                recurrence: None,
                            };
                            store.update(
                                id,
                                Input {
                                    reminder: Some(Some(reminder)),
                                    ..Input::default()
                                },
                                false,
                            );
                        }
                    }
                    if ui.button("Clear")
                        && let Some(id) = &id
                    {
                        store.update(
                            id,
                            Input {
                                reminder: Some(None),
                                ..Input::default()
                            },
                            false,
                        );
                    }
                });
            }
        }
    }

    pub fn view(
        &mut self,
        ui: &mut Ui,
        store: &mut Store,
        labels: &[Label],
        zone: &Zone,
    ) -> Outcome {
        let note = self
            .id
            .as_ref()
            .and_then(|i| store.note(i))
            .map(|e| e.note.clone());
        if let (Some(_), None) = (&self.id, &note) {
            // Deleted elsewhere, or refused: nothing left to edit.
            return Outcome::Deleted;
        }
        if let Some(n) = &note {
            self.color = n.color.clone();
        }
        let read_only = note.as_ref().is_some_and(|n| n.deleted_at.is_some());
        let conflict = note
            .as_ref()
            .is_some_and(|n| store.conflict.as_deref() == Some(n.id.as_str()));
        let mut out = Outcome::Stay;
        let fill = palette(&self.color);
        let t = ui.theme();
        ui.modal(920.0, 680.0, |ui| {
            if let Some(c) = fill {
                let a = ui.area();
                ui.fill_round(a, t.px(6.0), c);
                ui.pad(8.0);
            }
            if let Some(n) = &note {
                let foot = ui.cut_bottom(t.px(22.0));
                let mut meta = format!("Edited {}", edited_label(&n.updated_at, now_ms(), zone));
                if let Some(r) = &n.reminder {
                    meta.push_str(&format!(
                        "  ·  Reminder {}",
                        reminder_label(&r.at, now_ms(), zone)
                    ));
                }
                if !n.tags.is_empty() {
                    meta.push_str(&format!(
                        "  ·  {}",
                        n.tags
                            .iter()
                            .map(|x| store.label_name(x))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !n.attachments.is_empty() {
                    meta.push_str(&format!(
                        "  ·  {} attachments (open them in Secretary)",
                        n.attachments.len()
                    ));
                }
                ui.text_in(foot, Face::Sans, t.pt(12.0), t.item, &meta);
            }
            if read_only {
                let foot = ui.cut_bottom(t.px(44.0));
                ui.within(foot, |ui| {
                    ui.space(6.0);
                    ui.row(34.0, |ui| {
                        if ui.button("Restore") {
                            store.restore(std::slice::from_ref(self.id.as_ref().expect("id")));
                            out = Outcome::Close;
                        }
                        if ui.button("Delete forever") {
                            store.delete_forever(std::slice::from_ref(
                                self.id.as_ref().expect("id"),
                            ));
                            out = Outcome::Deleted;
                        }
                        if ui.primary_button("Close") {
                            out = Outcome::Close;
                        }
                    });
                });
                ui.dim("Can't edit notes in the trash.");
            } else {
                self.draw_toolbar(ui, store, note.as_ref(), &mut out);
                let tags = note.as_ref().map_or_else(Vec::new, |n| n.tags.clone());
                // The panel goes above the toolbar: it was cut off the bottom, so it takes its own place from the bottom.
                if self.panel != Panel::None {
                    let h = if self.panel == Panel::Labels {
                        178.0
                    } else {
                        44.0
                    };
                    let p = ui.cut_bottom(t.px(h));
                    ui.within(p, |ui| self.draw_panel(ui, store, labels, &tags, zone));
                }
            }
            if conflict {
                let c = ui.cut_top(t.px(40.0));
                ui.within(c, |ui| {
                    ui.row(34.0, |ui| {
                        ui.label("This note changed somewhere else.");
                        if ui.button("Keep mine") {
                            store.clear_conflict();
                            self.guard = false;
                            self.dirty_title = true;
                            self.dirty_body = true;
                            self.flush(store);
                        }
                        if ui.button("Use theirs") {
                            store.clear_conflict();
                            store.want_sync();
                            out = Outcome::Deleted;
                        }
                    });
                });
            }
            let title_r = ui.cut_top(t.px(34.0));
            let res = if read_only {
                ui.text_in(title_r, Face::Bold, t.pt(17.0), t.text, &self.title.text);
                Default::default()
            } else {
                ui.text_field_at(title_r, "title", &mut self.title)
            };
            if res.changed {
                self.dirty_title = true;
                self.save_at = Some(Instant::now() + SAVE_DELAY);
            }
            if res.submitted {
                if self.rows.is_some() {
                    self.focus_next = self
                        .rows
                        .as_ref()
                        .and_then(|r| r.first())
                        .map(|r| r.item.id.clone());
                } else {
                    self.focus_body = true;
                }
            }
            ui.space(6.0);
            let area = ui.area();
            if self.rows.is_some() {
                self.draw_rows(ui, area, read_only);
            } else if read_only {
                ui.wrapped(Face::Sans, t.pt(15.0), t.item, &self.body.text.clone());
            } else {
                if self.focus_body {
                    self.focus_body = false;
                    ui.focus("body");
                }
                let res = ui.text_field_at(area, "body", &mut self.body);
                if res.changed {
                    self.touch_body();
                }
            }
        });
        if ui.key(keys::KEY_ESC) {
            out = Outcome::Close;
        }
        out
    }

    fn draw_toolbar(
        &mut self,
        ui: &mut Ui,
        store: &mut Store,
        note: Option<&Note>,
        out: &mut Outcome,
    ) {
        let t = ui.theme();
        let bar = ui.cut_bottom(t.px(42.0));
        let id = self.id.clone();
        let one = id.as_ref().map(std::slice::from_ref);
        let mut flip: Option<Panel> = None;
        let mut checkboxes = false;
        ui.within(bar, |ui| {
            ui.space(6.0);
            ui.row(34.0, |ui| {
                if let (Some(n), Some(one)) = (note, one)
                    && ui.button_id("ed-pin", if n.pinned { "Unpin" } else { "Pin" })
                {
                    store.set_pinned(one, !n.pinned);
                }
                if ui.button("Colour") {
                    flip = Some(Panel::Colors);
                }
                if ui.button("Labels") {
                    flip = Some(Panel::Labels);
                }
                if note.is_some() && ui.button("Remind") {
                    flip = Some(Panel::Remind);
                }
                if ui.button_id(
                    "ed-boxes",
                    if self.rows.is_some() {
                        "Plain text"
                    } else {
                        "Checkboxes"
                    },
                ) {
                    checkboxes = true;
                }
                if let (Some(n), Some(one)) = (note, one) {
                    if ui.button_id("ed-arch", if n.archived { "Unarchive" } else { "Archive" }) {
                        if n.archived {
                            store.unarchive(one);
                        } else {
                            store.archive(one);
                        }
                        *out = Outcome::Close;
                    }
                    if ui.button("Copy") {
                        store.duplicate(&one[0]);
                        *out = Outcome::Close;
                    }
                    if ui.button("Delete") {
                        store.trash(one);
                        *out = Outcome::Deleted;
                    }
                }
                let a = ui.area();
                let w = ui.px(84.0);
                if ui.button_at(Rect::new(a.right() - w, a.y, w, a.h), "ed-close", "Close") {
                    *out = Outcome::Close;
                }
            });
        });
        if let Some(p) = flip {
            self.panel = if self.panel == p { Panel::None } else { p };
        }
        if checkboxes {
            self.toggle_checkboxes(store);
        }
    }
}
