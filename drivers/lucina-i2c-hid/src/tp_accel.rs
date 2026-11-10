//! libinput 1.28.1's touchpad pointer acceleration (`src/filter-touchpad.c` with the trackers of `src/filter.c`),
//! ported 1:1 for a touchpad on I2C: `tp_init_accel` (evdev-mt-touchpad.c) builds it with
//! `use_velocity_averaging = false` (2 trackers) and event-delta smoothing threshold/value 0 (only Bluetooth pads
//! get 50 ms/10 ms), at the pad's own dpi, with libinput's default speed setting 0.
//!
//! The caller scales deltas to the x axis' resolution first (libinput's `tp_scale_to_xaxis`);
//! [`TouchpadAccel::filter`] then accelerates in device units and normalizes to [`DEFAULT_MOUSE_DPI`] afterwards,
//! exactly as `accelerator_filter_touchpad` does.

/// `TP_MAGIC_SLOWDOWN` (filter-touchpad.c): once normalized, touchpads get the mouse acceleration slowed down.
const TP_MAGIC_SLOWDOWN: f64 = 0.2968;
/// The `baseline` of `touchpad_accel_profile_linear` and `touchpad_constant_filter`.
const BASELINE: f64 = 0.9;
/// The threshold in mm/s, set by `create_pointer_accelerator_filter_touchpad`.
const THRESHOLD_MM_S: f64 = 130.0;
/// `DEFAULT_MOUSE_DPI` (libinput-util.h).
const DEFAULT_MOUSE_DPI: f64 = 1000.0;
/// `MOTION_TIMEOUT` = `ms2us(1000)` (filter.c).
const MOTION_TIMEOUT: u64 = 1_000_000;
/// `MAX_VELOCITY_DIFF` = `v_ms2us(1)` (filter.c), in units/us.
const MAX_VELOCITY_DIFF: f64 = 0.001;
/// `trackers_init(&filter->trackers, use_velocity_averaging ? 16 : 2)`: this pad is on I2C, so 2.
const NTRACKERS: usize = 2;
/// `UNDEFINED_DIRECTION` (libinput-private.h).
const UNDEFINED_DIRECTION: u32 = 0xff;

// `enum directions` (libinput-private.h), with `bit(n)` = 1 << n (util-bits.h).
const N: u32 = 1 << 0;
const NE: u32 = 1 << 1;
const E: u32 = 1 << 2;
const SE: u32 = 1 << 3;
const S: u32 = 1 << 4;
const SW: u32 = 1 << 5;
const W: u32 = 1 << 6;
const NW: u32 = 1 << 7;

/// `sqrt` for f64 (core has none): Newton's method to full f64 precision, sqrt(0) = 0.
fn sqrt(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    // Start at or above the root; Newton then decreases monotonically to it.
    let mut r = if x >= 1.0 { x } else { 1.0 };
    loop {
        let next = (r + x / r) / 2.0;
        if next >= r {
            return r;
        }
        r = next;
    }
}

/// `atan` for f64 (core has none), full f64 precision: invert above 1, then halve the argument with
/// atan(t) = 2*atan(t/(1 + sqrt(1 + t^2))) down to |t| <= 0.1, then the Taylor series.
fn atan(t: f64) -> f64 {
    if t.is_nan() {
        return t;
    }
    let neg = t < 0.0;
    let a = t.abs();
    let r = if a > 1.0 {
        core::f64::consts::FRAC_PI_2 - atan_reduced(1.0 / a)
    } else {
        atan_reduced(a)
    };
    if neg {
        -r
    } else {
        r
    }
}

/// `atan` for |t| <= 1.
fn atan_reduced(mut t: f64) -> f64 {
    let mut k = 0u32;
    while t > 0.1 {
        t /= 1.0 + sqrt(1.0 + t * t);
        k += 1;
    }
    let t2 = t * t;
    let mut term = t;
    let mut sum = t;
    let mut n = 1u64;
    loop {
        term *= t2;
        let c = term / ((2 * n + 1) as f64);
        if n & 1 == 1 {
            sum -= c;
        } else {
            sum += c;
        }
        if c == 0.0 {
            break;
        }
        n += 1;
    }
    sum * (1u64 << k) as f64
}

