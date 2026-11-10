// SPDX-License-Identifier: MPL-2.0

//! The idle governor: which of a CPU's idle states to enter, and whether to stop its tick.
//!
//! Behaviour of Linux's timer-events-oriented governor (`drivers/cpuidle/governors/teo.c`, 7.1), reimplemented: the facts
//! below are its rules, the code is ours. Each state `i` owns a bin of idle durations, from its target residency up to the
//! next state's. Every wake-up adds a pulse to one bin, as a *hit* (the CPU woke about when its next timer said) or an
//! *intercept* (something else woke it earlier), and every metric decays by 1/8 at each wake-up. To choose, the governor
//! starts from the deepest enabled state; if intercepts in shallower bins outweigh what lies at and above the candidate, it
//! steps down to where they concentrate; a latency limit caps the depth; then the time to the next timer (the sleep length)
//! caps the target residency. The tick is kept when the expected idle period is shorter than a tick.
//!
//! All durations are nanoseconds as `i64`, with [`KTIME_MAX`] for "no timer", as the original keeps them; metrics are `u32`
//! with wrapping arithmetic, as its `unsigned int` (they stay below 2^16 in practice: each decays by 1/8 per update).

/// The most idle states a CPU may have (Linux's `CPUIDLE_STATE_MAX` is 10; Juno's CPUs have 3, the cluster's 3).
pub const MAX_STATES: usize = 10;

/// "No timer": the sleep length when nothing is armed.
pub const KTIME_MAX: i64 = i64::MAX;

/// A pulse added to a metric when it grows.
const PULSE: u32 = 1024;
/// Each update takes 1/2^DECAY_SHIFT off every metric.
const DECAY_SHIFT: u32 = 3;
/// Idle periods shorter than this count as short (the original's `RESIDENCY_THRESHOLD_NS`, 15 µs).
pub const RESIDENCY_THRESHOLD_NS: i64 = 15_000;
/// Below this latency limit the sleep length is not worth asking for (`LATENCY_THRESHOLD_NS`, half the above).
const LATENCY_THRESHOLD_NS: i64 = RESIDENCY_THRESHOLD_NS / 2;

/// What the governor needs to know about one idle state (defined with the verified checks, which reason about it).
pub use super::admit::State;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Bin {
    intercepts: u32,
    hits: u32,
}

/// What [`Teo::select`] needs from the timer code at the moment of choosing.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    /// The scheduler tick's period (`TICK_NSEC`).
    pub tick_ns: i64,
    /// Whether the tick is already stopped on this CPU.
    pub tick_stopped: bool,
    /// The deepest exit latency allowed (the CPU latency QoS limit); `KTIME_MAX` for none.
    pub latency_req_ns: i64,
}

/// The time until this CPU's next timer event if its tick stops, and until its next tick: the original's
/// `tick_nohz_get_sleep_length(&delta_tick)`. Asked for only when it matters, because it costs a look at the timers.
pub trait SleepLength {
    /// `(sleep_length_ns, delta_tick_ns)`.
    fn sleep_length(&mut self) -> (i64, i64);
}

impl<F: FnMut() -> (i64, i64)> SleepLength for F {
    fn sleep_length(&mut self) -> (i64, i64) {
        self()
    }
}

/// The choice: a state index and whether the tick may stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub state: usize,
    pub stop_tick: bool,
}

/// One CPU's governor data (`struct teo_cpu`, plus the device's `last_state_idx`).
#[derive(Clone, Debug)]
pub struct Teo {
    sleep_length_ns: i64,
    bins: [Bin; MAX_STATES],
    total: u32,
    total_tick: u32,
    tick_intercepts: u32,
    short_idles: u32,
    tick_wakeup: bool,
    /// The state entered last time, until its wake-up has been accounted.
    last_state: Option<usize>,
}

impl Default for Teo {
    fn default() -> Self {
        Self::new()
    }
}

fn decay(metric: &mut u32) {
    let delta = *metric >> DECAY_SHIFT;
    *metric = if delta != 0 { *metric - delta } else { 0 };
}

impl Teo {
    /// The sleep length the last [`Teo::select`] asked for, or [`KTIME_MAX`] if it chose without asking (the caller then
    /// asks itself before checking a state against the next timer).
    pub fn sleep_length_ns(&self) -> i64 {
        self.sleep_length_ns
    }

    /// A CPU's data when the governor starts on it: all zero (`teo_enable_device`).
    pub const fn new() -> Self {
        Self {
            sleep_length_ns: 0,
            bins: [Bin { intercepts: 0, hits: 0 }; MAX_STATES],
            total: 0,
            total_tick: 0,
            tick_intercepts: 0,
            short_idles: 0,
            tick_wakeup: false,
            last_state: None,
        }
    }

