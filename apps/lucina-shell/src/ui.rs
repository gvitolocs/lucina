//! The shell's look and layout: the status bar, the home screen with its app tiles, the terminal
//! window and Settings. Drawing only: state changes happen in `main.rs`.
//!
//! The system is called Juno on PCs (the LucinaOS for laptops; its kernel build is LucinaJuno).
//! The colours are the LucinaOS boot menu's (navy background, gold accents, white title), so the
//! shell continues the boot screens without a visible jump.

use crate::app_groups;
use crate::apps::Ended;
use crate::system::Status;
use crate::term::{Color, Screen};
use lucina_app::Image;
use lucina_app::gfx::{Face, Frame, Rect, Rgb, Text};

pub use lucina_app::theme::{
    BAR, DANGER, DIM, GOLD, GREEN, ITEM, NAVY, PANEL, PANEL_HI, TERM_BG, TERM_FG, WHITE,
};

/// The system's name in the interface: Diana on the POCO X3, Juno everywhere else.
pub fn system_name() -> &'static str {
    static NAME: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let compatible = std::fs::read("/proc/device-tree/compatible").unwrap_or_default();
        if compatible.split(|&b| b == 0).any(|c| c == b"xiaomi,surya") {
            "Diana"
        } else {
            "Juno"
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Terminal,
    Settings,
    /// An app ended badly: what happened, and an offer to open it again.
    Ended,
}

impl View {
    pub fn name(self) -> &'static str {
        match self {
            View::Home => "Home",
            View::Terminal => "Terminal",
            View::Settings => "Settings",
            View::Ended => "App stopped",
        }
    }
}

/// What a home screen tile does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileAction {
    Open(View),
    /// Start this app (its position among the manifests).
    App(usize),
}

/// What a tile shows as its icon.
pub enum Icon {
    /// A name in the icon pack, and the letter to show if it is not there.
    Named(String, String),
    /// Characters on a plate, as the manifest says.
    Text(String),
    Picture(Image),
}

/// One tile on the home screen.
pub struct Tile {
    /// The manifest id, or "terminal" and "settings" for the shell's own tiles.
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub icon: Icon,
    pub action: TileAction,
}

/// What the home screen shows.
#[derive(Default)]
pub struct Home {
    pub tiles: Vec<Tile>,
    /// A tile, or, counting on after them, a power button.
    pub sel: usize,
    /// One line on what just happened: an app closed.
    pub note: Option<String>,
    /// The tiles as they are drawn, scrolled to `scroll`.
    pub placed: Vec<Rect>,
    /// The group headers as they are drawn.
    pub headers: Vec<Rect>,
    /// The group index of each header.
    pub header_groups: Vec<usize>,
    /// The row each tile is on.
    pub tile_rows: Vec<usize>,
    /// The top of each row, before scrolling.
    pub row_tops: Vec<i32>,
    /// How many rows the page is scrolled by.
    pub scroll: i32,
    /// The furthest `scroll` may go.
    pub max_scroll: i32,
    /// The area the tiles are placed in.
    pub area: Rect,
    /// The height of the tiles.
    pub tile_h: i32,
    /// The distance between rows.
    pub row_step: i32,
    /// The placement before scrolling.
    pub base_tiles: Vec<Rect>,
    pub base_headers: Vec<Rect>,
}

impl Home {
    /// Lay the tiles out again from their groups and the screen.
    pub fn reflow(&mut self, l: &Layout) {
        let mut counts = [0usize; 4];
        for t in &self.tiles {
            counts[app_groups::group_of(&t.id) as usize] += 1;
        }
        let area = l.apps_area();
        let p = app_groups::place(&counts, area, l.gap, l.px(22.0), l.px(72.0), l.tile_h, 10);
        self.area = area;
        self.tile_h = p.height;
        self.header_groups = p.header_groups;
        self.tile_rows = p.tile_rows;
        self.row_tops = p.row_tops;
        self.base_tiles = p.tiles;
        self.base_headers = p.headers;
        self.row_step = (p.height + l.gap).max(1);
        let bottom = self
            .base_tiles
            .iter()
            .chain(self.base_headers.iter())
            .map(|r| r.bottom())
            .max()
            .unwrap_or(area.y);
        self.max_scroll = ((bottom - area.bottom()).max(0) + self.row_step - 1) / self.row_step;
        self.scroll = self.scroll.clamp(0, self.max_scroll);
        self.apply_scroll();
    }

    fn apply_scroll(&mut self) {
        let dy = self.scroll * self.row_step;
        self.placed = self
            .base_tiles
            .iter()
            .map(|r| Rect::new(r.x, r.y - dy, r.w, r.h))
            .collect();
        self.headers = self
            .base_headers
            .iter()
            .map(|r| Rect::new(r.x, r.y - dy, r.w, r.h))
            .collect();
    }

    /// Scroll by `rows`, a row at a time. Returns whether anything moved.
    pub fn scroll_by(&mut self, rows: i32) -> bool {
        let next = (self.scroll + rows).clamp(0, self.max_scroll);
        if next == self.scroll {
            return false;
        }
        self.scroll = next;
        self.apply_scroll();
        true
    }

    /// Scroll so that the selected tile is in view. Returns whether the page moved.
    pub fn ensure_visible(&mut self) -> bool {
        if self.sel >= self.placed.len() {
            return false;
        }
        let mut moved = false;
        loop {
            let r = self.placed[self.sel];
            if r.h > self.area.h {
                break;
            }
            if r.y < self.area.y && self.scroll > 0 {
                self.scroll -= 1;
                moved = true;
                self.apply_scroll();
            } else if r.bottom() > self.area.bottom() && self.scroll < self.max_scroll {
                self.scroll += 1;
                moved = true;
                self.apply_scroll();
            } else {
                break;
            }
        }
        moved
    }

