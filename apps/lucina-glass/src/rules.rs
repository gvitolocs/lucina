// The integer rules under the glass kit's drawing: how far an animation is, where an interpolated value may land, how a row of
// equal cells tiles a length, how a month falls on a 7-column grid, how the pager's dots trade width, how a stagger is capped and
// how a timer splits into minutes and seconds. Verus checks this very file: `//@` lines hold the proof, see
// scripts/verify-shipped.sh (D-011). The widgets call these functions for every position they draw and hold no copy of the rules.
//
// Progress runs in 16.16 fixed point: [`UNIT`] is "done". Float easings and springs (src/anim.rs) are turned into a unit with
// `anim::unit`, which clamps, so a curve can bend time but never push a value outside the ends these functions guarantee.
//
// Not proved: the float easing curves themselves, the anti-aliasing and blending (tests/look.rs renders and checks pixels).

/// "Done", in 16.16 fixed point.
pub const UNIT: u32 = 65536;

/// The longest side the layout rules accept, in pixels: far beyond any screen, small enough that no product overflows.
pub const MAX_LEN: i32 = 1_048_576;

/// The most cells in one row of [`cell`].
pub const MAX_CELLS: u32 = 64;

/// How far `elapsed` is into `dur`, in UNITs: 0 at the start, [`UNIT`] from `dur` on. A zero duration is done at once.
pub fn progress(elapsed: u64, dur: u64) -> u32 //@ret
//@ ensures ret <= UNIT, elapsed >= dur ==> ret == UNIT, (dur > 0 && elapsed == 0) ==> ret == 0,
{
    if elapsed >= dur {
        return UNIT;
    }
    //@ assert((elapsed as u128) * 65536 <= 0xffff_ffff_ffff_ffff * 65536) by (nonlinear_arith) requires elapsed <= 0xffff_ffff_ffff_ffffu64;
    let scaled = (elapsed as u128) * (UNIT as u128);
    //@ assert(elapsed == 0 ==> scaled == 0);
    //@ assert(scaled == 0 ==> scaled / (dur as u128) == 0) by (nonlinear_arith) requires dur > 0;
    let p = scaled / (dur as u128);
    if p > UNIT as u128 { UNIT } else { p as u32 }
}

/// The value `t` of the way from `a` to `b` (`t` in UNITs): exactly `a` at 0 and `b` at [`UNIT`], and never outside the two.
pub fn lerp(a: i32, b: i32, t: u32) -> i32 //@ret
//@ requires t <= UNIT,
//@ ensures (a <= b ==> a <= ret && ret <= b), (b <= a ==> b <= ret && ret <= a), t == 0 ==> ret == a, t == UNIT ==> ret == b,
{
    if a <= b {
        let d = (b as i64 - a as i64) as u64;
        //@ assert(d * (t as u64) <= d * 65536) by (nonlinear_arith) requires t <= 65536;
        //@ assert(d <= 0x1_0000_0000);
        //@ assert(d * 65536 <= 0x1_0000_0000 * 65536) by (nonlinear_arith) requires d <= 0x1_0000_0000;
        let step = d * (t as u64) / (UNIT as u64);
        //@ assert(step <= d);
        //@ assert(t == 0 ==> step == 0) by (nonlinear_arith) requires step as int == (d as int) * (t as int) / 65536;
        //@ assert(t == 65536 ==> step == d) by (nonlinear_arith) requires step as int == (d as int) * (t as int) / 65536;
        (a as i64 + step as i64) as i32
    } else {
        let d = (a as i64 - b as i64) as u64;
        //@ assert(d * (t as u64) <= d * 65536) by (nonlinear_arith) requires t <= 65536;
        //@ assert(d <= 0x1_0000_0000);
        //@ assert(d * 65536 <= 0x1_0000_0000 * 65536) by (nonlinear_arith) requires d <= 0x1_0000_0000;
        let step = d * (t as u64) / (UNIT as u64);
        //@ assert(step <= d);
        //@ assert(t == 0 ==> step == 0) by (nonlinear_arith) requires step as int == (d as int) * (t as int) / 65536;
        //@ assert(t == 65536 ==> step == d) by (nonlinear_arith) requires step as int == (d as int) * (t as int) / 65536;
        (a as i64 - step as i64) as i32
    }
}

