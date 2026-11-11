//! Finger tracking for a swipe: axis lock, progress, velocity over a trailing window, and the release target.
//! Follows niri `swipe_tracker.rs` and the workspace switch in `layout/monitor.rs`, and GNOME Shell `swipeTracker.js`.
//! Deltas arrive in the gesture's own unit; time is monotonic microseconds. No I/O, no clock, no allocation.

use crate::snap_rules;

/// Trailing window (microseconds) over which velocity is measured (niri `HISTORY_LIMIT` 150 ms).
pub const HISTORY_US: u64 = 150_000;
/// Per-millisecond velocity decay of a touchpad fling (niri `DECELERATION_TOUCHPAD`).
pub const DECELERATION_TOUCHPAD: f64 = 0.997;
/// Axis lock distance: the gesture locks once `dx^2 + dy^2 >= LOCK_DISTANCE^2` (libinput units; the caller scales).
pub const LOCK_DISTANCE: f64 = 16.0;
/// Capacity of the velocity ring.
pub const MAX_SAMPLES: usize = 64;
/// Below this speed (index units per second) a release snaps to the closest point (GNOME `VELOCITY_THRESHOLD_TOUCHPAD`).
pub const SNAP_SPEED_THRESHOLD: f64 = 0.6;

/// The axis a swipe has locked to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Axis lock (niri gesture begin / GNOME `_getAxis`): feed deltas until the cumulative distance reaches [`LOCK_DISTANCE`];
/// then the larger of |cx|, |cy| decides. A tie goes to [`Axis::Horizontal`]. Once locked it stays locked.
#[derive(Clone, Copy, Debug)]
pub struct AxisLock {
    cx: f64,
    cy: f64,
    locked: Option<Axis>,
}

impl Default for AxisLock {
    fn default() -> Self {
        Self::new()
    }
}

impl AxisLock {
    /// A fresh lock with nothing accumulated.
    pub fn new() -> Self {
        Self {
            cx: 0.0,
            cy: 0.0,
            locked: None,
        }
    }

    /// Adds a delta; returns the axis from the update that reaches the lock distance on, and on every update after.
    pub fn update(&mut self, dx: f64, dy: f64) -> Option<Axis> {
        self.cx += dx;
        self.cy += dy;
        if self.locked.is_none()
            && self.cx * self.cx + self.cy * self.cy >= LOCK_DISTANCE * LOCK_DISTANCE
        {
            self.locked = Some(if libm::fabs(self.cx) >= libm::fabs(self.cy) {
                Axis::Horizontal
            } else {
                Axis::Vertical
            });
        }
        self.locked
    }

    /// Everything accumulated so far, so the tracker can start from it.
    pub fn cumulative(&self) -> (f64, f64) {
        (self.cx, self.cy)
    }
}

/// Velocity tracker over a ring of `(delta, time_us)` samples (niri `SwipeTracker`).
#[derive(Clone, Debug)]
pub struct Tracker {
    deltas: [f64; MAX_SAMPLES],
    times: [u64; MAX_SAMPLES],
    head: usize,
    len: usize,
    pos: f64,
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    /// An empty tracker at position 0.
    pub fn new() -> Self {
        Self {
            deltas: [0.0; MAX_SAMPLES],
            times: [0; MAX_SAMPLES],
            head: 0,
            len: 0,
            pos: 0.0,
        }
    }

    fn idx(&self, i: usize) -> usize {
        (self.head + i) % MAX_SAMPLES
    }

    /// Adds a delta. A timestamp older than the newest sample is ignored; samples older than `newest - HISTORY_US` are dropped.
    /// When the ring is full the oldest sample is overwritten.
    pub fn push(&mut self, delta: f64, time_us: u64) {
        if self.len > 0 && time_us < self.times[self.idx(self.len - 1)] {
            return;
        }
        self.pos += delta;
        if self.len == MAX_SAMPLES {
            self.head = (self.head + 1) % MAX_SAMPLES;
            self.len -= 1;
        }
        let slot = self.idx(self.len);
        self.deltas[slot] = delta;
        self.times[slot] = time_us;
        self.len += 1;
        while self.len > 1 && time_us - self.times[self.head] > HISTORY_US {
            self.head = (self.head + 1) % MAX_SAMPLES;
            self.len -= 1;
        }
    }