    /// The row a tile is on.
    pub fn row_of(&self, i: usize) -> Option<usize> {
        self.tile_rows.get(i).copied()
    }

    /// The tile of `row` nearest the x coordinate.
    pub fn nearest_in_row(&self, row: usize, x: i32) -> Option<usize> {
        self.tile_rows
            .iter()
            .enumerate()
            .filter(|(_, r)| **r == row)
            .min_by_key(|(i, _)| {
                let r = self.base_tiles[*i];
                (r.x + r.w / 2 - x).abs()
            })
            .map(|(i, _)| i)
    }

    /// The last row of tiles.
    pub fn last_row(&self) -> Option<usize> {
        self.row_tops.len().checked_sub(1)
    }
}

/// What the two buttons at the bottom of Home do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Power {
    Restart,
    ShutDown,
}

impl Power {
    pub fn name(self) -> &'static str {
        match self {
            Power::Restart => "Restart",
            Power::ShutDown => "Shut down",
        }
    }
}

/// Home's power buttons, numbered after the tiles in `Home::sel`.
pub const POWER_BUTTONS: [Power; 2] = [Power::Restart, Power::ShutDown];
/// The dialog: Cancel is 0 and comes first, so that a stray Enter does nothing.
pub const CONFIRM_CANCEL: usize = 0;
pub const CONFIRM_DO: usize = 1;

/// Settings: one section per list entry, each a list of (label, value) rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsPage {
    pub sections: Vec<(&'static str, Vec<(String, String)>)>,
    /// The section whose first rows can be changed, and how many: Left/Right, or a click on − and +, change them.
    pub adjustable: Option<(usize, usize)>,
}

/// Every position and size, from the screen size. `k` scales the reference layout drawn for
/// 1024x768 (the boot menu's) to the screen: 0.78 on QEMU's 800x600, 1.4 on the Book Go's
/// 1920x1080 panel.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub w: i32,
    pub h: i32,
    pub k: f32,
    pub bar: Rect,
    pub bar_home: Rect,
    /// The left and right margin of the pages.
    pub margin: i32,
    /// The gap between tiles, and between rows and groups.
    pub gap: i32,
    /// The old tile height: the most a tile may be on the apps page.
    pub tile_h: i32,
    pub window: Rect,
    pub grid: Rect,
    pub term_px: u16,
    pub cell_w: i32,
    pub cell_h: i32,
    pub term_ascent: i32,
    pub cols: usize,
    pub rows: usize,
    pub list: Rect,
    pub list_row: i32,
    pub panel: Rect,
    /// Home's Restart and Shut down.
    pub power: [Rect; 2],
    /// The confirmation dialog and its Cancel and confirm buttons.
    pub dialog: Rect,
    pub dialog_buttons: [Rect; 2],
}

impl Layout {
    pub fn new(w: u32, h: u32, text: &mut Text) -> Layout {
        let (w, h) = (w as i32, h as i32);
        let k = (w as f32 / 1024.0).min(h as f32 / 768.0).clamp(0.6, 3.0);
        let px = |v: i32| (v as f32 * k).round() as i32;
        let bar = Rect::new(0, 0, w, px(36).max(24));
        let margin = (w * 6 / 100).max(px(24));

        let (bw, bh) = (px(130), px(38).max(24));
        let by = h - px(104);
        // The old tile size: 176 x 136 reference pixels. The apps page sizes its tiles from the
        // space left instead, never above this height.
        let gap = px(20);
        let tile_w = px(176).min((w - 2 * margin - 2 * gap) / 3);
        let tile_h = (tile_w as f32 * 136.0 / 176.0).round() as i32;

        let window = Rect::new(
            px(14),
            bar.bottom() + px(14),
            w - 2 * px(14),
            h - bar.bottom() - 2 * px(14),
        );
        let title_h = px(30).max(20);
        let pad = px(10).max(6);
        let term_px = ((15.0 * k).round() as i32).clamp(12, 20) as u16;
        let cell_w = (text.advance(Face::Mono, term_px, 'M').round() as i32).max(1);
        let term_ascent = text.ascent(Face::Mono, term_px);
        let cell_h = term_ascent + text.descent(Face::Mono, term_px) + (term_px as i32 / 8).max(1);
        let inner = Rect::new(
            window.x + pad,
            window.y + title_h + pad / 2,
            window.w - 2 * pad,
            window.h - title_h - pad,
        );
        let cols = (inner.w / cell_w).max(1) as usize;
        let rows = (inner.h / cell_h).max(1) as usize;
        let grid = Rect::new(inner.x, inner.y, cols as i32 * cell_w, rows as i32 * cell_h);

        let list_top = bar.bottom() + px(110);
        let list = Rect::new(margin, list_top, px(220), h - list_top - px(60));
        let panel_x = list.right() + px(24);
        let panel = Rect::new(panel_x, list_top, w - margin - panel_x, list.h);

        // At the right of the page's title: the whole height below is the apps'.
        let _ = by;
        let power = [0, 1].map(|i| {
            Rect::new(
                w - margin - 2 * bw - gap / 2 + i * (bw + gap / 2),
                bar.bottom() + px(10),
                bw,
                bh,
            )
        });
        let (dw, dh) = (px(460).min(w - 2 * px(16)), px(180));
        let dialog = Rect::new((w - dw) / 2, (h - dh) / 2, dw, dh);
        let dialog_buttons = [0, 1].map(|i| {
            Rect::new(
                dialog.right() - (2 - i) * (bw + px(16)),
                dialog.bottom() - bh - px(18),
                bw,
                bh,
            )
        });
        Layout {
            w,
            h,
            k,
            bar,
            bar_home: Rect::new(0, 0, px(200), bar.h),
            margin,
            gap,
            tile_h,
            window,
            grid,
            term_px,
            cell_w,
            cell_h,
            term_ascent,
            cols,
            rows,
            list,
            list_row: px(44).max(28),
            panel,
            power,
            dialog,
            dialog_buttons,
        }
    }