/// A count-up's shown value `t` of the way to `target`: never above it, and exactly it once done.
pub fn count_up(target: u32, t: u32) -> u32 //@ret
//@ requires t <= UNIT,
//@ ensures ret <= target, t == UNIT ==> ret == target, t == 0 ==> ret == 0,
{
    //@ assert((target as u64) * (t as u64) <= (target as u64) * 65536) by (nonlinear_arith) requires t <= 65536;
    //@ assert((target as u64) * 65536 <= 0xffff_ffff * 65536) by (nonlinear_arith) requires target <= 0xffff_ffffu32;
    //@ proof { vstd::arithmetic::div_mod::lemma_div_is_ordered((target as int) * (t as int), (target as int) * 65536, 65536); }
    //@ proof { vstd::arithmetic::div_mod::lemma_div_by_multiple(target as int, 65536); }
    let v = (target as u64) * (t as u64) / (UNIT as u64);
    //@ assert(v <= target);
    //@ assert(t == 0 ==> v == 0) by (nonlinear_arith) requires v as int == (target as int) * (t as int) / 65536;
    //@ assert(t == 65536 ==> v == target) by (nonlinear_arith) requires v as int == (target as int) * (t as int) / 65536;
    v as u32
}

/// Whole per cent of `done` out of `total`, at most 100; an empty total is 0 %.
pub fn percent(done: u32, total: u32) -> u32 //@ret
//@ ensures ret <= 100, (total > 0 && done >= total) ==> ret == 100,
{
    if total == 0 {
        return 0;
    }
    if done >= total {
        return 100;
    }
    //@ assert((done as u64) * 100 < (total as u64) * 100) by (nonlinear_arith) requires done < total;
    let p = (done as u64) * 100 / (total as u64);
    //@ assert(p < 100) by (nonlinear_arith) requires p as int == (done as int) * 100 / (total as int), done < total, total > 0;
    p as u32
}

/// One stretch of a row: from `start`, `len` long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Span {
    pub start: i32,
    pub len: i32,
}

//@ pub open spec fn free_len(len: int, n: int, gap: int) -> int { len - gap * (n - 1) }
//@ pub open spec fn cell_start(start: int, len: int, n: int, gap: int, i: int) -> int { start + (free_len(len, n, gap) * i) / n + gap * i }
//@ pub open spec fn row_ok(start: int, len: int, n: int, gap: int) -> bool {
//@     0 < n <= MAX_CELLS && 0 <= gap <= MAX_LEN && 0 <= len <= MAX_LEN && -MAX_LEN <= start <= MAX_LEN && gap * (n - 1) <= len
//@ }

/// Cell `i` of `n` equal cells across `len` from `start`, `gap` apart. The cells tile the row exactly: the first starts at
/// `start`, each next one starts `gap` after this one ends, and the last ends at `start + len`; sizes differ by at most one pixel.
pub fn cell(start: i32, len: i32, n: u32, gap: i32, i: u32) -> Span //@ret
//@ requires row_ok(start as int, len as int, n as int, gap as int), i < n,
//@ ensures ret.start == cell_start(start as int, len as int, n as int, gap as int, i as int),
//@     ret.start + ret.len + gap == cell_start(start as int, len as int, n as int, gap as int, i + 1),
//@     ret.len >= 0, start <= ret.start, ret.start + ret.len <= start + len,
//@     i == 0 ==> ret.start == start, i + 1 == n ==> ret.start + ret.len == start + len,
{
    //@ assert(gap * (n as int - 1) <= MAX_LEN * 63) by (nonlinear_arith) requires 0 <= gap <= MAX_LEN, 0 < n <= 64;
    let free = (len as i64 - gap as i64 * (n as i64 - 1)) as u64;
    //@ let ghost fl = free_len(len as int, n as int, gap as int);
    //@ assert(free as int == fl && 0 <= fl <= MAX_LEN);
    //@ assert(fl * (i as int) <= fl * (i as int + 1)) by (nonlinear_arith) requires fl >= 0;
    //@ assert(fl * (i as int + 1) <= fl * (n as int)) by (nonlinear_arith) requires fl >= 0, i < n;
    //@ assert(fl * (n as int) <= MAX_LEN * 64) by (nonlinear_arith) requires 0 <= fl <= MAX_LEN, n <= 64;
    let a = free * (i as u64) / (n as u64);
    let b = free * (i as u64 + 1) / (n as u64);
    //@ proof { vstd::arithmetic::div_mod::lemma_div_is_ordered(fl * (i as int), fl * (i as int + 1), n as int); }
    //@ proof { vstd::arithmetic::div_mod::lemma_div_is_ordered(fl * (i as int + 1), fl * (n as int), n as int); }
    //@ proof { vstd::arithmetic::div_mod::lemma_div_by_multiple(fl, n as int); }
    //@ assert(a as int == fl * (i as int) / (n as int) && b as int == fl * (i as int + 1) / (n as int));
    //@ assert(a <= b && b as int <= fl);
    //@ assert(i == 0 ==> a == 0);
    //@ assert(gap * (i as int) <= gap * (n as int - 1)) by (nonlinear_arith) requires gap >= 0, i < n;
    //@ assert(gap * (i as int) >= 0) by (nonlinear_arith) requires gap >= 0, i >= 0;
    //@ assert(gap * (i as int + 1) == gap * (i as int) + gap) by (nonlinear_arith);
    //@ assert(i as int + 1 == n ==> b as int == fl);
    let x0 = start as i64 + a as i64 + gap as i64 * i as i64;
    let x1 = start as i64 + b as i64 + gap as i64 * i as i64;
    //@ assert(-MAX_LEN <= x0 && x0 <= x1 && x1 <= start + len && start + len <= 2 * MAX_LEN);
    //@ assert(i32::MIN <= x0 && x1 <= i32::MAX);
    Span { start: x0 as i32, len: (x1 - x0) as i32 }
}

