//! A damped spring of unit mass that carries a value from one number to another, following the
//! behaviour of niri's `animation/spring.rs`: the closed-form solution of
//! `y'' + 2 * zeta * omega0 * y' + omega0^2 * y = 0` (with `y = value - target`), used for workspace
//! switches, view movement and the overview.

use libm::{cos, exp, fabs, sin, sqrt};

/// How the spring pulls: `stiffness` is the spring constant (omega0 = sqrt(stiffness)), `damping_ratio`
/// says how fast the motion dies out (1.0 is critical damping, below 1.0 the value rings, above 1.0 it
/// crawls) and `epsilon` is how close to the target counts as arrived. Numbers are niri's `SpringParams`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub damping_ratio: f64,
    pub stiffness: f64,
    pub epsilon: f64,
}

impl Params {
    /// niri's workspace-switch spring: critically damped and stiff, so a swipe release glides to the
    /// next workspace without ringing.
    pub const WORKSPACE_SWITCH: Params = Params {
        damping_ratio: 1.0,
        stiffness: 1000.0,
        epsilon: 0.0001,
    };
    /// niri's view spring, which also drives opening and closing the overview.
    pub const VIEW: Params = Params {
        damping_ratio: 1.0,
        stiffness: 800.0,
        epsilon: 0.0001,
    };
}

/// Which closed form applies, decided once from the damping ratio.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Regime {
    /// damping_ratio == 1: y = (y0 + (v0 + beta * y0) * t) * e^(-beta * t).
    Critical,
    /// damping_ratio < 1: a decaying oscillation at `wd`.
    Under { wd: f64 },
    /// damping_ratio > 1: a sum of two decaying exponentials with rates `r1 > r2`.
    Over { r1: f64, r2: f64, a: f64, b: f64 },
}

/// One spring animation: the value `from` moving to `to`, starting with `velocity` units per second.
/// Everything is precomputed in [`Spring::new`] so that `value_at` and `velocity_at` only do arithmetic
/// and exponentials, with no allocation - the niri `Spring` type.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    from: f64,
    to: f64,
    velocity: f64,
    params: Params,
    /// Exponential decay rate, damping_ratio * omega0.
    beta: f64,
    /// Starting displacement, `from - to`.
    y0: f64,
    regime: Regime,
}

/// Spacing of the samples used by [`Spring::duration_s`], in seconds (niri scans the curve at 1 ms).
const SAMPLE_STEP_S: f64 = 0.001;
/// Last sample scanned by [`Spring::duration_s`]: a spring that has not settled by now is reported as
/// lasting 3 s, which keeps the duration finite for degenerate parameters.
const SAMPLE_CAP_MS: u32 = 3000;
/// The cap in seconds.
const SAMPLE_CAP_S: f64 = 3.0;

impl Spring {
    /// Starts a spring at `from`, heading for `to`, already moving at `velocity` (units per second,
    /// the same units as `from` and `to`) - niri keeps the gesture velocity so a release continues
    /// smoothly instead of starting from rest.
    pub fn new(from: f64, to: f64, velocity: f64, params: Params) -> Spring {
        let omega0 = sqrt(params.stiffness);
        let beta = params.damping_ratio * omega0;
        let y0 = from - to;

        let regime = if fabs(params.damping_ratio - 1.0) < 1e-9 {
            Regime::Critical
        } else if params.damping_ratio < 1.0 {
            let wd = omega0 * sqrt(1.0 - params.damping_ratio * params.damping_ratio);
            Regime::Under { wd }
        } else {
            let s = sqrt(beta * beta - omega0 * omega0);
            let r1 = -beta + s;
            let r2 = -beta - s;
            let a = (velocity - y0 * r2) / (r1 - r2);
            let b = y0 - a;
            Regime::Over { r1, r2, a, b }
        };

        Spring {
            from,
            to,
            velocity,
            params,
            beta,
            y0,
            regime,
        }
    }

    /// The value at `t_s` seconds after the start. Times at or before the start give `from` exactly,
    /// so a restarted frame never shows a jump.
    pub fn value_at(&self, t_s: f64) -> f64 {
        if t_s <= 0.0 {
            return self.from;
        }
        self.to + self.displacement(t_s)
    }