    pub fn px(&self, v: f32) -> i32 {
        (v * self.k).round() as i32
    }

    pub fn pt(&self, v: f32) -> u16 {
        ((v * self.k).round() as i32).clamp(9, 300) as u16
    }

    /// The area of the apps page: under its title, to the foot of the screen.
    pub fn apps_area(&self) -> Rect {
        let top = self.bar.bottom() + self.px(64.0);
        let bottom = self.h - self.px(16.0);
        Rect::new(
            self.margin,
            top,
            self.w - 2 * self.margin,
            (bottom - top).max(1),
        )
    }

    /// The power button under (`x`, `y`).
    pub fn power_at(&self, x: i32, y: i32) -> Option<usize> {
        self.power.iter().position(|r| r.contains(x, y))
    }

    /// The dialog's button under (`x`, `y`).
    pub fn dialog_button_at(&self, x: i32, y: i32) -> Option<usize> {
        self.dialog_buttons.iter().position(|r| r.contains(x, y))
    }

    /// The Settings section under (`x`, `y`), of `n`.
    pub fn section_at(&self, x: i32, y: i32, n: usize) -> Option<usize> {
        if !self.list.contains(x, y) {
            return None;
        }
        let i = ((y - self.list.y) / self.list_row) as usize;
        (i < n).then_some(i)
    }

    /// The pixel rectangle of terminal row `r`.
    pub fn term_row(&self, r: usize) -> Rect {
        Rect::new(
            self.grid.x,
            self.grid.y + r as i32 * self.cell_h,
            self.grid.w,
            self.cell_h,
        )
    }

    /// The baseline of row `i` of the Settings panel.
    pub fn panel_baseline(&self, i: usize) -> i32 {
        let pad = self.px(24.0);
        self.panel.y + pad + self.px(24.0) + self.px(16.0) + (i as i32 + 1) * self.px(34.0)
    }

    /// The band row `i` of the panel is drawn in.
    pub fn panel_row(&self, i: usize) -> Rect {
        let pad = self.px(24.0);
        Rect::new(
            self.panel.x + pad,
            self.panel_baseline(i) - self.px(24.0),
            self.panel.w - 2 * pad,
            self.px(34.0),
        )
    }

    /// The −, the value and the + of an adjustable row `i`, at the right of the panel.
    pub fn steppers(&self, i: usize) -> (Rect, Rect, Rect) {
        let r = self.panel_row(i);
        let b = self.px(30.0);
        let vw = self.px(150.0);
        let gap = self.px(6.0);
        let h = r.h - self.px(6.0);
        let y = r.y + (r.h - h) / 2;
        let plus = Rect::new(r.right() - self.px(8.0) - b, y, b, h);
        let value = Rect::new(plus.x - gap - vw, y, vw, h);
        let minus = Rect::new(value.x - gap - b, y, b, h);
        (minus, value, plus)
    }

    /// The adjustable row under (`x`, `y`), of the first `n`, and whether the click was on − (Some(false)), + (Some(true))
    /// or elsewhere on the row (None).
    pub fn stepper_at(&self, x: i32, y: i32, n: usize) -> Option<(usize, Option<bool>)> {
        for i in 0..n {
            if self.panel_row(i).contains(x, y) {
                let (m, _, p) = self.steppers(i);
                return Some((
                    i,
                    if m.contains(x, y) {
                        Some(false)
                    } else if p.contains(x, y) {
                        Some(true)
                    } else {
                        None
                    },
                ));
            }
        }
        None
    }
}

/// The whole screen: bar and view. Returns the area drawn (all of it).
#[allow(clippy::too_many_arguments)]
pub fn draw_all(
    fr: &mut Frame,
    t: &mut Text,
    l: &Layout,
    view: View,
    status: &Status,
    model: &str,
    home: &Home,
    ended: Option<&Ended>,
    settings: &SettingsPage,
    settings_sel: usize,
    settings_row: Option<usize>,
    term: Option<&Screen>,
    shell_note: Option<&str>,
) -> Rect {
    fr.fill_rect(fr.bounds(), NAVY);
    draw_bar(fr, t, l, view, status);
    match view {
        View::Home => draw_home(fr, t, l, model, home),
        View::Ended => {
            if let Some(e) = ended {
                draw_ended(fr, t, l, e);
            }
        }
        View::Settings => draw_settings(fr, t, l, settings, settings_sel, settings_row),
        View::Terminal => {
            draw_window(fr, t, l, shell_note);
            if let Some(s) = term {
                let rows: Vec<usize> = (0..s.rows.min(l.rows)).collect();
                draw_term_rows(fr, t, l, s, &rows, true);
            }
        }
    }
    fr.bounds()
}

