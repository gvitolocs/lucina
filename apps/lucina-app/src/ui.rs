//! Widgets, in immediate mode: an app has one [`view`](crate::App::view) function that lays out and
//! draws everything from its state, and asks each widget what happened (`if ui.button("Save") { .. }`).
//! There is no widget tree to keep in step with the state, and nothing to invalidate by hand.
//!
//! The runtime calls `view` once per input event with drawing turned off, so the event reaches the
//! right widget and the state changes, and then once more to draw. Which pixels really changed is
//! found afterwards ([`crate::dirty`]). So `view` must be cheap and free of side effects other than
//! reacting to its widgets: it runs several times per key. Do slow work on a thread
//! ([`crate::Sender::spawn`]).
//!
//! Layout cuts rectangles off the current area (`cut_top` and friends); widgets without a rectangle
//! take their place from the flow, down a column or, inside [`Ui::row`], along a row.

use std::ops::Range;
use std::time::{Duration, Instant};

use lucina_ax::accesskit::{Action, Role};
use lucina_ax::rules::{Act, Kind};
use lucina_ax::{Widget, tree};

use crate::edit::TextEdit;
use crate::gfx::{Face, Frame, Rect, Rgb, Text};
use crate::image::Image;
use crate::keys::{self, Press};
use crate::theme;

/// The 1px hairline on cards and list rows (docs/design/rules.md).
const HAIRLINE: Rgb = Rgb::hex(0x38383a);

/// A widget's identity from one frame to the next: a string, or a string and a number for
/// repeated widgets such as the rows of a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id(u64);

impl Id {
    /// The id as a number: the key of the widget's node in the tree Lucina reads.
    pub(crate) fn raw(self) -> u64 {
        self.0
    }
}

fn fnv(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

impl From<&str> for Id {
    fn from(s: &str) -> Id {
        Id(fnv(FNV_START, s.as_bytes()))
    }
}

impl From<(&str, usize)> for Id {
    fn from((s, n): (&str, usize)) -> Id {
        Id(fnv(fnv(FNV_START, s.as_bytes()), &n.to_le_bytes()))
    }
}

/// What the runtime hands the widgets, one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key(Press),
    /// The pointer moved here.
    Move(i32, i32),
    Down(i32, i32),
    Up(i32, i32),
    /// The secondary (right) button went down here.
    Secondary(i32, i32),
    /// Wheel notches: positive is up.
    Wheel(i32),
    /// Smooth scroll in screen pixels, already scaled by the theme: `dy` positive moves the content
    /// toward the top, as [`Input::Wheel`]; `dx` positive is right.
    Scroll {
        dx: i32,
        dy: i32,
    },
}

/// Colours and sizes, scaled to the screen like the shell's layout (`k` is 1.4 on the Book Go's
/// 1920x1080 panel, 1 at 1024x768).
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub k: f32,
    pub bg: Rgb,
    pub panel: Rgb,
    pub panel_hi: Rgb,
    pub bar: Rgb,
    pub accent: Rgb,
    pub text: Rgb,
    pub item: Rgb,
    pub dim: Rgb,
    pub selection: Rgb,
    pub danger: Rgb,
    pub ok: Rgb,
}

impl Theme {
    pub fn new(w: u32, h: u32) -> Theme {
        Theme {
            k: (w as f32 / 1024.0).min(h as f32 / 768.0).clamp(0.6, 3.0),
            bg: theme::NAVY,
            panel: theme::PANEL,
            panel_hi: theme::PANEL_HI,
            bar: theme::BAR,
            accent: theme::GOLD,
            text: theme::WHITE,
            item: theme::ITEM,
            dim: theme::DIM,
            selection: theme::SELECTION,
            danger: theme::DANGER,
            ok: theme::GREEN,
        }
    }

    /// A length in reference pixels (1024x768) on this screen.
    pub fn px(&self, v: f32) -> i32 {
        (v * self.k).round() as i32
    }

    /// A font size in reference pixels on this screen.
    pub fn pt(&self, v: f32) -> u16 {
        ((v * self.k).round() as i32).clamp(9, 300) as u16
    }
}

/// What persists between passes: focus, the pressed widget, the pointer.
#[derive(Default)]
pub struct UiState {
    pub(crate) focus: Option<Id>,
    pressed: Option<Id>,
    hover: Option<Id>,
    hover_now: Option<Id>,
    chain: Vec<Id>,
    chain_now: Vec<Id>,
    pub(crate) pointer: (i32, i32),
    modal_prev: bool,
    modal_now: bool,
    /// Something changed that the screen must show.
    pub(crate) want_paint: bool,
    pub(crate) wake: Option<Instant>,
    pub(crate) quit: bool,
    /// A program to run over this app when this pass is over, and how the last one ended.
    pub(crate) program: Option<crate::program::Program>,
    pub(crate) ended: Option<crate::program::Ended>,
    clipboard: String,
    clipboard_at: Option<std::time::SystemTime>,
    /// A password field has the keyboard: what is typed stays out of the trace.
    pub(crate) typing_secret: bool,
    typing_secret_now: bool,
    /// Someone asked for the tree: this pass records it ([`AxPass`]). `None` in every normal pass.
    pub(crate) ax: Option<AxPass>,
    /// The input of this pass came from Lucina's injection device.
    pub(crate) injected: bool,
    /// How many [`Ui::owner_only`] areas the widget being placed is inside.
    owner_only_depth: u32,
}

/// Shared between the apps: a file in `/tmp`, private to root as everything here is.
const CLIPBOARD: &str = "/tmp/lucina-clipboard";

impl UiState {
    fn clipboard_load(&mut self) {
        let Ok(meta) = std::fs::metadata(CLIPBOARD) else {
            return;
        };
        let at = meta.modified().ok();
        if at.is_some() && at == self.clipboard_at {
            return;
        }
        if meta.len() < 16 * 1024 * 1024
            && let Ok(s) = std::fs::read_to_string(CLIPBOARD)
        {
            self.clipboard = s;
            self.clipboard_at = at;
        }
    }

    fn clipboard_save(&mut self) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // Written by this app only when it really changed; a read-only /tmp only costs the sharing.
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(CLIPBOARD)
        {
            let _ = f.write_all(self.clipboard.as_bytes());
        }
        self.clipboard_at = std::fs::metadata(CLIPBOARD).and_then(|m| m.modified()).ok();
    }

    pub(crate) fn begin_pass(&mut self) {
        self.chain_now.clear();
        self.hover_now = None;
        self.modal_now = false;
        self.typing_secret_now = false;
    }

    /// After a pass: what no widget took moves the focus. Returns whether the focus or the hover
    /// changed.
    pub(crate) fn end_pass(&mut self, left: Option<Input>) -> bool {
        std::mem::swap(&mut self.chain, &mut self.chain_now);
        self.typing_secret = self.typing_secret_now;
        let mut changed = self.modal_prev != self.modal_now;
        if changed {
            // Lucina follows a dialog by event, not by watching the screen (stage D). Nothing is sent with nobody listening.
            lucina_events::emit(if self.modal_now {
                "dialog.opened"
            } else {
                "dialog.closed"
            })
            .send();
        }
        self.modal_prev = self.modal_now;
        if self.hover != self.hover_now {
            self.hover = self.hover_now;
            changed = true;
        }
        match left {
            Some(Input::Key(p)) => {
                let delta = match p.code {
                    keys::KEY_TAB if p.mods.shift => -1,
                    keys::KEY_TAB | keys::KEY_DOWN | keys::KEY_RIGHT => 1,
                    keys::KEY_UP | keys::KEY_LEFT => -1,
                    _ => 0,
                };
                if delta != 0 && !self.chain.is_empty() && !p.mods.ctrl && !p.mods.alt {
                    let n = self.chain.len() as i32;
                    let at = self
                        .focus
                        .and_then(|f| self.chain.iter().position(|&c| c == f));
                    let next = match at {
                        Some(i) => (i as i32 + delta).rem_euclid(n),
                        None => 0,
                    };
                    self.focus = Some(self.chain[next as usize]);
                    changed = true;
                }
            }
            Some(Input::Up(..)) => self.pressed = None,
            _ => {}
        }
        // The focused widget went away, or nothing was focused yet: the first one takes it, so
        // that the keyboard always has somewhere to go.
        if self.focus.is_none_or(|f| !self.chain.contains(&f))
            && self.focus != self.chain.first().copied()
        {
            self.focus = self.chain.first().copied();
            changed = true;
        }
        changed
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Down,
    Right,
}

