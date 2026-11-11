//! How it looks. The web's three columns on a wide screen, its three tabs on a narrow one, and its sheets as dialogs.
//!
//! The keyboard: arrows move in a list, Enter opens, Tab walks everything, Esc goes back one step. On a notebook's page Ctrl+1, 2 and 3
//! go to Sources, Notes and Chat, and Ctrl+F searches. In the chat's log, 1 to 9 open a citation and S saves a reply to a note.

use std::time::Instant;

use lucina_app::keys::{self, Mods};
use lucina_app::{Face, Rect, Ui};
use lucina_app_support::dates::date_label;

use crate::app::{
    AddMode, Notebooks, Page, PICK_WAIT, Picker, SEARCH_WAIT, ShownMessage, Sheet, SheetKind, Tab, Todo,
};
use crate::md;
use crate::server::Paging;
use crate::wire::{clock, context_label, count, counts_line, insight_pending, kind_label, origin_label, podcast_pending, status_label};

const KEY_S: u16 = 31;
const KEY_F: u16 = 33;
const KEY_9: u16 = 10;

/// From here the three columns fit side by side. The web's own `useWide` asks the same of its slot.
const WIDE_PX: i32 = 1200;

const CREDIT: &str = "Based on Open Notebook by Luis Novo (MIT).";

fn plain(p: Mods) -> bool {
    !p.ctrl && !p.alt && !p.meta && !p.shift
}

/// A button at the right end of `row`, which then ends before it.
fn button_right(ui: &mut Ui, row: &mut Rect, label: &str, primary: bool) -> bool {
    let t = ui.theme();
    let w = ui.measure(Face::Sans, t.pt(15.0), label) + 2 * t.px(14.0);
    let h = t.px(34.0).min(row.h);
    let r = Rect::new(row.right() - w, row.y + (row.h - h) / 2, w, h);
    row.w -= w + t.px(8.0);
    let clicked = if primary {
        ui.within(r, |ui| ui.primary_button(label))
    } else {
        ui.button_at(r, label, label)
    };
    clicked
}

/// A one-line title and a dim line under it, for a row.
fn two_lines(ui: &mut Ui, r: Rect, title: &str, sub: &str, right: &str) {
    let t = ui.theme();
    let pad = t.px(10.0);
    let right_w = if right.is_empty() { 0 } else { ui.measure(Face::Sans, t.pt(13.0), right) + pad };
    let w = r.w - 2 * pad - right_w;
    let top = Rect::new(r.x + pad, r.y + t.px(4.0), w, r.h / 2 - t.px(2.0));
    let low = Rect::new(r.x + pad, r.y + r.h / 2, r.w - 2 * pad, r.h / 2 - t.px(4.0));
    ui.text_in(top, Face::Bold, t.pt(15.0), t.text, title);
    ui.text_in(low, Face::Sans, t.pt(13.0), t.dim, sub);
    if right_w > 0 {
        let rr = Rect::new(r.right() - right_w, top.y, right_w, top.h);
        ui.text_in(rr, Face::Sans, t.pt(13.0), t.dim, right);
    }
}

fn panel(ui: &mut Ui, r: Rect) -> Rect {
    let t = ui.theme();
    // Buttons are drawn in `panel`: on it they would not show.
    ui.fill_round(r, t.px(10.0), t.bar);
    r.inset(t.px(10.0))
}

/// The pane's title, with room for buttons at its right.
fn pane_title(ui: &mut Ui, title: &str) -> Rect {
    let t = ui.theme();
    let mut row = ui.cut_top(t.px(36.0));
    let w = ui.measure(Face::Bold, t.pt(16.0), title) + t.px(12.0);
    let head = Rect::new(row.x, row.y, w, row.h);
    ui.text_in(head, Face::Bold, t.pt(16.0), t.text, title);
    row.x += w;
    row.w -= w;
    ui.cut_top(t.px(4.0));
    row
}

fn notice_line(ui: &mut Ui, text: &str) {
    let t = ui.theme();
    let px = t.pt(14.0);
    let w = ui.area().w;
    let h = ui.wrap_count(Face::Sans, px, text, w) * ui.line_height(Face::Sans, px) + t.px(4.0);
    let r = ui.cut_bottom(h);
    ui.within(r, |ui| ui.wrapped(Face::Sans, px, t.danger, text));
}

pub fn view(a: &mut Notebooks, ui: &mut Ui) {
    let title = if a.sample { "Notebooks (sample data)" } else { "Notebooks" };
    ui.header(title);
    if let Some(s) = a.signing_in() {
        s.view(ui, "Your notebooks wait in your account. This laptop asks to be added as a device.");
        return;
    }
    if let Some(p) = a.page.as_mut().filter(|p| p.refocus) {
        p.refocus = false;
        a.want_focus = Some(match (&p.reader, &p.sheet, p.tab) {
            (_, Some(Sheet::Settings(_)), _) => "set-delete",
            (_, Some(Sheet::Podcast(_)), _) => "pod-list",
            (Some(_), _, _) => "insights",
            (_, _, Tab::Sources) => "sources",
            (_, _, Tab::Notes) => "notes",
            (_, _, Tab::Chat) => "ask",
        });
    }
    if let Some(id) = a.want_focus {
        ui.focus(id);
        if a.focus_asked == Some(id) {
            a.want_focus = None;
            a.focus_asked = None;
        } else {
            a.focus_asked = Some(id);
        }
    }
    let mut todo = None;
    if a.page.is_some() {
        view_page(a, ui, &mut todo);
    } else {
        view_list(a, ui, &mut todo);
        new_modal(a, ui, &mut todo);
    }
    a.tick(ui);
    if let Some(x) = todo {
        a.apply(x);
    }
}

