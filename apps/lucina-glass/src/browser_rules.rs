// The integer rules under the browser controls of the glass kit (src/widgets/: text field, switch, stepper, tab chips, popover,
// sheet, dialog, progress bar): how big a hit area is and whether a point lands in it, how the keyboard focus moves, where a
// switch's thumb may be, how far a stepper may go, how wide a download's fill is, how a text field scrolls to keep its caret in
// view and when the caret shows, how wide a tab is, and where a popover, a sheet and a dialog sit so they never leave the screen.
// Verus checks this very file, as it does src/rules.rs: `//@` lines hold the proof (scripts/verify-shipped.sh, D-011);
// verification/glass-browser/mutants.sh plants bugs that Verus must refuse.
//
// Lengths are hundredths of a dp, as in src/rules.rs. Animation progress is rules::UNIT (16.16); the controls pass it through
// rules::lerp, so a thumb or a sheet is on its two ends exactly at the start and at the end of its move.

/// The longest side the layout rules accept, in hundredths of a dp: far beyond any screen, small enough that no sum overflows.
pub const MAX_LEN: i32 = 1_048_576;

/// "Done", in 16.16 fixed point (the same as rules::UNIT).
pub const UNIT: u32 = 65536;

/// The most controls in one focus order.
pub const MAX_FOCUS: u32 = 1024;

/// A hit area: from (`x`, `y`), `w` wide and `h` tall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct HitBox {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// A length grown to at least `min` (a 44 dp tap target): never shorter than it was.
pub fn grow_to(len: i32, min: i32) -> i32 //@ret
//@ ensures ret >= len, ret >= min, ret == len || ret == min,
{
    if len >= min { len } else { min }
}

/// The hit area of a control drawn at (`x`, `y`, `w`, `h`): at least `min` each way, grown evenly about the drawn rectangle,
/// so it always covers what is drawn.
pub fn hit_box(x: i32, y: i32, w: i32, h: i32, min: i32) -> HitBox //@ret
//@ requires -MAX_LEN <= x <= MAX_LEN, -MAX_LEN <= y <= MAX_LEN, 0 <= w <= MAX_LEN, 0 <= h <= MAX_LEN, 0 <= min <= MAX_LEN,
//@ ensures ret.w >= min, ret.h >= min, ret.w >= w, ret.h >= h,
//@     ret.x <= x, x + w <= ret.x + ret.w, ret.y <= y, y + h <= ret.y + ret.h,
//@     w >= min ==> (ret.x == x && ret.w == w), h >= min ==> (ret.y == y && ret.h == h),
//@     x - min <= ret.x, ret.x + ret.w <= x + w + min, y - min <= ret.y, ret.y + ret.h <= y + h + min,
//@     ret.x == x - (ret.w - w) / 2, ret.y == y - (ret.h - h) / 2,
//@     ret.w == (if w >= min { w } else { min }), ret.h == (if h >= min { h } else { min }),
{
    let nw = grow_to(w, min);
    let nh = grow_to(h, min);
    HitBox {
        x: x - (nw - w) / 2,
        y: y - (nh - h) / 2,
        w: nw,
        h: nh,
    }
}

//@ pub open spec fn in_box(b: HitBox, px: int, py: int) -> bool {
//@     b.x <= px && px < b.x + b.w && b.y <= py && py < b.y + b.h
//@ }

// Two hit boxes that touch, side by side (`a` ends where `b` starts) or one above the other, never both take the same point.
// This is about the boxes `hit` is given, not about `tap`: two touching controls smaller than the minimum have grown hit areas that
// overlap, so both `tap` calls take the points between them, and the caller decides by the order it asks them in.
// A library lemma: no proof in this file needs it; it states, for any caller laying controls out, what `hit` promises for two
// neighbours, and Verus checks it with the rest of the file.
//@ pub proof fn lemma_touching_never_both(a: HitBox, b: HitBox, px: int, py: int)
//@     requires a.x + a.w <= b.x || a.y + a.h <= b.y,
//@     ensures !(in_box(a, px, py) && in_box(b, px, py)),
//@ {
//@ }

/// Whether the point (`px`, `py`) lies in `b`: the left and top edges in, the right and bottom edges out, so two hit boxes
/// that touch never both take the same point (the grown areas of [`tap`] for two touching small controls can overlap).
pub fn hit(b: HitBox, px: i32, py: i32) -> bool //@ret
//@ ensures ret == in_box(b, px as int, py as int),
{
    px as i64 >= b.x as i64
        && (px as i64) < b.x as i64 + b.w as i64
        && py as i64 >= b.y as i64
        && (py as i64) < b.y as i64 + b.h as i64
}