/// The status bar. Returns its rectangle.
pub fn draw_bar(fr: &mut Frame, t: &mut Text, l: &Layout, view: View, status: &Status) -> Rect {
    let b = l.bar;
    fr.fill_rect(b, BAR);
    let line = l.px(2.0).max(1);
    fr.fill_rect(Rect::new(0, b.bottom() - line, b.w, line), GOLD);
    let base = b.y + (b.h - line) / 2 + t.ascent(Face::Sans, l.pt(15.0)) / 2 - 1;
    let x = l.px(16.0);
    let w = t.draw(fr, x, base, Face::Bold, l.pt(16.0), WHITE, system_name());
    t.draw(
        fr,
        x + w + l.px(10.0),
        base,
        Face::Sans,
        l.pt(15.0),
        GOLD,
        &format!("·  {}", view.name()),
    );

    // Right side, right to left: clock, battery, network, camera.
    let size = l.pt(15.0);
    let mut right = b.w - l.px(16.0);
    if !status.clock.is_empty() {
        let cw = t.measure(Face::Bold, size, &status.clock);
        right -= cw;
        t.draw(fr, right, base, Face::Bold, size, WHITE, &status.clock);
        right -= l.px(26.0);
    }

    let (iw, ih) = (l.px(22.0).max(12), l.px(11.0).max(7));
    if let Some(battery) = &status.battery {
        let text = battery.text();
        let bw = t.measure(Face::Sans, size, &text);
        right -= bw;
        t.draw(fr, right, base, Face::Sans, size, ITEM, &text);
        // A battery: body, nub, and as much filled as there is charge.
        right -= l.px(8.0) + iw;
        let icon = Rect::new(right, base - ih + l.px(1.0), iw, ih);
        let th = l.px(1.5).max(1);
        fr.fill_rect(icon, ITEM);
        let inner = icon.inset(th);
        fr.fill_rect(inner, BAR);
        let filled = inner.w * i32::from(battery.percent.min(100)) / 100;
        fr.fill_rect(Rect::new(inner.x, inner.y, filled, inner.h), ITEM);
        fr.fill_rect(
            Rect::new(icon.right(), icon.y + ih / 3, th + 1, ih / 3),
            ITEM,
        );
        right -= l.px(26.0);
    }

    if !status.network.is_empty() {
        let nw = t.measure(Face::Sans, size, &status.network);
        right -= nw;
        t.draw(fr, right, base, Face::Sans, size, ITEM, &status.network);
        let dot = l.px(4.0).max(3) as f32;
        let online = status.online;
        fr.fill_disc(
            (right - l.px(10.0)) as f32,
            (base - ih / 2) as f32,
            dot,
            if online { GREEN } else { DIM },
        );
        right -= l.px(26.0);
    }

    // The camera mark, when something captures: a red disc and "Camera on", left of the rest, drawn
    // the same right-to-left way (text first, then the disc to its left). The 26 px before it come
    // from the item placed before, as for the battery and the network.
    if status.camera.is_some() {
        let mark = "Camera on";
        let mw = t.measure(Face::Bold, l.pt(15.0), mark);
        right -= mw;
        t.draw(fr, right, base, Face::Bold, l.pt(15.0), WHITE, mark);
        right -= l.px(8.0);
        let rad = l.px(5.0) as f32;
        right -= l.px(10.0);
        fr.fill_disc(right as f32 + rad, (base - ih / 2) as f32, rad, DANGER);
    }
    b
}

fn draw_home(fr: &mut Frame, t: &mut Text, l: &Layout, _model: &str, home: &Home) {
    let margin = l.margin;
    let title_px = l.pt(30.0);
    let title_base = l.bar.bottom() + l.px(38.0);
    t.draw(fr, margin, title_base, Face::Bold, title_px, WHITE, "Apps");

    let area = l.apps_area();
    fr.set_clip(area);

    let head_px = l.pt(14.0);
    for (&g, r) in home.header_groups.iter().zip(&home.headers) {
        if r.bottom() <= area.y || r.y >= area.bottom() {
            continue;
        }
        let name = app_groups::Group::ALL[g].name();
        let name = t.fit(Face::Sans, head_px, name, r.w);
        let base = r.y + (r.h + t.ascent(Face::Sans, head_px) - t.descent(Face::Sans, head_px)) / 2;
        t.draw_clipped(fr, area, r.x, base, Face::Sans, head_px, DIM, &name);
    }

    let name_px = l.pt(13.0);
    let pad = l.px(10.0);
    let rad = l.px(10.0).max(4);
    for (i, (r, tile)) in home.placed.iter().zip(home.tiles.iter()).enumerate() {
        if r.bottom() <= area.y || r.y >= area.bottom() {
            continue;
        }
        if i == home.sel {
            fr.bordered_round_rect(*r, rad, l.px(3.0).max(2), GOLD, PANEL_HI);
        } else {
            fr.fill_round_rect(*r, rad, PANEL);
        }
        let pad = pad.min((r.h - l.px(10.0)) / 2).max(l.px(4.0));
        let icon_side = l
            .px(46.0)
            .min((r.h - 2 * pad - l.px(18.0)).max(l.px(16.0)))
            .min(r.w - 2 * pad);
        let icon = Rect::new(r.x + pad, r.y + pad, icon_side, icon_side);
        draw_icon(fr, t, icon, &tile.icon);
        let inner = r.w - 2 * pad;
        let name = t.fit(Face::Bold, name_px, &tile.title, inner);
        let name_base = r.bottom() - pad - t.descent(Face::Bold, name_px);
        t.draw_clipped(
            fr,
            area,
            r.x + pad,
            name_base,
            Face::Bold,
            name_px,
            WHITE,
            &name,
        );
    }
    fr.set_clip(fr.bounds());

    for (i, (r, p)) in l.power.iter().zip(POWER_BUTTONS).enumerate() {
        draw_button(fr, t, l, *r, p.name(), home.tiles.len() + i == home.sel);
    }

    if let Some(note) = &home.note {
        let size = l.pt(14.0);
        let note = t.fit(Face::Sans, size, note, l.w - 2 * margin);
        t.draw(fr, margin, l.h - l.px(50.0), Face::Sans, size, ITEM, &note);
    }
}

