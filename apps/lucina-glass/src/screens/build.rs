//! Screen 1, "Build": the night sky behind a glass header (LIVE pill, the thin title, the phase and its
//! percentage), a segmented control, a row of four actions, and a dark sheet with the upload, the timer and its ring,
//! the media controls, volume, two toggles and two stat cards. One fact per card (docs/design/rules.md).

use crate::anim::{Ease, Secs, Tween, phase};
use crate::canvas::{Canvas, Color, R};
use crate::icons::Icon;
use crate::imagery;
use crate::text::{Font, Type};
use crate::tokens::{color, radius, space, ty};
use crate::widgets::chrome;
use crate::widgets::{ActionButton, CountUp, LivePill, MediaControls, ProgressTrack, RoundButton, Segmented, TimerRing, Volume, cell_x};
use crate::Paint;

use super::{Backdrop, Pointer, Screen, content as c, home_indicator};

pub struct Build {
    bg: Option<Backdrop>,
    live: LivePill,
    track: ProgressTrack,
    percent: CountUp,
    seg: Segmented,
    pub actions: [ActionButton; 4],
    pub timer: TimerRing,
    media: MediaControls,
    volume: Volume,
    round: [RoundButton; 2],
    /// The sheet's entrance: it rises from below.
    sheet: Tween,
    pressed: Option<Target>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    Action(usize),
    Media(usize),
    Round(usize),
    Volume,
}

const SHEET_Y: f32 = 392.0;
const ACTION_ICONS: [Icon; 4] = [Icon::Doc, Icon::Mail, Icon::Clock, Icon::Share];

impl Build {
    pub fn new(now: Secs) -> Build {
        Build {
            bg: None,
            live: LivePill::new(c::LIVE, now),
            track: ProgressTrack::new(3, 2, 0.76),
            percent: CountUp::new(c::PHASE_PERCENT, now, "%"),
            seg: Segmented::new(&c::SEGMENTS, 0),
            actions: std::array::from_fn(|i| ActionButton::new(ACTION_ICONS[i], c::ACTIONS[i])),
            timer: TimerRing::new(c::TIMER_TOTAL, c::TIMER_LEFT, now),
            media: MediaControls::new(true),
            volume: Volume::new(0.62),
            round: [RoundButton::new(Icon::Heart), RoundButton::new(Icon::Repeat)],
            sheet: Tween::new(1.0, 0.0, now, 0.7, Ease::EMPHASIZED),
            pressed: None,
        }
    }
}

