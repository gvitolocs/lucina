//! `GalleryViewer`: one picture, whole screen. ←/→ walk, `i` the info panel, `f` favourite, `r` rotate, Delete, `d` download, Shift+`a` add to
//! an album, Escape closes. The picture itself is `lucina_picture_view`: `+`/`-`/`0`, the wheel and drag zoom and pan. Enter on a video plays it,
//! in `lucina-app-video`, a program of its own.
//!
//! The 1600 px JPEG fills this screen, fitted once when it arrives. Zooming decodes it unfitted, then the original where this machine
//! can read it: only for the picture on screen, let go when it moves on.

use std::collections::HashMap;

use lucina_app::program::{Ended, Program};
use lucina_app::{Face, Image, Rect, Rgb, TextEdit, Ui, keys};
use lucina_files::pictures::{Fit, Want};
use lucina_nav as nav;
use lucina_picture_view::PictureView;
use serde_json::{Value, json};

use crate::app::{self, Gallery, Shelf};
use crate::model::{self, Item};
use crate::server::Job;

/// A picture larger than the screen, for zooming.
const ZOOM_FIT: u32 = 8192;
/// Fitted pictures kept: this one and its neighbours.
const KEPT: usize = 3;
const INFO_W: f32 = 320.0;

pub struct Viewer {
    pub index: usize,
    /// Fitted to the screen, by the url and rotation they came from.
    fitted: HashMap<(String, u16), Image>,
    /// The picture on screen, fitted or zoomed.
    pub picture: PictureView,
    /// Where its unfitted source came from: the best one that has arrived.
    source_url: Option<String>,
    pub failed: Option<String>,
    pub info: bool,
    pub details: Option<(String, Result<Value, String>)>,
    pub caption: TextEdit,
    caption_id: String,
    screen: (u32, u32),
}

impl Viewer {
    pub fn new(index: usize) -> Viewer {
        Viewer {
            index,
            fitted: HashMap::new(),
            picture: PictureView::new(),
            source_url: None,
            failed: None,
            info: false,
            details: None,
            caption: TextEdit::new().placeholder("Add a caption"),
            caption_id: String::new(),
            screen: (0, 0),
        }
    }

    /// Another picture: nothing of the last zoom stays.
    pub fn reset_for_item(&mut self) {
        self.picture.clear();
        self.source_url = None;
        self.failed = None;
    }

    /// This picture first, then its neighbours, and when zooming the larger forms of this one.
    pub fn wants(&self, items: &[Item]) -> Vec<Want> {
        let mut out = Vec::new();
        let (w, h) = self.screen;
        if w == 0 {
            return out;
        }
        let fit = Fit::Within(w, h);
        let at = |i: usize, out: &mut Vec<Want>| {
            if let Some(item) = items.get(i)
                && let Some(url) = item.preview_url()
                && !self.fitted.contains_key(&(url.to_owned(), item.rotation))
            {
                out.push(Want { url: url.to_owned(), fit, rotation: item.rotation });
            }
        };
        at(self.index, &mut out);
        if let (true, Some(view), Some(item)) = (self.picture.is_zoomed(), self.picture.view(), items.get(self.index)) {
            let have = self.source_url.as_deref();
            // The original only when the drawn size has outrun the 1600 px picture, as on the other Secretary apps.
            let want = if view.wants_original(1600.0, 1.0) { item.decodable_original().or(item.preview_url()) } else { item.preview_url() };
            if let Some(url) = want
                && have != Some(url)
            {
                out.push(Want { url: url.to_owned(), fit: Fit::Within(ZOOM_FIT, ZOOM_FIT), rotation: item.rotation });
            }
        }
        at(self.index + 1, &mut out);
        if self.index > 0 {
            at(self.index - 1, &mut out);
        }
        out
    }

