//! Anti-aliased corner coverage for rounded rectangles: for a corner of radius `r` (1..=MAX_RADIUS), the coverage
//! 0..=16 of pixel (x, y) counted from the corner's outer edge, by 4x4 supersampling against the quarter circle.
//! Built at compile time into one table (the sum of r^2 bytes, 11.4 KB), so drawing a corner is a lookup: the DSi's
//! ARM9 has no FPU and pays for every multiply.

pub const MAX_RADIUS: u16 = 32;
pub const FULL: u8 = 16;

/// Where radius `r`'s r x r block starts in the table: the sum of k^2 for k < r.
const fn offset(r: usize) -> usize {
    let r = r - 1;
    r * (r + 1) * (2 * r + 1) / 6
}

const LEN: usize = offset(MAX_RADIUS as usize + 1);

/// The coverage of pixel (x, y) of a radius-`r` corner: 16 sub-pixels at (8x + 2i + 1, 8y + 2j + 1) in 1/8 px,
/// inside the circle of radius 8r centred at (8r, 8r).
const fn sample(r: i32, x: i32, y: i32) -> u8 {
    let rr = 8 * r;
    let mut n = 0;
    let mut j = 0;
    while j < 4 {
        let mut i = 0;
        while i < 4 {
            let (px, py) = (8 * x + 2 * i + 1, 8 * y + 2 * j + 1);
            if (rr - px) * (rr - px) + (rr - py) * (rr - py) <= rr * rr {
                n += 1;
            }
            i += 1;
        }
        j += 1;
    }
    n
}

const fn build() -> [u8; LEN] {
    let mut t = [0u8; LEN];
    let mut r = 1;
    while r <= MAX_RADIUS as usize {
        let base = offset(r);
        let mut y = 0;
        while y < r {
            let mut x = 0;
            while x < r {
                t[base + y * r + x] = sample(r as i32, x as i32, y as i32);
                x += 1;
            }
            y += 1;
        }
        r += 1;
    }
    t
}

static TABLE: [u8; LEN] = build();

/// Coverage 0..=16 of pixel (x, y) of a corner of radius `r` (clamped to 1..=MAX_RADIUS); FULL outside the corner.
#[inline]
pub fn at(r: u16, x: u16, y: u16) -> u8 {
    let r = r.clamp(1, MAX_RADIUS) as usize;
    let (x, y) = (x as usize, y as usize);
    if x >= r || y >= r {
        return FULL;
    }
    TABLE[offset(r) + y * r + x]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_sampling() {
        for r in 1..=MAX_RADIUS {
            for y in 0..r {
                for x in 0..r {
                    assert_eq!(at(r, x, y), sample(r as i32, x as i32, y as i32), "r{r} ({x},{y})");
                }
            }
            assert_eq!(at(r, r, 0), FULL);
            // coverage never falls going inwards along the diagonal
            for k in 1..r {
                assert!(at(r, k, k) >= at(r, k - 1, k - 1), "r{r} at {k}");
            }
        }
        assert_eq!(offset(1), 0);
        assert_eq!(offset(2), 1);
        assert_eq!(offset(3), 5);
        assert_eq!(LEN, 11440);
    }
}
