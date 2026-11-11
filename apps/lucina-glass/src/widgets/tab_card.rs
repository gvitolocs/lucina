//! A tab card of the overview: a thumbnail of the page over a row with the site mark (or a spinner), the title and a close
//! button, on the thick material; the selected card is ringed in the accent.

use crate::Paint;
use crate::canvas::{Canvas, R};
use crate::icons::Icon;
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};

use super::bar::BarButton;
use super::chrome::spinner;
use super::ink::{focus_ring, tap, text_left};
use super::tab_chip::site_mark;

/// The room round the thumbnail and the height of the row under it, dp.
const PAD: f32 = 8.0;
const ROW: f32 = 44.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPart {
    Close,
    Card,
}

#[derive(Clone, Debug)]
pub struct TabCard {
    pub title: String,
    pub host: String,
    pub loading: bool,
    pub selected: bool,
    pub focused: bool,
    pub min_hit: f32,
    last: R,
    close: BarButton,
}

impl TabCard {
    pub fn new(title: &str, host: &str) -> TabCard {
        TabCard {
            title: title.to_string(),
            host: host.to_string(),
            loading: false,
            selected: false,
            focused: false,
            min_hit: space::TAP,
            last: R::default(),
            close: BarButton::new(Icon::Close),
        }
    }

    /// The card's height for a thumbnail `th` tall.
    pub fn height(th: f32) -> f32 {
        th + 2.0 * PAD + ROW
    }

    /// The columns of a `cols` wide grid over `width` dp from `x`, `gap` apart: column `i`'s left edge and the common width.
    pub fn column(x: f32, width: f32, cols: u32, gap: f32, i: u32) -> (f32, f32) {
        super::cell_x(
            R::new(x, 0.0, width, 0.0),
            cols.max(1),
            gap,
            i.min(cols.max(1) - 1),
            1,
        )
    }

    /// Draw it in `r`; `thumb` is the page's render, of which the top part (in the thumbnail's proportion) is shown.
    /// `touch` gives the close button the 44 dp of a finger.
    pub fn draw(&mut self, p: &mut Paint, r: R, thumb: &Canvas, touch: bool) {
        self.last = r;
        p.glass(r, radius::LG, color::MATERIAL);
        if self.selected {
            p.cv.stroke_rrect(r, radius::LG, 2.0, color::ACCENT);
        }
        let th = (r.h - 2.0 * PAD - ROW).max(1.0);
        let tr = R::new(r.x + PAD, r.y + PAD, r.w - 2.0 * PAD, th);
        let sw = thumb.w as f32;
        let sh = (sw * th / tr.w).min(thumb.h as f32);
        p.cv.blit_region(thumb, (0.0, 0.0, sw, sh), tr, 10.0, 1.0);
        let cy = tr.bottom() + 24.0;
        if self.loading {
            spinner(p, r.x + 20.0, cy, 8.0, color::INK_2);
        } else {
            site_mark(p, r.x + 20.0, cy, 18.0, &self.host, color::INK);
        }
        let room = r.w - 20.0 - 18.0 - 8.0 - 36.0;
        let title = p.fit(Font::Regular, ty::BODY, &self.title, room);
        text_left(
            p,
            Font::Regular,
            ty::BODY,
            r.x + 40.0,
            cy,
            color::INK,
            &title,
        );
        self.close.min_hit = self.min_hit;
        self.close.draw(
            p,
            r.right() - 24.0,
            cy,
            if touch { space::TAP } else { 30.0 },
            color::INK_2,
        );
        if self.focused {
            focus_ring(p.cv, r, radius::LG);
        }
    }

    /// The part under (`x`, `y`) as last drawn: the close button first, else the card.
    pub fn hit(&self, x: f32, y: f32) -> Option<CardPart> {
        if self.last.contains(x, y) && self.close.hit(x, y) {
            Some(CardPart::Close)
        } else if tap(self.last, self.min_hit, x, y) {
            Some(CardPart::Card)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Color;
    use crate::text::Type;

    #[test]
    fn draws_with_a_thumbnail_and_selection_ring() {
        let mut ty = Type::new();
        let mut page = Canvas::new(200, 300, 1.0);
        page.clear(Color::hex(0xffffff));
        let mut frames = Vec::new();
        for selected in [false, true] {
            let mut cv = Canvas::new(240, 200, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.0,
                backdrop: None,
            };
            let mut c = TabCard::new("Release notes", "notes.example.org");
            c.selected = selected;
            let h = TabCard::height(100.0);
            c.draw(&mut p, R::new(20.0, 10.0, 200.0, h), &page, false);
            frames.push(cv.px.clone());
            // The thumbnail is white inside the card.
            assert_eq!(cv.px[70 * 240 + 120] & 0xffffff, 0xffffff);
            assert_eq!(
                c.hit(20.0 + 200.0 - 24.0, 10.0 + 8.0 + 100.0 + 24.0),
                Some(CardPart::Close)
            );
            assert_eq!(c.hit(100.0, 60.0), Some(CardPart::Card));
            assert_eq!(c.hit(5.0, 5.0), None);
        }
        assert_ne!(frames[0], frames[1], "the selection ring draws");
    }

    #[test]
    fn grid_columns_tile_the_width() {
        let (x0, w) = TabCard::column(32.0, 1000.0, 4, 16.0, 0);
        let (x3, w3) = TabCard::column(32.0, 1000.0, 4, 16.0, 3);
        assert_eq!(x0, 32.0);
        assert!((x3 + w3 - 1032.0).abs() < 0.02 && (w - w3).abs() < 0.02);
        let q = |v: f32| (v * 100.0).round() as i32;
        let inside = |x: f32, w: f32| {
            crate::rules::fits(
                crate::rules::Box2 {
                    x: q(x),
                    y: 1000,
                    w: q(w),
                    h: 5000,
                },
                q(32.0),
                0,
                q(1000.0),
                10000,
            )
        };
        assert!(inside(x3, w3));
        assert!(!inside(x3 + 10.0, w3));
    }
}
