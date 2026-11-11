//! The design tokens, in one place: change a value here and every widget and screen follows. They are the Lucina house
//! tokens (SecretaryIOS `design/tokens.json`, dark values) under the rules of docs/design/rules.md, plus the glass and the
//! thin display face the owner kept from the concept. Lengths are dp on a 360 x 800 phone (1080 x 2400 at 3x); durations
//! are seconds.

use crate::canvas::Color;

/// Colours (rules 9 and 10).
pub mod color {
    use super::Color;

    /// `text`.
    pub const INK: Color = Color::hex(0xffffff);
    /// `textSecondary`: #EBEBF5 at 60 %.
    pub const INK_2: Color = Color::hexa(0xebebf5, 153);
    /// `textTertiary`: #EBEBF5 at 30 %.
    pub const INK_3: Color = Color::hexa(0xebebf5, 77);
    // INK_3 is below 4.5:1 on the cards: marks and dots only, never text.
    /// Hairlines and quiet fills on glass (the `separator` role, translucent so the glass shows).
    pub const HAIR: Color = Color::hexa(0xebebf5, 36);
    /// `accent`: what is selected or active.
    pub const ACCENT: Color = Color::hex(0x7f8fe0);
    /// `accentFill`: the accent under white text, and its gradient's dark end.
    pub const ACCENT_FILL: Color = Color::hex(0x4a5bb5);
    /// `accentSoft`.
    pub const ACCENT_SOFT: Color = Color::hexa(0x7f8fe0, 46);
    /// `negative`: live or urgent.
    pub const LIVE: Color = Color::hex(0xff453a);
    /// The negative's darker end, for its gradients.
    pub const LIVE_DEEP: Color = Color::hex(0xd6302a);
    /// `info`: secondary signals (from other people).
    pub const SIGNAL: Color = Color::hex(0x64d2ff);
    /// `positive`.
    pub const POSITIVE: Color = Color::hex(0x30d158);
    /// `warning`.
    pub const WARNING: Color = Color::hex(0xff9f0a);
    /// `bg`: the black page.
    pub const VOID: Color = Color::hex(0x000000);
    /// `card` and `cardRaised`: surfaces where there is no backdrop to show through.
    pub const CARBON: Color = Color::hex(0x1c1c1e);
    pub const CARBON_HI: Color = Color::hex(0x2c2c2e);
    /// The light "today" tile and the play button.
    pub const PAPER: Color = Color::hex(0xf2f2f7);
    pub const PAPER_INK: Color = Color::hex(0x000000);
    /// Glass over a photograph: the tint mixed into the blurred backdrop, and its rim (rule 7).
    pub const GLASS_TINT: Color = Color::hexa(0x1c1c1e, 110);
    pub const GLASS_RIM: Color = Color::hexa(0xffffff, 70);
    /// Content cards: a thick material, mostly opaque so text keeps its contrast, with the blurred backdrop still showing
    /// (HIG Materials: no Liquid Glass in the content layer; thicker materials for text). Glass (`GLASS_TINT`) is for controls.
    pub const MATERIAL: Color = Color::hexa(0x1c1c1e, 168);
    /// The bottom sheet and other dark glass (the tab bar's `#1C1C1EB8`).
    pub const SMOKE: Color = Color::hexa(0x1c1c1e, 184);
    /// Bars that follow the page behind them (browser tab strip, toolbar, address pill, dock; widgets/adaptive.rs): the dark
    /// glass is the tint over a dark page, the light glass over a light one, and `INK_ON_LIGHT` is the ink on the latter.
    pub const GLASS_BAR_TINT: Color = Color::hexa(0x1c1c1e, 150);
    pub const GLASS_TINT_LIGHT: Color = Color::hexa(0xf2f2f7, 176);
    pub const GLASS_RIM_LIGHT: Color = Color::hexa(0x000000, 46);
    pub const INK_ON_LIGHT: Color = Color::hex(0x1c1c1e);
    /// Shadows under floating cards (`scrim`).
    pub const SHADOW: Color = Color::hexa(0x000000, 110);
}

/// Corner radii (rule 14).
pub mod radius {
    /// `sheet`: the bottom sheet, the header cards.
    pub const XL: f32 = 24.0;
    /// `card`.
    pub const LG: f32 = 18.0;
    /// `tile` and `button`: calendar tiles.
    pub const MD: f32 = 14.0;
    /// `field`.
    pub const SM: f32 = 12.0;
    pub const PILL: f32 = 999.0;
}

/// Spacing: a 4 dp grid (rule 13).
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    /// `gridGap`.
    pub const M: f32 = 12.0;
    /// `gutter` and `cardPadding`.
    pub const L: f32 = 16.0;
    pub const XL: f32 = 20.0;
    pub const XXL: f32 = 28.0;
    /// The screen's side margin: the gutter.
    pub const EDGE: f32 = 16.0;
    /// The smallest tap target.
    pub const TAP: f32 = 44.0;
}

/// The type scale: (size dp, tracking dp). Rule 11, with rule 12's exception (Lato Thin display words and numerals).
pub mod ty {
    /// A screen's display word ("Build", "Notifications"): Lato Thin, the concept's signature.
    pub const DISPLAY: (f32, f32) = (76.0, -2.0);
    /// The big numerals: Lato Thin.
    pub const NUMERAL: (f32, f32) = (56.0, -1.5);
    /// `largeTitle` size, in the display face: a screen title in a header card.
    pub const TITLE: (f32, f32) = (34.0, -0.5);
    /// `title2`: a stat value.
    pub const TITLE2: (f32, f32) = (22.0, 0.0);
    /// `headline`: card titles.
    pub const HEADLINE: (f32, f32) = (17.0, 0.0);
    /// `subheadline`.
    pub const LEAD: (f32, f32) = (15.0, 0.0);
    /// `footnote`.
    pub const BODY: (f32, f32) = (13.0, 0.0);
    /// `caption` (12/600): a stat label with its icon.
    pub const CAPTION: (f32, f32) = (12.0, 0.0);
    /// `caption2`.
    pub const SMALL: (f32, f32) = (11.0, 0.0);
    /// Mono, uppercase: codes and timers only (rule 5).
    pub const MONO: (f32, f32) = (10.5, 0.6);
    pub const MICRO: (f32, f32) = (8.5, 0.8);
}

/// Glass.
pub mod glass {
    /// Backdrop blur radius in dp (three box passes; about a 28 dp Gaussian). The tab bar's `blur` is 24.
    pub const BLUR: f32 = 16.0;
}

/// Motion (rule 15).
pub mod motion {
    /// `fast`: press feedback.
    pub const PRESS: f64 = 0.12;
    /// `base`: small state changes.
    pub const SHORT: f64 = 0.22;
    /// `slow`: moves and morphs.
    pub const MEDIUM: f64 = 0.38;
    /// Screen transitions.
    pub const LONG: f64 = 0.62;
    /// The live pill's breathing period.
    pub const PULSE: f64 = 1.6;
    /// Between staggered items, ms.
    pub const STAGGER_MS: u32 = 38;
    /// The latest any staggered item starts, ms.
    pub const STAGGER_CAP_MS: u32 = 520;
}
