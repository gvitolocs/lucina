// The integer geometry of tiling: the widths of a two-way split for a divider ratio, where a divider ratio may go so that neither
// side gets narrower than a minimum, and the rectangles of a split, of a maximized tile and of the four quarter tiles. Verus checks
// this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011); verification/juno-wm-rules/mutants.sh plants
// bugs that Verus must refuse. tile.rs calls these functions for every split, quarter, maximized tile and divider clamp and holds no
// copy of the rules; it only converts `Rect` to and from the `i64` tuples `(x, y, w, h)` used here.
//
// All arithmetic is `i64`. The proof covers coordinates and sizes in `-LIMIT..=LIMIT` and paddings in `0..=LIMIT` (the `requires`),
// and holds for any `LIMIT` up to 2^28: nothing can overflow, and every coordinate and size returned fits `i32` (`fits_i32`), so the
// `as i32` in `tile::rect` is exact. tile.rs clamps every input into that range first (`tile::bounded`, `tile::bounded_padding`: a
// negative padding becomes 0, a size beyond `LIMIT` is cut to `LIMIT`); those lines and the choice of the zone are not under Verus.
//
// Proved, for a work area `(x, y, w, h)` and `padding`, where `room(w, padding) = max(w - 3 * padding, 0)` is the width the two sides
// share:
// - the left width is exactly `left_width(room, ratio)` (the room times the ratio kept in 0..=1000, rounded down, then kept in
//   `1..=room-1` when the room has two pixels to give) and the right one the rest of the room; monotone in the ratio
//   (`left_width_monotone`);
// - the left side starts `padding` after `x`, the right one exactly `padding` after the left one ends; both have the same `y`
//   (`y + padding`) and height (`max(h - 2 * padding, 0)`);
// - with `w >= 2 * padding && h >= 2 * padding` both lie inside the work area; with `w >= 3 * padding` they cover it up to the
//   paddings (`l.x == x + padding`, `r.x + r.w + padding == x + w`);
// - smaller work areas: with `w < 3 * padding` both widths are 0 and with `h <= 2 * padding` both heights are 0 (empty rectangles);
//   with `w < 2 * padding` the right one starts past the work area (`r.x > x + w`), with `w < padding` the left one too, and with
//   `h < padding` both start below it. Empty rectangles cover no pixel, so nothing is drawn outside the work area;
// - the two halves never share a pixel (`split_geom`), and no two different quarters do (`quarters_disjoint`, a lemma about
//   `quarter_spec`, which `quarter` is proved to return); a quarter lies inside the work area under the same condition as the halves;
// - `clamp_ratio` takes the room `w` the split shares (tile.rs computes it as `room(bounded(w), padding)`; that line is unproved
//   glue): when the room holds two sides of `min_w > 0`, both sides of the split at the returned ratio are at least `min_w` wide,
//   and the ratio is the nearest point of `lo..=1000-lo`, `lo = min_ratio(room, min_w)` the least ratio with
//   `ratio * room >= min_w * 1000`. That interval is symmetric and counts in per-mille, not pixels, so it may refuse a few ratios
//   whose sides would still be wide enough (room 988, `min_w` 200: 798 would still give 200 on the right and is cut to 797; with
//   `min_w` 1 every ratio gives both sides a pixel, yet ratios below `lo` are moved up). A room too narrow for two such sides puts
//   the divider in the middle (500).

/// The widest work area, padding or minimum width the proof covers.
pub const LIMIT: i64 = 268_435_456;

// `v` kept inside `lo..=hi` (for `lo <= hi`).
//@ pub open spec fn clamp(v: int, lo: int, hi: int) -> int {
//@     if v < lo { lo } else if v > hi { hi } else { v }
//@ }

// The width the two sides of a split share: the work width less both edge paddings and the one between the sides, never negative.
//@ pub open spec fn room(w: int, padding: int) -> int {
//@     if w - 3 * padding > 0 { w - 3 * padding } else { 0 }
//@ }

// The left side's share of `a` pixels at `ratio` per mille: rounded down, and at least one pixel on each side when there are two.
//@ pub open spec fn left_width(a: int, ratio: int) -> int {
//@     if a >= 2 { clamp(a * clamp(ratio, 0, 1000) / 1000, 1, a - 1) } else { a * clamp(ratio, 0, 1000) / 1000 }
//@ }