fn view_list(a: &mut Notebooks, ui: &mut Ui, todo: &mut Option<Todo>) {
    let t = ui.theme();
    ui.pad(20.0);
    let mut bar = ui.cut_top(t.px(40.0));
    if button_right(ui, &mut bar, "New notebook", true) {
        *todo = Some(Todo::NewNotebook);
    }
    let mut hit = |ui: &mut Ui, label: &str, on: bool, r: Rect| {
        let clicked = if on { ui.within(r, |ui| ui.primary_button(label)) } else { ui.button_at(r, ("filter", usize::from(label == "Archived")), label) };
        if clicked && !on {
            *todo = Some(Todo::Filter(label == "Archived"));
        }
    };
    let w = t.px(100.0);
    hit(ui, "Active", !a.archived, Rect::new(bar.x, bar.y + t.px(3.0), w, t.px(34.0)));
    hit(ui, "Archived", a.archived, Rect::new(bar.x + w + t.px(8.0), bar.y + t.px(3.0), w, t.px(34.0)));
    let context = match (a.current_list(), a.archived) {
        (Some(l), false) => count(l.len() as u32, "No notebooks", "notebook", "notebooks"),
        (Some(l), true) => count(l.len() as u32, "Nothing archived", "archived notebook", "archived notebooks"),
        _ => String::new(),
    };
    let info = Rect::new(bar.x + 2 * w + t.px(24.0), bar.y, bar.w - 2 * w - t.px(24.0), bar.h);
    let line = if a.current_load().is_loading() && a.current_list().is_some() { format!("{context} · Updating…") } else { context };
    ui.text_in(info, Face::Sans, t.pt(14.0), t.dim, &line);
    ui.cut_top(t.px(8.0));

    let credit = ui.cut_bottom(t.px(22.0));
    ui.text_in(credit, Face::Sans, t.pt(12.0), t.dim, CREDIT);
    let failed = match a.current_load() {
        lucina_app::Load::Failed(m, _) => Some(m.clone()),
        _ => None,
    };
    if let Some(m) = &failed {
        let row = ui.cut_bottom(t.px(40.0));
        ui.within(row, |ui| {
            if ui.button("Try again") {
                *todo = Some(Todo::ReloadList);
            }
        });
        notice_line(ui, m);
    }
    let r = ui.area();
    let archived = a.archived;
    let zone = a.zone.clone();
    let Some(list) = a.lists[usize::from(archived)].value() else {
        if failed.is_none() {
            ui.within(r, |ui| ui.dim("Loading…"));
        }
        return;
    };
    if list.is_empty() {
        ui.within(r, |ui| {
            ui.heading(if archived { "Nothing archived" } else { "No notebooks yet" });
            ui.space(4.0);
            ui.paragraph(if archived {
                "Notebooks you archive wait here."
            } else {
                "A notebook gathers sources (pages, PDFs, videos, text) and your notes, and answers questions from them with citations."
            });
        });
        return;
    }
    let res = ui.list("notebooks", &mut a.list, r, list.len(), 58.0, |ui, i, _| {
        let row = ui.area();
        let nb = &list[i];
        let name = if nb.archived { format!("{} · Archived", nb.name) } else { nb.name.clone() };
        let counts = counts_line(&nb.counts);
        let sub = if nb.description.is_empty() { counts } else { format!("{} · {counts}", nb.description) };
        two_lines(ui, row, &name, &sub, &date_label(&nb.updated_at, &zone));
    });
    if let Some(i) = res.activated {
        *todo = Some(Todo::OpenNotebook(i));
    }
}

fn new_modal(a: &mut Notebooks, ui: &mut Ui, todo: &mut Option<Todo>) {
    let Some(f) = &mut a.new_form else { return };
    let t = ui.theme();
    let mut close = ui.key(keys::KEY_ESC);
    ui.modal(560.0, 340.0, |ui| {
        ui.heading("New notebook");
        ui.space(6.0);
        if std::mem::take(&mut f.first) {
            ui.focus("new-name");
        }
        ui.dim("Name");
        if ui.text_field("new-name", &mut f.name).submitted {
            *todo = Some(Todo::CreateNotebook);
        }
        ui.dim("Description");
        ui.text_area("new-description", &mut f.description, 3);
        if let Some(e) = &f.error {
            ui.wrapped(Face::Sans, t.pt(14.0), t.danger, e);
        }
        ui.space(8.0);
        ui.row(40.0, |ui| {
            if ui.button_id("new-cancel", "Cancel") {
                close = true;
            }
            if ui.primary_button(if f.busy { "Saving…" } else { "Create" }) {
                *todo = Some(Todo::CreateNotebook);
            }
        });
    });
    if close {
        a.new_form = None;
        a.want_focus = Some("notebooks");
    }
}

fn view_page(a: &mut Notebooks, ui: &mut Ui, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let zone = a.zone.clone();
    let Some(p) = &mut a.page else { return };
    if p.sheet.is_none() && p.reader.is_none() && p.confirm.is_none() && p.chat.picker.is_none() {
        if let Some(k) = ui.press() {
            if k.mods.ctrl && !k.mods.alt {
                let hit = match k.code {
                    keys::KEY_1 => Some(Todo::Tab(Tab::Sources)),
                    keys::KEY_2 => Some(Todo::Tab(Tab::Notes)),
                    keys::KEY_3 => Some(Todo::Tab(Tab::Chat)),
                    KEY_F => Some(Todo::Sheet(SheetKind::Search)),
                    _ => None,
                };
                if let Some(h) = hit {
                    ui.take_press();
                    *todo = Some(h);
                }
            }
        }
    }
    ui.pad(14.0);
    let mut bar = ui.cut_top(t.px(40.0));
    let back = Rect::new(bar.x, bar.y + t.px(3.0), t.px(120.0), t.px(34.0));
    if ui.button_at(back, "back", "‹ Notebooks") {
        *todo = Some(Todo::Back);
    }
    bar.x += back.w + t.px(10.0);
    bar.w -= back.w + t.px(10.0);
    if button_right(ui, &mut bar, "Add source", true) {
        *todo = Some(Todo::Sheet(SheetKind::Add));
    }
    if button_right(ui, &mut bar, "Settings", false) {
        *todo = Some(Todo::Sheet(SheetKind::Settings));
    }
    if button_right(ui, &mut bar, "Podcast", false) {
        *todo = Some(Todo::Sheet(SheetKind::Podcast));
    }
    if button_right(ui, &mut bar, "Search", false) {
        *todo = Some(Todo::Sheet(SheetKind::Search));
    }
    match p.detail.value() {
        Some(d) => {
            let top = Rect::new(bar.x, bar.y, bar.w, bar.h / 2 + t.px(2.0));
            let low = Rect::new(bar.x, top.bottom(), bar.w, bar.h - top.h);
            ui.text_in(top, Face::Bold, t.pt(17.0), t.text, &d.notebook.name);
            let mut bits = Vec::new();
            if !d.notebook.description.is_empty() {
                bits.push(d.notebook.description.clone());
            }
            bits.push(count(d.notebook.counts.sources, "No sources", "source", "sources"));
            bits.push(count(d.notebook.counts.notes, "No notes", "note", "notes"));
            if d.notebook.archived {
                bits.push("Archived".into());
            }
            if p.detail.is_loading() && !d.has_pending() {
                bits.push("Updating…".into());
            }
            ui.text_in(low, Face::Sans, t.pt(13.0), t.dim, &bits.join(" · "));
        }
        None => ui.text_in(bar, Face::Sans, t.pt(14.0), t.dim, "Loading…"),
    }
    ui.cut_top(t.px(6.0));

    if p.not_found {
        ui.dim("This notebook doesn't exist.");
    }
    if let lucina_app::Load::Failed(m, value) = &p.detail {
        let m = m.clone();
        let none = value.is_none();
        if !p.not_found {
            let row = ui.cut_bottom(t.px(40.0));
            ui.within(row, |ui| {
                if ui.button("Try again") {
                    *todo = Some(Todo::Reload);
                }
            });
            notice_line(ui, &m);
        }
        if none {
            page_overlays(p, ui, todo, &zone);
            esc(a, ui, todo);
            return;
        }
    }
    if p.detail.value().is_none() {
        esc(a, ui, todo);
        return;
    }
    if let Some(m) = p.notice.clone() {
        let mut row = ui.cut_top(t.px(38.0));
        if button_right(ui, &mut row, "Dismiss", false) {
            p.notice = None;
        }
        ui.text_in(row, Face::Sans, t.pt(14.0), t.danger, &m);
    }

    let area = ui.area();
    if area.w >= WIDE_PX {
        let gap = t.px(10.0);
        let left = (area.w - 2 * gap) * 3 / 10;
        let middle = (area.w - 2 * gap) * 22 / 100;
        let chat = area.w - 2 * gap - left - middle;
        let (a1, a2, a3) = (
            Rect::new(area.x, area.y, left, area.h),
            Rect::new(area.x + left + gap, area.y, middle, area.h),
            Rect::new(area.x + left + middle + 2 * gap, area.y, chat, area.h),
        );
        sources_pane(p, ui, a1, todo);
        notes_pane(p, ui, a2, todo);
        chat_pane(p, ui, a3, todo);
    } else {
        let tabs = ui.cut_top(t.px(40.0));
        let w = t.px(110.0);
        for (i, (tab, label)) in [(Tab::Sources, "Sources"), (Tab::Notes, "Notes"), (Tab::Chat, "Chat")].into_iter().enumerate() {
            let r = Rect::new(tabs.x + (w + t.px(8.0)) * i as i32, tabs.y + t.px(2.0), w, t.px(34.0));
            let clicked = if p.tab == tab { ui.within(r, |ui| ui.primary_button(label)) } else { ui.button_at(r, ("tab", i), label) };
            if clicked {
                *todo = Some(Todo::Tab(tab));
            }
        }
        ui.cut_top(t.px(6.0));
        let r = ui.area();
        match p.tab {
            Tab::Sources => sources_pane(p, ui, r, todo),
            Tab::Notes => notes_pane(p, ui, r, todo),
            Tab::Chat => chat_pane(p, ui, r, todo),
        }
    }
    page_overlays(p, ui, todo, &zone);
    esc(a, ui, todo);
}