    /// After a wake-up: `state` is what was entered (`None` if the entry failed) and `tick_wakeup` whether the tick woke the CPU
    /// (`teo_reflect`). The time spent goes in at the next [`Teo::select`].
    pub fn reflect(&mut self, state: Option<usize>, tick_wakeup: bool) {
        self.tick_wakeup = tick_wakeup;
        self.last_state = state;
    }

    /// Accounts the last wake-up, `last_residency_ns` after entering `states[last]` (`teo_update`).
    fn update(&mut self, states: &[State], last: usize, last_residency_ns: i64, tick_ns: i64) {
        let n = states.len().min(MAX_STATES);
        let lat_ns = states[last].exit_latency_ns;
        decay(&mut self.short_idles);

        // The time from the wake-up to the first instruction is rarely the worst case: take off half the exit latency.
        let mut measured_ns = last_residency_ns;
        if measured_ns >= lat_ns {
            measured_ns -= lat_ns / 2;
            if measured_ns < RESIDENCY_THRESHOLD_NS {
                self.short_idles = self.short_idles.wrapping_add(PULSE);
            }
        } else {
            measured_ns /= 2;
            self.short_idles = self.short_idles.wrapping_add(PULSE);
        }

        // Decay every bin; find the bins the sleep length and the measured duration fall in.
        let mut total: u32 = 0;
        let mut idx_timer = 0;
        let mut idx_duration = 0;
        for i in 0..n {
            let bin = &mut self.bins[i];
            decay(&mut bin.hits);
            total = total.wrapping_add(bin.hits);
            decay(&mut bin.intercepts);
            total = total.wrapping_add(bin.intercepts);
            let residency = states[i].target_residency_ns;
            if residency <= self.sleep_length_ns {
                idx_timer = i;
                if residency <= measured_ns {
                    idx_duration = i;
                }
            }
        }
        self.total = total.wrapping_add(PULSE);

        decay(&mut self.tick_intercepts);
        decay(&mut self.total_tick);
        if self.tick_wakeup {
            self.total_tick = self.total_tick.wrapping_add(PULSE);
            // Tick wake-ups dominate: count a hit on the deepest state, which makes stopping the tick likelier.
            if 3u32.wrapping_mul(self.total_tick) > 2u32.wrapping_mul(self.total) {
                self.bins[n - 1].hits = self.bins[n - 1].hits.wrapping_add(PULSE);
                return;
            }
            // Few intercepts within a tick: the tick probably woke the CPU because an expected intercept did not come.
            if 3u32.wrapping_mul(self.tick_intercepts) < 2u32.wrapping_mul(total) {
                self.bins[idx_timer].hits = self.bins[idx_timer].hits.wrapping_add(PULSE);
                return;
            }
        }

        // A hit: the measured duration is in the sleep length's bin and the wake came about when the timer was due.
        if idx_timer == idx_duration && self.sleep_length_ns.saturating_sub(measured_ns) < lat_ns / 2 {
            self.bins[idx_timer].hits = self.bins[idx_timer].hits.wrapping_add(PULSE);
        } else {
            self.bins[idx_duration].intercepts = self.bins[idx_duration].intercepts.wrapping_add(PULSE);
            if measured_ns <= tick_ns {
                self.tick_intercepts = self.tick_intercepts.wrapping_add(PULSE);
            }
        }
    }

    /// The deepest enabled state shallower than `cap` whose residency fits `duration_ns`, else the shallowest enabled one
    /// below `cap`, else `cap` (`teo_find_shallower_state`).
    fn shallower(states: &[State], disabled: &[bool], cap: usize, duration_ns: i64) -> usize {
        let mut idx = cap;
        let mut i = cap;
        while i > 0 {
            i -= 1;
            if disabled[i] {
                continue;
            }
            idx = i;
            if states[i].target_residency_ns <= duration_ns {
                break;
            }
        }
        idx
    }