// The least divider ratio with `ratio * a >= min_w * 1000`: `ceil(min_w * 1000 / a)`.
//@ pub open spec fn min_ratio(a: int, min_w: int) -> int {
//@     (min_w * 1000 + a - 1) / a
//@ }

// One axis of a split at `ratio`: `((left start, left size), (right start, right size))`.
//@ pub open spec fn span(x: int, w: int, padding: int, ratio: int) -> ((int, int), (int, int)) {
//@     ((x + padding, left_width(room(w, padding), ratio)),
//@         (x + 2 * padding + left_width(room(w, padding), ratio), room(w, padding) - left_width(room(w, padding), ratio)))
//@ }

// `inner` lies inside `outer`, with no negative size (`(x, y, w, h)`; edges may coincide).
//@ pub open spec fn contains(outer: (int, int, int, int), inner: (int, int, int, int)) -> bool {
//@     inner.2 >= 0 && inner.3 >= 0 && outer.0 <= inner.0 && outer.1 <= inner.1
//@         && inner.0 + inner.2 <= outer.0 + outer.2 && inner.1 + inner.3 <= outer.1 + outer.3
//@ }

// The two rectangles share a pixel (as `tile::Rect::intersects`: empty ones never do).
//@ pub open spec fn intersects(a: (int, int, int, int), b: (int, int, int, int)) -> bool {
//@     a.2 > 0 && a.3 > 0 && b.2 > 0 && b.3 > 0 && a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
//@ }

// Every coordinate and size of the rectangle is an `i32` and no size is negative: `tile::rect` converts it exactly.
//@ pub open spec fn fits_i32(r: (int, int, int, int)) -> bool {
//@     -2147483648 <= r.0 <= 2147483647 && -2147483648 <= r.1 <= 2147483647 && 0 <= r.2 <= 2147483647 && 0 <= r.3 <= 2147483647
//@ }

// The rectangle as the `int` tuple the statements are written against.
//@ pub open spec fn r4(r: (i64, i64, i64, i64)) -> (int, int, int, int) {
//@     (r.0 as int, r.1 as int, r.2 as int, r.3 as int)
//@ }

// The two parts of one axis as `int`s.
//@ pub open spec fn p2(r: ((i64, i64), (i64, i64))) -> ((int, int), (int, int)) {
//@     ((r.0.0 as int, r.0.1 as int), (r.1.0 as int, r.1.1 as int))
//@ }

// The quarter tile: the left or right part of the horizontal half split times the top or bottom part of the vertical one.
//@ pub open spec fn quarter_spec(x: int, y: int, w: int, h: int, padding: int, right: bool, bottom: bool) -> (int, int, int, int) {
//@     let sx = span(x, w, padding, 500);
//@     let sy = span(y, h, padding, 500);
//@     let hs = if right { sx.1 } else { sx.0 };
//@     let vs = if bottom { sy.1 } else { sy.0 };
//@     (hs.0, vs.0, hs.1, vs.1)
//@ }

// A larger ratio never gives the left side less.
//@ pub proof fn left_width_monotone(a: int, r1: int, r2: int)
//@     requires 0 <= a, r1 <= r2,
//@     ensures left_width(a, r1) <= left_width(a, r2),
//@ {
//@     let c1 = clamp(r1, 0, 1000);
//@     let c2 = clamp(r2, 0, 1000);
//@     assert(a * c1 <= a * c2) by(nonlinear_arith) requires 0 <= a, c1 <= c2;
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered(a * c1, a * c2, 1000);
//@ }

// No two different quarters share a pixel: on the axis where they differ, one ends at least `padding >= 0` before the other starts.
//@ pub proof fn quarters_disjoint(x: int, y: int, w: int, h: int, padding: int, r1: bool, b1: bool, r2: bool, b2: bool)
//@     requires padding >= 0, r1 != r2 || b1 != b2,
//@     ensures !intersects(quarter_spec(x, y, w, h, padding, r1, b1), quarter_spec(x, y, w, h, padding, r2, b2)),
//@ {
//@ }

