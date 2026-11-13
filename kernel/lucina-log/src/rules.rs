// The log library's rules that can be wrong in a way a test would miss: the per-source rate limiter's window arithmetic, when
// the level control file is due for a re-read, which bytes a field key may hold, and the byte budget left for a message.
// Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011);
// verification/juno-log/mutants.sh plants bugs that Verus must refuse. control.rs, logger.rs and encode.rs call these
// functions and hold no copy of the rules.
//
// After journald's RateLimitBurst/RateLimitIntervalSec for the limiter, with the count of what was held back reported once
// by the first record after the window.
//
// Not proved: the JSON text, the file and socket handling, the clock reads.

/// One source's rate-limit window: when it started, how many records it let through, and how many it held back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Window {
    pub start_ms: i64,
    pub admitted: u64,
    pub suppressed: u64,
}

/// What the limiter decided: whether this record goes out, how many records the window that just ended held back (to be
/// reported once, 0 when none), and the window to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)] //@omit
pub struct Admitted {
    pub admit: bool,
    pub report: u64,
    pub win: Window,
}

/// `burst` records per `window_ms`: the first ever starts a window; within the window records pass until `burst` have, then
/// they are held and counted; the first record after the window (or after the clock went back) starts a new window, passes,
/// and reports how many the old one held back (exactly, until the count saturates at u64::MAX held records).
pub fn admit(prev: Option<Window>, now_ms: i64, window_ms: i64, burst: u64) -> Admitted //@ret
//@ requires
//@     window_ms > 0, burst >= 1, now_ms >= 0,
//@     match prev { Some(w) => w.start_ms >= 0 && w.admitted <= burst, None => true },
//@ ensures
//@     // The window kept is one the next call may be given again (the requires above hold for it), never starts in the future,
//@     // and holds at least the record that opened it: burst is a hard ceiling on what one window lets through.
//@     ret.win.admitted <= burst,
//@     ret.win.start_ms >= 0,
//@     ret.win.start_ms <= now_ms,
//@     ret.win.admitted >= 1,
//@     // A count is reported only together with a record that goes out (the logger writes the report, then the record).
//@     ret.report > 0 ==> ret.admit,
//@     // Every call is counted exactly once: it goes out, or it is held and added to the count; a count leaves the window only
//@     // as the report, once, and the new window starts from 0. Summed over calls: reports + the count kept == calls held.
//@     // (The count saturates at u64::MAX, 2^64 held records; the equation is stated below that.)
//@     match prev {
//@         None => ret.report + ret.win.suppressed == 0,
//@         Some(w) => w.suppressed < u64::MAX
//@             ==> ret.report + ret.win.suppressed == w.suppressed + (if ret.admit { 0int } else { 1int }),
//@     },
//@     // The whole decision, case by case (this determines every field of the result).
//@     match prev {
//@         None => ret.admit && ret.report == 0
//@             && ret.win == (Window { start_ms: now_ms, admitted: 1, suppressed: 0 }),
//@         Some(w) => if w.start_ms <= now_ms && now_ms - w.start_ms < window_ms {
//@             ret.report == 0 && ret.win.start_ms == w.start_ms && (if w.admitted < burst {
//@                 ret.admit && ret.win.admitted == (w.admitted + 1) as u64 && ret.win.suppressed == w.suppressed
//@             } else {
//@                 !ret.admit && ret.win.admitted == w.admitted
//@                     && ret.win.suppressed == (if w.suppressed < u64::MAX { (w.suppressed + 1) as u64 } else { w.suppressed })
//@             })
//@         } else {
//@             ret.admit && ret.report == w.suppressed
//@                 && ret.win == (Window { start_ms: now_ms, admitted: 1, suppressed: 0 })
//@         },
//@     },
{
    match prev {
        None => Admitted {
            admit: true,
            report: 0,
            win: Window {
                start_ms: now_ms,
                admitted: 1,
                suppressed: 0,
            },
        },
        Some(w) => {
            if w.start_ms <= now_ms && now_ms - w.start_ms < window_ms {
                if w.admitted < burst {
                    Admitted {
                        admit: true,
                        report: 0,
                        win: Window {
                            start_ms: w.start_ms,
                            admitted: w.admitted + 1,
                            suppressed: w.suppressed,
                        },
                    }
                } else {
                    Admitted {
                        admit: false,
                        report: 0,
                        win: Window {
                            start_ms: w.start_ms,
                            admitted: w.admitted,
                            suppressed: if w.suppressed < u64::MAX {
                                w.suppressed + 1
                            } else {
                                w.suppressed
                            },
                        },
                    }
                }
            } else {
                Admitted {
                    admit: true,
                    report: w.suppressed,
                    win: Window {
                        start_ms: now_ms,
                        admitted: 1,
                        suppressed: 0,
                    },
                }
            }
        }
    }
}

/// Whether the level control file is due for a look: the first time always; after that once `interval_ms` has passed since
/// the last look. A clock that went back is due too, so a stale `last_ms` can never hold the check off.
pub fn recheck_due(last_ms: Option<i64>, now_ms: i64, interval_ms: i64) -> bool //@ret
//@ requires
//@     interval_ms > 0, now_ms >= 0,
//@     match last_ms { Some(l) => l >= 0, None => true },
//@ ensures
//@     ret == match last_ms { None => true, Some(l) => now_ms < l || now_ms - l >= interval_ms },
{
    match last_ms {
        None => true,
        Some(l) => now_ms < l || now_ms - l >= interval_ms,
    }
}

/// Whether a byte may be in a field key: `a-z`, `0-9`, `_` and `.` (docs/design/juno-logging.md section 2).
#[allow(clippy::manual_range_contains)] //@omit
pub fn is_key_byte(b: u8) -> bool //@ret
//@ ensures ret == ((97 <= b && b <= 122) || (48 <= b && b <= 57) || b == 95 || b == 46),
{
    (b >= 97 && b <= 122) || (b >= 48 && b <= 57) || b == 95 || b == 46
}

/// The bytes still free in a record of at most `max` bytes after `used` are taken; 0 when it is already full or over.
#[allow(clippy::implicit_saturating_sub)] //@omit
pub fn budget(max: usize, used: usize) -> usize //@ret
//@ ensures
//@     used <= max ==> ret == max - used,
//@     used > max ==> ret == 0,
//@     ret <= max,
{
    if used < max { max - used } else { 0 }
}
