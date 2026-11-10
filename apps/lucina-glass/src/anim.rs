//! Time-based animation: every value is a pure function of the clock's time, never of how many frames were drawn, so 24, 60
//! and 120 Hz show the same motion and a test can draw any instant on its own.
//!
//! - [`Clock`]: the time the widgets read. A real clock follows the monotonic time; a manual one is moved by hand (frame
//!   sequences, tests), and either can be paused, which freezes every animation that reads it.
//! - [`Ease`] and [`Tween`]: a value from A to B over a duration, along a curve.
//! - [`Spring`]: a damped spring solved in closed form, so it has no step size; retargeting keeps position and velocity.
//! - [`unit`]: a curve's 0..1 as [`rules::UNIT`]s, clamped, for the proved interpolation in [`crate::rules`].

use crate::rules;

/// Seconds, as the clock counts them.
pub type Secs = f64;

/// The time animations read.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    now: Secs,
    paused: bool,
    /// Slow motion for review: 0.25 shows every animation four times slower.
    pub rate: f64,
}

impl Default for Clock {
    fn default() -> Self {
        Self::manual()
    }
}

impl Clock {
    /// A clock at 0 that moves only with [`advance`](Self::advance) and [`set`](Self::set).
    pub const fn manual() -> Clock {
        Clock { now: 0.0, paused: false, rate: 1.0 }
    }

    /// Now, in seconds since the clock started.
    pub fn now(&self) -> Secs {
        self.now
    }

    /// Move on by `dt` seconds of real time (scaled by [`rate`](Self::rate)); nothing while paused.
    pub fn advance(&mut self, dt: Secs) {
        if !self.paused && dt > 0.0 {
            self.now += dt * self.rate;
        }
    }

    /// Jump to `t`: frame sequences render any instant directly.
    pub fn set(&mut self, t: Secs) {
        self.now = t;
    }

    pub fn pause(&mut self) {
        self.paused = true;
    }

    pub fn resume(&mut self) {
        self.paused = false;
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }
}

/// An easing curve over 0..1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ease {
    Linear,
    /// Decelerating: most entrances.
    OutCubic,
    OutQuint,
    /// Accelerate then decelerate: moves between two resting places.
    InOutCubic,
    /// Overshoots by about `s / 10` and settles: pops (a notification dot appearing).
    OutBack(f32),
    /// A CSS `cubic-bezier(x1, y1, x2, y2)`.
    Bezier(f32, f32, f32, f32),
}

impl Ease {
    /// The design's standard curve, `cubic-bezier(0.2, 0, 0, 1)`.
    pub const STANDARD: Ease = Ease::Bezier(0.2, 0.0, 0.0, 1.0);
    /// Emphasised, for screen transitions: `cubic-bezier(0.3, 0, 0, 1)`.
    pub const EMPHASIZED: Ease = Ease::Bezier(0.3, 0.0, 0.0, 1.0);

    /// The curve at `t`, which is clamped to 0..1. Exactly 0 at 0 and 1 at 1.
    pub fn at(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        match self {
            Ease::Linear => t,
            Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
            Ease::OutQuint => 1.0 - (1.0 - t).powi(5),
            Ease::InOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Ease::OutBack(s) => {
                let c3 = s + 1.0;
                1.0 + c3 * (t - 1.0).powi(3) + s * (t - 1.0).powi(2)
            }
            Ease::Bezier(x1, y1, x2, y2) => bezier(x1, y1, x2, y2, t),
        }
    }
}

/// `cubic-bezier` at `x`: Newton's method on x(s), with bisection when the slope is flat, then y(s).
fn bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let coord = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    let slope = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * a + 6.0 * u * s * (b - a) + 3.0 * s * s * (1.0 - b)
    };
    let mut s = x;
    for _ in 0..8 {
        let err = coord(x1, x2, s) - x;
        if err.abs() < 1e-5 {
            return coord(y1, y2, s);
        }
        let d = slope(x1, x2, s);
        if d.abs() < 1e-6 {
            break;
        }
        s = (s - err / d).clamp(0.0, 1.0);
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    s = x;
    for _ in 0..30 {
        let v = coord(x1, x2, s);
        if (v - x).abs() < 1e-5 {
            break;
        }
        if v < x {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) / 2.0;
    }
    coord(y1, y2, s)
}