/// What a [`Ui::hit`] area reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hit {
    /// Pressed and released inside the area.
    pub clicked: bool,
    /// The pointer is over it.
    pub hovered: bool,
    /// The button is down on it.
    pub pressed: bool,
    /// The secondary button was pressed on it: open its context menu.
    pub secondary: bool,
}

/// What a text field reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextResponse {
    pub changed: bool,
    pub submitted: bool,
    pub focused: bool,
}

/// What a list reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListResponse {
    /// Enter, or a click, on this row.
    pub activated: Option<usize>,
    /// The secondary button on this row.
    pub secondary: Option<usize>,
    /// The selection moved.
    pub selection_changed: bool,
}

/// A list's scroll position and selection. Keep one per list in the app's state.
#[derive(Clone, Debug, Default)]
pub struct ListState {
    /// Pixels scrolled from the top.
    pub scroll: i32,
    pub selected: Option<usize>,
    /// The rows mark selection themselves: no band across the list for the selected or hovered row.
    pub plain: bool,
    /// For rows of different heights: where each row starts, and the end of the last.
    offsets: Vec<i32>,
    offsets_width: i32,
    /// The selection moved, so it must be scrolled into view.
    reveal: bool,
}

impl ListState {
    pub fn new() -> ListState {
        ListState::default()
    }

    /// Rows of a measured list changed height: measure them again.
    pub fn invalidate(&mut self) {
        self.offsets.clear();
    }

    /// Select a row and scroll to it.
    pub fn select(&mut self, i: usize) {
        self.selected = Some(i);
        self.reveal = true;
    }

    /// Scroll to the end on the next draw (a chat's newest message, say).
    pub fn scroll_to_end(&mut self) {
        self.scroll = i32::MAX;
    }
}

/// How tall a list's rows are.
enum Rows<'a> {
    Fixed(i32),
    Measured(&'a [i32]),
}

impl Rows<'_> {
    fn top(&self, i: usize) -> i32 {
        match self {
            Rows::Fixed(h) => (i as i32).saturating_mul(*h),
            Rows::Measured(o) => o[i.min(o.len() - 1)],
        }
    }

    /// The row at `y` (content coordinates), which may be one past the last.
    fn at(&self, y: i32) -> usize {
        match self {
            Rows::Fixed(h) => (y / (*h).max(1)).max(0) as usize,
            Rows::Measured(o) => o.partition_point(|&t| t <= y).saturating_sub(1),
        }
    }
}

/// The interface one [`view`](crate::App::view) pass works with.
pub struct Ui<'a> {
    frame: &'a mut Frame,
    text: &'a mut Text,
    st: &'a mut UiState,
    theme: Theme,
    input: Option<Input>,
    painting: bool,
    area: Rect,
    flow: Flow,
    gap: i32,
    clip: Rect,
    in_modal: bool,
}

#[derive(Clone, Copy, Default)]
struct Interaction {
    hovered: bool,
    focused: bool,
    pressed: bool,
    clicked: bool,
    secondary: bool,
}