/// One step back. Asking takes the key, so this comes after every widget.
fn esc(a: &mut Notebooks, ui: &mut Ui, todo: &mut Option<Todo>) {
    let Some(p) = &mut a.page else { return };
    if ui.key(keys::KEY_ESC) {
        *todo = Some(if p.confirm.is_some() {
            Todo::CloseConfirm
        } else if p.chat.picker.is_some() || p.sheet.is_some() {
            Todo::CloseSheet
        } else if p.reader.is_some() {
            Todo::CloseReader
        } else {
            Todo::Back
        });
    }
}

fn sources_pane(p: &mut Page, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let inner = panel(ui, r);
    ui.within(inner, |ui| {
        let mut title = pane_title(ui, "Sources");
        if button_right(ui, &mut title, "Add", false) {
            *todo = Some(Todo::Sheet(SheetKind::Add));
        }
        let Some(d) = p.detail.value() else { return };
        if d.sources.is_empty() {
            ui.paragraph("Add a web page, a YouTube link, a PDF or some text. Lucina reads it and the chat can answer from it.");
            return;
        }
        let selected = p.sources.selected.and_then(|i| d.sources.get(i).map(|s| (i, s)));
        if let Some((i, s)) = selected {
            let mut row = ui.cut_bottom(t.px(38.0));
            if button_right(ui, &mut row, "Remove", false) {
                *todo = Some(Todo::AskRemoveSource(i));
            }
            if s.status == "failed" && button_right(ui, &mut row, "Retry", false) {
                *todo = Some(Todo::Retry(i));
            }
            let label = format!("In chat: {}", context_label(s.context.as_deref().unwrap_or("full")));
            if button_right(ui, &mut row, &label, false) {
                *todo = Some(Todo::ToggleContext(i));
            }
            if s.status == "failed" {
                notice_line(ui, s.error.as_deref().unwrap_or("This didn't work."));
            }
        }
        let area = ui.area();
        let res = ui.list("sources", &mut p.sources, area, d.sources.len(), 56.0, |ui, i, _| {
            let s = &d.sources[i];
            let mut bits = Vec::new();
            if s.status != "ready" {
                bits.push(status_label(&s.status).to_owned());
            }
            let kind = kind_label(&s.kind);
            bits.push(kind.to_owned());
            let origin = origin_label(&s.origin);
            // Pasted text has "text" for an origin, which says nothing after its kind.
            if !origin.is_empty() && !origin.eq_ignore_ascii_case(kind) {
                bits.push(origin.clone());
            }
            if s.status == "ready" && s.chunks > 0 {
                bits.push(count(s.chunks, "", "passage", "passages"));
            }
            if let Some(n) = s.pages.filter(|n| *n > 0) {
                bits.push(count(n, "", "page", "pages"));
            }
            let row = ui.area();
            // A link that could not be read has no title yet.
            let title = if s.title.is_empty() { origin.as_str() } else { s.title.as_str() };
            two_lines(ui, row, title, &bits.join(" · "), "");
        });
        if let Some(i) = res.activated {
            *todo = Some(Todo::OpenSource(i));
        }
    });
}

fn notes_pane(p: &mut Page, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let inner = panel(ui, r);
    ui.within(inner, |ui| {
        let mut title = pane_title(ui, "Notes");
        if button_right(ui, &mut title, "Write", false) {
            *todo = Some(Todo::Sheet(SheetKind::Write));
        }
        if button_right(ui, &mut title, "Link", false) {
            *todo = Some(Todo::Sheet(SheetKind::Link));
        }
        let Some(d) = p.detail.value() else { return };
        if d.notes.is_empty() {
            ui.paragraph("Notes you write here or link from Keep show up with the notebook.");
            return;
        }
        if let Some(i) = p.notes.selected.filter(|i| *i < d.notes.len()) {
            let mut row = ui.cut_bottom(t.px(38.0));
            if button_right(ui, &mut row, "Unlink", false) {
                *todo = Some(Todo::Unlink(i));
            }
        }
        let area = ui.area();
        let res = ui.list("notes", &mut p.notes, area, d.notes.len(), 56.0, |ui, i, _| {
            let n = &d.notes[i];
            let title = if n.title.is_empty() { &n.slug } else { &n.title };
            let sub = md::plain_excerpt(&n.excerpt, &n.title);
            let row = ui.area();
            two_lines(ui, row, title, if sub.is_empty() { &n.slug } else { &sub }, "");
        });
        if let Some(i) = res.activated {
            *todo = Some(Todo::OpenNote(i));
        }
    });
}