    /// `Err` gives it back when the picture was not for the viewer.
    pub fn picture_arrived(&mut self, want: &Want, img: Image) -> Result<(), Image> {
        match want.fit {
            Fit::Within(ZOOM_FIT, ZOOM_FIT) => {
                self.source_url = Some(want.url.clone());
                self.picture.set_source(img);
                Ok(())
            }
            Fit::Within(..) => {
                self.fitted.insert((want.url.clone(), want.rotation), img);
                Ok(())
            }
            _ => Err(img),
        }
    }

    pub fn picture_failed(&mut self, want: &Want, e: &str) {
        // A larger form that does not decode (a HEIC): the 1600 px picture goes on being the zoom.
        if want.fit != Fit::Within(ZOOM_FIT, ZOOM_FIT) && !self.fitted.keys().any(|(u, _)| *u == want.url) {
            self.failed = Some(e.to_owned());
        }
    }

    pub fn details_arrived(&mut self, id: &str, result: Result<Value, String>) {
        self.details = Some((id.to_owned(), result));
    }

    fn trim(&mut self, items: &[Item]) {
        if self.fitted.len() <= KEPT {
            return;
        }
        let keep: Vec<(String, u16)> = [self.index.checked_sub(1), Some(self.index), Some(self.index + 1)]
            .into_iter()
            .flatten()
            .filter_map(|i| items.get(i))
            .filter_map(|it| it.preview_url().map(|u| (u.to_owned(), it.rotation)))
            .collect();
        self.fitted.retain(|k, _| keep.contains(k));
    }

    pub fn go(&mut self, to: usize, items: &[Item]) {
        if to < items.len() && to != self.index {
            self.index = to;
            self.reset_for_item();
            self.trim(items);
            self.details = None;
        }
    }

    pub fn is_zoomed(&self) -> bool {
        self.picture.is_zoomed()
    }
}

