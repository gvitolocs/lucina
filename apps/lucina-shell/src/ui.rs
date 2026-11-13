//! The shell's look and layout: the status bar, the home screen with its app tiles, the terminal
//! window and Settings. Drawing only: state changes happen in `main.rs`.
//!
//! The system is called Juno on PCs (the LucinaOS for laptops; its kernel build is LucinaJuno).
//! Since the glass restyle these screens are the kit's: every colour, radius, space and type step is
//! a `lucina_glass` token, a number the glass widgets published, or one of the toolkit's composites
//! (`lucina_app::theme`, `lucina_app::ui`) — no colour literal and no size of our own in the paint
//! code, which `the_paint_code_names_no_colour_of_its_own` and
//! `the_paint_code_scales_no_length_of_its_own` enforce. The terminal's ANSI table is
//! the exception: those colours are the shell's *content*, not its chrome (`ANSI`, `palette`).

use crate::app_groups;
use crate::apps::Ended;
use crate::system::Status;
use crate::term::{Color, Screen};
use lucina_app::Image;
use lucina_app::gfx::{Face, Frame, Rect, Rgb, Text};
use lucina_app::theme;
use lucina_app::ui::{CONTROLS, MEASURED, TextRole, quiet, rim, row_selected};
use lucina_glass::tokens::{color, radius, space, ty};
use lucina_glass::widgets::dialog_card;

/// The accent under its historical name: only the preview test reaches for it, to find one pixel of it.
#[cfg(test)]
pub use lucina_app::theme::GOLD;
/// The colours the other modules of the shell name, and the surfaces and the terminal's ink this file
/// still paints with. The screens take their ink from the kit's roles and their composites
/// (`GROUND`, `CARD`, `theme::ACCENT`, `rim`, `quiet`, `row_selected`); the four old aliases that no
/// longer buy anything (`ITEM`, `DIM`, `WHITE`, `PANEL_HI`) are gone with them.
pub use lucina_app::theme::{BAR, DANGER, GREEN, NAVY, TERM_BG, TERM_FG};

/// The page's ground, the kit's black (`color::VOID`, through `theme::GROUND`). `NAVY` above is the
/// same colour under its historical name.
const GROUND: Rgb = theme::GROUND;
/// The card the shell paints where there is no backdrop to show through (`color::CARBON`).
const CARD: Rgb = theme::PANEL;

/// A length the glass kit published: `CONTROLS` first, then `MEASURED` (both tables are read against
/// the kit by `lucina_app`'s own test). The shell takes its numbers from them so a change in the kit
/// moves these screens with it; `the_shell_only_asks_the_kit_for_what_it_publishes` checks every pair
/// used here really is in a table.
fn control(which: &str, what: &str) -> f32 {
    CONTROLS
        .iter()
        .chain(MEASURED.iter())
        .find(|c| c.control == which && c.what == what)
        .map(|c| c.dp)
        .expect("the shell asks for a control the kit does not publish")
}

/// The ink a role carries over `surface`: the colour token the kit's `SCALE` names for it, composited
/// the way the kit composites a translucent token where it has no alpha. `SCALE` gives the size and
/// the face; only the surface is the shell's, since these screens sit on the page or on a card of it.
fn role_ink(name: &str, surface: Rgb) -> Rgb {
    match name {
        "INK_2" => theme::over(color::INK_2, surface),
        "ACCENT" => theme::over(color::ACCENT, surface),
        // `"INK"`, `"WHITE"`, and `"caller"` where the app picks: content is the primary ink.
        _ => theme::over(color::INK, surface),
    }
}

/// A type step of the kit: its face, its size on this screen, and its ink over `surface`. The steps
/// come from `lucina_app::ui::SCALE`, and `Layout::pt` scales them exactly as the kit's `Theme::pt`
/// does (`the_shell_scales_type_as_the_kit_does`).
fn role(l: &Layout, r: TextRole, surface: Rgb) -> (Face, u16, Rgb) {
    let s = r.spec();
    (s.face, l.pt(s.size), role_ink(s.colour, surface))
}

/// The least there is to draw: the kit's hairline is one device pixel wide (`ui::Paint::glass` strokes
/// `separator` at 1 dp, and 1 dp is what an opaque canvas has).
fn hair(l: &Layout) -> i32 {
    l.px(1.0).max(1)
}

/// The card of the kit: its material, and the `separator` hairline around it (`Paint::glass` with no
/// backdrop is exactly this: the tint and a hairline stroke). The hairline is one device pixel, the
/// least there is to draw.
fn card(fr: &mut Frame, l: &Layout, r: Rect, rad: i32, fill: Rgb) {
    fr.bordered_round_rect(r, rad, hair(l), rim(fill), fill);
}

/// The kit's focus ring: the published offset outside the control, the published stroke thick, in the
/// accent, over the surface behind.
fn focus_ring(fr: &mut Frame, l: &Layout, r: Rect, rad: i32, surface: Rgb) {
    let off = l.px(control("focus ring", "offset"));
    let w = l.px(control("focus ring", "stroke")).max(1);
    fr.bordered_round_rect(r.inset(-off), rad + off, w, theme::ACCENT, surface);
}

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
        // A header band is the dim caption's own line with the kit's small gap under it; the tiles
        // flow on `space::XL`, and never shrink below the plate and one line of name.
        let head = l.px(ty::BODY.0) + l.px(space::S);
        let p = app_groups::place(
            &counts,
            area,
            l.gap,
            head,
            l.px(TILE_MIN_H_DP),
            l.tile_h,
            10,
        );
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

