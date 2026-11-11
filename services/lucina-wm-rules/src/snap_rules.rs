// The integer step of a swipe's release target: after the float code of `tracker::release_target` has rounded where the gesture
// would land, this clamps that point into the snap points the gesture may reach. Verus checks this very file: `//@` lines hold
// the proof, see scripts/verify-shipped.sh (D-011); verification/juno-wm-rules/mutants.sh plants bugs that Verus must refuse.
// tracker.rs calls this function and holds no copy of the rule. Proved without weakening: the bound by `last`, the window
// `start-1..=start+1` without long swipes (for `start <= last`; a `start` past `last` gives `last`), and the exact clamp into
// `0..=last` with long swipes.

/// Highest snap index or start index the proof covers (far beyond any number of spaces; no cast to `i64` can overflow).
pub const LIMIT: usize = 1_073_741_824; // 2^30

/// The snap point a release lands on: `rounded` (the float target, rounded to a whole point) kept inside `0..=last`, and without
/// `long_swipes` also inside `start - 1 ..= start + 1`.
pub fn clamp_target(rounded: i64, start: usize, last: usize, long_swipes: bool) -> usize //@ret
//@ requires start <= LIMIT, last <= LIMIT,
//@ ensures
//@     ret <= last,
//@     !long_swipes && start <= last ==> start as int - 1 <= ret as int <= start as int + 1,
//@     !long_swipes && start > last ==> ret == last,
//@     long_swipes ==> ret as int == (if rounded < 0 { 0 } else if rounded > last as int { last as int } else { rounded as int }),
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