fn message_height(text: &mut lucina_app::gfx::Text, m: &ShownMessage, t: &lucina_app::Theme, w: i32) -> i32 {
    let inner = w - 2 * t.px(10.0);
    let px = t.pt(13.0);
    let line = Ui::wrapped_height(text, Face::Sans, px, "X", inner);
    let mut h = t.px(8.0) + md::height(text, &m.doc, t, inner) + t.px(4.0);
    if !m.is_user() {
        h += m.cited.len() as i32 * (line + t.px(4.0)) + line + t.px(6.0);
    }
    h
}

fn draw_message(ui: &mut Ui, m: &ShownMessage, i: usize, saved: bool, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let row = ui.area();
    if m.is_user() {
        ui.fill_round(row.inset(t.px(2.0)), t.px(6.0), t.panel_hi);
    }
    let inner = Rect::new(row.x + t.px(10.0), row.y + t.px(4.0), row.w - 2 * t.px(10.0), row.h - t.px(8.0));
    md::draw(ui, &m.doc, inner);
    if m.is_user() {
        return;
    }
    let px = t.pt(13.0);
    let line = ui.line_height(Face::Sans, px);
    let w = inner.w;
    let foot_h = line + t.px(6.0);
    let foot_y = row.bottom() - foot_h;
    let mut y = foot_y - m.cited.len() as i32 * (line + t.px(4.0));
    for (k, c) in m.cited.iter().enumerate() {
        let r = Rect::new(inner.x, y, w, line + t.px(2.0));
        let page = c.page.map_or(String::new(), |p| format!(" · p. {p}"));
        ui.text_in(r, Face::Sans, px, t.accent, &format!("[{}] {}{page}", k + 1, c.title));
        if ui.hit(("cite", i * 100 + k), r).clicked {
            *todo = Some(Todo::Cite(i, k));
        }
        y += line + t.px(4.0);
    }
    let model = match &m.message.model {
        Some(s) if s.to_lowercase().contains("qwen") => "Local (Qwen)",
        Some(_) => "DeepSeek",
        None => "",
    };
    let foot = Rect::new(inner.x, foot_y, t.px(130.0), line + t.px(2.0));
    ui.text_in(foot, Face::Sans, px, t.dim, model);
    let save = Rect::new(inner.x + t.px(130.0), foot_y, t.px(170.0), line + t.px(2.0));
    if saved {
        ui.text_in(save, Face::Sans, px, t.ok, "Saved to Keep");
    } else {
        ui.text_in(save, Face::Sans, px, t.accent, "Save to note");
        if ui.hit(("save", i), save).clicked {
            *todo = Some(Todo::SaveReply(i));
        }
    }
}

fn chat_pane(p: &mut Page, ui: &mut Ui, r: Rect, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let has_sources = p.detail.value().is_some_and(|d| d.sources.iter().any(|s| s.status == "ready"));
    let inner = panel(ui, r);
    let c = &mut p.chat;
    ui.within(inner, |ui| {
        let mut title = pane_title(ui, "Chat");
        let sending = c.sending();
        if button_right(ui, &mut title, "Delete", false) && !sending {
            *todo = Some(Todo::AskDeleteChat);
        }
        if button_right(ui, &mut title, "Rename", false) && !sending {
            *todo = Some(Todo::OpenRename);
        }
        if button_right(ui, &mut title, "New", false) && !sending {
            *todo = Some(Todo::NewChat);
        }
        let name = c.shown_title();
        let pick = Rect::new(title.x, title.y + t.px(1.0), title.w.max(t.px(60.0)), t.px(34.0));
        if ui.button_at(pick, "chat-pick", &name) && !sending {
            *todo = Some(Todo::OpenPicker);
        }

        // The composer: Enter sends, Shift+Enter starts a new line.
        let mut row = ui.cut_bottom(t.px(38.0));
        if button_right(ui, &mut row, "Send", true) {
            *todo = Some(Todo::Send);
        }
        let model = if c.model == "qwen" { "Model: Local (Qwen)" } else { "Model: DeepSeek" };
        if button_right(ui, &mut row, model, false) {
            *todo = Some(Todo::CycleModel);
        }
        if ui.is_focused("log") {
            ui.text_in(row, Face::Sans, t.pt(12.0), t.dim, "1-9 opens a citation · S saves to a note");
        }
        let px = t.pt(15.0);
        let box_h = 2 * ui.line_height(Face::Sans, px) + 2 * t.px(6.0);
        let composer = ui.cut_bottom(box_h + t.px(4.0));
        if ui.is_focused("ask") {
            if let Some(k) = ui.press() {
                if k.code == keys::KEY_ENTER && plain(k.mods) {
                    ui.take_press();
                    *todo = Some(Todo::Send);
                }
            }
        }
        ui.within(composer, |ui| {
            ui.text_area("ask", &mut c.draft, 2);
        });
        if let Some(e) = c.error.clone() {
            notice_line(ui, &e);
        }

        let n = c.messages.len() + if c.pending.is_some() { 2 } else { 0 };
        let area = ui.area();
        if n == 0 {
            ui.within(area, |ui| {
                if c.loading {
                    ui.dim("Loading…");
                } else if has_sources {
                    ui.paragraph("Ask something about your sources. Answers cite the passages they come from.");
                } else {
                    ui.paragraph("Add a source first, then ask about it here.");
                }
            });
            return;
        }
        if ui.is_focused("log") {
            if let Some(k) = ui.press() {
                let target = c.log.selected.or_else(|| c.messages.iter().rposition(|m| !m.is_user()));
                if plain(k.mods) && (keys::KEY_1..=KEY_9).contains(&k.code) {
                    if let Some(i) = target {
                        ui.take_press();
                        *todo = Some(Todo::Cite(i, usize::from(k.code - keys::KEY_1)));
                    }
                } else if plain(k.mods) && k.code == KEY_S {
                    if let Some(i) = target.filter(|i| c.messages.get(*i).is_some_and(|m| !m.is_user())) {
                        ui.take_press();
                        *todo = Some(Todo::SaveReply(i));
                    }
                }
            }
        }
        let theme = t;
        let (messages, pending, saved) = (&c.messages, &c.pending, &c.saved);
        let count = messages.len();
        let res = ui.list_measured(
            "log",
            &mut c.log,
            area,
            n,
            |text, i, w| {
                if let Some(m) = messages.get(i) {
                    return message_height(text, m, &theme, w);
                }
                let inner = w - 2 * theme.px(10.0);
                let Some(pe) = pending else { return 1 };
                if i == count {
                    theme.px(12.0) + md::height(text, &pe.user, &theme, inner)
                } else {
                    let thinking = Ui::wrapped_height(text, Face::Sans, theme.pt(15.0), "X", inner);
                    theme.px(12.0) + md::height(text, &pe.doc, &theme, inner).max(thinking)
                }
            },
            |ui, i, _| {
                if let Some(m) = messages.get(i) {
                    draw_message(ui, m, i, saved.contains_key(&m.message.id), todo);
                    return;
                }
                let Some(pe) = pending else { return };
                let row = ui.area();
                let inner = Rect::new(row.x + t.px(10.0), row.y + t.px(4.0), row.w - 2 * t.px(10.0), row.h - t.px(8.0));
                if i == count {
                    ui.fill_round(row.inset(t.px(2.0)), t.px(6.0), t.panel_hi);
                    md::draw(ui, &pe.user, inner);
                } else if pe.reply.is_empty() {
                    ui.text_in(inner, Face::Sans, t.pt(15.0), t.dim, "Reading your sources…");
                } else {
                    md::draw(ui, &pe.doc, inner);
                }
            },
        );
        if let Some(i) = res.activated {
            if messages.get(i).is_some_and(|m| !m.cited.is_empty()) {
                *todo = Some(Todo::Cite(i, 0));
            }
        }
    });
}