    /// d/dt of [`Spring::value_at`], the units-per-second speed at `t_s`. Times at or before the start
    /// give the initial velocity exactly.
    pub fn velocity_at(&self, t_s: f64) -> f64 {
        if t_s <= 0.0 {
            return self.velocity;
        }
        self.displacement_derivative(t_s)
    }

    /// Seconds after the start at which the spring counts as arrived: the first 1 ms sample from which
    /// |value - to| < epsilon and |velocity| < epsilon * 10 hold for every later sample up to the 3 s
    /// cap. The velocity half of the test is what niri adds so that a spring which passes the target
    /// slowly is not cut off mid-move; a spring that is already sitting on the target at rest lasts 0 s.
    pub fn duration_s(&self) -> f64 {
        if self.from == self.to && self.velocity == 0.0 {
            return 0.0;
        }

        let epsilon = self.params.epsilon;
        let velocity_epsilon = epsilon * 10.0;

        // Scanning from the cap backwards stops at the last sample that is still moving or still away
        // from the target; the spring settles at the next one.
        for i in (0..=SAMPLE_CAP_MS).rev() {
            let t = f64::from(i) * SAMPLE_STEP_S;
            if fabs(self.displacement(t)) >= epsilon
                || fabs(self.displacement_derivative(t)) >= velocity_epsilon
            {
                let settled = f64::from(i + 1) * SAMPLE_STEP_S;
                return if settled > SAMPLE_CAP_S {
                    SAMPLE_CAP_S
                } else {
                    settled
                };
            }
        }

        0.0
    }

    /// Whether the spring has arrived at `to` by `t_s` seconds, i.e. `t_s >= duration_s()`.
    pub fn is_done(&self, t_s: f64) -> bool {
        t_s >= self.duration_s()
    }

    /// Displacement from the target, y(t) = value(t) - to, for t > 0.
    fn displacement(&self, t: f64) -> f64 {
        match self.regime {
            Regime::Critical => {
                let k = self.velocity + self.beta * self.y0;
                (self.y0 + k * t) * exp(-self.beta * t)
            }
            Regime::Under { wd } => {
                let c = (self.velocity + self.beta * self.y0) / wd;
                exp(-self.beta * t) * (self.y0 * cos(wd * t) + c * sin(wd * t))
            }
            Regime::Over { r1, r2, a, b } => a * exp(r1 * t) + b * exp(r2 * t),
        }
    }

