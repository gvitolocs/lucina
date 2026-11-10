//! The three screens on one stage, the move between them, and the demos: named, timed scripts of actions that show one
//! component's animation, shared by the live gallery and the headless renderer.

use lucina_glass::screens::{Agenda, Build, Notification, Pointer, Screen, Transition};
use lucina_glass::{Canvas, R, Secs, Type};

pub struct Stage {
    pub screens: Vec<Box<dyn Screen>>,
    pub current: usize,
    /// While moving: the screen being left and the move.
    pub moving: Option<(usize, Transition)>,
    pub ty: Type,
    scratch: Option<(Canvas, Canvas)>,
}

impl Stage {
    pub fn new(now: Secs) -> Stage {
        Stage {
            screens: vec![Box::new(Build::new(now)), Box::new(Agenda::new(now)), Box::new(Notification::new(now))],
            current: 0,
            moving: None,
            ty: Type::new(),
            scratch: None,
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.screens.iter().map(|s| s.name()).collect()
    }

    /// Move to screen `to`, sliding in from the right when it is further along.
    pub fn go(&mut self, now: Secs, to: usize) {
        if to >= self.screens.len() || to == self.current {
            return;
        }
        let dir = if to > self.current { 1.0 } else { -1.0 };
        self.moving = Some((self.current, Transition::new(now, dir)));
        self.current = to;
        self.screens[to].enter(now + 0.12);
    }

    /// Draw the stage at `now` into `cv`.
    pub fn draw(&mut self, cv: &mut Canvas, now: Secs) {
        if let Some((_, t)) = self.moving
            && t.done(now)
        {
            self.moving = None;
        }
        match self.moving {
            None => self.screens[self.current].draw(cv, &mut self.ty, now),
            Some((from, t)) => {
                if self.scratch.as_ref().is_none_or(|(a, _)| a.w != cv.w || a.h != cv.h || a.k != cv.k) {
                    self.scratch = Some((Canvas::new(cv.w, cv.h, cv.k), Canvas::new(cv.w, cv.h, cv.k)));
                }
                let (a, b) = self.scratch.as_mut().expect("scratch");
                self.screens[from].draw(a, &mut self.ty, now);
                self.screens[self.current].draw(b, &mut self.ty, now);
                t.compose(cv, a, b, now);
            }
        }
    }

    pub fn pointer(&mut self, now: Secs, ev: Pointer) -> bool {
        if self.moving.is_some() {
            return false;
        }
        self.screens[self.current].pointer(now, ev)
    }

    /// One step of a demo: `go:N` moves to screen N, `enter` replays the entrance, anything else is the screen's own action.
    pub fn act(&mut self, now: Secs, action: &str) -> bool {
        if let Some(n) = action.strip_prefix("go:") {
            if let Ok(i) = n.parse() {
                self.go(now, i);
                return true;
            }
            return false;
        }
        if action == "enter" {
            self.screens[self.current].enter(now);
            return true;
        }
        self.screens[self.current].act(now, action)
    }
}

/// A demo: which screen, the part of it to look at (dp), how long, and what happens when.
pub struct Demo {
    pub name: &'static str,
    pub what: &'static str,
    pub screen: usize,
    pub crop: R,
    pub dur: Secs,
    pub steps: &'static [(Secs, &'static str)],
}

/// When demos start: long enough after the stage is made that every entrance has settled.
pub const SETTLED: Secs = 3.0;