pub fn view(g: &mut Gallery, ui: &mut Ui) {
    let t = ui.theme();
    // How the player went, if one ran.
    if let Some(Ended::Failed(why) | Ended::NotStarted(why)) = ui.program_ended() {
        g.say(why);
    }
    let Some(index) = g.viewer.as_ref().map(|v| v.index) else { return };
    let Some(item) = g.items.get(index).cloned() else {
        g.viewer = None;
        return;
    };
    let trash = g.shelf == Shelf::Trash;
    let info = g.viewer.as_ref().is_some_and(|v| v.info);
    // The bar.
    let bar = ui.cut_top(t.px(40.0));
    ui.fill(bar, t.bar);
    let title = if item.title.is_empty() { "Photo".to_owned() } else { item.title.clone() };
    let n = g.items.len();
    let more = if g.has_more() { "+" } else { "" };
    ui.text_in(Rect::new(bar.x + t.px(12.0), bar.y + t.px(2.0), bar.w - t.px(360.0), t.px(20.0)), Face::Bold, t.pt(14.0), t.text, &title);
    let when = item.taken_at.as_deref().map_or_else(|| "Date unknown".to_owned(), |x| model::taken_label(x, None, &g.zone));
    ui.text_in(
        Rect::new(bar.x + t.px(12.0), bar.y + t.px(21.0), bar.w - t.px(360.0), t.px(16.0)),
        Face::Sans,
        t.pt(12.0),
        t.dim,
        &format!("{when} · {} of {n}{more}", index + 1),
    );
    let hint = if trash { "← → · r restore · Delete forever · i info · Esc" } else { "← → · f ♥ · r rotate · i info · d download · Esc" };
    let hint_w = ui.measure(Face::Sans, t.pt(12.0), hint);
    ui.text_in(Rect::new(bar.right() - hint_w - t.px(10.0), bar.y, hint_w, bar.h), Face::Sans, t.pt(12.0), t.dim, hint);
    if item.favorite {
        ui.text_in(Rect::new(bar.right() - hint_w - t.px(36.0), bar.y, t.px(20.0), bar.h), Face::Bold, t.pt(15.0), Rgb(0xff_6b_81), "♥");
    }

    let area = ui.area();
    let (pic, panel) = if info {
        let w = t.px(INFO_W).min(area.w / 2);
        (Rect::new(area.x, area.y, area.w - w, area.h), Some(Rect::new(area.right() - w, area.y, w, area.h)))
    } else {
        (area, None)
    };
    ui.fill(pic, Rgb(0x05_07_10));
    // First in the focus order, so that the keyboard starts here and not in the caption. It takes no pointer: the picture does.
    let enter = ui.focus_area("viewer-keys", Rect::new(pic.x, pic.y, 0, 0));

    // The picture.
    let url = item.preview_url().map(str::to_owned);
    let toast = g.toast.clone();
    {
        let Some(v) = g.viewer.as_mut() else { return };
        if v.screen != (pic.w.max(1) as u32, pic.h.max(1) as u32) {
            // The panel opened or closed: the picture is fitted anew.
            v.screen = (pic.w.max(1) as u32, pic.h.max(1) as u32);
            v.fitted.clear();
        }
        let key = url.clone().map(|u| (u, item.rotation));
        let image = key.as_ref().and_then(|k| v.fitted.get(k));
        if v.picture.is_zoomed() || image.is_some() {
            v.picture.show(ui, pic, image, 0x05_07_10);
            if let (true, Some(view)) = (v.picture.is_zoomed(), v.picture.view()) {
                let pct = (view.scale * 100.0).round() as u32;
                let tag = Rect::new(pic.x + t.px(8.0), pic.y + t.px(8.0), t.px(60.0), t.px(20.0));
                ui.fill_round(tag, t.px(4.0), Rgb(0x10_14_28));
                ui.text_in(tag.inset(t.px(3.0)), Face::Sans, t.pt(12.0), t.text, &format!("{pct}%"));
            }
        } else if url.is_none() {
            ui.text_in(pic.inset(t.px(20.0)), Face::Sans, t.pt(15.0), t.item, "No picture of this one yet: the server has not made it.");
        } else if let Some(e) = &v.failed {
            ui.text_in(pic.inset(t.px(20.0)), Face::Sans, t.pt(15.0), t.item, &format!("Couldn't load the picture. {e}"));
        } else if let Some(tile) = g.tiles.get(&item.id) {
            // The tile, until the large picture is here.
            ui.image_in(pic, &tile.image);
        }
        if item.video {
            let foot = Rect::new(pic.x, pic.bottom() - t.px(30.0), pic.w, t.px(30.0));
            ui.fill(foot, Rgb(0x10_14_28));
            ui.text_in(foot.inset(t.px(6.0)), Face::Sans, t.pt(14.0), t.text, &format!("▶ {}  Enter plays it", model::format_duration(item.duration_ms.unwrap_or(0))));
        }
    }
    if !toast.is_empty() {
        let line = Rect::new(pic.x + t.px(8.0), pic.bottom() - t.px(60.0), pic.w - t.px(16.0), t.px(24.0));
        ui.fill_round(line, t.px(4.0), Rgb(0x10_14_28));
        ui.text_in(line.inset(t.px(4.0)), Face::Sans, t.pt(13.0), t.text, &toast);
    }

    // The panel.
    if let Some(r) = panel {
        info_panel(g, ui, r, &item);
    }

    // Keys. In the caption field they are its own.
    if ui.is_focused("caption") {
        if ui.key(keys::KEY_ESC) {
            ui.focus("viewer-keys");
        }
        return;
    }
    if g.dialog.is_some() {
        return;
    }
    let base = picture_size(&item, pic);
    if item.video {
        if enter.clicked {
            play(g, ui, &item);
        }
    } else if let Some(v) = g.viewer.as_mut() {
        // The picture's own: the wheel, drag, + - 0, the arrows while zoomed. Enter or a click: in on that point, out again.
        let used = v.picture.input(ui, "viewer-picture", pic, base);
        if used.clicked {
            let (px, py) = ui.pointer();
            v.picture.toggle((px - pic.x) as f32, (py - pic.y) as f32, base, pic);
        } else if enter.clicked {
            v.picture.toggle(pic.w as f32 / 2.0, pic.h as f32 / 2.0, base, pic);
        }
    }
    let Some(p) = ui.press() else { return };
    // Alt+Left is "back", like Esc (when the picture is not zoomed, where Esc fits it first).
    if p.mods.alt && !p.mods.ctrl && !p.mods.meta && !p.mods.shift && p.code == keys::KEY_LEFT {
        ui.take_press();
        g.close_viewer();
        return;
    }
    let plain = !p.mods.ctrl && !p.mods.alt && !p.mods.meta;
    if !plain {
        return;
    }
    let zoomed = g.viewer.as_ref().is_some_and(Viewer::is_zoomed);
    let mut taken = true;
    match p.code {
        keys::KEY_ESC => {
            if zoomed {
                if let Some(v) = g.viewer.as_mut() {
                    v.picture.fit();
                }
            } else {
                g.close_viewer();
            }
        }
        // The pictures and videos in the order of the grid. The ends stop and do not wrap (`lucina-nav`, as Windows Photos does):
        // Left on the first and Right on the last stay, and the key is still taken, so the focus does not walk off.
        keys::KEY_LEFT | keys::KEY_RIGHT | keys::KEY_HOME | keys::KEY_END if !p.mods.shift => {
            let n = g.items.len();
            let to = match p.code {
                keys::KEY_LEFT => nav::prev(Some(index), n),
                keys::KEY_RIGHT => nav::next(Some(index), n),
                keys::KEY_HOME => nav::first(n),
                _ => nav::last(n),
            };
            match to {
                Some(to) if to != index => walk(g, to),
                _ if p.code == keys::KEY_RIGHT && g.has_more() => g.load_more(crate::app::FIRST_PAGE),
                _ => {}
            }
        }
        // Space plays a video, as Enter does.
        keys::KEY_SPACE if item.video => play(g, ui, &item),
        keys::KEY_I => toggle_info(g, &item),
        keys::KEY_F if !trash => favorite(g, &item),
        keys::KEY_R if trash => g.run_batch("restore", vec![item.id.clone()], None, false),
        keys::KEY_R if !item.video => rotate(g, &item),
        keys::KEY_D => g.start_download(item.clone()),
        keys::KEY_DELETE | keys::KEY_BACKSPACE => g.open_delete(vec![item.id.clone()]),
        keys::KEY_A if p.mods.shift && !trash => g.open_album_picker(vec![item.id.clone()]),
        _ => taken = false,
    }
    if taken {
        ui.take_press();
    }
}