    /// Analytic derivative of [`Spring::displacement`], y'(t), for t > 0.
    fn displacement_derivative(&self, t: f64) -> f64 {
        match self.regime {
            Regime::Critical => {
                let k = self.velocity + self.beta * self.y0;
                exp(-self.beta * t) * (k - self.beta * (self.y0 + k * t))
            }
            Regime::Under { wd } => {
                let c = (self.velocity + self.beta * self.y0) / wd;
                let cos_c = -self.beta * self.y0 + c * wd;
                let sin_c = -self.beta * c - self.y0 * wd;
                exp(-self.beta * t) * (cos_c * cos(wd * t) + sin_c * sin(wd * t))
            }
            Regime::Over { r1, r2, a, b } => a * r1 * exp(r1 * t) + b * r2 * exp(r2 * t),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A critically damped spring (niri workspace switch), from 0 to 1 at rest.
    fn critical() -> Spring {
        Spring::new(0.0, 1.0, 0.0, Params::WORKSPACE_SWITCH)
    }

    /// An underdamped spring that rings around its target.
    fn underdamped() -> Spring {
        Spring::new(
            0.0,
            1.0,
            0.0,
            Params {
                damping_ratio: 0.5,
                stiffness: 1000.0,
                epsilon: 0.0001,
            },
        )
    }

    /// An overdamped spring that creeps to its target.
    fn overdamped() -> Spring {
        Spring::new(
            0.0,
            1.0,
            0.0,
            Params {
                damping_ratio: 2.0,
                stiffness: 1000.0,
                epsilon: 0.0001,
            },
        )
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        fabs(a - b) < tol
    }

    #[test]
    fn starts_exactly_at_from_in_every_regime() {
        for spring in [critical(), underdamped(), overdamped()] {
            assert_eq!(spring.value_at(0.0), spring.from);
            assert_eq!(spring.velocity_at(0.0), spring.velocity);
        }
        assert_eq!(critical().value_at(0.0), 0.0);
        assert_eq!(critical().velocity_at(0.0), 0.0);

        let moving = Spring::new(2.0, -3.0, 7.5, Params::VIEW);
        assert_eq!(moving.value_at(0.0), 2.0);
        assert_eq!(moving.velocity_at(0.0), 7.5);
    }

    #[test]
    fn negative_time_gives_from() {
        for spring in [critical(), underdamped(), overdamped()] {
            assert_eq!(spring.value_at(-1.0), spring.from);
            assert_eq!(spring.value_at(-0.0001), spring.from);
            assert_eq!(spring.velocity_at(-1.0), spring.velocity);
        }
    }

    #[test]
    fn reaches_the_target_at_its_own_duration() {
        for spring in [critical(), underdamped(), overdamped()] {
            let d = spring.duration_s();
            assert!(d > 0.0 && d <= SAMPLE_CAP_S, "duration out of range: {d}");
            assert!(
                fabs(spring.value_at(d) - spring.to) < spring.params.epsilon,
                "value_at(duration) = {} is not within epsilon of {}",
                spring.value_at(d),
                spring.to
            );
            assert!(fabs(spring.velocity_at(d)) < spring.params.epsilon * 10.0);
            assert!(spring.is_done(d));
            assert!(!spring.is_done(d - SAMPLE_STEP_S));
        }
    }

    #[test]
    fn critical_matches_the_closed_form() {
        // beta = omega0 = sqrt(1000) = 31.6227766, y0 = -1, v0 = 0, so
        // value(0.1) = 1 - (1 + beta * 0.1) * e^(-beta * 0.1).
        let beta = 31.6227766;
        let expected = 1.0 - (1.0 + beta * 0.1) * exp(-beta * 0.1);
        assert!(
            close(critical().value_at(0.1), expected, 1e-9),
            "got {}",
            critical().value_at(0.1)
        );
    }

    #[test]
    fn critical_never_overshoots() {
        let spring = critical();
        let d = spring.duration_s();
        let mut t = 0.0;
        while t <= d + 0.5 {
            assert!(
                spring.value_at(t) <= 1.0,
                "critical overshot at t={t}: {}",
                spring.value_at(t)
            );
            t += SAMPLE_STEP_S;
        }
        assert!(spring.value_at(d) < 1.0);
    }

    #[test]
    fn critical_with_a_fast_start_overshoots_then_settles() {
        // A critical spring passes its target only when the starting speed beats beta * |y0|, so 20 units/s
        // overshoots a stiffness of 100 (beta = 10) but not the workspace-switch preset (beta = 31.6).
        // This is niri's swipe release: fling it hard and the page runs past the workspace, then comes back.
        let springs = [
            Spring::new(
                0.0,
                1.0,
                20.0,
                Params {
                    damping_ratio: 1.0,
                    stiffness: 100.0,
                    epsilon: 0.0001,
                },
            ),
            Spring::new(0.0, 1.0, 60.0, Params::WORKSPACE_SWITCH),
        ];

        for spring in &springs {
            let mut peak: f64 = 1.0;
            let mut t = 0.0;
            while t <= 3.0 {
                peak = peak.max(spring.value_at(t));
                t += SAMPLE_STEP_S;
            }
            assert!(
                peak > 1.0,
                "a fast critical start did not overshoot (peak {peak})"
            );

            let d = spring.duration_s();
            assert!(d < SAMPLE_CAP_S, "the overshooting spring never settled");
            assert!(fabs(spring.value_at(d) - 1.0) < spring.params.epsilon);
            // Once settled it stays put: the tail of the curve is back on the target.
            assert!(fabs(spring.value_at(d + 0.1) - 1.0) < spring.params.epsilon);
        }

        // The same 20 units/s on the stiff preset is not enough to break past the target.
        let restrained = Spring::new(0.0, 1.0, 20.0, Params::WORKSPACE_SWITCH);
        let mut t = 0.0;
        while t <= 3.0 {
            assert!(
                restrained.value_at(t) <= 1.0,
                "20 units/s overshot the stiff preset at t={t}"
            );
            t += SAMPLE_STEP_S;
        }
    }

    #[test]
    fn underdamped_overshoots_and_lasts_longer_than_critical() {
        let under = underdamped();
        let crit = critical();

        let mut peak: f64 = 1.0;
        let mut t = 0.0;
        while t <= under.duration_s() {
            peak = peak.max(under.value_at(t));
            t += SAMPLE_STEP_S;
        }
        assert!(peak > 1.0, "underdamped did not overshoot (peak {peak})");

        // It also comes back down below the target at least once, i.e. it rings.
        let mut below = false;
        let mut t = 0.0;
        while t <= under.duration_s() {
            below |= under.value_at(t) < 0.9;
            t += SAMPLE_STEP_S;
        }
        assert!(below, "underdamped did not ring back below the target");

        assert!(
            under.duration_s() > crit.duration_s(),
            "underdamped {:?} s should outlast critical {:?} s",
            under.duration_s(),
            crit.duration_s()
        );
    }

    #[test]
    fn overdamped_never_overshoots() {
        let spring = overdamped();
        let d = spring.duration_s();
        let mut t = 0.0;
        let mut previous = spring.value_at(0.0);
        while t <= d + 0.5 {
            let value = spring.value_at(t);
            assert!(value <= 1.0, "overdamped overshot at t={t}: {value}");
            assert!(
                value >= previous - 1e-12,
                "overdamped went backwards at t={t}"
            );
            previous = value;
            t += SAMPLE_STEP_S;
        }
    }

    #[test]
    fn settled_spring_lasts_no_time() {
        let resting = Spring::new(1.0, 1.0, 0.0, Params::WORKSPACE_SWITCH);
        assert_eq!(resting.duration_s(), 0.0);
        assert!(resting.is_done(0.0));
        assert_eq!(resting.value_at(0.0), 1.0);
        assert_eq!(resting.value_at(1.0), 1.0);
    }

    #[test]
    fn duration_respects_the_cap_when_it_cannot_settle() {
        // epsilon = 0 can never be satisfied, so the scan reports the cap.
        let stuck = Spring::new(
            0.0,
            1.0,
            0.0,
            Params {
                damping_ratio: 1.0,
                stiffness: 1000.0,
                epsilon: 0.0,
            },
        );
        assert_eq!(stuck.duration_s(), SAMPLE_CAP_S);
        assert!(!stuck.is_done(SAMPLE_CAP_S - 0.5));
        assert!(stuck.is_done(SAMPLE_CAP_S));
    }

    #[test]
    fn the_named_presets_settle_in_the_time_niri_ones_do() {
        // A whole page at rest: 0.41 s for the workspace switch, 0.45 s for the view.
        let workspace = Spring::new(0.0, 1.0, 0.0, Params::WORKSPACE_SWITCH);
        let view = Spring::new(0.0, 1.0, 0.0, Params::VIEW);
        assert!(
            close(workspace.duration_s(), 0.409, 0.002),
            "got {:?}",
            workspace.duration_s()
        );
        assert!(
            close(view.duration_s(), 0.453, 0.002),
            "got {:?}",
            view.duration_s()
        );
        assert!(workspace.duration_s() < view.duration_s());
    }

    #[test]
    fn velocity_is_the_derivative_of_value() {
        let springs = [
            critical(),
            underdamped(),
            overdamped(),
            Spring::new(0.0, 1.0, 20.0, Params::WORKSPACE_SWITCH),
            Spring::new(0.5, -0.25, -8.0, Params::VIEW),
            Spring::new(
                0.0,
                1.0,
                0.0,
                Params {
                    damping_ratio: 0.9,
                    stiffness: 800.0,
                    epsilon: 0.0001,
                },
            ),
            Spring::new(
                0.0,
                1.0,
                0.0,
                Params {
                    damping_ratio: 1.0000000005,
                    stiffness: 1000.0,
                    epsilon: 0.0001,
                },
            ),
        ];

        let h = 0.000001;
        for spring in &springs {
            for i in 1..60 {
                let t = f64::from(i) * 0.01;
                let numeric = (spring.value_at(t + h) - spring.value_at(t - h)) / (2.0 * h);
                let analytic = spring.velocity_at(t);
                assert!(
                    close(numeric, analytic, 1e-4),
                    "t={t}: numeric {numeric} vs analytic {analytic}"
                );
            }
        }
    }

    #[test]
    fn value_tracks_the_displacement_back_to_the_target() {
        let spring = Spring::new(10.0, 0.0, 0.0, Params::VIEW);
        assert_eq!(spring.value_at(0.0), 10.0);
        assert!(spring.value_at(0.01) < 10.0);
        assert!(fabs(spring.value_at(spring.duration_s())) < spring.params.epsilon);
        assert_eq!(spring.to, 0.0);
        assert_eq!(spring.from, 10.0);
    }
}
