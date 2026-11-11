// The integer step of a swipe's release target: after the float code of `tracker::release_target` has rounded where the gesture
// would land, this clamps that point into the snap points the gesture may reach. Verus checks this very file: `//@` lines hold
// the proof, see scripts/verify-shipped.sh (D-011); verification/juno-wm-rules/mutants.sh plants bugs that Verus must refuse.
// tracker.rs calls this function and holds no copy of the rule.
//
// Proved, for every input in the `requires`: the result is the exact value `snap_target` below without long swipes (the rounded
// point kept in `start-1..=start+1`, then in `0..=last`), and the exact clamp of the rounded point into `0..=last` with them. From the
// exact value: the result is at most `last`, within one point of `start` when `start <= last`, `last` when `start > last`, monotone in
// `rounded` (`snap_target_monotone`, for `last >= 0`, which every `usize` is), and `start` itself when the rounded point is `start` (a release that did not move).
//
// What the system property ("the snap target is a valid space index, at most one step from the start") rests on and this file does
// NOT prove: `last + 1 == spaces.len()` (`controller.rs`: `last = spaces.len().saturating_sub(1)`), `start <= last`
// (`controller.rs`: `start = start.min(last)` before the release; with it the `start > last` clauses never apply), the shell passing
// `long_swipes == false` (`controller.rs`), and `tracker.rs` clamping `start` and `last` into `LIMIT` before the call. The index is
// clamped once more, unproved, by `Spaces::go_to`.

/// Highest snap index or start index the proof covers (far beyond any number of spaces; no cast to `i64` can overflow).
pub const LIMIT: usize = 1_073_741_824; // 2^30

// `v` kept inside `lo..=hi` (for `lo <= hi`).
//@ pub open spec fn clamp(v: int, lo: int, hi: int) -> int {
//@     if v < lo { lo } else if v > hi { hi } else { v }
//@ }

// The target without long swipes: the rounded point at most one point from `start`, then inside the row `0..=last`.
//@ pub open spec fn snap_target(rounded: int, start: int, last: int) -> int {
//@     clamp(clamp(rounded, start - 1, start + 1), 0, last)
//@ }

// A release further along never lands on an earlier point.
//@ pub proof fn snap_target_monotone(r1: int, r2: int, start: int, last: int)
//@     requires r1 <= r2, 0 <= last,
//@     ensures snap_target(r1, start, last) <= snap_target(r2, start, last),
//@ {
//@ }

/// The snap point a release lands on: `rounded` (the float target, rounded to a whole point) kept inside `0..=last`, and without
/// `long_swipes` also inside `start - 1 ..= start + 1`.
pub fn clamp_target(rounded: i64, start: usize, last: usize, long_swipes: bool) -> usize //@ret
//@ requires start <= LIMIT, last <= LIMIT,
//@ ensures
//@     ret <= last,
//@     !long_swipes && start <= last ==> start as int - 1 <= ret as int <= start as int + 1,
//@     !long_swipes && start > last ==> ret == last,
//@     long_swipes ==> ret as int == (if rounded < 0 { 0 } else if rounded > last as int { last as int } else { rounded as int }),
//@     !long_swipes ==> ret as int == snap_target(rounded as int, start as int, last as int),
//@     !long_swipes && start <= last && rounded == start as int ==> ret == start,
{
    let mut lo: i64 = 0;
    let mut hi: i64 = last as i64;
    if !long_swipes {
        let s = start as i64;
        if s - 1 > lo {
            lo = s - 1;
        }
        if s + 1 < hi {
            hi = s + 1;
        }
    }
    let clamped = if rounded < lo {
        lo
    } else if rounded > hi {
        hi
    } else {
        rounded
    };
    if clamped > last as i64 {
        last
    } else {
        clamped as usize
    }
}