fn page_overlays(p: &mut Page, ui: &mut Ui, todo: &mut Option<Todo>, zone: &lucina_app::time::Zone) {
    if let Some(c) = &mut p.confirm {
        let mut pick = None;
        ui.modal(520.0, 190.0, |ui| {
            // The stray Enter lands on Cancel.
            if std::mem::take(&mut c.first) {
                ui.focus("confirm-no");
            }
            ui.paragraph(&c.text);
            ui.space(10.0);
            ui.row(40.0, |ui| {
                if ui.button_id("confirm-no", "Cancel") {
                    pick = Some(false);
                }
                if ui.button_id("confirm-yes", &c.yes) {
                    pick = Some(true);
                }
            });
        });
        match pick {
            Some(true) => {
                let action = c.action.clone();
                p.confirm = None;
                p.refocus = true;
                *todo = Some(action);
            }
            Some(false) => {
                p.confirm = None;
                p.refocus = true;
            }
            None => {}
        }
        return;
    }
    if p.chat.picker.is_some() {
        chat_picker(p, ui, todo);
    } else if p.sheet.is_some() {
        sheet(p, ui, todo, zone);
    } else if p.reader.is_some() {
        reader(p, ui, todo);
    }
}

fn chat_picker(p: &mut Page, ui: &mut Ui, todo: &mut Option<Todo>) {
    let c = &mut p.chat;
    let Some(state) = &mut c.picker else { return };
    let chats = &c.chats;
    ui.modal(560.0, 420.0, |ui| {
        ui.heading("Chats");
        ui.space(6.0);
        if chats.is_empty() {
            ui.dim("No chats yet.");
            return;
        }
        let r = ui.area();
        let res = ui.list("chat-picker", state, r, chats.len(), 40.0, |ui, i, _| {
            let t = ui.theme();
            let row = ui.area().inset(t.px(8.0));
            ui.text_in(row, Face::Sans, t.pt(15.0), t.text, &chats[i].title);
        });
        if let Some(i) = res.activated {
            *todo = Some(Todo::PickChat(i));
        }
    });
}

fn form_error(ui: &mut Ui, e: &Option<String>) {
    let t = ui.theme();
    if let Some(m) = e {
        ui.wrapped(Face::Sans, t.pt(14.0), t.danger, m);
        ui.space(4.0);
    }
}

/// `NotePicker.tsx`, with what Enter on a row does here: it picks the note and submits.
fn picker_view(ui: &mut Ui, picker: &mut Picker, id: (&'static str, &'static str), todo: &mut Option<Todo>) {
    let was = picker.query.text.clone();
    let resp = ui.text_field(id.0, &mut picker.query);
    if picker.query.text != was {
        picker.due = Some(Instant::now() + if picker.query.text.trim().is_empty() { std::time::Duration::ZERO } else { PICK_WAIT });
    }
    if (resp.submitted || (resp.focused && ui.key(keys::KEY_DOWN))) && !picker.rows().is_empty() {
        ui.focus(id.1);
    }
    form_error(ui, &picker.error);
    let rows = picker.rows();
    let r = ui.area();
    if rows.is_empty() {
        if picker.notes.is_some() {
            ui.dim("No notes match.");
        }
        return;
    }
    let res = ui.list(id.1, &mut picker.list, r, rows.len(), 50.0, |ui, i, _| {
        let row = ui.area();
        two_lines(ui, row, &rows[i].1, &rows[i].2, "");
    });
    if let Some(i) = res.activated {
        *todo = Some(Todo::SubmitLink(rows[i].0.clone()));
    }
}

