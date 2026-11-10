// The integer rules under Fortuna's look that a test would miss in a corner: which grid line the Games tab scrolls
// to, how a cover is fitted into its box, and the rectangle rules the compositor's dirty set rests on (clipping to the
// screen, when two rectangles share a pixel, a rectangle holding two others). Verus checks this very file: `//@` lines
// hold the proof, see scripts/verify-shipped.sh (D-011); verification/fortuna-look/mutants.sh plants bugs that Verus
// must refuse. layout.rs and compose.rs call these functions and keep no copy of the rules.
//
// Rectangles are (x, y, w, h) in pixels; a rectangle holds the pixels x <= px < x + w, y <= py < y + h, so one with
// w <= 0 or h <= 0 holds none.
//
// Not proved: that `Dirty::add` keeps its rectangles disjoint (the loop over the array; compose.rs's pixel-exact
// composed == painted test covers it), the centring of a fitted cover (layout.rs, unit tests).

/// A rectangle: x, y, w, h.
pub type Rect4 = (i32, i32, i32, i32);

/// The largest coordinate or size the rectangle rules take: far beyond a 256 x 192 screen, small enough that no sum
/// overflows.
pub const MAX_COORD: i32 = 0x10000;
/// The largest box side or shape term `fit_dims` takes: products stay far inside an i32.
pub const MAX_SIDE: i32 = 4096;

//@ pub open spec fn has(r: Rect4, px: int, py: int) -> bool { r.0 <= px < r.0 + r.2 && r.1 <= py < r.1 + r.3 }

//@ pub open spec fn small(r: Rect4) -> bool {
//@     -MAX_COORD <= r.0 <= MAX_COORD && -MAX_COORD <= r.1 <= MAX_COORD && -MAX_COORD <= r.2 <= MAX_COORD
//@         && -MAX_COORD <= r.3 <= MAX_COORD
//@ }

/// The first grid line on screen when line `sel` of `total` is selected and `lines` show at a time: the selection
/// sits on the second visible line (so the grid does not jump on the first press), except at the very top and bottom.
pub fn first_line(sel: usize, total: usize, lines: usize) -> usize //@ret
//@ requires lines >= 2,
//@ ensures
//@     sel < total ==> ret <= sel && sel < ret + lines,
//@     total >= lines ==> ret + lines <= total,
//@     total < lines ==> ret == 0,
//@     sel == 0 ==> ret == 0,
//@     1 <= sel && sel + lines <= total + 1 ==> ret == sel - 1,
{
    let last_first = if total > lines { total - lines } else { 0 };
    let want = if sel > 0 { sel - 1 } else { 0 };
    if want > last_first {
        last_first
    } else {
        want
    }
}

/// The size of a cover of shape `w`:`h` fitted into a `bw` x `bh` box: as wide as the box, or as tall where that is
/// too tall; never cropped, and off the true shape by less than one pixel of rounding.
pub fn fit_dims(bw: i32, bh: i32, w: i32, h: i32) -> (i32, i32) //@ret
//@ requires 0 <= bw <= MAX_SIDE, 0 <= bh <= MAX_SIDE, 1 <= w <= MAX_SIDE, 1 <= h <= MAX_SIDE,
//@ ensures
//@     0 <= ret.0 <= bw, 0 <= ret.1 <= bh,
//@     (ret.0 == bw && 0 <= bw * h - ret.1 * w < w) || (ret.1 == bh && 0 <= bh * w - ret.0 * h < h),
{
    //@ assert(0 <= (bw as int) * (h as int) <= 4096 * 4096) by (nonlinear_arith) requires 0 <= bw <= 4096, 1 <= h <= 4096;
    //@ assert(0 <= (bh as int) * (w as int) <= 4096 * 4096) by (nonlinear_arith) requires 0 <= bh <= 4096, 1 <= w <= 4096;
    let p = (bw as u32) * (h as u32);
    let tall = p / (w as u32);
    //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(p as int, w as int); }
    //@ proof { vstd::arithmetic::div_mod::lemma_mod_pos_bound(p as int, w as int); }
    //@ assert((w as int) * (tall as int) <= p) ;
    //@ assert(tall <= p) by (nonlinear_arith) requires (w as int) * (tall as int) <= p, w >= 1, tall >= 0;
    //@ assert((bw as int) * (h as int) - (tall as int) * (w as int) == (p as int) % (w as int)) by (nonlinear_arith) requires p == (bw as int) * (h as int), p == (w as int) * (tall as int) + (p as int) % (w as int);
    if tall <= bh as u32 {
        (bw, tall as i32)
    } else {
        let q = (bh as u32) * (w as u32);
        let cw = q / (h as u32);
        //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(q as int, h as int); }
        //@ proof { vstd::arithmetic::div_mod::lemma_mod_pos_bound(q as int, h as int); }
        //@ assert((w as int) * (tall as int) >= (w as int) * ((bh as int) + 1)) by (nonlinear_arith) requires tall >= bh + 1, w >= 1;
        //@ assert(q < p) by (nonlinear_arith) requires q == (bh as int) * (w as int), (w as int) * (tall as int) <= p, (w as int) * (tall as int) >= (w as int) * ((bh as int) + 1), w >= 1;
        //@ assert(cw < bw) by (nonlinear_arith) requires (h as int) * (cw as int) <= q, q < p, p == (bw as int) * (h as int), h >= 1;
        //@ assert((bh as int) * (w as int) - (cw as int) * (h as int) == (q as int) % (h as int)) by (nonlinear_arith) requires q == (bh as int) * (w as int), q == (h as int) * (cw as int) + (q as int) % (h as int);
        (cw as i32, bh)
    }
}