    /// Number of samples currently in the window.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no sample is held.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Sum of all deltas pushed (not only those still in the window).
    pub fn pos(&self) -> f64 {
        self.pos
    }

    /// Units per second over the window: sum of deltas divided by the time from the first to the last sample. Like GNOME, the first
    /// sample's delta is left out of the sum (it only marks the start of the span). 0 with fewer than two samples or a zero time span.
    pub fn velocity(&self) -> f64 {
        if self.len < 2 {
            return 0.0;
        }
        let first_t = self.times[self.head];
        let last_t = self.times[self.idx(self.len - 1)];
        if last_t <= first_t {
            return 0.0;
        }
        let mut sum = 0.0;
        for i in 1..self.len {
            sum += self.deltas[self.idx(i)];
        }
        sum / ((last_t - first_t) as f64 / 1_000_000.0)
    }

    /// Where the fling would stop (niri `projected_end_pos`). A velocity `v` in units per millisecond decaying by `d = 0.997` each
    /// millisecond travels `v * integral(d^t dt) = v / -ln(d)`, so with `velocity` in units per second:
    /// `pos + (velocity / 1000) / -ln(DECELERATION_TOUCHPAD)`.
    pub fn projected_end(&self) -> f64 {
        self.pos + fling_distance(self.velocity())
    }
}

/// Distance a fling of `velocity` (units per second) travels before it stops: `(velocity / 1000) / -ln(0.997)`.
fn fling_distance(velocity: f64) -> f64 {
    (velocity / 1000.0) / -libm::log(DECELERATION_TOUCHPAD)
}

/// Where a released swipe goes: the snap point index and the initial spring velocity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Release {
    pub target: usize,
    /// Initial spring velocity in index units per second, damped while rubber-banded.
    pub velocity: f64,
}

/// Release target (niri workspace switch + GNOME speed gate). `progress` is the position in snap-point index units (it may be outside
/// `[0, last]` while rubber-banded), `velocity` index units per second, `start` the point the gesture began on, `last` the highest index,
/// `long_swipes` whether more than one point from `start` may be passed.
/// Under [`SNAP_SPEED_THRESHOLD`] the target is the closest point to `progress`; otherwise it is
/// `round(progress + velocity / (1000 * -ln(0.997)))`. Without long swipes it stays within `start-1..=start+1`; never outside `0..=last`.
pub fn release_target(
    progress: f64,
    velocity: f64,
    start: usize,
    last: usize,
    long_swipes: bool,
) -> Release {
    let basis = if libm::fabs(velocity) < SNAP_SPEED_THRESHOLD {
        progress
    } else {
        progress + fling_distance(velocity)
    };
    // The float code ends here: the target is rounded to a whole point (a NaN becomes 0, an infinity the largest `i64`), and the
    // integer rule keeps it inside the points this gesture may reach.
    let rounded = libm::round(basis) as i64;
    let target = snap_rules::clamp_target(
        rounded,
        start.min(snap_rules::LIMIT),
        last.min(snap_rules::LIMIT),
        long_swipes,
    );
    Release {
        target,
        velocity: velocity
            * crate::rubber::Band::GESTURE.clamp_derivative(0.0, last as f64, progress),
    }
}