fn picture_size(item: &Item, pic: Rect) -> (u32, u32) {
    let (mut w, mut h) = (item.width.unwrap_or(pic.w.max(1) as u32), item.height.unwrap_or(pic.h.max(1) as u32));
    if item.rotation == 90 || item.rotation == 270 {
        std::mem::swap(&mut w, &mut h);
    }
    (w.max(1), h.max(1))
}

/// The player is a program of its own: it takes the screen until it ends, and a crash in it costs this app nothing.
fn play(g: &mut Gallery, ui: &mut Ui, item: &Item) {
    match Program::app("video", &["--item", &item.id]) {
        Some(p) => ui.run_program(p),
        None => g.say("The video player is not on this machine.".to_owned()),
    }
}

fn walk(g: &mut Gallery, to: usize) {
    let items = std::mem::take(&mut g.items);
    let info = if let Some(v) = g.viewer.as_mut() {
        v.go(to, &items);
        v.info
    } else {
        false
    };
    g.items = items;
    // The grid comes back to the picture last seen.
    g.focus = to;
    if info && let Some(it) = g.items.get(to) {
        let job = Job::Details { id: it.id.clone() };
        g.send(job);
    }
    // `loadMore` near the end of what is loaded.
    if to + 10 >= g.items.len() && g.has_more() {
        g.load_more(crate::app::FIRST_PAGE);
    }
}