/// Where the two answers on the "app stopped" screen are.
pub fn ended_buttons(l: &Layout) -> (Rect, Rect) {
    let (x, w, h) = (l.margin, l.px(190.0), l.px(40.0));
    let y = l.h - l.px(110.0);
    (
        Rect::new(x, y, w, h),
        Rect::new(x + w + l.px(16.0), y, w, h),
    )
}

/// An app stopped: which, why, what it last said, and the way on.
fn draw_ended(fr: &mut Frame, t: &mut Text, l: &Layout, e: &Ended) {
    let margin = l.margin;
    let width = l.w - 2 * margin;
    let mut y = l.bar.bottom() + l.px(80.0);
    let heading = format!("{} stopped", e.title);
    let heading = t.fit(Face::Bold, l.pt(34.0), &heading, width);
    t.draw(fr, margin, y, Face::Bold, l.pt(34.0), WHITE, &heading);
    y += l.px(36.0);
    let what = t.fit(Face::Sans, l.pt(18.0), &e.what, width);
    t.draw(fr, margin, y, Face::Sans, l.pt(18.0), GOLD, &what);
    y += l.px(30.0);
    let (b1, b2) = ended_buttons(l);
    if !e.tail.is_empty() {
        let px = l.pt(13.0);
        let lh = t.ascent(Face::Mono, px) + t.descent(Face::Mono, px) + 2;
        let bx = Rect::new(margin, y, width, (b1.y - l.px(20.0) - y).max(lh));
        fr.fill_round_rect(bx, l.px(8.0), TERM_BG);
        let inner = bx.inset(l.px(12.0));
        let fits = (inner.h / lh).max(1) as usize;
        let lines = &e.tail[e.tail.len().saturating_sub(fits)..];
        let mut base = inner.y + t.ascent(Face::Mono, px);
        for line in lines {
            let line = t.fit(Face::Mono, px, line, inner.w);
            t.draw(fr, inner.x, base, Face::Mono, px, TERM_FG, &line);
            base += lh;
        }
    }
    let rad = l.px(8.0);
    let size = l.pt(16.0);
    fr.fill_round_rect(b1, rad, GOLD);
    fr.fill_round_rect(b2, rad, PANEL_HI);
    let base = b1.y + (b1.h + t.ascent(Face::Sans, size) - t.descent(Face::Sans, size)) / 2;
    t.draw(
        fr,
        b1.x + l.px(14.0),
        base,
        Face::Sans,
        size,
        NAVY,
        "Enter: reopen",
    );
    t.draw(
        fr,
        b2.x + l.px(14.0),
        base,
        Face::Sans,
        size,
        WHITE,
        "Esc: home",
    );
}

fn draw_button(fr: &mut Frame, t: &mut Text, l: &Layout, r: Rect, name: &str, focused: bool) {
    let rad = l.px(8.0);
    if focused {
        fr.bordered_round_rect(r, rad, l.px(2.0).max(2), GOLD, PANEL_HI);
    } else {
        fr.fill_round_rect(r, rad, PANEL);
    }
    let size = l.pt(15.0);
    let w = t.measure(Face::Sans, size, name);
    let base = r.y + r.h / 2 + t.ascent(Face::Sans, size) / 2 - 1;
    t.draw(
        fr,
        r.x + (r.w - w) / 2,
        base,
        Face::Sans,
        size,
        if focused { WHITE } else { ITEM },
        name,
    );
}

/// The one question before the machine goes away. Returns its area.
pub fn draw_confirm(fr: &mut Frame, t: &mut Text, l: &Layout, action: Power, sel: usize) -> Rect {
    let d = l.dialog;
    fr.bordered_round_rect(d, l.px(14.0), l.px(2.0).max(2), GOLD, BAR);
    let pad = l.px(24.0);
    let question = match action {
        Power::Restart => format!("Restart {}?", system_name()),
        Power::ShutDown => format!("Shut down {}?", system_name()),
    };
    let question = question.as_str();
    t.draw(
        fr,
        d.x + pad,
        d.y + pad + l.px(20.0),
        Face::Bold,
        l.pt(22.0),
        WHITE,
        question,
    );
    let note = t.fit(
        Face::Sans,
        l.pt(14.0),
        "This session is lost: the system runs from RAM.",
        d.w - 2 * pad,
    );
    t.draw(
        fr,
        d.x + pad,
        d.y + pad + l.px(50.0),
        Face::Sans,
        l.pt(14.0),
        ITEM,
        &note,
    );
    draw_button(
        fr,
        t,
        l,
        l.dialog_buttons[CONFIRM_CANCEL],
        "Cancel",
        sel == CONFIRM_CANCEL,
    );
    draw_button(
        fr,
        t,
        l,
        l.dialog_buttons[CONFIRM_DO],
        action.name(),
        sel == CONFIRM_DO,
    );
    d
}

