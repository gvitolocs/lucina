//! The look of Juno, shared by the loader, the shell and every app so that nothing jumps from one screen to the next: these are
//! the glass kit's tokens (docs/design/rules.md; `lucina_glass::tokens`), read from it rather than copied beside it. A black page,
//! dark cards, one accent for what is selected, semantic colours. `ui::Theme` scales sizes to the screen on top of these.

use crate::gfx::Rgb;
use lucina_glass::Color;
use lucina_glass::tokens::color;

/// A token as the toolkit's colour: `Rgb` is opaque by construction, so a translucent token is not a colour of its own here.
pub const fn rgb(c: Color) -> Rgb {
    Rgb(c.xrgb())
}

/// One channel of `c` over `under`, the glass canvas's own formula (`lucina_glass::canvas::Canvas::blend_px` at full coverage):
/// `round(dst + (src - dst) * alpha)`.
const fn blend_channel(src: u8, under: u32, shift: u32, alpha: f32) -> u32 {
    let dst = ((under >> shift) & 0xff) as f32;
    // The value is in 0..=255, so adding a half and truncating is the round the canvas does.
    ((src as f32 - dst) * alpha + dst + 0.5) as u32
}

/// The pixel `c` paints over `under`: what a translucent token means where it is used, since the toolkit has no alpha.
pub const fn over(c: Color, under: Rgb) -> Rgb {
    let alpha = c.a as f32 / 255.0;
    Rgb((blend_channel(c.r, under.0, 16, alpha) << 16)
        | (blend_channel(c.g, under.0, 8, alpha) << 8)
        | blend_channel(c.b, under.0, 0, alpha))
}

/// A token at a fraction of its own alpha, which is how the glass widgets write their states
/// (`color::INK.alpha(0.12)`); the same rounding as [`Color::alpha`], which is not a `const fn`.
pub const fn frac(c: Color, f: f32) -> Color {
    let f = if f > 1.0 {
        1.0
    } else if f < 0.0 {
        0.0
    } else {
        f
    };
    Color {
        a: (c.a as f32 * f + 0.5) as u8,
        ..c
    }
}

/// The page: `bg`, the black the glass kit pages on.
pub const GROUND: Rgb = rgb(color::VOID);
/// `accent`: focus, what is selected, the main action.
pub const ACCENT: Rgb = rgb(color::ACCENT);
/// `text`.
pub const WHITE: Rgb = rgb(color::INK);
/// The content a person reads — bodies, list titles, file names, message text, navigation entries — in
/// the kit's primary text token. The name `ITEM` is kept so the 115 call sites that use it still work;
/// it is not the secondary shade, which would dim content the owner reads.
pub const ITEM: Rgb = rgb(color::INK);
/// `textSecondary`: metadata only — dates, authors, counts, hints, captions, field labels, disabled.
pub const DIM: Rgb = over(color::INK_2, GROUND);
/// `card`.
pub const PANEL: Rgb = rgb(color::CARBON);
/// `cardRaised`.
pub const PANEL_HI: Rgb = rgb(color::CARBON_HI);
/// The header strip and the side rails: `card`, the surface glass bars are made of.
pub const BAR: Rgb = rgb(color::CARBON);
/// The terminal's ground: content rather than chrome, so it is the darkest token, `bg`.
pub const TERM_BG: Rgb = rgb(color::VOID);
/// The terminal's ink: no token for it (the same bytes as `paper`, which is the light tile and the play button, not text).
pub const TERM_FG: Rgb = Rgb::hex(0xf2f2f7);
/// `positive`.
pub const GREEN: Rgb = rgb(color::POSITIVE);
/// `accentFill`.
pub const SELECTION: Rgb = rgb(color::ACCENT_FILL);
/// `negative`.
pub const DANGER: Rgb = rgb(color::LIVE);
/// The 1px hairline on cards, fields and dialogs: `separator` over the card it sits on.
pub const HAIRLINE: Rgb = over(color::HAIR, PANEL);
/// `paper`: the glass kit's primary button, whose ink is `PAPER_INK` (`widgets/button.rs`). The accent
/// stays for what is selected or active (rule 10), so it is not a button colour.
pub const PAPER: Rgb = rgb(color::PAPER);
/// The ink on `paper`.
pub const PAPER_INK: Rgb = rgb(color::PAPER_INK);

