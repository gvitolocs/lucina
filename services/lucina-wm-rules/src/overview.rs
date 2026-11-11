//! Mission Control layout: where each window preview goes in the overview, the zoomed row of spaces, and the interpolation and zoom curves
//! that animate between the normal view and the overview. The preview layout follows GNOME Shell's `workspace.js` window layout (try every row
//! count, score scale against covered space with weights 1 and 0.1, keep the best); the zoom follows niri's `compute_overview_zoom`.
//! Integer per-mille arithmetic only, no allocation.

pub use crate::tile::Rect;

/// Most windows the overview lays out; further entries of `sizes` are ignored.
pub const MAX_WINDOWS: usize = 16;

/// Largest share of the area the previews may use, in per-mille (a scale of at most 95/100).
const FILL_PERMILLE: i64 = 950;
/// Largest enlargement of a small window, in per-mille (1.5x).
const MAX_SCALE_PERMILLE: i64 = 1500;
/// GNOME `LAYOUT_SCALE_WEIGHT` (1.0) and `LAYOUT_SPACE_WEIGHT` (0.1), both multiplied by 10 to stay integer.
const SCALE_WEIGHT: i64 = 10;
const SPACE_WEIGHT: i64 = 1;

/// One candidate row count with its common scale and score.
struct Plan {
    rows: usize,
    scale: i64,
    score: i64,
}

/// Distribute windows `0..n` in order into `rows` non-empty rows, each row's natural width as near the ideal width (total / rows) as possible.
/// `row_of[i]` receives the row of window `i`.
fn assign_rows(sizes: &[(i32, i32)], n: usize, rows: usize, row_of: &mut [usize; MAX_WINDOWS]) {
    let total: i64 = sizes[..n].iter().map(|s| s.0.max(1) as i64).sum();
    let ideal = total / rows as i64;
    let mut i = 0;
    for row in 0..rows {
        let rows_left_after = rows - row - 1;
        let limit = n - rows_left_after;
        row_of[i] = row;
        let mut width = sizes[i].0.max(1) as i64;
        i += 1;
        if row + 1 == rows {
            while i < n {
                row_of[i] = row;
                i += 1;
            }
            break;
        }
        while i < limit {
            let next = sizes[i].0.max(1) as i64;
            if (width + next - ideal).abs() < (width - ideal).abs() {
                width += next;
                row_of[i] = row;
                i += 1;
            } else {
                break;
            }
        }
    }
}

/// Common scale (per-mille) for a row assignment and the resulting score.
fn plan_for(
    area: Rect,
    sizes: &[(i32, i32)],
    n: usize,
    rows: usize,
    gap: i64,
    avail_w: i64,
    avail_h: i64,
) -> Plan {
    let mut row_of = [0usize; MAX_WINDOWS];
    assign_rows(sizes, n, rows, &mut row_of);
    let mut sum_w = [0i64; MAX_WINDOWS];
    let mut count = [0i64; MAX_WINDOWS];
    let mut max_h = [0i64; MAX_WINDOWS];
    for i in 0..n {
        let r = row_of[i];
        sum_w[r] += sizes[i].0.max(1) as i64;
        count[r] += 1;
        max_h[r] = max_h[r].max(sizes[i].1.max(1) as i64);
    }
    let mut scale = MAX_SCALE_PERMILLE;
    for r in 0..rows {
        scale = scale.min((avail_w - gap * (count[r] - 1)) * 1000 / sum_w[r]);
    }
    let sum_h: i64 = max_h[..rows].iter().sum();
    scale = scale
        .min((avail_h - gap * (rows as i64 - 1)) * 1000 / sum_h)
        .max(1);
    let mut covered = 0i64;
    for s in &sizes[..n] {
        covered += (s.0.max(1) as i64 * scale / 1000) * (s.1.max(1) as i64 * scale / 1000);
    }
    let area_px = (area.w.max(1) as i64) * (area.h.max(1) as i64);
    let space = covered * 1000 / area_px;
    Plan {
        rows,
        scale,
        score: SCALE_WEIGHT * scale + SPACE_WEIGHT * space,
    }
}