/// The widths of the left and the right side of a split at `ratio_permille` (500 = half), for a work area `w` wide with `padding`
/// to its edges and exactly `padding` between the two sides. The room `w - 3 * padding` (or 0) is shared by ratio; each side keeps
/// at least one pixel whenever there are two to give.
#[allow(clippy::manual_clamp)] // Verus has no `clamp`
pub fn split_widths(w: i64, padding: i64, ratio_permille: i64) -> (i64, i64) //@ret
//@ requires -LIMIT <= w <= LIMIT, 0 <= padding <= LIMIT, -2147483648 <= ratio_permille <= 2147483647,
//@ ensures
//@     ret.0 >= 0, ret.1 >= 0,
//@     ret.0 + ret.1 == (if w - 3 * padding > 0 { w - 3 * padding } else { 0 }),
//@     ret.0 + ret.1 >= 2 ==> ret.0 >= 1 && ret.1 >= 1,
//@     w >= padding ==> ret.0 + padding + ret.1 <= w,
//@     w >= 2 * padding ==> padding + ret.0 + padding + ret.1 <= w,
//@     w >= 3 * padding ==> ret.0 + ret.1 == w - 3 * padding,
//@     room(w as int, padding as int) >= 2 ==> ret.0 == clamp(room(w as int, padding as int) * clamp(ratio_permille as int, 0, 1000) / 1000,
//@         1, room(w as int, padding as int) - 1),
//@     room(w as int, padding as int) < 2 ==> ret.0 == room(w as int, padding as int) * clamp(ratio_permille as int, 0, 1000) / 1000,
//@     ret.0 == left_width(room(w as int, padding as int), ratio_permille as int),
//@     ret.1 == room(w as int, padding as int) - ret.0,
{
    let inner = if w - 2 * padding > 0 {
        w - 2 * padding
    } else {
        0
    };
    let avail = if inner - padding > 0 {
        inner - padding
    } else {
        0
    };
    let ratio = if ratio_permille < 0 {
        0
    } else if ratio_permille > 1000 {
        1000
    } else {
        ratio_permille
    };
    // No overflow for any ratio an `i64` here can hold; the bound by 1000 is left to the `ensures`, so a wrong clamp fails them.
    //@ assert(avail <= 268435456);
    //@ assert(0 <= avail * ratio <= 576460752303423488) by(nonlinear_arith) requires 0 <= avail <= 268435456, 0 <= ratio <= 2147483647;
    let mut lw = avail * ratio / 1000;
    //@ assert(ratio <= 1000 ==> lw <= avail) by(nonlinear_arith) requires lw == avail * ratio / 1000, 0 <= avail, 0 <= ratio;
    if avail >= 2 {
        if lw < 1 {
            lw = 1;
        } else if lw > avail - 1 {
            lw = avail - 1;
        }
    }
    (lw, avail - lw)
}