/// `atan2` for f64 (core has none).
fn atan2(y: f64, x: f64) -> f64 {
    if x > 0.0 {
        atan(y / x)
    } else if x < 0.0 {
        if y >= 0.0 {
            atan(y / x) + core::f64::consts::PI
        } else {
            atan(y / x) - core::f64::consts::PI
        }
    } else if y > 0.0 {
        core::f64::consts::FRAC_PI_2
    } else if y < 0.0 {
        -core::f64::consts::FRAC_PI_2
    } else {
        f64::NAN
    }
}

/// `xy_get_direction` (libinput-private.h), as `device_float_get_direction` calls it on a delta.
fn xy_get_direction(x: f64, y: f64) -> u32 {
    let mut dir = UNDEFINED_DIRECTION;
    if x.abs() < 2.0 && y.abs() < 2.0 {
        if x > 0.0 && y > 0.0 {
            dir = S | SE | E;
        } else if x > 0.0 && y < 0.0 {
            dir = N | NE | E;
        } else if x < 0.0 && y > 0.0 {
            dir = S | SW | W;
        } else if x < 0.0 && y < 0.0 {
            dir = N | NW | W;
        } else if x > 0.0 {
            dir = NE | E | SE;
        } else if x < 0.0 {
            dir = NW | W | SW;
        } else if y > 0.0 {
            dir = SE | S | SW;
        } else if y < 0.0 {
            dir = NE | N | NW;
        }
    } else {
        let mut r = atan2(y, x);
        r = (r + 2.5 * core::f64::consts::PI) % (2.0 * core::f64::consts::PI); // fmod
        r *= 4.0 * core::f64::consts::FRAC_1_PI;
        let d1 = ((r + 0.9) as i32 % 8) as u32;
        let d2 = ((r + 0.1) as i32 % 8) as u32;
        dir = (1 << d1) | (1 << d2);
    }
    dir
}

/// `struct pointer_tracker`.
#[derive(Clone, Copy)]
struct Tracker {
    /// Delta to the most recent event.
    delta: (f64, f64),
    /// Time in us.
    time: u64,
    dir: u32,
}

/// `struct pointer_trackers` with the fixed array `trackers_init` allocates for 2 trackers, plus the
/// `pointer_delta_smoothener` `create_pointer_accelerator_filter_touchpad` sets: threshold 0, value 0, because this
/// pad is on I2C and not Bluetooth, so the smoothener never fires (`tdelta < 0` is never true).
struct Trackers {
    trackers: [Tracker; NTRACKERS],
    cur: usize,
    smoothener: Option<(u64, u64)>,
}

impl Trackers {
    /// `trackers_init(&trackers, 2)` (zalloc'd) plus the smoothener of `create_pointer_accelerator_filter_touchpad`.
    fn new() -> Self {
        Trackers {
            trackers: [Tracker {
                delta: (0.0, 0.0),
                time: 0,
                dir: 0,
            }; NTRACKERS],
            cur: 0,
            smoothener: Some((0, 0)),
        }
    }

    /// `trackers_by_offset`.
    fn by_offset(&self, offset: usize) -> &Tracker {
        &self.trackers[(self.cur + NTRACKERS - offset) % NTRACKERS]
    }

    /// `trackers_reset`.
    fn reset(&mut self, time: u64) {
        for offset in 1..NTRACKERS {
            let t = &mut self.trackers[(self.cur + NTRACKERS - offset) % NTRACKERS];
            t.time = 0;
            t.dir = 0;
            t.delta = (0.0, 0.0);
        }
        let t = &mut self.trackers[self.cur];
        t.time = time;
        t.dir = UNDEFINED_DIRECTION;
    }

    /// `trackers_feed`.
    fn feed(&mut self, dx: f64, dy: f64, time: u64) {
        for t in &mut self.trackers {
            t.delta.0 += dx;
            t.delta.1 += dy;
        }
        self.cur = (self.cur + 1) % NTRACKERS;
        let t = &mut self.trackers[self.cur];
        t.delta = (0.0, 0.0);
        t.time = time;
        t.dir = xy_get_direction(dx, dy);
    }

