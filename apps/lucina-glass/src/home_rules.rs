// The integer rules under Juno's home (src/home): how cards stack in a column when some are left out, when the next frame is due,
// which item an arrow key moves the focus to, and how the notification pager and stack wrap. Verus checks this very file, as it does
// src/rules.rs: `//@` lines hold the proof (scripts/verify-shipped.sh, D-011); verification/juno-glass-home/mutants.sh plants bugs
// that Verus must refuse.
//
// Times are milliseconds on the monotonic clock. Lengths are hundredths of a dp, as in src/rules.rs.

/// The largest time the frame rule takes, in ms: far beyond any uptime, small enough that no sum overflows.
pub const MAX_MS: u64 = 4_503_599_627_370_496;

/// The longest column the stacking rule takes, in hundredths of a dp.
pub const MAX_COL: i32 = 1_048_576;

/// Where one card of a column goes: from `start`, and where the next one may start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Slot {
    pub fits: bool,
    pub start: i32,
    pub next: i32,
}

/// Place a card `h` tall at `cursor` in a column that ends at `bottom`, `gap` before the next card. A card that does not fit is
/// left out (`fits` false) and the cursor stays where it was, so the cards after it can still use the room. A card that fits lies
/// wholly between the cursor and the bottom, and the next one starts after it: cards never overlap and never leave the column.
pub fn stack(cursor: i32, bottom: i32, h: i32, gap: i32) -> Slot //@ret
//@ requires -MAX_COL <= cursor <= MAX_COL, -MAX_COL <= bottom <= MAX_COL, 0 <= h <= MAX_COL, 0 <= gap <= MAX_COL,
//@ ensures ret.fits ==> (ret.start == cursor && ret.start + h <= bottom && ret.next == ret.start + h + gap),
//@     !ret.fits ==> ret.next == cursor,
//@     ret.next >= cursor,
//@     (cursor + h <= bottom) ==> ret.fits,
{
    if cursor as i64 + h as i64 <= bottom as i64 {
        Slot { fits: true, start: cursor, next: cursor + h + gap }
    } else {
        Slot { fits: false, start: cursor, next: cursor }
    }
}

/// When the next frame is due, in ms, or 0 for "none: sleep until something happens". It is the one rule that decides whether the
/// home screen keeps drawing.
/// - `last` is when the last frame was shown and `period` the panel's frame time: never two frames closer than that.
/// - `moving_until`: the end of the last animation started (0 if none). While it lies after the last frame, one more frame is due, so
///   every animation is drawn at its very end and comes to rest exactly.
/// - `ambient` (0 = none) is the slower rate of what moves forever (the live pill, a spinner), and only until `ambient_until`, when
///   nobody has touched the machine for a while: then it rests and the CPU sleeps.
pub fn next_frame(now: u64, last: u64, period: u64, moving_until: u64, ambient: u64, ambient_until: u64) -> u64 //@ret
//@ requires now <= MAX_MS, last <= now, 0 < period <= MAX_MS, moving_until <= MAX_MS, ambient <= MAX_MS, ambient_until <= MAX_MS,
//@ ensures
//@     ret != 0 ==> (ret >= now && ret >= last + period),
//@     moving_until > last ==> (ret != 0 && ret <= if last + period > now { last + period } else { now as int }),
//@     (moving_until <= last && (ambient == 0 || ambient_until <= now)) ==> ret == 0,
//@     (moving_until <= last && ret != 0) ==> ret >= last + ambient,
{
    if moving_until > last {
        let due = last + period;
        return if due > now { due } else { now };
    }
    if ambient == 0 || ambient_until <= now {
        return 0;
    }
    let step = if ambient > period { ambient } else { period };
    let due = last + step;
    if due > now { due } else { now }
}

/// The four arrow keys.
pub const LEFT: u8 = 0;
pub const RIGHT: u8 = 1;
pub const UP: u8 = 2;
pub const DOWN: u8 = 3;