/// A divider ratio (per-mille of the work width `w`) kept so that neither side is narrower than `min_w` pixels. A work area too
/// narrow for two such sides puts the divider in the middle (500); with no width or no minimum the ratio is only kept in 0..=1000.
#[allow(clippy::manual_clamp)] // Verus has no `clamp`
pub fn clamp_ratio(w: i64, min_w: i64, ratio_permille: i64) -> i64 //@ret
//@ requires -LIMIT <= w <= LIMIT, -LIMIT <= min_w <= LIMIT, -2147483648 <= ratio_permille <= 2147483647,
//@ ensures
//@     0 <= ret <= 1000,
//@     w <= 0 || min_w <= 0 ==> ret == (if ratio_permille < 0 { 0 } else if ratio_permille > 1000 { 1000 } else { ratio_permille }),
//@     w > 0 && min_w > 0 && 2 * min_w > w ==> ret == 500,
//@     w > 0 && min_w > 0 && 2 * min_w <= w ==> ret * w >= min_w * 1000 && (1000 - ret) * w >= min_w * 1000,
//@     w > 0 && min_w > 0 && 2 * min_w <= w && 0 <= ratio_permille <= 1000
//@         && ratio_permille * w >= min_w * 1000 && (1000 - ratio_permille) * w >= min_w * 1000 ==> ret == ratio_permille,
//@     w > 0 && min_w > 0 && 2 * min_w <= w ==>
//@         ret == clamp(clamp(ratio_permille as int, 0, 1000), min_ratio(w as int, min_w as int), 1000 - min_ratio(w as int, min_w as int))
//@         && left_width(w as int, ret as int) >= min_w && w - left_width(w as int, ret as int) >= min_w
//@         && min_ratio(w as int, min_w as int) * w >= min_w * 1000 && (min_ratio(w as int, min_w as int) - 1) * w < min_w * 1000
//@         && 1 <= min_ratio(w as int, min_w as int) <= 500,
{
    let ratio = if ratio_permille < 0 {
        0
    } else if ratio_permille > 1000 {
        1000
    } else {
        ratio_permille
    };
    if w <= 0 || min_w <= 0 {
        return ratio;
    }
    if 2 * min_w > w {
        return 500;
    }
    let lo = (min_w * 1000 + w - 1) / w;
    // The proof speaks of the spec values (`min_ratio`, `left_width`) and of `kept` only through implications, so a wrong `lo`,
    // `hi` or `kept` can only fail the `ensures`.
    //@ let ghost wi: int = w as int;
    //@ let ghost mi: int = min_w as int;
    //@ let ghost li: int = min_ratio(wi, mi);
    //@ let ghost num: int = mi * 1000 + wi - 1;
    //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(num, wi); }
    //@ assert(li * wi >= mi * 1000 && (li - 1) * wi < mi * 1000) by(nonlinear_arith)
    //@     requires wi > 0, mi > 0, li == num / wi, num == wi * (num / wi) + num % wi, 0 <= num % wi < wi, num == mi * 1000 + wi - 1;
    //@ assert(li <= 500) by(nonlinear_arith) requires wi > 0, 2 * mi <= wi, (li - 1) * wi < mi * 1000, mi > 0;
    //@ assert(li >= 1) by(nonlinear_arith) requires wi > 0, mi > 0, li * wi >= mi * 1000, li <= 500;
    let hi = 1000 - lo;
    let kept = if ratio < lo {
        lo
    } else if ratio > hi {
        hi
    } else {
        ratio
    };
    //@ let ghost ki: int = kept as int;
    //@ let ghost ri: int = ratio_permille as int;
    //@ let ghost raw: int = wi * ki / 1000;
    //@ assert(li <= ki <= 1000 - li ==> ki * wi >= mi * 1000 && (1000 - ki) * wi >= mi * 1000) by(nonlinear_arith)
    //@     requires li * wi >= mi * 1000, wi > 0;
    //@ assert(0 <= ki <= 1000 && ki * wi >= mi * 1000 && (1000 - ki) * wi >= mi * 1000 ==> raw >= mi && wi - raw >= mi) by(nonlinear_arith)
    //@     requires raw == wi * ki / 1000, wi > 0, mi > 0;
    //@ assert(0 <= ki <= 1000 && 1 <= raw <= wi - 1 ==> left_width(wi, ki) == raw);
    //@ assert(ri * wi >= mi * 1000 && (1000 - ri) * wi >= mi * 1000 ==> li <= ri <= 1000 - li) by(nonlinear_arith)
    //@     requires wi > 0, (li - 1) * wi < mi * 1000;
    kept
}

/// The left and the right side of a split along one axis starting at `x`: `((left_x, left_w), (right_x, right_w))`. There is
/// `padding` to both edges and exactly `padding` between the sides; the widths come from `split_widths`.
pub fn split_span(x: i64, w: i64, padding: i64, ratio_permille: i64) -> ((i64, i64), (i64, i64)) //@ret
//@ requires -LIMIT <= x <= LIMIT, -LIMIT <= w <= LIMIT, 0 <= padding <= LIMIT, -2147483648 <= ratio_permille <= 2147483647,
//@ ensures
//@     p2(ret) == span(x as int, w as int, padding as int, ratio_permille as int),
//@     ret.0.0 == x + padding,
//@     ret.0.0 + ret.0.1 + padding == ret.1.0,
//@     ret.0.1 >= 0, ret.1.1 >= 0,
//@     w >= 2 * padding ==> x <= ret.0.0 && ret.1.0 + ret.1.1 <= x + w,
//@     w >= 3 * padding ==> ret.1.0 + ret.1.1 + padding == x + w,
//@     w < 3 * padding ==> ret.0.1 == 0 && ret.1.1 == 0,
//@     w < 2 * padding ==> ret.1.0 > x + w,
//@     w < padding ==> ret.0.0 > x + w,
//@     -LIMIT <= ret.0.0 <= 3 * LIMIT, -LIMIT <= ret.1.0 <= 3 * LIMIT, ret.0.1 <= LIMIT, ret.1.1 <= LIMIT,
{
    let (lw, rw) = split_widths(w, padding, ratio_permille);
    let left_x = x + padding;
    let right_x = x + padding + lw + padding;
    ((left_x, lw), (right_x, rw))
}

