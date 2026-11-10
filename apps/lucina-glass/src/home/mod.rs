//! Juno's home: the approved laptop concept (`docs/design/phone-concept.md`, "Variants", Juno) as the real desktop, drawn from
//! [`HomeData`] and nothing else.
//!
//! - **Layout.** Three columns over 1280 x 720 dp (1920 x 1080 at 1.5 px/dp) and a dock. Cards stack down each column through the
//!   proved [`home_rules::stack`]; a card whose data is missing is left out and the ones after it move up.
//! - **Frames.** Every part of the screen is a [`Region`] with the time its motion ends. [`Home::damage`] lists only what changed or
//!   moves, [`Home::next_frame`] says when the next frame is due through the proved [`home_rules::next_frame`], and it is never:
//!   with nothing moving, nobody draws and the CPU sleeps. What moves forever (the live pill, spinners) runs at a slower rate and
//!   rests [`AMBIENT_FOR`] after the last input.
//! - **Input.** Arrows move a focus ring to the nearest item that way ([`home_rules::arrow_score`]); Enter does it. The pointer
//!   hovers and presses. Both return an [`Outcome`] for the shell to carry out (open an app, change the volume).
//!
//! The sixteen animations of the concept, and where they run here:
//! action-button (hover, press), calendar-stagger (entrance, another week), card-stack (Clear, paging the stack), day-chips (choosing a
//! day), live-pill (Build running), media-press (Play), notification-enter (the title on entrance, a new notification arriving),
//! pager-dots (paging the stack), phase-advance (the next phase starts), progress-count-up (the percentage and the booked share),
//! screen-forward and screen-back (the shell's move to the apps page and back: [`slide_compose`]), segmented (Work and Lucina),
//! sheet-rise (the dock rising and the stat cards fading up on entrance), timer-ring (the countdown to the next event) and
//! vertical-slider (the shell's brightness and volume panel: [`crate::widgets::chrome::VSlider`]).

pub mod data;
#[cfg(any(test, feature = "fixture"))]
pub mod fixture;

use crate::Paint;
use crate::anim::{Ease, Secs, Tween, phase};
use crate::canvas::{Canvas, Color, R};
use crate::home_rules as hr;
use crate::icons::{self, Icon};
use crate::text::{Font, Type};
use crate::tokens::{color, motion, radius, space, ty};
use crate::widgets::calendar::{Tile, TileKind};
use crate::widgets::card_stack::Card;
use crate::widgets::{
    ActionButton, CalendarGrid, CardStack, CountUp, DayChips, LivePill, PagerDots, ProgressTrack,
    RoundButton, Segmented, TimerRing, Volume, baseline_for, cell_x, chrome,
};

pub use data::*;

/// The screen in dp, and pixels per dp, on the laptop's 1920 x 1080 panel.
pub const W: f32 = 1280.0;
pub const H: f32 = 720.0;
pub const K: f32 = 1.5;

/// How long ambient motion (the live pill, spinners, the timer's roll) runs after the last input, seconds.
pub const AMBIENT_FOR: Secs = 60.0;
/// The ambient frame time, seconds: 30 per second is plenty for a pill that breathes over 1.6 s.
pub const AMBIENT_FRAME: Secs = 1.0 / 30.0;
/// How far drawing may spill outside a region (halos, shadows, overshoot), dp.
const SPILL: f32 = 14.0;

/// The parts of the screen that are redrawn on their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rg {
    Status,
    Build,
    Seg,
    Actions,
    StatsLeft,
    Device,
    Person,
    Deadline,
    Days,
    Grid,
    StatsRight,
    Title,
    Stack,
    Pager,
    NoteActions,
    Upload,
    Timer,
    Media,
    Volume,
    /// The live pill alone: it breathes forever, so only its own few pixels are drawn at the ambient rate.
    Live,
    /// The calendar's summary tile alone: choosing a day counts its share there, and nothing else in the grid moves.
    Summary,
    /// The device card's spinner alone.
    Spin,
    /// The upload's spinner alone.
    UploadSpin,
}

/// One region: where it is and until when it moves.
#[derive(Clone, Copy, Debug)]
pub struct Region {
    pub id: Rg,
    pub r: R,
    pub until: Secs,
    /// Moves forever while shown (the ambient rate).
    pub ambient: bool,
    pub dirty: bool,
}

/// What a key or click asks the shell to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    None,
    /// Something on the screen changed: draw.
    Redraw,
    Open(Target),
    /// Play or pause the media in this app.
    Media(String),
    Volume(f32),
    /// A notification was cleared.
    Dismissed(String),
    /// The key is not the home's: the shell may use it.
    Unused,
}

/// The keys the home answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Delete,
    Esc,
}

/// A pointer event, in dp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    Move(f32, f32),
    Down(f32, f32),
    Up(f32, f32),
}

/// Something the focus can rest on.
#[derive(Clone, Debug, PartialEq)]
enum Item {
    Build,
    Segment(usize),
    Action(usize),
    StatLeft(usize),
    /// Line `1` of left stat card `0`.
    StatLine(usize, usize),
    Device,
    Deadline,
    Day(usize),
    StatRight(usize),
    Stack,
    NoteAction(usize),
    Media,
    Volume,
}

/// Where everything goes for the data present.
#[derive(Clone, Debug, Default, PartialEq)]
struct Layout {
    status: R,
    build: Option<R>,
    seg: Option<R>,
    actions: Option<R>,
    stats_left: Option<R>,
    device: Option<R>,
    person: Option<R>,
    deadline: Option<R>,
    days: Option<R>,
    grid: Option<R>,
    panel: Option<R>,
    stats_right: Option<R>,
    title: Option<R>,
    stack: Option<R>,
    pager: Option<R>,
    note_actions: Option<R>,
    dock: Option<R>,
    upload: Option<R>,
    timer: Option<R>,
    media: Option<R>,
    volume: Option<R>,
}

/// The play button of the media pill: presses like the concept's controls, play and pause cross-fading as they swap.
#[derive(Clone)]
struct MediaPill {
    press: Tween,
    swap: Tween,
}

#[derive(Clone)]
pub struct Home {
    pub data: HomeData,
    layout: Layout,
    regions: Vec<Region>,
    items: Vec<(Item, R)>,
    focus: Option<usize>,
    hover: Option<usize>,
    pressed: Option<usize>,
    // The widgets.
    live: LivePill,
    track: ProgressTrack,
    percent: CountUp,
    seg: Segmented,
    actions: Vec<ActionButton>,
    days: DayChips,
    grid: CalendarGrid,
    day_sel: usize,
    stack: CardStack,
    pager: PagerDots,
    note_actions: [ActionButton; 2],
    timer: TimerRing,
    media: MediaPill,
    volume: Volume,
    arrow: RoundButton,
    // Entrances.
    dock_rise: Tween,
    tiles_fade: Tween,
    title_rise: Secs,
    stack_rise: Secs,
    // Frames.
    last_frame: Secs,
    last_input: Secs,
    last_second: i64,
    dirty_all: bool,
    /// Rectangles to draw once more at the next frame (where the focus ring was and is).
    extra: Vec<R>,
    /// Motion marked on a region the layout has not made yet (data that adds a card animates it): applied once it exists.
    pending: Vec<(Rg, Secs)>,
    /// Notifications already shown, so a refresh does not make them enter again.
    seen: Vec<String>,
}

fn q(v: f32) -> i32 {
    (v * 100.0).round() as i32
}

fn ms(t: Secs) -> u64 {
    (t.max(0.0) * 1000.0).round() as u64
}

/// The detail lines of a stat card `r`, as many of `n` (at most three) as fit between the caption and the numeral: one line each,
/// 28 dp apart, under the caption.
fn stat_lines(r: R, n: usize) -> Vec<R> {
    let top = r.y + space::L + 30.0;
    let bottom = r.bottom() - space::L - ty::NUMERAL.0 - space::S;
    (0..n.min(3))
        .map(|i| {
            R::new(
                r.x + space::L - 4.0,
                top + i as f32 * 28.0,
                r.w - 2.0 * space::L + 8.0,
                26.0,
            )
        })
        .filter(|l| l.bottom() <= bottom)
        .collect()
}

/// Lays cards down a column from `top` to `bottom`, through the proved rule: `place(h)` is the card's box, or `None` when it no
/// longer fits.
struct Column {
    x: f32,
    w: f32,
    cursor: i32,
    bottom: i32,
    gap: i32,
}

impl Column {
    fn new(r: R, gap: f32) -> Column {
        Column {
            x: r.x,
            w: r.w,
            cursor: q(r.y),
            bottom: q(r.bottom()),
            gap: q(gap),
        }
    }

    fn place(&mut self, h: f32) -> Option<R> {
        let s = hr::stack(self.cursor, self.bottom, q(h), self.gap);
        self.cursor = s.next;
        s.fits
            .then(|| R::new(self.x, s.start as f32 / 100.0, self.w, h))
    }