impl Screen for Build {
    fn name(&self) -> &'static str {
        "build"
    }

    fn enter(&mut self, now: Secs) {
        self.live.since = now;
        self.track.replay(now + 0.15, 0.76);
        self.percent.restart(now + 0.15, c::PHASE_PERCENT);
        self.sheet = Tween::new(1.0, 0.0, now, 0.7, Ease::EMPHASIZED);
        self.timer = TimerRing::new(c::TIMER_TOTAL, c::TIMER_LEFT, now);
    }

    fn act(&mut self, now: Secs, action: &str) -> bool {
        match action {
            "talks" => self.seg.select(now, 1),
            "work" => self.seg.select(now, 0),
            "press-play" => self.media.press(now, 2),
            "release-play" => {
                self.media.release(now, 2);
                if self.media.playing { self.timer.resume(now) } else { self.timer.pause(now) }
            }
            "hover-book" => self.actions[2].set_hover(now, true),
            "press-book" => self.actions[2].press(now),
            "release-book" => self.actions[2].release(now),
            "unhover-book" => self.actions[2].set_hover(now, false),
            "next-phase" => self.track.advance(now),
            "fill" => self.track.set_fill(now, 0.95),
            _ => return false,
        }
        true
    }

    fn pointer(&mut self, now: Secs, ev: Pointer) -> bool {
        match ev {
            Pointer::Move(x, y) => {
                for b in &mut self.actions {
                    let on = b.hit(x, y);
                    b.set_hover(now, on);
                }
                if self.pressed == Some(Target::Volume)
                    && let Some(v) = self.volume.value_at(x, y)
                {
                    self.volume.set(now, v);
                }
                true
            }
            Pointer::Down(x, y) => {
                if let Some(i) = self.seg.hit(x, y) {
                    self.seg.select(now, i);
                    return true;
                }
                let t = if let Some(i) = self.actions.iter().position(|b| b.hit(x, y)) {
                    self.actions[i].press(now);
                    Some(Target::Action(i))
                } else if let Some(i) = self.media.hit(x, y) {
                    self.media.press(now, i);
                    Some(Target::Media(i))
                } else if let Some(i) = self.round.iter().position(|b| b.hit(x, y)) {
                    self.round[i].press(now);
                    Some(Target::Round(i))
                } else if let Some(v) = self.volume.value_at(x, y) {
                    self.volume.set(now, v);
                    Some(Target::Volume)
                } else {
                    None
                };
                self.pressed = t;
                t.is_some()
            }
            Pointer::Up(_, _) => {
                match self.pressed.take() {
                    Some(Target::Action(i)) => self.actions[i].release(now),
                    Some(Target::Media(i)) => {
                        self.media.release(now, i);
                        if i == 2 {
                            if self.media.playing { self.timer.resume(now) } else { self.timer.pause(now) }
                        }
                    }
                    Some(Target::Round(i)) => self.round[i].release(now),
                    Some(Target::Volume) | None => {}
                }
                true
            }
        }
    }

    fn draw(&mut self, cv: &mut Canvas, ty: &mut Type, now: Secs) {
        Backdrop::ensure(&mut self.bg, cv, imagery::night_sky);
        let bg = self.bg.as_ref().expect("backdrop");
        cv.px.copy_from_slice(&bg.image.px);
        let full = cv.bounds();
        cv.scrim(R::new(0.0, 0.0, full.w, 90.0), Color::hexa(0x000000, 90), Color::hexa(0x000000, 0));
        let mut p = Paint { cv, ty, now, backdrop: Some(&bg.blurred) };
        chrome::status_bar(&mut p, R::new(0.0, 0.0, full.w, 46.0), color::INK);
        let x = space::EDGE;
        let w = full.w - 2.0 * space::EDGE;

        // The header: the title, LIVE, and one secondary line (the phase) with its track and percentage.
        let head = R::new(x, 52.0, w, 166.0);
        p.glass(head, radius::XL, color::MATERIAL);
        let (lw, _) = self.live.size(&mut p);
        self.live.draw(&mut p, head.right() - space::L - lw, head.y + space::L);
        p.text(Font::Thin, ty::DISPLAY, head.x + 10.0, head.y + 92.0, color::INK, c::BUILD_TITLE);
        p.text(Font::Regular, ty::CAPTION, head.x + space::L, head.bottom() - 22.0, color::INK_2, c::PHASE);
        let pw = p.measure(Font::Regular, ty::CAPTION, c::PHASE);
        let track = R::new(head.x + space::L + pw + space::M, head.bottom() - 31.0, head.w - pw - 124.0, 10.0);
        self.track.draw(&mut p, track);
        self.percent.draw(&mut p, head.right() - space::L, head.bottom() - 18.0, Font::Thin, 34.0, color::INK);

        // Segments and the action row: icons over one word.
        self.seg.draw(&mut p, R::new(x, 230.0, w, space::TAP));
        let row = R::new(x, 290.0, w, 64.0);
        // A soft dark band so the labels read on any backdrop.
        p.cv.fill_rrect_soft(R::new(-40.0, row.y - 8.0, full.w + 80.0, row.h + 16.0), 0.0, 36.0, Color::hexa(0x000814, 150));
        for (i, b) in self.actions.iter_mut().enumerate() {
            let (cx, cw) = cell_x(row, 4, space::M, i as u32, 1);
            b.draw(&mut p, cx + cw / 2.0, row.y);
        }

        // The sheet, rising in on entrance.
        let lift = self.sheet.at(now).max(0.0) * (full.h - SHEET_Y);
        let sheet = R::new(0.0, SHEET_Y + lift, full.w, full.h - SHEET_Y + 40.0);
        p.shadow(sheet, radius::XL, 22.0);
        p.glass(sheet, radius::XL, color::SMOKE);
        p.cv.push_clip(sheet, radius::XL);
        let o = p.cv.origin;
        p.cv.origin = (o.0, o.1 + lift);
        let s = R::new(0.0, SHEET_Y, full.w, full.h - SHEET_Y);
        p.cv.fill_rrect(R::new(s.cx() - 18.0, s.y + 8.0, 36.0, 4.0), 2.0, color::INK_3);
        let ux = s.x + space::EDGE + 6.0;
        chrome::spinner(&mut p, ux + 6.0, s.y + 34.0, 6.0, color::INK);
        p.text(Font::Regular, ty::CAPTION, ux + 20.0, s.y + 38.0, color::INK_2, c::UPLOADING);
        self.timer.draw_digits(&mut p, ux - 2.0, s.y + 96.0, 44.0, color::INK);
        self.timer.draw_ring(&mut p, s.right() - 58.0, s.y + 70.0, 30.0);
        self.media.draw(&mut p, R::new(s.x + space::EDGE, s.y + 118.0, s.w - 2.0 * space::EDGE, 50.0));
        self.volume.draw(&mut p, R::new(ux, s.y + 180.0, s.w - 140.0, 24.0));
        self.round[0].draw(&mut p, s.right() - 78.0, s.y + 192.0, 18.0);
        self.round[1].draw(&mut p, s.right() - 34.0, s.y + 192.0, 18.0);
        let tiles = R::new(s.x + space::EDGE, s.y + 226.0, s.w - 2.0 * space::EDGE, s.h - 226.0 - 22.0);
        p.backdrop = None;
        let k = Ease::OutCubic.at(phase(now, self.sheet.start + 0.35, 0.5));
        let oo = p.cv.opacity;
        p.cv.opacity = oo * k;
        let icons = [Icon::Person, Icon::Heart];
        for i in 0..2u32 {
            let (tx, tw) = cell_x(tiles, 2, space::M, i, 1);
            let t = R::new(tx, tiles.y + (10.0 + 4.0 * i as f32) * (1.0 - k), tw, tiles.h);
            chrome::stat_tile(&mut p, t, icons[i as usize], c::TILES[i as usize].0, c::TILES[i as usize].1);
        }
        p.cv.opacity = oo;
        p.cv.origin = o;
        p.cv.pop_clip();
        home_indicator(p.cv, color::INK);
    }
}