    /// `calculate_trackers_velocity`: hypot of the delta over the time delta (us), in units/us.
    fn velocity_of(&self, tracker: &Tracker, time: u64) -> f64 {
        let mut tdelta = time.wrapping_sub(tracker.time).wrapping_add(1);
        if let Some((threshold, value)) = self.smoothener
            && tdelta < threshold
        {
            tdelta = value;
        }
        sqrt(tracker.delta.0 * tracker.delta.0 + tracker.delta.1 * tracker.delta.1) / tdelta as f64
    }

    /// `trackers_velocity_after_timeout`: the first movement after a timeout is measured over MOTION_TIMEOUT itself.
    fn velocity_after_timeout(&self, tracker: &Tracker) -> f64 {
        self.velocity_of(tracker, tracker.time.wrapping_add(MOTION_TIMEOUT))
    }

    /// `trackers_velocity`.
    fn velocity(&self, time: u64) -> f64 {
        let mut result = 0.0;
        let mut initial_velocity = 0.0;
        let mut dir = self.by_offset(0).dir;

        for offset in 1..NTRACKERS {
            let tracker = self.by_offset(offset);

            // Bug: time running backwards
            if tracker.time > time {
                break;
            }

            // Stop if too far away in time
            if time - tracker.time > MOTION_TIMEOUT {
                if offset == 1 {
                    result = self.velocity_after_timeout(tracker);
                }
                break;
            }

            let velocity = self.velocity_of(tracker, time);

            // Stop if direction changed
            dir &= tracker.dir;
            if dir == 0 {
                // First movement after dirchange - velocity is that of the last movement
                if offset == 1 {
                    result = velocity;
                }
                break;
            }

            // Always average the first two events
            if initial_velocity == 0.0 || offset <= 2 {
                result = velocity;
                initial_velocity = velocity;
            } else {
                // Stop if velocity differs too much from initial
                let velocity_diff = (initial_velocity - velocity).abs();
                if velocity_diff > MAX_VELOCITY_DIFF {
                    break;
                }
                result = velocity;
            }
        }

        result
    }
}

/// libinput's touchpad accelerator (adaptive profile, speed 0), for a pad whose device units are first scaled to the
/// x axis' resolution by the caller (libinput's tp_scale_to_xaxis).
pub struct TouchpadAccel {
    /// `last_velocity`, units/us in device-native dpi.
    last_velocity: f64,
    trackers: Trackers,
    /// Threshold in mm/s.
    threshold: f64,
    dpi: i32,
    /// Factor based on the speed setting.
    speed_factor: f64,
}

impl TouchpadAccel {
    /// create_pointer_accelerator_filter_touchpad(dpi, 0, 0, false) with speed 0.
    pub fn new(dpi: i32) -> TouchpadAccel {
        TouchpadAccel {
            last_velocity: 0.0,
            trackers: Trackers::new(),
            threshold: THRESHOLD_MM_S,
            dpi,
            // speed_factor(0.0): pow(0.0 + 1.0, 2.38) = pow(1.0, 2.38) = 1.0 exactly, so 1.0 * 0.95 + 0.05 = 1.0.
            speed_factor: 1.0,
        }
    }

    /// accelerator_filter_touchpad: a delta in device units (x resolution), time in µs → normalized (1000 dpi) units.
    pub fn filter(&mut self, dx: f64, dy: f64, time_us: u64) -> (f64, f64) {
        // calculate_acceleration_factor
        self.trackers.feed(dx, dy, time_us);
        let velocity = self.trackers.velocity(time_us);
        // calculate_acceleration_simpsons
        let accel_factor = (self.profile(velocity)
            + self.profile(self.last_velocity)
            + 4.0 * self.profile((self.last_velocity + velocity) / 2.0))
            / 6.0;
        self.last_velocity = velocity;

        // Accelerate for device units, normalize afterwards (normalize_for_dpi)
        let ax = dx * accel_factor;
        let ay = dy * accel_factor;
        (
            ax * DEFAULT_MOUSE_DPI / self.dpi as f64,
            ay * DEFAULT_MOUSE_DPI / self.dpi as f64,
        )
    }