/// Cells `i` to `i + span - 1` of [`cell`]'s row as one stretch: a wide calendar tile, a segmented control's thumb.
pub fn cells(start: i32, len: i32, n: u32, gap: i32, i: u32, span: u32) -> Span //@ret
//@ requires row_ok(start as int, len as int, n as int, gap as int), 0 < span, i + span <= n,
//@ ensures ret.start == cell_start(start as int, len as int, n as int, gap as int, i as int),
//@     ret.len >= 0, start <= ret.start, ret.start + ret.len <= start + len,
{
    let first = cell(start, len, n, gap, i);
    let last = cell(start, len, n, gap, i + span - 1);
    //@ proof { lemma_cell_order(start as int, len as int, n as int, gap as int, i as int, (i + span - 1) as int); }
    Span { start: first.start, len: last.start + last.len - first.start }
}

//@ /// Cells start in order along the row.
//@ pub proof fn lemma_cell_order(start: int, len: int, n: int, gap: int, i: int, j: int)
//@     requires row_ok(start, len, n, gap), 0 <= i <= j < n,
//@     ensures cell_start(start, len, n, gap, i) <= cell_start(start, len, n, gap, j),
//@ {
//@     let free = free_len(len, n, gap);
//@     assert(free >= 0) by (nonlinear_arith) requires gap * (n - 1) <= len, free == len - gap * (n - 1);
//@     assert(free * i <= free * j) by (nonlinear_arith) requires free >= 0, i <= j;
//@     vstd::arithmetic::div_mod::lemma_div_is_ordered(free * i, free * j, n);
//@     assert(gap * i <= gap * j) by (nonlinear_arith) requires gap >= 0, i <= j;
//@ }

/// Columns of the calendar grid and of the day chips: Monday to Sunday.
pub const WEEK: u32 = 7;
/// The most rows a month takes on a 7-column grid.
pub const MAX_ROWS: u32 = 6;

/// Whether `y` is a leap year (Gregorian).
pub fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in month `m` (1 to 12) of year `y`.
pub fn days_in_month(y: u32, m: u32) -> u32 //@ret
//@ requires 1 <= m <= 12,
//@ ensures 28 <= ret <= 31,
{
    if m == 2 {
        if is_leap(y) { 29 } else { 28 }
    } else if m == 4 || m == 6 || m == 9 || m == 11 {
        30
    } else {
        31
    }
}