/// What a function key changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdKind {
    Brightness,
    Volume,
    Muted,
    /// The touchpad's hot key: on or off.
    Touchpad(bool),
}

impl OsdKind {
    pub fn name(self) -> &'static str {
        match self {
            OsdKind::Brightness => "Brightness",
            OsdKind::Volume => "Volume",
            OsdKind::Muted => "Muted",
            OsdKind::Touchpad(true) => "Touchpad on",
            OsdKind::Touchpad(false) => "Touchpad off",
        }
    }
}

/// A level shown for a moment after a function key, over whatever the shell shows.
#[derive(Clone, Copy, Debug)]
pub struct Osd {
    pub kind: OsdKind,
    pub level: u32,
    pub max: u32,
    pub until: std::time::Instant,
}

impl Osd {
    /// How long it stays: Windows' and macOS' level indicators stay about this long after the last press.
    pub const SHOWN: std::time::Duration = std::time::Duration::from_millis(1500);

    pub fn new(kind: OsdKind, level: u32, max: u32) -> Osd {
        Osd {
            kind,
            level: level.min(max),
            max: max.max(1),
            until: std::time::Instant::now() + Osd::SHOWN,
        }
    }
}

/// What the screen says while the machine goes away, in case that takes a moment.
pub fn draw_leaving(fr: &mut Frame, t: &mut Text, l: &Layout, action: Power) -> Rect {
    fr.fill_rect(fr.bounds(), NAVY);
    let text = match action {
        Power::Restart => "Restarting…",
        Power::ShutDown => "Shutting down…",
    };
    let size = l.pt(26.0);
    let w = t.measure(Face::Bold, size, text);
    t.draw(fr, (l.w - w) / 2, l.h / 2, Face::Bold, size, WHITE, text);
    fr.bounds()
}

/// A tile's icon: one of the pack's, characters from the manifest, or its picture.
fn draw_icon(fr: &mut Frame, t: &mut Text, r: Rect, icon: &Icon) {
    match icon {
        Icon::Named(id, letter) => lucina_app::icons::draw_in(fr, t, r, id, letter),
        Icon::Text(s) => lucina_app::icons::draw_letter(fr, t, r, s),
        Icon::Picture(img) => {
            fr.draw_image(
                r.x + (r.w - img.w as i32) / 2,
                r.y + (r.h - img.h as i32) / 2,
                img,
            );
        }
    }
}

fn draw_settings(
    fr: &mut Frame,
    t: &mut Text,
    l: &Layout,
    page: &SettingsPage,
    sel: usize,
    focus: Option<usize>,
) {
    let margin = l.list.x;
    t.draw(
        fr,
        margin,
        l.bar.bottom() + l.px(64.0),
        Face::Bold,
        l.pt(30.0),
        WHITE,
        "Settings",
    );
    let size = l.pt(17.0);
    for (i, (name, _)) in page.sections.iter().enumerate() {
        let row = Rect::new(
            l.list.x,
            l.list.y + i as i32 * l.list_row,
            l.list.w,
            l.list_row - l.px(6.0),
        );
        if i == sel {
            fr.fill_round_rect(row, l.px(8.0), PANEL_HI);
            fr.fill_rect(
                Rect::new(
                    row.x,
                    row.y + l.px(8.0),
                    l.px(4.0).max(2),
                    row.h - l.px(16.0),
                ),
                GOLD,
            );
        }
        let base = row.y + row.h / 2 + t.ascent(Face::Sans, size) / 2 - 1;
        t.draw(
            fr,
            row.x + l.px(18.0),
            base,
            if i == sel { Face::Bold } else { Face::Sans },
            size,
            if i == sel { WHITE } else { ITEM },
            name,
        );
    }

    let p = l.panel;
    fr.fill_round_rect(p, l.px(14.0), PANEL);
    let Some((name, rows)) = page.sections.get(sel) else {
        return;
    };
    let pad = l.px(24.0);
    t.draw(
        fr,
        p.x + pad,
        p.y + pad + l.px(24.0),
        Face::Bold,
        l.pt(22.0),
        GOLD,
        name,
    );
    let size = l.pt(15.0);
    let label_w = rows
        .iter()
        .map(|(k, _)| t.measure(Face::Sans, size, k))
        .max()
        .unwrap_or(0)
        .min(p.w / 3);
    let clip = p.inset(pad / 2);
    let adjustable = page
        .adjustable
        .filter(|&(s, _)| s == sel)
        .map_or(0, |(_, n)| n);
    // The hint for the rows that can be changed takes the panel's last line.
    let last = if adjustable > 0 {
        p.bottom() - pad - l.px(30.0)
    } else {
        p.bottom() - pad
    };
    for (i, (k, v)) in rows.iter().enumerate() {
        let y = l.panel_baseline(i);
        if y > last {
            break;
        }
        fr.fill_rect(
            Rect::new(p.x + pad, y - l.px(24.0), p.w - 2 * pad, 1),
            PANEL_HI,
        );
        if i >= adjustable {
            t.draw_clipped(fr, clip, p.x + pad, y, Face::Sans, size, DIM, k);
            let vx = p.x + pad + label_w + l.px(24.0);
            let v = t.fit(Face::Sans, size, v, p.right() - pad - vx);
            t.draw_clipped(fr, clip, vx, y, Face::Sans, size, WHITE, &v);
            continue;
        }
        let focused = focus == Some(i);
        let mut label_x = p.x + pad;
        if focused {
            let row = l.panel_row(i);
            fr.fill_round_rect(row, l.px(8.0), PANEL_HI);
            fr.fill_rect(
                Rect::new(
                    row.x,
                    row.y + l.px(8.0),
                    l.px(4.0).max(2),
                    row.h - l.px(16.0),
                ),
                GOLD,
            );
            label_x += l.px(12.0);
        }
        let (minus, value, plus) = l.steppers(i);
        // A long label stops before the −.
        let label_clip = clip.intersect(&Rect::new(clip.x, clip.y, minus.x - clip.x, clip.h));
        t.draw_clipped(fr, label_clip, label_x, y, Face::Sans, size, DIM, k);
        let glyph = if focused { GOLD } else { WHITE };
        for (r, g) in [(minus, "−"), (plus, "+")] {
            fr.fill_round_rect(r, l.px(6.0), if focused { PANEL } else { PANEL_HI });
            let base = r.y + r.h / 2 + t.ascent(Face::Sans, size) / 2 - 1;
            let gx = r.x + (r.w - t.measure(Face::Sans, size, g)) / 2;
            t.draw(fr, gx, base, Face::Sans, size, glyph, g);
        }
        let face = if focused { Face::Bold } else { Face::Sans };
        let v = t.fit(face, size, v, value.w);
        let vx = value.x + (value.w - t.measure(face, size, &v)) / 2;
        t.draw_clipped(fr, clip, vx, y, face, size, WHITE, &v);
    }
    if adjustable > 0 {
        let hint = t.fit(
            Face::Sans,
            l.pt(13.0),
            "Enter or Right to change    Up and Down choose    Left and Right change    Esc goes back",
            p.w - 2 * pad,
        );
        t.draw(
            fr,
            p.x + pad,
            p.bottom() - pad,
            Face::Sans,
            l.pt(13.0),
            DIM,
            &hint,
        );
    }
}