    /// What is left, if at least `min` dp.
    fn rest(&self, min: f32) -> Option<R> {
        let h = (self.bottom - self.cursor) as f32 / 100.0;
        (h >= min).then(|| R::new(self.x, self.cursor as f32 / 100.0, self.w, h))
    }
}

fn layout(d: &HomeData, seg: usize) -> Layout {
    let full = R::new(0.0, 0.0, W, H);
    let mut l = Layout {
        status: R::new(0.0, 0.0, full.w, 40.0),
        ..Layout::default()
    };
    // A full dock when something is uploading, counting down or playing; with only the volume, a slim strip under the right
    // column, and the columns use the height (an empty full-width bar reads as broken).
    let full_dock = d.upload.is_some() || d.countdown.is_some() || d.media.is_some();
    let slim = !full_dock && d.volume.is_some();
    let area = R::new(
        20.0,
        48.0,
        full.w - 40.0,
        if full_dock { 576.0 } else { 654.0 },
    );
    let left = crate::layouts::pane(area, 12, 1, space::L, 0, 0, 4, 1);
    let mid = crate::layouts::pane(area, 12, 1, space::L, 4, 0, 4, 1);
    let mut right = crate::layouts::pane(area, 12, 1, space::L, 8, 0, 4, 1);
    let strip = R::new(right.x, full.h - 18.0 - 40.0, right.w, 40.0);
    if slim {
        right.h = strip.y - space::M - right.y;
    }

    // Left: Build, the segments, their actions; the stat cards sit at the foot.
    // The stat cards sit at the foot, 150 dp tall; with no Build card they grow up to just under the actions, so the column has
    // no hole where Build would be (the right column's tall stat cards do the same).
    // Segments, a gap, the 8 dp above the actions, a gap, the actions, the gap before the cards, and a little air.
    let top_used = if d.build.is_some() {
        0.0
    } else {
        space::TAP + space::M + 8.0 + space::M + ActionButton::size().1 + space::M + space::S
    };
    let foot_h = if d.build.is_some() {
        150.0
    } else {
        (left.h - top_used).max(150.0)
    };
    let foot =
        (!d.stats_left.is_empty()).then(|| R::new(left.x, left.bottom() - foot_h, left.w, foot_h));
    let top_part = R::new(
        left.x,
        left.y,
        left.w,
        foot.map_or(left.h, |f| f.y - left.y - space::M),
    );
    let mut c = Column::new(top_part, space::M);
    if d.build.is_some() {
        l.build = c.place(166.0);
    }
    if d.segments.len() >= 2 {
        l.seg = c.place(space::TAP);
    }
    if d.segments.get(seg).is_some_and(|s| !s.1.is_empty()) {
        // A little more room above the row than between cards: the buttons float on the backdrop.
        c.place(8.0);
        l.actions = c.place(ActionButton::size().1);
    }
    l.stats_left = foot;

    // The middle: the device card, a person, what is due, the week.
    let mut c = Column::new(mid, space::M);
    if d.device.is_some() {
        l.device = c.place(100.0);
    }
    if d.person.is_some() {
        l.person = c.place(52.0).map(|r| R::new(r.x, r.y, r.w * 0.62, r.h));
    }
    if d.deadline.is_some() {
        l.deadline = c.place(80.0);
    }
    if d.week.is_some() {
        l.days = c.place(36.0);
        l.grid = c.rest(160.0);
    }

    // The right: a glass panel with two stat cards and the notifications.
    if !d.stats_right.is_empty() || d.notes.is_some() {
        l.panel = Some(right);
        let inner = right.inset(space::L);
        let mut c = Column::new(inner, space::XL);
        if !d.stats_right.is_empty() {
            l.stats_right = c.place(if d.notes.is_some() { 200.0 } else { inner.h });
        }
        if d.notes.is_some() {
            l.title = c.place(48.0);
            let mut c2 = c;
            c2.gap = q(22.0);
            l.stack = c2.place(96.0);
            c2.gap = q(space::L);
            c2.place(18.0);
            let row = c2.place(ActionButton::size().1);
            if let Some(row) = row {
                l.pager = Some(R::new(row.x, row.y + 4.0, 160.0, 36.0));
                l.note_actions = Some(R::new(row.right() - 150.0, row.y, 150.0, row.h));
            }
        }
    }

    // The dock: what uploads, the countdown, the media and the volume.
    if slim {
        l.dock = Some(strip);
        l.volume = Some(R::new(
            strip.x + 18.0,
            strip.cy() - 12.0,
            strip.w - 36.0,
            24.0,
        ));
    } else if full_dock {
        let dock = R::new(20.0, 638.0, full.w - 40.0, 64.0);
        l.dock = Some(dock);
        let mut x = dock.x + 20.0;
        if d.upload.is_some() {
            l.upload = Some(R::new(x, dock.y + 12.0, 150.0, 40.0));
            x += 162.0;
        }
        if d.countdown.is_some() {
            l.timer = Some(R::new(x, dock.y + 6.0, 170.0, 52.0));
        }
        if d.media.is_some() {
            l.media = Some(R::new(dock.cx() - 190.0, dock.y + 8.0, 380.0, 48.0));
        }
        if d.volume.is_some() {
            l.volume = Some(R::new(dock.right() - 250.0, dock.cy() - 12.0, 220.0, 24.0));
        }
    }
    l
}

impl MediaPill {
    fn new() -> MediaPill {
        MediaPill {
            press: Tween::at_rest(0.0),
            swap: Tween::at_rest(1.0),
        }
    }
}

fn tiles(week: &Week, sel: usize, now: Secs) -> Vec<Tile> {
    let mut v: Vec<Tile> = Vec::new();
    let shown = week.days.len().min(6);
    for (i, d) in week.days.iter().take(shown).enumerate() {
        if i == 4 {
            v.push(summary_tile(week, sel, now));
        }
        let dots: Vec<Color> = d
            .dots
            .iter()
            .take(3)
            .map(|k| match k {
                Dot::Own => color::ACCENT,
                Dot::Other => color::SIGNAL,
                Dot::Now => color::LIVE,
            })
            .collect();
        v.push(Tile {
            light: d.today,
            ..Tile::day(d.n, None, &dots)
        });
    }
    if shown <= 4 {
        v.push(summary_tile(week, sel, now));
    }
    v
}

/// The selected day's busy ranges, booked share and hourly bars.
fn summary_parts(week: &Week, sel: usize) -> (String, u32, Vec<f32>) {
    let Some(d) = week.days.get(sel) else {
        return (String::new(), 0, Vec::new());
    };
    let window = week.last_hour.saturating_sub(week.first_hour) * 60;
    let busy: u32 = d.hours.iter().map(|m| (*m).min(60)).sum();
    let ranges = if d.ranges.is_empty() {
        format!("{} free", d.name)
    } else {
        d.ranges.clone()
    };
    (
        ranges,
        hr::booked(busy, window),
        d.hours.iter().map(|m| (*m).min(60) as f32 / 60.0).collect(),
    )
}

fn summary_tile(week: &Week, sel: usize, now: Secs) -> Tile {
    let (ranges, pct, bars) = summary_parts(week, sel);
    Tile {
        kind: TileKind::Summary {
            ranges,
            percent: CountUp::new(pct, now + 0.35, "%"),
            bars,
        },
        span: 2,
        light: false,
    }
}

impl Home {
    pub fn new(now: Secs) -> Home {
        Home {
            data: HomeData::default(),
            layout: Layout::default(),
            regions: Vec::new(),
            items: Vec::new(),
            focus: None,
            hover: None,
            pressed: None,
            live: LivePill::new("Live", now),
            track: ProgressTrack::new(3, 1, 0.0),
            percent: CountUp::new(0, now, "%"),
            seg: Segmented::new(&["", ""], 0),
            actions: Vec::new(),
            days: DayChips::new(&[""], 0),
            grid: CalendarGrid::new(4, 2, Vec::new(), now),
            day_sel: 0,
            stack: CardStack::new(Vec::new()),
            pager: PagerDots::new(1, 0),
            note_actions: [
                ActionButton::new(Icon::ArrowUpRight, "Open"),
                ActionButton::new(Icon::Doc, "Clear"),
            ],
            timer: TimerRing::new(3600, 0.0, now),
            media: MediaPill::new(),
            volume: Volume::new(0.0),
            arrow: RoundButton::new(Icon::ArrowUpRight),
            dock_rise: Tween::at_rest(0.0),
            tiles_fade: Tween::at_rest(1.0),
            title_rise: -10.0,
            stack_rise: -10.0,
            last_frame: now,
            last_input: now,
            last_second: -1,
            dirty_all: true,
            extra: Vec::new(),
            pending: Vec::new(),
            seen: Vec::new(),
        }
    }

