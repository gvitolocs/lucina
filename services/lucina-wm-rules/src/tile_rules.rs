// The integer arithmetic of a two-way split: how wide the left and the right side are for a divider ratio, and where a divider
// ratio may go so that neither side gets narrower than a minimum. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011); verification/juno-wm-rules/mutants.sh plants bugs that Verus must refuse. tile.rs calls these
// functions and holds no copy of the rules. All arithmetic is `i64`, and the proof covers widths, paddings and minima up to 8192
// pixels (what `tile::Rect` documents), where nothing can overflow.
//
// A work area narrower than `2 * padding` cannot hold its own edge paddings: both sides are then 0 wide and the right one starts
// past the work area. The proof states the fit only for `w >= 2 * padding` (the whole row) and for `w >= padding` (the two sides
// and the one padding between them); it does not claim more.

/// The widest work area, padding or minimum width the proof covers.
pub const LIMIT: i64 = 8192;

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
//@     padding + ret.0 <= padding + ret.0 + padding,
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
    //@ assert(0 <= avail * ratio <= 8192 * 1000) by(nonlinear_arith) requires 0 <= avail <= 8192, 0 <= ratio <= 1000;
    let mut lw = avail * ratio / 1000;
    //@ assert(lw <= avail) by(nonlinear_arith) requires lw == avail * ratio / 1000, 0 <= avail, 0 <= ratio <= 1000;
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
    //@ let ghost wi: int = w as int;
    //@ let ghost mi: int = min_w as int;
    //@ let ghost li: int = lo as int;
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
    //@ assert(ki * wi >= mi * 1000 && (1000 - ki) * wi >= mi * 1000) by(nonlinear_arith)
    //@     requires li * wi >= mi * 1000, li <= ki <= 1000 - li, wi > 0;
    //@ assert(ri * wi >= mi * 1000 && (1000 - ri) * wi >= mi * 1000 && 0 <= ri <= 1000 ==> li <= ri <= 1000 - li) by(nonlinear_arith)
    //@     requires wi > 0, (li - 1) * wi < mi * 1000;
    kept
}
