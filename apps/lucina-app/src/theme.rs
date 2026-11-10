//! The look of Juno, shared by the loader, the shell and every app so that nothing jumps from one screen to the next: the glass
//! concept's tokens (docs/design/rules.md; `lucina_glass::tokens`). A near-black page that matches the night sky, dark cards, one
//! accent for what is selected, semantic colours. `ui::Theme` scales sizes to the screen on top of these.

use crate::gfx::Rgb;

/// The page: the night sky's top colour, so a plain screen and the home meet without a jump.
pub const GROUND: Rgb = Rgb::hex(0x070b18);
/// `accent`: focus, what is selected, the main action.
pub const ACCENT: Rgb = Rgb::hex(0x7f8fe0);
pub const WHITE: Rgb = Rgb::hex(0xffffff);
pub const ITEM: Rgb = Rgb::hex(0xd8d8e0);
pub const DIM: Rgb = Rgb::hex(0x8e8e96);
/// `card`.
pub const PANEL: Rgb = Rgb::hex(0x1c1c1e);
/// `cardRaised`.
pub const PANEL_HI: Rgb = Rgb::hex(0x2c2c2e);
pub const BAR: Rgb = Rgb::hex(0x0c0d12);
pub const TERM_BG: Rgb = Rgb::hex(0x050608);
pub const TERM_FG: Rgb = Rgb::hex(0xf2f2f7);
pub const GREEN: Rgb = Rgb::hex(0x30d158);
/// `accentFill`.
pub const SELECTION: Rgb = Rgb::hex(0x4a5bb5);
pub const DANGER: Rgb = Rgb::hex(0xff453a);

/// The old names, for the code that still says them: the page is no longer navy and the accent no longer gold.
pub const NAVY: Rgb = GROUND;
pub const GOLD: Rgb = ACCENT;