// The reference layout's own numbers: the page geometry the kit does not publish, because it is not a
// control. Every one is a dp of the 1024 x 768 reference screen, scaled by `k`.
/// The status band's height: the glass kit lays its bar in a band of 40 dp (`widgets/home/mod.rs:187`,
/// `chrome::status_bar`). The kit keeps the number to itself, so it is copied here with the reason;
/// the report lists `StatusBar` as the control the toolkit still lacks.
const BAR_H_DP: f32 = 40.0;
/// The launcher's tile: the reference grid's 176 x 136.
const TILE_W_DP: f32 = 176.0;
const TILE_H_DP: f32 = 136.0;
/// The smallest the apps page may make a tile: the plate and one line of name.
const TILE_MIN_H_DP: f32 = 72.0;
/// The power buttons' width: long enough for "Shut down" with the kit's padding at its sides.
const POWER_W_DP: f32 = 130.0;
/// The band under the bar a screen's title stands in. The toolkit has no `Header` control yet, so the
/// shell keeps the reference layout's band.
const TITLE_BAND_DP: f32 = 64.0;
/// Where Settings begins under that band, and how wide its section column is.
const SETTINGS_TOP_DP: f32 = 110.0;
const LIST_W_DP: f32 = 220.0;
/// The foot of the page, below the list and the panel.
const PAGE_FOOT_DP: f32 = 60.0;
/// The dialog card's width; its height is the kit's (`dialog_card::HEIGHT`).
const DIALOG_W_DP: f32 = 460.0;
/// The value column between a stepper's discs: the widest value a row shows, "1920 x 1080", in the
/// kit's control face.
const STEPPER_VALUE_W_DP: f32 = 150.0;
/// The bar's brand block: the block at its left that goes home when tapped.
const BRAND_W_DP: f32 = 200.0;
/// The status band's marks, in dp: what glass `chrome::status_bar` keeps inside itself (its battery is
/// 23 x 12 dp with a 3.5 dp wall, its items stand 26 dp apart, its camera mark is a 10 dp disc) rather
/// than publishing. The shell's glyphs are the reference bar's; they move to the kit when it has a
/// `StatusBar`, which the report of this step asks for.
const BAR_BATTERY_W_DP: f32 = 22.0;
const BAR_BATTERY_H_DP: f32 = 11.0;
const BAR_STROKE_DP: f32 = 1.5;
const BAR_ITEM_GAP_DP: f32 = 26.0;
/// The online mark's diameter, and the camera mark's, which the kit's header takes as 10 dp.
const BAR_DOT_DP: f32 = 8.0;
const BAR_MARK_DP: f32 = 10.0;
/// The terminal's cell height in dp. This is content, not chrome: `cols` and `rows` off it are the
/// size the shell tells the pty, so it stays the reference layout's own.
const TERM_PX_DP: f32 = 15.0;

impl Layout {
    pub fn new(w: u32, h: u32, text: &mut Text) -> Layout {
        let (w, h) = (w as i32, h as i32);
        let k = (w as f32 / 1024.0).min(h as f32 / 768.0).clamp(0.6, 3.0);
        let px = |v: f32| (v * k).round() as i32;
        // The status band, and the page's gutter: the kit's `space::EDGE` (rule 13), not the boot
        // menu's six percent.
        let bar = Rect::new(0, 0, w, px(BAR_H_DP).max(px(space::TAP) / 2));
        let margin = px(space::EDGE);

        // A control is a tap target tall (`CONTROLS`: button, height).
        let (bw, bh) = (px(POWER_W_DP), px(space::TAP));
        // The tiles flow on the kit's card gap, `space::XL`.
        let gap = px(space::XL);
        let tile_w = px(TILE_W_DP).min((w - 2 * margin - 2 * gap) / 3);
        let tile_h = (tile_w as f32 * TILE_H_DP / TILE_W_DP).round() as i32;

        // The terminal window is a card on the page: the gutter at its sides, `space::L` above and below.
        let inset = px(space::L);
        let window = Rect::new(
            inset,
            bar.bottom() + inset,
            w - 2 * inset,
            h - bar.bottom() - 2 * inset,
        );
        // Its title strip is the kit's app bar: a row of tap-sized controls.
        let title_h = px(space::TAP);
        let pad = px(space::M);
        let term_px = ((TERM_PX_DP * k).round() as i32).clamp(12, 20) as u16;
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

        let list_top = bar.bottom() + px(SETTINGS_TOP_DP);
        let list = Rect::new(
            margin,
            list_top,
            px(LIST_W_DP),
            h - list_top - px(PAGE_FOOT_DP),
        );
        let panel_x = list.right() + px(space::L);
        let panel = Rect::new(panel_x, list_top, w - margin - panel_x, list.h);

        // The power buttons at the right of the title band, above the tiles.
        let power = [0, 1].map(|i| {
            Rect::new(
                w - margin - 2 * bw - gap / 2 + i * (bw + gap / 2),
                bar.bottom() + px(space::S),
                bw,
                bh,
            )
        });
        // The dialog card: the kit's height, the page's gutter at its sides, its buttons a flow of two
        // with the kit's gap on the card's own padding.
        let (dw, dh) = (
            px(DIALOG_W_DP).min(w - 2 * px(space::L)),
            px(dialog_card::HEIGHT),
        );
        let dialog = Rect::new((w - dw) / 2, (h - dh) / 2, dw, dh);
        let (dpad, dgap) = (px(space::L), px(control("flow", "gap")));
        let dialog_buttons = [0, 1].map(|i| {
            Rect::new(
                dialog.right() - dpad - (2 - i) * bw - (1 - i) * dgap,
                dialog.bottom() - dpad - bh,
                bw,
                bh,
            )
        });
        Layout {
            w,
            h,
            k,
            bar,
            bar_home: Rect::new(0, 0, px(BRAND_W_DP), bar.h),
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
            // A section is a row of the kit's list: a tap target tall.
            list_row: px(space::TAP),
            panel,
            power,
            dialog,
            dialog_buttons,
        }
    }

    pub fn px(&self, v: f32) -> i32 {
        (v * self.k).round() as i32
    }

    /// A type step in pixels. The shell has no `Theme` — it is not an app — so this scales the kit's
    /// `k` itself; `the_shell_scales_type_as_the_kit_does` keeps the two agree at every size.
    pub fn pt(&self, v: f32) -> u16 {
        ((v * self.k).round() as i32).clamp(9, 300) as u16
    }