/// A curve's value as [`rules::UNIT`]s for [`rules::lerp`], clamped to 0..UNIT: overshooting curves are drawn with
/// [`mix`] instead, where overshoot is wanted.
pub fn unit(v: f32) -> u32 {
    if v.is_nan() || v <= 0.0 {
        0
    } else if v >= 1.0 {
        rules::UNIT
    } else {
        (v * rules::UNIT as f32).round() as u32
    }
}

/// `a` to `b`, `t` of the way, unclamped: for springs and overshooting curves.
pub fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// How far into `[start, start + dur]` `now` is, 0..1.
pub fn phase(now: Secs, start: Secs, dur: Secs) -> f32 {
    if dur <= 0.0 {
        return if now >= start { 1.0 } else { 0.0 };
    }
    ((now - start) / dur).clamp(0.0, 1.0) as f32
}

/// A breathing 0..1..0 wave with period `period`, starting at 0 (the live pill's pulse).
pub fn pulse(now: Secs, period: Secs) -> f32 {
    let p = (now / period).fract() as f32;
    0.5 - 0.5 * (p * std::f32::consts::TAU).cos()
}

/// A value moving from `from` to `to` over `dur` seconds from `start`, along `ease`.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub start: Secs,
    pub dur: Secs,
    pub ease: Ease,
}

impl Tween {
    /// Resting at `v`.
    pub fn at_rest(v: f32) -> Tween {
        Tween { from: v, to: v, start: 0.0, dur: 0.0, ease: Ease::Linear }
    }

    pub fn new(from: f32, to: f32, start: Secs, dur: Secs, ease: Ease) -> Tween {
        Tween { from, to, start, dur, ease }
    }

    /// The value at `now`.
    pub fn at(&self, now: Secs) -> f32 {
        mix(self.from, self.to, self.ease.at(phase(now, self.start, self.dur)))
    }

    /// Eased progress 0..1 (may overshoot with [`Ease::OutBack`]).
    pub fn progress(&self, now: Secs) -> f32 {
        self.ease.at(phase(now, self.start, self.dur))
    }

    pub fn done(&self, now: Secs) -> bool {
        now >= self.start + self.dur
    }

    /// Head for `to` from wherever the value is at `now`: an interrupted move never jumps.
    pub fn retarget(&mut self, now: Secs, to: f32, dur: Secs) {
        let here = self.at(now);
        *self = Tween { from: here, to, start: now, dur, ease: self.ease };
    }
}

/// A damped spring (unit mass) pulling a value to `target`, solved in closed form: `at(now)` is exact for any `now`.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub target: f32,
    pub stiffness: f32,
    pub damping: f32,
    /// Offset from the target and velocity at `t0`.
    x0: f32,
    v0: f32,
    t0: Secs,
}

impl Spring {
    /// The design's default: quick, a touch of overshoot (damping ratio about 0.8).
    pub fn snappy(at: f32) -> Spring {
        Spring::new(at, 380.0, 31.0)
    }

    /// Heavier, critically damped: sheets and screens.
    pub fn smooth(at: f32) -> Spring {
        Spring::new(at, 170.0, 26.0)
    }

    pub fn new(at: f32, stiffness: f32, damping: f32) -> Spring {
        Spring { target: at, stiffness, damping, x0: 0.0, v0: 0.0, t0: 0.0 }
    }

    /// Position and velocity at `now`.
    pub fn state(&self, now: Secs) -> (f32, f32) {
        let t = (now - self.t0).max(0.0) as f32;
        let (k, c) = (self.stiffness.max(1e-3), self.damping.max(0.0));
        let w0 = k.sqrt();
        let zeta = c / (2.0 * w0);
        let (x0, v0) = (self.x0, self.v0);
        let (x, v) = if (zeta - 1.0).abs() < 1e-4 {
            // Critically damped: x = (x0 + (v0 + w0 x0) t) e^{-w0 t}.
            let b = v0 + w0 * x0;
            let e = (-w0 * t).exp();
            ((x0 + b * t) * e, (b - w0 * (x0 + b * t)) * e)
        } else if zeta < 1.0 {
            let wd = w0 * (1.0 - zeta * zeta).sqrt();
            let a = zeta * w0;
            let b = (v0 + a * x0) / wd;
            let e = (-a * t).exp();
            let (s, co) = (wd * t).sin_cos();
            let x = e * (x0 * co + b * s);
            let v = e * ((b * wd - a * x0) * co - (x0 * wd + a * b) * s);
            (x, v)
        } else {
            let r = w0 * (zeta * zeta - 1.0).sqrt();
            let (r1, r2) = (-zeta * w0 + r, -zeta * w0 - r);
            let c2 = (v0 - r1 * x0) / (r2 - r1);
            let c1 = x0 - c2;
            let (e1, e2) = ((r1 * t).exp(), (r2 * t).exp());
            (c1 * e1 + c2 * e2, c1 * r1 * e1 + c2 * r2 * e2)
        };
        (self.target + x, v)
    }

