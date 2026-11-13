//! How long a request's thread sleeps on the controller's interrupt, and what it does when a sleep ends without one.
//!
//! The thread sleeps at most [`SLEEP_CAP_US`] at a time, then looks at the slot again. A request the controller finished
//! but whose interrupt never woke the thread is then found within about 20 ms instead of at the 30 s SCSI timeout, and it
//! is counted as a missed interrupt (`docs/design/juno-writeback.md`, step 3). Linux never needs this: blk-mq's timeout
//! aborts and logs the request. This driver cannot abort one (no task management), so looking again is the adapted
//! equivalent. The overall request timeout is unchanged: it is still decided by the request loop in `host`.

/// Longest single sleep on the controller's interrupt.
pub const SLEEP_CAP_US: u64 = 20_000;

/// Lines about missed interrupts the log gets per [`MISS_NOTE_WINDOW_US`]; later misses in the window are only counted.
pub const MISS_NOTES_PER_WINDOW: u32 = 10;
pub const MISS_NOTE_WINDOW_US: u64 = 1_000_000;

/// What a request does after a sleep on the interrupt ended at its timeout with no interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterSleep {
    /// The slot is still busy and there is time left: sleep again.
    WaitAgain,
    /// The slot is done although no interrupt came: complete the request and count a missed interrupt.
    CompleteMissed,
    /// The slot is still busy and the request's timeout has passed: the request times out as it always did.
    TimeOut,
}

/// The decision after a sleep that ended without the interrupt. `done`: the request's slot is finished (its door bell bit
/// reads clear and its descriptor holds a status); `elapsed_us`: time since the door bell was rung; `timeout_us`: the
/// request's timeout. A finished slot wins over the timeout, as in the request loop, which looks at the slot first.
pub fn after_sleep(done: bool, elapsed_us: u64, timeout_us: u64) -> AfterSleep {
    if done {
        AfterSleep::CompleteMissed
    } else if elapsed_us > timeout_us {
        AfterSleep::TimeOut
    } else {
        AfterSleep::WaitAgain
    }
}

/// The length of the next sleep: what is left of the request's timeout, at most [`SLEEP_CAP_US`], at least 1 us.
pub fn sleep_us(elapsed_us: u64, timeout_us: u64) -> u64 {
    timeout_us.saturating_sub(elapsed_us).clamp(1, SLEEP_CAP_US)
}

/// The length of the next sleep while the machine sleeps: what is left of the request's timeout, [`SLEEP_CAP_US`]
/// lifted, at least 1 us (patch 1342).
///
/// The cap exists to find a completion whose interrupt was lost; a sleeping machine takes no periodic look at
/// anything, and Linux needs none here because the host is suspended for the sleep (`ufshcd_system_suspend`,
/// `drivers/ufs/core/ufshcd.c:10091`) with no request waiting on it. So a request that is in flight across a sleep is
/// waited out on its interrupt or on its own timeout, which is the bound the wait had before patch 1243: a completion
/// whose interrupt is lost during a sleep costs the rest of the timeout, not 20 ms, and the CPU is not woken
/// [`SLEEP_CAP_US`] at a time through the sleep.
pub fn sleep_us_asleep(elapsed_us: u64, timeout_us: u64) -> u64 {
    timeout_us.saturating_sub(elapsed_us).max(1)
}

/// Rate limit of the log lines about missed interrupts: [`MISS_NOTES_PER_WINDOW`] lines per [`MISS_NOTE_WINDOW_US`], then
/// one "N more" line when the next window starts. The count of misses itself is never limited (it lives in the host).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MissNotes {
    window_start_us: u64,
    in_window: u32,
    suppressed: u64,
    started: bool,
}

/// What to log for one missed interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissNote {
    /// Misses not logged in the window that just ended: log "N more" first (0: none).
    pub more: u64,
    /// Log this miss's own line.
    pub line: bool,
}

