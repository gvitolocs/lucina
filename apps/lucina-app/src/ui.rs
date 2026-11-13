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
use lucina_glass::Color;
use lucina_glass::tokens::{color, radius, space, ty};
use lucina_glass::widgets::switch;

use crate::edit::TextEdit;
use crate::gfx::{Face, Frame, Rect, Rgb, Text};
use crate::image::Image;
use crate::keys::{self, Press};
use crate::theme;

/// The 1px hairline on cards and list rows (docs/design/rules.md): the glass `separator` over the card.
const HAIRLINE: Rgb = theme::HAIRLINE;

// ---- The glass control geometry ----
//
// Restyle step 3: a toolkit control takes its radius, height and padding from the glass kit, and its
// hover/pressed/selected fills from the translucent token the kit paints at that place — as the flat
// pixel that composite makes, because the toolkit has no alpha and no backdrop here (that is step 4).
// `CONTROLS` lists every length that is a `lucina_glass::tokens` radius or space (rules 13 and 14)
// or a public constant of a glass widget, and `MEASURED` the few the kit keeps inside a widget; tests
// read both tables against the kit, so a length of the toolkit's own fails.

/// The smallest tap target (`space::TAP`, rule 13): a button's and a field's height, a list row's
/// floor, and the app bar's height, which is a row of tap-sized controls (`widgets/tab_card.rs`).
const TAP: f32 = space::TAP;
/// `radius::MD` — the button and tile radius of rule 14. The glass button is a rounded square, not a
/// pill: its own radius is the token, not half its height.
const CONTROL_RAD: f32 = radius::MD;
/// `radius::SM`, the field.
const FIELD_RAD: f32 = radius::SM;
/// `radius::XL`, the sheet and the dialog card.
const SHEET_RAD: f32 = radius::XL;
/// `space::L`: the gutter and the card padding (rule 13) — a bar's side margin, a dialog's content
/// margin, a button's padding at the sides.
const EDGE: f32 = space::L;
/// `space::M`, the grid gap: the padding inside a field, and between a control and its label.
const CELL: f32 = space::M;
/// `space::S`: the flow gap, and the gap that follows a bar's mark.
const GAP: f32 = space::S;
/// `space::XS`: how far a list's selection band stands in from the list's edges.
const BAND_INSET: f32 = space::XS;
/// The switch's thumb (`widgets/switch.rs:26`, the kit's private `THUMB`): 22 dp in the 28 dp track,
/// so 3 dp of margin, which is where the thumb sits at each end. A test measures it off the glass
/// canvas.
const SWITCH_THUMB: f32 = 22.0;
/// The 20 dp mark the kit draws in a row and in a field (`widgets/list_row.rs:58`,
/// `widgets/text_field.rs:58`); the app bar's icon is that mark.
const MARK: f32 = 20.0;
/// The keyboard focus ring, as `lucina_glass::widgets::ink::focus_ring` draws it: 3 dp outside the
/// control, 1.6 dp wide, its corners the control's radius grown by the offset. Measured off the
/// glass canvas in `the_focus_ring_is_the_glass_ring`.
const RING_OFF: f32 = 3.0;
const RING_W: f32 = 1.6;
/// The caret, as wide as the ring and as tall as the field's text line
/// (`widgets/text_field.rs:61`, `FieldMetrics::bar`).
const CARET_W: f32 = 1.6;

/// One length a toolkit control takes from the glass kit: the number, and where the kit keeps it.
/// `source` is the token or public constant by name; the test `the_control_lengths_are_glass_tokens`
/// resolves it against `lucina_glass` and fails when the two disagree, or when the name is not one
/// the kit answers to.
#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub control: &'static str,
    pub what: &'static str,
    pub source: &'static str,
    pub dp: f32,
}

#[rustfmt::skip]
pub const CONTROLS: [Control; 18] = [
    Control { control: "button",   what: "height",         source: "space::TAP", dp: TAP },
    Control { control: "button",   what: "radius",         source: "radius::MD", dp: CONTROL_RAD },
    Control { control: "button",   what: "side padding",   source: "space::L",   dp: EDGE },
    Control { control: "field",    what: "height",         source: "space::TAP", dp: TAP },
    Control { control: "field",    what: "radius",         source: "radius::SM", dp: FIELD_RAD },
    Control { control: "field",    what: "text padding",   source: "space::M",   dp: CELL },
    Control { control: "list row", what: "minimum height", source: "space::TAP", dp: TAP },
    Control { control: "list row", what: "band radius",    source: "radius::MD", dp: CONTROL_RAD },
    Control { control: "list row", what: "band inset",     source: "space::XS",  dp: BAND_INSET },
    Control { control: "switch",   what: "track width",    source: "switch::W",  dp: switch::W },
    Control { control: "switch",   what: "track height",   source: "switch::H",  dp: switch::H },
    Control { control: "switch",   what: "label gap",      source: "space::M",   dp: CELL },
    Control { control: "app bar",  what: "height",         source: "space::TAP", dp: TAP },
    Control { control: "app bar",  what: "side padding",   source: "space::L",   dp: EDGE },
    Control { control: "app bar",  what: "gap after mark", source: "space::S",   dp: GAP },
    Control { control: "dialog",   what: "radius",         source: "radius::XL", dp: SHEET_RAD },
    Control { control: "dialog",   what: "content padding", source: "space::L",  dp: EDGE },
    Control { control: "flow",     what: "gap",            source: "space::S",   dp: GAP },
];

/// The lengths the kit keeps in a widget rather than in `tokens`: not tokens, so not in `CONTROLS`,
/// but the kit's numbers all the same. `source` names the widget; the test reads each one from the
/// kit itself — from a public constant or field where the widget has one, off the canvas where the
/// widget draws it — and fails when the toolkit's number is not the kit's, or is not the kit's to
/// take.
#[rustfmt::skip]
pub const MEASURED: [Control; 5] = [
    Control { control: "switch",     what: "thumb",       source: "switch::thumb",      dp: SWITCH_THUMB },
    Control { control: "app bar",    what: "mark",        source: "text_field::mark",   dp: MARK },
    Control { control: "field",      what: "caret width", source: "text_field::caret",  dp: CARET_W },
    Control { control: "focus ring", what: "offset",      source: "focus_ring::offset", dp: RING_OFF },
    Control { control: "focus ring", what: "stroke",      source: "focus_ring::stroke", dp: RING_W },
];

/// `c` at a fraction of its own alpha over `surface`: what the glass kit's translucent fill paints
/// where the toolkit uses it, since the toolkit has no alpha (step 4 gives it the backdrop).
fn tint(c: Color, f: f32, surface: Rgb) -> Rgb {
    crate::prof::count(crate::prof::TOKEN, 1);
    theme::over(theme::frac(c, f), surface)
}

/// The fill of a secondary control: `INK` at 12 % over the card it sits on (`widgets/button.rs:45`).
/// Public: the shell paints the kit's surfaces on its own screens (step 3c) and must not restate the
/// fraction.
pub fn quiet(surface: Rgb) -> Rgb {
    tint(color::INK, 0.12, surface)
}

/// The same control hovered: the kit's brighter glass, the ink at 60/255 (`widgets/action.rs:63`).
fn hovered(surface: Rgb) -> Rgb {
    tint(color::INK, 60.0 / 255.0, surface)
}

/// A secondary control's hairline at rest: `separator` over the fill it rings (`widgets/button.rs:46`).
pub fn rim(surface: Rgb) -> Rgb {
    crate::prof::count(crate::prof::TOKEN, 1);
    theme::over(color::HAIR, surface)
}

/// A field's ground: `INK` at 9 % over the card (`widgets/text_field.rs:222`).
fn field_ground(surface: Rgb) -> Rgb {
    tint(color::INK, 0.09, surface)
}

/// A field's outline: `INK` at 16 % over its own ground (`widgets/text_field.rs:227`).
fn field_edge(surface: Rgb) -> Rgb {
    tint(color::INK, 0.16, surface)
}

/// The selected list row: `accentSoft` over the card (`widgets/list_row.rs:52`). Public, for the same
/// reason as `quiet`: a selected row of the shell's is the kit's selected row.
pub fn row_selected(surface: Rgb) -> Rgb {
    crate::prof::count(crate::prof::TOKEN, 1);
    theme::over(color::ACCENT_SOFT, surface)
}

