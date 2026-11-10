//! The calendar grid of rounded tiles: day numbers, app glyphs, notification dots and one wide tile with ranges and a battery.
//! Tiles are placed by the proved row rule ([`rules::cells`]) in a fixed number of columns, flowing left to right. On entrance
//! each tile rises, fades and grows in, [`rules::stagger`] apart; then its dots pop in with an overshoot.

use crate::anim::{Ease, Secs, phase};
use crate::canvas::{Color, R};
use crate::icons::{self, Icon};
use crate::rules;
use crate::text::Font;
use crate::tokens::{color, motion, radius};
use crate::Paint;

use super::{CountUp, cell_x, cell_y};

/// What a tile shows.
#[derive(Clone)]
pub enum TileKind {
    /// A day: its number, an optional glyph, dots (accent or signal colours).
    Day { n: u32, icon: Option<Icon>, dots: Vec<Color> },
    /// The wide summary tile: a range text and a battery-style percentage.
    /// `bars`: the day's load per hour, 0..1, drawn as rising bars (the busiest in the accent).
    Summary { ranges: String, percent: CountUp, bars: Vec<f32> },
    /// An empty tile (a gap in the month), drawn faint.
    Blank,
}

#[derive(Clone)]
pub struct Tile {
    pub kind: TileKind,
    /// Columns it spans.
    pub span: u32,
    /// The light ("paper") tile: today.
    pub light: bool,
}

impl Tile {
    pub fn day(n: u32, icon: Option<Icon>, dots: &[Color]) -> Tile {
        Tile { kind: TileKind::Day { n, icon, dots: dots.to_vec() }, span: 1, light: false }
    }
}

#[derive(Clone)]
pub struct CalendarGrid {
    pub tiles: Vec<Tile>,
    pub cols: u32,
    pub rows: u32,
    /// When the entrance started.
    pub appear: Secs,
    pub gap: f32,
}

/// How long one tile's entrance takes.
const IN: f64 = 0.46;
/// The dots pop this long after their tile starts.
const DOT_DELAY: f64 = 0.22;

impl CalendarGrid {
    pub fn new(cols: u32, rows: u32, tiles: Vec<Tile>, appear: Secs) -> CalendarGrid {
        CalendarGrid { tiles, cols: cols.max(1), rows: rows.max(1), appear, gap: 7.0 }
    }

    /// Play the entrance again from `now`.
    pub fn replay(&mut self, now: Secs) {
        self.appear = now;
        for t in &mut self.tiles {
            if let TileKind::Summary { percent, .. } = &mut t.kind {
                let target = percent.target;
                percent.restart(now + 0.35, target);
            }
        }
    }

    /// Where each tile goes: (index, rectangle), flowing left to right, wrapping at `cols`.
    pub fn layout(&self, r: R) -> Vec<(usize, R)> {
        let mut out = Vec::new();
        let (mut col, mut row) = (0u32, 0u32);
        for (i, t) in self.tiles.iter().enumerate() {
            let span = t.span.clamp(1, self.cols);
            if col + span > self.cols {
                col = 0;
                row += 1;
            }
            if row >= self.rows {
                break;
            }
            let (x, w) = cell_x(r, self.cols, self.gap, col, span);
            let (y, h) = cell_y(r, self.rows, self.gap, row, 1);
            out.push((i, R::new(x, y, w, h)));
            col += span;
        }
        out
    }

    pub fn draw(&mut self, p: &mut Paint, r: R) {
        let now = p.now;
        let layout = self.layout(r);
        for (order, &(i, tr)) in layout.iter().enumerate() {
            let delay = rules::stagger(order as u32, motion::STAGGER_MS, motion::STAGGER_CAP_MS) as f64 / 1000.0;
            let start = self.appear + delay;
            let k = Ease::OutCubic.at(phase(now, start, IN));
            if k <= 0.0 {
                continue;
            }
            let o = p.cv.opacity;
            p.cv.opacity = o * k;
            let tr = tr.scale_about_center(0.92 + 0.08 * k).offset(0.0, 16.0 * (1.0 - k));
            let pop = Ease::OutBack(2.4).at(phase(now, start + DOT_DELAY, 0.36));
            draw_tile(p, &self.tiles[i], tr, pop);
            p.cv.opacity = o;
        }
    }
}

fn draw_tile(p: &mut Paint, t: &Tile, r: R, pop: f32) {
    let (bg, ink, ink2) = if t.light {
        (color::PAPER, color::PAPER_INK, Color::hexa(0x111214, 150))
    } else {
        (color::CARBON, color::INK, color::INK_2)
    };
    if t.light {
        p.cv.fill_rrect(r, radius::MD, bg);
    } else {
        // Dark glass: the backdrop shows faintly through.
        p.glass(r, radius::MD, Color::hexa(0x121316, 196));
    }
    match &t.kind {
        TileKind::Blank => {}
        TileKind::Day { n, icon, dots } => {
            p.text(Font::Light, (17.0, 0.0), r.x + 10.0, r.y + 24.0, ink, &n.to_string());
            if let Some(ic) = icon {
                icons::draw(p.cv, *ic, r.x + 10.0 + 9.0, r.bottom() - 10.0 - 9.0, 18.0, ink2);
            }
            // Dots in the top-right corner, popping in.
            for (j, c) in dots.iter().enumerate() {
                let rad = 3.6 * pop.max(0.0);
                if rad > 0.05 {
                    let cx = r.right() - 11.0 - j as f32 * 10.0;
                    p.cv.glow(cx, r.y + 11.0, rad * 3.0, c.alpha(0.35));
                    p.cv.fill_circle(cx, r.y + 11.0, rad, *c);
                }
            }
        }
        TileKind::Summary { ranges, percent, bars } => {
            p.text(Font::Thin, (26.0, -0.4), r.x + 11.0, r.y + 34.0, ink, ranges);
            // The load, counted up (one value: no battery glyph repeating it, rule 4).
            let v = percent.value(p.now);
            percent.draw(p, r.right() - 12.0, r.bottom() - 12.0, Font::Thin, 26.0, ink);
            // Bar glyphs: a week of load, rising in with the count.
            let k = v as f32 / percent.target.max(1) as f32;
            let top = bars.iter().cloned().fold(0.0f32, f32::max);
            for (j, b) in bars.iter().enumerate() {
                let hgt = b.clamp(0.0, 1.0) * 22.0 * k;
                let x = r.x + 12.0 + j as f32 * 7.0;
                let busiest = top > 0.0 && *b >= top;
                p.cv.fill_rrect(R::new(x, r.bottom() - 12.0 - hgt, 3.5, hgt.max(1.0)), 1.5, if busiest { color::ACCENT } else { ink2 });
            }
        }
    }
}