    /// Take new data at `now`: what changed is marked for drawing, and changes that have an animation play it.
    pub fn set_data(&mut self, now: Secs, d: HomeData) {
        let old = std::mem::replace(&mut self.data, d.clone());
        let d = &d;

        if d.build != old.build
            && let Some(b) = &d.build
        {
            let fill = (b.percent.min(100)) as f32 / 100.0;
            match &old.build {
                Some(o) if o.phases == b.phases && b.phase == o.phase + 1 => {
                    // phase-advance: the old phase completes, the next dot grows into a pill.
                    self.track.advance(now);
                    self.track.set_fill(now, fill);
                }
                Some(o) if o.phases == b.phases && o.phase == b.phase => {
                    self.track.set_fill(now, fill)
                }
                _ => {
                    self.track = ProgressTrack::new(b.phases.max(1), b.phase.max(1), 0.0);
                    self.track.replay(now, fill);
                }
            }
            // progress-count-up: on from what is shown.
            self.percent.retarget(now, b.percent.min(100));
            if b.live && !old.build.as_ref().is_some_and(|o| o.live) {
                self.live.since = now;
            }
            self.live.live = b.live;
            self.mark(Rg::Build, now + 1.3);
        }
        if d.segments != old.segments {
            let labels: Vec<&str> = d.segments.iter().map(|s| s.0.as_str()).collect();
            let sel = self.seg.selected.min(labels.len().saturating_sub(1));
            if labels.len() != self.seg.labels.len()
                || labels.iter().zip(&self.seg.labels).any(|(a, b)| *a != b)
            {
                self.seg = Segmented::new(&labels, sel);
            }
            self.actions_for(sel);
            self.touch(Rg::Seg);
            self.touch(Rg::Actions);
        }
        if d.week != old.week
            && let Some(w) = &d.week
        {
            let names: Vec<&str> = w.days.iter().take(5).map(|x| x.name.as_str()).collect();
            let same_week = old
                .week
                .as_ref()
                .is_some_and(|o| o.days.first().map(|x| x.n) == w.days.first().map(|x| x.n));
            if !same_week {
                self.day_sel = w.selected.min(names.len().saturating_sub(1));
                self.days = DayChips::new(&names, self.day_sel);
                self.grid = CalendarGrid::new(4, 2, tiles(w, self.day_sel, now), now);
                // calendar-stagger: another week comes in.
                self.grid.replay(now);
                self.mark(Rg::Grid, now + 2.0);
            } else {
                self.retile(now);
            }
            self.touch(Rg::Days);
        }
        if d.notes != old.notes {
            let notes = d.notes.clone().unwrap_or_default();
            let fresh = notes.iter().any(|n| !self.seen.contains(&n.id));
            let top_id = self.stack.cards.get(self.stack.top).map(|c| c.fact.clone());
            self.stack.cards = notes
                .iter()
                .map(|n| Card {
                    fact: n.fact.clone(),
                    secondary: n.line.clone(),
                    urgent: n.urgent,
                })
                .collect();
            self.stack.top = if fresh {
                0
            } else {
                notes
                    .iter()
                    .position(|n| Some(&n.fact) == top_id.as_ref())
                    .unwrap_or(0)
            };
            self.pager = PagerDots::new(notes.len().clamp(1, 5), self.stack.top.min(4));
            if fresh
                && !old
                    .notes
                    .as_ref()
                    .is_none_or(|o| o.is_empty() && notes.is_empty())
            {
                // notification-enter: the new card rises in.
                self.stack_rise = now;
                self.mark(Rg::Stack, now + 0.7);
            }
            self.seen = notes.iter().map(|n| n.id.clone()).collect();
            self.touch(Rg::Pager);
            self.touch(Rg::NoteActions);
        }
        if d.countdown != old.countdown {
            if let Some(c) = &d.countdown {
                self.timer = TimerRing::new(c.window.max(1), (c.at - now).max(0.0), now);
                self.timer.icon = Icon::Clock;
            }
            self.touch(Rg::Timer);
        }
        if d.media != old.media {
            if let (Some(m), Some(o)) = (&d.media, &old.media)
                && m.playing != o.playing
            {
                self.media.swap = Tween::new(0.0, 1.0, now, motion::SHORT, Ease::OutCubic);
                self.mark(Rg::Media, now + motion::SHORT);
            }
            self.touch(Rg::Media);
        }
        if d.volume != old.volume {
            if let Some((v, _)) = d.volume {
                if old.volume.is_none() {
                    self.volume = Volume::new(v);
                } else {
                    self.volume.set(now, v);
                    self.mark(Rg::Volume, now + 0.8);
                }
            }
            self.touch(Rg::Volume);
        }
        for (a, b, rg) in [
            (
                d.status.is_some() && d.status != old.status,
                true,
                Rg::Status,
            ),
            (d.device != old.device, true, Rg::Device),
            (d.person != old.person, true, Rg::Person),
            (d.deadline != old.deadline, true, Rg::Deadline),
            (d.stats_left != old.stats_left, true, Rg::StatsLeft),
            (d.stats_right != old.stats_right, true, Rg::StatsRight),
            (d.upload != old.upload, true, Rg::Upload),
        ] {
            if a && b {
                self.touch(rg);
            }
        }
        let l = layout(&self.data, self.seg.selected);
        if l != self.layout {
            self.layout = l;
            self.dirty_all = true;
        }
        self.rebuild_regions();
    }

    /// The summary tile follows the selected day (progress-count-up on its share).
    fn retile(&mut self, now: Secs) {
        let Some(w) = &self.data.week else { return };
        let (ranges, pct, bars) = summary_parts(w, self.day_sel);
        let mut fresh = tiles(w, self.day_sel, now);
        for (t, n) in self.grid.tiles.iter_mut().zip(fresh.iter_mut()) {
            if let (
                TileKind::Summary { percent, .. },
                TileKind::Summary {
                    ranges: r2,
                    bars: b2,
                    ..
                },
            ) = (&mut t.kind, &mut n.kind)
            {
                *r2 = ranges.clone();
                *b2 = bars.clone();
                let mut p = CountUp::new(pct, now, "%");
                p.base = percent.value(now);
                std::mem::swap(percent, &mut p);
                percent.retarget(now, pct);
            }
        }
        for (t, n) in self.grid.tiles.iter_mut().zip(fresh) {
            match (&mut t.kind, n.kind) {
                (
                    TileKind::Summary {
                        ranges: r, bars: b, ..
                    },
                    TileKind::Summary {
                        ranges: r2,
                        bars: b2,
                        ..
                    },
                ) => {
                    *r = r2;
                    *b = b2;
                }
                (k, nk) => {
                    *k = nk;
                    t.light = n.light;
                }
            }
        }
        self.mark(Rg::Summary, now + 1.3);
    }

    fn actions_for(&mut self, seg: usize) {
        self.actions = self.data.segments.get(seg).map_or(Vec::new(), |s| {
            s.1.iter()
                .take(4)
                .map(|a| ActionButton::new(a.icon, &a.label))
                .collect()
        });
    }

    /// Play the entrance from `now`: the dock rises and the stat cards fade up (sheet-rise), the calendar comes in staggered with its
    /// share counting up (calendar-stagger, progress-count-up), and the notifications' title rises (notification-enter).
    pub fn enter(&mut self, now: Secs) {
        self.dock_rise = Tween::new(1.0, 0.0, now, 0.7, Ease::EMPHASIZED);
        self.tiles_fade = Tween::new(0.0, 1.0, now + 0.35, 0.5, Ease::OutCubic);
        self.title_rise = now;
        self.grid.replay(now + 0.1);
        if let Some(b) = &self.data.build {
            self.percent.restart(now + 0.2, b.percent.min(100));
            self.track
                .replay(now + 0.2, b.percent.min(100) as f32 / 100.0);
        }
        // Only what enters is drawn: the rest of the screen is already there.
        self.mark(Rg::Grid, now + 2.1);
        self.mark(Rg::Build, now + 1.5);
        self.mark(Rg::StatsLeft, now + 0.9);
        self.mark(Rg::StatsRight, now + 0.9);
        self.mark(Rg::Title, now + 0.75);
    }

    /// Draw `r` (dp) again at the next frame: something the shell draws over the home there changed.
    pub fn redraw(&mut self, r: R) {
        self.extra.push(r);
    }

    /// Everything is drawn again at the next frame (a new backdrop, the page came back).
    pub fn invalidate(&mut self) {
        self.dirty_all = true;
    }

