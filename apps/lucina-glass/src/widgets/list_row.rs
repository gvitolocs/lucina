//! A list row: an optional icon, a title, an optional value and chevron; the selected row is the one accent.

use crate::Paint;
use crate::canvas::R;
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, space};

use super::ink::{Palette, focus_ring, tap, text_left, text_right};

#[derive(Clone, Debug, Default)]
pub struct ListRow {
    pub icon: Option<Icon>,
    pub title: String,
    pub value: Option<String>,
    pub chevron: bool,
    pub selected: bool,
    pub focused: bool,
    pub min_hit: f32,
    last: R,
}

impl ListRow {
    pub fn new(icon: Option<Icon>, title: &str) -> ListRow {
        ListRow {
            icon,
            title: title.to_string(),
            min_hit: space::TAP,
            ..ListRow::default()
        }
    }

    pub fn value(mut self, v: &str) -> ListRow {
        self.value = Some(v.to_string());
        self
    }

    pub fn chevron(mut self) -> ListRow {
        self.chevron = true;
        self
    }

    pub fn selected(mut self, on: bool) -> ListRow {
        self.selected = on;
        self
    }

    /// Draw it in `r` with the text style `main`, in the inks `pal`.
    pub fn draw(&mut self, p: &mut Paint, r: R, main: (f32, f32), pal: &Palette) {
        self.last = r;
        if self.selected {
            p.cv.fill_rrect(r, radius::MD, color::ACCENT_SOFT);
        }
        let mut x = r.x + 12.0;
        if let Some(i) = self.icon {
            icons::draw(
                p.cv,
                i,
                x + 10.0,
                r.cy(),
                20.0,
                if self.selected {
                    color::ACCENT
                } else {
                    pal.ink
                },
            );
            x += 20.0 + 14.0;
        }
        let mut right = r.right() - 12.0;
        if self.chevron {
            icons::draw(
                p.cv,
                Icon::ChevronRight,
                right - 6.0,
                r.cy(),
                18.0,
                pal.ink2,
            );
            right -= 22.0;
        }
        if let Some(v) = &self.value {
            text_right(p, Font::Regular, main, right, r.cy(), pal.ink2, v);
            right -= p.measure(Font::Regular, main, v) + 12.0;
        }
        let t = p.fit(Font::Regular, main, &self.title, right - x);
        text_left(p, Font::Regular, main, x, r.cy(), pal.ink, &t);
        if self.focused {
            focus_ring(p.cv, r, radius::MD);
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> bool {
        tap(self.last, self.min_hit, x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{Canvas, Color};
    use crate::text::Type;

    #[test]
    fn selected_rows_get_the_accent_fill_and_long_titles_are_cut() {
        let mut ty = Type::new();
        let draw = |ty: &mut Type, row: &mut ListRow| {
            let mut cv = Canvas::new(300, 60, 1.0);
            cv.clear(Color::hex(0x101010));
            let mut p = Paint {
                cv: &mut cv,
                ty,
                now: 0.0,
                backdrop: None,
            };
            row.draw(
                &mut p,
                R::new(10.0, 10.0, 280.0, 40.0),
                (15.0, 0.0),
                &Palette::STANDARD,
            );
            cv
        };
        let mut plain = ListRow::new(Some(Icon::Gear), "Settings")
            .chevron()
            .value("On");
        let a = draw(&mut ty, &mut plain);
        let mut sel = ListRow::new(Some(Icon::Gear), "Settings")
            .chevron()
            .value("On")
            .selected(true);
        let b = draw(&mut ty, &mut sel);
        assert_ne!(
            a.px[30 * 300 + 200],
            b.px[30 * 300 + 200],
            "the fill shows between the text"
        );
        let mut long = ListRow::new(
            None,
            &"A very long title that cannot fit in the row at all ".repeat(3),
        )
        .chevron();
        let c = draw(&mut ty, &mut long);
        assert!(c.px.iter().any(|&v| v != 0x101010));
        assert!(
            (0..60).all(|y| c.px[y * 300 + 295] == 0x101010),
            "nothing spills past the row"
        );
        assert!(plain.hit(150.0, 30.0) && !plain.hit(150.0, 100.0));
    }
}