/// The terminal window's frame and title strip.
pub fn draw_window(fr: &mut Frame, t: &mut Text, l: &Layout, note: Option<&str>) -> Rect {
    let w = l.window;
    fr.bordered_round_rect(w, l.px(10.0), 1, PANEL_HI, TERM_BG);
    let title_h = l.grid.y - w.y - l.px(5.0);
    let strip = Rect::new(w.x + 1, w.y + 1, w.w - 2, title_h);
    fr.fill_rect(
        Rect::new(
            strip.x + l.px(10.0),
            strip.bottom(),
            strip.w - 2 * l.px(10.0),
            1,
        ),
        PANEL_HI,
    );
    let size = l.pt(15.0);
    let base = strip.y + strip.h / 2 + t.ascent(Face::Sans, size) / 2;
    let x = w.x + l.px(14.0);
    let tw = t.draw(fr, x, base, Face::Bold, size, WHITE, "Terminal");
    let sub = note.unwrap_or("sh");
    t.draw(
        fr,
        x + tw + l.px(10.0),
        base,
        Face::Sans,
        size,
        if note.is_some() { GOLD } else { DIM },
        &format!("·  {sub}"),
    );
    let hint = format!("{}×{}   Shift+PgUp history   Super+1 Home", l.cols, l.rows);
    let hw = t.measure(Face::Sans, l.pt(13.0), &hint);
    t.draw(
        fr,
        w.right() - l.px(14.0) - hw,
        base,
        Face::Sans,
        l.pt(13.0),
        DIM,
        &hint,
    );
    w
}

/// The 16 ANSI colours, tuned to stay readable on the navy terminal background.
const ANSI: [u32; 16] = [
    0x1c1c1e, 0xf0616d, 0x5fcf93, 0xff9f0a, 0x6d8cf0, 0xc678dd, 0x56c2d6, 0xc3cbee, //
    0x4a5488, 0xff8a94, 0x8be3b4, 0xe6c36f, 0x9bb0ff, 0xdda6ee, 0x8adcec, 0xffffff,
];

fn palette(c: Color, default: Rgb, bold: bool) -> Rgb {
    match c {
        Color::Default => default,
        Color::Rgb(v) => Rgb::hex(v),
        Color::Indexed(i) => {
            // Bold brightens the first eight, as on the Linux console.
            let i = if bold && i < 8 { i + 8 } else { i };
            match i {
                0..=15 => Rgb::hex(ANSI[i as usize]),
                16..=231 => {
                    let n = i as u32 - 16;
                    let level = |v: u32| if v == 0 { 0 } else { 55 + v * 40 };
                    Rgb((level(n / 36) << 16) | (level(n / 6 % 6) << 8) | level(n % 6))
                }
                _ => {
                    let g = 8 + (i as u32 - 232) * 10;
                    Rgb((g << 16) | (g << 8) | g)
                }
            }
        }
    }
}