    fn region_rect(&self, id: Rg) -> Option<R> {
        let l = &self.layout;
        match id {
            Rg::Status => Some(l.status),
            Rg::Build => l.build,
            Rg::Seg => l.seg,
            Rg::Actions => l.actions,
            Rg::StatsLeft => l.stats_left,
            Rg::Device => l.device,
            Rg::Person => l.person,
            Rg::Deadline => l.deadline,
            Rg::Days => l.days,
            Rg::Grid => l.grid,
            Rg::StatsRight => l.stats_right,
            Rg::Title => l.title,
            Rg::Stack => l
                .stack
                .map(|r| R::new(r.x - 8.0, r.y, r.w + 16.0, r.h + 26.0)),
            Rg::Pager => l.pager,
            Rg::NoteActions => l.note_actions,
            Rg::Upload => l.upload,
            Rg::Timer => l.timer,
            Rg::Media => l.media,
            Rg::Volume => l.volume,
            // The pill is at most about 70 dp wide, top right of Build.
            Rg::Live => l
                .build
                .filter(|_| self.data.build.as_ref().is_some_and(|b| b.live))
                .map(|r| R::new(r.right() - space::L - 74.0, r.y + space::L, 74.0, 20.0)),
            Rg::Spin => l
                .device
                .filter(|_| self.data.device.as_ref().is_some_and(|d| d.busy))
                .map(|r| R::new(r.right() - 36.0, r.bottom() - 30.0, 16.0, 16.0)),
            Rg::UploadSpin => l
                .upload
                .map(|r| R::new(r.x + 2.0, r.cy() - 8.0, 16.0, 16.0)),
            Rg::Summary => l.grid.and_then(|r| {
                let at = self
                    .grid
                    .tiles
                    .iter()
                    .position(|t| matches!(t.kind, TileKind::Summary { .. }))?;
                self.grid
                    .layout(r)
                    .into_iter()
                    .find(|(i, _)| *i == at)
                    .map(|(_, b)| b)
            }),
        }
    }

    fn rebuild_regions(&mut self) {
        const ALL: [Rg; 23] = [
            Rg::Status,
            Rg::Build,
            Rg::Seg,
            Rg::Actions,
            Rg::StatsLeft,
            Rg::Device,
            Rg::Person,
            Rg::Deadline,
            Rg::Days,
            Rg::Grid,
            Rg::StatsRight,
            Rg::Title,
            Rg::Stack,
            Rg::Pager,
            Rg::NoteActions,
            Rg::Upload,
            Rg::Timer,
            Rg::Media,
            Rg::Volume,
            Rg::Live,
            Rg::Spin,
            Rg::UploadSpin,
            Rg::Summary,
        ];
        let old = std::mem::take(&mut self.regions);
        for id in ALL {
            if let Some(r) = self.region_rect(id) {
                let prev = old.iter().find(|g| g.id == id);
                let ambient = matches!(id, Rg::Live | Rg::Spin | Rg::UploadSpin);
                let marked = self
                    .pending
                    .iter()
                    .filter(|(p, _)| *p == id)
                    .map(|(_, u)| *u)
                    .fold(0.0f64, f64::max);
                let until = prev.map_or(0.0, |p| p.until).max(marked);
                self.regions.push(Region {
                    id,
                    r,
                    until,
                    ambient,
                    dirty: prev.is_none_or(|p| p.dirty || p.r != r),
                });
            }
        }
        self.pending.clear();
        // The focus list follows the layout.
        let l = &self.layout;
        let mut items: Vec<(Item, R)> = Vec::new();
        if let Some(r) = l.build
            && self.data.build.as_ref().is_some_and(|b| b.app.is_some())
        {
            items.push((Item::Build, r));
        }
        if let Some(r) = l.seg {
            let n = self.data.segments.len() as u32;
            for i in 0..n {
                let (x, w) = cell_x(r.inset(4.0), n, 0.0, i, 1);
                items.push((Item::Segment(i as usize), R::new(x, r.y, w, r.h)));
            }
        }
        if let Some(r) = l.actions {
            let n = self.actions.len();
            for i in 0..n {
                let (x, w) = cell_x(r, 4, space::M, i as u32, 1);
                let (bw, _) = ActionButton::size();
                items.push((
                    Item::Action(i),
                    R::new(x + w / 2.0 - bw / 2.0, r.y, bw, space::TAP),
                ));
            }
        }
        if let Some(r) = l.stats_left {
            let n = self.data.stats_left.len().min(2) as u32;
            for (i, s) in self.data.stats_left.iter().take(2).enumerate() {
                let card = crate::layouts::pane(r, n, 1, space::M, i as u32, 0, 1, 1);
                for (j, line) in stat_lines(card, s.items.len()).into_iter().enumerate() {
                    items.push((Item::StatLine(i, j), line));
                }
                if s.app.is_some() {
                    items.push((Item::StatLeft(i), card));
                }
            }
        }
        if let Some(r) = l.device {
            items.push((Item::Device, r));
        }
        if let Some(r) = l.deadline {
            items.push((Item::Deadline, r));
        }
        if let Some(r) = l.days {
            let n = self.days.labels.len() as u32;
            for i in 0..n {
                let (x, w) = cell_x(r, n, 6.0, i, 1);
                items.push((Item::Day(i as usize), R::new(x, r.y, w, r.h)));
            }
        }
        if let Some(r) = l.stats_right {
            let tall = r.h > r.w;
            for (i, s) in self.data.stats_right.iter().take(2).enumerate() {
                if s.app.is_some() {
                    let b = if tall {
                        crate::layouts::pane(r, 1, 2, space::M, 0, i as u32, 1, 1)
                    } else {
                        crate::layouts::pane(r, 2, 1, space::M, i as u32, 0, 1, 1)
                    };
                    items.push((Item::StatRight(i), b));
                }
            }
        }
        let has_notes = self.data.notes.as_ref().is_some_and(|n| !n.is_empty());
        if let (Some(r), true) = (l.stack, has_notes) {
            items.push((Item::Stack, r));
        }
        if let (Some(r), true) = (l.note_actions, has_notes) {
            let (aw, _) = ActionButton::size();
            items.push((
                Item::NoteAction(0),
                R::new(r.right() - aw * 2.0 - space::S, r.y, aw, space::TAP),
            ));
            items.push((
                Item::NoteAction(1),
                R::new(r.right() - aw, r.y, aw, space::TAP),
            ));
        }
        if let Some(r) = l.media {
            items.push((Item::Media, R::new(r.right() - 48.0, r.y, 48.0, r.h)));
        }
        if let Some(r) = l.volume {
            items.push((Item::Volume, r));
        }
        let keep = self
            .focus
            .and_then(|i| self.items.get(i))
            .map(|(it, _)| it.clone());
        self.items = items;
        self.focus = keep.and_then(|k| self.items.iter().position(|(it, _)| *it == k));
    }

    fn touch(&mut self, id: Rg) {
        if let Some(g) = self.regions.iter_mut().find(|g| g.id == id) {
            g.dirty = true;
        } else {
            // Not laid out yet: the next layout draws everything anyway.
            self.dirty_all = true;
        }
    }

    fn mark(&mut self, id: Rg, until: Secs) {
        self.touch(id);
        match self.regions.iter_mut().find(|g| g.id == id) {
            Some(g) => g.until = g.until.max(until),
            None => self.pending.push((id, until)),
        }
    }

    /// Move the focus ring: only where it was and where it goes are drawn again.
    fn focus_to(&mut self, i: Option<usize>) {
        for j in [self.focus, i].into_iter().flatten() {
            if let Some((_, r)) = self.items.get(j) {
                self.extra.push(r.inset(-8.0));
            }
        }
        self.focus = i;
    }

    fn ambient_on(&self, now: Secs) -> bool {
        now < self.last_input + AMBIENT_FOR
    }

    /// The rectangles (dp) to draw at `now`, and the frame counted as drawn. An empty list means nothing to draw. The first rectangle
    /// is the whole screen when everything is to be drawn.
    pub fn damage(&mut self, now: Secs) -> Vec<R> {
        let ambient = self.ambient_on(now);
        // The countdown ticks each second: its digits roll while someone is about, and simply change otherwise.
        if self.data.countdown.is_some() {
            let sec = now.floor() as i64;
            if sec != self.last_second {
                self.last_second = sec;
                let roll = if ambient { 0.34 } else { 0.0 };
                self.mark(Rg::Timer, now + roll);
            }
        }
        if self.dirty_all {
            self.dirty_all = false;
            for g in &mut self.regions {
                g.dirty = false;
            }
            self.last_frame = now;
            return vec![R::new(0.0, 0.0, W, H)];
        }
        let last = self.last_frame;
        let mut out = std::mem::take(&mut self.extra);
        for g in &mut self.regions {
            let moving = g.until > last || g.dirty || (g.ambient && ambient);
            g.dirty = false;
            if moving {
                out.push(g.r.inset(if g.ambient && g.until <= last && !g.dirty {
                    -8.0
                } else {
                    -SPILL
                }));
            }
        }
        // The dock moves as one while it rises.
        if (self.dock_rise.at(now) > 0.0 || self.dock_rise.start + self.dock_rise.dur > last)
            && let Some(d) = self.layout.dock
        {
            out.push(R::new(
                d.x - SPILL,
                d.y - SPILL,
                d.w + 2.0 * SPILL,
                H - d.y + SPILL,
            ));
        }
        if !out.is_empty() {
            self.last_frame = now;
        }
        merge(out)
    }

