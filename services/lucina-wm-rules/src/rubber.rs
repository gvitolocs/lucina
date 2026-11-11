//! A rubber band: the resistance felt when a gesture drags a value past the end of its range,
//! following the behaviour of niri's `rubber_band.rs`. Used for workspace and overview swipes, where
//! dragging beyond the first or last workspace gives a little way and then stops.

/// A band is `stiffness` (how much of the drag is transferred near the end) and `limit` (the most it
/// will ever give, in the same units as the value). niri uses `stiffness: 0.5, limit: 0.05` for its
/// workspace and overview gestures, where the value is a whole workspace width.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub stiffness: f64,
    pub limit: f64,
}

impl Band {
    /// niri's rubber band for swiping workspaces and the overview.
    pub const GESTURE: Band = Band {
        stiffness: 0.5,
        limit: 0.05,
    };

    /// band(x) = c * d * x / (c * x + d): it starts with slope `stiffness` (so the first movement feels
    /// soft) and saturates at `limit`, never reaching it. Defined for x >= 0; a negative x mirrors it,
    /// the band pulling equally in both directions.
    pub fn band(&self, x: f64) -> f64 {
        if x < 0.0 {
            return -self.band(-x);
        }
        let c = self.stiffness;
        let d = self.limit;
        c * d * x / (c * x + d)
    }

    /// Keeps `x` inside `[min, max]`, and when it is outside, shows only `band` of the overshoot
    /// measured from the nearest end: `end + sign * band(|x - end|)`.
    pub fn clamp(&self, min: f64, max: f64, x: f64) -> f64 {
        if x < min {
            min - self.band(min - x)
        } else if x > max {
            max + self.band(x - max)
        } else {
            x
        }
    }