impl MissNotes {
    /// A miss at `now_us`.
    pub fn miss(&mut self, now_us: u64) -> MissNote {
        let mut more = 0;
        if !self.started || now_us.wrapping_sub(self.window_start_us) >= MISS_NOTE_WINDOW_US {
            more = self.suppressed;
            *self = Self {
                window_start_us: now_us,
                in_window: 0,
                suppressed: 0,
                started: true,
            };
        }
        let line = self.in_window < MISS_NOTES_PER_WINDOW;
        if line {
            self.in_window += 1;
        } else {
            self.suppressed += 1;
        }
        MissNote { more, line }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finished_slot_completes_as_missed() {
        assert_eq!(
            after_sleep(true, 20_000, 30_000_000),
            AfterSleep::CompleteMissed
        );
        assert_eq!(after_sleep(true, 0, 30_000_000), AfterSleep::CompleteMissed);
    }

    #[test]
    fn a_finished_slot_wins_over_the_timeout() {
        assert_eq!(
            after_sleep(true, 30_000_001, 30_000_000),
            AfterSleep::CompleteMissed
        );
        assert_eq!(
            after_sleep(true, u64::MAX, 30_000_000),
            AfterSleep::CompleteMissed
        );
    }

    #[test]
    fn a_busy_slot_waits_again_until_the_timeout() {
        assert_eq!(
            after_sleep(false, 20_000, 30_000_000),
            AfterSleep::WaitAgain
        );
        assert_eq!(after_sleep(false, 0, 30_000_000), AfterSleep::WaitAgain);
        // The boundary is the request loop's own: past the timeout, not at it.
        assert_eq!(
            after_sleep(false, 30_000_000, 30_000_000),
            AfterSleep::WaitAgain
        );
        assert_eq!(
            after_sleep(false, 30_000_001, 30_000_000),
            AfterSleep::TimeOut
        );
        assert_eq!(
            after_sleep(false, u64::MAX, 30_000_000),
            AfterSleep::TimeOut
        );
    }

    #[test]
    fn sleeps_are_capped_at_20_ms() {
        assert_eq!(sleep_us(0, 30_000_000), SLEEP_CAP_US);
        assert_eq!(sleep_us(29_000_000, 30_000_000), SLEEP_CAP_US);
        assert_eq!(sleep_us(0, 60_000_000), SLEEP_CAP_US);
    }

    #[test]
    fn the_last_sleep_ends_at_the_timeout() {
        assert_eq!(sleep_us(29_995_000, 30_000_000), 5_000);
        assert_eq!(sleep_us(0, 7), 7);
    }

    #[test]
    fn a_sleep_is_never_zero() {
        assert_eq!(sleep_us(30_000_000, 30_000_000), 1);
        assert_eq!(sleep_us(u64::MAX, 30_000_000), 1);
        assert_eq!(sleep_us(0, 0), 1);
    }

    #[test]
    fn a_sleeping_machine_is_waited_out_past_the_cap() {
        // The 30 s timeout still bounds the wait; only the 20 ms look-again is lifted.
        assert_eq!(sleep_us_asleep(0, 30_000_000), 30_000_000);
        assert_eq!(sleep_us_asleep(29_000_000, 30_000_000), 1_000_000);
        assert_eq!(sleep_us_asleep(29_995_000, 30_000_000), 5_000);
    }

    #[test]
    fn an_asleep_sleep_is_never_zero() {
        assert_eq!(sleep_us_asleep(30_000_000, 30_000_000), 1);
        assert_eq!(sleep_us_asleep(u64::MAX, 30_000_000), 1);
        assert_eq!(sleep_us_asleep(0, 0), 1);
    }

    #[test]
    fn ten_lines_a_second_then_n_more() {
        let mut n = MissNotes::default();
        for i in 0..MISS_NOTES_PER_WINDOW as u64 {
            assert_eq!(
                n.miss(1_000 + i),
                MissNote {
                    more: 0,
                    line: true
                }
            );
        }
        for i in 0..5 {
            assert_eq!(
                n.miss(500_000 + i),
                MissNote {
                    more: 0,
                    line: false
                }
            );
        }
        // The next window says how many were not logged, and logs again.
        assert_eq!(
            n.miss(1_000 + MISS_NOTE_WINDOW_US),
            MissNote {
                more: 5,
                line: true
            }
        );
        assert_eq!(
            n.miss(1_001 + MISS_NOTE_WINDOW_US),
            MissNote {
                more: 0,
                line: true
            }
        );
    }

    #[test]
    fn the_first_miss_is_logged_at_any_time() {
        let mut n = MissNotes::default();
        assert_eq!(
            n.miss(0),
            MissNote {
                more: 0,
                line: true
            }
        );
        let mut n = MissNotes::default();
        assert_eq!(
            n.miss(u64::MAX),
            MissNote {
                more: 0,
                line: true
            }
        );
    }

    #[test]
    fn a_clock_that_wraps_starts_a_new_window() {
        let mut n = MissNotes::default();
        for _ in 0..MISS_NOTES_PER_WINDOW + 3 {
            n.miss(u64::MAX - 10);
        }
        assert_eq!(
            n.miss(MISS_NOTE_WINDOW_US),
            MissNote {
                more: 3,
                line: true
            }
        );
    }
}