    /// When the next frame is due on the view's clock, or `None`: nothing moves, sleep until data or input comes.
    pub fn next_frame(&self, now: Secs, period: Secs) -> Option<Secs> {
        let moving = self
            .regions
            .iter()
            .map(|g| g.until)
            .fold(0.0f64, f64::max)
            .max(self.dock_rise.start + self.dock_rise.dur);
        let any_ambient = self.regions.iter().any(|g| g.ambient);
        let dirty =
            self.dirty_all || !self.extra.is_empty() || self.regions.iter().any(|g| g.dirty);
        if dirty {
            return Some(now);
        }
        let last = self.last_frame.min(now);
        let due = hr::next_frame(
            ms(now),
            ms(last),
            ms(period).max(1),
            if moving > last {
                ms(moving).max(ms(last) + 1)
            } else {
                0
            },
            if any_ambient { ms(AMBIENT_FRAME) } else { 0 },
            ms(self.last_input + AMBIENT_FOR),
        );
        let mut next = (due != 0).then(|| due as f64 / 1000.0);
        if self.data.countdown.is_some() {
            let tick = now.floor() + 1.0;
            next = Some(next.map_or(tick, |n| n.min(tick)));
        }
        next
    }

    /// Draw the whole scene at `now` onto `cv` (its clip decides what is really touched), over `backdrop` (the image) with its
    /// blurred copy for the glass.
    pub fn draw(
        &mut self,
        cv: &mut Canvas,
        ty: &mut Type,
        now: Secs,
        backdrop: &Canvas,
        blurred: &Canvas,
    ) {
        let (x0, y0, x1, y1) = cv.clip_px();
        let column = backdrop.w == 1 && backdrop.h == cv.h;
        for y in y0..y1 {
            let row = y as usize * cv.w as usize;
            let span = &mut cv.px[row + x0 as usize..row + x1 as usize];
            if column {
                span.fill(backdrop.px[y as usize]);
            } else {
                span.copy_from_slice(&backdrop.px[row + x0 as usize..row + x1 as usize]);
            }
        }
        let mut p = Paint {
            cv,
            ty,
            now,
            backdrop: Some(blurred),
        };
        let l = self.layout.clone();
        p.cv.scrim(
            R::new(0.0, 0.0, W, 60.0),
            Color::hexa(0x000000, 120),
            Color::hexa(0x000000, 0),
        );
        if let Some(s) = &self.data.status {
            status_bar(&mut p, l.status, s);
        }

        // Left.
        if let (Some(r), Some(b)) = (l.build, self.data.build.clone()) {
            self.draw_build(&mut p, r, &b);
        }
        if let Some(r) = l.seg {
            self.seg.draw(&mut p, r);
        }
        if let Some(r) = l.actions {
            if p.backdrop.is_some() {
                p.cv.fill_rrect_soft(
                    r.inset_xy(-8.0, -4.0),
                    radius::LG,
                    14.0,
                    Color::hexa(0x000000, 170),
                );
            }
            for (i, b) in self.actions.iter_mut().enumerate() {
                let (x, w) = cell_x(r, 4, space::M, i as u32, 1);
                b.draw(&mut p, x + w / 2.0, r.y);
            }
        }
        if let Some(r) = l.stats_left {
            let k = self.tiles_fade.at(now).clamp(0.0, 1.0);
            let stats = self.data.stats_left.clone();
            let o = p.cv.opacity;
            p.cv.opacity = o * k;
            p.cv.origin.1 += 12.0 * (1.0 - k);
            let n = stats.len().min(2) as u32;
            for (i, s) in stats.iter().take(2).enumerate() {
                let t = crate::layouts::pane(r, n, 1, space::M, i as u32, 0, 1, 1);
                chrome::stat_tile(&mut p, t, s.icon, &s.caption, &s.value);
                // The count's detail, when the card is tall enough: its top items, one line each.
                for (line, item) in stat_lines(t, s.items.len()).into_iter().zip(&s.items) {
                    let title = p.fit(Font::Regular, ty::LEAD, &item.title, line.w - 8.0);
                    p.text(
                        Font::Regular,
                        ty::LEAD,
                        line.x + 4.0,
                        baseline_for(&p, Font::Regular, ty::LEAD.0, line.cy()),
                        color::INK,
                        &title,
                    );
                }
            }
            p.cv.origin.1 -= 12.0 * (1.0 - k);
            p.cv.opacity = o;
        }

        // The middle.
        if let (Some(r), Some(dv)) = (l.device, self.data.device.clone()) {
            p.glass(r, radius::XL, color::MATERIAL);
            p.text(
                Font::Thin,
                ty::TITLE,
                r.x + space::L,
                r.y + 46.0,
                color::INK,
                "Juno",
            );
            self.arrow.draw(&mut p, r.right() - 34.0, r.y + 32.0, 18.0);
            let line = p.fit(
                Font::Regular,
                ty::CAPTION,
                &dv.line,
                r.w - 2.0 * space::L - 30.0,
            );
            p.text(
                Font::Regular,
                ty::CAPTION,
                r.x + space::L,
                r.bottom() - 18.0,
                color::INK_2,
                &line,
            );
            if dv.busy {
                chrome::spinner(&mut p, r.right() - 28.0, r.bottom() - 22.0, 6.0, color::INK);
            }
        }
        if let (Some(r), Some(pe)) = (l.person, self.data.person.clone()) {
            chrome::profile_chip(&mut p, r, &pe.initials, &pe.name, &pe.line);
        }
        if let (Some(r), Some(dl)) = (l.deadline, self.data.deadline.clone()) {
            p.cv.push_clip(r, radius::LG);
            chrome::reminder_card(
                &mut p,
                r,
                (&dl.fact, &dl.line),
                if dl.soon {
                    color::WARNING
                } else {
                    color::ACCENT
                },
            );
            p.cv.pop_clip();
        }
        if let Some(r) = l.days {
            self.days.draw(&mut p, r);
        }
        if let Some(r) = l.grid {
            self.grid.draw(&mut p, r);
        }

        // The right.
        if let Some(panel) = l.panel {
            p.glass(panel, radius::XL, Color::hexa(0x1c1c1e, 200));
        }
        if let Some(r) = l.stats_right {
            let stats = self.data.stats_right.clone();
            let k = self.tiles_fade.at(now).clamp(0.0, 1.0);
            let o = p.cv.opacity;
            p.cv.opacity = o * k;
            let tall = r.h > r.w * 0.8;
            let n = stats.len().min(2) as u32;
            for (i, s) in stats.iter().take(2).enumerate() {
                let b = if tall && n == 2 {
                    crate::layouts::pane(r, 2, 1, space::M, i as u32, 0, 1, 1)
                } else {
                    crate::layouts::pane(r, n, 1, space::M, i as u32, 0, 1, 1)
                };
                chrome::stat_tile(&mut p, b, s.icon, &s.caption, &s.value);
            }
            p.cv.opacity = o;
        }
        if let Some(r) = l.title {
            // notification-enter: the title rises in.
            let k = Ease::OutCubic.at(phase(now, self.title_rise + 0.1, 0.6));
            let o = p.cv.opacity;
            p.cv.opacity = o * k;
            p.text(
                Font::Thin,
                (48.0, -48.0 * 0.03),
                r.x - 2.0,
                r.y + 38.0 + 18.0 * (1.0 - k),
                color::INK,
                "Notification",
            );
            p.cv.opacity = o;
        }
        if let Some(r) = l.stack {
            if self.stack.cards.is_empty() {
                p.text(
                    Font::Regular,
                    ty::LEAD,
                    r.x,
                    r.y + 30.0,
                    color::INK_2,
                    "Nothing new",
                );
            } else {
                let k = Ease::OutCubic.at(phase(now, self.stack_rise, 0.55));
                let o = p.cv.opacity;
                p.cv.opacity = o * (0.3 + 0.7 * k);
                p.cv.origin.1 += 14.0 * (1.0 - k);
                self.stack.draw(&mut p, r);
                p.cv.origin.1 -= 14.0 * (1.0 - k);
                p.cv.opacity = o;
            }
        }
        if self.stack.cards.len() > 1
            && let Some(r) = l.pager
        {
            self.pager.draw(&mut p, r.x, r.cy());
        }
        if !self.stack.cards.is_empty()
            && let Some(r) = l.note_actions
        {
            let (aw, _) = ActionButton::size();
            self.note_actions[0].draw(&mut p, r.right() - aw * 1.5 - space::S, r.y);
            self.note_actions[1].draw(&mut p, r.right() - aw / 2.0, r.y);
        }

        // The dock, rising in on entrance (sheet-rise).
        if let Some(dock) = l.dock {
            let lift = self.dock_rise.at(now).max(0.0) * (H - dock.y + 10.0);
            p.cv.origin.1 += lift;
            p.glass(dock, radius::PILL, color::SMOKE);
            if let (Some(r), Some(u)) = (l.upload, self.data.upload.clone()) {
                chrome::spinner(&mut p, r.x + 10.0, r.cy(), 6.0, color::INK);
                p.text(
                    Font::Regular,
                    ty::CAPTION,
                    r.x + 26.0,
                    r.cy() + 4.0,
                    color::INK_2,
                    &u.label,
                );
                if let Some(f) = u.fraction {
                    let bar = R::new(r.x + 26.0, r.cy() + 11.0, 100.0, 2.0);
                    p.cv.fill_rrect(bar, 1.0, color::HAIR);
                    p.cv.fill_rrect(
                        R::new(bar.x, bar.y, bar.w * f.clamp(0.0, 1.0), bar.h),
                        1.0,
                        color::INK,
                    );
                }
            }
            if let Some(r) = l.timer {
                self.timer.draw_ring(&mut p, r.x + 24.0, r.cy(), 21.0);
                self.timer
                    .draw_digits(&mut p, r.x + 54.0, r.cy() + 12.0, 32.0, color::INK);
            }
            if let (Some(r), Some(m)) = (l.media, self.data.media.clone()) {
                self.draw_media(&mut p, r, &m);
            }
            if let (Some(r), Some((_, muted))) = (l.volume, self.data.volume) {
                self.volume.draw(&mut p, r);
                if muted {
                    p.cv.line(
                        r.x + 2.0,
                        r.cy() - 7.0,
                        r.x + 15.0,
                        r.cy() + 7.0,
                        1.6,
                        color::LIVE,
                    );
                }
            }
            p.cv.origin.1 -= lift;
        }

        // The keyboard's focus ring.
        if let Some((item, r)) = self.focus.and_then(|i| self.items.get(i)).cloned() {
            let rad = match item {
                Item::Action(_) | Item::NoteAction(_) | Item::Media => space::TAP / 2.0,
                Item::Segment(_) | Item::Day(_) | Item::Volume => radius::PILL,
                Item::Build | Item::Device => radius::XL,
                _ => radius::LG,
            };
            p.cv.stroke_rrect(r.inset(-3.0), rad + 3.0, 1.6, color::ACCENT);
        }
    }