    /// Chooses the state to enter (`teo_select`). `last_residency_ns` is how long the CPU stayed in the state it entered last
    /// (cpuidle's `last_residency_ns`); it is used once, at the first call after a [`Teo::reflect`] with a state.
    ///
    /// `states` and `disabled` have the same length, at least 1 and at most [`MAX_STATES`]; state 0 is the shallowest.
    pub fn select(
        &mut self,
        states: &[State],
        disabled: &[bool],
        clock: Clock,
        last_residency_ns: i64,
        sleep: &mut impl SleepLength,
    ) -> Choice {
        let n = states.len().min(disabled.len()).min(MAX_STATES);
        assert!(n > 0, "an idle driver has at least one state");
        let states = &states[..n];
        let disabled = &disabled[..n];

        if let Some(last) = self.last_state.take() {
            if last < n {
                self.update(states, last, last_residency_ns, clock.tick_ns);
            }
        }

        // Unknown until asked: a wake-up then counts as an intercept, which is fine unless timers dominate.
        self.sleep_length_ns = KTIME_MAX;

        let mut idx: Option<usize> = if disabled[0] { None } else { Some(0) };
        let mut idx0 = 0;
        let mut constraint_idx = 0;
        let mut hit_sum: u32 = 0;
        let mut intercept_sum: u32 = 0;
        let mut intercept_max: u32 = 0;
        let mut intercept_max_idx: Option<usize> = None;
        let mut idx_intercept_sum: u32 = 0;
        let mut idx_hit_sum: u32 = 0;
        for i in 1..n {
            let prev = self.bins[i - 1];
            hit_sum = hit_sum.wrapping_add(prev.hits);
            intercept_sum = intercept_sum.wrapping_add(prev.intercepts);
            // The bin with the most intercepts so far, the deepest of equals.
            if prev.intercepts >= intercept_max {
                intercept_max = prev.intercepts;
                intercept_max_idx = Some(i - 1);
            }
            if disabled[i] {
                continue;
            }
            if idx.is_none() {
                idx0 = i;
            }
            idx = Some(i);
            if states[i].exit_latency_ns <= clock.latency_req_ns {
                constraint_idx = i;
            }
            idx_intercept_sum = intercept_sum;
            idx_hit_sum = hit_sum;
        }

        let Some(mut idx) = idx else {
            // Nothing enabled: state 0 anyway, with the tick.
            return Choice { state: 0, stop_tick: false };
        };

        let duration_ns;
        let mut delta_tick = clock.tick_ns / 2;
        'end: {
            if idx == idx0 {
                // One enabled state: use it, and keep the tick if it is shallow.
                duration_ns = states[idx].target_residency_ns;
                break 'end;
            }

            // Intercepts below the candidate outweigh everything at and above it: look shallower.
            if 2u32.wrapping_mul(idx_intercept_sum) > self.total.wrapping_sub(idx_hit_sum) {
                let mut sum: u32 = 0;
                let mut i = idx;
                while i > idx0 {
                    i -= 1;
                    sum = sum.wrapping_add(self.bins[i].intercepts);
                    if disabled[i] {
                        continue;
                    }
                    idx = i;
                    if 2u32.wrapping_mul(sum) > idx_intercept_sum && intercept_max_idx.is_some_and(|m| i <= m) {
                        break;
                    }
                }
            }

            if idx > constraint_idx {
                idx = constraint_idx;
            }

            // A shallow candidate with mostly short idles, or a tight latency limit: the sleep length would not change the
            // answer, so skip asking for it and keep the tick.
            if !clock.tick_stopped
                && (idx == 0 || states[idx].target_residency_ns < RESIDENCY_THRESHOLD_NS)
                && (2u32.wrapping_mul(self.short_idles) >= self.total || clock.latency_req_ns < LATENCY_THRESHOLD_NS)
            {
                return Choice { state: idx, stop_tick: false };
            }

            let (length, next_tick) = sleep.sleep_length();
            delta_tick = next_tick;
            self.sleep_length_ns = length;

            // The tick is stopped and the next timer is far: do not get stuck in a shallow state.
            if clock.tick_stopped
                && length > 2 * clock.tick_ns
                && states[idx].target_residency_ns < clock.tick_ns
            {
                let mut i = constraint_idx;
                while i > idx {
                    if !disabled[i] && states[i].target_residency_ns <= length {
                        idx = i;
                        break;
                    }
                    i -= 1;
                }
                return Choice { state: idx, stop_tick: true };
            }

            if idx == 0 {
                return Choice { state: 0, stop_tick: false };
            }

            // The next timer comes before the candidate's residency: go shallower.
            if states[idx].target_residency_ns > length {
                idx = Self::shallower(states, disabled, idx, length);
            }

            // A state shorter than a tick, and intercepts within a tick are most wake-ups: keep the tick.
            duration_ns = if states[idx].target_residency_ns < clock.tick_ns
                && 3u32.wrapping_mul(self.tick_intercepts) >= 2u32.wrapping_mul(self.total)
            {
                clock.tick_ns / 2
            } else {
                length
            };
        }