    /// touchpad_constant_filter (also libinput's filter_scroll for touchpads): no acceleration.
    pub fn constant(&self, dx: f64, dy: f64) -> (f64, f64) {
        let nx = dx * DEFAULT_MOUSE_DPI / self.dpi as f64;
        let ny = dy * DEFAULT_MOUSE_DPI / self.dpi as f64;
        (
            BASELINE * TP_MAGIC_SLOWDOWN * nx,
            BASELINE * TP_MAGIC_SLOWDOWN * ny,
        )
    }

    /// touchpad_accelerator_restart.
    pub fn restart(&mut self, time_us: u64) {
        self.trackers.reset(time_us);
    }

    /// `touchpad_accel_profile_linear` for this filter.
    fn profile(&self, speed_in: f64) -> f64 {
        profile_linear_with(self.dpi, self.threshold, self.speed_factor, speed_in)
    }
}

/// touchpad_accel_profile_linear's factor for a speed in device units per µs (exposed for tests). Speed setting 0,
/// so speed_factor = 1.0.
pub fn profile_linear(dpi: i32, speed_units_per_us: f64) -> f64 {
    profile_linear_with(dpi, THRESHOLD_MM_S, 1.0, speed_units_per_us)
}

/// `touchpad_accel_profile_linear` with the filter's dpi, threshold and speed_factor.
fn profile_linear_with(dpi: i32, threshold: f64, speed_factor: f64, speed_in: f64) -> f64 {
    let baseline = BASELINE;

    // Convert to mm/s because that's something one can understand: v_us2s(speed_in) * 25.4 / dpi
    let speed_in = speed_in * 1_000_000.0 * 25.4 / dpi as f64;

    let factor = if speed_in < 7.0 {
        // Decelerate down to 30% of input speed: y = 0.1x + 0.3
        baseline.min(0.1 * speed_in + 0.3)
    } else if speed_in < threshold {
        // Up to the threshold, we keep the baseline
        baseline
    } else {
        // A curve up to four times the threshold, then a plateau
        let upper_threshold = threshold * 4.0;
        let speed_in = speed_in.min(upper_threshold);
        0.0025 * (speed_in / threshold) * (speed_in - threshold) + baseline
    };

    factor * speed_factor * TP_MAGIC_SLOWDOWN
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mm/s to device units/us at the given dpi: units/s = mm/s * dpi / 25.4, units/us = that / 1e6.
    fn mm_s_to_units_per_us(mm_s: f64, dpi: i32) -> f64 {
        mm_s * dpi as f64 / 25.4 / 1_000_000.0
    }

    #[test]
    fn profile_linear_matches_c_formula() {
        // dpi 1000, so units/us = mm/s / 25400. Expected values written by hand from the C formula
        // (factor * speed_factor(0) = 1 * TP_MAGIC_SLOWDOWN 0.2968):
        let cases: [(f64, f64); 8] = [
            (0.0, 0.3 * 0.2968),    // 0 < 7: min(0.9, 0.1*0 + 0.3) = 0.3
            (3.0, 0.6 * 0.2968),    // 0.1*3 + 0.3 = 0.6
            (7.0, 0.9 * 0.2968),    // 7 <= s < 130: baseline 0.9
            (50.0, 0.9 * 0.2968),   // baseline
            (130.0, 0.9 * 0.2968),  // s == threshold: 0.0025*(130/130)*(130-130) + 0.9 = 0.9
            (260.0, 1.55 * 0.2968), // 0.0025*(260/130)*(260-130) + 0.9 = 0.65 + 0.9
            (520.0, 4.8 * 0.2968),  // 0.0025*(520/130)*(520-130) + 0.9 = 3.9 + 0.9
            (1000.0, 4.8 * 0.2968), // clamped to 4*130 = 520
        ];
        for (mm_s, expected) in cases {
            let got = profile_linear(1000, mm_s_to_units_per_us(mm_s, 1000));
            assert!(
                (got - expected).abs() <= 1e-15 * expected,
                "{mm_s} mm/s: got {got}, expected {expected}"
            );
        }
    }

    #[test]
    fn first_motion_after_new_and_restart() {
        // First motion after new(), more than MOTION_TIMEOUT after time 0 (trackers_init zallocs time 0):
        // trackers_velocity takes the timeout branch at offset 1 and uses trackers_velocity_after_timeout,
        // velocity = hypot(delta) / (MOTION_TIMEOUT + 1), with last_velocity still 0.
        let mut a = TouchpadAccel::new(1000);
        let (x, y) = a.filter(6.0, 8.0, MOTION_TIMEOUT + 1_000_000);
        let v = 10.0 / (MOTION_TIMEOUT as f64 + 1.0); // units/us
        let mm_s = v * 1_000_000.0 * 25.4 / 1000.0; // 0.2537 mm/s, below the 7 mm/s knee
        let p = |s: f64| (0.1 * s + 0.3) * 0.2968; // profile in the decel region
        let factor = (p(mm_s) + p(0.0) + 4.0 * p(mm_s / 2.0)) / 6.0; // Simpson over (v, 0)
        assert!((x - 6.0 * factor).abs() < 1e-12, "x = {x}, expected {}", 6.0 * factor);
        assert!((y - 8.0 * factor).abs() < 1e-12, "y = {y}");

        // After restart, the same first-motion rule, measured from the restart time.
        let mut b = TouchpadAccel::new(1000);
        b.restart(5_000_000);
        let (x2, _) = b.filter(6.0, 8.0, 5_000_000 + MOTION_TIMEOUT + 1_000_000);
        assert!((x2 - x).abs() < 1e-12, "x2 = {x2}, x = {x}");
    }

    #[test]
    fn steady_motion_reaches_plateau_and_fast_motion_more() {
        // 10 units per 10 ms = 25.4 mm/s, inside [7, 130) mm/s: the plateau factor 0.9 * 0.2968 per unit
        // (1000/1000 normalization). (1 unit per 10 ms is only 2.54 mm/s, below the 7 mm/s knee where the
        // profile decelerates, so it cannot reach the plateau.)
        let mut a = TouchpadAccel::new(1000);
        let mut last = (0.0, 0.0);
        for i in 1..=20u64 {
            last = a.filter(10.0, 0.0, i * 10_000); // 200 ms of motion
        }
        let plateau = 0.9 * 0.2968;
        assert!((last.0 - 10.0 * plateau).abs() < 1e-9, "{}", last.0);

        // 100 units per 10 ms = 254 mm/s, above the 130 mm/s threshold: more than the plateau.
        let mut b = TouchpadAccel::new(1000);
        let mut last = (0.0, 0.0);
        for i in 1..=20u64 {
            last = b.filter(100.0, 0.0, i * 10_000);
        }
        assert!(
            last.0 / 100.0 > plateau,
            "{} vs plateau {plateau}",
            last.0 / 100.0
        );
    }

    #[test]
    fn constant_filter_is_baseline_times_magic_slowdown() {
        let a = TouchpadAccel::new(406);
        let (x, y) = a.constant(10.0, -5.0);
        // touchpad_constant_filter: normalize_for_dpi, then baseline * TP_MAGIC_SLOWDOWN * normalized
        let ex = 0.9 * 0.2968 * (10.0 * 1000.0 / 406.0);
        let ey = 0.9 * 0.2968 * (-5.0 * 1000.0 / 406.0);
        assert!((x - ex).abs() < 1e-12 && (y - ey).abs() < 1e-12, "({x}, {y})");
    }

    #[test]
    fn sqrt_known_values() {
        assert_eq!(sqrt(0.0), 0.0);
        let cases = [
            (1.0, 1.0),
            (2.0, 1.4142135623730951),
            (1e-12, 1e-6),
            (1e12, 1e6),
        ];
        for (x, expected) in cases {
            let got = sqrt(x);
            assert!(
                (got - expected).abs() <= 1e-15 * expected,
                "sqrt({x}): got {got}, expected {expected}"
            );
        }
    }
}