pub const DEMOS: &[Demo] = &[
    Demo { name: "live-pill", what: "LIVE! pill: halo swells and fades, dot breathes", screen: 0, crop: R::new(220.0, 52.0, 140.0, 64.0), dur: 1.6, steps: &[] },
    Demo {
        name: "progress-count-up",
        what: "Phase track fills, its head dot leads; 76% counts up",
        screen: 0,
        crop: R::new(12.0, 170.0, 336.0, 56.0),
        dur: 1.5,
        steps: &[(0.0, "enter")],
    },
    Demo { name: "phase-advance", what: "Next phase: the dot grows into a pill", screen: 0, crop: R::new(12.0, 170.0, 336.0, 56.0), dur: 0.9, steps: &[(0.1, "next-phase"), (0.3, "fill")] },
    Demo { name: "segmented", what: "Segmented control: thumb slides on a spring, labels brighten", screen: 0, crop: R::new(8.0, 224.0, 344.0, 56.0), dur: 1.0, steps: &[(0.1, "talks"), (0.6, "work")] },
    Demo {
        name: "action-button",
        what: "Action row: hover lifts and rims a button, press shrinks it into the accent",
        screen: 0,
        crop: R::new(8.0, 284.0, 344.0, 76.0),
        dur: 1.3,
        steps: &[(0.1, "hover-book"), (0.55, "press-book"), (0.75, "release-book"), (1.1, "unhover-book")],
    },
    Demo { name: "timer-ring", what: "Timer: digits roll each second, the ring sweeps", screen: 0, crop: R::new(0.0, 420.0, 360.0, 92.0), dur: 2.2, steps: &[] },
    Demo { name: "media-press", what: "Media pill: press shrinks and lights, play turns to pause", screen: 0, crop: R::new(0.0, 500.0, 360.0, 70.0), dur: 1.0, steps: &[(0.1, "press-play"), (0.3, "release-play")] },
    Demo { name: "sheet-rise", what: "Bottom sheet rises, stat tiles fade up", screen: 0, crop: R::new(0.0, 0.0, 360.0, 800.0), dur: 1.3, steps: &[(0.0, "enter")] },
    Demo { name: "day-chips", what: "Day chips: the red pill stretches across and contracts", screen: 1, crop: R::new(8.0, 314.0, 344.0, 52.0), dur: 1.1, steps: &[(0.1, "select-thu"), (0.65, "select-mon")] },
    Demo { name: "calendar-stagger", what: "Calendar tiles rise in, staggered; dots pop; 51% counts up", screen: 1, crop: R::new(0.0, 362.0, 360.0, 438.0), dur: 1.6, steps: &[(0.0, "replay")] },
    Demo { name: "card-stack", what: "Notification stack: top card thrown, the rest move up", screen: 2, crop: R::new(0.0, 500.0, 360.0, 140.0), dur: 1.2, steps: &[(0.1, "swipe")] },
    Demo { name: "pager-dots", what: "Pager: width passes from dot to dot, row width constant", screen: 2, crop: R::new(8.0, 672.0, 200.0, 36.0), dur: 0.9, steps: &[(0.1, "page-4"), (0.5, "page-2")] },
    Demo { name: "vertical-slider", what: "Rail slider: the thumb steps on a spring", screen: 2, crop: R::new(10.0, 84.0, 64.0, 224.0), dur: 1.0, steps: &[(0.1, "slider-down"), (0.5, "slider-down")] },
    Demo { name: "notification-enter", what: "The title rises in", screen: 2, crop: R::new(0.0, 420.0, 360.0, 80.0), dur: 1.0, steps: &[(0.0, "enter")] },
    Demo { name: "screen-forward", what: "Build to the agenda: the new screen slides over, the old sinks back", screen: 0, crop: R::new(0.0, 0.0, 360.0, 800.0), dur: 0.9, steps: &[(0.05, "go:1")] },
    Demo { name: "screen-back", what: "Notification back to the agenda", screen: 2, crop: R::new(0.0, 0.0, 360.0, 800.0), dur: 0.9, steps: &[(0.05, "go:1")] },
];

/// A stage at rest on `demo`'s screen at [`SETTLED`].
pub fn stage_for(demo: &Demo) -> Stage {
    let mut s = Stage::new(0.0);
    s.current = demo.screen;
    s.screens[demo.screen].enter(0.0);
    s
}

/// Copy the dp rectangle `r` of `cv` into a new canvas.
pub fn crop(cv: &Canvas, r: R) -> Canvas {
    let x0 = (r.x * cv.k).round().max(0.0) as u32;
    let y0 = (r.y * cv.k).round().max(0.0) as u32;
    let w = ((r.w * cv.k).round() as u32).min(cv.w - x0);
    let h = ((r.h * cv.k).round() as u32).min(cv.h - y0);
    let mut out = Canvas::new(w, h, cv.k);
    for y in 0..h {
        let src = ((y0 + y) * cv.w + x0) as usize;
        out.px[(y * w) as usize..((y + 1) * w) as usize].copy_from_slice(&cv.px[src..src + w as usize]);
    }
    out
}

/// The frames of `demo` at `fps`, each cropped, drawn at `k` pixels per dp. Steps happen at their exact times, whatever the
/// frame rate.
pub fn frames(demo: &Demo, k: f32, fps: f64) -> Vec<Canvas> {
    let mut stage = stage_for(demo);
    let mut cv = Canvas::new((360.0 * k) as u32, (800.0 * k) as u32, k);
    let n = (demo.dur * fps).ceil() as usize + 1;
    let mut next = 0;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / fps;
        while next < demo.steps.len() && demo.steps[next].0 <= t {
            let (at, a) = demo.steps[next];
            assert!(stage.act(SETTLED + at, a), "demo {}: unknown action {a}", demo.name);
            next += 1;
        }
        stage.draw(&mut cv, SETTLED + t);
        out.push(crop(&cv, demo.crop));
    }
    out
}