    /// d clamp / dx: 1.0 inside the range, and c * d^2 / (c * |x - end| + d)^2 outside, i.e. how much
    /// of the next unit of drag still shows up as movement of the value. It equals the slope of
    /// [`Band::band`] at that distance, so it is the number a gesture needs to stay under the finger.
    pub fn clamp_derivative(&self, min: f64, max: f64, x: f64) -> f64 {
        let distance = if x < min {
            min - x
        } else if x > max {
            x - max
        } else {
            return 1.0;
        };
        let c = self.stiffness;
        let d = self.limit;
        let denominator = c * distance + d;
        c * d * d / (denominator * denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libm::fabs;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        fabs(a - b) < tol
    }

    #[test]
    fn gesture_band_has_the_niri_numbers() {
        assert_eq!(Band::GESTURE.stiffness, 0.5);
        assert_eq!(Band::GESTURE.limit, 0.05);
        assert_eq!(
            Band::GESTURE,
            Band {
                stiffness: 0.5,
                limit: 0.05
            }
        );
    }

    #[test]
    fn band_is_zero_at_zero_and_gives_no_resistance_to_an_empty_drag() {
        assert_eq!(Band::GESTURE.band(0.0), 0.0);
    }

    #[test]
    fn band_increases_and_saturates_below_its_limit() {
        let band = Band::GESTURE;
        let mut previous = -1.0;
        let mut x = 0.0;
        while x <= 100.0 {
            let value = band.band(x);
            assert!(value > previous, "band is not increasing at x={x}");
            assert!(
                value < band.limit,
                "band passed its limit at x={x}: {value}"
            );
            previous = value;
            x += 0.01;
        }

        assert!(
            close(band.band(1.0e9), 0.05, 1e-6),
            "got {}",
            band.band(1.0e9)
        );
        assert!(band.band(1.0e9) < 0.05);

        // The slope right at the end of the range is the stiffness itself.
        assert!(close(band.band(0.000001) / 0.000001, band.stiffness, 1e-5));

        // Mirrored below zero.
        assert_eq!(band.band(-0.5), -band.band(0.5));
    }

    #[test]
    fn clamp_is_the_identity_inside_the_range() {
        let band = Band::GESTURE;
        for x in [0.0, 0.25, 0.5, 0.9999, 1.0] {
            assert_eq!(band.clamp(0.0, 1.0, x), x);
        }
        assert_eq!(band.clamp(-2.0, -1.0, -1.5), -1.5);
        assert_eq!(band.clamp(-1.0, 1.0, -1.0), -1.0);
        assert_eq!(band.clamp(-1.0, 1.0, 0.0), 0.0);
    }

    #[test]
    fn clamp_shows_only_the_band_outside_the_range() {
        let band = Band::GESTURE;
        let above = band.clamp(0.0, 1.0, 1.5);
        assert!(close(above, 1.0 + band.band(0.5), 1e-12), "got {above}");
        assert!(above > 1.0 && above < 1.0 + band.limit);

        let below = band.clamp(0.0, 1.0, -0.5);
        assert!(close(below, 0.0 - band.band(0.5), 1e-12), "got {below}");
        assert!(below < 0.0 && below > -band.limit);

        // Far outside it sticks to the limit on either side, symmetric about the range.
        assert!(close(band.clamp(0.0, 1.0, 1.0e6), 1.0 + band.limit, 1e-6));
        assert!(close(band.clamp(0.0, 1.0, -1.0e6), 0.0 - band.limit, 1e-6));
    }

    #[test]
    fn clamp_is_continuous_at_the_ends() {
        let band = Band::GESTURE;
        assert!(fabs(band.clamp(0.0, 1.0, 1.0 + 0.000000001) - 1.0) < 1e-6);
        assert!(fabs(band.clamp(0.0, 1.0, 0.0 - 0.000000001) - 0.0) < 1e-6);
        assert!(fabs(band.clamp(0.0, 1.0, 1.0 + 0.0001) - 1.0) < band.limit);
    }

    #[test]
    fn clamp_derivative_is_one_inside_and_the_slope_outside() {
        let band = Band::GESTURE;
        assert_eq!(band.clamp_derivative(0.0, 1.0, 0.0), 1.0);
        assert_eq!(band.clamp_derivative(0.0, 1.0, 0.5), 1.0);
        assert_eq!(band.clamp_derivative(0.0, 1.0, 1.0), 1.0);
        assert_eq!(band.clamp_derivative(-2.0, -1.0, -1.75), 1.0);

        // Just outside, the derivative equals the band stiffness; it falls off with distance.
        assert!(close(
            band.clamp_derivative(0.0, 1.0, 1.0000001),
            band.stiffness,
            1e-6
        ));
        let c = band.stiffness;
        let d = band.limit;
        let expected_at_half = c * d * d / (c * 0.5 + d) / (c * 0.5 + d);
        assert!(close(
            band.clamp_derivative(0.0, 1.0, 1.5),
            expected_at_half,
            1e-12
        ));
        assert!(band.clamp_derivative(0.0, 1.0, 5.0) < band.clamp_derivative(0.0, 1.0, 2.0));
        assert!(band.clamp_derivative(0.0, 1.0, -5.0) < band.clamp_derivative(0.0, 1.0, -2.0));
    }

    #[test]
    fn clamp_derivative_matches_a_central_difference() {
        let band = Band::GESTURE;
        let h = 0.000001;
        for x in [1.2, 1.5, 2.0, 10.0, 1.0e4, -0.2, -0.5, -3.0, -1.0e4] {
            let numeric = (band.clamp(0.0, 1.0, x + h) - band.clamp(0.0, 1.0, x - h)) / (2.0 * h);
            let analytic = band.clamp_derivative(0.0, 1.0, x);
            assert!(
                close(numeric, analytic, 1e-8),
                "x={x}: numeric {numeric} vs analytic {analytic}"
            );
        }

        // A band with a different stiffness and limit behaves the same way.
        let stiff = Band {
            stiffness: 2.0,
            limit: 0.25,
        };
        for x in [1.05, 0.95, 3.0, -2.0] {
            let numeric = (stiff.clamp(0.0, 1.0, x + h) - stiff.clamp(0.0, 1.0, x - h)) / (2.0 * h);
            let analytic = stiff.clamp_derivative(0.0, 1.0, x);
            assert!(
                close(numeric, analytic, 1e-6),
                "x={x}: numeric {numeric} vs analytic {analytic}"
            );
        }
    }

    #[test]
    fn clamp_never_gives_more_than_the_limit() {
        // The value can never leave [min - limit, max + limit], however far the drag goes.
        let band = Band::GESTURE;
        assert!(band.clamp(10.0, 20.0, 1.0e9) < 20.0 + band.limit);
        assert!(band.clamp(10.0, 20.0, -1.0e9) > 10.0 - band.limit);
        assert_eq!(band.clamp(10.0, 20.0, 15.0), 15.0);
    }
}