fn sheet(p: &mut Page, ui: &mut Ui, todo: &mut Option<Todo>, zone: &lucina_app::time::Zone) {
    let podcasts = p.detail.value().map_or(Vec::new(), |d| d.podcasts.clone());
    let archived = p.detail.value().is_some_and(|d| d.notebook.archived);
    let Some(s) = &mut p.sheet else { return };
    match s {
        Sheet::Add(f) => {
            let t = ui.theme();
            ui.modal(680.0, 520.0, |ui| {
                ui.heading("Add source");
                ui.space(4.0);
                let modes = [
                    (AddMode::Link, "Link"),
                    (AddMode::Text, "Paste text"),
                    (AddMode::File, "Upload"),
                    (AddMode::Path, "Lucina's computer"),
                    (AddMode::Note, "Keep note"),
                ];
                ui.row(38.0, |ui| {
                    for (i, (m, label)) in modes.into_iter().enumerate() {
                        let hit = if f.mode == m { ui.primary_button(label) } else { ui.button_id(("add-mode", i), label) };
                        if hit {
                            *todo = Some(Todo::AddMode(m));
                        }
                    }
                });
                ui.space(6.0);
                form_error(ui, &f.error);
                let first = std::mem::take(&mut f.first);
                match f.mode {
                    AddMode::Link => {
                        if first {
                            ui.focus("add-url");
                        }
                        ui.dim("Link");
                        if ui.text_field("add-url", &mut f.url).submitted {
                            *todo = Some(Todo::SubmitAdd);
                        }
                        ui.dim("A web page or a YouTube video.");
                    }
                    AddMode::Text => {
                        if first {
                            ui.focus("add-title");
                        }
                        ui.dim("Title (optional)");
                        ui.text_field("add-title", &mut f.title);
                        ui.dim("Text");
                        ui.text_area("add-text", &mut f.text, 7);
                    }
                    AddMode::File => {
                        ui.paragraph("Juno has no file picker yet, so a file cannot be chosen here. Add it from Secretary on another device, and it shows here.");
                    }
                    AddMode::Path => {
                        if first {
                            ui.focus("add-path");
                        }
                        ui.dim("Path");
                        if ui.text_field("add-path", &mut f.path).submitted {
                            *todo = Some(Todo::SubmitAdd);
                        }
                        ui.dim("A file on Lucina's computer, starting with /.");
                    }
                    AddMode::Note => {
                        if first {
                            ui.focus("add-pick");
                        }
                        let bottom = ui.cut_bottom(t.px(44.0));
                        ui.within(bottom, |ui| {
                            if ui.button_id("add-cancel-note", "Cancel") {
                                *todo = Some(Todo::CloseSheet);
                            }
                        });
                        picker_view(ui, &mut f.picker, ("add-pick", "add-picks"), todo);
                        return;
                    }
                }
                let bottom = ui.cut_bottom(t.px(44.0));
                ui.within(bottom, |ui| {
                    ui.row(40.0, |ui| {
                        if ui.button_id("add-cancel", "Cancel") {
                            *todo = Some(Todo::CloseSheet);
                        }
                        if f.mode != AddMode::File && ui.primary_button(if f.busy { "Adding…" } else { "Add" }) {
                            *todo = Some(Todo::SubmitAdd);
                        }
                    });
                });
            });
        }
        Sheet::Write(f) => {
            ui.modal(640.0, 480.0, |ui| {
                ui.heading("Write note");
                ui.space(6.0);
                if std::mem::take(&mut f.first) {
                    ui.focus("write-title");
                }
                form_error(ui, &f.error);
                ui.dim("Title");
                ui.text_field("write-title", &mut f.title);
                ui.dim("Text");
                ui.text_area("write-text", &mut f.text, 8);
                ui.dim("Saved in Keep with the label \"notebook\" and linked here.");
                ui.space(6.0);
                ui.row(40.0, |ui| {
                    if ui.button_id("write-cancel", "Cancel") {
                        *todo = Some(Todo::CloseSheet);
                    }
                    if ui.primary_button(if f.busy { "Saving…" } else { "Save note" }) {
                        *todo = Some(Todo::SubmitWrite);
                    }
                });
            });
        }
        Sheet::Link(f) => {
            let t = ui.theme();
            ui.modal(640.0, 500.0, |ui| {
                ui.heading("Link note");
                ui.space(6.0);
                if std::mem::take(&mut f.first) {
                    ui.focus("link-pick");
                }
                form_error(ui, &f.error);
                let bottom = ui.cut_bottom(t.px(44.0));
                ui.within(bottom, |ui| {
                    if ui.button_id("link-cancel", "Cancel") {
                        *todo = Some(Todo::CloseSheet);
                    }
                });
                picker_view(ui, &mut f.picker, ("link-pick", "link-picks"), todo);
            });
        }
        Sheet::Search(f) => {
            ui.modal(720.0, 540.0, |ui| {
                ui.heading("Search this notebook");
                ui.space(6.0);
                if std::mem::take(&mut f.first) {
                    ui.focus("search-field");
                }
                let was = f.query.text.clone();
                let resp = ui.text_field("search-field", &mut f.query);
                if f.query.text != was {
                    if f.query.text.trim().chars().count() < 2 {
                        f.hits = None;
                        f.due = None;
                    } else {
                        f.due = Some(Instant::now() + SEARCH_WAIT);
                    }
                }
                let has = f.hits.as_ref().is_some_and(|h| !h.is_empty());
                if has && (resp.submitted || (resp.focused && ui.key(keys::KEY_DOWN))) {
                    ui.focus("search-hits");
                }
                form_error(ui, &f.error);
                ui.space(4.0);
                match &f.hits {
                    None => ui.dim("Type at least two letters."),
                    Some(h) if h.is_empty() => ui.dim(&format!("Nothing found for \"{}\".", f.query.text.trim())),
                    Some(h) => {
                        let r = ui.area();
                        let res = ui.list("search-hits", &mut f.list, r, h.len(), 58.0, |ui, i, _| {
                            let hit = &h[i];
                            let kind = match hit.kind.as_str() {
                                "note" => "Note",
                                "insight" => "Insight",
                                _ => "Source",
                            };
                            let row = ui.area();
                            two_lines(ui, row, &hit.title, &md::plain_excerpt(&hit.excerpt, ""), kind);
                        });
                        if let Some(i) = res.activated {
                            *todo = Some(Todo::OpenHit(i));
                        }
                    }
                }
            });
        }
        Sheet::Podcast(f) => podcast(ui, f, &podcasts, todo, zone),
        Sheet::Settings(f) => {
            ui.modal(620.0, 560.0, |ui| {
                ui.heading("Notebook settings");
                ui.space(6.0);
                if std::mem::take(&mut f.first) {
                    ui.focus("set-name");
                }
                form_error(ui, &f.error);
                ui.dim("Name");
                if ui.text_field("set-name", &mut f.name).submitted {
                    *todo = Some(Todo::SaveSettings);
                }
                ui.dim("Description");
                ui.text_area("set-description", &mut f.description, 3);
                ui.space(4.0);
                if ui.primary_button("Save") {
                    *todo = Some(Todo::SaveSettings);
                }
                ui.space(10.0);
                ui.separator();
                ui.space(6.0);
                ui.dim(if archived { "Bring this notebook back to the list." } else { "Hide this notebook from the list. Nothing is deleted." });
                if ui.button_id("set-archive", if archived { "Restore" } else { "Archive" }) {
                    *todo = Some(Todo::Archive);
                }
                ui.space(10.0);
                ui.separator();
                ui.space(6.0);
                ui.dim("Deletes the notebook, its chats and podcasts. Linked Keep notes stay in Keep.");
                ui.checkbox("Also delete sources no other notebook uses", &mut f.delete_exclusive);
                ui.space(4.0);
                if ui.button_id("set-delete", "Delete notebook") {
                    *todo = Some(Todo::AskDeleteNotebook);
                }
            });
        }
        Sheet::Note(n) => {
            let (title, excerpt, slug) = (n.title.clone(), md::plain_excerpt(&n.excerpt, &n.title), n.slug.clone());
            ui.modal(620.0, 320.0, |ui| {
                ui.heading(if title.is_empty() { &slug } else { &title });
                ui.space(6.0);
                ui.paragraph(&excerpt);
                ui.space(8.0);
                ui.dim(&format!("The whole note is \"{slug}\" in the Notes app."));
                ui.space(8.0);
                if ui.button_id("note-close", "Close") {
                    *todo = Some(Todo::CloseSheet);
                }
            });
        }
        Sheet::Rename(e, first) => {
            ui.modal(520.0, 190.0, |ui| {
                ui.heading("Rename chat");
                ui.space(6.0);
                if std::mem::take(first) {
                    ui.focus("rename");
                }
                if ui.text_field("rename", e).submitted {
                    *todo = Some(Todo::SaveRename);
                }
                ui.space(8.0);
                ui.row(40.0, |ui| {
                    if ui.button_id("rename-cancel", "Cancel") {
                        *todo = Some(Todo::CloseSheet);
                    }
                    if ui.primary_button("Save") {
                        *todo = Some(Todo::SaveRename);
                    }
                });
            });
        }
    }
}