/// Terminal rows `rows` of `s`, with the cursor when `focused`. Returns the area drawn.
pub fn draw_term_rows(
    fr: &mut Frame,
    t: &mut Text,
    l: &Layout,
    s: &Screen,
    rows: &[usize],
    focused: bool,
) -> Rect {
    let mut area = Rect::default();
    let mut buf = [0u8; 4];
    for &r in rows {
        if r >= s.rows || r >= l.rows {
            continue;
        }
        let rr = l.term_row(r);
        fr.fill_rect(rr, TERM_BG);
        for c in 0..s.cols.min(l.cols) {
            let cell = s.view_cell(r, c);
            let mut fg = palette(cell.attr.fg, TERM_FG, cell.attr.bold);
            let mut bg = palette(cell.attr.bg, TERM_BG, false);
            if cell.attr.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let cursor =
                focused && s.cursor_visible && s.view_back() == 0 && r == s.row && c == s.col;
            if cursor {
                bg = GOLD;
                fg = TERM_BG;
            }
            let x = rr.x + c as i32 * l.cell_w;
            let cell_r = Rect::new(x, rr.y, l.cell_w, l.cell_h);
            if bg != TERM_BG {
                fr.fill_rect(cell_r, bg);
            }
            if cell.ch != ' ' {
                let s = cell.ch.encode_utf8(&mut buf);
                t.draw_clipped(
                    fr,
                    cell_r,
                    x,
                    rr.y + l.term_ascent,
                    Face::Mono,
                    l.term_px,
                    fg,
                    s,
                );
            }
            if cell.attr.underline {
                fr.fill_rect(Rect::new(x, rr.bottom() - 2, l.cell_w, 1), fg);
            }
        }
        area = area.union(&rr);
    }
    area
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_fit_the_screens() {
        let mut t = Text::new();
        for (w, h) in [(800, 600), (1024, 768), (1280, 800), (1920, 1080)] {
            let l = Layout::new(w, h, &mut t);
            let screen = Rect::new(0, 0, w as i32, h as i32);
            for r in [&l.window, &l.grid, &l.list, &l.panel, &l.apps_area()] {
                assert_eq!(r.intersect(&screen), *r, "{w}x{h}: {r:?} off screen");
            }
            assert!(
                l.window.contains(l.grid.x, l.grid.y)
                    && l.window.contains(l.grid.right() - 1, l.grid.bottom() - 1)
            );
            assert!(l.cols >= 80 || w < 1024, "{w}x{h}: {} columns", l.cols);
            assert!(l.rows >= 24, "{w}x{h}: {} rows", l.rows);
            for r in l.power.iter().chain(&l.dialog_buttons).chain([&l.dialog]) {
                assert_eq!(r.intersect(&screen), *r, "{w}x{h}: {r:?} off screen");
            }
            assert!(l.apps_area().y >= l.bar.bottom());
            assert!(l.power.iter().all(|r| r.bottom() <= l.apps_area().y));
            assert!(l.power[1].bottom() < l.h);
            assert!(l.dialog_buttons.iter().all(|b| {
                l.dialog.contains(b.x, b.y) && l.dialog.contains(b.right() - 1, b.bottom() - 1)
            }));
        }
    }

    #[test]
    fn hit_tests() {
        let mut t = Text::new();
        let l = Layout::new(1024, 768, &mut t);
        let c = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        let (x, y) = c(l.power[1]);
        assert_eq!(l.power_at(x, y), Some(1));
        assert_eq!(l.power_at(0, 0), None);
        assert_eq!(
            l.section_at(l.list.x + 5, l.list.y + l.list_row + 3, 5),
            Some(1)
        );
        assert_eq!(
            l.section_at(l.list.x + 5, l.list.y + l.list_row * 7, 5),
            None
        );
    }

    #[test]
    fn colours_resolve() {
        assert_eq!(palette(Color::Default, TERM_FG, false), TERM_FG);
        assert_eq!(
            palette(Color::Indexed(1), TERM_FG, false),
            Rgb::hex(ANSI[1])
        );
        assert_eq!(palette(Color::Indexed(1), TERM_FG, true), Rgb::hex(ANSI[9]));
        assert_eq!(palette(Color::Indexed(16), TERM_FG, false), Rgb::hex(0));
        assert_eq!(
            palette(Color::Indexed(231), TERM_FG, false),
            Rgb::hex(0xffffff)
        );
        assert_eq!(
            palette(Color::Indexed(232), TERM_FG, false),
            Rgb::hex(0x080808)
        );
    }

    #[test]
    fn steppers_hit_tests() {
        let mut t = Text::new();
        let l = Layout::new(1024, 768, &mut t);
        let c = |r: Rect| (r.x + r.w / 2, r.y + r.h / 2);
        let (x, y) = c(l.steppers(0).0);
        assert_eq!(l.stepper_at(x, y, 3), Some((0, Some(false))));
        let (x, y) = c(l.steppers(0).2);
        assert_eq!(l.stepper_at(x, y, 3), Some((0, Some(true))));
        let (x, y) = c(l.steppers(1).1);
        assert_eq!(l.stepper_at(x, y, 3), Some((1, None)));
        let (x, y) = (
            l.panel_row(3).x - 1,
            l.panel_row(3).y + l.panel_row(3).h / 2,
        );
        assert_eq!(l.stepper_at(x, y, 3), None);
        let (x, y) = (l.panel.x - 1, l.panel.y + l.panel.h / 2);
        assert_eq!(l.stepper_at(x, y, 3), None);
        assert!(l.panel_row(1).y > l.panel_row(0).y);
        let (m, v, p) = l.steppers(0);
        assert!(l.panel.contains(m.x, m.y) && l.panel.contains(m.right() - 1, m.bottom() - 1));
        assert!(l.panel.contains(v.x, v.y) && l.panel.contains(v.right() - 1, v.bottom() - 1));
        assert!(l.panel.contains(p.x, p.y) && l.panel.contains(p.right() - 1, p.bottom() - 1));
        assert!(
            m.intersect(&v).is_empty() && m.intersect(&p).is_empty() && v.intersect(&p).is_empty()
        );
    }
}