/// The switch's track at rest: `INK` at 16 % over the card (`widgets/switch.rs:83`).
fn track_off(surface: Rgb) -> Rgb {
    tint(color::INK, 0.16, surface)
}

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

/// One role of the toolkit's type scale. Nothing in it is the toolkit's own: `token` names the
/// `lucina_glass::tokens::ty` constant the size is taken from, `face` is the face that token is
/// drawn in ([`Face::glass`]), and `rgb` is a constant of [`crate::theme`], which step 1 took from
/// `tokens`. The test `the_scale_is_all_glass_tokens` reads this table in code.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    pub name: &'static str,
    /// The `tokens::ty` constant by name: `"LEAD"`, `"BODY"`, `"HEADLINE"`.
    pub token: &'static str,
    /// Its em size, read from the token itself, never copied beside it.
    pub size: f32,
    pub face: Face,
    /// The glass colour token `rgb` comes from, or `"caller"` where the app picks the colour
    /// (a button's label, a field's text).
    pub colour: &'static str,
    pub rgb: Rgb,
}

impl Spec {
    /// The role's size on this screen, in device pixels.
    pub fn pt(&self, t: Theme) -> u16 {
        t.pt(self.size)
    }
}

/// Which of the toolkit's own text roles a piece of type is. An app that names its own size in a
/// `ui.text_in` call is outside this table: those are the apps' own numbers, reported rather than
/// clamped here, because clamping would hide the edit an app still needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextRole {
    Label,
    Dim,
    Heading,
    Paragraph,
    /// The app's name in the header bar.
    HeaderTitle,
    /// The "F1 Home" hint and the camera mark beside it.
    HeaderHint,
    /// A button's or a slider's label; the colour is the caller's.
    Control,
    /// What is typed in a field; the colour is the caller's.
    Field,
}

/// The roles, in the order of [`TextRole`]. Each size is the glass token named beside it; a test
/// reads this table and fails when the two disagree.
#[rustfmt::skip]
pub const SCALE: [Spec; 8] = [
    Spec { name: "label", token: "LEAD", size: ty::LEAD.0,
           face: Face::Sans, colour: "INK", rgb: theme::ITEM },
    Spec { name: "dim", token: "BODY", size: ty::BODY.0,
           face: Face::Sans, colour: "INK_2", rgb: theme::DIM },
    Spec { name: "heading", token: "HEADLINE", size: ty::HEADLINE.0,
           face: Face::Bold, colour: "INK", rgb: theme::ITEM },
    Spec { name: "paragraph", token: "LEAD", size: ty::LEAD.0,
           face: Face::Sans, colour: "INK", rgb: theme::ITEM },
    Spec { name: "header-title", token: "LEAD", size: ty::LEAD.0,
           face: Face::Bold, colour: "INK", rgb: theme::WHITE },
    Spec { name: "header-hint", token: "BODY", size: ty::BODY.0,
           face: Face::Sans, colour: "INK_2", rgb: theme::DIM },
    Spec { name: "control", token: "LEAD", size: ty::LEAD.0,
           face: Face::Sans, colour: "caller", rgb: theme::WHITE },
    Spec { name: "field", token: "LEAD", size: ty::LEAD.0,
           face: Face::Sans, colour: "caller", rgb: theme::WHITE },
];