fn podcast(ui: &mut Ui, f: &mut crate::app::PodcastForm, podcasts: &[crate::wire::Podcast], todo: &mut Option<Todo>, zone: &lucina_app::time::Zone) {
    ui.modal(860.0, 640.0, |ui| {
        let t = ui.theme();
        ui.heading("Podcast");
        ui.dim("Lucina writes a script from the notebook's sources and voices it.");
        if std::mem::take(&mut f.first) {
            ui.focus("pod-minutes");
        }
        form_error(ui, &f.error);
        ui.space(4.0);
        let episode = f.episodes.get(f.episode).map_or("…", |p| p.name.as_str()).to_owned();
        let speaker = f.speakers.get(f.speaker).map_or("…", |p| p.name.as_str()).to_owned();
        ui.row(38.0, |ui| {
            if ui.button_id("pod-episode", &format!("Format: {episode}")) {
                *todo = Some(Todo::CycleEpisode);
            }
            if ui.button_id("pod-speaker", &format!("Voices: {speaker}")) {
                *todo = Some(Todo::CycleSpeaker);
            }
        });
        ui.space(4.0);
        ui.dim("Minutes");
        let row = ui.cut_top(t.px(36.0));
        ui.text_field_at(Rect::new(row.x, row.y, t.px(120.0), t.px(34.0)), "pod-minutes", &mut f.minutes);
        ui.dim("Briefing");
        ui.text_area("pod-briefing", &mut f.briefing, 2);
        ui.space(4.0);
        if ui.primary_button(if f.busy { "Creating…" } else { "Create podcast" }) {
            *todo = Some(Todo::CreatePodcast);
        }
        ui.space(6.0);
        ui.heading("Episodes");
        if podcasts.is_empty() {
            ui.dim("No episodes yet.");
            return;
        }
        if f.list.selected.is_none() {
            f.list.select(0);
        }
        let selected = f.list.selected.filter(|i| *i < podcasts.len());
        if let Some(i) = selected {
            let bottom = ui.cut_bottom(t.px(40.0));
            ui.within(bottom, |ui| {
                ui.row(38.0, |ui| {
                    if ui.button_id("pod-delete", "Delete") {
                        *todo = Some(Todo::AskDeletePodcast(i));
                    }
                });
            });
        }
        let r = ui.area();
        let open = f.open.clone();
        let theme = t;
        let res = ui.list_measured(
            "pod-list",
            &mut f.list,
            r,
            podcasts.len(),
            |text, i, w| {
                let inner = w - 2 * theme.px(10.0);
                let p = &podcasts[i];
                let mut h = theme.px(50.0);
                if p.status == "failed" {
                    h += Ui::wrapped_height(text, Face::Sans, theme.pt(13.0), p.error.as_deref().unwrap_or("This didn't work."), inner);
                }
                if p.audio {
                    h += Ui::wrapped_height(text, Face::Sans, theme.pt(13.0), NO_SOUND, inner);
                }
                if open.as_deref() == Some(p.id.as_str()) {
                    for l in &p.transcript {
                        h += Ui::wrapped_height(text, Face::Sans, theme.pt(14.0), &format!("{}: {}", l.speaker, l.text), inner) + theme.px(3.0);
                    }
                }
                h
            },
            |ui, i, _| {
                let p = &podcasts[i];
                let row = ui.area();
                let mut sub = vec![p.episode_profile.clone(), p.speaker_profile.clone()];
                if let Some(d) = p.duration_s {
                    sub.push(clock(d));
                }
                sub.push(date_label(&p.created_at, zone));
                let head = Rect::new(row.x, row.y, row.w, t.px(48.0));
                let state = status_label(&p.status);
                let state = if podcast_pending(&p.status) || p.status == "failed" { state } else { "" };
                two_lines(ui, head, &p.title, &sub.join(" · "), state);
                let body = Rect::new(row.x + t.px(10.0), head.bottom(), row.w - 2 * t.px(10.0), row.bottom() - head.bottom());
                ui.within(body, |ui| {
                    if p.status == "failed" {
                        ui.wrapped(Face::Sans, t.pt(13.0), t.danger, p.error.as_deref().unwrap_or("This didn't work."));
                    }
                    if p.audio {
                        ui.wrapped(Face::Sans, t.pt(13.0), t.dim, NO_SOUND);
                    }
                    if open.as_deref() == Some(p.id.as_str()) {
                        for l in &p.transcript {
                            ui.wrapped(Face::Sans, t.pt(14.0), t.item, &format!("{}: {}", l.speaker, l.text));
                            ui.space(3.0 / t.k);
                        }
                    }
                });
            },
        );
        if let Some(i) = res.activated {
            *todo = Some(Todo::ToggleTranscript(i));
        }
    });
}

const NO_SOUND: &str = "Juno has no sound driver yet, so it cannot play here. Enter shows the transcript.";