/// The part of `r` on a `sw` x `sh` screen: exactly the pixels of `r` that are on screen, never a negative size.
pub fn clip(r: Rect4, sw: i32, sh: i32) -> Rect4 //@ret
//@ requires small(r), 0 <= sw <= MAX_COORD, 0 <= sh <= MAX_COORD,
//@ ensures
//@     ret.2 >= 0, ret.3 >= 0, small(ret),
//@     forall|px: int, py: int| has(ret, px, py) <==> (has(r, px, py) && 0 <= px < sw && 0 <= py < sh),
{
    let x0 = if r.0 > 0 { r.0 } else { 0 };
    let y0 = if r.1 > 0 { r.1 } else { 0 };
    let x1 = if r.0 + r.2 < sw { r.0 + r.2 } else { sw };
    let y1 = if r.1 + r.3 < sh { r.1 + r.3 } else { sh };
    (x0, y0, if x1 > x0 { x1 - x0 } else { 0 }, if y1 > y0 { y1 - y0 } else { 0 })
}

/// Whether `a` and `b` share a pixel; ones that only touch do not.
pub fn overlaps(a: Rect4, b: Rect4) -> bool //@ret
//@ requires small(a), small(b),
//@ ensures ret <==> exists|px: int, py: int| has(a, px, py) && has(b, px, py),
{
    let r = a.2 > 0
        && a.3 > 0
        && b.2 > 0
        && b.3 > 0
        && a.0 < b.0 + b.2
        && b.0 < a.0 + a.2
        && a.1 < b.1 + b.3
        && b.1 < a.1 + a.3;
    //@ proof {
    //@     let px: int = if a.0 < b.0 { b.0 as int } else { a.0 as int };
    //@     let py: int = if a.1 < b.1 { b.1 as int } else { a.1 as int };
    //@     if r { assert(has(a, px, py) && has(b, px, py)); }
    //@     assert forall|qx: int, qy: int| has(a, qx, qy) && has(b, qx, qy) implies r by {}
    //@ }
    r
}

/// The smallest rectangle holding every pixel of `a` and of `b`.
pub fn union(a: Rect4, b: Rect4) -> Rect4 //@ret
//@ requires small(a), small(b), a.2 >= 0, a.3 >= 0, b.2 >= 0, b.3 >= 0,
//@ ensures forall|px: int, py: int| has(a, px, py) || has(b, px, py) ==> has(ret, px, py),
{
    let x0 = if a.0 < b.0 { a.0 } else { b.0 };
    let y0 = if a.1 < b.1 { a.1 } else { b.1 };
    let x1 = if a.0 + a.2 > b.0 + b.2 { a.0 + a.2 } else { b.0 + b.2 };
    let y1 = if a.1 + a.3 > b.1 + b.3 { a.1 + a.3 } else { b.1 + b.3 };
    (x0, y0, x1 - x0, y1 - y0)
}
