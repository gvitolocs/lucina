//! The phone screens' parts as blocks that fit any rectangle, for the larger layouts: each takes its widgets and a box.
//! The same rules hold everywhere (docs/design/rules.md): one fact per card, icons over words, glass surfaces.

use crate::canvas::{Canvas, R};
use crate::icons::Icon;
use crate::screens::agenda;
use crate::screens::content as c;
use crate::text::Font;
use crate::tokens::{color, radius, space, ty};
use crate::widgets::calendar::{Tile, TileKind};
use crate::widgets::card_stack::Card;
use crate::widgets::chrome;
use crate::widgets::{
    ActionButton, CalendarGrid, CardStack, CountUp, DayChips, LivePill, MediaControls, PagerDots, ProgressTrack, RoundButton, Segmented,
    TimerRing, Volume, cell_x,
};
use crate::Paint;

const ACTION_ICONS: [Icon; 4] = [Icon::Doc, Icon::Mail, Icon::Clock, Icon::Share];

/// Every widget the variants show, made once.
pub struct Parts {
    pub live: LivePill,
    pub track: ProgressTrack,
    pub percent: CountUp,
    pub seg: Segmented,
    pub actions: [ActionButton; 4],
    pub timer: TimerRing,
    pub media: MediaControls,
    pub volume: Volume,
    pub round: [RoundButton; 2],
    pub arrow: RoundButton,
    pub days: DayChips,
    pub grid: CalendarGrid,
    pub stack: CardStack,
    pub pager: PagerDots,
    pub notify_actions: [ActionButton; 2],
}

impl Parts {
    pub fn new(now: f64, cols: u32, rows: u32) -> Parts {
        Parts {
            live: LivePill::new(c::LIVE, now),
            track: ProgressTrack::new(3, 2, 0.76),
            percent: CountUp::new(c::PHASE_PERCENT, now, "%"),
            seg: Segmented::new(&c::SEGMENTS, 0),
            actions: std::array::from_fn(|i| ActionButton::new(ACTION_ICONS[i], c::ACTIONS[i])),
            timer: TimerRing::new(c::TIMER_TOTAL, c::TIMER_LEFT, now),
            media: MediaControls::new(true),
            volume: Volume::new(0.62),
            round: [RoundButton::new(Icon::Heart), RoundButton::new(Icon::Repeat)],
            arrow: RoundButton::new(Icon::ArrowUpRight),
            days: DayChips::new(&c::DAYS, c::DAY_SELECTED),
            grid: CalendarGrid::new(cols, rows, tiles(now, cols, cols * rows), now),
            stack: CardStack::new(c::CARDS.iter().map(|&(f, s)| Card { fact: f.into(), secondary: s.into(), urgent: true }).collect()),
            pager: PagerDots::new(c::PAGES, c::PAGE),
            notify_actions: [ActionButton::new(Icon::Upload, c::NOTIFY_ACTIONS[0]), ActionButton::new(Icon::Clock, c::NOTIFY_ACTIONS[1])],
        }
    }
}

/// The month's tiles from the 14th, with the two-column summary tile opening the second row of a `cols`-column grid,
/// `n` cells' worth.
pub fn tiles(now: f64, cols: u32, n: u32) -> Vec<Tile> {
    let (l, s) = (color::LIVE, color::SIGNAL);
    let icons = [Some(Icon::Pen), Some(Icon::Mail), Some(Icon::Ball), Some(Icon::Send), Some(Icon::Doc), None];
    let mut v = Vec::new();
    let mut day = 14;
    let mut used = 0;
    while used < n {
        if used == cols && cols >= 2 {
            v.push(Tile { kind: TileKind::Summary { ranges: c::RANGES.into(), percent: CountUp::new(c::LOAD_PERCENT, now + 0.35, "%"), bars: c::LOAD_BARS.to_vec() }, span: 2, light: false });
            used += 2;
            continue;
        }
        let dots: &[crate::canvas::Color] = match day % 5 {
            0 => &[l],
            1 => &[s, l],
            3 => &[s],
            _ => &[],
        };
        v.push(Tile { light: day == 15, ..Tile::day(day, icons[(day as usize) % icons.len()], dots) });
        day += 1;
        used += 1;
    }
    v
}