fn toggle_info(g: &mut Gallery, item: &Item) {
    let Some(v) = g.viewer.as_mut() else { return };
    v.info = !v.info;
    if v.info {
        v.details = None;
        g.send(Job::Details { id: item.id.clone() });
    }
}

/// `favorite`: at once on screen, put back if the server says no.
fn favorite(g: &mut Gallery, item: &Item) {
    let on = !item.favorite;
    let job = app::patch_job(item, app::favorite_body(on));
    let mut changed = item.clone();
    changed.favorite = on;
    g.replace_item(changed);
    g.send(job);
}

/// `rotate`.
fn rotate(g: &mut Gallery, item: &Item) {
    let r = app::next_rotation(item.rotation);
    let job = app::patch_job(item, app::rotation_body(r));
    let mut changed = item.clone();
    changed.rotation = r;
    g.replace_item(changed);
    if let Some(v) = g.viewer.as_mut() {
        v.reset_for_item();
    }
    g.send(job);
}

/// `PhotoInfo`.
fn info_panel(g: &mut Gallery, ui: &mut Ui, r: Rect, item: &Item) {
    let t = ui.theme();
    ui.fill(r, t.panel);
    let zone = g.zone.clone();
    let titles: Vec<(String, String)> = match &g.albums {
        Some(Ok(a)) => a.iter().map(|x| (x.id.clone(), x.title.clone())).collect(),
        _ => Vec::new(),
    };
    let trash_note = item.trash_expires_at.as_deref().and_then(|e| model::days_left(e, g.now_ms)).map(|left| match left {
        0 => "In the Trash. About to be removed for good.".to_owned(),
        1 => "In the Trash. Removed for good in 1 day.".to_owned(),
        n => format!("In the Trash. Removed for good in {n} days."),
    });
    let mut save = false;
    if let Some(v) = g.viewer.as_mut() {
        if v.caption_id != item.id {
            v.caption_id = item.id.clone();
            v.caption.set(item.caption.as_deref().unwrap_or(""));
        }
        let details = v.details.as_ref().filter(|(id, _)| *id == item.id).map(|(_, d)| d);
        let trash = trash_note.clone();
        ui.within(r.inset(t.px(12.0)), |ui| {
            ui.heading("Info");
            if let Some(note) = &trash {
                ui.paragraph(note);
            }
            match details {
                None => ui.dim("Loading details…"),
                Some(Err(_)) => {
                    ui.dim("Some details couldn't be loaded.");
                    info_rows(ui, &model::details_rows(&Value::Null, item, &zone, &titles));
                }
                Some(Ok(body)) => info_rows(ui, &model::details_rows(body, item, &zone, &titles)),
            }
            ui.space(8.0);
            ui.dim("Caption");
            let res = ui.text_field("caption", &mut v.caption);
            if res.submitted {
                save = true;
            }
        });
    }
    if save && let Some(text) = g.viewer.as_ref().map(|v| v.caption.text.clone()) {
        let job = app::patch_job(item, json!({"caption": text}));
        let mut changed = item.clone();
        changed.caption = if text.is_empty() { None } else { Some(text) };
        g.replace_item(changed);
        g.send(job);
        ui.focus("viewer-keys");
    }
}

fn info_rows(ui: &mut Ui, rows: &[(String, String)]) {
    let t = ui.theme();
    for (k, v) in rows {
        let r = ui.cut_top(t.px(36.0));
        ui.text_in(Rect::new(r.x, r.y, r.w, t.px(15.0)), Face::Sans, t.pt(11.0), t.dim, k);
        ui.text_in(Rect::new(r.x, r.y + t.px(15.0), r.w, t.px(19.0)), Face::Sans, t.pt(14.0), t.text, v);
    }
}