    fn draw_build(&mut self, p: &mut Paint, r: R, b: &Build) {
        let title = 64.0;
        p.glass(r, radius::XL, color::MATERIAL);
        if b.live {
            let (lw, _) = self.live.size(p);
            self.live.draw(p, r.right() - space::L - lw, r.y + space::L);
        }
        p.cv.push_clip(r, radius::XL);
        p.text(
            Font::Thin,
            (title, -title * 0.026),
            r.x + 10.0,
            r.y + 18.0 + title * 0.95,
            color::INK,
            &b.title,
        );
        let phase_s = b.line.clone();
        p.text(
            Font::Regular,
            ty::CAPTION,
            r.x + space::L,
            r.bottom() - 22.0,
            color::INK_2,
            &phase_s,
        );
        let pw = p.measure(Font::Regular, ty::CAPTION, &phase_s);
        self.track.draw(
            p,
            R::new(
                r.x + space::L + pw + space::M,
                r.bottom() - 31.0,
                r.w - pw - 124.0,
                10.0,
            ),
        );
        self.percent.draw(
            p,
            r.right() - space::L,
            r.bottom() - 18.0,
            Font::Thin,
            34.0,
            color::INK,
        );
        p.cv.pop_clip();
    }

    fn draw_media(&mut self, p: &mut Paint, r: R, m: &Media) {
        p.glass(r, radius::PILL, color::GLASS_TINT);
        let (cx, cy) = (r.right() - 26.0, r.cy());
        p.cv.push_clip(R::new(r.x + 18.0, r.y, r.w - 70.0, r.h), 0.0);
        p.text(
            Font::Regular,
            ty::LEAD,
            r.x + 22.0,
            baseline_for(p, Font::Regular, ty::LEAD.0, cy),
            color::INK,
            &m.title,
        );
        p.cv.pop_clip();
        let pr = self.media.press.at(p.now).clamp(-0.2, 1.2);
        let scale = 1.0 - 0.14 * pr;
        if pr > 0.01 {
            p.cv.fill_circle(
                cx,
                cy,
                21.0,
                Color::hexa(0xffffff, (40.0 * pr.min(1.0)) as u8),
            );
        }
        p.cv.fill_circle(cx, cy, 18.0 * scale, color::INK);
        let s = self.media.swap.at(p.now).clamp(0.0, 1.0);
        let (now_icon, was_icon) = if m.playing {
            (Icon::Pause, Icon::Play)
        } else {
            (Icon::Play, Icon::Pause)
        };
        let size = 20.0 * scale;
        let o = p.cv.opacity;
        if s < 1.0 {
            p.cv.opacity = o * (1.0 - s);
            icons::draw(
                p.cv,
                was_icon,
                cx,
                cy,
                size * (1.0 - 0.3 * s),
                color::PAPER_INK,
            );
        }
        p.cv.opacity = o * s;
        icons::draw(
            p.cv,
            now_icon,
            cx,
            cy,
            size * (0.7 + 0.3 * s),
            color::PAPER_INK,
        );
        p.cv.opacity = o;
    }

    // ---- Input ----

    fn item_at(&self, x: f32, y: f32) -> Option<usize> {
        self.items.iter().position(|(_, r)| r.contains(x, y))
    }

    fn set_hover(&mut self, now: Secs, i: Option<usize>) {
        if self.hover == i {
            return;
        }
        for j in [self.hover, i].into_iter().flatten() {
            let on = Some(j) == i;
            match self.items.get(j).map(|x| x.0.clone()) {
                Some(Item::Action(a)) => {
                    if let Some(b) = self.actions.get_mut(a) {
                        b.set_hover(now, on);
                    }
                    self.mark(Rg::Actions, now + motion::SHORT);
                }
                Some(Item::NoteAction(a)) => {
                    self.note_actions[a].set_hover(now, on);
                    self.mark(Rg::NoteActions, now + motion::SHORT);
                }
                _ => {}
            }
        }
        self.hover = i;
    }

    fn press(&mut self, now: Secs, i: usize) {
        match self.items.get(i).map(|x| x.0.clone()) {
            Some(Item::Action(a)) => {
                if let Some(b) = self.actions.get_mut(a) {
                    b.press(now);
                }
                self.mark(Rg::Actions, now + motion::PRESS);
            }
            Some(Item::NoteAction(a)) => {
                self.note_actions[a].press(now);
                self.mark(Rg::NoteActions, now + motion::PRESS);
            }
            Some(Item::Media) => {
                self.media.press = Tween::new(
                    self.media.press.at(now),
                    1.0,
                    now,
                    motion::PRESS,
                    Ease::OutCubic,
                );
                self.mark(Rg::Media, now + motion::PRESS);
            }
            _ => {}
        }
    }

    fn release(&mut self, now: Secs, i: usize) {
        match self.items.get(i).map(|x| x.0.clone()) {
            Some(Item::Action(a)) => {
                if let Some(b) = self.actions.get_mut(a) {
                    b.release(now);
                }
                self.mark(Rg::Actions, now + motion::MEDIUM);
            }
            Some(Item::NoteAction(a)) => {
                self.note_actions[a].release(now);
                self.mark(Rg::NoteActions, now + motion::MEDIUM);
            }
            Some(Item::Media) => {
                self.media.press = Tween::new(
                    self.media.press.at(now),
                    0.0,
                    now,
                    motion::MEDIUM,
                    Ease::OutBack(1.6),
                );
                self.mark(Rg::Media, now + motion::MEDIUM);
            }
            _ => {}
        }
    }

