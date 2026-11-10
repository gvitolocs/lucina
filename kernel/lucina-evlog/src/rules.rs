// The event log's rules that can be wrong in a way a test would miss: the hourly gate that folds repeated warnings and
// counts what it held back, and which segments retention drops. Verus checks this very file: `//@` lines hold the proof,
// see scripts/verify-shipped.sh (D-011); verification/juno-evlog/mutants.sh plants bugs that Verus must refuse. follow.rs
// and store.rs call these functions and hold no copy of the rules. The PMIC rules are in lucina-pon (kernel/lucina-pon/src/rules.rs).
//
// After Windows' live-dump throttling (one per component per window) for the gate, with the count of what was held back
// added; after Event Log's circular logs for retention, except that the newest segment is never dropped.
//
// Not proved: the stability index (floating point), the text, the file handling around retention.

/// One signature's gate: when it last let an event through, and how many it has held back since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Gate {
    pub last_ms: i64,
    pub held: u64,
}

/// What the gate decided: whether this one goes through, and if so how many like it were held back before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Passed {
    pub pass: bool,
    pub held_back: u64,
    pub gate: Gate,
}

/// One event through per `window_ms`: the first ever passes; within the window it is held and counted; the first after the
/// window passes and reports exactly how many were held. A clock that went back lets one through and starts over.
pub fn throttle(prev: Option<Gate>, now_ms: i64, window_ms: i64) -> Passed //@ret
//@ requires
//@     window_ms > 0, now_ms >= 0,
//@     match prev { Some(g) => g.last_ms >= 0 && g.held < u64::MAX, None => true },
//@ ensures
//@     ret.pass ==> ret.gate == (Gate { last_ms: now_ms, held: 0 }),
//@     match prev {
//@         None => ret.pass && ret.held_back == 0,
//@         Some(g) => if g.last_ms <= now_ms && now_ms - g.last_ms < window_ms {
//@             !ret.pass && ret.held_back == 0 && ret.gate == (Gate { last_ms: g.last_ms, held: (g.held + 1) as u64 })
//@         } else {
//@             ret.pass && ret.held_back == g.held
//@         },
//@     },
{
    match prev {
        None => Passed {
            pass: true,
            held_back: 0,
            gate: Gate {
                last_ms: now_ms,
                held: 0,
            },
        },
        Some(g) => {
            if g.last_ms <= now_ms && now_ms - g.last_ms < window_ms {
                Passed {
                    pass: false,
                    held_back: 0,
                    gate: Gate {
                        last_ms: g.last_ms,
                        held: g.held + 1,
                    },
                }
            } else {
                Passed {
                    pass: true,
                    held_back: g.held,
                    gate: Gate {
                        last_ms: now_ms,
                        held: 0,
                    },
                }
            }
        }
    }
}

//@ pub open spec fn sum_from(s: Seq<u64>, i: int) -> int
//@     decreases s.len() - i when 0 <= i
//@ { if i < 0 || i >= s.len() { 0 } else { s[i] + sum_from(s, i + 1) } }
//@
//@ pub proof fn sum_from_nonneg(s: Seq<u64>, i: int)
//@     requires 0 <= i,
//@     ensures sum_from(s, i) >= 0,
//@     decreases s.len() - i,
//@ { if i < s.len() { sum_from_nonneg(s, i + 1); } }

/// How many of a chain's oldest segments retention drops, given their sizes oldest first and their total: every one of the
/// `too_old` leading segments past their age, then the fewest more that bring the rest within `limit`, but never the newest.
pub fn segments_to_drop(sizes: &[u64], total: u64, limit: u64, too_old: usize) -> usize //@ret
//@ requires sizes.len() >= 1, total == sum_from(sizes@, 0), too_old <= sizes.len(),
//@ ensures
//@     ret < sizes.len(),
//@     ret >= too_old || ret == sizes.len() - 1,
//@     sum_from(sizes@, ret as int) <= limit || ret == sizes.len() - 1,
//@     ret > too_old ==> sum_from(sizes@, ret - 1) > limit,
{
    let mut n: usize = 0;
    let mut left = total;
    while n + 1 < sizes.len() && (n < too_old || left > limit)
    //@ invariant
    //@     n < sizes.len(), too_old <= sizes.len(), left == sum_from(sizes@, n as int),
    //@     n > too_old ==> sum_from(sizes@, n - 1) > limit,
    //@ decreases sizes.len() - n,
    {
        //@ proof { sum_from_nonneg(sizes@, n + 1); }
        left = left - sizes[n];
        n += 1;
    }
    n
}
