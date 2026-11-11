//! Split view and edge snap: which tile zone a dragged window lands in, the rectangles of those zones, and the divider of a two-way split.
//! Edge zones follow GNOME Shell's `meta-window-drag.c` (left/right edge tile, top edge maximize, corner quarter tiles); the padding between
//! tiles follows KWin's quick-tile padding; the magnetic centre follows macOS/GNOME split dividers. Integer geometry only, no allocation.

use crate::tile_rules;

/// A rectangle in whole pixels: `x`,`y` is the top-left corner, `w`,`h` the size. Coordinates and sizes up to `tile_rules::LIMIT` (2^28, far beyond any real display) cannot overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// The x coordinate one past the last column (`x + w`).
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    /// The y coordinate one past the last row (`y + h`).
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// True when `other` lies completely inside this rectangle (edges may coincide).
    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// True when the two rectangles share at least one pixel; rectangles that only touch along an edge do not intersect, empty ones never do.
    pub fn intersects(&self, other: &Rect) -> bool {
        !self.is_empty()
            && !other.is_empty()
            && self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// True when the rectangle has no area (width or height is zero or negative).
    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

/// The tile zone a dragged window would snap to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// No snapping: the window stays free.
    None,
    /// The left half of the work area.
    Left,
    /// The right half of the work area.
    Right,
    /// The whole work area (maximize), reached by dragging to the top edge.
    Top,
    /// The top-left quarter.
    TopLeft,
    /// The top-right quarter.
    TopRight,
    /// The bottom-left quarter.
    BottomLeft,
    /// The bottom-right quarter.
    BottomRight,
}

/// Edge zone of a dragged window by pointer position (GNOME `meta-window-drag.c`): within `edge` px of the left or right work-area edge gives
/// `Left`/`Right`; at or above the top edge (up to `edge` px inside it) gives `Top` (maximize); within `edge` of two edges gives a quarter tile.
/// A pointer near only the bottom edge gives `None`. The caller skips this call while Shift is held. `edge` is normally 48.
pub fn edge_zone(work: Rect, px: i32, py: i32, edge: i32) -> Zone {
    let left = px < work.x + edge;
    let right = px >= work.right() - edge;
    let top = py < work.y + edge;
    let bottom = py >= work.bottom() - edge;
    match (top, bottom, left, right) {
        (true, _, true, _) => Zone::TopLeft,
        (true, _, _, true) => Zone::TopRight,
        (true, _, _, _) => Zone::Top,
        (_, true, true, _) => Zone::BottomLeft,
        (_, true, _, true) => Zone::BottomRight,
        (_, _, true, _) => Zone::Left,
        (_, _, _, true) => Zone::Right,
        _ => Zone::None,
    }
}

/// `v` kept in `-LIMIT..=LIMIT`, the range `tile_rules` covers. Inputs beyond it are clamped (2^28 is far beyond any real
/// display), so no arithmetic below can overflow.
fn bounded(v: i32) -> i64 {
    (v as i64).clamp(-tile_rules::LIMIT, tile_rules::LIMIT)
}

/// A padding in `0..=LIMIT`: a negative padding behaves as 0 at this boundary.
fn bounded_padding(p: i32) -> i64 {
    (p.max(0) as i64).min(tile_rules::LIMIT)
}

/// A `tile_rules` rectangle `(x, y, w, h)` as a `Rect`. The conversion with `as` cannot lose bits: the prover shows every value fits
/// `i32` for inputs bounded by `bounded` / `bounded_padding`.
fn rect((x, y, w, h): (i64, i64, i64, i64)) -> Rect {
    Rect {
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
    }
}

/// The rectangle of a tile zone: `None` gives the work area unchanged, `Top` the work area inset by `padding`, halves and quarters have
/// `padding` to the work-area edge and between each other (KWin quick-tile padding, 4). The left/top half is the floor half, the right/bottom
/// half takes the odd pixel. A negative `padding` behaves as 0 and work-area values beyond ±`tile_rules::LIMIT` are clamped (not for `None`).
pub fn zone_rect(work: Rect, zone: Zone, padding: i32) -> Rect {
    let (x, y, w, h) = (
        bounded(work.x),
        bounded(work.y),
        bounded(work.w),
        bounded(work.h),
    );
    let p = bounded_padding(padding);
    match zone {
        Zone::None => work,
        Zone::Top => rect(tile_rules::inset(x, y, w, h, p)),
        Zone::Left => rect(tile_rules::split_geom(x, y, w, h, p, 500).0),
        Zone::Right => rect(tile_rules::split_geom(x, y, w, h, p, 500).1),
        Zone::TopLeft => rect(tile_rules::quarter(x, y, w, h, p, false, false)),
        Zone::TopRight => rect(tile_rules::quarter(x, y, w, h, p, true, false)),
        Zone::BottomLeft => rect(tile_rules::quarter(x, y, w, h, p, false, true)),
        Zone::BottomRight => rect(tile_rules::quarter(x, y, w, h, p, true, true)),
    }
}