impl<'a> Ui<'a> {
    pub(crate) fn new(
        frame: &'a mut Frame,
        text: &'a mut Text,
        st: &'a mut UiState,
        theme: Theme,
        input: Option<Input>,
        painting: bool,
    ) -> Ui<'a> {
        let area = frame.bounds();
        frame.set_clip(area);
        if painting {
            frame.fill_rect(area, theme.bg);
        }
        Ui {
            frame,
            text,
            st,
            theme,
            input,
            painting,
            area,
            flow: Flow::Down,
            gap: theme.px(8.0),
            clip: area,
            in_modal: false,
        }
    }

    pub(crate) fn input_left(&self) -> Option<Input> {
        self.input
    }

    pub fn theme(&self) -> Theme {
        self.theme
    }

    /// A length in reference pixels (1024x768) on this screen.
    pub fn px(&self, v: f32) -> i32 {
        self.theme.px(v)
    }

    /// Whether this pass draws. Skip expensive preparation of pictures when it does not.
    pub fn painting(&self) -> bool {
        self.painting
    }

    /// End the app, as if it had returned from `main`, after this pass.
    pub fn quit(&mut self) {
        self.st.quit = true;
    }

    /// Run this program over the app as soon as this pass is over. The app draws nothing and reads no input until it ends, then the
    /// whole screen is drawn again and [`Ui::program_ended`] says how it went.
    pub fn run_program(&mut self, program: crate::program::Program) {
        self.st.program = Some(program);
        self.st.want_paint = true;
    }

    /// How the last program ended, once.
    pub fn program_ended(&mut self) -> Option<crate::program::Ended> {
        self.st.ended.take()
    }

    /// Run `view` again after `d`, for clocks and the like.
    pub fn wake_after(&mut self, d: Duration) {
        let t = Instant::now() + d;
        self.st.wake = Some(self.st.wake.map_or(t, |w| w.min(t)));
    }

    /// Ask for another draw after this input pass; widgets do this themselves. Needed only when a
    /// plain pointer move changed the app's own state.
    pub fn repaint(&mut self) {
        self.st.want_paint = true;
    }

    // ---- Layout ----

    /// What is left of the current area.
    pub fn area(&self) -> Rect {
        self.area
    }

    pub fn cut_top(&mut self, h: i32) -> Rect {
        let h = h.min(self.area.h).max(0);
        let r = Rect::new(self.area.x, self.area.y, self.area.w, h);
        self.area.y += h;
        self.area.h -= h;
        r
    }

    pub fn cut_bottom(&mut self, h: i32) -> Rect {
        let h = h.min(self.area.h).max(0);
        self.area.h -= h;
        Rect::new(self.area.x, self.area.bottom(), self.area.w, h)
    }

    pub fn cut_left(&mut self, w: i32) -> Rect {
        let w = w.min(self.area.w).max(0);
        let r = Rect::new(self.area.x, self.area.y, w, self.area.h);
        self.area.x += w;
        self.area.w -= w;
        r
    }

    pub fn cut_right(&mut self, w: i32) -> Rect {
        let w = w.min(self.area.w).max(0);
        self.area.w -= w;
        Rect::new(self.area.right(), self.area.y, w, self.area.h)
    }

    /// Empty space in the flow, in reference pixels.
    pub fn space(&mut self, v: f32) {
        let d = self.px(v);
        match self.flow {
            Flow::Down => {
                self.cut_top(d);
            }
            Flow::Right => {
                self.cut_left(d);
            }
        }
    }

    /// Shrink the current area on every side, in reference pixels.
    pub fn pad(&mut self, v: f32) {
        self.area = self.area.inset(self.px(v));
    }

    /// Run `f` with `r` as the area, clipped to it.
    pub fn within<R>(&mut self, r: Rect, f: impl FnOnce(&mut Ui) -> R) -> R {
        let (area, flow, clip) = (self.area, self.flow, self.clip);
        self.area = r;
        self.flow = Flow::Down;
        self.clip = clip.intersect(&r);
        self.frame.set_clip(self.clip);
        let out = f(self);
        self.area = area;
        self.flow = flow;
        self.clip = clip;
        self.frame.set_clip(clip);
        out
    }

    /// A row of height `h` (reference pixels) from the flow; widgets inside it go left to right.
    pub fn row<R>(&mut self, h: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
        let h = self.px(h);
        let r = self.next(self.area.w, h);
        self.within(r, |ui| {
            ui.flow = Flow::Right;
            f(ui)
        })
    }

    /// The next place in the flow for something of this size.
    fn next(&mut self, w: i32, h: i32) -> Rect {
        let gap = self.gap;
        match self.flow {
            Flow::Down => {
                let r = self.cut_top(h);
                self.cut_top(gap);
                Rect::new(r.x, r.y, w.min(r.w), r.h)
            }
            Flow::Right => {
                let r = self.cut_left(w);
                self.cut_left(gap);
                Rect::new(r.x, r.y, r.w, h.min(r.h))
            }
        }
    }

    // ---- Drawing ----

    pub fn fill(&mut self, r: Rect, c: Rgb) {
        if self.painting {
            self.frame.fill_rect(r, c);
        }
    }

    pub fn fill_round(&mut self, r: Rect, rad: i32, c: Rgb) {
        if self.painting {
            self.frame.fill_round_rect(r, rad, c);
        }
    }

    /// `s` with its baseline at `y`, from `x`.
    #[allow(clippy::too_many_arguments)]
    pub fn text_at(&mut self, x: i32, y: i32, f: Face, px: u16, c: Rgb, s: &str) {
        if self.st.ax.is_some() {
            let asc = self.text.ascent(f, px);
            let h = asc + self.text.descent(f, px);
            let w = self.text.measure(f, px, s);
            self.ax_text(Role::Label, "label", Rect::new(x, y - asc, w, h), s, true);
        }
        self.text_at_raw(x, y, f, px, c, s);
    }

    #[allow(clippy::too_many_arguments)]
    fn text_at_raw(&mut self, x: i32, y: i32, f: Face, px: u16, c: Rgb, s: &str) {
        if self.painting {
            let clip = self.clip;
            self.text.draw_clipped(self.frame, clip, x, y, f, px, c, s);
        }
    }

    pub fn measure(&mut self, f: Face, px: u16, s: &str) -> i32 {
        self.text.measure(f, px, s)
    }

    /// A line's height in a font.
    pub fn line_height(&mut self, f: Face, px: u16) -> i32 {
        self.text.ascent(f, px) + self.text.descent(f, px) + (px as i32 / 6).max(1)
    }

    /// `s` on one line, cut with an ellipsis to fit, vertically centred in `r`.
    #[allow(clippy::too_many_arguments)]
    pub fn text_in(&mut self, r: Rect, f: Face, px: u16, c: Rgb, s: &str) {
        self.ax_text(Role::Label, "label", r, s, true);
        self.text_in_raw(r, f, px, c, s);
    }

    fn text_in_raw(&mut self, r: Rect, f: Face, px: u16, c: Rgb, s: &str) {
        if !self.painting {
            return;
        }
        let s = self.text.fit(f, px, s, r.w);
        let asc = self.text.ascent(f, px);
        let desc = self.text.descent(f, px);
        self.text_at_raw(r.x, r.y + (r.h + asc - desc) / 2, f, px, c, &s);
    }

    /// One line in the flow, cut to fit; returns where it went.
    fn line(&mut self, f: Face, px: u16, c: Rgb, s: &str) -> Rect {
        let h = self.line_height(f, px);
        let w = if self.flow == Flow::Right {
            self.measure(f, px, s).min(self.area.w)
        } else {
            self.area.w
        };
        let r = self.next(w, h);
        let r = if self.flow == Flow::Right {
            // Centred in the row.
            Rect::new(r.x, self.area.y, r.w, self.area.h)
        } else {
            r
        };
        self.text_in_raw(r, f, px, c, s);
        r
    }

    /// A line of normal text.
    pub fn label(&mut self, s: &str) {
        let (px, c) = (self.theme.pt(15.0), self.theme.text);
        let r = self.line(Face::Sans, px, c, s);
        self.ax_text(Role::Label, "label", r, s, true);
    }

    /// A line of secondary text.
    pub fn dim(&mut self, s: &str) {
        let (px, c) = (self.theme.pt(13.0), self.theme.dim);
        let r = self.line(Face::Sans, px, c, s);
        self.ax_text(Role::Label, "label", r, s, true);
    }

    /// A section's title.
    pub fn heading(&mut self, s: &str) {
        let (px, c) = (self.theme.pt(17.0), self.theme.text);
        let r = self.line(Face::Bold, px, c, s);
        self.ax_text(Role::Heading, "heading", r, s, false);
    }

    /// Text wrapped to the area's width, in the theme's body size.
    pub fn paragraph(&mut self, s: &str) {
        let (px, c) = (self.theme.pt(15.0), self.theme.item);
        self.wrapped(Face::Sans, px, c, s);
    }

    /// Text wrapped to the area's width.
    pub fn wrapped(&mut self, f: Face, px: u16, c: Rgb, s: &str) {
        let w = self.area.w;
        let lines = self.text.wrap(f, px, s, w);
        let lh = self.line_height(f, px);
        let r = self.next(w, lh * lines.len() as i32);
        self.ax_text(Role::Paragraph, "paragraph", r, s, true);
        let asc = self.text.ascent(f, px);
        for (i, l) in lines.iter().enumerate() {
            self.text_at_raw(
                r.x,
                r.y + i as i32 * lh + asc,
                f,
                px,
                c,
                s[l.clone()].trim_end(),
            );
        }
    }

    /// The height of `s` wrapped to `w`, for measured list rows.
    pub fn wrapped_height(text: &mut Text, f: Face, px: u16, s: &str, w: i32) -> i32 {
        let lh = text.ascent(f, px) + text.descent(f, px) + (px as i32 / 6).max(1);
        lh * text.wrap(f, px, s, w).len() as i32
    }

    /// How many lines `s` takes wrapped to `w`, for drawing inside a measured row.
    pub fn wrap_count(&mut self, f: Face, px: u16, s: &str, w: i32) -> i32 {
        self.text.wrap(f, px, s, w).len() as i32
    }

    /// A picture at (`x`, `y`) of the area it is given, centred. Fit it first with
    /// [`Image::fit_within`]: this copies pixels and never scales.
    pub fn image_in(&mut self, r: Rect, img: &Image) {
        self.ax_text(Role::Image, "image", r, "", false);
        if self.painting {
            let x = r.x + (r.w - img.w as i32) / 2;
            let y = r.y + (r.h - img.h as i32) / 2;
            let saved = self.clip;
            self.frame.set_clip(saved.intersect(&r));
            self.frame.draw_image(x, y, img);
            self.frame.set_clip(saved);
        }
    }

    /// A picture in the flow.
    pub fn image(&mut self, img: &Image) {
        let r = self.next(img.w as i32, img.h as i32);
        self.image_in(r, img);
    }

    /// A thin line across the flow.
    pub fn separator(&mut self) {
        let h = self.px(1.0).max(1);
        let r = self.next(self.area.w, h);
        self.fill(r, HAIRLINE);
    }

    /// The app's title bar, at the top of the area. Add it first in `view`.
    pub fn header(&mut self, title: &str) {
        let r = self.cut_top(self.px(36.0).max(24));
        self.ax_text(Role::Heading, "header", r, title, false);
        let t = self.theme;
        self.fill(r, t.bar);
        let rule = Rect::new(r.x, r.bottom() - 2, r.w, 2);
        self.fill(rule, t.accent);
        let pad = t.px(14.0);
        // The app's own icon, when the shell named one.
        let mut left = r.x + pad;
        if let Some(id) = crate::icons::own_name() {
            let side = t.px(23.0).min(r.h - 4);
            let at = Rect::new(left, r.y + (r.h - 2 - side) / 2, side, side);
            self.icon(at, id, "");
            left += side + t.px(8.0);
        }
        let hint = "F1 Home";
        let hint_w = self.measure(Face::Sans, t.pt(13.0), hint);
        // The "camera on" mark, when something captures: a red disc and who, at the right end of the
        // bar, left of the hint. Its width is taken off the title's area so the two never overlap.
        let camera = crate::camera_mark::current();
        let mut cam_w = 0;
        let mut cam_text = String::new();
        let mut cam_cw = 0;
        if let Some((_, program)) = &camera {
            cam_text = crate::camera_mark::label(program);
            cam_cw = self.measure(Face::Sans, t.pt(13.0), &cam_text);
            cam_w = t.px(10.0) + t.px(8.0) + cam_cw + t.px(8.0);
        }
        self.text_in_raw(
            Rect::new(left, r.y, r.right() - 2 * pad - hint_w - cam_w - left, r.h),
            Face::Bold,
            t.pt(15.0),
            t.text,
            title,
        );
        self.text_in_raw(
            Rect::new(r.right() - pad - hint_w, r.y, hint_w, r.h),
            Face::Sans,
            t.pt(13.0),
            t.dim,
            hint,
        );
        if camera.is_some() {
            let gap = t.px(8.0);
            let d = t.px(10.0);
            let text_x = r.right() - pad - hint_w - gap - cam_cw;
            let disc_x = text_x - gap - d;
            if self.painting {
                self.frame.fill_disc(
                    disc_x as f32 + d as f32 / 2.0,
                    (r.y + r.h / 2) as f32,
                    d as f32 / 2.0,
                    t.danger,
                );
            }
            self.text_in_raw(
                Rect::new(text_x, r.y, cam_cw, r.h),
                Face::Sans,
                t.pt(13.0),
                t.text,
                &cam_text,
            );
            self.ax_text(
                Role::Label,
                "camera-on",
                Rect::new(disc_x, r.y, cam_w, r.h),
                &cam_text,
                // A program's name, not the owner's data.
                false,
            );
        }
    }

    /// An app's icon by name, centred in `r`: the same picture the home screen shows ([`crate::icons`]).
    /// `letter` on a plate when the pack has no such icon; with no letter, nothing.
    pub fn icon(&mut self, r: Rect, id: &str, letter: &str) {
        if !self.painting || (letter.is_empty() && crate::icons::get(id, 24).is_none()) {
            return;
        }
        crate::icons::draw_in(self.frame, self.text, r, id, letter);
    }

    // ---- Input ----

    /// Whether this widget is out of reach because a [`modal`](Self::modal) is open.
    fn inert(&self) -> bool {
        self.st.modal_prev && !self.in_modal
    }

    /// Whether this pass's input may reach the widget being placed: inside [`owner_only`](Self::owner_only), nothing Lucina
    /// injected does.
    fn input_reaches(&self) -> bool {
        self.st.owner_only_depth == 0 || lucina_ax::rules::input_reaches(true, self.st.injected)
    }

    fn register(&mut self, id: Id) {
        if !self.inert() {
            self.st.chain_now.push(id);
        }
    }

    fn interact(&mut self, id: Id, r: Rect, focusable: bool) -> Interaction {
        if focusable {
            self.register(id);
        }
        let mut out = Interaction {
            focused: self.st.focus == Some(id),
            ..Interaction::default()
        };
        if self.inert() {
            return out;
        }
        let r = r.intersect(&self.clip);
        let (px, py) = self.st.pointer;
        out.hovered = r.contains(px, py);
        if out.hovered {
            self.st.hover_now = Some(id);
        }
        if !self.input_reaches() {
            out.pressed = self.st.pressed == Some(id);
            return out;
        }
        match self.input {
            Some(Input::Down(x, y)) if r.contains(x, y) => {
                self.st.pressed = Some(id);
                if focusable {
                    self.st.focus = Some(id);
                    out.focused = true;
                }
                self.input = None;
                self.st.want_paint = true;
            }
            Some(Input::Up(x, y)) if self.st.pressed == Some(id) => {
                self.st.pressed = None;
                self.input = None;
                self.st.want_paint = true;
                out.clicked = r.contains(x, y);
            }
            Some(Input::Secondary(x, y)) if r.contains(x, y) => {
                self.input = None;
                self.st.want_paint = true;
                out.secondary = true;
            }
            Some(Input::Key(p))
                if out.focused
                    && focusable
                    && matches!(
                        p.code,
                        keys::KEY_ENTER | keys::KEY_KPENTER | keys::KEY_SPACE
                    )
                    && !p.mods.ctrl
                    && !p.mods.alt =>
            {
                self.input = None;
                out.clicked = true;
            }
            _ => {}
        }
        out.pressed = self.st.pressed == Some(id);
        out
    }

    /// Whether the key `code` was pressed with no modifier, and no widget wanted it. Ask at the end
    /// of `view`; asking takes the key.
    pub fn key(&mut self, code: u16) -> bool {
        match self.input {
            Some(Input::Key(p))
                if p.code == code
                    && !p.mods.ctrl
                    && !p.mods.alt
                    && !p.mods.meta
                    && !p.mods.shift =>
            {
                self.input = None;
                true
            }
            _ => false,
        }
    }

    /// The key pressed in this pass that no widget wanted so far, with its modifiers.
    pub fn press(&mut self) -> Option<Press> {
        match self.input {
            Some(Input::Key(p)) => Some(p),
            _ => None,
        }
    }

    /// The event of this pass that no widget took so far, for a surface that handles its own input,
    /// such as a browser's page. Call it after the widgets.
    pub fn input(&self) -> Option<Input> {
        self.input
    }

    /// Take the current event: [`input`](Self::input) found a use for it.
    pub fn take_input(&mut self) {
        self.input = None;
    }

    /// Opaque pixels (`0x00RRGGBB`, rows of `stride`) at the corner of `r`, clipped to it. Never scales.
    pub fn pixels(&mut self, r: Rect, stride: usize, px: &[u32]) {
        self.ax_text(Role::Image, "image", r, "", false);
        if self.painting {
            let saved = self.clip;
            self.frame.set_clip(saved.intersect(&r));
            self.frame.copy_pixels(r.x, r.y, stride, px);
            self.frame.set_clip(saved);
        }
    }

    /// Take the current key: [`press`](Self::press) found a use for it.
    pub fn take_press(&mut self) {
        if matches!(self.input, Some(Input::Key(_))) {
            self.input = None;
        }
    }

    /// Whether this widget has the keyboard.
    pub fn is_focused(&self, id: impl Into<Id>) -> bool {
        self.st.focus == Some(id.into())
    }

    /// Give the keyboard to this widget.
    pub fn focus(&mut self, id: impl Into<Id>) {
        let id = id.into();
        if self.st.focus != Some(id) {
            self.st.focus = Some(id);
            self.st.want_paint = true;
        }
    }

    /// Put `s` on the clipboard, as Ctrl+C in a field does.
    pub fn copy(&mut self, s: &str) {
        self.st.clipboard = s.to_owned();
        self.st.clipboard_save();
    }

    /// Forget the clipboard if it holds exactly `s`, here and in the file the apps share: after a pasted secret was saved.
    pub fn forget_clipboard_if(&mut self, s: &str) {
        self.st.clipboard_load();
        if !s.is_empty() && self.st.clipboard.trim_end_matches(['\r', '\n']) == s {
            self.st.clipboard.clear();
            let _ = std::fs::remove_file(CLIPBOARD);
            self.st.clipboard_at = None;
        }
    }

    // ---- Widgets ----

    /// A 2px accent ring 3px outside `r`, following its radius; the widget itself is drawn over it.
    fn focus_ring(&mut self, r: Rect, rad: i32, under: Rgb) {
        let t = self.theme;
        let off = t.px(3.0).max(3);
        let w = t.px(2.0).max(2);
        let ring = Rect::new(r.x - off, r.y - off, r.w + 2 * off, r.h + 2 * off);
        self.frame
            .bordered_round_rect(ring, rad + off, w, t.accent, under);
    }

    fn draw_button(&mut self, r: Rect, label: &str, i: Interaction, look: Look) {
        if !self.painting {
            return;
        }
        let t = self.theme;
        let rad = (r.h / 2).max(1);
        let fill = match look {
            Look::Primary => t.selection,
            Look::Danger => t.danger,
            Look::Plain if i.pressed => Rgb::hex(0).over(t.panel_hi, 64),
            Look::Plain if i.hovered => Rgb::hex(0xffffff).over(t.panel_hi, 20),
            Look::Plain => t.panel_hi,
        };
        if i.focused {
            self.focus_ring(r, rad, fill);
        }
        self.frame.fill_round_rect(r, rad, fill);
        let px = t.pt(15.0);
        let w = self.text.measure(Face::Sans, px, label);
        let color = t.text;
        let inner = Rect::new(
            r.x + (r.w - w).max(t.px(6.0)) / 2,
            r.y,
            r.w - t.px(6.0),
            r.h,
        );
        self.text_in_raw(inner, Face::Sans, px, color, label);
    }

    fn button_size(&mut self, label: &str) -> (i32, i32) {
        let t = self.theme;
        let w = self.measure(Face::Sans, t.pt(15.0), label) + 2 * t.px(14.0);
        (w, t.px(44.0))
    }

    /// A button in the flow, named by its label. True when it was clicked or activated.
    pub fn button(&mut self, label: &str) -> bool {
        self.button_id(label, label)
    }

    /// A button with its own id, for labels that repeat.
    pub fn button_id(&mut self, id: impl Into<Id>, label: &str) -> bool {
        let (w, h) = self.button_size(label);
        let r = self.next(w, h);
        self.button_at(r, id, label)
    }

    /// The window's main action: filled with the accent colour.
    pub fn primary_button(&mut self, label: &str) -> bool {
        let (w, h) = self.button_size(label);
        let r = self.next(w, h);
        self.styled_button(r, label.into(), label, Look::Primary)
    }

    /// A button for what cannot be undone (delete, overwrite), in the danger colour. Lucina sees it as destructive and asks the
    /// owner before pressing it.
    pub fn danger_button(&mut self, label: &str) -> bool {
        let (w, h) = self.button_size(label);
        let r = self.next(w, h);
        self.styled_button(r, label.into(), label, Look::Danger)
    }

    /// A button in a rectangle of your own.
    pub fn button_at(&mut self, r: Rect, id: impl Into<Id>, label: &str) -> bool {
        self.styled_button(r, id.into(), label, Look::Plain)
    }

    fn styled_button(&mut self, r: Rect, id: Id, label: &str, look: Look) -> bool {
        let mut i = self.interact(id, r, true);
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(id, Role::Button, r);
            w.name = label;
            w.actions = PRESS;
            w.destructive = look == Look::Danger;
            i.clicked |= self.ax_press(id, &w);
        }
        self.draw_button(r, label, i, look);
        i.clicked
    }

    /// An area that answers the pointer and nothing else: it draws nothing and stays out of the focus
    /// order. For things inside a row that a click should reach, such as a calendar's events and empty
    /// slots. It respects the clip and dialogs, and inside a list row it takes the click before the row
    /// does. Name it for Lucina with [`describe`](Self::describe).
    pub fn hit(&mut self, id: impl Into<Id>, r: Rect) -> Hit {
        let id = id.into();
        let mut i = self.interact(id, r, false);
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(id, Role::Unknown, r);
            w.actions = CLICK;
            i.clicked |= self.ax_press(id, &w);
        }
        Hit {
            clicked: i.clicked,
            hovered: i.hovered,
            pressed: i.pressed,
            secondary: i.secondary,
        }
    }

    /// A [`hit`](Self::hit) that can have the keyboard: clicking gives it the focus and Tab walks to it.
    /// For a surface that reads its own keys through [`input`](Self::input), before calling this.
    pub fn focus_area(&mut self, id: impl Into<Id>, r: Rect) -> Hit {
        let id = id.into();
        let mut i = self.interact(id, r, true);
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(id, Role::Unknown, r);
            w.actions = PRESS;
            i.clicked |= self.ax_press(id, &w);
        }
        Hit {
            clicked: i.clicked,
            hovered: i.hovered,
            pressed: i.pressed,
            secondary: i.secondary,
        }
    }

    /// Where the pointer is, for working out what a [`hit`](Self::hit) landed on.
    pub fn pointer(&self) -> (i32, i32) {
        self.st.pointer
    }

    /// A box with a label; clicking or Space flips `on`. True when it changed.
    pub fn checkbox(&mut self, label: &str, on: &mut bool) -> bool {
        let t = self.theme;
        let px = t.pt(15.0);
        let w = t.px(30.0) + self.measure(Face::Sans, px, label);
        let h = t.px(26.0);
        let r = self.next(w, h);
        let id: Id = label.into();
        let mut i = self.interact(id, r, true);
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(id, Role::CheckBox, r);
            w.name = label;
            w.actions = PRESS;
            w.toggled = Some(*on);
            i.clicked |= self.ax_press(id, &w);
        }
        if i.clicked {
            *on = !*on;
            self.st.want_paint = true;
        }
        if self.painting {
            let size = t.px(20.0);
            let b = Rect::new(r.x, r.y + (r.h - size) / 2, size, size);
            let rad = t.px(4.0);
            let fill = if *on { t.accent } else { t.panel_hi };
            if i.focused {
                self.focus_ring(b, rad, fill);
            }
            self.frame
                .bordered_round_rect(b, rad, t.px(1.0).max(1), HAIRLINE, fill);
            if *on {
                let m = t.pt(15.0);
                let tick = "✓";
                let asc = self.text.ascent(Face::Sans, m);
                let tw = self.measure(Face::Sans, m, tick);
                self.text_at_raw(
                    b.x + (b.w - tw) / 2,
                    b.y + (b.h + asc) / 2 - t.px(2.0),
                    Face::Sans,
                    m,
                    t.bg,
                    tick,
                );
            }
            let tr = Rect::new(r.x + t.px(30.0), r.y, r.w - t.px(30.0), r.h);
            self.text_in_raw(tr, Face::Sans, px, t.text, label);
        }
        i.clicked
    }

    /// A one-line text field in the flow, as wide as the area. Give the field an id of your own.
    pub fn text_field(&mut self, id: impl Into<Id>, e: &mut TextEdit) -> TextResponse {
        let h = self.px(34.0);
        let w = self.area.w;
        let r = self.next(w, h);
        self.text_field_at(r, id, e)
    }

    /// A multi-line text box, `rows` lines tall, in the flow.
    pub fn text_area(&mut self, id: impl Into<Id>, e: &mut TextEdit, rows: i32) -> TextResponse {
        let px = self.theme.pt(15.0);
        let lh = self.line_height(Face::Sans, px);
        let h = lh * rows + 2 * self.px(6.0);
        let w = self.area.w;
        let r = self.next(w, h);
        e.multiline = true;
        self.text_field_at(r, id, e)
    }

    /// A text field in a rectangle of your own.
    pub fn text_field_at(&mut self, r: Rect, id: impl Into<Id>, e: &mut TextEdit) -> TextResponse {
        let id = id.into();
        let t = self.theme;
        let px = t.pt(15.0);
        let pad = t.px(10.0);
        let inner = if e.multiline {
            r.inset(t.px(6.0))
        } else {
            Rect::new(r.x + pad, r.y, r.w - 2 * pad, r.h)
        };
        let inner = Rect::new(inner.x, inner.y, inner.w.max(1), inner.h.max(1));
        let mut out = TextResponse::default();
        self.register(id);
        let mut focused = self.st.focus == Some(id);
        let inert = self.inert();
        let shown = Shown::new(e);
        let lines = if e.multiline {
            self.text.wrap(Face::Sans, px, &shown.text, inner.w)
        } else {
            std::iter::once(0..shown.text.len()).collect()
        };
        let lh = self.line_height(Face::Sans, px);
        let clipped = r.intersect(&self.clip);
        let (hx, hy) = self.st.pointer;
        if !inert && clipped.contains(hx, hy) {
            self.st.hover_now = Some(id);
        }

        let hit = |ui: &mut Ui, x: i32, y: i32, e: &TextEdit| -> usize {
            let line = if e.multiline {
                (((y - inner.y + e.scroll) / lh).max(0) as usize).min(lines.len() - 1)
            } else {
                0
            };
            let scroll_x = if e.multiline { 0 } else { e.scroll };
            let at = ui.text.offset_at(
                Face::Sans,
                px,
                &shown.text[lines[line].clone()],
                x - inner.x + scroll_x,
            );
            shown.real(lines[line].start + at, e)
        };
        if e.password && focused {
            self.st.typing_secret_now = true;
        }
        let reaches = self.input_reaches();
        match self.input {
            _ if inert || !reaches => {}
            Some(Input::Down(x, y)) if clipped.contains(x, y) => {
                self.st.pressed = Some(id);
                self.st.focus = Some(id);
                focused = true;
                let at = hit(self, x, y, e);
                e.move_to(at, false);
                self.input = None;
                self.st.want_paint = true;
            }
            Some(Input::Move(x, y)) if self.st.pressed == Some(id) => {
                let at = hit(self, x, y, e);
                if at != e.cursor {
                    e.move_to(at, true);
                    self.st.want_paint = true;
                }
                self.input = None;
            }
            Some(Input::Up(..)) if self.st.pressed == Some(id) => {
                self.st.pressed = None;
                self.input = None;
            }
            Some(Input::Wheel(n)) if clipped.contains(hx, hy) && e.multiline => {
                e.scroll -= n * lh;
                self.input = None;
                self.st.want_paint = true;
            }
            Some(Input::Scroll { dy, .. }) if clipped.contains(hx, hy) && e.multiline => {
                e.scroll -= dy;
                self.input = None;
                self.st.want_paint = true;
            }
            Some(Input::Key(p)) if focused => {
                let vertical = e.multiline && matches!(p.code, keys::KEY_UP | keys::KEY_DOWN);
                if vertical {
                    self.input = None;
                    let line = shown.line_of(e.cursor, &lines);
                    let target = if p.code == keys::KEY_UP {
                        line.checked_sub(1)
                    } else {
                        (line + 1 < lines.len()).then_some(line + 1)
                    };
                    if let Some(n) = target {
                        let cur = shown.shown(e.cursor) - lines[line].start;
                        let goal = e.goal_x.unwrap_or_else(|| {
                            self.text.measure(
                                Face::Sans,
                                px,
                                &shown.text[lines[line].start..lines[line].start + cur],
                            )
                        });
                        let at = self.text.offset_at(
                            Face::Sans,
                            px,
                            &shown.text[lines[n].clone()],
                            goal,
                        );
                        e.move_to(shown.real(lines[n].start + at, e), p.mods.shift);
                        e.goal_x = Some(goal);
                    }
                    self.st.want_paint = true;
                } else {
                    let paste = p.mods.ctrl && p.code == keys::KEY_V;
                    if paste {
                        self.st.clipboard_load();
                    }
                    let before = self.st.clipboard.clone();
                    let k = e.key(p, &mut self.st.clipboard);
                    if self.st.clipboard != before {
                        self.st.clipboard_save();
                    }
                    if k.used {
                        self.input = None;
                        self.st.want_paint = true;
                    }
                    out.changed = k.changed;
                    out.submitted = k.submitted;
                }
            }
            _ => {}
        }
        if self.st.ax.is_some() {
            let role = if e.password {
                Role::PasswordInput
            } else if e.multiline {
                Role::MultilineTextInput
            } else {
                Role::TextInput
            };
            let mut w = self.ax_widget(id, role, r);
            w.name = &e.placeholder;
            w.actions = FIELD;
            if e.password {
                w.kind = Kind::Password;
            } else {
                w.value = Some(&e.text);
            }
            match self.ax_place(&w, false) {
                Some(Act::SetValue) => {
                    let v = self
                        .st
                        .ax
                        .as_mut()
                        .and_then(|ax| ax.value.take())
                        .unwrap_or_default();
                    e.set(&v);
                    self.focus(id);
                    focused = true;
                    out.changed = true;
                }
                Some(Act::Focus) => {
                    self.focus(id);
                    focused = true;
                }
                Some(Act::Click) => {
                    self.focus(id);
                    focused = true;
                    // What Enter does on one line.
                    out.submitted = !e.multiline;
                }
                _ => {}
            }
        }
        out.focused = focused;

        // The key may have changed the text, so the layout is made again for drawing and for
        // keeping the cursor in view.
        let shown = Shown::new(e);
        let lines = if e.multiline {
            self.text.wrap(Face::Sans, px, &shown.text, inner.w)
        } else {
            std::iter::once(0..shown.text.len()).collect()
        };
        let cursor = shown.shown(e.cursor);
        let cur_line = shown.line_of(e.cursor, &lines);
        let cur_x = self
            .text
            .measure(Face::Sans, px, &shown.text[lines[cur_line].start..cursor]);
        if e.multiline {
            let top = cur_line as i32 * lh;
            if focused {
                if top < e.scroll {
                    e.scroll = top;
                } else if top + lh > e.scroll + inner.h {
                    e.scroll = top + lh - inner.h;
                }
            }
            let most = (lines.len() as i32 * lh - inner.h).max(0);
            e.scroll = e.scroll.clamp(0, most);
        } else {
            if cur_x < e.scroll {
                e.scroll = cur_x;
            } else if cur_x > e.scroll + inner.w - 2 {
                e.scroll = cur_x - inner.w + 2;
            }
            e.scroll = e.scroll.max(0);
        }

        if self.painting {
            let rad = t.px(12.0);
            if focused {
                self.focus_ring(r, rad, t.panel_hi);
            }
            self.frame
                .bordered_round_rect(r, rad, t.px(1.0).max(1), HAIRLINE, t.panel_hi);
            let asc = self.text.ascent(Face::Sans, px);
            let desc = self.text.descent(Face::Sans, px);
            let saved = self.clip;
            let view = saved.intersect(&inner);
            self.frame.set_clip(view);
            self.clip = view;
            let (dx, dy) = if e.multiline {
                (0, -e.scroll)
            } else {
                (-e.scroll, (inner.h - lh) / 2)
            };
            if e.text.is_empty() && !focused && !e.placeholder.is_empty() {
                let (ph, base) = (e.placeholder.clone(), inner.y + dy + asc);
                self.text_at_raw(inner.x, base, Face::Sans, px, t.dim, &ph);
            }
            let sel = e.selection();
            let sel = shown.shown(sel.start)..shown.shown(sel.end);
            for (i, l) in lines.iter().enumerate() {
                let top = inner.y + dy + i as i32 * lh;
                if top + lh < view.y || top > view.bottom() {
                    continue;
                }
                let x = inner.x + dx;
                if sel.start < sel.end && sel.start <= l.end && sel.end >= l.start {
                    let a = sel.start.max(l.start);
                    let b = sel.end.min(l.end);
                    let xa = self.text.measure(Face::Sans, px, &shown.text[l.start..a]);
                    let xb = self.text.measure(Face::Sans, px, &shown.text[l.start..b]);
                    let extra = if sel.end > l.end { t.px(4.0) } else { 0 };
                    self.fill(Rect::new(x + xa, top, xb - xa + extra, lh), t.selection);
                }
                let line = shown.text[l.clone()].trim_end_matches('\n');
                self.text_at_raw(x, top + asc, Face::Sans, px, t.text, line);
                if focused && i == cur_line {
                    let w = t.px(2.0).max(1);
                    let c = Rect::new(x + cur_x, top + asc - asc, w, asc + desc);
                    self.fill(c, t.accent);
                }
            }
            self.clip = saved;
            self.frame.set_clip(saved);
        }
        out
    }

    /// A list of `n` rows, all `row_h` reference pixels tall. Only the rows in view are visited, so
    /// its length does not matter. `row` draws one row: it gets the row's index and whether it is
    /// selected, with the row as its area. Widgets inside a row get an id with the index.
    pub fn list(
        &mut self,
        id: impl Into<Id>,
        st: &mut ListState,
        r: Rect,
        n: usize,
        row_h: f32,
        row: impl FnMut(&mut Ui, usize, bool),
    ) -> ListResponse {
        let h = self.px(row_h).max(1).max(self.px(44.0));
        self.list_inner(id.into(), st, r, n, Rows::Fixed(h), row)
    }

    /// A list whose rows have different heights: `measure` gives a row's height for a width. The
    /// results are kept, so call [`ListState::invalidate`] when rows change other than by being
    /// added at the end.
    pub fn list_measured(
        &mut self,
        id: impl Into<Id>,
        st: &mut ListState,
        r: Rect,
        n: usize,
        mut measure: impl FnMut(&mut Text, usize, i32) -> i32,
        row: impl FnMut(&mut Ui, usize, bool),
    ) -> ListResponse {
        let width = r.w - self.scrollbar_w();
        if st.offsets_width != width || st.offsets.len() > n + 1 {
            st.offsets.clear();
            st.offsets_width = width;
        }
        if st.offsets.is_empty() {
            st.offsets.push(0);
        }
        while st.offsets.len() < n + 1 {
            let i = st.offsets.len() - 1;
            let top = *st.offsets.last().unwrap_or(&0);
            st.offsets.push(top + measure(self.text, i, width).max(1));
        }
        let offsets = std::mem::take(&mut st.offsets);
        let out = self.list_inner(id.into(), st, r, n, Rows::Measured(&offsets), row);
        st.offsets = offsets;
        out
    }

    fn scrollbar_w(&self) -> i32 {
        self.px(6.0).max(4)
    }

    fn list_inner(
        &mut self,
        id: Id,
        st: &mut ListState,
        r: Rect,
        n: usize,
        rows: Rows,
        mut row: impl FnMut(&mut Ui, usize, bool),
    ) -> ListResponse {
        let t = self.theme;
        let mut out = ListResponse::default();
        self.register(id);
        let inert = self.inert();
        let focused = self.st.focus == Some(id);
        let clipped = r.intersect(&self.clip);
        let total = rows.top(n);
        let most = (total - r.h).max(0);
        st.scroll = st.scroll.clamp(0, most);
        if st.selected.is_some_and(|s| s >= n) {
            st.selected = n.checked_sub(1);
        }
        let (hx, hy) = self.st.pointer;
        let hover_row = (!inert && clipped.contains(hx, hy))
            .then(|| rows.at(hy - r.y + st.scroll))
            .filter(|&i| i < n);
        if hover_row.is_some() {
            self.st.hover_now = Some(Id::from((
                "list-hover",
                hover_row.unwrap_or(0) ^ id.0 as usize,
            )));
        }

        // Lucina's action on the list, then keys and the wheel: they move the list before the rows are visited.
        let ax_depth = self.st.ax.as_ref().map(|ax| ax.builder.depth());
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(id, Role::ListBox, r);
            w.actions = LIST;
            let act = self.ax_place(&w, true);
            let up = self
                .st
                .ax
                .as_ref()
                .is_some_and(|ax| ax.action == Some(Action::ScrollUp));
            match act {
                Some(Act::Scroll) => {
                    let by = if up { -r.h } else { r.h };
                    st.scroll = st.scroll.saturating_add(by).clamp(0, most);
                    self.st.want_paint = true;
                }
                Some(Act::Focus) => self.focus(id),
                _ => {}
            }
        }
        let reaches = self.input_reaches();
        if !inert && reaches {
            match self.input {
                Some(Input::Key(p)) if focused && !p.mods.ctrl && !p.mods.alt => {
                    let page = (r.h / rows.top(1).max(1)).max(1) as usize;
                    let cur = st.selected;
                    // The pure arithmetic is `lucina-nav` (proved by Verus): the ends stop, nothing wraps, and a move
                    // never leaves 0..n. A key the list answers is taken even when the selection does not change (Down on
                    // the last row, Up on the first), so that it does not walk the focus off the list.
                    let next = match p.code {
                        keys::KEY_DOWN => match cur {
                            None if n > 0 => Some(rows.at(st.scroll).min(n - 1)),
                            _ => lucina_nav::next(cur, n),
                        },
                        keys::KEY_UP => lucina_nav::prev(cur, n),
                        keys::KEY_PAGEDOWN => lucina_nav::page_down(cur, n, page),
                        keys::KEY_PAGEUP => lucina_nav::page_up(cur.or(Some(0)), n, page),
                        keys::KEY_HOME => lucina_nav::first(n),
                        keys::KEY_END => lucina_nav::last(n),
                        _ => None,
                    };
                    let answers = matches!(
                        p.code,
                        keys::KEY_DOWN
                            | keys::KEY_UP
                            | keys::KEY_PAGEDOWN
                            | keys::KEY_PAGEUP
                            | keys::KEY_HOME
                            | keys::KEY_END
                    ) && n > 0;
                    if answers && next == cur {
                        self.input = None;
                        self.st.want_paint = true;
                    } else if let Some(s) = next {
                        st.select(s);
                        out.selection_changed = true;
                        self.input = None;
                        self.st.want_paint = true;
                    } else if matches!(p.code, keys::KEY_ENTER | keys::KEY_KPENTER)
                        && let Some(s) = cur
                    {
                        out.activated = Some(s);
                        self.input = None;
                        self.st.want_paint = true;
                    }
                }
                Some(Input::Wheel(d)) if clipped.contains(hx, hy) => {
                    st.scroll = (st.scroll - d * t.px(40.0)).clamp(0, most);
                    self.input = None;
                    self.st.want_paint = true;
                }
                Some(Input::Scroll { dy, .. }) if clipped.contains(hx, hy) => {
                    st.scroll = (st.scroll - dy).clamp(0, most);
                    self.input = None;
                    self.st.want_paint = true;
                }
                _ => {}
            }
        }
        if st.reveal {
            st.reveal = false;
            if let Some(s) = st.selected {
                let top = rows.top(s);
                let bottom = rows.top(s + 1);
                if top < st.scroll {
                    st.scroll = top;
                } else if bottom > st.scroll + r.h {
                    st.scroll = bottom - r.h;
                }
                st.scroll = st.scroll.clamp(0, most);
            }
        }

        // The visible rows.
        let bar_w = self.scrollbar_w();
        let row_w = r.w - bar_w;
        let saved_clip = self.clip;
        let saved_area = self.area;
        let saved_flow = self.flow;
        self.clip = clipped;
        self.frame.set_clip(clipped);
        let mut i = rows.at(st.scroll);
        while i < n {
            let top = r.y + rows.top(i) - st.scroll;
            if top >= r.bottom() {
                break;
            }
            let bottom = r.y + rows.top(i + 1) - st.scroll;
            let rect = Rect::new(r.x, top, row_w, bottom - top);
            let selected = st.selected == Some(i);
            if self.painting && !st.plain {
                let inset = t.px(4.0).max(2);
                let row_r = Rect::new(rect.x + inset, rect.y, (rect.w - 2 * inset).max(1), rect.h);
                if selected {
                    // SELECTION at ~45% over the panel: a tinted row, never a bright bar.
                    self.frame.fill_round_rect(
                        row_r,
                        t.px(8.0).max(4),
                        t.selection.over(t.panel, 114),
                    );
                } else if hover_row == Some(i) {
                    self.frame
                        .fill_round_rect(row_r, t.px(8.0).max(4), t.panel_hi);
                }
            }
            self.area = rect;
            self.flow = Flow::Down;
            let row_depth = self.st.ax.as_ref().map(|ax| ax.builder.depth());
            if self.st.ax.is_some() {
                let mut w = self.ax_widget(Id::from(("row", i)), Role::ListBoxOption, rect);
                w.focused = false;
                w.actions = PRESS;
                w.selected = Some(selected);
                w.name_from_children = true;
                match self.ax_place(&w, true) {
                    Some(Act::Click) => {
                        self.focus(id);
                        st.selected = Some(i);
                        out.selection_changed = true;
                        out.activated = Some(i);
                        self.st.want_paint = true;
                    }
                    Some(Act::Focus) => {
                        st.select(i);
                        out.selection_changed = true;
                        self.st.want_paint = true;
                    }
                    _ => {}
                }
            }
            row(self, i, selected);
            if let (Some(ax), Some(d)) = (self.st.ax.as_mut(), row_depth) {
                ax.builder.close_to(d);
            }
            i += 1;
        }
        if let (Some(ax), Some(d)) = (self.st.ax.as_mut(), ax_depth) {
            ax.builder.close_to(d);
        }
        self.area = saved_area;
        self.flow = saved_flow;
        self.clip = saved_clip;
        self.frame.set_clip(saved_clip);

        // A click no widget inside a row took.
        if !inert && reaches {
            match self.input {
                Some(Input::Down(x, y)) if clipped.contains(x, y) => {
                    self.st.focus = Some(id);
                    let i = rows.at(y - r.y + st.scroll);
                    if i < n {
                        st.selected = Some(i);
                        out.selection_changed = true;
                        self.st.pressed = Some(id);
                    }
                    self.input = None;
                    self.st.want_paint = true;
                }
                Some(Input::Secondary(x, y)) if clipped.contains(x, y) => {
                    let i = rows.at(y - r.y + st.scroll);
                    if i < n {
                        st.selected = Some(i);
                        out.secondary = Some(i);
                    }
                    self.input = None;
                    self.st.want_paint = true;
                }
                Some(Input::Up(x, y)) if self.st.pressed == Some(id) => {
                    self.st.pressed = None;
                    self.input = None;
                    if clipped.contains(x, y) {
                        let i = rows.at(y - r.y + st.scroll);
                        if i < n {
                            out.activated = Some(i);
                        }
                    }
                }
                _ => {}
            }
        }

        if self.painting && total > r.h && r.h > 0 {
            let h = ((r.h as i64 * r.h as i64 / total as i64) as i32)
                .max(t.px(24.0))
                .min(r.h);
            let y = r.y + ((st.scroll as i64 * (r.h - h) as i64) / most.max(1) as i64) as i32;
            let c = if focused { t.accent } else { t.panel_hi };
            self.fill_round(
                Rect::new(r.right() - bar_w + 1, y, bar_w - 2, h),
                bar_w / 2,
                c,
            );
        }
        out
    }

    /// A dialog over everything, centred, `w` x `h` reference pixels. While it is open only the
    /// widgets inside it answer. Call it last in `view`.
    pub fn modal<R>(&mut self, w: f32, h: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
        let t = self.theme;
        let screen = self.frame.bounds();
        let (w, h) = (t.px(w).min(screen.w), t.px(h).min(screen.h));
        let r = Rect::new((screen.w - w) / 2, (screen.h - h) / 2, w, h);
        if self.painting {
            self.frame.set_clip(screen);
            self.frame.dim(0x80);
            self.frame
                .bordered_round_rect(r, t.px(24.0), t.px(1.0).max(1), HAIRLINE, t.panel);
        }
        self.st.modal_now = true;
        let was = self.in_modal;
        // The first pass with the dialog only shows it; from the next one the focus stays inside.
        self.in_modal = true;
        let saved_clip = self.clip;
        self.clip = screen;
        let depth = self.st.ax.as_ref().map(|ax| ax.builder.depth());
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(Id(tree::key("dialog")), Role::Dialog, r);
            w.focused = false;
            self.ax_place(&w, true);
        }
        let out = self.within(r.inset(t.px(16.0)), f);
        if let (Some(ax), Some(d)) = (self.st.ax.as_mut(), depth) {
            ax.builder.close_to(d);
        }
        self.clip = saved_clip;
        self.frame.set_clip(saved_clip);
        self.in_modal = was;
        out
    }

    // ---- What Lucina reads (lucina-ax) ----

    /// Names the widget placed last, for Lucina: a [`hit`](Self::hit) area, a custom widget.
    pub fn describe(&mut self, name: &str) {
        if let Some(ax) = self.st.ax.as_mut() {
            ax.builder.describe_last(name);
        }
    }

    /// Publishes one field of the app's own state for Lucina, beside what it draws: a game's map and position, a player's track
    /// and time. Nothing is drawn. Use it for what the screen shows only as pixels; never for a secret.
    pub fn state(&mut self, name: &str, value: &str) {
        if let Some(ax) = self.st.ax.as_mut() {
            ax.builder.state(name, value);
        }
    }

    /// Tags the widget placed last with what it shows, as the app's intents name it (`note:n_42`), so that "open this one" on
    /// screen and the intents agree.
    pub fn entity(&mut self, entity: &str) {
        if let Some(ax) = self.st.ax.as_mut() {
            ax.builder.entity_last(entity);
        }
    }

    /// An area whose values and drawn text Lucina does not get: she reads its widgets (buttons, fields by their labels) and may
    /// work them, but the text it draws and the values of its fields are held back and marked secret. For secrets and codes.
    pub fn private<R>(&mut self, name: &str, f: impl FnOnce(&mut Ui) -> R) -> R {
        let depth = self.st.ax.as_ref().map(|ax| ax.builder.depth());
        if self.st.ax.is_some() {
            let mut w = self.ax_widget(Id(tree::key("private")), Role::GenericContainer, self.area);
            w.focused = false;
            w.name = name;
            w.kind = Kind::Private;
            self.ax_place(&w, true);
        }
        let out = f(self);
        if let (Some(ax), Some(d)) = (self.st.ax.as_mut(), depth) {
            ax.builder.close_to(d);
        }
        out
    }

    /// Widgets only the owner's own hands can work: approving a sign-in, a pairing. Lucina's actions on them are refused, and
    /// the input she injects does not reach them.
    pub fn owner_only<R>(&mut self, f: impl FnOnce(&mut Ui) -> R) -> R {
        self.st.owner_only_depth += 1;
        if let Some(ax) = self.st.ax.as_mut() {
            ax.builder.begin_owner_only();
        }
        let out = f(self);
        if let Some(ax) = self.st.ax.as_mut() {
            ax.builder.end_owner_only();
        }
        self.st.owner_only_depth -= 1;
        out
    }

    /// A node for the widget `id` at `r`, for the tree being read.
    fn ax_widget<'w>(&self, id: Id, role: Role, r: Rect) -> Widget<'w> {
        let mut w = Widget::new(id.raw(), role);
        w.bounds = (r.x, r.y, r.w, r.h);
        w.inert = self.inert();
        w.focused = self.st.focus == Some(id);
        w
    }

    /// Places `w` in the tree being read, as a leaf or as a container whose children follow, and returns Lucina's action if it
    /// names this widget and may reach it.
    fn ax_place(&mut self, w: &Widget, container: bool) -> Option<Act> {
        let ax = self.st.ax.as_mut()?;
        let placed = if container {
            ax.builder.open(w)
        } else {
            ax.builder.leaf(w)
        };
        let act = lucina_ax::rules::take(&mut ax.pending, placed.id, placed.may_act);
        ax.taken |= act.is_some();
        act
    }

    /// A widget that takes Click and Focus: places it and gives it the focus when asked. True for a click.
    fn ax_press(&mut self, id: Id, w: &Widget) -> bool {
        match self.ax_place(w, false) {
            Some(Act::Click) => {
                self.st.want_paint = true;
                true
            }
            Some(Act::Focus) => {
                self.focus(id);
                false
            }
            _ => false,
        }
    }

    /// Text or a picture with no actions, keyed by its role word: occurrences keep repeated ones apart.
    fn ax_text(&mut self, role: Role, word: &str, r: Rect, s: &str, content: bool) {
        if self.st.ax.is_some() {
            let mut w = Widget::new(tree::key(word), role);
            w.bounds = (r.x, r.y, r.w, r.h);
            w.inert = self.inert();
            w.name = s;
            w.content = content;
            self.ax_place(&w, false);
        }
    }
}