/// Build's header card: LIVE, the thin title at `title` dp, the phase line with its track and percentage.
pub fn build_header(p: &mut Paint, r: R, parts: &mut Parts, title: f32) {
    p.glass(r, radius::XL, color::MATERIAL);
    let (lw, _) = parts.live.size(p);
    parts.live.draw(p, r.right() - space::L - lw, r.y + space::L);
    p.text(Font::Thin, (title, -title * 0.026), r.x + 10.0, r.y + 18.0 + title * 0.95, color::INK, c::BUILD_TITLE);
    p.text(Font::Regular, ty::CAPTION, r.x + space::L, r.bottom() - 22.0, color::INK_2, c::PHASE);
    let pw = p.measure(Font::Regular, ty::CAPTION, c::PHASE);
    parts.track.draw(p, R::new(r.x + space::L + pw + space::M, r.bottom() - 31.0, r.w - pw - 124.0, 10.0));
    parts.percent.draw(p, r.right() - space::L, r.bottom() - 18.0, Font::Thin, 34.0, color::INK);
}

/// The row of four actions across `r` (64 dp tall).
pub fn actions(p: &mut Paint, r: R, parts: &mut Parts) {
    // A soft dark band behind the row, so labels keep their contrast over a bright backdrop (HIG Materials: dim under
    // clear glass where the content is bright).
    if p.backdrop.is_some() {
        p.cv.fill_rrect_soft(r.inset_xy(-8.0, -4.0), radius::LG, 14.0, crate::canvas::Color::hexa(0x000000, 170));
    }
    for (i, b) in parts.actions.iter_mut().enumerate() {
        let (x, w) = cell_x(r, 4, space::M, i as u32, 1);
        b.draw(p, x + w / 2.0, r.y);
    }
}

/// A keyboard focus ring around `r`: the accent, 3 dp outside.
pub fn focus_ring(cv: &mut Canvas, r: R, rad: f32) {
    cv.stroke_rrect(r.inset(-3.0), rad + 3.0, 1.6, color::ACCENT);
}

/// The sheet's contents as one card: upload, timer and ring, media pill, volume and the two toggles.
pub fn player(p: &mut Paint, r: R, parts: &mut Parts) {
    p.glass(r, radius::LG, color::SMOKE);
    let x = r.x + space::L + 4.0;
    chrome::spinner(p, x + 6.0, r.y + 26.0, 6.0, color::INK);
    p.text(Font::Regular, ty::CAPTION, x + 20.0, r.y + 30.0, color::INK_2, c::UPLOADING);
    parts.timer.draw_digits(p, x - 2.0, r.y + 88.0, 44.0, color::INK);
    parts.timer.draw_ring(p, r.right() - 52.0, r.y + 62.0, 30.0);
    parts.media.draw(p, R::new(r.x + space::L, r.y + 110.0, r.w - 2.0 * space::L, 50.0));
    parts.volume.draw(p, R::new(x, r.y + 174.0, r.w - 140.0, 24.0));
    parts.round[0].draw(p, r.right() - 76.0, r.y + 186.0, 18.0);
    parts.round[1].draw(p, r.right() - 34.0, r.y + 186.0, 18.0);
}

/// The agenda's header, titled with the device's name.
pub fn agenda_header(p: &mut Paint, r: R, parts: &mut Parts, device: &str) {
    agenda::header(p, r, device, c::QUEUE, &mut parts.arrow);
}

/// Two stat cards side by side (or stacked when `r` is tall): views and teams.
pub fn stats(p: &mut Paint, r: R) {
    let tall = r.h > r.w;
    for i in 0..2u32 {
        let b = if tall { super::pane(r, 1, 2, space::M, 0, i, 1, 1) } else { super::pane(r, 2, 1, space::M, i, 0, 1, 1) };
        let (icon, (cap, val)) = if i == 0 { (Icon::Person, c::ACTIVITY) } else { (Icon::Send, c::TEAMS) };
        chrome::stat_tile(p, b, icon, cap, val);
    }
}

/// "Notification", the stack, the pager and the two actions, in `r`; returns the bottom.
pub fn notifications(p: &mut Paint, r: R, parts: &mut Parts, title: f32) -> f32 {
    p.text(Font::Thin, (title, -title * 0.03), r.x - 2.0, r.y + title * 0.8, color::INK, c::NOTIFY_TITLE);
    let sy = r.y + title * 0.8 + 22.0;
    parts.stack.draw(p, R::new(r.x, sy, r.w, 96.0));
    let row = sy + 96.0 + 40.0;
    parts.pager.draw(p, r.x, row + 22.0);
    let (aw, ah) = ActionButton::size();
    parts.notify_actions[0].draw(p, r.right() - aw * 1.5 - space::S, row);
    parts.notify_actions[1].draw(p, r.right() - aw / 2.0, row);
    row + ah
}