/// The weekday of day `d` of month `m` of year `y`, 0 = Monday (Sakamoto's method, for years from 1 on).
pub fn weekday(y: u32, m: u32, d: u32) -> u32 //@ret
//@ requires 1 <= y <= 100000, 1 <= m <= 12, 1 <= d <= 31,
//@ ensures ret < WEEK,
{
    let t: [u32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let yy = if m < 3 { y - 1 } else { y };
    let k = t[(m - 1) as usize];
    // 0 = Sunday from the formula; Monday-first below.
    let sun = (yy + yy / 4 - yy / 100 + yy / 400 + k + d) % 7;
    (sun + 6) % 7
}

/// Rows a month needs on the grid when its first day falls in column `first`.
pub fn rows_needed(first: u32, days: u32) -> u32 //@ret
//@ requires first < WEEK, 28 <= days <= 31,
//@ ensures 4 <= ret <= MAX_ROWS, first + days <= ret * WEEK, (ret - 1) * WEEK < first + days,
{
    (first + days + WEEK - 1) / WEEK
}

/// Where day `day` sits on the grid: (row, column).
pub fn cell_of(day: u32, first: u32, days: u32) -> (u32, u32) //@ret
//@ requires first < WEEK, 1 <= day <= days, days <= 31,
//@ ensures ret.0 < MAX_ROWS, ret.1 < WEEK, ret.0 * WEEK + ret.1 == first + day - 1,
//@     ret.0 < rows_needed_spec(first as int, days as int),
{
    let _ = days;
    let i = first + day - 1;
    (i / WEEK, i % WEEK)
}

//@ pub open spec fn rows_needed_spec(first: int, days: int) -> int { (first + days + 6) / 7 }

/// The day shown at (row, column), or 0 for a cell before the first or after the last.
pub fn day_at(row: u32, col: u32, first: u32, days: u32) -> u32 //@ret
//@ requires row < MAX_ROWS, col < WEEK, first < WEEK, days <= 31,
//@ ensures ret <= days,
//@     ret != 0 ==> row * WEEK + col == first + ret - 1,
//@     ret == 0 ==> (row * WEEK + col < first || row * WEEK + col >= first + days),
{
    let i = row * WEEK + col;
    if i < first || i >= first + days { 0 } else { i - first + 1 }
}

/// The two dots' widths while the pager's active dot moves from one to the other, `t` of the way: the leaving dot shrinks as
/// much as the arriving one grows, so the row never changes width and nothing beside it moves.
pub fn dot_widths(small: u32, big: u32, t: u32) -> (u32, u32) //@ret
//@ requires small <= big, big <= 4096, t <= UNIT,
//@ ensures ret.0 + ret.1 == small + big, small <= ret.0 <= big, small <= ret.1 <= big,
//@     t == 0 ==> ret.0 == big, t == UNIT ==> ret.1 == big,
{
    let grow = count_up(big - small, t);
    (big - grow, small + grow)
}

/// When item `index` of a staggered entrance starts, in ms: `step` apart, never later than `cap`, so a long list does not keep
/// the last items waiting.
pub fn stagger(index: u32, step: u32, cap: u32) -> u32 //@ret
//@ ensures ret <= cap, (index as int) * (step as int) <= cap ==> ret == index * step,
{
    //@ assert((index as u64) * (step as u64) <= 0xffff_ffff * 0xffff_ffff) by (nonlinear_arith) requires index <= 0xffff_ffffu32, step <= 0xffff_ffffu32;
    let d = index as u64 * step as u64;
    if d > cap as u64 { cap } else { d as u32 }
}

/// A timer's minutes and seconds, the minutes capped at 99 so the digits never grow.
pub fn mm_ss(secs: u32) -> (u32, u32) //@ret
//@ ensures ret.0 <= 99, ret.1 < 60, secs < 6000 ==> ret.0 * 60 + ret.1 == secs,
{
    let s = if secs > 5999 { 5999 } else { secs };
    (s / 60, s % 60)
}

/// A box in pixels (or hundredths of a dp): the panes of the laptop, tablet and DSi layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Box2 {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

//@ pub open spec fn inside(b: Box2, x: int, y: int, w: int, h: int) -> bool {
//@     x <= b.x && b.x + b.w <= x + w && y <= b.y && b.y + b.h <= y + h && b.w >= 0 && b.h >= 0
//@ }

/// The pane at column `col` and row `row` of a `cols` x `rows` grid over the box (`x`, `y`, `w`, `h`), `gap` apart, spanning
/// `cs` columns and `rs` rows: never outside the box, whatever the device. Each axis is [`cells`].
#[allow(clippy::too_many_arguments)] //@omit
pub fn grid_box(x: i32, y: i32, w: i32, h: i32, cols: u32, rows: u32, gap: i32, col: u32, row: u32, cs: u32, rs: u32) -> Box2 //@ret
//@ requires row_ok(x as int, w as int, cols as int, gap as int), row_ok(y as int, h as int, rows as int, gap as int),
//@     0 < cs, col + cs <= cols, 0 < rs, row + rs <= rows,
//@ ensures inside(ret, x as int, y as int, w as int, h as int),
{
    let a = cells(x, w, cols, gap, col, cs);
    let b = cells(y, h, rows, gap, row, rs);
    Box2 { x: a.start, y: b.start, w: a.len, h: b.len }
}

/// Whether the box `b` lies inside the box (`x`, `y`, `w`, `h`): the check every layout's tests run on every pane.
pub fn fits(b: Box2, x: i32, y: i32, w: i32, h: i32) -> bool //@ret
//@ ensures ret == inside(b, x as int, y as int, w as int, h as int),
{
    b.w >= 0 && b.h >= 0 && x as i64 <= b.x as i64 && b.x as i64 + b.w as i64 <= x as i64 + w as i64
        && y as i64 <= b.y as i64 && b.y as i64 + b.h as i64 <= y as i64 + h as i64
}
