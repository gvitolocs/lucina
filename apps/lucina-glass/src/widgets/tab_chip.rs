//! A tab chip: a site mark (or a spinner while loading), the page's title and a close cross in a pill; the active tab is ringed
//! in the accent. Also the site mark itself, a quiet disc with the site's initial (no third-party marks, no decoration).

use crate::Paint;
use crate::browser_rules;
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::text::Font;
use crate::tokens::{color, radius, space};

use super::chrome::spinner;
use super::ink::{Palette, focus_ring, tap, text_centre, text_left};

/// Hundredths of a dp held to the range `browser_rules` takes, whatever the float (`f32::clamp` leaves a NaN as it is and the `as i32` that follows turns it into 0; the tests with NaN inputs pin this).
fn q(v: f32) -> i32 {
    (v * 100.0).round().clamp(
        -(browser_rules::MAX_LEN as f32),
        browser_rules::MAX_LEN as f32,
    ) as i32
}

/// A quiet disc of diameter `d` with the host's initial: a site's mark when no icon was fetched.
pub fn site_mark(p: &mut Paint, cx: f32, cy: f32, d: f32, host: &str, ink: Color) {
    p.cv.fill_circle(cx, cy, d / 2.0, ink.alpha(0.16));
    let initial: String = host
        .chars()
        .next()
        .map(|c| c.to_ascii_uppercase().to_string())
        .unwrap_or_default();
    text_centre(p, Font::Semibold, (d * 0.55, 0.0), cx, cy, ink, &initial);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPart {
    Close,
    Tab,
}

#[derive(Clone, Debug)]
pub struct TabChip {
    pub title: String,
    pub host: String,
    pub loading: bool,
    pub active: bool,
    pub focused: bool,
    pub min_hit: f32,
    last: R,
}

impl TabChip {
    pub fn new(title: &str, host: &str) -> TabChip {
        TabChip {
            title: title.to_string(),
            host: host.to_string(),
            loading: false,
            active: false,
            focused: false,
            min_hit: space::TAP,
            last: R::default(),
        }
    }

    /// The width of each of `n` chips over `avail` dp with `gap` between, at most `max`: they all fit
    /// ([`browser_rules::tab_width`]).
    pub fn width(avail: f32, n: usize, gap: f32, max: f32) -> f32 {
        let n = n.clamp(1, 64) as u32;
        let avail = q(avail).max(0);
        // The gaps may not take more than the strip: each is at most an equal share of it (the rule's `gap * (n - 1) <= avail`).
        let gap = if n > 1 {
            q(gap).max(0).min(avail / (n as i32 - 1))
        } else {
            q(gap).max(0)
        };
        browser_rules::tab_width(avail, n, gap, q(max).max(0)) as f32 / 100.0
    }

    /// Draw it in `r` (as tall as the strip's controls) with the text style `st`, in the inks `pal` of its bar.
    pub fn draw(&mut self, p: &mut Paint, r: R, st: (f32, f32), pal: &Palette) {
        self.last = r;
        let cy = r.cy();
        if self.active {
            p.cv.fill_rrect(r, radius::PILL, color::ACCENT_SOFT);
            p.cv.stroke_rrect(r, radius::PILL, 1.2, color::ACCENT);
        }
        let mx = r.x + 10.0 + 9.0;
        if self.loading {
            spinner(p, mx, cy, 8.0, pal.ink2);
        } else {
            site_mark(p, mx, cy, 18.0, &self.host, pal.ink);
        }
        let tx = mx + 9.0 + 8.0;
        let room = r.right() - 32.0 - tx;
        let title = p.fit(Font::Regular, st, &self.title, room);
        text_left(
            p,
            Font::Regular,
            st,
            tx,
            cy,
            if self.active { pal.ink } else { pal.ink2 },
            &title,
        );
        icons::draw(
            p.cv,
            Icon::Close,
            r.right() - 17.0,
            cy,
            16.0,
            if self.active { pal.ink2 } else { pal.ink3 },
        );
        if self.focused {
            focus_ring(p.cv, r, r.h / 2.0);
        }
    }

    /// The close cross's area: its 16 dp mark grown to the chip's height (the most that does not take the neighbour's).
    fn close_area(&self) -> R {
        R::new(
            self.last.right() - 17.0 - 8.0,
            self.last.cy() - 8.0,
            16.0,
            16.0,
        )
    }

    /// The part under (`x`, `y`) as last drawn: the close cross first, else the chip.
    pub fn hit(&self, x: f32, y: f32) -> Option<TabPart> {
        if tap(self.close_area(), self.min_hit.min(self.last.h), x, y) && self.last.contains(x, y) {
            Some(TabPart::Close)
        } else if tap(self.last, self.min_hit, x, y) {
            Some(TabPart::Tab)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::Canvas;
    use crate::text::Type;

    #[test]
    fn widths_fit_the_strip() {
        let w = TabChip::width(1000.0, 5, 4.0, 210.0);
        assert_eq!(w, 196.8, "(1000 - 4 x 4) / 5");
        assert_eq!(TabChip::width(1000.0, 2, 4.0, 210.0), 210.0, "capped");
        for n in 1..=12 {
            let w = TabChip::width(700.0, n, 4.0, 230.0);
            assert!(
                w * n as f32 + 4.0 * (n as f32 - 1.0) <= 700.0 + 0.001,
                "n = {n}"
            );
        }
        assert!(
            TabChip::width(10.0, 8, 4.0, 200.0) >= 0.0,
            "a strip too narrow for the gaps still answers"
        );
        // A huge gap, a huge strip and non-numbers meet the rule's limits: the tabs still fit.
        assert_eq!(
            TabChip::width(1000.0, 5, 1.0e6, 210.0),
            0.0,
            "gaps that would take the whole strip leave no width, not a wrong one"
        );
        let w = TabChip::width(1.0e9, 4, 2.0, 210.0);
        assert!((0.0..=210.0).contains(&w), "{w}");
        assert!(TabChip::width(f32::NAN, 3, f32::INFINITY, f32::NAN) >= 0.0);
    }

    #[test]
    fn draws_each_state_and_tells_close_from_tab() {
        let mut ty = Type::new();
        let mut counts = Vec::new();
        for (active, loading) in [(false, false), (true, false), (true, true)] {
            let mut cv = Canvas::new(300, 80, 1.0);
            cv.clear(Color::hex(0));
            let mut p = Paint {
                cv: &mut cv,
                ty: &mut ty,
                now: 0.5,
                backdrop: None,
            };
            let mut t = TabChip::new("Example Domain with a long title to cut", "example.org");
            t.active = active;
            t.loading = loading;
            t.draw(
                &mut p,
                R::new(20.0, 20.0, 210.0, 40.0),
                (13.0, 0.0),
                &Palette::STANDARD,
            );
            counts.push(cv.px.iter().filter(|&&v| v != 0).count());
            assert_eq!(t.hit(20.0 + 210.0 - 17.0, 40.0), Some(TabPart::Close));
            assert_eq!(t.hit(100.0, 40.0), Some(TabPart::Tab));
            assert_eq!(t.hit(250.0, 40.0), None);
        }
        assert!(counts[1] > counts[0], "the active tab is ringed");
    }

    #[test]
    fn site_mark_writes_the_initial() {
        let mut ty = Type::new();
        let mut cv = Canvas::new(40, 40, 1.0);
        cv.clear(Color::hex(0));
        let mut p = Paint {
            cv: &mut cv,
            ty: &mut ty,
            now: 0.0,
            backdrop: None,
        };
        site_mark(&mut p, 20.0, 20.0, 24.0, "notes.example.org", color::INK);
        assert!(cv.px.iter().any(|&v| v != 0));
        assert!(
            cv.px.iter().filter(|&&v| v & 0xffffff > 0x404040).count() > 5,
            "the initial is brighter than the disc"
        );
    }
}