/// How far a candidate's centre (`tx`, `ty`) is from the focused one's (`fx`, `fy`) for the arrow `dir`, or 0 when it does not lie
/// that way at all. Lower is nearer. The distance along the arrow counts once; the distance across it counts four times, so the
/// focus prefers what is straight ahead over what is closer but off to the side. Coordinates are hundredths of a dp. The score is
/// proved exactly, so the arrow cannot weigh the wrong axis.
pub fn arrow_score(dir: u8, fx: i32, fy: i32, tx: i32, ty: i32) -> u64 //@ret
//@ requires -MAX_COL <= fx <= MAX_COL, -MAX_COL <= fy <= MAX_COL, -MAX_COL <= tx <= MAX_COL, -MAX_COL <= ty <= MAX_COL,
//@ ensures
//@     ret != 0 ==> ((dir == LEFT ==> tx < fx) && (dir == RIGHT ==> tx > fx) && (dir == UP ==> ty < fy) && (dir == DOWN ==> ty > fy)),
//@     dir > DOWN ==> ret == 0,
//@     (dir == LEFT && tx < fx) ==> ret != 0,
//@     (dir == RIGHT && tx > fx) ==> ret != 0,
//@     (dir == UP && ty < fy) ==> ret != 0,
//@     (dir == DOWN && ty > fy) ==> ret != 0,
//@     (dir == LEFT && tx < fx) ==> ret as int == (fx - tx) + 4 * (if ty >= fy { ty - fy } else { fy - ty }),
//@     (dir == RIGHT && tx > fx) ==> ret as int == (tx - fx) + 4 * (if ty >= fy { ty - fy } else { fy - ty }),
//@     (dir == UP && ty < fy) ==> ret as int == (fy - ty) + 4 * (if tx >= fx { tx - fx } else { fx - tx }),
//@     (dir == DOWN && ty > fy) ==> ret as int == (ty - fy) + 4 * (if tx >= fx { tx - fx } else { fx - tx }),
{
    let dx = tx as i64 - fx as i64;
    let dy = ty as i64 - fy as i64;
    let (along, across) = if dir == LEFT {
        (-dx, dy)
    } else if dir == RIGHT {
        (dx, dy)
    } else if dir == UP {
        (-dy, dx)
    } else if dir == DOWN {
        (dy, dx)
    } else {
        return 0;
    };
    if along <= 0 {
        return 0;
    }
    let side = if across < 0 { -across } else { across };
    (along as u64) + 4 * (side as u64)
}

/// Item `i` moved one step back (`delta` -1), not at all (0) or one step on (1) in a ring of `n` (the notification pager, the card
/// stack): always a valid index, wrapping from the last to the first and back.
pub fn wrap(i: u32, n: u32, delta: i32) -> u32 //@ret
//@ requires n > 0, i < n, -1 <= delta <= 1,
//@ ensures ret < n, delta == 0 ==> ret == i,
//@     delta == 1 ==> ret == (if i + 1 == n { 0 } else { i + 1 }),
//@     delta == -1 ==> ret == (if i == 0 { n - 1 } else { i - 1 }),
{
    if delta > 0 {
        if i + 1 == n { 0 } else { i + 1 }
    } else if delta < 0 {
        if i == 0 { n - 1 } else { i - 1 }
    } else {
        i
    }
}

/// A day's booked share of the working day, in whole per cent (the summary tile's number): `busy` of `window` minutes, at most 100.
pub fn booked(busy: u32, window: u32) -> u32 //@ret
//@ ensures ret <= 100, window == 0 ==> ret == 0, (window > 0 && busy >= window) ==> ret == 100,
{
    if window == 0 {
        return 0;
    }
    if busy >= window {
        return 100;
    }
    //@ assert((busy as u64) * 100 < (window as u64) * 100) by (nonlinear_arith) requires busy < window;
    let p = (busy as u64) * 100 / (window as u64);
    //@ assert(p < 100) by (nonlinear_arith) requires p as int == (busy as int) * 100 / (window as int), busy < window, window > 0;
    p as u32
}

/// The x offset, in pixels, of a page sliding in from one side over `width` pixels, `t` (UNITs of 65536) of the way: exactly the
/// whole width at the start and 0 at the end, never outside, so the move lands on the pixel it started from.
pub fn slide(width: u32, t: u32) -> u32 //@ret
//@ requires width <= 65536, t <= 65536,
//@ ensures ret <= width, t == 0 ==> ret == width, t == 65536 ==> ret == 0,
{
    //@ assert((width as u64) * (t as u64) <= (width as u64) * 65536) by (nonlinear_arith) requires t <= 65536;
    //@ proof { vstd::arithmetic::div_mod::lemma_div_is_ordered((width as int) * (t as int), (width as int) * 65536, 65536); }
    //@ proof { vstd::arithmetic::div_mod::lemma_div_by_multiple(width as int, 65536); }
    let gone = (width as u64) * (t as u64) / 65536;
    //@ assert(gone <= width);
    //@ assert(t == 0 ==> gone == 0) by (nonlinear_arith) requires gone as int == (width as int) * (t as int) / 65536;
    //@ assert(t == 65536 ==> gone == width) by (nonlinear_arith) requires gone as int == (width as int) * (t as int) / 65536;
    width - gone as u32
}