    /// Do item `i`.
    fn activate(&mut self, now: Secs, i: usize) -> Outcome {
        let Some((item, _)) = self.items.get(i).cloned() else {
            return Outcome::None;
        };
        match item {
            Item::Build => self
                .data
                .build
                .as_ref()
                .and_then(|b| b.app.clone())
                .map_or(Outcome::None, |a| Outcome::Open(Target::App(a))),
            Item::Segment(s) => {
                if s != self.seg.selected {
                    // segmented: the thumb slides on its spring; the row of actions follows.
                    self.seg.select(now, s);
                    self.actions_for(s);
                    self.mark(Rg::Seg, now + 0.7);
                    self.touch(Rg::Actions);
                    self.layout = layout(&self.data, s);
                    self.rebuild_regions();
                }
                Outcome::Redraw
            }
            Item::Action(a) => {
                let seg = self.seg.selected;
                self.data
                    .segments
                    .get(seg)
                    .and_then(|s| s.1.get(a))
                    .map_or(Outcome::None, |x| Outcome::Open(x.target.clone()))
            }
            Item::StatLeft(s) => self
                .data
                .stats_left
                .get(s)
                .and_then(|x| x.app.clone())
                .map_or(Outcome::None, |a| Outcome::Open(Target::App(a))),
            Item::StatLine(s, j) => self
                .data
                .stats_left
                .get(s)
                .and_then(|x| x.items.get(j))
                .map_or(Outcome::None, |it| Outcome::Open(it.target.clone())),
            Item::StatRight(s) => self
                .data
                .stats_right
                .get(s)
                .and_then(|x| x.app.clone())
                .map_or(Outcome::None, |a| Outcome::Open(Target::App(a))),
            Item::Device => Outcome::Open(Target::Settings),
            Item::Deadline => Outcome::Open(Target::App("calendar".into())),
            Item::Day(d) => {
                self.select_day(now, d);
                Outcome::Redraw
            }
            Item::Stack | Item::NoteAction(0) => self
                .top_note()
                .and_then(|n| n.app.clone())
                .map_or(Outcome::None, |a| Outcome::Open(Target::App(a))),
            Item::NoteAction(_) => self.dismiss(now),
            Item::Media => self
                .data
                .media
                .as_ref()
                .map_or(Outcome::None, |m| Outcome::Media(m.app.clone())),
            Item::Volume => Outcome::None,
        }
    }

    /// What can be done on the screen, in focus order: a name for each item (its own words) and its box in dp. For Lucina's
    /// reading of the screen (the shell's AccessKit tree).
    pub fn items(&self) -> Vec<(String, R)> {
        let seg = self.seg.selected;
        self.items
            .iter()
            .map(|(it, r)| {
                let name = match it {
                    Item::Build => self
                        .data
                        .build
                        .as_ref()
                        .map_or(String::new(), |b| b.title.clone()),
                    Item::Segment(i) => self
                        .data
                        .segments
                        .get(*i)
                        .map_or(String::new(), |s| s.0.clone()),
                    Item::Action(i) => self
                        .data
                        .segments
                        .get(seg)
                        .and_then(|s| s.1.get(*i))
                        .map_or(String::new(), |a| a.label.clone()),
                    Item::StatLeft(i) => self
                        .data
                        .stats_left
                        .get(*i)
                        .map_or(String::new(), |s| s.caption.clone()),
                    Item::StatLine(i, j) => self
                        .data
                        .stats_left
                        .get(*i)
                        .and_then(|s| s.items.get(*j))
                        .map_or(String::new(), |it| it.title.clone()),
                    Item::StatRight(i) => self
                        .data
                        .stats_right
                        .get(*i)
                        .map_or(String::new(), |s| s.caption.clone()),
                    Item::Device => "Juno".to_string(),
                    Item::Deadline => self
                        .data
                        .deadline
                        .as_ref()
                        .map_or(String::new(), |d| d.fact.clone()),
                    Item::Day(i) => self.days.labels.get(*i).cloned().unwrap_or_default(),
                    Item::Stack => self.top_note().map_or(String::new(), |n| n.fact.clone()),
                    Item::NoteAction(0) => "Open".to_string(),
                    Item::NoteAction(_) => "Clear".to_string(),
                    Item::Media => if self.data.media.as_ref().is_some_and(|m| m.playing) {
                        "Pause"
                    } else {
                        "Play"
                    }
                    .to_string(),
                    Item::Volume => "Volume".to_string(),
                };
                (name, *r)
            })
            .collect()
    }

    /// Do item `i` of [`Home::items`], as Enter on it would.
    pub fn activate_item(&mut self, now: Secs, i: usize) -> Outcome {
        self.last_input = now;
        self.activate(now, i)
    }

    fn top_note(&self) -> Option<&Note> {
        self.data.notes.as_ref().and_then(|n| n.get(self.stack.top))
    }

    fn select_day(&mut self, now: Secs, d: usize) {
        if d != self.day_sel && d < self.days.labels.len() {
            self.day_sel = d;
            // day-chips: the pill stretches across and contracts; the summary counts to the day's share.
            self.days.select(now, d);
            self.mark(Rg::Days, now + 0.9);
            self.retile(now);
        }
    }

    /// Clear the top notification (card-stack: it is thrown, the rest move up).
    fn dismiss(&mut self, now: Secs) -> Outcome {
        let Some(id) = self.top_note().map(|n| n.id.clone()) else {
            return Outcome::None;
        };
        self.stack.throw(now, 1.0, 0.0);
        self.mark(Rg::Stack, now + 1.0);
        self.touch(Rg::Pager);
        Outcome::Dismissed(id)
    }

    /// Page the stack by `d` (card-stack and pager-dots).
    fn page(&mut self, now: Secs, d: i32) {
        let n = self.stack.cards.len();
        if n < 2 {
            return;
        }
        let next = hr::wrap(self.stack.top as u32, n as u32, d.signum()) as usize;
        if d > 0 {
            self.stack.throw(now, -1.0, 0.0);
        } else {
            self.stack.top = next;
        }
        self.pager.go(now, next.min(self.pager.n - 1));
        self.mark(Rg::Stack, now + 1.0);
        self.mark(Rg::Pager, now + motion::MEDIUM + 0.05);
    }

    /// A key. `Unused` leaves it to the shell.
    pub fn key(&mut self, now: Secs, k: Key) -> Outcome {
        self.last_input = now;
        let Some(f) = self.focus else {
            return match k {
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    if self.items.is_empty() {
                        return Outcome::Unused;
                    }
                    self.focus_to(Some(0));
                    Outcome::Redraw
                }
                _ => Outcome::Unused,
            };
        };
        let item = self.items[f].0.clone();
        match (k, &item) {
            (Key::Left | Key::Right, Item::Volume) => {
                let v = self.data.volume.map_or(0.0, |x| x.0);
                let nv = (v + if k == Key::Left { -0.05 } else { 0.05 }).clamp(0.0, 1.0);
                return Outcome::Volume(nv);
            }
            (Key::Left | Key::Right, Item::Stack) => {
                self.page(now, if k == Key::Left { -1 } else { 1 });
                return Outcome::Redraw;
            }
            (Key::Delete, Item::Stack | Item::NoteAction(_)) => return self.dismiss(now),
            (Key::Enter, _) => {
                self.press(now, f);
                self.release(now + motion::PRESS, f);
                return self.activate(now, f);
            }
            (Key::Esc, _) => {
                self.focus_to(None);
                return Outcome::Redraw;
            }
            _ => {}
        }
        let dir = match k {
            Key::Left => hr::LEFT,
            Key::Right => hr::RIGHT,
            Key::Up => hr::UP,
            Key::Down => hr::DOWN,
            _ => return Outcome::Unused,
        };
        let from = self.items[f].1;
        let best = self
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != f)
            .map(|(i, (_, r))| {
                (
                    i,
                    hr::arrow_score(dir, q(from.cx()), q(from.cy()), q(r.cx()), q(r.cy())),
                )
            })
            .filter(|(_, s)| *s != 0)
            .min_by_key(|(_, s)| *s);
        match best {
            Some((i, _)) => {
                self.focus_to(Some(i));
                if let Item::Day(d) = self.items[i].0 {
                    self.select_day(now, d);
                }
                Outcome::Redraw
            }
            None => Outcome::None,
        }
    }

    /// A pointer event in dp.
    pub fn pointer(&mut self, now: Secs, ev: Pointer) -> Outcome {
        self.last_input = now;
        match ev {
            Pointer::Move(x, y) => {
                if let Some(v) = self.volume.value_at(x, y)
                    && self
                        .pressed
                        .is_some_and(|i| self.items.get(i).is_some_and(|it| it.0 == Item::Volume))
                {
                    return Outcome::Volume(v);
                }
                let at = self.item_at(x, y);
                self.set_hover(now, at);
                Outcome::None
            }
            Pointer::Down(x, y) => {
                if self.focus.is_some() {
                    self.focus_to(None);
                }
                let at = self.item_at(x, y);
                self.pressed = at;
                if let Some(i) = at {
                    self.press(now, i);
                    if self.items[i].0 == Item::Volume
                        && let Some(v) = self.volume.value_at(x, y)
                    {
                        return Outcome::Volume(v);
                    }
                }
                Outcome::Redraw
            }
            Pointer::Up(x, y) => {
                let Some(i) = self.pressed.take() else {
                    return Outcome::None;
                };
                self.release(now, i);
                if self.item_at(x, y) == Some(i) {
                    self.activate(now, i)
                } else {
                    Outcome::Redraw
                }
            }
        }
    }

    /// Note input that the home did not take (keeps ambient motion going).
    pub fn poke(&mut self, now: Secs) {
        self.last_input = now;
    }
}