/// The left and the right rectangle `(x, y, w, h)` of a two-way split of the work area: both lie `padding` below `y` and are
/// `h - 2 * padding` high (never negative).
#[allow(clippy::type_complexity)] // the plain-tuple form the statement is written against
pub fn split_geom(
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    padding: i64,
    ratio_permille: i64,
) -> ((i64, i64, i64, i64), (i64, i64, i64, i64)) //@ret
//@ requires
//@     -LIMIT <= x <= LIMIT, -LIMIT <= y <= LIMIT, -LIMIT <= w <= LIMIT, -LIMIT <= h <= LIMIT, 0 <= padding <= LIMIT,
//@     -2147483648 <= ratio_permille <= 2147483647,
//@ ensures
//@     ret.0.2 == left_width(room(w as int, padding as int), ratio_permille as int),
//@     ret.1.2 == room(w as int, padding as int) - ret.0.2,
//@     ret.0.0 == x + padding,
//@     ret.0.0 + ret.0.2 + padding == ret.1.0,
//@     ret.0.1 == y + padding, ret.1.1 == y + padding,
//@     ret.0.3 == (if h - 2 * padding > 0 { h - 2 * padding } else { 0 }), ret.1.3 == ret.0.3,
//@     w >= 2 * padding && h >= 2 * padding ==>
//@         contains((x as int, y as int, w as int, h as int), r4(ret.0)) && contains((x as int, y as int, w as int, h as int), r4(ret.1)),
//@     w >= 3 * padding ==> ret.1.0 + ret.1.2 + padding == x + w,
//@     w < 3 * padding ==> ret.0.2 == 0 && ret.1.2 == 0,
//@     w < 2 * padding ==> ret.1.0 > x + w,
//@     w < padding ==> ret.0.0 > x + w,
//@     h <= 2 * padding ==> ret.0.3 == 0 && ret.1.3 == 0,
//@     h < padding ==> ret.0.1 > y + h,
//@     !intersects(r4(ret.0), r4(ret.1)),
//@     fits_i32(r4(ret.0)), fits_i32(r4(ret.1)),
{
    let (left, right) = split_span(x, w, padding, ratio_permille);
    let ih = if h - 2 * padding > 0 {
        h - 2 * padding
    } else {
        0
    };
    let iy = y + padding;
    ((left.0, iy, left.1, ih), (right.0, iy, right.1, ih))
}

/// The work area shrunk by `padding` on every side (never negative in size).
pub fn inset(x: i64, y: i64, w: i64, h: i64, padding: i64) -> (i64, i64, i64, i64) //@ret
//@ requires -LIMIT <= x <= LIMIT, -LIMIT <= y <= LIMIT, -LIMIT <= w <= LIMIT, -LIMIT <= h <= LIMIT, 0 <= padding <= LIMIT,
//@ ensures
//@     ret.0 == x + padding, ret.1 == y + padding,
//@     ret.2 == (if w - 2 * padding > 0 { w - 2 * padding } else { 0 }),
//@     ret.3 == (if h - 2 * padding > 0 { h - 2 * padding } else { 0 }),
//@     w >= 2 * padding && h >= 2 * padding ==> contains((x as int, y as int, w as int, h as int), r4(ret)),
//@     fits_i32(r4(ret)),
{
    let iw = if w - 2 * padding > 0 {
        w - 2 * padding
    } else {
        0
    };
    let ih = if h - 2 * padding > 0 {
        h - 2 * padding
    } else {
        0
    };
    (x + padding, y + padding, iw, ih)
}

/// One quarter of the work area: the horizontal part comes from a half split of the width, the vertical part from a half split of
/// the height, `right` / `bottom` choose the side.
pub fn quarter(
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    padding: i64,
    right: bool,
    bottom: bool,
) -> (i64, i64, i64, i64) //@ret
//@ requires -LIMIT <= x <= LIMIT, -LIMIT <= y <= LIMIT, -LIMIT <= w <= LIMIT, -LIMIT <= h <= LIMIT, 0 <= padding <= LIMIT,
//@ ensures
//@     r4(ret) == quarter_spec(x as int, y as int, w as int, h as int, padding as int, right, bottom),
//@     w >= 2 * padding && h >= 2 * padding ==> contains((x as int, y as int, w as int, h as int), r4(ret)),
//@     fits_i32(r4(ret)),
{
    let (hl, hr) = split_span(x, w, padding, 500);
    let (vt, vb) = split_span(y, h, padding, 500);
    let hs = if right { hr } else { hl };
    let vs = if bottom { vb } else { vt };
    (hs.0, vs.0, hs.1, vs.1)
}