impl TextRole {
    pub fn spec(self) -> &'static Spec {
        &SCALE[self as usize]
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
            gap: theme.px(GAP),
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
        let sp = TextRole::Label.spec();
        let r = self.line(sp.face, sp.pt(self.theme), sp.rgb, s);
        self.ax_text(Role::Label, "label", r, s, true);
    }

    /// A line of secondary text.
    pub fn dim(&mut self, s: &str) {
        let sp = TextRole::Dim.spec();
        let r = self.line(sp.face, sp.pt(self.theme), sp.rgb, s);
        self.ax_text(Role::Label, "label", r, s, true);
    }

    /// A section's title.
    pub fn heading(&mut self, s: &str) {
        let sp = TextRole::Heading.spec();
        let r = self.line(sp.face, sp.pt(self.theme), sp.rgb, s);
        self.ax_text(Role::Heading, "heading", r, s, false);
    }

    /// Text wrapped to the area's width, in the theme's body size.
    pub fn paragraph(&mut self, s: &str) {
        let sp = TextRole::Paragraph.spec();
        self.wrapped(sp.face, sp.pt(self.theme), sp.rgb, s);
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
    ///
    /// Restyle step 3: the bar is a row of tap-sized controls, so it is `space::TAP` tall; the 2 px
    /// accent band it used to carry is the hairline the kit rules its surfaces with, since the accent
    /// is for what is selected or active (rules 10 and 13) and a bar title is neither.
    pub fn header(&mut self, title: &str) {
        let r = self.cut_top(self.px(TAP));
        self.ax_text(Role::Heading, "header", r, title, false);
        let t = self.theme;
        self.fill(r, t.bar);
        let hair = self.px(1.0).max(1);
        let rule = Rect::new(r.x, r.bottom() - hair, r.w, hair);
        self.fill(rule, HAIRLINE);
        let pad = t.px(EDGE);
        // The app's own icon, when the shell named one: the kit's 20 dp mark.
        let mut left = r.x + pad;
        if let Some(id) = crate::icons::own_name() {
            let side = t.px(MARK).min(r.h - 4);
            let at = Rect::new(left, r.y + (r.h - 2 - side) / 2, side, side);
            self.icon(at, id, "");
            left += side + t.px(GAP);
        }
        let (hh, ht) = (TextRole::HeaderHint.spec(), TextRole::HeaderTitle.spec());
        let hint = "F1 Home";
        let hint_w = self.measure(hh.face, hh.pt(t), hint);
        // The "camera on" mark, when something captures: a red disc and who, at the right end of the
        // bar, left of the hint. Its width is taken off the title's area so the two never overlap.
        let camera = crate::camera_mark::current();
        let mut cam_w = 0;
        let mut cam_text = String::new();
        let mut cam_cw = 0;
        if let Some((_, program)) = &camera {
            cam_text = crate::camera_mark::label(program);
            cam_cw = self.measure(hh.face, hh.pt(t), &cam_text);
            cam_w = t.px(10.0) + t.px(GAP) + cam_cw + t.px(GAP);
        }
        self.text_in_raw(
            Rect::new(left, r.y, r.right() - 2 * pad - hint_w - cam_w - left, r.h),
            ht.face,
            ht.pt(t),
            ht.rgb,
            title,
        );
        self.text_in_raw(
            Rect::new(r.right() - pad - hint_w, r.y, hint_w, r.h),
            hh.face,
            hh.pt(t),
            hh.rgb,
            hint,
        );
        if camera.is_some() {
            let gap = t.px(GAP);
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
                hh.face,
                hh.pt(t),
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

    /// The glass focus ring (`widgets::ink::focus_ring`): `RING_W` of accent `RING_OFF` outside the
    /// control, its corners the control's radius grown by the offset. The widget is drawn over it.
    fn focus_ring(&mut self, r: Rect, rad: i32, under: Rgb) {
        let t = self.theme;
        let off = t.px(RING_OFF).max(1);
        let w = t.px(RING_W).max(1);
        let ring = Rect::new(r.x - off, r.y - off, r.w + 2 * off, r.h + 2 * off);
        self.frame
            .bordered_round_rect(ring, rad + off, w, t.accent, under);
    }

    /// The glass button (`widgets/button.rs`): corners of `radius::MD`, the label centred in its own
    /// Semibold type. The primary one is the kit's paper button, `PAPER` with `PAPER_INK` on it; the
    /// quiet one is the ink at 12 % under a hairline, the kit's brighter glass under the pointer and
    /// `accentFill` while pressed. The accent stays for what is selected or active (rule 10).
    fn draw_button(&mut self, r: Rect, label: &str, i: Interaction, look: Look) {
        if !self.painting {
            return;
        }
        let t = self.theme;
        let rad = t.px(CONTROL_RAD);
        let (fill, ink) = match look {
            Look::Primary => (theme::PAPER, theme::PAPER_INK),
            Look::Danger => (t.danger, t.text),
            Look::Plain if i.pressed => (t.selection, t.text),
            Look::Plain if i.hovered => (hovered(t.panel), t.text),
            Look::Plain => (quiet(t.panel), t.text),
        };
        if i.focused {
            self.focus_ring(r, rad, fill);
        }
        match look {
            Look::Plain => {
                let hair = t.px(1.0).max(1);
                self.frame
                    .bordered_round_rect(r, rad, hair, rim(fill), fill);
            }
            _ => self.frame.fill_round_rect(r, rad, fill),
        }
        let cs = TextRole::Control.spec();
        let px = cs.pt(t);
        let w = self.text.measure(cs.face, px, label);
        let inner = Rect::new(
            r.x + (r.w - w).max(t.px(GAP)) / 2,
            r.y,
            r.w - t.px(GAP),
            r.h,
        );
        self.text_in_raw(inner, cs.face, px, ink, label);
    }

    /// A button is a tap target tall (`space::TAP`, rule 13) with the card padding at its sides.
    fn button_size(&mut self, label: &str) -> (i32, i32) {
        let t = self.theme;
        let cs = TextRole::Control.spec();
        let w = self.measure(cs.face, cs.pt(t), label) + 2 * t.px(EDGE);
        (w, t.px(TAP))
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

    /// A switch with a label; clicking or Space flips `on`. True when it changed.
    ///
    /// Restyle step 3: the toolkit's box-and-tick is the glass kit's switch
    /// (`widgets/switch.rs`): a pill track of `switch::W` by `switch::H`, the thumb 22 dp inside it
    /// with 3 dp of margin, the accent when on and `INK` at 16 % at rest, `INK` for the thumb. On is
    /// the accent because on is active (rule 10). The row stays a tap target tall, so the hit area
    /// and the ax node are what they were.
    pub fn checkbox(&mut self, label: &str, on: &mut bool) -> bool {
        let t = self.theme;
        let cs = TextRole::Control.spec();
        let px = cs.pt(t);
        let track_w = t.px(switch::W);
        let gap = t.px(CELL);
        let w = track_w + gap + self.measure(cs.face, px, label);
        let h = t.px(TAP);
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
            let track_h = t.px(switch::H);
            let b = Rect::new(r.x, r.y + (r.h - track_h) / 2, track_w, track_h);
            let rad = b.h / 2;
            let thumb = t.px(SWITCH_THUMB);
            let pad = (b.h - thumb) / 2;
            let fill = if *on { t.accent } else { track_off(t.panel) };
            if i.focused {
                self.focus_ring(b, rad, fill);
            }
            if *on {
                self.frame.fill_round_rect(b, rad, fill);
            } else {
                self.frame
                    .bordered_round_rect(b, rad, t.px(1.0).max(1), rim(fill), fill);
            }
            // The thumb's travel is the kit's `browser_rules::thumb_ends`: PAD from each end.
            let cx = if *on {
                b.right() - pad - thumb / 2
            } else {
                b.x + pad + thumb / 2
            };
            self.frame.fill_disc(
                cx as f32 + 0.5,
                (b.y + b.h / 2) as f32 + 0.5,
                thumb as f32 / 2.0,
                t.text,
            );
            let tr = Rect::new(b.right() + gap, r.y, r.w - track_w - gap, r.h);
            self.text_in_raw(tr, cs.face, px, t.text, label);
        }
        i.clicked
    }

    /// A one-line text field in the flow, as wide as the area. Give the field an id of your own.
    /// The field is a tap target tall (`space::TAP`).
    pub fn text_field(&mut self, id: impl Into<Id>, e: &mut TextEdit) -> TextResponse {
        let h = self.px(TAP);
        let w = self.area.w;
        let r = self.next(w, h);
        self.text_field_at(r, id, e)
    }

    /// A multi-line text box, `rows` lines tall, in the flow.
    pub fn text_area(&mut self, id: impl Into<Id>, e: &mut TextEdit, rows: i32) -> TextResponse {
        let fs = TextRole::Field.spec();
        let px = fs.pt(self.theme);
        let lh = self.line_height(fs.face, px);
        let h = lh * rows + 2 * self.px(CELL);
        let w = self.area.w;
        let r = self.next(w, h);
        e.multiline = true;
        self.text_field_at(r, id, e)
    }

    /// A text field in a rectangle of your own.
    pub fn text_field_at(&mut self, r: Rect, id: impl Into<Id>, e: &mut TextEdit) -> TextResponse {
        let id = id.into();
        let t = self.theme;
        let fs = TextRole::Field.spec();
        let px = fs.pt(t);
        let face = fs.face;
        let pad = t.px(CELL);
        let inner = if e.multiline {
            r.inset(t.px(CELL))
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
            self.text.wrap(face, px, &shown.text, inner.w)
        } else {
            std::iter::once(0..shown.text.len()).collect()
        };
        let lh = self.line_height(face, px);
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
                face,
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
                                face,
                                px,
                                &shown.text[lines[line].start..lines[line].start + cur],
                            )
                        });
                        let at = self
                            .text
                            .offset_at(face, px, &shown.text[lines[n].clone()], goal);
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
            self.text.wrap(face, px, &shown.text, inner.w)
        } else {
            std::iter::once(0..shown.text.len()).collect()
        };
        let cursor = shown.shown(e.cursor);
        let cur_line = shown.line_of(e.cursor, &lines);
        let cur_x = self
            .text
            .measure(face, px, &shown.text[lines[cur_line].start..cursor]);
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
            // The glass field (`widgets/text_field.rs`): `radius::SM`, the card's ink at 9 % as its
            // ground and at 16 % as its outline, the accent caret `CARET_W` wide.
            let rad = t.px(FIELD_RAD);
            let ground = field_ground(t.panel);
            if focused {
                self.focus_ring(r, rad, ground);
            }
            self.frame
                .bordered_round_rect(r, rad, t.px(1.0).max(1), field_edge(ground), ground);
            let asc = self.text.ascent(face, px);
            let desc = self.text.descent(face, px);
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
                self.text_at_raw(inner.x, base, face, px, t.dim, &ph);
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
                    let xa = self.text.measure(face, px, &shown.text[l.start..a]);
                    let xb = self.text.measure(face, px, &shown.text[l.start..b]);
                    let extra = if sel.end > l.end { t.px(space::XS) } else { 0 };
                    self.fill(Rect::new(x + xa, top, xb - xa + extra, lh), t.selection);
                }
                let line = shown.text[l.clone()].trim_end_matches('\n');
                self.text_at_raw(x, top + asc, face, px, t.text, line);
                if focused && i == cur_line {
                    let w = t.px(CARET_W).max(1);
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
        let h = self.px(row_h).max(1).max(self.px(TAP));
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
                // The glass row (`widgets/list_row.rs:52`): the selected row is `accentSoft` over the
                // card, a hovered one the raised card, both a tile of `radius::MD` standing
                // `space::XS` in from the list's edges.
                let inset = t.px(BAND_INSET);
                let rad = t.px(CONTROL_RAD);
                let row_r = Rect::new(rect.x + inset, rect.y, (rect.w - 2 * inset).max(1), rect.h);
                if selected {
                    self.frame
                        .fill_round_rect(row_r, rad, row_selected(t.panel));
                } else if hover_row == Some(i) {
                    self.frame.fill_round_rect(row_r, rad, t.panel_hi);
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
    ///
    /// Restyle step 3: the card is the kit's sheet, `radius::XL` with a hairline and `space::L` of
    /// padding (rule 14). The kit draws it as thick material over a blurred page; that is the
    /// compositor's work (step 4), so it stays the flat card token with the scrim it has always had.
    pub fn modal<R>(&mut self, w: f32, h: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
        let t = self.theme;
        let screen = self.frame.bounds();
        let (w, h) = (t.px(w).min(screen.w), t.px(h).min(screen.h));
        let r = Rect::new((screen.w - w) / 2, (screen.h - h) / 2, w, h);
        if self.painting {
            self.frame.set_clip(screen);
            self.frame.dim(0x80);
            self.frame
                .bordered_round_rect(r, t.px(SHEET_RAD), t.px(1.0).max(1), HAIRLINE, t.panel);
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
        let out = self.within(r.inset(t.px(EDGE)), f);
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

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_glass::text::Font;
    use lucina_glass::tokens::{color, ty};

    /// The `ty` constant a role names, taken from glass rather than written beside it.
    fn token(name: &str) -> Option<(f32, f32)> {
        Some(match name {
            "DISPLAY" => ty::DISPLAY,
            "NUMERAL" => ty::NUMERAL,
            "TITLE" => ty::TITLE,
            "TITLE2" => ty::TITLE2,
            "HEADLINE" => ty::HEADLINE,
            "LEAD" => ty::LEAD,
            "BODY" => ty::BODY,
            "CAPTION" => ty::CAPTION,
            "SMALL" => ty::SMALL,
            "MONO" => ty::MONO,
            "MICRO" => ty::MICRO,
            _ => return None,
        })
    }

    /// A colour the palette is made of: a token, or a translucent token composited the way
    /// `theme.rs` composites it. Step 1 proved those constants are tokens; a role may not go beside
    /// them with a number of its own.
    fn is_a_token_colour(c: Rgb) -> bool {
        [
            theme::WHITE,
            theme::ITEM,
            theme::DIM,
            theme::ACCENT,
            theme::SELECTION,
            theme::DANGER,
            theme::GREEN,
            theme::PANEL,
            theme::PANEL_HI,
            theme::BAR,
            theme::HAIRLINE,
            theme::GROUND,
        ]
        .contains(&c)
    }

    /// Nothing in the toolkit's type scale is the toolkit's own size, face or colour. Written to
    /// fail if one is invented: `size: 14.0` under a token name, a face that draws in a file the
    /// glass kit does not embed, a grey that is not `INK_2`.
    #[test]
    fn the_scale_is_all_glass_tokens() {
        for sp in SCALE.iter() {
            let t = token(sp.token)
                .unwrap_or_else(|| panic!("{}: no ty token named {}", sp.name, sp.token));
            assert_eq!(
                sp.size, t.0,
                "{}: size {} is not the {} token ({})",
                sp.name, sp.size, sp.token, t.0
            );
            // The face is one the glass kit embeds, under the name glass gives it.
            let file = match sp.face.glass() {
                Font::Regular => "Lato-Regular.ttf",
                Font::Semibold => "Lato-Semibold.ttf",
                Font::Mono => "DejaVuSansMono.ttf",
                other => {
                    panic!("{}: a role is not drawn in {other:?}", sp.name)
                }
            };
            assert_eq!(
                sp.face.file(),
                file,
                "{}: {} does not draw in the glass face {:?}",
                sp.name,
                sp.face.file(),
                sp.face.glass()
            );
            assert!(
                is_a_token_colour(sp.rgb),
                "{}: colour {:#08x} is not a palette token",
                sp.name,
                sp.rgb.0
            );
        }
        // The kit has one secondary text colour, and it belongs to metadata alone: a content role
        // that lands on it is the step 3 mistake the owner saw in Notes.
        assert_eq!(
            TextRole::Dim.spec().rgb,
            theme::DIM,
            "INK_2 is the kit's only secondary text colour"
        );
        // And the colour names in the table are the tokens they say.
        assert_eq!(rgb_of("INK"), theme::WHITE);
        assert_eq!(rgb_of("INK_2"), theme::DIM);
    }

    /// The roles that carry what the owner reads — body, list titles, navigation entries, headings —
    /// resolve to the primary text token. Written to fail if one is moved onto the metadata shade,
    /// which is what dimmed the note bodies and the sidebar in step 3.
    #[test]
    fn content_roles_are_the_primary_token() {
        let metadata = rgb_of("INK_2");
        let primary = rgb_of("INK");
        // `theme::ITEM` is the name 115 app call sites use for a list title or a navigation entry;
        // it is the primary token, not the shade, and `Theme` hands it out as `item`.
        assert_eq!(
            theme::ITEM,
            primary,
            "ITEM is content, so it is the primary token"
        );
        let th = theme1();
        assert_eq!(th.item, primary, "Theme::item is content text");
        assert_eq!(th.dim, metadata, "Theme::dim is metadata");
        for sp in SCALE.iter() {
            let content = matches!(
                sp.name,
                "label" | "paragraph" | "heading" | "header-title" | "control" | "field"
            );
            if content {
                assert_ne!(
                    sp.rgb, metadata,
                    "{}: content text is not the metadata shade",
                    sp.name
                );
            }
            if matches!(sp.name, "label" | "paragraph" | "heading") {
                assert_eq!(
                    sp.rgb, primary,
                    "{}: a body-text role must resolve to the primary token",
                    sp.name
                );
                assert_eq!(sp.colour, "INK", "{}: names the primary token", sp.name);
            }
            if matches!(sp.name, "dim" | "header-hint") {
                assert_eq!(
                    sp.rgb, metadata,
                    "{}: metadata is the kit's secondary token",
                    sp.name
                );
            }
        }
    }

    /// WCAG 2 relative luminance of an opaque pixel.
    fn luma(c: Rgb) -> f64 {
        let lin = |v: u32| {
            let v = (v & 0xff) as f64 / 255.0;
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.0 >> 16) + 0.7152 * lin(c.0 >> 8) + 0.0722 * lin(c.0)
    }

    /// The WCAG 2 contrast ratio of two pixels.
    fn ratio(a: Rgb, b: Rgb) -> f64 {
        let (x, y) = (luma(a), luma(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// Every surface the toolkit paints text on, named as the glass kit names it.
    fn surfaces() -> [(&'static str, Rgb); 8] {
        [
            ("page ground", theme::GROUND),
            ("card", theme::PANEL),
            ("raised card", theme::PANEL_HI),
            ("bar", theme::BAR),
            ("selection fill", theme::SELECTION),
            ("accent", theme::ACCENT),
            ("text field", field_ground(theme::PANEL)),
            ("selected row", row_selected(theme::PANEL)),
        ]
    }

    /// The surfaces each role is drawn on, from the code that draws it: a role is not judged against
    /// a surface it never sits on. `accent` carries no text anywhere in the toolkit (ring, switch
    /// track, caret, scrollbar thumb), so no role lists it; `selection fill` is under a pressed
    /// button's label and a multiline field's selection, both content.
    const TEXT_ON: [(&str, &[&str]); 6] = [
        (
            "label",
            &[
                "page ground",
                "card",
                "raised card",
                "bar",
                "selection fill",
                "text field",
                "selected row",
            ],
        ),
        ("paragraph", &["page ground", "card", "raised card", "bar"]),
        ("heading", &["page ground", "card", "raised card", "bar"]),
        (
            "dim",
            &[
                "page ground",
                "card",
                "raised card",
                "bar",
                "text field",
                "selected row",
            ],
        ),
        ("header-title", &["bar"]),
        ("header-hint", &["bar"]),
    ];

    /// A pair the kit's own tokens do not carry to the floor, recorded with the number it measures.
    /// This is not a lowered threshold: the test fails if the number moves, and the pair stays a
    /// finding for the coordinator, because the only fix is a glass token, not a toolkit colour.
    /// All three are `textSecondary` — INK_2 at 60 % — on a surface lighter than `card`.
    const FINDINGS: [(&str, &str, f64); 3] = [
        ("dim", "raised card", 4.22),
        ("dim", "text field", 3.99),
        ("dim", "selected row", 3.90),
    ];

    /// Body text 4.5:1 and large text 3.0:1 (WCAG 2 AA) for every role against every surface it is
    /// drawn on. `HEADLINE` is the only large size in the scale.
    #[test]
    fn text_contrast_meets_wcag_on_every_surface() {
        let mut table = String::new();
        let mut under: Vec<String> = Vec::new();
        for sp in SCALE.iter() {
            if sp.colour == "caller" {
                continue;
            }
            let need = if sp.token == "HEADLINE" { 3.0 } else { 4.5 };
            let on = TEXT_ON
                .iter()
                .find(|(name, _)| *name == sp.name)
                .unwrap_or_else(|| panic!("{}: no surface list, so no contrast proof", sp.name))
                .1;
            for (surface, back) in surfaces() {
                if !on.contains(&surface) {
                    continue;
                }
                let r = ratio(sp.rgb, back);
                table.push_str(&format!(
                    "{:>12} on {:<13} {:>5.2}:1  need {need:.1}\n",
                    sp.name, surface, r
                ));
                match FINDINGS
                    .iter()
                    .find(|(n, s, _)| *n == sp.name && *s == surface)
                {
                    Some((_, _, at)) => assert!(
                        (r - at).abs() < 0.02,
                        "{} on {surface}: recorded as {at:.2}:1, now {r:.2}:1",
                        sp.name
                    ),
                    None if r < need => under.push(format!(
                        "{} on {surface}: {r:.2}:1 under {need:.1}",
                        sp.name
                    )),
                    None => {}
                }
            }
        }
        // The primary button's own pair, which the caller picks rather than the scale.
        let paper = ratio(theme::PAPER_INK, theme::PAPER);
        table.push_str(&format!("paper button ink on paper {paper:.2}:1\n"));
        print!("{table}");
        assert!(under.is_empty(), "under WCAG AA:\n{}\n", under.join("\n"));
    }

    fn rgb_of(name: &str) -> Rgb {
        match name {
            "INK" => Rgb::hex(color::INK.xrgb()),
            "INK_2" => theme::DIM,
            other => panic!("{other} is not a colour token this table uses"),
        }
    }

    /// `TextRole::spec` indexes `SCALE` by the enum's order, so the two have to agree.
    #[test]
    fn the_scale_is_in_the_order_of_textrole() {
        let names: Vec<&str> = SCALE.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            vec![
                "label",
                "dim",
                "heading",
                "paragraph",
                "header-title",
                "header-hint",
                "control",
                "field"
            ],
            "SCALE must list every TextRole, in its order"
        );
        for (i, sp) in names.iter().enumerate() {
            assert_eq!(SCALE[i].name, *sp);
        }
        assert_eq!(TextRole::Field as usize, SCALE.len() - 1);
    }

    // ---- Restyle step 3: the toolkit's controls are the glass controls ----

    use lucina_glass::Paint;
    use lucina_glass::canvas::{Canvas, R};
    use lucina_glass::widgets::button::PushButton;
    use lucina_glass::widgets::ink::{Palette, focus_ring};
    use lucina_glass::widgets::list_row::ListRow;
    use lucina_glass::widgets::switch::Switch;
    use lucina_glass::widgets::text_field::FieldMetrics;

    /// The theme of these tests: `k` 1.0, so one of the kit's dp and one device pixel are the same
    /// number and a measurement says what the kit's number is.
    fn theme1() -> Theme {
        Theme {
            k: 1.0,
            ..Theme::new(1024, 768)
        }
    }

    /// One painting pass of `draw` on a screen filled with `ground`, the pointer at `at` and held
    /// down there when `press`. The pixels come back with the theme they were drawn with.
    fn pass(
        w: u32,
        h: u32,
        ground: Rgb,
        at: Option<(i32, i32)>,
        press: bool,
        draw: impl FnOnce(&mut Ui),
    ) -> (Frame, Theme) {
        let t = theme1();
        let mut frame = Frame::new(w, h);
        let mut text = Text::new();
        let mut st = UiState {
            pointer: at.unwrap_or((-9, -9)),
            ..UiState::default()
        };
        let input = match (at, press) {
            (Some((x, y)), true) => Some(Input::Down(x, y)),
            (Some((x, y)), false) => Some(Input::Move(x, y)),
            (None, _) => None,
        };
        st.begin_pass();
        {
            let mut ui = Ui::new(&mut frame, &mut text, &mut st, t, input, true);
            let all = ui.area();
            ui.fill(all, ground);
            draw(&mut ui);
        }
        (frame, t)
    }

    /// The rectangle of the pixels that are exactly `c` within `area`, or nothing when the screen
    /// holds none: the shape drawn in that colour.
    fn inked_in(fr: &Frame, c: Rgb, area: Rect) -> Option<Rect> {
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if fr.get(x, y) == Some(c) {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (x1 >= x0).then(|| Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1))
    }

    fn inked(fr: &Frame, c: Rgb) -> Option<Rect> {
        let b = fr.bounds();
        inked_in(fr, c, Rect::new(0, 0, b.w, b.h))
    }

    /// Where the shape of `c` reaches along the row `y`, from the first pixel that is at least half
    /// `c` to the last: the edge as the rasteriser put it, which an exact-colour reading loses the
    /// last pixel or two of wherever the boundary falls between columns.
    fn hspan(fr: &Frame, surface: Rgb, c: Rgb, y: i32) -> (i32, i32) {
        let b = fr.bounds();
        run(
            |m| ink_of(fr.get(m, y).unwrap_or(surface), surface, c) >= 0.5,
            b.w,
        )
        .expect("no ink on that row")
    }

    fn vspan(fr: &Frame, surface: Rgb, c: Rgb, x: i32) -> (i32, i32) {
        let b = fr.bounds();
        run(
            |m| ink_of(fr.get(x, m).unwrap_or(surface), surface, c) >= 0.5,
            b.h,
        )
        .expect("no ink in that column")
    }

    /// The first and last position of the run where `yes` holds, out of `0..n`.
    fn run(yes: impl Fn(i32) -> bool, n: i32) -> Option<(i32, i32)> {
        let hits: Vec<i32> = (0..n).filter(|&m| yes(m)).collect();
        Some((*hits.first()?, *hits.last()?))
    }

    /// A rectangle's middle row, where its fill is away from every corner and every edge.
    fn mid(r: Rect) -> i32 {
        r.y + r.h / 2
    }

    /// The shape of pixels exactly `c` that `seed` lies in, by filling out from it. A colour's
    /// global bounding box is not the control's: anti-aliasing a glyph paints every step between
    /// the ink and the surface it sits on, and one of those steps answers to the control's own
    /// colour somewhere the control is not. What is joined to the seed is what was drawn.
    fn blob(fr: &Frame, c: Rgb, seed: (i32, i32)) -> Rect {
        assert_eq!(
            fr.get(seed.0, seed.1),
            Some(c),
            "the seed of the shape is a pixel of that colour"
        );
        let b = fr.bounds();
        let at = |x: i32, y: i32| (y * b.w + x) as usize;
        let mut seen = vec![false; (b.w * b.h) as usize];
        let mut stack = vec![seed];
        let (mut x0, mut y0, mut x1, mut y1) = (seed.0, seed.1, seed.0, seed.1);
        while let Some((x, y)) = stack.pop() {
            if x < 0
                || y < 0
                || x >= b.w
                || y >= b.h
                || std::mem::replace(&mut seen[at(x, y)], true)
            {
                continue;
            }
            if fr.get(x, y) != Some(c) {
                continue;
            }
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
            stack.extend([(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)]);
        }
        Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    }

    /// How far in the ink of the colour `c` lies two rows below the top of a shape drawn on `r`, past
    /// the edge of its body. An arc meets its tangent slowly, so no reading off pixels is the radius
    /// itself: the last rows of a corner of R dp come in by a fraction of a pixel, and a semicircle is
    /// not a corner at all. So this says nothing on its own and a great deal against [`kit_depth`] or
    /// [`glass_ring`], which take the same reading off the same shape the kit's own canvas paints at the
    /// token radius, at the same size. The toolkit drew the kit's corner, or the two differ.
    fn corner(fr: &Frame, surface: Rgb, c: Rgb, r: Rect) -> i32 {
        depth(|x, y| fr.get(x, y).unwrap_or(surface), surface, c, r)
    }

    fn depth(get: impl Fn(i32, i32) -> Rgb, surface: Rgb, c: Rgb, r: Rect) -> i32 {
        let half = |x: i32, y: i32| ink_of(get(x, y), surface, c) >= 0.5;
        let first = |y: i32| ((r.x - 4)..(r.right() + 4)).find(|&x| half(x, y));
        let body = first(r.y + r.h / 2).expect("the body of the shape is ink");
        let top = ((r.y - 4)..r.bottom())
            .find(|&y| first(y).is_some())
            .expect("no ink in that area");
        first(top + 2).unwrap_or(body) - body
    }

    /// The depth a rounded rectangle of `rad` dp comes to, on the kit's own canvas: `Canvas::fill_rrect`
    /// at the size `r` is, which is how every glass widget draws one. Anti-aliasing costs a reading of
    /// an arc some of its radius, and a semicircle is not a corner at all, so a toolkit control's
    /// [`corner`] answers to this and not to a radius spelled out.
    fn kit_depth(rad: i32, w: i32, h: i32, surface: Rgb) -> i32 {
        let cv = glass((w + 24) as u32, (h + 24) as u32, surface, |p| {
            p.cv.fill_rrect(
                R::new(12.0, 12.0, w as f32, h as f32),
                rad as f32,
                Color::hex(theme::WHITE.0),
            );
        });
        depth(
            |x, y| Rgb::hex(cv.get(x + 12, y + 12)),
            surface,
            theme::WHITE,
            Rect::new(0, 0, w, h),
        )
    }

    fn near(got: i32, want: i32, by: i32, what: &str) {
        assert!(
            (got - want).abs() <= by,
            "{what}: {got} where the kit has {want}, {} off",
            (got - want).abs()
        );
    }

    /// The pixel a glass canvas paints of `token` over `under`: the kit's own composite.
    fn glass_paints(under: Rgb, token: Color) -> Rgb {
        let mut cv = Canvas::new(8, 8, 1.0);
        cv.clear(Color::hex(under.0));
        cv.fill_rrect(R::new(0.0, 0.0, 8.0, 8.0), 0.0, token);
        Rgb::hex(cv.get(4, 4))
    }

    /// A glass canvas of `w` x `h` on `under`, with the kit's own widget drawing on it.
    fn glass(w: u32, h: u32, under: Rgb, what: impl FnOnce(&mut Paint)) -> Canvas {
        let mut cv = Canvas::new(w, h, 1.0);
        cv.clear(Color::hex(under.0));
        let mut ty = lucina_glass::text::Type::new();
        {
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            what(&mut p);
        }
        cv
    }

    /// How much of the colour `c` a pixel is, where `under` is the surface it sits on: nothing of it
    /// on the bare surface, all of it in the colour itself, and what anti-aliasing shows between.
    fn ink_of(px: Rgb, under: Rgb, c: Rgb) -> f32 {
        let d = |a: Rgb, b: Rgb| {
            let ch = |v: Rgb, s: u32| ((v.0 >> s) & 0xff) as i32;
            (ch(a, 16) - ch(b, 16)).abs()
                + (ch(a, 8) - ch(b, 8)).abs()
                + (ch(a, 0) - ch(b, 0)).abs()
        };
        (d(px, under) as f32 / d(c, under).max(1) as f32).clamp(0.0, 1.0)
    }

    /// The kit's `CONTROLS` source, resolved where the number lives.
    fn glass_length(name: &str) -> Option<f32> {
        Some(match name {
            "space::XS" => space::XS,
            "space::S" => space::S,
            "space::M" => space::M,
            "space::L" => space::L,
            "space::TAP" => space::TAP,
            "radius::SM" => radius::SM,
            "radius::MD" => radius::MD,
            "radius::XL" => radius::XL,
            "switch::W" => switch::W,
            "switch::H" => switch::H,
            _ => return None,
        })
    }

    /// The kit's `MEASURED` source and how near the toolkit's copy has to be: `0.0` where the kit
    /// keeps the number where it can be read, one dp where it has to be measured off what the
    /// widget draws, because that is all a canvas of pixels can tell.
    fn glass_measured(name: &str) -> Option<(f32, f32)> {
        let m = FieldMetrics::bar(40.0, ty::LEAD);
        Some(match name {
            "text_field::mark" => (m.mark, 0.0),
            "text_field::caret" => (m.caret_w, 0.0),
            "switch::thumb" => (glass_thumb(), 1.0),
            "focus_ring::offset" => (glass_ring(radius::MD, 80, 40).0, 1.0),
            "focus_ring::stroke" => (glass_ring(radius::MD, 80, 40).1, 1.0),
            _ => return None,
        })
    }

    /// The thumb's diameter in dp. `Switch` keeps it to itself, so it is measured: along the track's
    /// middle row, the pixels nearer the kit's INK than the track's own fill are the disc, and its
    /// anti-aliased ends join it from halfway over, which is where a rasteriser puts the edge.
    fn glass_thumb() -> f32 {
        let cv = glass(120, 60, theme::PANEL, |p| {
            let mut s = Switch::new(false);
            s.draw(p, 90.0, 30.0);
        });
        let ink = color::INK.xrgb();
        // A stretch of the track away from the disc: what the thumb is not.
        let track = cv.get(82, 30);
        let apart = |a: u32, b: u32| -> i32 {
            let ch = |v: u32, s: u32| ((v >> s) & 0xff) as i32;
            (ch(a, 16) - ch(b, 16)).abs()
                + (ch(a, 8) - ch(b, 8)).abs()
                + (ch(a, 0) - ch(b, 0)).abs()
        };
        let xs: Vec<i32> = (0..120)
            .filter(|&x| {
                let v = cv.get(x, 30);
                apart(v, ink) < apart(v, track)
            })
            .collect();
        let (first, last) = (
            *xs.first().expect("the switch drew no thumb"),
            *xs.last().expect("the switch drew no thumb"),
        );
        (last - first + 1) as f32
    }

    /// What the kit's `focus_ring` paints on a bare canvas around a control of rect `r` at radius
    /// `rad`: how far outside the control its ink reaches, how thick the band is, and how deep its
    /// corner comes in - all read off the pixels the way the toolkit's screen is read. The kit
    /// strokes a path inset by three dp with a band of 1.6, centred on that path; a rasteriser
    /// spreads the ink over the pixels it covers, so the edge is where the ink is half over the
    /// surface.
    fn glass_ring(rad: f32, w: i32, h: i32) -> (f32, f32, i32) {
        let under = theme::PANEL;
        let cv = glass((w + 80) as u32, (h + 80) as u32, under, |p| {
            focus_ring(p.cv, R::new(40.0, 40.0, w as f32, h as f32), rad);
        });
        let get = |x: i32, y: i32| Rgb::hex(cv.get(x + 40, y + 40));
        let half = |x: i32| ink_of(get(x, h / 2), under, theme::ACCENT) >= 0.5;
        let outside: Vec<i32> = ((-40)..0).filter(|&x| half(x)).collect();
        let outer = *outside
            .first()
            .expect("the ring drew no ink outside the control");
        let deep = depth(get, under, theme::ACCENT, Rect::new(0, 0, w, h));
        ((-outer) as f32, outside.len() as f32, deep)
    }

    /// Every length `CONTROLS` says the toolkit takes from the kit is the kit's, to the number, and
    /// every source is one `lucina_glass` answers to. A token renamed or recoloured, a height or a
    /// radius the toolkit invented, a row that names a source that is not the kit's: this fails.
    #[test]
    fn the_control_lengths_are_glass_tokens() {
        for c in CONTROLS.iter() {
            let v = glass_length(c.source).unwrap_or_else(|| {
                panic!(
                    "{}: {} is not a length of the glass kit",
                    c.control, c.source
                )
            });
            assert_eq!(
                c.dp, v,
                "{} {}: {} is not the kit's {} ({})",
                c.control, c.what, c.dp, c.source, v
            );
        }
        for c in MEASURED.iter() {
            let (v, tol) = glass_measured(c.source).unwrap_or_else(|| {
                panic!(
                    "{}: {} is not a number of the glass kit",
                    c.control, c.source
                )
            });
            assert!(
                (c.dp - v).abs() <= tol,
                "{} {}: {} against the kit's {v} ({} allowed)",
                c.control,
                c.what,
                c.dp,
                tol
            );
        }
    }

    /// The toolkit scales nothing that is not a token or a number read from the kit. What it is
    /// allowed to keep is listed here with why, and every entry has to be in use: an excuse that
    /// outlives the length it excuses fails.
    #[test]
    fn no_lengths_of_its_own() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/ui.rs"))
            .expect("the toolkit's own source");
        let allowed = [
            (
                "1.0",
                "the hairline: one device pixel is the least the toolkit can draw",
            ),
            ("6.0", "the scrollbar's width; the kit scrolls without one"),
            (
                "10.0",
                "the camera mark's dot: a state mark, and the kit has no token for it",
            ),
            ("24.0", "the shortest thumb the scrollbar can show"),
            (
                "40.0",
                "how far a wheel notch scrolls: a step of content, not a layout length",
            ),
        ];
        let mut bad: Vec<String> = Vec::new();
        for (n, line) in src.lines().enumerate() {
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
                "px({a}) is allowed with no use left: {why}"
            );
        }
        assert!(
            bad.is_empty(),
            "the toolkit scales a length that is not a glass token:\n{}",
            bad.join("\n")
        );
    }

    /// The quiet button: the kit's fill and its hairline, `radius::MD`, a tap target tall, the label
    /// with `space::L` at its sides, and the states the kit's widgets give.
    #[test]
    fn the_button_is_the_glass_button() {
        let t = theme1();
        let label = "Open file";
        let (fr, _) = pass(300, 120, t.panel, None, false, |ui| {
            ui.button(label);
        });
        let fill = glass_paints(t.panel, color::INK.alpha(0.12));
        assert_eq!(
            quiet(t.panel),
            fill,
            "widgets/button.rs fills the secondary button with INK at 12 %"
        );
        // The fill is a pixel inside the control, and anti-aliased against the card the hairline
        // passes through the fill's own colour, so the shape is read out from a pixel of it.
        let inner = blob(&fr, fill, (6, 22));
        let hair = t.px(1.0).max(1);
        assert_eq!(
            fr.get(inner.x - hair, 22),
            Some(rim(fill)),
            "widgets/button.rs strokes it with `separator`: one device pixel, inside the control"
        );
        let r = inner.inset(-hair);
        assert_eq!(r.h, t.px(TAP), "a button is a tap target tall");
        let mut text = Text::new();
        let cs = TextRole::Control.spec();
        assert_eq!(
            r.w,
            text.measure(cs.face, cs.pt(t), label) + 2 * t.px(EDGE),
            "the button is its label with `space::L` at each side"
        );
        // The stroke is the outer shape and the fill sits inside it, so the boundary of everything
        // nearer the fill than the card is the control's own: radius::MD on the button's rect.
        near(
            corner(&fr, t.panel, fill, r),
            kit_depth(t.px(CONTROL_RAD), r.w, r.h, t.panel),
            1,
            "the button's corners are the kit's radius::MD, drawn on the kit's canvas",
        );
        // Where the kit's own PushButton paints over the card, the toolkit paints the same pixel.
        let cv = glass(300, 120, t.panel, |p| {
            let mut b = PushButton::new(label, false);
            b.draw(p, R::new(r.x as f32, r.y as f32, r.w as f32, r.h as f32));
        });
        assert_eq!(
            fr.get(r.x + 6, 22),
            Some(Rgb::hex(cv.get(r.x + 6, 22))),
            "the quiet button is the kit's button painted"
        );

        // Hover is the kit's brighter glass; a press is its accent fill.
        let (fr, _) = pass(300, 120, t.panel, Some((5, 22)), false, |ui| {
            ui.button(label);
        });
        let hov = glass_paints(t.panel, color::INK.alpha(60.0 / 255.0));
        assert_eq!(
            hovered(t.panel),
            hov,
            "widgets/action.rs brightens hovered glass to INK at 60/255"
        );
        blob(&fr, hov, (6, 22));
        let (fr, _) = pass(300, 120, t.panel, Some((5, 22)), true, |ui| {
            ui.button(label);
        });
        assert_eq!(
            theme::SELECTION,
            theme::rgb(color::ACCENT_FILL),
            "the palette's selection is the kit's `accentFill`, which its press fills with"
        );
        blob(&fr, theme::SELECTION, (6, 22));

        // The main action is the kit's paper button, with the paper's own ink on it.
        let (fr, _) = pass(300, 120, t.panel, None, false, |ui| {
            ui.primary_button(label);
        });
        let paper = theme::rgb(color::PAPER);
        assert_eq!(
            theme::PAPER,
            paper,
            "the primary button is `paper`, not the accent"
        );
        let r = inked(&fr, paper).expect("the main action is filled with paper");
        assert_eq!(r.h, t.px(TAP));
        near(
            corner(&fr, t.panel, paper, r),
            kit_depth(t.px(CONTROL_RAD), r.w, r.h, t.panel),
            1,
            "the paper button's corners are radius::MD too",
        );
        let ink = inked_in(&fr, theme::rgb(color::PAPER_INK), r).expect("its label is `paperInk`");
        assert!(ink.w > 0 && ink.x > r.x, "and it is inside the button");
    }

    /// The field: the kit's ground and outline, `radius::SM`, its text `space::M` in, its caret the
    /// kit's width.
    #[test]
    fn the_field_is_the_glass_field() {
        let t = theme1();
        // A capital M first: a glyph that reaches its own box, so where its ink starts is where the
        // text starts.
        let (fr, _) = pass(300, 120, t.panel, None, false, |ui| {
            let mut e = TextEdit::with_text("MMM");
            let r = Rect::new(20, 30, 240, ui.px(TAP));
            ui.text_field_at(r, "f", &mut e);
        });
        let ground = glass_paints(t.panel, color::INK.alpha(0.09));
        assert_eq!(
            field_ground(t.panel),
            ground,
            "widgets/text_field.rs fills a field with INK at 9 %"
        );
        let edge = glass_paints(ground, color::INK.alpha(0.16));
        assert_eq!(
            field_edge(ground),
            edge,
            "and outlines it with INK at 16 % over its own ground"
        );
        let r = inked(&fr, edge).expect("the field is rimmed with the kit's outline");
        assert_eq!(
            (r.x, r.y, r.w, r.h),
            (20, 30, 240, t.px(TAP)),
            "the field is the rectangle it was given, a tap target tall"
        );
        near(
            corner(&fr, t.panel, edge, r),
            kit_depth(t.px(FIELD_RAD), r.w, r.h, t.panel),
            1,
            "the field's corners are radius::SM",
        );
        let ink = inked_in(&fr, t.text, r).expect("what is typed is the ink");
        near(
            ink.x - r.x,
            t.px(CELL),
            2,
            "the text stands `space::M` in from the edge, not the 10 px it used to",
        );

        // The caret is the kit's, in the accent of what is active.
        let (fr, _) = pass(300, 120, t.panel, None, false, |ui| {
            ui.focus("g");
            let mut e = TextEdit::with_text("");
            let r = Rect::new(20, 30, 240, ui.px(TAP));
            ui.text_field_at(r, "g", &mut e);
        });
        let r = inked(&fr, ground).expect("the field's ground");
        let caret = inked_in(&fr, t.accent, r.inset(3)).expect("a focused field shows its caret");
        assert_eq!(
            caret.w,
            t.px(CARET_W).max(1),
            "the caret is the kit's 1.6 dp, as wide as its ring",
        );
        assert!(
            caret.x > r.x && caret.right() < r.right(),
            "and it stands inside the field"
        );
    }

    /// A selected row is the kit's row: `accentSoft` over the card, a `radius::MD` tile standing
    /// `space::XS` in from the list's edges, a tap target tall.
    #[test]
    fn the_selected_row_is_the_glass_row() {
        let t = theme1();
        let (fr, _) = pass(300, 200, t.panel, None, false, |ui| {
            let mut st = ListState::default();
            st.select(1);
            let r = Rect::new(0, 0, 300, 200);
            ui.list("rows", &mut st, r, 3, 44.0, |_ui, _i, _sel| {});
        });
        let want = glass_paints(t.panel, color::ACCENT_SOFT);
        assert_eq!(
            row_selected(t.panel),
            want,
            "widgets/list_row.rs fills the selected row with `accentSoft`, not with a dimmed accent"
        );
        let band = inked(&fr, want).expect("the selected row is accentSoft over the card");
        assert_eq!(band.x, t.px(BAND_INSET), "the band stands `space::XS` in");
        assert_eq!(band.y, t.px(TAP), "the second of three rows");
        assert_eq!(band.h, t.px(TAP), "and a row is a tap target tall");
        near(
            corner(&fr, t.panel, want, band),
            kit_depth(t.px(CONTROL_RAD), band.w, band.h, t.panel),
            1,
            "the band is a `radius::MD` tile",
        );
        let cv = glass(300, 200, t.panel, |p| {
            let mut row = ListRow::new(None, "").selected(true);
            row.draw(
                p,
                R::new(band.x as f32, band.y as f32, band.w as f32, band.h as f32),
                ty::LEAD,
                &Palette::STANDARD,
            );
        });
        assert_eq!(
            fr.get(band.x + 3, mid(band)),
            Some(Rgb::hex(cv.get(band.x + 3, mid(band)))),
            "the band is the kit's row painted"
        );
    }

    /// The on/off control is the kit's switch: its track, its thumb, where the thumb sits, its accent
    /// for on, and `space::M` before the label.
    #[test]
    fn the_checkbox_is_the_glass_switch() {
        for on in [false, true] {
            let t = theme1();
            // No label in this pass, so every colour on the screen is the control's own: the label is
            // the ink anti-aliased over the card, which paints every step between the two, and one of
            // those steps is the resting track's colour.
            let (fr, _) = pass(200, 80, t.panel, None, false, move |ui| {
                let mut flag = on;
                assert!(!ui.checkbox("", &mut flag), "a pass changes nothing");
            });
            let rest = glass_paints(t.panel, color::INK.alpha(0.16));
            assert_eq!(
                track_off(t.panel),
                rest,
                "widgets/switch.rs rests the track at INK 16 %"
            );
            assert_eq!(
                t.accent,
                theme::rgb(color::ACCENT),
                "and fills it with the accent when on, which is what is active (rule 10)"
            );
            let want = if on { t.accent } else { rest };
            if !on {
                // The resting track is stroked, like the button; on is filled to its edge.
                assert_eq!(
                    fr.get(0, 22),
                    Some(rim(rest)),
                    "the track's rim is the same hairline as everywhere else"
                );
            }
            // The track's own edge, read where the ink is half over the card. An exact colour stops
            // short of the last column of a curve, and the rim of the resting track is not its fill.
            let (x0, x1) = hspan(&fr, t.panel, want, 22);
            let (y0, y1) = vspan(&fr, t.panel, want, (x0 + x1) / 2);
            let track = Rect::new(x0, y0, x1 - x0 + 1, y1 - y0 + 1);
            assert_eq!(
                (track.w, track.h),
                (t.px(switch::W), t.px(switch::H)),
                "the track is the kit's W by its H"
            );
            assert_eq!(track.x, 0, "it leads the row");
            assert_eq!(
                track.y,
                (t.px(TAP) - t.px(switch::H)) / 2,
                "and sits in the middle of a row a tap target tall"
            );
            near(
                corner(&fr, t.panel, want, track),
                kit_depth(t.px(switch::H) / 2, track.w, track.h, t.panel),
                1,
                "the track is a pill, rounded at half its height like the kit's",
            );
            let thumb = inked_in(&fr, t.text, track).expect("the thumb is the kit's INK");
            near(
                thumb.w,
                t.px(SWITCH_THUMB),
                2,
                "the thumb is the kit's disc, which it measures off its own canvas",
            );
            let pad = (t.px(switch::H) - t.px(SWITCH_THUMB)) / 2;
            let edge = if on {
                track.right() - thumb.right()
            } else {
                thumb.x - track.x
            };
            near(
                edge,
                pad,
                2,
                "and it rests PAD from the end of its travel, not centred in a box",
            );
            // The same track, drawn by the kit's own widget, is the same pixel.
            let cv = glass(200, 80, t.panel, |p| {
                let mut s = Switch::new(on);
                s.draw(p, track.right() as f32, mid(track) as f32);
            });
            let x = if on { track.x + 4 } else { track.right() - 5 };
            assert_eq!(
                fr.get(x, mid(track)),
                Some(Rgb::hex(cv.get(x, mid(track)))),
                "the switch at on={on} is the kit's switch painted"
            );
        }
        // The label stands `space::M` after the track.
        let t = theme1();
        let (fr, _) = pass(320, 80, t.panel, None, false, |ui| {
            let mut flag = false;
            ui.checkbox("Wi-Fi", &mut flag);
        });
        let ink = inked_in(
            &fr,
            t.text,
            Rect::new(t.px(switch::W), 0, 320 - t.px(switch::W), 80),
        )
        .expect("the label is the ink");
        near(
            ink.x - t.px(switch::W),
            t.px(CELL),
            3,
            "`space::M` between the track and its label",
        );
    }

    /// The keyboard ring is the Home's: the accent, `RING_W` dp of it, standing `RING_OFF` outside the
    /// control, its corners the control's radius grown by the offset.
    #[test]
    fn the_focus_ring_is_the_glass_ring() {
        let t = theme1();
        let (fr, _) = pass(300, 120, t.panel, None, false, |ui| {
            ui.focus("f");
            let mut e = TextEdit::with_text("");
            let r = Rect::new(60, 30, 200, ui.px(TAP));
            ui.text_field_at(r, "f", &mut e);
        });
        let ground = glass_paints(t.panel, color::INK.alpha(0.09));
        let edge = glass_paints(ground, color::INK.alpha(0.16));
        let r = inked(&fr, edge).expect("the focused field");
        assert_eq!(
            (r.x, r.y),
            (60, 30),
            "the control is where it was put, with room for its ring"
        );
        let half = |x: i32| ink_of(fr.get(x, mid(r)).unwrap_or(t.panel), t.panel, t.accent) >= 0.5;
        let (outer, inner) =
            run(|x| x < r.x && half(x), r.x).expect("the ring draws no accent outside the control");
        let wide = inner - outer + 1;
        let (g_off, g_w, g_deep) = glass_ring(FIELD_RAD, r.w, r.h);
        near(
            r.x - outer,
            t.px(RING_OFF),
            1,
            "the ring stands RING_OFF outside the control",
        );
        near(r.x - outer, g_off as i32, 1, "as the kit's ring does");
        near(wide, t.px(RING_W), 1, "the ring is RING_W dp of ink");
        near(wide, g_w as i32, 1, "as the kit's band is");
        near(
            corner(&fr, t.panel, t.accent, r),
            g_deep,
            1,
            "its corners follow the control's radius grown by the offset, as the kit's do",
        );
        // The ring is the accent of what is active, not a grey band.
        assert_eq!(t.accent, theme::rgb(color::ACCENT), "the ring is `accent`");
    }

    /// The bar is a tap-sized row of controls ruled at its foot with a hairline, not banded with the
    /// accent, which is for what is selected or active (rule 10).
    #[test]
    fn the_bar_is_a_tap_row_ruled_with_a_hairline() {
        let t = theme1();
        let (fr, _) = pass(400, 200, theme::GROUND, None, false, |ui| {
            ui.header("Notes");
        });
        let face = inked(&fr, t.bar).expect("the bar is the surface glass bars are made of");
        assert_eq!((face.x, face.y, face.w), (0, 0, 400));
        // The rule is painted over the bar's own last row, so its face stops one row short: the two
        // together are the tap row.
        assert_eq!(
            face.h + 1,
            t.px(TAP),
            "the bar and its rule are a tap row tall"
        );
        let bar = Rect::new(face.x, face.y, face.w, face.h + 1);
        assert!(
            (0..bar.w).all(|x| fr.get(x, bar.bottom() - 1) == Some(HAIRLINE)),
            "its foot is the hairline, one device pixel across"
        );
        assert!(
            (0..bar.w).all(|x| fr.get(x, bar.bottom() - 3) == Some(t.bar)),
            "and not a band of the accent"
        );
        assert!(
            inked(&fr, t.accent).is_none(),
            "nothing in the bar is the accent of its own"
        );
        let title = inked_in(&fr, t.text, Rect::new(0, 0, 200, bar.h)).expect("the app's name");
        near(title.x, t.px(EDGE), 2, "it stands the gutter in");
    }

    /// The dialog is the kit's sheet: `radius::XL`, the card's hairline rim, `space::L` of padding.
    /// Its material, shadow and blur are the compositor's, and wait for step 4.
    #[test]
    fn the_dialog_is_the_glass_sheet() {
        let t = theme1();
        let (fr, _) = pass(400, 300, theme::GROUND, None, false, |ui| {
            ui.modal(200.0, 120.0, |_ui| {});
        });
        let card = inked(&fr, HAIRLINE).expect("the sheet is rimmed with the hairline");
        assert_eq!(
            (card.w, card.h),
            (200, 120),
            "the dialog is the size asked for"
        );
        assert_eq!(
            (card.x, card.y),
            ((400 - 200) / 2, (300 - 120) / 2),
            "and centred on the screen"
        );
        assert_eq!(
            HAIRLINE,
            theme::over(color::HAIR, theme::PANEL),
            "the rim is `separator` over the card, as it is everywhere else"
        );
        // The sheet sits on its scrim, so the card's edge is read against what it is drawn over.
        let scrim = fr
            .get(card.x - 6, mid(card))
            .expect("the sheet is on its scrim");
        near(
            corner(&fr, scrim, HAIRLINE, card),
            kit_depth(t.px(SHEET_RAD), card.w, card.h, scrim),
            1,
            "its corners are the sheet's `radius::XL`",
        );
        let (fr, _) = pass(400, 300, theme::GROUND, None, false, |ui| {
            ui.modal(200.0, 120.0, |ui| {
                ui.button("OK");
            });
        });
        let fill = glass_paints(t.panel, color::INK.alpha(0.12));
        let b = blob(
            &fr,
            fill,
            (card.x + t.px(EDGE) + 6, card.y + t.px(EDGE) + t.px(TAP) / 2),
        );
        assert_eq!(
            (b.x, b.y),
            (card.x + t.px(EDGE) + 1, card.y + t.px(EDGE) + 1),
            "the content stands `space::L` in from the card, once, and is a button of the toolkit"
        );
    }
}