// The hit area of `hit_box` as a spec: at least `min` each way, grown evenly about the drawn rectangle; whether (`px`, `py`) is in it.
//@ pub open spec fn in_hit_area(x: int, y: int, w: int, h: int, min: int, px: int, py: int) -> bool {
//@     let gw = if w >= min { w } else { min };
//@     let gh = if h >= min { h } else { min };
//@     let gx = x - (gw - w) / 2;
//@     let gy = y - (gh - h) / 2;
//@     gx <= px && px < gx + gw && gy <= py && py < gy + gh
//@ }

// What the exact hit area gives a caller: every point of the drawn rectangle is in it, and no point farther from it than `min`.
//@ pub proof fn lemma_hit_area_bounds(x: int, y: int, w: int, h: int, min: int, px: int, py: int)
//@     requires 0 <= w, 0 <= h, 0 <= min,
//@     ensures (x <= px && px < x + w && y <= py && py < y + h) ==> in_hit_area(x, y, w, h, min, px, py),
//@         in_hit_area(x, y, w, h, min, px, py) ==> (x - min <= px && px < x + w + min && y - min <= py && py < y + h + min),
//@ {
//@ }

/// Whether the point lands on a control drawn at (`x`, `y`, `w`, `h`) with a hit area of at least `min`: every point of the drawn
/// rectangle does, and no point farther from it than the growth does.
#[allow(clippy::too_many_arguments)] //@omit
pub fn tap(x: i32, y: i32, w: i32, h: i32, min: i32, px: i32, py: i32) -> bool //@ret
//@ requires -MAX_LEN <= x <= MAX_LEN, -MAX_LEN <= y <= MAX_LEN, 0 <= w <= MAX_LEN, 0 <= h <= MAX_LEN, 0 <= min <= MAX_LEN,
//@ ensures ret == in_hit_area(x as int, y as int, w as int, h as int, min as int, px as int, py as int),
{
    let b = hit_box(x, y, w, h, min);
    hit(b, px, py)
}

/// Where the keyboard focus goes from control `cur` of `n`, forwards or `back`, wrapping at both ends.
pub fn focus_step(cur: u32, n: u32, back: bool) -> u32 //@ret
//@ requires 0 < n <= MAX_FOCUS, cur < n,
//@ ensures ret < n,
//@     !back && cur + 1 < n ==> ret == cur + 1, !back && cur + 1 == n ==> ret == 0,
//@     back && cur > 0 ==> ret == cur - 1, back && cur == 0 ==> ret == n - 1,
{
    if back {
        if cur == 0 { n - 1 } else { cur - 1 }
    } else if cur + 1 == n {
        0
    } else {
        cur + 1
    }
}

/// The focus after the order shrank to `n` controls (a tab closed, a row hidden): the same control if it is still there, else the
/// last one; none for an empty order (`n` 0 gives 0, which then names no control).
pub fn focus_clamp(cur: u32, n: u32) -> u32 //@ret
//@ ensures n > 0 ==> ret < n, n == 0 ==> ret == 0, cur < n ==> ret == cur, (n > 0 && cur >= n) ==> ret == n - 1,
{
    if n == 0 {
        0
    } else if cur >= n {
        n - 1
    } else {
        cur
    }
}

/// Where a switch's thumb travels between: its left edge at `from` (off) and at `to` (on), in a track at `x`, `w` wide, with a
/// thumb `d` wide and `pad` of margin. Feed the two to rules::lerp with the flip's progress: the thumb never leaves the track.
pub fn thumb_ends(x: i32, w: i32, d: i32, pad: i32) -> (i32, i32) //@ret
//@ requires -MAX_LEN <= x <= MAX_LEN, 0 <= d <= MAX_LEN, 0 <= pad <= MAX_LEN, d + 2 * pad <= w <= MAX_LEN,
//@ ensures ret.0 == x + pad, ret.1 == x + w - pad - d, x + pad <= ret.0, ret.0 <= ret.1, ret.1 + d + pad == x + w,
{
    (x + pad, x + w - pad - d)
}

/// A stepper's value after one step up or down: never outside `lo` to `hi`, whatever it was, and exactly one `step` away when
/// that stays inside.
pub fn step_value(v: i32, lo: i32, hi: i32, step: i32, up: bool) -> i32 //@ret
//@ requires lo <= hi, 0 <= step,
//@ ensures lo <= ret <= hi,
//@     ret == (if (if up { v + step } else { v - step }) < lo { lo as int }
//@         else if (if up { v + step } else { v - step }) > hi { hi as int }
//@         else { if up { v + step } else { v - step } }),
//@     (lo <= v <= hi && up && v + step <= hi) ==> ret == v + step,
//@     (lo <= v <= hi && !up && v - step >= lo) ==> ret == v - step,
//@     (lo <= v <= hi && up && v + step > hi) ==> ret == hi,
//@     (lo <= v <= hi && !up && v - step < lo) ==> ret == lo,
{
    let t = if up {
        v as i64 + step as i64
    } else {
        v as i64 - step as i64
    };
    if t < lo as i64 {
        lo
    } else if t > hi as i64 {
        hi
    } else {
        t as i32
    }
}