/// How a button is filled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Plain,
    Primary,
    Danger,
}

const CLICK: &[Action] = &[Action::Click];
const PRESS: &[Action] = &[Action::Click, Action::Focus];
const FIELD: &[Action] = &[Action::Click, Action::Focus, Action::SetValue];
const LIST: &[Action] = &[Action::Focus, Action::ScrollDown, Action::ScrollUp];

/// One pass of `view` that records the tree Lucina reads, and may carry her action to the widget it names.
pub(crate) struct AxPass {
    pub(crate) builder: lucina_ax::Builder,
    pub(crate) pending: Option<lucina_ax::rules::Pending>,
    /// The text of a `set_value`.
    pub(crate) value: Option<String>,
    /// The AccessKit action, for the direction of a scroll.
    pub(crate) action: Option<Action>,
    /// A widget took the action.
    pub(crate) taken: bool,
}

/// A text field's characters as they are drawn: the text itself, or bullets for a password.
struct Shown {
    text: String,
    /// The real text, for a password.
    real: Option<String>,
}

impl Shown {
    fn new(e: &TextEdit) -> Shown {
        if e.password {
            Shown {
                text: "•".repeat(e.text.chars().count()),
                real: Some(e.text.clone()),
            }
        } else {
            Shown {
                text: e.text.clone(),
                real: None,
            }
        }
    }

    /// Where the real byte offset `at` falls in the drawn string.
    fn shown(&self, at: usize) -> usize {
        match &self.real {
            Some(real) => real[..at.min(real.len())].chars().count() * '•'.len_utf8(),
            None => at,
        }
    }

    /// Where the drawn byte offset `at` falls in the real text.
    fn real(&self, at: usize, e: &TextEdit) -> usize {
        if self.real.is_some() {
            let n = at / '•'.len_utf8();
            e.text
                .char_indices()
                .nth(n)
                .map_or(e.text.len(), |(i, _)| i)
        } else {
            at
        }
    }

    /// The line holding the real offset `at`.
    fn line_of(&self, at: usize, lines: &[Range<usize>]) -> usize {
        let s = self.shown(at);
        lines.iter().rposition(|l| l.start <= s).unwrap_or(0)
    }
}