/// Two-way split with the divider at `ratio_permille` (500 = half): the left and the right rectangle. There is `padding` to the work-area
/// edges and exactly `padding` between the two; the usable width `w - 3 * padding` is shared by ratio, and each side keeps at least one pixel
/// whenever there are two to give. Both rectangles lie inside `work` and never intersect.
pub fn split_h(work: Rect, ratio_permille: i32, padding: i32) -> (Rect, Rect) {
    let (l, r) = tile_rules::split_geom(
        bounded(work.x),
        bounded(work.y),
        bounded(work.w),
        bounded(work.h),
        bounded_padding(padding),
        ratio_permille as i64,
    );
    (rect(l), rect(r))
}

/// Clamp a divider ratio (per-mille of the width `split_h` shares) so that neither side is narrower than `min_w` pixels. The ratio applies to
/// the room `work.w - 3 * padding` that `split_h` hands out, not to the full work width. When that room is too narrow for two such sides the
/// divider goes to the middle (500).
pub fn clamp_ratio(work: Rect, padding: i32, ratio_permille: i32, min_w: i32) -> i32 {
    // tile_rules covers widths, paddings and minima up to LIMIT with padding >= 0, so bound the inputs first.
    let padding = bounded_padding(padding);
    let avail = (bounded(work.w) - 3 * padding).clamp(0, tile_rules::LIMIT);
    tile_rules::clamp_ratio(avail, bounded(min_w), ratio_permille as i64) as i32
}