/// Whether the stepper's button is live: it is off at the end it has reached.
pub fn can_step(v: i32, lo: i32, hi: i32, up: bool) -> bool //@ret
//@ ensures ret == (if up { v < hi } else { v > lo }),
{
    if up { v < hi } else { v > lo }
}

/// How far `done` is of `total`, in UNITs: 0 for nothing done or an unknown (zero) total, [`UNIT`] once everything is done.
pub fn fraction(done: u64, total: u64) -> u32 //@ret
//@ ensures ret <= UNIT, (total > 0 && done >= total) ==> ret == UNIT, done == 0 ==> ret == 0, total == 0 ==> ret == 0,
//@     (total > 0 && done < total) ==> (ret as int == (done as int) * (UNIT as int) / (total as int) && ret < UNIT),
{
    if total == 0 {
        return 0;
    }
    if done >= total {
        return UNIT;
    }
    //@ assert((done as u128) * 65536 <= 0xffff_ffff_ffff_ffff * 65536) by (nonlinear_arith) requires done <= 0xffff_ffff_ffff_ffffu64;
    let scaled = (done as u128) * (UNIT as u128);
    //@ assert(done == 0 ==> scaled == 0);
    //@ assert(scaled == 0 ==> scaled / (total as u128) == 0) by (nonlinear_arith) requires total > 0;
    let p = scaled / (total as u128);
    //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(scaled as int, total as int); }
    //@ proof { vstd::arithmetic::div_mod::lemma_mod_pos_bound(scaled as int, total as int); }
    //@ assert(p < 65536) by (nonlinear_arith)
    //@     requires scaled as int == (total as int) * (p as int) + (scaled as int) % (total as int),
    //@         0 <= (scaled as int) % (total as int), scaled as int == (done as int) * 65536, done < total, total > 0;
    if p > UNIT as u128 { UNIT } else { p as u32 }
}

/// A progress bar's fill, `t` UNITs of its `w`: never wider than the bar, empty at 0, the whole bar at [`UNIT`].
pub fn fill_width(w: u32, t: u32) -> u32 //@ret
//@ requires t <= UNIT,
//@ ensures ret <= w, t == UNIT ==> ret == w, t == 0 ==> ret == 0, ret as int == (w as int) * (t as int) / (UNIT as int),
{
    //@ assert((w as u64) * (t as u64) <= (w as u64) * 65536) by (nonlinear_arith) requires t <= 65536;
    //@ assert((w as u64) * 65536 <= 0xffff_ffff * 65536) by (nonlinear_arith) requires w <= 0xffff_ffffu32;
    //@ proof { vstd::arithmetic::div_mod::lemma_div_is_ordered((w as int) * (t as int), (w as int) * 65536, 65536); }
    //@ proof { vstd::arithmetic::div_mod::lemma_div_by_multiple(w as int, 65536); }
    let v = (w as u64) * (t as u64) / (UNIT as u64);
    //@ assert(v <= w);
    //@ assert(t == 0 ==> v == 0) by (nonlinear_arith) requires v as int == (w as int) * (t as int) / 65536;
    //@ assert(t == 65536 ==> v == w) by (nonlinear_arith) requires v as int == (w as int) * (t as int) / 65536;
    v as u32
}

/// Where a text field's text starts (its scroll, from the left of the text) so the caret stays in the `view` wide window: the
/// scroll does not move while the caret is already in view, else it moves just far enough.
pub fn scroll_for_caret(scroll: i32, caret: i32, view: i32) -> i32 //@ret
//@ requires 0 <= scroll <= MAX_LEN, 0 <= caret <= MAX_LEN, 0 < view <= MAX_LEN,
//@ ensures 0 <= ret, ret <= caret, caret <= ret + view,
//@     (scroll <= caret && caret <= scroll + view) ==> ret == scroll,
//@     caret < scroll ==> ret == caret, caret > scroll + view ==> ret == caret - view,
{
    if caret < scroll {
        caret
    } else if caret > scroll + view {
        caret - view
    } else {
        scroll
    }
}

/// The caret's place (in characters) after one arrow key: one step, stopping at both ends of the text.
pub fn caret_step(caret: u32, len: u32, back: bool) -> u32 //@ret
//@ requires caret <= len,
//@ ensures ret <= len,
//@     back && caret > 0 ==> ret == caret - 1, back && caret == 0 ==> ret == 0,
//@     !back && caret < len ==> ret == caret + 1, !back && caret == len ==> ret == len,
{
    if back {
        if caret == 0 { 0 } else { caret - 1 }
    } else if caret >= len {
        len
    } else {
        caret + 1
    }
}