/// Slots for the windows of the given `sizes` inside `area`, filling rows (GNOME `workspace.js`): every row count `1..=n` is tried, the windows
/// are distributed in order into rows of near-equal width, one common scale (at most 95% of the area, small windows enlarged at most 1.5x) is
/// found, and the row count with the best score `1.0 * scale + 0.1 * covered area` wins (ties go to fewer rows). `out[i]` is the rectangle of
/// window `i`; each row is centred horizontally, the block of rows vertically, windows are centred in their row, and `gap` pixels (24) separate
/// neighbours (reduced when the area is too small for it). Only `min(sizes.len(), MAX_WINDOWS)` entries of `out` are written.
pub fn layout(area: Rect, sizes: &[(i32, i32)], gap: i32, out: &mut [Rect; MAX_WINDOWS]) {
    let n = sizes.len().min(MAX_WINDOWS);
    if n == 0 {
        return;
    }
    let avail_w = area.w.max(0) as i64 * FILL_PERMILLE / 1000;
    let avail_h = area.h.max(0) as i64 * FILL_PERMILLE / 1000;
    let gap = (gap.max(0) as i64).min(avail_w.min(avail_h) / (2 * n as i64));

    let mut best = plan_for(area, sizes, n, 1, gap, avail_w, avail_h);
    for rows in 2..=n {
        let p = plan_for(area, sizes, n, rows, gap, avail_w, avail_h);
        if p.score > best.score {
            best = p;
        }
    }

    let mut row_of = [0usize; MAX_WINDOWS];
    assign_rows(sizes, n, best.rows, &mut row_of);
    let mut scaled = [(0i64, 0i64); MAX_WINDOWS];
    let mut row_w = [0i64; MAX_WINDOWS];
    let mut row_h = [0i64; MAX_WINDOWS];
    let mut count = [0i64; MAX_WINDOWS];
    for i in 0..n {
        let w = (sizes[i].0.max(1) as i64 * best.scale / 1000).max(1);
        let h = (sizes[i].1.max(1) as i64 * best.scale / 1000).max(1);
        scaled[i] = (w, h);
        let r = row_of[i];
        row_w[r] += w;
        row_h[r] = row_h[r].max(h);
        count[r] += 1;
    }
    let mut block_h = gap * (best.rows as i64 - 1);
    for r in 0..best.rows {
        row_w[r] += gap * (count[r] - 1);
        block_h += row_h[r];
    }
    let mut row_y = area.y as i64 + (area.h as i64 - block_h) / 2;
    let mut i = 0;
    for r in 0..best.rows {
        let mut x = area.x as i64 + (area.w as i64 - row_w[r]) / 2;
        while i < n && row_of[i] == r {
            let (w, h) = scaled[i];
            out[i] = Rect {
                x: x as i32,
                y: (row_y + (row_h[r] - h) / 2) as i32,
                w: w as i32,
                h: h as i32,
            };
            x += w + gap;
            i += 1;
        }
        row_y += row_h[r] + gap;
    }
}

/// On-screen rectangle of the screen-sized space `index` in the zoomed spaces row (niri overview): the screen is drawn at `zoom_permille`
/// (e.g. 600), the `active` space is centred on the screen, neighbours sit `gap` apart where gap = 100 per-mille of the zoomed screen height.
/// `x = centre_x + (index - active) * (w * zoom / 1000 + gap) - w * zoom / 2000`, the row is vertically centred.
pub fn space_rect(screen: Rect, zoom_permille: i32, index: usize, active: usize) -> Rect {
    let zoom = zoom_permille.clamp(1, 1000) as i64;
    let w = screen.w as i64 * zoom / 1000;
    let h = screen.h as i64 * zoom / 1000;
    let gap = screen.h as i64 * zoom / 10_000;
    let cx = screen.x as i64 + screen.w as i64 / 2;
    let cy = screen.y as i64 + screen.h as i64 / 2;
    let x = cx + (index as i64 - active as i64) * (w + gap) - screen.w as i64 * zoom / 2000;
    let y = cy - screen.h as i64 * zoom / 2000;
    Rect {
        x: x.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
    }
}