fn reader(p: &mut Page, ui: &mut Ui, todo: &mut Option<Todo>) {
    let Some(r) = &mut p.reader else { return };
    ui.modal(1340.0, 740.0, |ui| {
        let t = ui.theme();
        let title = r.source.as_ref().filter(|s| !s.title.is_empty()).map_or("Source".to_owned(), |s| s.title.clone());
        let mut head = ui.cut_top(t.px(36.0));
        if button_right(ui, &mut head, "Close", false) {
            *todo = Some(Todo::CloseReader);
        }
        ui.text_in(head, Face::Bold, t.pt(17.0), t.text, &title);
        if std::mem::take(&mut r.first) {
            ui.focus("passages");
        }
        if let Some(s) = &r.source {
            let mut bits = vec![kind_label(&s.kind).to_owned()];
            let origin = s.origin.strip_prefix("upload:").or_else(|| s.origin.strip_prefix("note:")).unwrap_or(&s.origin);
            if !origin.is_empty() && !origin.eq_ignore_ascii_case(kind_label(&s.kind)) {
                bits.push(origin.to_owned());
            }
            if let Some(n) = s.pages.filter(|n| *n > 0) {
                bits.push(count(n, "", "page", "pages"));
            }
            if let Some(d) = s.duration_s.filter(|d| *d > 0.0) {
                bits.push(clock(d));
            }
            bits.push(status_label(&s.status).to_owned());
            let row = ui.cut_top(t.px(22.0));
            ui.text_in(row, Face::Sans, t.pt(13.0), t.dim, &bits.join(" · "));
            if s.status == "failed" {
                notice_line(ui, s.error.as_deref().unwrap_or("This didn't work."));
            }
        } else if r.loading {
            ui.dim("Loading…");
        }
        if let Some(e) = r.error.clone() {
            notice_line(ui, &e);
        }
        ui.cut_top(t.px(4.0));
        let area = ui.area();
        if area.w >= WIDE_PX * 3 / 4 {
            let left_w = area.w * 58 / 100;
            let left = Rect::new(area.x, area.y, left_w, area.h);
            let right = Rect::new(area.x + left_w + t.px(10.0), area.y, area.w - left_w - t.px(10.0), area.h);
            ui.within(left, |ui| passages(ui, r, todo));
            ui.within(right, |ui| insights(ui, r, todo));
        } else {
            let tabs = ui.cut_top(t.px(40.0));
            let w = t.px(110.0);
            for (i, label) in ["Text", "Insights"].into_iter().enumerate() {
                let tr = Rect::new(tabs.x + (w + t.px(8.0)) * i as i32, tabs.y + t.px(2.0), w, t.px(34.0));
                let on = r.show_insights == (i == 1);
                let clicked = if on { ui.within(tr, |ui| ui.primary_button(label)) } else { ui.button_at(tr, ("reader-tab", i), label) };
                if clicked {
                    *todo = Some(Todo::ReaderPane(i == 1));
                }
            }
            ui.cut_top(t.px(4.0));
            if r.show_insights {
                insights(ui, r, todo);
            } else {
                passages(ui, r, todo);
            }
        }
    });
}

fn passages(ui: &mut Ui, r: &mut crate::app::Reader, todo: &mut Option<Todo>) {
    let t = ui.theme();
    if r.has_earlier() || r.has_later() {
        let bottom = ui.cut_bottom(t.px(40.0));
        ui.within(bottom, |ui| {
            ui.row(38.0, |ui| {
                if r.has_earlier() && ui.button("Earlier passages") {
                    *todo = Some(Todo::MorePassages(Paging::Earlier));
                }
                if r.has_later() && ui.button("More text") {
                    *todo = Some(Todo::MorePassages(Paging::Later));
                }
            });
        });
    }
    if r.chunks.is_empty() {
        if r.source.as_ref().is_some_and(|s| s.status == "ready") {
            ui.dim("No text was extracted.");
        }
        return;
    }
    let area = ui.area();
    let around = r.around;
    let chunks = &r.chunks;
    let theme = t;
    ui.list_measured(
        "passages",
        &mut r.text,
        area,
        chunks.len(),
        |text, i, w| {
            let inner = w - 2 * theme.px(10.0);
            let refs = Ui::wrapped_height(text, Face::Sans, theme.pt(12.0), "X", inner);
            refs + Ui::wrapped_height(text, Face::Sans, theme.pt(15.0), &chunks[i].text, inner) + theme.px(14.0)
        },
        |ui, i, _| {
            let c = &chunks[i];
            let row = ui.area();
            if around == Some(c.ordinal) {
                ui.fill(Rect::new(row.x, row.y + t.px(2.0), t.px(3.0), row.h - t.px(4.0)), t.accent);
            }
            let inner = Rect::new(row.x + t.px(10.0), row.y + t.px(4.0), row.w - 2 * t.px(10.0), row.h - t.px(6.0));
            let label = match (c.page, c.start_s) {
                (Some(p), _) => format!("p. {p}"),
                (_, Some(s)) => clock(s),
                _ => format!("#{}", c.ordinal + 1),
            };
            if ui.painting() {
                ui.within(inner, |ui| {
                    ui.wrapped(Face::Sans, t.pt(12.0), t.dim, &label);
                    ui.wrapped(Face::Sans, t.pt(15.0), t.text, &c.text);
                });
            }
        },
    );
}

fn insights(ui: &mut Ui, r: &mut crate::app::Reader, todo: &mut Option<Todo>) {
    let t = ui.theme();
    let ready = r.source.as_ref().is_some_and(|s| s.status == "ready");
    let mut top = ui.cut_top(t.px(38.0));
    if button_right(ui, &mut top, "Run", true) && ready {
        *todo = Some(Todo::RunTransformation);
    }
    let name = r.transformations.get(r.chosen).map_or("…", |x| x.name.as_str());
    let label = format!("Transformation: {name}");
    let pick = Rect::new(top.x, top.y + t.px(2.0), top.w, t.px(34.0));
    if ui.button_at(pick, "transformation", &label) {
        *todo = Some(Todo::CycleTransformation);
    }
    ui.cut_top(t.px(4.0));
    if r.insights.is_empty() {
        ui.paragraph("No insights yet. Run a transformation to get a summary, key points and more.");
        return;
    }
    if let Some(i) = r.list.selected.filter(|i| *i < r.insights.len()) {
        let status = r.insights[i].insight.status.clone();
        let bottom = ui.cut_bottom(t.px(40.0));
        let saved = r.saved.contains_key(&r.insights[i].insight.id);
        ui.within(bottom, |ui| {
            ui.row(38.0, |ui| {
                if status == "ready" {
                    if saved {
                        ui.dim("Saved to Keep");
                    } else if ui.button("Save as note") {
                        *todo = Some(Todo::SaveInsight(i));
                    }
                }
                if !insight_pending(&status) && ui.button_id("insight-delete", "Delete") {
                    *todo = Some(Todo::AskDeleteInsight(i));
                }
            });
        });
    }
    let area = ui.area();
    let shown = &r.insights;
    let theme = t;
    ui.list_measured(
        "insights",
        &mut r.list,
        area,
        shown.len(),
        |text, i, w| {
            let inner = w - 2 * theme.px(10.0);
            theme.px(32.0) + md::height(text, &shown[i].doc, &theme, inner) + theme.px(8.0)
        },
        |ui, i, _| {
            let x = &shown[i];
            let row = ui.area();
            let head = Rect::new(row.x + t.px(10.0), row.y + t.px(4.0), row.w - 2 * t.px(10.0), t.px(26.0));
            let state = if x.insight.status == "ready" { "" } else { status_label(&x.insight.status) };
            let line = if state.is_empty() { x.insight.title.clone() } else { format!("{} · {state}", x.insight.title) };
            ui.text_in(head, Face::Bold, t.pt(15.0), t.text, &line);
            let body = Rect::new(head.x, head.bottom(), head.w, row.bottom() - head.bottom());
            md::draw(ui, &x.doc, body);
        },
    );
}