/// Whether the caret shows `ms` after the last key: solid for the first half of the period, dark for the second, so a key press
/// always shows it. A zero period never blinks.
pub fn caret_visible(ms: u64, period: u64) -> bool //@ret
//@ ensures period == 0 ==> ret, (period > 0 && ms % period == 0) ==> ret, (period > 0 && ms % period >= period - period / 2) ==> !ret,
//@     ret == (period == 0 || ms % period < period - period / 2),
{
    if period == 0 {
        return true;
    }
    ms % period < period - period / 2
}

/// The width of each of `n` tabs side by side over `avail` with `gap` between, at most `max` each: all of them together fit.
pub fn tab_width(avail: i32, n: u32, gap: i32, max: i32) -> i32 //@ret
//@ requires 0 < n <= 64, 0 <= avail <= MAX_LEN, 0 <= gap <= MAX_LEN, 0 <= max <= MAX_LEN, gap * (n - 1) <= avail,
//@ ensures 0 <= ret <= max, ret * (n as int) + gap * (n as int - 1) <= avail,
//@     ret == max || (ret + 1) * (n as int) + gap * (n as int - 1) > avail,
//@     ret == (if (avail - gap * (n as int - 1)) / (n as int) > max { max as int } else { (avail - gap * (n as int - 1)) / (n as int) }),
{
    //@ assert(gap * (n as int - 1) <= MAX_LEN * 63) by (nonlinear_arith) requires 0 <= gap <= MAX_LEN, 0 < n <= 64;
    let free = (avail as i64 - gap as i64 * (n as i64 - 1)) as u64;
    let each = free / (n as u64);
    //@ proof { vstd::arithmetic::div_mod::lemma_fundamental_div_mod(free as int, n as int); }
    //@ proof { vstd::arithmetic::div_mod::lemma_mod_pos_bound(free as int, n as int); }
    //@ assert(each * (n as u64) <= free) by (nonlinear_arith) requires each as int == (free as int) / (n as int), n > 0;
    let w = if each > max as u64 { max as u64 } else { each };
    //@ assert(w * (n as u64) <= each * (n as u64)) by (nonlinear_arith) requires w <= each;
    //@ assert(w == max || (w as int + 1) * (n as int) > free as int) by (nonlinear_arith)
    //@     requires w == max || w == each, free as int == (n as int) * (each as int) + (free as int) % (n as int),
    //@         (free as int) % (n as int) < n as int, each as int == (free as int) / (n as int);
    w as i32
}

/// Where a popover's left edge goes when it hangs from a control whose right end is at `anchor_right`: right-aligned under it,
/// kept `margin` inside the screen on both sides.
pub fn popover_x(anchor_right: i32, w: i32, margin: i32, screen: i32) -> i32 //@ret
//@ requires -MAX_LEN <= anchor_right <= MAX_LEN, 0 <= w <= MAX_LEN, 0 <= margin <= MAX_LEN, w + 2 * margin <= screen <= MAX_LEN,
//@ ensures margin <= ret, ret + w <= screen - margin,
//@     (margin <= anchor_right - w && anchor_right <= screen - margin) ==> ret == anchor_right - w,
//@     ret == (if anchor_right - w < margin { margin as int } else if anchor_right - w > screen - margin - w { screen - margin - w }
//@         else { anchor_right - w }),
{
    let x = anchor_right as i64 - w as i64;
    if x < margin as i64 {
        margin
    } else if x > screen as i64 - margin as i64 - w as i64 {
        screen - margin - w
    } else {
        x as i32
    }
}

/// The top of a bottom sheet `total` tall that rests `margin` above the screen's bottom edge: wholly on screen.
pub fn sheet_top(screen_h: i32, margin: i32, total: i32) -> i32 //@ret
//@ requires 0 <= margin <= MAX_LEN, 0 <= total <= MAX_LEN, total + margin <= screen_h <= MAX_LEN,
//@ ensures ret >= 0, ret + total + margin == screen_h,
{
    screen_h - margin - total
}

/// The start of a `len` long piece centred in the stretch from `start`, `outer` long (a dialog in the window): inside it.
pub fn centre(start: i32, outer: i32, len: i32) -> i32 //@ret
//@ requires -MAX_LEN <= start <= MAX_LEN, 0 <= len <= outer <= MAX_LEN,
//@ ensures start <= ret, ret + len <= start + outer, ret == start + (outer - len) / 2,
{
    start + (outer - len) / 2
}