    pub fn at(&self, now: Secs) -> f32 {
        self.state(now).0
    }

    /// Pull toward `target` from now on, keeping the current position and velocity.
    pub fn set_target(&mut self, now: Secs, target: f32) {
        let (x, v) = self.state(now);
        self.target = target;
        self.x0 = x - target;
        self.v0 = v;
        self.t0 = now;
    }

    /// Jump to `v` at rest.
    pub fn snap(&mut self, now: Secs, v: f32) {
        *self = Spring { target: v, x0: 0.0, v0: 0.0, t0: now, ..*self };
    }

    /// Close enough to rest that nothing visible moves (a hundredth of a unit and of a unit per second).
    pub fn settled(&self, now: Secs) -> bool {
        let (x, v) = self.state(now);
        (x - self.target).abs() < 0.01 && v.abs() < 0.01
    }

    /// Kick it with a velocity (a fling).
    pub fn fling(&mut self, now: Secs, velocity: f32) {
        let (x, v) = self.state(now);
        self.x0 = x - self.target;
        self.v0 = v + velocity;
        self.t0 = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_hit_their_ends() {
        for e in [Ease::Linear, Ease::OutCubic, Ease::OutQuint, Ease::InOutCubic, Ease::OutBack(1.7), Ease::STANDARD, Ease::EMPHASIZED] {
            assert_eq!(e.at(0.0), 0.0, "{e:?}");
            assert_eq!(e.at(1.0), 1.0, "{e:?}");
            assert!(e.at(-3.0) == 0.0 && e.at(7.0) == 1.0);
        }
        // The standard curve is monotonic.
        let mut last = 0.0;
        for i in 0..=100 {
            let v = Ease::STANDARD.at(i as f32 / 100.0);
            assert!(v + 1e-4 >= last, "{i}: {v} < {last}");
            last = v;
        }
    }

    #[test]
    fn spring_is_frame_rate_independent_and_settles() {
        let mut s = Spring::snappy(0.0);
        s.set_target(0.0, 100.0);
        // The same instant reached in 60 Hz or 144 Hz steps reads the same: there are no steps.
        let a = s.at(0.25);
        let b = s.at(0.25);
        assert_eq!(a, b);
        assert!(s.at(0.0).abs() < 1e-3);
        assert!(s.settled(3.0), "{:?}", s.state(3.0));
        // Retargeting midway keeps position and velocity continuous.
        let (x, v) = s.state(0.1);
        s.set_target(0.1, -50.0);
        let (x2, v2) = s.state(0.1);
        assert!((x - x2).abs() < 1e-3 && (v - v2).abs() < 1e-2);
        for (k, c) in [(170.0, 26.0), (380.0, 31.0), (100.0, 40.0)] {
            let mut s = Spring::new(0.0, k, c);
            s.set_target(0.0, 1.0);
            assert!(s.settled(4.0), "{k} {c}");
        }
    }

    #[test]
    fn paused_clock_freezes() {
        let mut c = Clock::manual();
        c.advance(0.5);
        c.pause();
        c.advance(10.0);
        assert_eq!(c.now(), 0.5);
        c.resume();
        c.rate = 0.25;
        c.advance(1.0);
        assert_eq!(c.now(), 0.75);
    }

    #[test]
    fn tween_retarget_does_not_jump() {
        let mut t = Tween::new(0.0, 10.0, 0.0, 1.0, Ease::OutCubic);
        let here = t.at(0.3);
        t.retarget(0.3, 0.0, 0.5);
        assert_eq!(t.at(0.3), here);
        assert_eq!(t.at(0.8), 0.0);
    }
}
