//! Screen 2, the agenda, titled with the device's name ("Diana"): white stairs in space behind a glass header (title,
//! an arrow, one secondary line with a spinner), a profile chip, a reminder card, day chips and the calendar grid.

use crate::anim::Secs;
use crate::canvas::{Canvas, Color, R};
use crate::icons::Icon;
use crate::imagery;
use crate::text::{Font, Type};
use crate::tokens::{color, radius, space, ty};
use crate::widgets::calendar::{Tile, TileKind};
use crate::widgets::chrome;
use crate::widgets::{CalendarGrid, CountUp, DayChips, RoundButton};
use crate::Paint;

use super::{Backdrop, Pointer, Screen, content as c, home_indicator};

pub struct Agenda {
    bg: Option<Backdrop>,
    pub days: DayChips,
    pub grid: CalendarGrid,
    arrow: RoundButton,
    pressed: bool,
}

fn tiles(now: Secs) -> Vec<Tile> {
    let (l, s) = (color::LIVE, color::SIGNAL);
    vec![
        Tile::day(14, Some(Icon::Pen), &[l]),
        Tile { light: true, ..Tile::day(15, Some(Icon::Mail), &[s, l]) },
        Tile::day(16, Some(Icon::Ball), &[]),
        Tile::day(17, Some(Icon::Send), &[s]),
        Tile { kind: TileKind::Summary { ranges: c::RANGES.into(), percent: CountUp::new(c::LOAD_PERCENT, now + 0.35, "%"), bars: c::LOAD_BARS.to_vec() }, span: 2, light: false },
        Tile::day(18, Some(Icon::Doc), &[l]),
        Tile::day(19, None, &[]),
        Tile::day(20, Some(Icon::Pen), &[]),
        Tile::day(21, Some(Icon::Mail), &[s]),
        Tile::day(22, None, &[l, s]),
        Tile::day(23, Some(Icon::Send), &[]),
        Tile::day(24, Some(Icon::Ball), &[]),
        Tile::day(25, Some(Icon::Doc), &[l]),
        Tile::day(26, None, &[s]),
        Tile::day(27, Some(Icon::Pen), &[]),
    ]
}

/// A header card: the title in the display face, an arrow, and one secondary line with a spinner.
pub fn header(p: &mut Paint, r: R, title: &str, secondary: &str, arrow: &mut RoundButton) {
    p.glass(r, radius::XL, color::MATERIAL);
    p.text(Font::Thin, ty::TITLE, r.x + space::L, r.y + 46.0, color::INK, title);
    arrow.draw(p, r.right() - 34.0, r.y + 32.0, 18.0);
    p.text(Font::Regular, ty::CAPTION, r.x + space::L, r.bottom() - 18.0, color::INK_2, secondary);
    chrome::spinner(p, r.right() - 28.0, r.bottom() - 22.0, 6.0, color::INK);
}

impl Agenda {
    pub fn new(now: Secs) -> Agenda {
        Agenda {
            bg: None,
            days: DayChips::new(&c::DAYS, c::DAY_SELECTED),
            grid: CalendarGrid::new(4, 4, tiles(now), now),
            arrow: RoundButton::new(Icon::ArrowUpRight),
            pressed: false,
        }
    }
}

impl Screen for Agenda {
    fn name(&self) -> &'static str {
        "agenda"
    }

    fn enter(&mut self, now: Secs) {
        self.grid.replay(now + 0.1);
    }

    fn act(&mut self, now: Secs, action: &str) -> bool {
        if let Some(d) = action.strip_prefix("select-") {
            if let Some(i) = c::DAYS.iter().position(|x| x.eq_ignore_ascii_case(d)) {
                self.days.select(now, i);
                return true;
            }
            return false;
        }
        match action {
            "replay" => self.grid.replay(now),
            _ => return false,
        }
        true
    }

    fn pointer(&mut self, now: Secs, ev: Pointer) -> bool {
        match ev {
            Pointer::Down(x, y) => {
                if let Some(i) = self.days.hit(x, y) {
                    self.days.select(now, i);
                    return true;
                }
                if self.arrow.hit(x, y) {
                    self.arrow.press(now);
                    self.pressed = true;
                    return true;
                }
                false
            }
            Pointer::Up(_, _) => {
                if std::mem::take(&mut self.pressed) {
                    self.arrow.release(now);
                    self.arrow.on = false;
                    self.grid.replay(now);
                    return true;
                }
                false
            }
            Pointer::Move(..) => false,
        }
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        Backdrop::ensure(&mut self.bg, cv, imagery::stairs_in_space);
        let bg = self.bg.as_ref().expect("backdrop");
        cv.px.copy_from_slice(&bg.image.px);
        let full = cv.bounds();
        let mut p = Paint { cv, ty, now, backdrop: Some(&bg.blurred) };
        chrome::status_bar(&mut p, R::new(0.0, 0.0, full.w, 46.0), color::INK);
        let x = space::EDGE;
        let w = full.w - 2.0 * space::EDGE;
        header(&mut p, R::new(x, 52.0, w, 100.0), c::PHONE, c::QUEUE, &mut self.arrow);
        chrome::profile_chip(&mut p, R::new(x, 164.0, 220.0, 52.0), c::PERSON.0, c::PERSON.1, c::PERSON.2);
        chrome::reminder_card(&mut p, R::new(x, 228.0, w, 80.0), c::REMINDER, color::WARNING);
        self.days.draw(&mut p, R::new(x, 322.0, w, 36.0));
        let grid = R::new(x, 372.0, w, full.h - 372.0 - 24.0);
        p.cv.fill_rrect_soft(grid.inset(-6.0), radius::XL, 16.0, Color::hexa(0x000000, 90));
        self.grid.draw(&mut p, grid);
        home_indicator(p.cv, color::INK);
    }
}