/// Overview open/close commit (ours, from KWin partial activation and niri's overview gesture). `progress` 0 = closed, 1 = open.
/// Closes when `velocity <= -1.5/s` and `progress <= 0.9`; otherwise opens when `progress >= 0.5`, or `velocity >= 1.5/s` and `progress >= 0.1`.
/// Returns true for open.
pub fn commit_overview(progress: f64, velocity: f64) -> bool {
    if velocity <= -1.5 && progress <= 0.9 {
        return false;
    }
    progress >= 0.5 || (velocity >= 1.5 && progress >= 0.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        libm::fabs(a - b) < 1e-6
    }

    #[test]
    fn axis_locks_on_horizontal_move() {
        let mut l = AxisLock::new();
        assert_eq!(l.update(3.0, 1.0), None);
        assert_eq!(l.update(0.0, 2.0), None);
        assert_eq!(l.update(20.0, 0.0), Some(Axis::Horizontal));
        assert_eq!(l.update(0.0, 100.0), Some(Axis::Horizontal));
        assert_eq!(l.cumulative(), (23.0, 103.0));
    }

    #[test]
    fn axis_diagonal_tie_is_horizontal() {
        let mut l = AxisLock::new();
        assert_eq!(l.update(12.0, 12.0), Some(Axis::Horizontal));
        let mut v = AxisLock::new();
        assert_eq!(v.update(5.0, 16.0), Some(Axis::Vertical));
    }

    fn steady() -> Tracker {
        let mut t = Tracker::new();
        for i in 0..=10u64 {
            t.push(2.0, i * 10_000);
        }
        t
    }

    #[test]
    fn velocity_steady() {
        assert!(close(steady().velocity(), 200.0));
    }

    #[test]
    fn out_of_order_ignored() {
        let mut t = steady();
        let pos = t.pos();
        t.push(50.0, 5_000);
        assert_eq!(t.pos(), pos);
        assert!(close(t.velocity(), 200.0));
    }

    #[test]
    fn window_drops_old_samples() {
        let mut t = Tracker::new();
        for i in 0..=30u64 {
            t.push(if i < 15 { 100.0 } else { 2.0 }, i * 10_000);
        }
        // newest 300 ms; kept: 150..=300 ms (16 samples)
        assert_eq!(t.len(), 16);
        assert!(close(t.velocity(), 200.0));
    }

    #[test]
    fn few_samples_zero_velocity() {
        let mut t = Tracker::new();
        assert_eq!(t.velocity(), 0.0);
        t.push(5.0, 1000);
        assert_eq!(t.velocity(), 0.0);
        t.push(5.0, 1000);
        assert_eq!(t.velocity(), 0.0);
    }

    #[test]
    fn ring_overflow_keeps_working() {
        let mut t = Tracker::new();
        for i in 0..200u64 {
            t.push(1.0, i * 100);
        }
        assert!(t.len() <= MAX_SAMPLES);
        assert!(close(t.velocity(), 10_000.0));
    }

    #[test]
    fn projected_end_cases() {
        let mut t = Tracker::new();
        t.push(1.0, 0);
        assert_eq!(t.projected_end(), t.pos());
        let s = steady();
        let expect = 22.0 + 200.0 / (1000.0 * -libm::log(0.997));
        assert!(close(s.projected_end(), expect));
    }

    #[test]
    fn release_slow_snaps_closest() {
        assert_eq!(release_target(0.4, 0.2, 0, 3, false).target, 0);
        assert_eq!(release_target(0.6, 0.2, 0, 3, false).target, 1);
    }

    #[test]
    fn release_fast() {
        assert_eq!(release_target(0.2, 3.0, 0, 3, false).target, 1);
        assert!(release_target(0.2, 10.0, 0, 5, false).target <= 1);
        assert_eq!(release_target(0.2, 10.0, 0, 3, true).target, 3);
        assert_eq!(release_target(0.2, 100.0, 0, 5, true).target, 5);
    }

    #[test]
    fn release_negative_and_clamped() {
        assert_eq!(release_target(1.8, -3.0, 2, 4, false).target, 1);
        assert_eq!(release_target(0.2, -10.0, 0, 3, true).target, 0);
        assert_eq!(release_target(2.8, 10.0, 2, 3, true).target, 3);
        assert_eq!(release_target(5.0, 0.0, 3, 3, true).target, 3);
    }

    #[test]
    fn release_velocity_damped_outside() {
        let r = release_target(0.5, 3.0, 0, 3, false);
        assert!(close(r.velocity, 3.0));
        let r = release_target(-0.25, -3.0, 0, 3, false);
        // niri clamp_derivative at 0.25 past the end: c d^2 / (c x + d)^2 with c = 0.5, d = 0.05.
        let slope = 0.5 * 0.05 * 0.05 / ((0.5 * 0.25 + 0.05) * (0.5 * 0.25 + 0.05));
        assert!(close(r.velocity, -3.0 * slope));
        assert_eq!(r.target, 0);
    }

    #[test]
    fn overview_commit() {
        assert!(commit_overview(0.5, 0.0));
        assert!(!commit_overview(0.49, 0.0));
        assert!(commit_overview(0.2, 1.5));
        assert!(!commit_overview(0.05, 2.0));
        assert!(!commit_overview(0.8, -1.5));
        assert!(commit_overview(0.95, -2.0));
        assert!(!commit_overview(0.3, -0.5));
    }
}
