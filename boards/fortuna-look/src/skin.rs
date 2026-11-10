//! The look in one place: type roles, colour roles and the two surfaces. Screens draw only through these, so a re-skin
//! changes this file (and the generated faces and tokens), not the screens.
//!
//! The merged look (2026-11-08): Fortuna's own structure and content, the house accent, coloured app icons; from the
//! Glass concept (docs/design/rules.md, apps/lucina-glass) the Lato Light display face for big values and titles, the
//! deep navy backdrop with a dithered gradient on the top screen, the thick MATERIAL for content and clear glass for
//! controls (each with a 1 px rim and a top highlight), the segmented control and the progress ring.

use crate::canvas::{Canvas, Rgba};
use crate::text::{Font, BOLD_11, BOLD_15, BOLD_9, LIGHT_15, LIGHT_19, LIGHT_27, LIGHT_40, SANS_11, SANS_9};
use crate::tokens::{color as c, glass as g};

// ---- type ---------------------------------------------------------------------------------------------------------

/// Display, Lato Light (Glass rule 12, Light for Thin on the DS): the clock and the boot screen's name.
pub static DISPLAY_XL: &Font = &LIGHT_40;
/// Display: a screen's display word (a folder's name, "Fortuna" on System).
pub static DISPLAY: &Font = &LIGHT_27;
/// Display: a stat's value ("2 h 14 min", "1.9 GB").
pub static VALUE: &Font = &LIGHT_19;
/// Display: a game's title on its card.
pub static GAME_TITLE: &Font = &LIGHT_15;
/// title2 (DejaVu Bold): dialog titles.
pub static TITLE: &Font = &BOLD_15;
/// headline: card titles, tile names, buttons, tabs, keys.
pub static HEADLINE: &Font = &BOLD_11;
/// body: list rows.
pub static BODY: &Font = &SANS_11;
/// footnote: the one short line under a fact, sizes and dates.
pub static FOOTNOTE: &Font = &SANS_9;
/// caption (600): a stat's label next to its icon. Sentence case.
pub static CAPTION: &Font = &BOLD_9;

// ---- colour roles (Glass rules 9 and 10) --------------------------------------------------------------------------

/// Text.
pub const INK: Rgba = g::INK;
/// Secondary text: #EBEBF5 at 60 %. INK_3 is for marks only (under 4.5:1).
pub const INK_2: Rgba = g::INK_2;
pub const INK_3: Rgba = g::INK_3;
/// What is selected or active: the focus ring, the active tab and filter, the ring's progress.
pub const ACCENT: Rgba = g::ACCENT;
/// The accent under white text: primary buttons.
pub const ACCENT_FILL: Rgba = g::ACCENT_FILL;
pub const ACCENT_SOFT: Rgba = g::ACCENT_SOFT;
/// Live or urgent only: a stalled service, an error, a destructive action that would cut something running.
pub const LIVE: Rgba = g::LIVE;
pub const POSITIVE: Rgba = g::POSITIVE;
pub const WARNING: Rgba = g::WARNING;
pub const SIGNAL: Rgba = g::SIGNAL;
/// Ink on the light app-icon tints.
pub const ON_LIGHT: Rgba = Rgba::hex(0x000000);
pub const HAIR: Rgba = g::HAIR;

// ---- backdrops (lucina-glass layouts/fortuna.rs) --------------------------------------------------------------------

pub const TOP_FROM: Rgba = Rgba::hex(0x0b1630);
pub const TOP_TO: Rgba = Rgba::hex(0x020306);
pub const BOTTOM_BG: Rgba = Rgba::hex(0x07080a);
pub const HINGE: Rgba = Rgba::hex(0x2a2b30);

/// The top screen's backdrop: the navy gradient.
pub fn top_backdrop(cv: &mut Canvas) {
    let (w, h) = (cv.w, cv.h);
    cv.gradient(0, 0, w, h, TOP_FROM, TOP_TO);
}

/// The bottom screen's backdrop.
pub fn bottom_backdrop(cv: &mut Canvas) {
    cv.clear(BOTTOM_BG);
}

// ---- surfaces (Glass rule 7, HIG Materials) -------------------------------------------------------------------------

/// The top highlight and the controls' rim: tokens.json glassHighlight and glassBorder (white at their alphas).
const HIGHLIGHT: Rgba = Rgba::new(255, 255, 255, c::GLASS_HIGHLIGHT_ALPHA);
const BORDER: Rgba = Rgba::new(255, 255, 255, c::GLASS_BORDER_ALPHA);

/// Content (cards, tiles, list groups): Glass's thick MATERIAL with its hairline and a top highlight.
pub fn material(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32) {
    cv.panel(x, y, w, h, r, g::MATERIAL, HAIR, HIGHLIGHT);
}

/// Controls (the tab bar, the keyboard, dialogs, the search field, filter pills): clear glass. With no backdrop to blur
/// on the DS it is Glass's SMOKE tint with the glassBorder rim.
pub fn glass(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32) {
    // glass is for controls only (rules.md): every glass shape is one
    cv.note_control();
    cv.panel(x, y, w, h, r, g::SMOKE, BORDER, HIGHLIGHT);
}

/// A generated cover's ground: the raised card colour, opaque, with the hairline.
pub fn cover_ground(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32) {
    cv.panel(x, y, w, h, r, g::CARBON_HI, HAIR, HIGHLIGHT);
}

/// The focus ring: 2 px of accent, 2 px outside the focused thing.
pub fn focus(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32) {
    cv.note_control();
    cv.outline(x, y, w, h, r, 1, 2, ACCENT);
}

/// A dialog: the token card colour, opaque (Glass rule 7: where there is no backdrop to blur, the card colour), with
/// the glass rim and highlight, so nothing under it shows through.
pub fn sheet(cv: &mut Canvas, x: i32, y: i32, w: i32, h: i32, r: i32) {
    cv.panel(x, y, w, h, r, g::CARBON, BORDER, HIGHLIGHT);
}

/// The name of the focused or pressed item, as on a Dock: one line in a small glass label just above it (below when
/// there is no room above). The DSi has no hover, so focus and press are when names show.
pub fn name_label(cv: &mut Canvas, x: i32, y: i32, w: i32, text: &str) {
    let tw = FOOTNOTE.width(text).min(cv.w - 16);
    let (lw, lh) = (tw + 12, 15);
    let lx = (x + w / 2 - lw / 2).clamp(2, cv.w - 2 - lw);
    let ly = if y - lh - 3 >= 0 { y - lh - 3 } else { y + 3 };
    cv.note_label(lx, ly, lw, lh);
    cv.begin_overlay();
    cv.panel(lx, ly, lw, lh, lh / 2, g::CARBON, BORDER, HIGHLIGHT);
    cv.end_overlay();
    cv.text_fit(FOOTNOTE, lx + 6, ly + 11, text, tw, INK);
}