/// The old names, for the code that still says them: the page is no longer navy and the accent no longer gold.
pub const NAVY: Rgb = GROUND;
pub const GOLD: Rgb = ACCENT;

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_glass::{Canvas, R};

    /// The pixel a glass canvas really paints: `c` filled over `under`, read back from the middle of the shape.
    fn glass_paints(under: Rgb, c: Color) -> Rgb {
        let mut cv = Canvas::new(8, 8, 1.0);
        cv.clear(Color::hex(under.0));
        cv.fill_rrect(R::new(0.0, 0.0, 8.0, 8.0), 0.0, c);
        Rgb::hex(cv.get(4, 4))
    }

    /// The palette is the tokens, and nothing else: a token renamed or recoloured shows up here, not in a picture.
    #[test]
    fn every_colour_is_a_token() {
        assert_eq!(ACCENT, rgb(color::ACCENT));
        assert_eq!(WHITE, rgb(color::INK));
        assert_eq!(ITEM, rgb(color::INK));
        assert_eq!(PANEL, rgb(color::CARBON));
        assert_eq!(PANEL_HI, rgb(color::CARBON_HI));
        assert_eq!(SELECTION, rgb(color::ACCENT_FILL));
        assert_eq!(DANGER, rgb(color::LIVE));
        assert_eq!(GREEN, rgb(color::POSITIVE));
        assert_eq!(GROUND, rgb(color::VOID));
        assert_eq!(BAR, rgb(color::CARBON));
        assert_eq!(TERM_BG, rgb(color::VOID));
        assert_eq!(PAPER, rgb(color::PAPER));
        assert_eq!(PAPER_INK, rgb(color::PAPER_INK));
        assert_eq!(NAVY, GROUND);
        assert_eq!(GOLD, ACCENT);
    }

    /// `frac` is the glass widget's own `Color::alpha`, to the byte: the control states of step 3 are
    /// written with it, and a different rounding would be a colour of the toolkit's.
    #[test]
    fn frac_is_the_glass_alpha() {
        for f in [
            0.0f32, 0.05, 0.09, 0.1, 0.12, 0.16, 0.235, 0.5, 1.0, 1.4, -0.2,
        ] {
            for c in [color::INK, color::HAIR, color::ACCENT_SOFT] {
                assert_eq!(frac(c, f).a, c.alpha(f).a, "alpha({f}) of {c:?}");
                assert_eq!(frac(c, f).xrgb(), c.xrgb(), "frac keeps the colour");
            }
        }
    }

    /// The translucent roles are the pixel the canvas paints with them, in `over` and in the canvas itself.
    #[test]
    fn the_translucent_roles_are_the_glass_composite() {
        for (name, got, under, token) in [
            ("DIM", DIM, GROUND, color::INK_2),
            ("HAIRLINE", HAIRLINE, PANEL, color::HAIR),
        ] {
            let painted = glass_paints(under, token);
            assert_eq!(
                got,
                over(token, under),
                "{name} is not its token composited"
            );
            assert_eq!(
                got, painted,
                "{name} is not what the canvas paints of {token:?}"
            );
        }
        // The canvas really blended: the same token read as if it were opaque is a different pixel.
        assert_ne!(glass_paints(GROUND, color::INK_2), rgb(color::INK_2));
    }

    /// Content text and metadata are two colours again: merging them onto the secondary shade dimmed
    /// the note bodies and the navigation entries the owner reads (step 3b review).
    #[test]
    fn content_is_not_metadata() {
        assert_ne!(
            ITEM, DIM,
            "ITEM is the primary token, not the metadata shade"
        );
    }
}