/// The status bar: the clock on the left; the network and the battery on the right, only as far as they are known.
fn status_bar(p: &mut Paint, r: R, s: &StatusBar) {
    let ink = color::INK;
    let by = baseline_for(p, Font::Semibold, 15.0, r.cy());
    let cw = p.text(Font::Semibold, (15.0, -0.2), r.x + 26.0, by, ink, &s.clock);
    if s.camera {
        p.cv.fill_circle(r.x + 26.0 + cw + 12.0, r.cy(), 4.0, color::LIVE);
    }
    let mut right = r.right() - 22.0;
    if let Some(level) = s.battery {
        let b = R::new(right - 25.0, r.cy() - 6.0, 23.0, 12.0);
        p.cv.stroke_rrect(b, 3.5, 1.0, ink.alpha(0.45));
        p.cv.fill_rrect(
            R::new(b.right() + 1.0, b.cy() - 2.0, 1.6, 4.0),
            0.8,
            ink.alpha(0.45),
        );
        let inner = b.inset(2.0);
        let fill = if level <= 10 && !s.charging {
            color::LIVE
        } else if s.charging {
            color::POSITIVE
        } else {
            ink
        };
        p.cv.fill_rrect(
            R::new(
                inner.x,
                inner.y,
                inner.w * level.min(100) as f32 / 100.0,
                inner.h,
            ),
            2.0,
            fill,
        );
        let txt = format!("{level}%");
        let tw = p.measure(Font::Semibold, (13.0, 0.0), &txt);
        p.text(
            Font::Semibold,
            (13.0, 0.0),
            b.x - 8.0 - tw,
            baseline_for(p, Font::Semibold, 13.0, r.cy()),
            ink,
            &txt,
        );
        right = b.x - 16.0 - tw;
    }
    if s.online {
        let (wx, wy) = (right - 10.0, r.cy() + 4.5);
        p.cv.fill_circle(wx, wy - 1.5, 1.8, ink);
        for rad in [7.0f32, 10.5] {
            p.cv.arc(wx, wy, rad, 1.9, -0.75, 1.5, ink);
        }
    }
}

/// Rectangles that overlap or nearly touch become one, so a frame copies fewer, larger blocks.
fn merge(mut v: Vec<R>) -> Vec<R> {
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..v.len() {
            for j in i + 1..v.len() {
                let (a, b) = (v[i], v[j]);
                let near =
                    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom();
                if near {
                    let x = a.x.min(b.x);
                    let y = a.y.min(b.y);
                    v[i] = R::new(
                        x,
                        y,
                        a.right().max(b.right()) - x,
                        a.bottom().max(b.bottom()) - y,
                    );
                    v.remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    v.into_iter()
        .map(|r| {
            let x = r.x.max(0.0);
            let y = r.y.max(0.0);
            R::new(
                x,
                y,
                (r.right().min(W) - x).max(0.0),
                (r.bottom().min(H) - y).max(0.0),
            )
        })
        .filter(|r| r.w > 0.0 && r.h > 0.0)
        .collect()
}

/// screen-forward and screen-back, by whole pixels so they keep up with the panel: the arriving page `front` slides in over the
/// leaving one `back`, which drifts a quarter of the way out and dims. `t` is 0..1 of the move, `dir` 1 when the new page comes
/// from the right. All three buffers are `w` x `h` XRGB pixels.
pub fn slide_compose(
    out: &mut [u32],
    back: &[u32],
    front: &[u32],
    w: usize,
    h: usize,
    t: f32,
    dir: i32,
) {
    let k = Ease::EMPHASIZED.at(t.clamp(0.0, 1.0));
    let unit = (k * 65536.0).round().clamp(0.0, 65536.0) as u32;
    let off = hr::slide(w as u32, unit) as usize;
    let drift = (w as u32 / 4 - hr::slide(w as u32 / 4, unit)) as usize;
    // The page behind darkens to 45 % as the new one covers it.
    let dim = 256 - (141.0 * k) as u32;
    for y in 0..h {
        let row = y * w;
        let (o, b, f) = (
            &mut out[row..row + w],
            &back[row..row + w],
            &front[row..row + w],
        );
        // Whole slices, so the darkening loop vectorises: it runs over two million pixels a frame.
        let drift = drift.min(w);
        if dir >= 0 {
            // The front page's left edge is at `off`; the back page has moved `drift` to the left.
            let edge = off.min(w);
            let shown = edge.min(w - drift);
            for (o, v) in o[..shown].iter_mut().zip(&b[drift..drift + shown]) {
                *o = darken(*v, dim);
            }
            o[shown..edge].fill(0);
            o[edge..w].copy_from_slice(&f[..w - edge]);
        } else {
            let edge = w - off.min(w);
            o[..edge].copy_from_slice(&f[off.min(w)..w]);
            let start = edge.max(drift);
            o[edge..start].fill(0);
            for (o, v) in o[start..w].iter_mut().zip(&b[start - drift..w - drift]) {
                *o = darken(*v, dim);
            }
        }
    }
}

#[inline]
fn darken(v: u32, k: u32) -> u32 {
    let rb = (((v & 0x00ff_00ff) * k) >> 8) & 0x00ff_00ff;
    let g = (((v & 0x0000_ff00) * k) >> 8) & 0x0000_ff00;
    rb | g
}

/// The glass's blurred copy of a backdrop at half its size (the glass samples it at half the coordinates): made at a quarter of
/// the size and scaled up once. The blur hides both, and the copy is a quarter of the bytes to keep and read.
pub fn quick_blur(img: &Canvas) -> Canvas {
    let (w, h) = (img.w.div_ceil(4), img.h.div_ceil(4));
    let small = img.resized(w, h);
    let r = (crate::tokens::glass::BLUR * img.k / 3.0 / 4.0)
        .round()
        .max(1.0) as u32;
    let b = small.blurred(r);
    let mut out = b.resized(img.w / 2, img.h / 2);
    out.k = img.k / 2.0;
    out
}

/// The home's backdrop for a `w` x `h` panel: the night sky, and its starless copy at half the size for the glass. Made in a few
/// tens of milliseconds, with nothing to read from a disk that is still cold at boot.
pub fn sky_backdrop(w: u32, h: u32, k: f32) -> (Canvas, Canvas) {
    (
        crate::imagery::night_sky(w, h, k, 7),
        crate::imagery::night_sky_ground(w / 2, h / 2, k / 2.0, 7),
    )
}

/// The first frame's backdrop: the sky's ground with nothing on it, and its half-size copy for the glass. A fill per row; the
/// sky ([`sky_backdrop`]) replaces it right after the first frame.
pub fn plain_backdrop(w: u32, h: u32, k: f32) -> (Canvas, Canvas) {
    // One pixel wide: a colour per row is all a vertical ground needs, and the glass reads it by row. The glass's copy is the
    // same column, at full height.
    let _ = w;
    let g = crate::imagery::night_ground(1, h, k);
    (g.clone(), g)
}

/// The backdrop's file names for a panel: the picture, and its blurred copy for the glass (at half the size).
pub fn backdrop_names(w: u32, h: u32) -> (String, String) {
    (format!("sky-{w}x{h}.raw"), format!("sky-{w}x{h}-blur.raw"))
}

/// A canvas as raw little-endian `0x00RRGGBB` pixels, as the backdrop files hold them.
pub fn raw_bytes(c: &Canvas) -> Vec<u8> {
    c.px.iter().flat_map(|p| p.to_le_bytes()).collect()
}

/// A `w` x `h` canvas read from a raw backdrop file straight into its pixels: one read, no per-pixel work (the file is the
/// pixels' own little-endian bytes). `None` unless the file is exactly that size.
pub fn read_raw(path: &std::path::Path, w: u32, h: u32, k: f32) -> Option<Canvas> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let n = w as usize * h as usize;
    if f.metadata().ok()?.len() != n as u64 * 4 {
        return None;
    }
    let mut c = Canvas::new(w, h, k);
    // SAFETY: the u32 pixels are plain memory with no invalid bit patterns; the byte view covers exactly their storage and is
    // only written through by `read_exact`. Juno and every build host are little-endian, which `raw_bytes` writes.
    let bytes = unsafe { std::slice::from_raw_parts_mut(c.px.as_mut_ptr().cast::<u8>(), n * 4) };
    f.read_exact(bytes).ok()?;
    if cfg!(target_endian = "big") {
        for p in &mut c.px {
            *p = u32::from_le(*p);
        }
    }
    Some(c)
}

#[cfg(test)]
mod tests;