        // The tick may stop unless the expected idle period is shorter than a tick.
        if duration_ns >= clock.tick_ns || clock.tick_stopped {
            return Choice { state: idx, stop_tick: true };
        }
        // The tick stays, so the state must fit before it.
        if idx > idx0 && states[idx].target_residency_ns > delta_tick {
            idx = Self::shallower(states, disabled, idx, delta_tick);
        }
        Choice { state: idx, stop_tick: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: i64 = 4_000_000; // HZ 250
    /// Juno's Silver CPUs, from the laptop's device tree as cpuidle reads it (residency = `min-residency-us`, latency =
    /// entry + exit): WFI (cpuidle-psci's 1 µs / 1 µs), `cpu-sleep-0-0` (1774 µs / 549 + 901 µs) and `cpu-sleep-0-1`
    /// (4001 µs / 702 + 915 µs).
    const SILVER: [State; 3] = [
        State { target_residency_ns: 1_000, exit_latency_ns: 1_000 },
        State { target_residency_ns: 1_774_000, exit_latency_ns: 1_450_000 },
        State { target_residency_ns: 4_001_000, exit_latency_ns: 1_617_000 },
    ];
    const ON: [bool; 3] = [false; 3];
    const FREE: Clock = Clock { tick_ns: TICK, tick_stopped: false, latency_req_ns: KTIME_MAX };

    fn sleep(len: i64) -> impl FnMut() -> (i64, i64) {
        move || (len, TICK)
    }

    #[test]
    fn a_fresh_cpu_with_a_far_timer_goes_deepest_and_stops_the_tick() {
        let mut t = Teo::new();
        let c = t.select(&SILVER, &ON, FREE, 0, &mut sleep(100_000_000));
        assert_eq!(c, Choice { state: 2, stop_tick: true });
    }

    #[test]
    fn a_near_timer_caps_the_depth() {
        let mut t = Teo::new();
        assert_eq!(t.select(&SILVER, &ON, FREE, 0, &mut sleep(2_000_000)).state, 1);
        let mut t = Teo::new();
        // 1 ms: only WFI fits, and the tick stays (the expected idle is below a tick).
        assert_eq!(t.select(&SILVER, &ON, FREE, 0, &mut sleep(1_000_000)), Choice { state: 0, stop_tick: false });
    }

    #[test]
    fn the_latency_limit_caps_the_depth() {
        let mut t = Teo::new();
        let clock = Clock { latency_req_ns: 1_500_000, ..FREE };
        assert_eq!(t.select(&SILVER, &ON, clock, 0, &mut sleep(100_000_000)).state, 1);
        let clock = Clock { latency_req_ns: 1_449_999, ..FREE };
        assert_eq!(t.select(&SILVER, &ON, clock, 0, &mut sleep(100_000_000)).state, 0);
    }

    #[test]
    fn disabled_states_are_never_chosen() {
        let mut t = Teo::new();
        let off = [false, false, true];
        assert_eq!(t.select(&SILVER, &off, FREE, 0, &mut sleep(100_000_000)).state, 1);
        let only_wfi = [false, true, true];
        assert_eq!(t.select(&SILVER, &only_wfi, FREE, 0, &mut sleep(100_000_000)).state, 0);
    }

    #[test]
    fn repeated_early_wakeups_pull_the_choice_shallower() {
        let mut t = Teo::new();
        // The timer says 100 ms, but something wakes the CPU after 50 µs every time.
        let mut last = 0;
        let mut chosen = 2;
        for _ in 0..40 {
            let c = t.select(&SILVER, &ON, FREE, last, &mut sleep(100_000_000));
            chosen = c.state;
            t.reflect(Some(c.state), false);
            last = 50_000 + SILVER[c.state].exit_latency_ns;
        }
        assert_eq!(chosen, 0, "intercepts in WFI's bin win");
    }

    #[test]
    fn timer_wakeups_keep_the_choice_deep() {
        let mut t = Teo::new();
        let mut last = 0;
        for _ in 0..40 {
            let c = t.select(&SILVER, &ON, FREE, last, &mut sleep(20_000_000));
            assert_eq!(c.state, 2);
            t.reflect(Some(c.state), false);
            last = 20_000_000;
        }
    }

    #[test]
    fn a_failed_entry_is_not_accounted() {
        let mut t = Teo::new();
        t.select(&SILVER, &ON, FREE, 0, &mut sleep(100_000_000));
        t.reflect(None, false);
        let before = t.clone();
        t.select(&SILVER, &ON, FREE, 12345, &mut sleep(100_000_000));
        assert_eq!(t.bins, before.bins);
        assert_eq!(t.total, before.total);
    }

    #[test]
    fn nothing_enabled_falls_back_to_state_0() {
        let mut t = Teo::new();
        assert_eq!(t.select(&SILVER, &[true; 3], FREE, 0, &mut sleep(1)), Choice { state: 0, stop_tick: false });
    }

    #[test]
    fn decay_reaches_zero() {
        let mut m = 7;
        decay(&mut m);
        assert_eq!(m, 0);
        let mut m = 1024;
        decay(&mut m);
        assert_eq!(m, 896);
    }
}