    /// The area of the apps page: under its title, to the foot of the screen.
    pub fn apps_area(&self) -> Rect {
        let top = self.bar.bottom() + self.px(TITLE_BAND_DP);
        let bottom = self.h - self.px(space::L);
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

    /// The panel's content padding: `space::L`, the kit's content padding for a card.
    pub fn panel_pad(&self) -> i32 {
        self.px(space::L)
    }

    /// The pitch of the panel's rows: the kit's list row, a tap target tall.
    pub fn panel_pitch(&self) -> i32 {
        self.px(space::TAP)
    }

    /// The room the section's own title takes above the rows: its line and the gap under it.
    pub fn panel_title_h(&self) -> i32 {
        self.px(space::XXL) + self.px(space::M)
    }

    /// The row `i` of the Settings panel, as the toolkit's `list` lays one.
    pub fn panel_row(&self, i: usize) -> Rect {
        let pad = self.panel_pad();
        Rect::new(
            self.panel.x + pad,
            self.panel.y + pad + self.panel_title_h() + i as i32 * self.panel_pitch(),
            self.panel.w - 2 * pad,
            self.panel_pitch(),
        )
    }

    /// The band row `i` is drawn in: the kit's band, inset from the row and cut at its radius.
    pub fn panel_band(&self, i: usize) -> Rect {
        self.panel_row(i).inset(self.px(space::XS))
    }

    /// The −, the value and the + of an adjustable row `i`, at the right of the panel: the kit's
    /// stepper, discs as tall as the band leaves, the value centred between them, the row's own inset
    /// at the right.
    pub fn steppers(&self, i: usize) -> (Rect, Rect, Rect) {
        let r = self.panel_row(i);
        let d = (r.h - self.px(space::XS)).max(1);
        let vw = self.px(STEPPER_VALUE_W_DP);
        let gap = self.px(space::S);
        let inset = self.px(space::M);
        let y = r.y + (r.h - d) / 2;
        let plus = Rect::new(r.right() - inset - d, y, d, d);
        let value = Rect::new(plus.x - gap - vw, y, vw, d);
        let minus = Rect::new(value.x - gap - d, y, d, d);
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
    fr.fill_rect(fr.bounds(), GROUND);
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

/// The status bar of the pages that are not the glass dashboard: the kit's header — its surface, the
/// `separator` hairline under it, its two roles for the words — so the bar of every page is the same
/// bar. The marks keep the shell's own geometry: glass `chrome::status_bar` builds its battery and
/// wifi glyphs from numbers it keeps inside itself, and hardcodes its clock to "9:41", so there is
/// nothing published to copy yet. `StatusBar` is the control the toolkit still lacks; the report
/// lists it and what it would have to publish.
pub fn draw_bar(fr: &mut Frame, t: &mut Text, l: &Layout, view: View, status: &Status) -> Rect {
    let b = l.bar;
    let hair = hair(l);
    fr.fill_rect(b, BAR);
    fr.fill_rect(
        Rect::new(b.x, b.bottom() - hair, b.w, hair),
        theme::HAIRLINE,
    );
    let pad = l.px(control("app bar", "side padding"));
    let band = Rect::new(b.x, b.y, b.w, (b.h - hair).max(1));
    let (tf, tp, tink) = role(l, TextRole::HeaderTitle, BAR);
    let (hf, hp, hink) = role(l, TextRole::HeaderHint, BAR);
    let (tb, hb) = (centre_base(t, tf, tp, band), centre_base(t, hf, hp, band));
    let cy = band.y + band.h / 2;

    // Which page is up is what is active: the accent is the one colour of the bar that is not ink.
    let w = t.draw(fr, pad, tb, tf, tp, tink, system_name());
    t.draw(
        fr,
        pad + w + l.px(control("app bar", "gap after mark")),
        hb,
        hf,
        hp,
        theme::ACCENT,
        &format!("·  {}", view.name()),
    );

    // Right side, right to left: clock, battery, network, camera.
    let mut right = b.w - pad;
    if !status.clock.is_empty() {
        right -= t.measure(tf, tp, &status.clock);
        t.draw(fr, right, tb, tf, tp, tink, &status.clock);
        right -= l.px(BAR_ITEM_GAP_DP);
    }

    let (iw, ih) = (
        l.px(BAR_BATTERY_W_DP).max(l.px(space::M)),
        l.px(BAR_BATTERY_H_DP).max(l.px(space::S)),
    );
    if let Some(battery) = &status.battery {
        let text = battery.text();
        right -= t.measure(hf, hp, &text);
        t.draw(fr, right, hb, hf, hp, hink, &text);
        // A battery: body, nub, and as much filled as there is charge.
        let gap = l.px(control("app bar", "gap after mark"));
        right -= gap + iw;
        let icon = Rect::new(right, cy - ih / 2, iw, ih);
        let th = l.px(BAR_STROKE_DP).max(1);
        fr.fill_rect(icon, tink);
        let inner = icon.inset(th);
        fr.fill_rect(inner, BAR);
        let filled = inner.w * i32::from(battery.percent.min(100)) / 100;
        fr.fill_rect(Rect::new(inner.x, inner.y, filled, inner.h), tink);
        fr.fill_rect(
            Rect::new(icon.right(), icon.y + ih / 3, th + hair, ih / 3),
            tink,
        );
        right -= l.px(BAR_ITEM_GAP_DP);
    }

    if !status.network.is_empty() {
        right -= t.measure(hf, hp, &status.network);
        t.draw(fr, right, hb, hf, hp, hink, &status.network);
        let d = l.px(BAR_DOT_DP).max(l.px(space::XS));
        right = bar_mark(
            fr,
            right,
            d,
            l.px(space::S),
            cy,
            if status.online { GREEN } else { hink },
        );
        right -= l.px(BAR_ITEM_GAP_DP);
    }

    // The camera mark, when something captures: a red disc and "Camera on", left of the rest, drawn
    // the same right-to-left way (text first, then the disc to its left). The gap before it comes from
    // the item placed before, as for the battery and the network.
    if status.camera.is_some() {
        let mark = "Camera on";
        right -= t.measure(tf, tp, mark);
        t.draw(fr, right, tb, tf, tp, tink, mark);
        let gap = l.px(control("app bar", "gap after mark"));
        let d = l.px(BAR_MARK_DP).max(l.px(space::M));
        bar_mark(fr, right - gap, d, l.px(space::S), cy, DANGER);
    }
    b
}

/// A round mark of the bar `d` across, standing `gap` left of `right` and centred on `cy`: the kit's
/// header draws its camera mark this way. Returns the mark's left edge, which is where the next item
/// to its left ends.
fn bar_mark(fr: &mut Frame, right: i32, d: i32, gap: i32, cy: i32, ink: Rgb) -> i32 {
    let x = right - gap - d;
    fr.fill_disc(x as f32 + d as f32 / 2.0, cy as f32, d as f32 / 2.0, ink);
    x
}

/// The icon plate on a launcher tile: the box the reference grid gave it. The kit has no app tile, so
/// the shell keeps this one of its own; the report lists `AppTile` as the control that is missing.
const TILE_ICON_DP: f32 = 46.0;

/// A page's title, on every page the shell shows: the kit's display step (`ty::TITLE`) in the primary
/// ink, standing `space::L` below the bar. The kit's dashboard sets this step in its Thin face, which
/// `gfx::Face` has not got, so the shell takes Semibold; the report lists it. Returns the baseline, so
/// a page can put its next line under the title's own.
fn page_title(fr: &mut Frame, t: &mut Text, l: &Layout, s: &str) -> i32 {
    let (face, px, ink) = (Face::Bold, l.pt(ty::TITLE.0), theme::WHITE);
    let base = l.bar.bottom() + l.px(space::L) + t.ascent(face, px);
    t.draw(fr, l.margin, base, face, px, ink, s);
    base
}

fn draw_home(fr: &mut Frame, t: &mut Text, l: &Layout, _model: &str, home: &Home) {
    page_title(fr, t, l, "Apps");

    let area = l.apps_area();
    fr.set_clip(area);

    // The group headers: the kit's dim caption, centred in the band `reflow` gave them.
    let (head_face, head_px, head_ink) = role(l, TextRole::Dim, GROUND);
    for (&g, r) in home.header_groups.iter().zip(&home.headers) {
        if r.bottom() <= area.y || r.y >= area.bottom() {
            continue;
        }
        let name = app_groups::Group::ALL[g].name();
        let name = t.fit(head_face, head_px, name, r.w);
        t.draw_clipped(
            fr,
            area,
            r.x,
            centre_base(t, head_face, head_px, *r),
            head_face,
            head_px,
            head_ink,
            &name,
        );
    }

    // A tile is the dashboard's card: `radius::LG`, the card material under the `separator` hairline,
    // its name on the last line. The one under the keyboard is the raised card with the kit's ring.
    let rad = l.px(radius::LG);
    let (name_face, name_px) = (Face::Sans, l.pt(ty::CAPTION.0));
    let name_ink = role_ink("INK", CARD);
    let line = t.ascent(name_face, name_px) + t.descent(name_face, name_px);
    for (i, (r, tile)) in home.placed.iter().zip(home.tiles.iter()).enumerate() {
        if r.bottom() <= area.y || r.y >= area.bottom() {
            continue;
        }
        let selected = i == home.sel;
        let fill = if selected { theme::PANEL_HI } else { CARD };
        if selected {
            focus_ring(fr, l, *r, rad, GROUND);
        }
        card(fr, l, *r, rad, fill);
        let pad = l
            .px(space::L)
            .min((r.h - line) / 2)
            .max(l.px(space::XS))
            .min((r.w / 2).max(1));
        let plate = l
            .px(TILE_ICON_DP)
            .min((r.h - 2 * pad - line).max(l.px(space::M)))
            .min(r.w - 2 * pad);
        draw_icon(
            fr,
            t,
            Rect::new(r.x + pad, r.y + pad, plate, plate),
            &tile.icon,
        );
        let name = t.fit(name_face, name_px, &tile.title, r.w - 2 * pad);
        t.draw_clipped(
            fr,
            area,
            r.x + pad,
            r.bottom() - pad - t.descent(name_face, name_px),
            name_face,
            name_px,
            name_ink,
            &name,
        );
    }
    fr.set_clip(fr.bounds());

    for (i, (r, p)) in l.power.iter().zip(POWER_BUTTONS).enumerate() {
        draw_button(
            fr,
            t,
            l,
            *r,
            p.name(),
            home.tiles.len() + i == home.sel,
            GROUND,
        );
    }

    // What just happened, in the foot band the page keeps for it.
    if let Some(note) = &home.note {
        let (face, px, ink) = role(l, TextRole::Paragraph, GROUND);
        let note = t.fit(face, px, note, l.w - 2 * l.margin);
        let foot = Rect::new(0, l.h - l.px(PAGE_FOOT_DP), l.w, l.px(PAGE_FOOT_DP));
        t.draw(
            fr,
            l.margin,
            centre_base(t, face, px, foot),
            face,
            px,
            ink,
            &note,
        );
    }
}

/// The baseline the line of `s` sits on when the line is centred in `r`, as the kit centres the text
/// of a row: its ascent above the middle, its descent below.
fn centre_base(t: &Text, f: Face, px: u16, r: Rect) -> i32 {
    r.y + (r.h + t.ascent(f, px) - t.descent(f, px)) / 2
}

/// Where the two answers on the "app stopped" screen are: the kit's buttons, a flow of two standing in
/// the foot band the page keeps below its content.
pub fn ended_buttons(l: &Layout) -> (Rect, Rect) {
    let (w, h) = (l.px(POWER_W_DP), l.px(space::TAP));
    let y = l.h - l.px(PAGE_FOOT_DP) - h;
    let gap = l.px(control("flow", "gap"));
    (
        Rect::new(l.margin, y, w, h),
        Rect::new(l.margin + w + gap, y, w, h),
    )
}

/// An app stopped: which, why, what it last said, and the way on.
fn draw_ended(fr: &mut Frame, t: &mut Text, l: &Layout, e: &Ended) {
    let width = l.w - 2 * l.margin;
    let heading = format!("{} stopped", e.title);
    let heading = t.fit(Face::Bold, l.pt(ty::TITLE.0), &heading, width);
    let title_base = page_title(fr, t, l, &heading);
    let (face, px, ink) = role(l, TextRole::Paragraph, GROUND);
    let what = t.fit(face, px, &e.what, width);
    let y =
        title_base + t.descent(Face::Bold, l.pt(ty::TITLE.0)) + l.px(space::L) + t.ascent(face, px);
    t.draw(fr, l.margin, y, face, px, ink, &what);

    let (b1, b2) = ended_buttons(l);
    if !e.tail.is_empty() {
        // What the app last said: its own ink on a field of the kit's quiet glass.
        let (mf, mp) = (Face::Mono, l.pt(ty::BODY.0));
        let lh = t.ascent(mf, mp) + t.descent(mf, mp) + l.px(space::XS);
        let top = y + t.descent(mf, mp) + l.px(space::L);
        let bx = Rect::new(l.margin, top, width, (b1.y - l.px(space::L) - top).max(lh));
        card(fr, l, bx, l.px(radius::MD), quiet(GROUND));
        let inner = bx.inset(l.px(space::M));
        let fits = (inner.h / lh).max(1) as usize;
        let lines = &e.tail[e.tail.len().saturating_sub(fits)..];
        let mut base = inner.y + t.ascent(mf, mp);
        for line in lines {
            let line = t.fit(mf, mp, line, inner.w);
            t.draw(fr, inner.x, base, mf, mp, TERM_FG, &line);
            base += lh;
        }
    }
    // This screen has no cursor: Enter always answers the first of the two, so the ring is on it.
    draw_primary_button(fr, t, l, b1, "Enter: reopen", true);
    draw_button(fr, t, l, b2, "Esc: home", false, GROUND);
}

/// A secondary button of the kit (`widgets/button.rs`, through `lucina_app::ui`): the control's own
/// corners, the quiet glass and its hairline at rest, the selected band and the ring when it carries
/// the keyboard. `surface` is the colour the button sits on.
fn draw_button(
    fr: &mut Frame,
    t: &mut Text,
    l: &Layout,
    r: Rect,
    name: &str,
    selected: bool,
    surface: Rgb,
) {
    let rad = l.px(control("button", "radius"));
    let fill = if selected {
        row_selected(surface)
    } else {
        quiet(surface)
    };
    if selected {
        focus_ring(fr, l, r, rad, fill);
    }
    card(fr, l, r, rad, fill);
    draw_button_label(fr, t, l, r, name, role_ink("INK", surface));
}

/// The kit's paper button: the one action of a dialog (`Look::Primary` in `lucina_app::ui`).
fn draw_primary_button(
    fr: &mut Frame,
    t: &mut Text,
    l: &Layout,
    r: Rect,
    name: &str,
    selected: bool,
) {
    let rad = l.px(control("button", "radius"));
    if selected {
        focus_ring(fr, l, r, rad, theme::PAPER);
    }
    fr.fill_round_rect(r, rad, theme::PAPER);
    draw_button_label(fr, t, l, r, name, theme::PAPER_INK);
}

/// A button's label, in the kit's control role and centred in its band: the text never has less room
/// at its sides than a flow gap.
fn draw_button_label(fr: &mut Frame, t: &mut Text, l: &Layout, r: Rect, name: &str, ink: Rgb) {
    let s = TextRole::Control.spec();
    let (face, px) = (s.face, l.pt(s.size));
    let w = t.measure(face, px, name);
    let x = r.x + (r.w - w).max(l.px(control("flow", "gap"))) / 2;
    t.draw(fr, x, centre_base(t, face, px, r), face, px, ink, name);
}

/// The one question before the machine goes away. Returns its area.
///
/// The kit's `modal` dims the page under the card and `dialog_card` lifts it on a soft shadow; the
/// shell paints its screens one damage rect at a time, so a scrim here would darken the page as many
/// times as it is repainted. The card therefore comes without either, which is the `DialogCard` the
/// toolkit still lacks (the report).
pub fn draw_confirm(fr: &mut Frame, t: &mut Text, l: &Layout, action: Power, sel: usize) -> Rect {
    let d = l.dialog;
    let rad = l.px(control("dialog", "radius"));
    let pad = l.px(control("dialog", "content padding"));
    card(fr, l, d, rad, CARD);
    let question = match action {
        Power::Restart => format!("Restart {}?", system_name()),
        Power::ShutDown => format!("Shut down {}?", system_name()),
    };
    let (qf, qp, qink) = role(l, TextRole::Heading, CARD);
    let question = t.fit(qf, qp, &question, d.w - 2 * pad);
    let qb = d.y + pad + t.ascent(qf, qp);
    t.draw(fr, d.x + pad, qb, qf, qp, qink, &question);
    let (nf, np, nink) = role(l, TextRole::Dim, CARD);
    let nb = qb + t.descent(qf, qp) + l.px(space::M) + t.ascent(nf, np);
    let note = t.fit(
        nf,
        np,
        "This session is lost: the system runs from RAM.",
        d.w - 2 * pad,
    );
    t.draw(fr, d.x + pad, nb, nf, np, nink, &note);
    draw_button(
        fr,
        t,
        l,
        l.dialog_buttons[CONFIRM_CANCEL],
        "Cancel",
        sel == CONFIRM_CANCEL,
        CARD,
    );
    draw_primary_button(
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

/// What the screen says while the machine goes away, in case that takes a moment: the page's ground,
/// and the kit's display step centred on it.
pub fn draw_leaving(fr: &mut Frame, t: &mut Text, l: &Layout, action: Power) -> Rect {
    fr.fill_rect(fr.bounds(), GROUND);
    let text = match action {
        Power::Restart => "Restarting…",
        Power::ShutDown => "Shutting down…",
    };
    let (face, px, ink) = (Face::Bold, l.pt(ty::TITLE.0), theme::WHITE);
    let w = t.measure(face, px, text);
    let band = Rect::new(0, 0, l.w, l.h);
    t.draw(
        fr,
        (l.w - w) / 2,
        centre_base(t, face, px, band),
        face,
        px,
        ink,
        text,
    );
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
    page_title(fr, t, l, "Settings");

    // The sections are the kit's list: a band of `radius::MD` standing in from the column's edges,
    // the selected one `accentSoft`, its name content and so the primary ink either way. The accent
    // ring is on whichever of the two columns the keyboard is in (rule 10).
    let inset = l.px(control("list row", "band inset"));
    let rad = l.px(control("list row", "band radius"));
    let (face, px, ink) = role(l, TextRole::Label, GROUND);
    for (i, (name, _)) in page.sections.iter().enumerate() {
        let row = Rect::new(
            l.list.x,
            l.list.y + i as i32 * l.list_row,
            l.list.w,
            l.list_row,
        );
        if i == sel {
            let band = Rect::new(row.x + inset, row.y, (row.w - 2 * inset).max(1), row.h);
            if focus.is_none() {
                focus_ring(fr, l, band, rad, GROUND);
            }
            fr.fill_round_rect(band, rad, row_selected(GROUND));
        }
        t.draw_clipped(
            fr,
            l.list,
            row.x + l.px(control("button", "side padding")) + inset,
            centre_base(t, face, px, row),
            face,
            px,
            ink,
            name,
        );
    }

    // The panel is the kit's card.
    let p = l.panel;
    let pad = l.panel_pad();
    let card_rad = l.px(radius::LG);
    card(fr, l, p, card_rad, CARD);
    let Some((name, rows)) = page.sections.get(sel) else {
        return;
    };
    let (title_face, title_px, title_ink) = role(l, TextRole::Heading, CARD);
    t.draw(
        fr,
        p.x + pad,
        p.y + pad + t.ascent(title_face, title_px),
        title_face,
        title_px,
        title_ink,
        name,
    );

    // What a row says is a key and a value: the key is metadata, the value content.
    let (key_face, key_px, key_ink) = role(l, TextRole::Dim, CARD);
    let (val_face, val_px, val_ink) = role(l, TextRole::Label, CARD);
    let label_w = rows
        .iter()
        .map(|(k, _)| t.measure(key_face, key_px, k))
        .max()
        .unwrap_or(0)
        .min(p.w / 3);
    let clip = p.inset(pad / 2);
    let adjustable = page
        .adjustable
        .filter(|&(s, _)| s == sel)
        .map_or(0, |(_, n)| n);
    // The hint for the rows that can be changed takes the panel's last line.
    let hint_h = if adjustable > 0 {
        t.ascent(key_face, key_px) + l.px(space::L)
    } else {
        0
    };
    let last = p.bottom() - pad - hint_h;
    let rule = rim(CARD);
    for (i, (k, v)) in rows.iter().enumerate() {
        let row = l.panel_row(i);
        if row.bottom() > last {
            break;
        }
        // The kit's `separator` hairline between two rows of a card.
        if i > 0 {
            fr.fill_rect(Rect::new(p.x + pad, row.y, p.w - 2 * pad, hair(l)), rule);
        }
        if i >= adjustable {
            t.draw_clipped(
                fr,
                clip,
                p.x + pad,
                centre_base(t, key_face, key_px, row),
                key_face,
                key_px,
                key_ink,
                k,
            );
            let vx = p.x + pad + label_w + l.px(space::M);
            let v = t.fit(val_face, val_px, v, p.right() - pad - vx);
            t.draw_clipped(
                fr,
                clip,
                vx,
                centre_base(t, val_face, val_px, row),
                val_face,
                val_px,
                val_ink,
                &v,
            );
            continue;
        }
        let focused = focus == Some(i);
        if focused {
            let band = l.panel_band(i);
            focus_ring(fr, l, band, rad, CARD);
            fr.fill_round_rect(band, rad, row_selected(CARD));
        }
        let label_x = row.x + inset;
        let (minus, value, plus) = l.steppers(i);
        // A long label stops before the −.
        let label_clip = clip.intersect(&Rect::new(clip.x, clip.y, minus.x - clip.x, clip.h));
        t.draw_clipped(
            fr,
            label_clip,
            label_x,
            centre_base(t, key_face, key_px, row),
            key_face,
            key_px,
            key_ink,
            k,
        );
        // The kit's stepper: a disc of the quiet glass at each end, its glyph in the content ink.
        for (r, g) in [(minus, "−"), (plus, "+")] {
            fr.fill_disc(
                (r.x + r.w / 2) as f32,
                (r.y + r.h / 2) as f32,
                (r.w.min(r.h) / 2) as f32,
                quiet(CARD),
            );
            let (glyph_face, glyph_px, _) = role(l, TextRole::Control, CARD);
            let gw = t.measure(glyph_face, glyph_px, g);
            t.draw(
                fr,
                r.x + (r.w - gw) / 2,
                centre_base(t, glyph_face, glyph_px, r),
                glyph_face,
                glyph_px,
                role_ink("INK", CARD),
                g,
            );
        }
        let v = t.fit(val_face, val_px, v, value.w);
        let vx = value.x + (value.w - t.measure(val_face, val_px, &v)) / 2;
        t.draw_clipped(
            fr,
            clip,
            vx,
            centre_base(t, val_face, val_px, row),
            val_face,
            val_px,
            val_ink,
            &v,
        );
    }
    if adjustable > 0 {
        let (hint_face, hint_px, hint_ink) = role(l, TextRole::Dim, CARD);
        let hint = t.fit(
            hint_face,
            hint_px,
            "Enter or Right to change    Up and Down choose    Left and Right change    Esc goes back",
            p.w - 2 * pad,
        );
        t.draw(
            fr,
            p.x + pad,
            p.bottom() - pad - t.descent(hint_face, hint_px),
            hint_face,
            hint_px,
            hint_ink,
            &hint,
        );
    }
}

/// The terminal window's frame and title strip: the kit's card, and the header its `app bar` lays —
/// the title in the header role, the hint beside it in the dim one. The grid inside is content and
/// keeps its own palette.
pub fn draw_window(fr: &mut Frame, t: &mut Text, l: &Layout, note: Option<&str>) -> Rect {
    let w = l.window;
    let hair = hair(l);
    let rad = l.px(radius::LG);
    fr.bordered_round_rect(w, rad, hair, theme::HAIRLINE, TERM_BG);
    // The strip is the kit's app bar: its height, its side padding, a hairline under it.
    let strip = Rect::new(
        w.x + hair,
        w.y + hair,
        w.w - 2 * hair,
        l.px(control("app bar", "height")),
    );
    let pad = l.px(control("app bar", "side padding"));
    fr.fill_rect(
        Rect::new(
            strip.x + pad,
            strip.bottom(),
            (strip.w - 2 * pad).max(1),
            hair,
        ),
        theme::HAIRLINE,
    );
    let (tf, tp, tink) = role(l, TextRole::HeaderTitle, TERM_BG);
    let (hf, hp, hink) = role(l, TextRole::HeaderHint, TERM_BG);
    let base = centre_base(t, tf, tp, strip);
    let x = w.x + pad;
    let tw = t.draw(fr, x, base, tf, tp, tink, "Terminal");
    let sub = note.unwrap_or("sh");
    // The shell's own word on the strip is what is active, so it is the one thing here in the accent.
    let sub_ink = if note.is_some() { theme::ACCENT } else { hink };
    t.draw(
        fr,
        x + tw + l.px(control("app bar", "gap after mark")),
        base,
        hf,
        hp,
        sub_ink,
        &format!("·  {sub}"),
    );
    let hint = format!("{}×{}   Shift+PgUp history   Super+1 Home", l.cols, l.rows);
    let hw = t.measure(hf, hp, &hint);
    t.draw(fr, w.right() - pad - hw, base, hf, hp, hink, &hint);
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
                // Where the owner types is what is active: the accent, not the grid's palette.
                bg = theme::ACCENT;
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
    // Only the checks that compare the shell against the kit read the table itself.
    use lucina_app::ui::SCALE;

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

    /// The shell's own source, the way the toolkit reads its own (`no_lengths_of_its_own`).
    fn paint_source() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui.rs"))
            .expect("the shell's own source")
    }

    /// The lines that are the terminal's *content* rather than its chrome: the ANSI table. Its sixteen
    /// colours are what a program on the other end of the pty asks for, and no token of the interface
    /// stands for them; `palette` is the same argument in code.
    fn ansi_span(src: &str) -> (usize, usize) {
        let lines: Vec<&str> = src.lines().collect();
        let from = lines
            .iter()
            .position(|l| l.contains("const ANSI: [u32; 16]"))
            .expect("the ANSI table is where the comment says");
        let to = from
            + lines[from..]
                .iter()
                .position(|l| l.trim_end() == "];")
                .expect("the ANSI table is closed");
        (from, to)
    }

    /// The paint code of these screens takes every colour from the kit: no `0x` literal anywhere
    /// except the terminal's content table. Comments are free to say hex, and the tests below name
    /// colours to check them, so this reads the code lines only, up to where `mod tests` begins.
    #[test]
    fn the_paint_code_names_no_colour_of_its_own() {
        let src = paint_source();
        let (ansi_from, ansi_to) = ansi_span(&src);
        let paint_lines = paint_lines(&src);
        let mut bad: Vec<String> = Vec::new();
        for (n, line) in src.lines().enumerate() {
            if n >= paint_lines {
                break;
            }
            let t = line.trim();
            if t.starts_with("//") || (ansi_from..=ansi_to).contains(&n) {
                continue;
            }
            for i in t.match_indices("0x").map(|(i, _)| i) {
                let hex: String = t[i + 2..]
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit())
                    .collect();
                if hex.len() == 6 {
                    bad.push(format!("{}: {}", n + 1, t));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "the shell paints a colour the kit does not name:\n{}",
            bad.join("\n")
        );
        // The table is still the only excuse, and still holds the sixteen colours it stands for.
        let in_table = src
            .lines()
            .skip(ansi_from)
            .take(ansi_to - ansi_from + 1)
            .filter(|l| !l.trim().starts_with("//"))
            .flat_map(|l| l.match_indices("0x").map(|_| ()))
            .count();
        assert_eq!(
            in_table, 16,
            "the ANSI table is where the content colours live"
        );
    }

    /// How far up the file the paint code goes, as text: everything before the tests at the bottom.
    /// The tests name colours and lengths to check them, so they are not evidence about the screens.
    /// The marker is the module, not the attribute: a `#[cfg(test)]` re-export near the top is not the
    /// bottom of the file.
    fn paint_text(src: &str) -> &str {
        let end = src
            .find("\n#[cfg(test)]\nmod tests")
            .expect("the shell's tests are at the bottom of the file");
        let t = &src[..end];
        // A marker that matched too early would leave these tests nothing to read, and pass.
        assert!(
            t.contains("fn draw_home") && t.contains("fn draw_settings"),
            "the scan stops before the screens it is meant to read"
        );
        t
    }

    fn paint_lines(src: &str) -> usize {
        paint_text(src).lines().count()
    }

    /// The same for sizes: the paint code scales a length that is a glass token, a number the kit
    /// published, or one of the named dp of the reference layout — never a number written into the
    /// middle of a screen. What it is allowed to keep is listed here with why, and every entry has to
    /// still be in use: an excuse that outlives the length it excuses fails.
    #[test]
    fn the_paint_code_scales_no_length_of_its_own() {
        let src = paint_source();
        let allowed = [(
            "1.0",
            "the hairline: one device pixel is the least there is to draw",
        )];
        let paint_lines = paint_lines(&src);
        let mut bad: Vec<String> = Vec::new();
        for (n, line) in src.lines().enumerate() {
            if n >= paint_lines {
                break;
            }
            let t = line.trim();
            if t.starts_with("//") {
                continue;
            }
            for p in t
                .match_indices("px(")
                .chain(t.match_indices("pt("))
                .map(|(i, _)| i)
            {
                let rest = &t[p + 3..];
                let num: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if !num.is_empty() && !allowed.iter().any(|(a, _)| *a == num) {
                    bad.push(format!("{}: {}", n + 1, t));
                }
            }
        }
        for (a, why) in allowed {
            assert!(
                src.contains(&format!("px({a}")),
                "px({a}) has no use left: {why}"
            );
        }
        assert!(
            bad.is_empty(),
            "the shell scales a length that is not the kit's:\n{}",
            bad.join("\n")
        );
    }

    /// Every `control` the shell reads really is published. The pairs are read out of the source, not
    /// out of a list kept beside it, so an entry that stops being used stops being excused too.
    #[test]
    fn the_shell_only_asks_the_kit_for_what_it_publishes() {
        let src = paint_source();
        let asked = control_pairs(paint_text(&src));
        assert!(asked.len() >= 12, "the shell hardly reads the tables");
        for (which, what) in &asked {
            assert!(
                CONTROLS
                    .iter()
                    .chain(MEASURED.iter())
                    .any(|c| c.control == *which && c.what == *what),
                "the kit does not publish {which} / {what}"
            );
        }
    }

    /// The `control("…", "…")` pairs in `src`, in the order they are written. The whole text is walked
    /// rather than the lines, because a call rustfmt has wrapped is the same request to the kit;
    /// arguments that are not string literals belong to the helper's own signature.
    fn control_pairs(src: &str) -> Vec<(String, String)> {
        const OPEN: &str = "control(";
        let mut out = Vec::new();
        let mut rest = src;
        while let Some(i) = rest.find(OPEN) {
            rest = &rest[i + OPEN.len()..];
            let end = match rest.find(')') {
                Some(end) => end,
                None => break,
            };
            let names: Vec<&str> = rest[..end]
                .split(',')
                .map(|a| a.trim())
                .filter(|a| a.starts_with('"') && a.ends_with('"') && a.len() > 1)
                .collect();
            if names.len() == 2 {
                out.push((
                    names[0][1..names[0].len() - 1].into(),
                    names[1][1..names[1].len() - 1].into(),
                ));
            }
            rest = &rest[end..];
        }
        out
    }

    /// The ink of a role over the page's own ground is the colour the toolkit published for it: the
    /// shell composites the same tokens the same way, it only has different surfaces to put them on.
    #[test]
    fn the_roles_over_the_page_are_the_kits_own() {
        for s in SCALE.iter() {
            assert_eq!(
                role_ink(s.colour, GROUND),
                s.rgb,
                "{} over the page is not the kit's ink for it",
                s.name
            );
        }
    }

    /// A role is scaled on the shell exactly as it is scaled in an app: the same `k`, the same
    /// rounding, the same floor and ceiling.
    #[test]
    fn the_shell_scales_type_as_the_kit_does() {
        for (w, h) in [(800u32, 600), (1024, 768), (1366, 768), (1920, 1080)] {
            let mut text = Text::new();
            let l = Layout::new(w, h, &mut text);
            let t = lucina_app::ui::Theme::new(w, h);
            for s in SCALE.iter() {
                assert_eq!(l.pt(s.size), t.pt(s.size), "{} at {w}x{h}", s.name);
            }
        }
    }

    /// CPU time of this process, on the clock `view_bench` measures against.
    fn cpu_us() -> i64 {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: a valid clock id and a timespec on the stack.
        unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut ts) };
        ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
    }

    /// The launcher as the shell has it with forty apps on the four groups.
    fn bench_home(l: &Layout) -> Home {
        let ids = [
            "terminal", "gallery", "desk", "camera", "notes", "player", "secrets", "viewer",
            "codecor", "recorder",
        ];
        let mut home = Home::default();
        for (i, id) in ids.iter().cycle().take(40).enumerate() {
            home.tiles.push(Tile {
                id: (*id).to_string(),
                title: format!("App {i}"),
                subtitle: String::new(),
                icon: Icon::Text("A".into()),
                action: TileAction::App(i),
            });
        }
        home.sel = 0;
        home.note = Some("Hello closed".into());
        home.reflow(l);
        home
    }

    /// What the launcher page costs to paint, before and after a change to it: the whole page and the tile
    /// grid alone, on the CPU clock, the best and the mean of the rounds. A release run measures what the
    /// device does (`cargo test -p lucina-shell --release -- --nocapture launcher_paint`); a debug run
    /// keeps to one short round so the suite stays quick.
    #[test]
    fn launcher_paint_is_measured() {
        // 300 frames x 5 rounds when optimised, as step 3b measured the toolkit; a short pass in debug.
        let (frames, rounds) = if cfg!(debug_assertions) {
            (20, 1)
        } else {
            (300, 5)
        };
        let mut text = Text::new();
        let l = Layout::new(1024, 768, &mut text);
        let mut fr = Frame::new(1024, 768);
        let home = bench_home(&l);
        let settings = SettingsPage {
            sections: vec![("Network", vec![("Host".into(), "eth0".into())])],
            adjustable: None,
        };
        let status = Status {
            clock: "14:05 CET".into(),
            battery: Some(crate::system::Battery {
                percent: 83,
                status: "Charging".into(),
                details: Vec::new(),
            }),
            network: "eth0 192.0.2.101".into(),
            online: true,
            camera: None,
        };
        let model = "Samsung Galaxy Book Go";
        let mut all = Vec::new();
        let mut grid = Vec::new();
        for _ in 0..rounds {
            let t0 = cpu_us();
            for _ in 0..frames {
                draw_all(
                    &mut fr,
                    &mut text,
                    &l,
                    View::Home,
                    &status,
                    model,
                    &home,
                    None,
                    &settings,
                    0,
                    None,
                    None,
                    None,
                );
            }
            all.push((cpu_us() - t0) as f64 / frames as f64);
            let t0 = cpu_us();
            for _ in 0..frames {
                draw_home(&mut fr, &mut text, &l, model, &home);
            }
            grid.push((cpu_us() - t0) as f64 / frames as f64);
        }
        let best = |v: &Vec<f64>| format!("{:.1}", v.iter().cloned().fold(f64::MAX, f64::min));
        let mean = |v: &Vec<f64>| format!("{:.1}", v.iter().sum::<f64>() / v.len() as f64);
        println!(
            "launcher paint, {frames} frames x{rounds} rounds, CPU us/frame: whole page best {} mean {}; tile grid best {} mean {}",
            best(&all),
            mean(&all),
            best(&grid),
            mean(&grid)
        );
        println!("rounds: whole {:?} grid {:?}", all, grid);
    }
}