/// Linear interpolation of rectangles by progress `t_permille` (0 = `from`, 1000 = `to`, clamped), to fly a window from its place to its slot.
/// Exact at both ends and monotonic in every field.
pub fn lerp_rect(from: Rect, to: Rect, t_permille: i32) -> Rect {
    let t = t_permille.clamp(0, 1000) as i64;
    let f = |a: i32, b: i32| (a as i64 + (b as i64 - a as i64) * t / 1000) as i32;
    Rect {
        x: f(from.x, to.x),
        y: f(from.y, to.y),
        w: f(from.w, to.w),
        h: f(from.h, to.h),
    }
}

/// The overview zoom (per-mille) at gesture progress `p_permille` (0..=1000): `1000 - p * (1000 - z_target) / 1000` (niri
/// `compute_overview_zoom`), with `z_target_permille` clamped to `1..=750`.
pub fn zoom_at(p_permille: i32, z_target_permille: i32) -> i32 {
    let p = p_permille.clamp(0, 1000) as i64;
    let target = z_target_permille.clamp(1, 750) as i64;
    (1000 - p * (1000 - target) / 1000) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 32,
        w: 1920,
        h: 1048,
    };
    const ZERO: Rect = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };

    fn sizes_for(n: usize, seed: u32) -> [(i32, i32); MAX_WINDOWS] {
        let mut s = [(0, 0); MAX_WINDOWS];
        let mut v = seed.wrapping_mul(2654435761).wrapping_add(12345);
        for item in s.iter_mut().take(n) {
            v = v.wrapping_mul(1664525).wrapping_add(1013904223);
            let w = 200 + ((v >> 8) % 1700) as i32;
            v = v.wrapping_mul(1664525).wrapping_add(1013904223);
            let h = 150 + ((v >> 8) % 1000) as i32;
            *item = (w, h);
        }
        s
    }

    fn check(area: Rect, sizes: &[(i32, i32)]) {
        let mut out = [ZERO; MAX_WINDOWS];
        layout(area, sizes, 24, &mut out);
        let n = sizes.len();
        for i in 0..n {
            assert!(
                area.contains(&out[i]),
                "n {n} i {i} {:?} not in {:?}",
                out[i],
                area
            );
            assert!(!out[i].is_empty());
            for j in i + 1..n {
                assert!(
                    !out[i].intersects(&out[j]),
                    "n {n}: {:?} and {:?} overlap",
                    out[i],
                    out[j]
                );
            }
            // aspect kept within rounding: w'/h' == w/h  <=>  w' * h == h' * w, tolerance one pixel of either side
            let (w, h) = (sizes[i].0 as i64, sizes[i].1 as i64);
            let (ow, oh) = (out[i].w as i64, out[i].h as i64);
            assert!(
                (ow * h - oh * w).abs() <= w.max(h),
                "aspect n {n} i {i}: {:?} vs {:?}",
                out[i],
                sizes[i]
            );
            if i > 0 {
                let (a, b) = (out[i - 1], out[i]);
                let same_row = b.y < a.bottom() && a.y < b.bottom();
                assert!(b.y >= a.y || same_row, "reading order {a:?} {b:?}");
                if same_row {
                    assert!(b.x >= a.right(), "reading order {a:?} {b:?}");
                } else {
                    assert!(b.y >= a.bottom(), "reading order {a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn zero_windows_writes_nothing() {
        let mut out = [Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }; MAX_WINDOWS];
        layout(AREA, &[], 24, &mut out);
        assert!(out.iter().all(|r| *r
            == Rect {
                x: 1,
                y: 2,
                w: 3,
                h: 4
            }));
    }

    #[test]
    fn single_window_is_centred_and_at_most_95_percent() {
        for sz in [
            (800, 600),
            (3000, 2000),
            (100, 100),
            (1920, 1048),
            (400, 2000),
        ] {
            let mut out = [ZERO; MAX_WINDOWS];
            layout(AREA, &[sz], 24, &mut out);
            let r = out[0];
            assert!(
                r.w as i64 * 100 <= AREA.w as i64 * 95 && r.h as i64 * 100 <= AREA.h as i64 * 95,
                "{sz:?} -> {r:?}"
            );
            let (lx, rx) = (r.x - AREA.x, AREA.right() - r.right());
            let (ty, by) = (r.y - AREA.y, AREA.bottom() - r.bottom());
            assert!(
                (lx - rx).abs() <= 1 && (ty - by).abs() <= 1,
                "{sz:?} -> {r:?}"
            );
        }
        let mut out = [ZERO; MAX_WINDOWS];
        layout(AREA, &[(100, 100)], 24, &mut out);
        assert_eq!((out[0].w, out[0].h), (150, 150));
    }

    #[test]
    fn many_sizes_and_counts() {
        for n in 1..=MAX_WINDOWS {
            for seed in 0..60 {
                let s = sizes_for(n, seed * 31 + n as u32);
                check(AREA, &s[..n]);
                check(
                    Rect {
                        x: 40,
                        y: 10,
                        w: 800,
                        h: 480,
                    },
                    &s[..n],
                );
                check(
                    Rect {
                        x: 0,
                        y: 0,
                        w: 390,
                        h: 760,
                    },
                    &s[..n],
                );
            }
        }
    }

    #[test]
    fn extra_windows_ignored() {
        let s = sizes_for(MAX_WINDOWS, 7);
        let mut long = [(300, 200); 20];
        long[..MAX_WINDOWS].copy_from_slice(&s);
        let mut a = [ZERO; MAX_WINDOWS];
        let mut b = [ZERO; MAX_WINDOWS];
        layout(AREA, &long, 24, &mut a);
        layout(AREA, &s, 24, &mut b);
        assert_eq!(a, b);
    }

    #[test]
    fn equal_windows_make_a_grid() {
        let mut out = [ZERO; MAX_WINDOWS];
        layout(AREA, &[(640, 480); 4], 24, &mut out);
        assert_eq!(out[0].y, out[1].y);
        assert_eq!(out[2].y, out[3].y);
        assert!(out[2].y > out[0].y);
        assert_eq!(out[0].x, out[2].x);
    }

    #[test]
    fn space_rect_centres_active() {
        let screen = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let a = space_rect(screen, 600, 2, 2);
        assert_eq!((a.w, a.h), (1152, 648));
        assert!(((a.x + a.w / 2) - 960).abs() <= 1);
        assert!(((a.y + a.h / 2) - 540).abs() <= 1);
        let next = space_rect(screen, 600, 3, 2);
        let prev = space_rect(screen, 600, 1, 2);
        let gap = 1080 * 600 / 10_000;
        assert_eq!(next.x - a.right(), gap);
        assert_eq!(a.x - prev.right(), gap);
        assert_eq!(next.y, a.y);
        let odd = Rect {
            x: 13,
            y: 7,
            w: 1001,
            h: 777,
        };
        let c = space_rect(odd, 333, 0, 0);
        assert!(((c.x + c.w / 2) - (13 + 500)).abs() <= 1);
    }

    #[test]
    fn lerp_ends_and_monotonic() {
        let a = Rect {
            x: 100,
            y: 200,
            w: 800,
            h: 600,
        };
        let b = Rect {
            x: 10,
            y: 500,
            w: 150,
            h: 700,
        };
        assert_eq!(lerp_rect(a, b, 0), a);
        assert_eq!(lerp_rect(a, b, 1000), b);
        assert_eq!(lerp_rect(a, b, -5), a);
        assert_eq!(lerp_rect(a, b, 5000), b);
        let mut prev = a;
        for t in 1..=1000 {
            let r = lerp_rect(a, b, t);
            assert!(r.x <= prev.x && r.y >= prev.y && r.w <= prev.w && r.h >= prev.h);
            prev = r;
        }
        assert_eq!(
            lerp_rect(a, b, 500),
            Rect {
                x: 55,
                y: 350,
                w: 475,
                h: 650
            }
        );
    }

    #[test]
    fn zoom_curve() {
        assert_eq!(zoom_at(0, 600), 1000);
        assert_eq!(zoom_at(1000, 600), 600);
        assert_eq!(zoom_at(500, 600), 800);
        assert_eq!(zoom_at(1000, 900), 750);
        assert_eq!(zoom_at(1000, 0), 1);
        assert_eq!(zoom_at(-10, 600), 1000);
        assert_eq!(zoom_at(2000, 600), 600);
        let mut prev = 1000;
        for p in 0..=1000 {
            let z = zoom_at(p, 600);
            assert!(z <= prev);
            prev = z;
        }
    }
}