/// Snap a dragged divider to 500 when it is within `snap` per-mille of it (magnetic centre, macOS/GNOME-like); otherwise unchanged.
pub fn snap_ratio(ratio_permille: i32, snap: i32) -> i32 {
    if (ratio_permille - 500).abs() <= snap {
        500
    } else {
        ratio_permille
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        x: 0,
        y: 32,
        w: 1920,
        h: 1048,
    };

    #[test]
    fn rect_basics() {
        let a = Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        let b = Rect {
            x: 10,
            y: 0,
            w: 10,
            h: 10,
        };
        let c = Rect {
            x: 9,
            y: 9,
            w: 10,
            h: 10,
        };
        assert_eq!((a.right(), a.bottom()), (10, 10));
        assert!(!a.intersects(&b));
        assert!(a.intersects(&c));
        assert!(a.contains(&Rect {
            x: 2,
            y: 2,
            w: 8,
            h: 8
        }));
        assert!(!a.contains(&c));
        assert!(
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 5
            }
            .is_empty()
        );
        assert!(!a.is_empty());
        assert!(!a.intersects(&Rect {
            x: 1,
            y: 1,
            w: 0,
            h: 0
        }));
    }

    #[test]
    fn edge_zone_corners_edges_middle() {
        let e = 48;
        assert_eq!(edge_zone(WORK, 960, 500, e), Zone::None);
        assert_eq!(edge_zone(WORK, 10, 500, e), Zone::Left);
        assert_eq!(edge_zone(WORK, 1915, 500, e), Zone::Right);
        assert_eq!(edge_zone(WORK, 960, 32, e), Zone::Top);
        assert_eq!(edge_zone(WORK, 960, 0, e), Zone::Top);
        assert_eq!(edge_zone(WORK, 3, 35, e), Zone::TopLeft);
        assert_eq!(edge_zone(WORK, 1919, 40, e), Zone::TopRight);
        assert_eq!(edge_zone(WORK, 3, 1079, e), Zone::BottomLeft);
        assert_eq!(edge_zone(WORK, 1919, 1079, e), Zone::BottomRight);
        assert_eq!(edge_zone(WORK, 960, 1079, e), Zone::None);
        assert_eq!(edge_zone(WORK, 47, 500, e), Zone::Left);
        assert_eq!(edge_zone(WORK, 48, 500, e), Zone::None);
        assert_eq!(edge_zone(WORK, 1872, 500, e), Zone::Right);
        assert_eq!(edge_zone(WORK, 1871, 500, e), Zone::None);
    }

    #[test]
    fn zone_rects() {
        assert_eq!(zone_rect(WORK, Zone::None, 4), WORK);
        assert_eq!(
            zone_rect(WORK, Zone::Top, 4),
            Rect {
                x: 4,
                y: 36,
                w: 1912,
                h: 1040
            }
        );
        let l = zone_rect(WORK, Zone::Left, 4);
        let r = zone_rect(WORK, Zone::Right, 4);
        assert!(WORK.contains(&l) && WORK.contains(&r));
        assert!(!l.intersects(&r));
        assert_eq!(r.x - l.right(), 4);
        assert_eq!(l.x, 4);
        assert_eq!(WORK.right() - r.right(), 4);
        assert_eq!(l.h, 1040);
    }

    #[test]
    fn quarters_tile_without_overlap() {
        for w in [100, 101, 640, 1921] {
            for h in [90, 91, 480, 1049] {
                let work = Rect { x: 7, y: 3, w, h };
                let z = [
                    Zone::TopLeft,
                    Zone::TopRight,
                    Zone::BottomLeft,
                    Zone::BottomRight,
                ];
                for i in 0..4 {
                    let a = zone_rect(work, z[i], 4);
                    assert!(work.contains(&a) && !a.is_empty());
                    for other in z.iter().skip(i + 1) {
                        assert!(!a.intersects(&zone_rect(work, *other, 4)));
                    }
                }
            }
        }
    }

    #[test]
    fn split_half() {
        for w in [800, 801, 1920, 1921] {
            let work = Rect {
                x: 0,
                y: 0,
                w,
                h: 600,
            };
            let (l, r) = split_h(work, 500, 4);
            assert!((l.w - r.w).abs() <= 1);
            assert_eq!(r.x - l.right(), 4);
            assert_eq!(l.x, 4);
            assert_eq!(work.right() - r.right(), 4);
            assert_eq!(l.y, 4);
            assert_eq!(l.h, 592);
        }
    }

    #[test]
    fn split_exhaustive() {
        for w in [20, 200, 1920] {
            let work = Rect {
                x: 11,
                y: 5,
                w,
                h: 300,
            };
            let mut ratio = 0;
            while ratio <= 1000 {
                let (l, r) = split_h(work, ratio, 4);
                assert!(
                    work.contains(&l) && work.contains(&r),
                    "ratio {ratio} w {w}"
                );
                assert!(!l.is_empty() && !r.is_empty());
                assert!(!l.intersects(&r));
                assert_eq!(r.x - l.right(), 4);
                assert_eq!(l.w + r.w, w - 12);
                ratio += 7;
            }
        }
    }

    #[test]
    fn split_degenerate_does_not_panic() {
        let (l, r) = split_h(
            Rect {
                x: 0,
                y: 0,
                w: 5,
                h: 5,
            },
            300,
            4,
        );
        assert!(!l.intersects(&r));
        let (l, r) = split_h(
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            2000,
            4,
        );
        assert!(l.is_empty() && r.is_empty());
    }

    #[test]
    fn clamp_keeps_min_width() {
        for w in [400, 999, 1920] {
            let work = Rect {
                x: 0,
                y: 0,
                w,
                h: 100,
            };
            for min_w in [100, 320] {
                if 2 * min_w > w {
                    continue;
                }
                for ratio in 0..=1000 {
                    let c = clamp_ratio(work, 0, ratio, min_w);
                    let left = (w as i64 * c as i64 / 1000) as i32;
                    assert!(
                        left >= min_w && w - left >= min_w,
                        "w {w} min {min_w} ratio {ratio} -> {c}"
                    );
                }
            }
        }
        assert_eq!(
            clamp_ratio(
                Rect {
                    x: 0,
                    y: 0,
                    w: 500,
                    h: 10
                },
                0,
                100,
                300
            ),
            500
        );
        assert_eq!(clamp_ratio(WORK, 0, 500, 100), 500);
    }

    #[test]
    fn clamp_keeps_min_width_of_the_split_sides() {
        for w in [600, 1000, 1920] {
            let work = Rect {
                x: 0,
                y: 0,
                w,
                h: 100,
            };
            for padding in [0, 4, 12] {
                let min_w = 200;
                assert!(2 * min_w <= w - 3 * padding);
                for ratio in 0..=1000 {
                    let r = clamp_ratio(work, padding, ratio, min_w);
                    let (l, rr) = split_h(work, r, padding);
                    assert!(
                        l.w >= min_w && rr.w >= min_w,
                        "w {w} padding {padding} ratio {ratio} -> {r}: {} and {}",
                        l.w,
                        rr.w
                    );
                }
            }
        }
    }

    #[test]
    fn split_geom_matches_the_documented_layout() {
        let (l, r) = tile_rules::split_geom(11, 5, 200, 300, 4, 500);
        assert_eq!(l, (15, 9, 94, 292));
        assert_eq!(r, (113, 9, 94, 292));
    }

    #[test]
    fn split_geom_clamps_height_and_has_no_negative_size() {
        let (l, r) = tile_rules::split_geom(0, 0, 5, 5, 4, 500);
        assert_eq!((l.3, r.3), (0, 0));
        assert_eq!((l.2, r.2), (0, 0));
    }

    #[test]
    fn inset_values() {
        assert_eq!(tile_rules::inset(0, 32, 1920, 1048, 4), (4, 36, 1912, 1040));
        assert_eq!(tile_rules::inset(3, 3, 5, 5, 4), (7, 7, 0, 0));
        assert_eq!(tile_rules::inset(1, 2, 10, 20, 0), (1, 2, 10, 20));
    }

    #[test]
    fn second_4k_screen_to_the_right_is_not_cut() {
        let work = Rect {
            x: 11520,
            y: 0,
            w: 3840,
            h: 2160,
        };
        let left = zone_rect(work, Zone::Left, 4);
        assert_eq!(left.x, 11524);
    }

    #[test]
    fn negative_padding_behaves_as_zero() {
        for p in [-1, -4, i32::MIN] {
            for zone in [
                Zone::Top,
                Zone::Left,
                Zone::Right,
                Zone::TopLeft,
                Zone::TopRight,
                Zone::BottomLeft,
                Zone::BottomRight,
            ] {
                assert_eq!(zone_rect(WORK, zone, p), zone_rect(WORK, zone, 0));
            }
            assert_eq!(split_h(WORK, 400, p), split_h(WORK, 400, 0));
            assert_eq!(
                clamp_ratio(WORK, p, 100, 300),
                clamp_ratio(WORK, 0, 100, 300)
            );
        }
    }

    #[test]
    fn huge_inputs_do_not_panic_and_fit_i32() {
        let extremes = [i32::MIN, -1, 0, 1, i32::MAX];
        for x in extremes {
            for w in extremes {
                let work = Rect { x, y: x, w, h: w };
                for p in extremes {
                    for zone in [
                        Zone::Top,
                        Zone::Left,
                        Zone::Right,
                        Zone::TopLeft,
                        Zone::TopRight,
                        Zone::BottomLeft,
                        Zone::BottomRight,
                    ] {
                        let r = zone_rect(work, zone, p);
                        assert!(i64::from(r.x) + i64::from(r.w) <= i64::from(i32::MAX));
                        assert!(r.w >= 0 && r.h >= 0);
                    }
                    for ratio in extremes {
                        let (l, r) = split_h(work, ratio, p);
                        assert!(l.w >= 0 && r.w >= 0 && l.h >= 0);
                        let _ = clamp_ratio(work, p, ratio, w);
                    }
                }
            }
        }
        // beyond the limit behaves as the limit
        let big = Rect {
            x: 0,
            y: 0,
            w: i32::MAX,
            h: i32::MAX,
        };
        let lim = tile_rules::LIMIT as i32;
        let at = Rect {
            x: 0,
            y: 0,
            w: lim,
            h: lim,
        };
        assert_eq!(split_h(big, 500, 4), split_h(at, 500, 4));
    }

    #[test]
    fn the_four_quarters_never_intersect_and_lie_inside_the_work_area() {
        let zones = [
            Zone::TopLeft,
            Zone::TopRight,
            Zone::BottomLeft,
            Zone::BottomRight,
        ];
        for padding in 0..=12 {
            for (w, h) in [(100, 90), (101, 91), (640, 480), (1921, 1049)] {
                let work = Rect { x: 7, y: 3, w, h };
                for i in 0..4 {
                    let a = zone_rect(work, zones[i], padding);
                    assert!(work.contains(&a), "padding {padding} {w}x{h}");
                    for z in zones.iter().skip(i + 1) {
                        assert!(!a.intersects(&zone_rect(work, *z, padding)));
                    }
                }
            }
        }
    }

    #[test]
    fn snap() {
        assert_eq!(snap_ratio(490, 20), 500);
        assert_eq!(snap_ratio(450, 20), 450);
        assert_eq!(snap_ratio(520, 20), 500);
        assert_eq!(snap_ratio(521, 20), 521);
    }
}
