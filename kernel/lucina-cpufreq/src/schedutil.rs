// SPDX-License-Identifier: MPL-2.0

//! Linux's schedutil, as plain integer logic with no clock behind it (patch 1235): PELT utilisation per CPU, the next
//! frequency of a domain, the hold, the rate limit and the suppression of requests that change nothing.
//!
//! Behaviour follows Linux v6.12 (`adc218676eef`): `kernel/sched/pelt.c`, `kernel/sched/sched-pelt.h`,
//! `kernel/sched/cpufreq_schedutil.c`, `include/linux/sched/cpufreq.h` and `drivers/cpufreq/cpufreq.c` (facts and
//! arithmetic, no code). Nothing here is proved; host tests only. The older governor (`CpuLoad`, `wanted_khz`) stays in
//! `rules.rs` for `lucina.cpufreq.gov=old`.
//!
//! # Calling it (for the kernel)
//!
//! One [`Pelt`] per CPU and one [`Domain`] per clock domain. On each tick of a CPU (and whenever the scheduler would call
//! `cpufreq_update_util`; a tick is the only trigger the kernel has today):
//!
//! 1. `pelt[cpu].update(now_us, busy_us)`: `now_us` is a monotonic clock in microseconds, `busy_us` the time this CPU was
//!    not idle since its previous `update` (at most `now_us - previous`; more is cut to that). A CPU that stopped its tick
//!    just calls `update` when it wakes, with the one sleep in the gap: the idle time decays the sum as Linux does.
//! 2. `domain.update(&steps, cap, now_us, max_of(pelt[cpu].util_avg() of the domain's CPUs), || hold[cpu].held(idle_calls))`.
//!    It returns `Some(position)` when the writer should be asked for that row of `steps`, `None` when nothing changes
//!    (rate limit, same row, held). `idle_calls` is the number of times this CPU has entered idle (Linux
//!    `tick_nohz_get_idle_calls_cpu`), counting entries that did not stop the tick.
//! 3. When the row really is written (the writer ran, or the limit changed), `domain.set_current(position)` if it differs
//!    from what `update` returned (a cooling cap, a fixed row); `domain.limits_changed()` after a cooling change.

use crate::rules::{Step, step_for};

/// `SCHED_CAPACITY_SCALE`: the capacity and the top of `util_avg` (include/linux/sched.h `SCHED_CAPACITY_SHIFT` 10).
pub const SCHED_CAPACITY_SCALE: u32 = 1024;
const SCHED_CAPACITY_SHIFT: u32 = 10;

/// `LOAD_AVG_PERIOD` (kernel/sched/sched-pelt.h:13): the half-life in periods, `y^32 = 1/2`.
pub const LOAD_AVG_PERIOD: u64 = 32;
/// `LOAD_AVG_MAX` (kernel/sched/sched-pelt.h:14): `1024 * sum(y^n)` for n >= 0, the largest the sum gets.
pub const LOAD_AVG_MAX: u32 = 47742;
/// `PELT_MIN_DIVIDER` (kernel/sched/pelt.h:41).
const PELT_MIN_DIVIDER: u32 = LOAD_AVG_MAX - 1024;
/// A period is 1024 time units. Linux counts 1024 ns units as "about a microsecond" (kernel/sched/pelt.c:109, :199); here
/// the unit is the microsecond itself, as the brief for patch 1235 asks.
const PERIOD: u64 = 1024;

/// `runnable_avg_yN_inv[]`, `0xffffffff * y^n` for n in 0..32 (kernel/sched/sched-pelt.h:4-11).
const RUNNABLE_AVG_YN_INV: [u32; 32] = [
    0xffffffff, 0xfa83b2da, 0xf5257d14, 0xefe4b99a, 0xeac0c6e6, 0xe5b906e6, 0xe0ccdeeb, 0xdbfbb796,
    0xd744fcc9, 0xd2a81d91, 0xce248c14, 0xc9b9bd85, 0xc5672a10, 0xc12c4cc9, 0xbd08a39e, 0xb8fbaf46,
    0xb504f333, 0xb123f581, 0xad583ee9, 0xa9a15ab4, 0xa5fed6a9, 0xa2704302, 0x9ef5325f, 0x9b8d39b9,
    0x9837f050, 0x94f4efa8, 0x91c3d373, 0x8ea4398a, 0x8b95c1e3, 0x88980e80, 0x85aac367, 0x82cd8698,
];

/// `decay_load` (kernel/sched/pelt.c:31-55): `val * y^n`, zero after 63 half-lives.
fn decay_load(mut val: u64, n: u64) -> u64 {
    if n > LOAD_AVG_PERIOD * 63 {
        return 0;
    }
    let mut local_n = n as u32;
    if local_n >= LOAD_AVG_PERIOD as u32 {
        val >>= local_n / LOAD_AVG_PERIOD as u32;
        local_n %= LOAD_AVG_PERIOD as u32;
    }
    // mul_u64_u32_shr(val, inv, 32)
    ((u128::from(val) * u128::from(RUNNABLE_AVG_YN_INV[local_n as usize])) >> 32) as u64
}

/// `__accumulate_pelt_segments` (kernel/sched/pelt.c:57-78): the contribution of a running span of `periods` periods
/// whose first part `d1` and last part `d3` are partial: `d1 y^p + 1024 sum(y^n, n=1..p-1) + d3`.
fn accumulate_pelt_segments(periods: u64, d1: u32, d3: u32) -> u32 {
    let c1 = decay_load(u64::from(d1), periods) as u32;
    let c2 = LOAD_AVG_MAX - decay_load(u64::from(LOAD_AVG_MAX), periods) as u32 - 1024;
    c1 + c2 + d3
}

/// One CPU's PELT utilisation: Linux's `struct sched_avg` reduced to the fields `util_avg` needs (`util_sum`,
/// `period_contrib`, `util_avg`, `last_update_time`).
///
/// Not frequency- or capacity-invariant: `busy_us` is wall time, so the value is "how much of the time was this CPU busy",
/// as Linux's is on a platform without `arch_scale_freq_capacity`. That is what [`next_freq_khz`] expects with `max_khz`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pelt {
    util_sum: u32,
    period_contrib: u32,
    util_avg: u32,
    last_update_us: u64,
}

impl Pelt {
    /// A CPU that has been idle forever, first seen at `now_us`.
    pub const fn new(now_us: u64) -> Self {
        Self {
            util_sum: 0,
            period_contrib: 0,
            util_avg: 0,
            last_update_us: now_us,
        }
    }

    /// `util_avg`, 0 to 1024 ([`SCHED_CAPACITY_SCALE`]): `util_sum / (LOAD_AVG_MAX - 1024 + period_contrib)`
    /// (`___update_load_avg`, kernel/sched/pelt.c:256-267; divider from `get_pelt_divider`, pelt.h:43-46). Like Linux it
    /// changes only when `update` crosses a period boundary.
    pub fn util_avg(&self) -> u32 {
        self.util_avg
    }

    /// Accounts the time since the previous call, `busy_us` of it running and the rest idle (`___update_load_sum`,
    /// kernel/sched/pelt.c:179-230, once with `running` clear and once with it set). Returns whether `util_avg` was
    /// recomputed (a period boundary was crossed).
    ///
    /// The busy part is taken as the later part of the interval: a tick finds the CPU busy now. `now_us` going backwards
    /// only resets the clock (pelt.c:190-193); an equal one does nothing (pelt.c:200-201).
    pub fn update(&mut self, now_us: u64, busy_us: u64) -> bool {
        if now_us < self.last_update_us {
            self.last_update_us = now_us;
            return false;
        }
        let delta = now_us - self.last_update_us;
        if delta == 0 {
            return false;
        }
        self.last_update_us = now_us;
        let busy = busy_us.min(delta);
        let mut crossed = false;
        if delta > busy {
            crossed |= self.accumulate_sum(delta - busy, false);
        }
        if busy > 0 {
            crossed |= self.accumulate_sum(busy, true);
        }
        if crossed {
            // ___update_load_avg: WRITE_ONCE(sa->util_avg, sa->util_sum / divider), never above the capacity.
            let divider = PELT_MIN_DIVIDER + self.period_contrib;
            self.util_avg = (self.util_sum / divider).min(SCHED_CAPACITY_SCALE);
        }
        crossed
    }

    /// `accumulate_sum` (kernel/sched/pelt.c:101-149) for `util_sum` only; true when a period boundary was crossed.
    fn accumulate_sum(&mut self, mut delta: u64, running: bool) -> bool {
        let mut contrib = delta as u32; // p == 0 -> delta < 1024
        delta += u64::from(self.period_contrib);
        let periods = delta / PERIOD;
        if periods > 0 {
            self.util_sum = decay_load(u64::from(self.util_sum), periods) as u32;
            delta %= PERIOD;
            if running {
                contrib =
                    accumulate_pelt_segments(periods, 1024 - self.period_contrib, delta as u32);
            }
        }
        self.period_contrib = delta as u32;
        if running {
            self.util_sum += contrib << SCHED_CAPACITY_SHIFT;
        }
        periods > 0
    }
}

/// `map_util_perf` (include/linux/sched/cpufreq.h:32-35): 25% headroom.
pub fn map_util_perf(util: u32) -> u32 {
    util + (util >> 2)
}

/// `map_util_freq` (include/linux/sched/cpufreq.h:26-30): `freq * util / cap`.
pub fn map_util_freq(util: u32, freq_khz: u32, cap: u32) -> u32 {
    (u64::from(freq_khz) * u64::from(util) / u64::from(cap)) as u32
}

/// The frequency `util` asks for, before it is resolved to a table row: `sugov_effective_cpu_perf` adds the headroom and
/// stops at the capacity (cpufreq_schedutil.c:181-196), then `get_next_freq` maps it on the reference frequency, which on a
/// platform with frequency invariance is the highest one (cpufreq_schedutil.c:125-141, :165-172).
pub fn next_freq_khz(util: u32, max_khz: u32) -> u32 {
    let perf = map_util_perf(util).min(SCHED_CAPACITY_SCALE);
    map_util_freq(perf, max_khz, SCHED_CAPACITY_SCALE)
}

/// `sugov_next_freq_shared` (cpufreq_schedutil.c:463-483): the domain takes the highest `util` of its CPUs, then
/// [`next_freq_khz`] and `cpufreq_driver_resolve_freq` (the lowest row at or above the frequency, `CPUFREQ_RELATION_LE`:
/// drivers/cpufreq/cpufreq.c:560-567, here with the existing [`step_for`], so within `cap` and with no skipping of
/// inefficient rows). Returns a position in `steps`.
pub fn next_step_shared(steps: &[Step], cap: usize, utils: impl IntoIterator<Item = u32>) -> usize {
    let util = utils.into_iter().max().unwrap_or(0);
    let top = steps.last().map_or(0, |s| s.khz);
    step_for(steps, next_freq_khz(util, top), cap)
}

/// One CPU's `sugov_hold_freq` state (cpufreq_schedutil.c:329-359, under `CONFIG_NO_HZ_COMMON`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hold {
    saved_idle_calls: u64,
}

impl Hold {
    /// "Maintain the frequency if the CPU has not been idle recently, as reduction is likely to be premature": true when
    /// `idle_calls` (idle entries of this CPU so far) equals the count at the previous call, which it then stores
    /// (cpufreq_schedutil.c:351-355). Call it only when a request is being decided, after the rate limit passed, as
    /// Linux does (cpufreq_schedutil.c:402-407). Linux's other two exits (sched_ext, uclamp_max) do not exist here.
    ///
    /// Linux 6.12 calls it from the single-CPU paths (`sugov_update_single_freq`, `sugov_update_single_perf`) and not from
    /// `sugov_update_shared`; the Juno domains share a policy, and the brief for patch 1235 asks for the hold there too.
    pub fn held(&mut self, idle_calls: u64) -> bool {
        let ret = idle_calls == self.saved_idle_calls;
        self.saved_idle_calls = idle_calls;
        ret
    }
}

/// One clock domain's `struct sugov_policy`: the rate limit, the row last asked for, and what `limits_changed` forces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Domain {
    rate_limit_us: u64,
    last_freq_update_us: Option<u64>,
    next: usize,
    need_freq_update: bool,
}

/// The default `rate_limit_us` when the driver gives no transition latency or delay: `USEC_PER_MSEC`
/// (`cpufreq_policy_transition_delay_us`, drivers/cpufreq/cpufreq.c:570-583; `qcom-cpufreq-hw` sets neither).
pub const DEFAULT_RATE_LIMIT_US: u64 = 1000;

impl Domain {
    /// A domain running row `current`, requests at least `rate_limit_us` apart. The first request is never limited
    /// (`last_freq_update_time` starts far in the past).
    pub const fn new(current: usize, rate_limit_us: u64) -> Self {
        Self {
            rate_limit_us,
            last_freq_update_us: None,
            next: current,
            need_freq_update: false,
        }
    }

    /// The row last asked for (`sg_policy->next_freq`).
    pub fn next(&self) -> usize {
        self.next
    }

    /// Tells the domain the row it runs when something else set it (a fixed row, a cooling cap).
    pub fn set_current(&mut self, position: usize) {
        self.next = position;
    }

    /// `sg_policy->limits_changed` (cpufreq_schedutil.c:84-88, :98-99): the next `update` ignores the rate limit, the hold
    /// and the equal-row suppression once, and the request goes out even if the row is the same.
    pub fn limits_changed(&mut self) {
        self.need_freq_update = true;
    }

    /// `sugov_should_update_freq` (cpufreq_schedutil.c:62-93) without the can-update-from-this-CPU test: false while less
    /// than `rate_limit_us` has passed since the last request, unless the limits changed.
    fn should_update(&self, now_us: u64) -> bool {
        if self.need_freq_update {
            return true;
        }
        match self.last_freq_update_us {
            None => true,
            // delta_ns is signed: a clock before the last request never passes.
            Some(last) => now_us >= last && now_us - last >= self.rate_limit_us,
        }
    }

    /// One governor update, `sugov_update_shared` with the hold of `sugov_update_single_freq` (cpufreq_schedutil.c:486-509,
    /// :402-416): `util` is the highest `util_avg` of the domain's CPUs (`sugov_next_freq_shared`), `held` is called only
    /// if the rate limit lets the update through and tells whether the CPU has stayed busy since its last decision
    /// ([`Hold::held`]).
    ///
    /// Returns the row to write, or `None` when the rate limit, the hold (a lower row is wanted but the CPU has not
    /// idled) or an unchanged row (`sugov_update_next_freq`, :95-107) says to do nothing. A returned row counts as
    /// requested: the domain remembers it and the time.
    pub fn update(
        &mut self,
        steps: &[Step],
        cap: usize,
        now_us: u64,
        util: u32,
        held: impl FnOnce() -> bool,
    ) -> Option<usize> {
        if !self.should_update(now_us) {
            return None;
        }
        // sugov_update_next_freq clears the flag (the driver has no CPUFREQ_NEED_UPDATE_LIMITS), nothing between reads it.
        let forced = core::mem::replace(&mut self.need_freq_update, false);
        let top = steps.last().map_or(0, |s| s.khz);
        let mut next = step_for(steps, next_freq_khz(util, top), cap);
        if held() && next < self.next && !forced {
            next = self.next;
        }
        if !forced && self.next == next {
            return None;
        }
        self.next = next;
        self.last_freq_update_us = Some(now_us);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The sc7180 `cpu6_opp_table` `opp-hz` values (arch/arm64/boot/dts/qcom/sc7180.dtsi:454-), in kHz.
    const GOLD_KHZ: [u32; 16] = [
        300_000, 652_800, 825_600, 979_200, 1_113_600, 1_267_200, 1_555_200, 1_708_800, 1_843_200,
        1_900_800, 1_996_800, 2_112_000, 2_208_000, 2_323_200, 2_400_000, 2_553_600,
    ];

    fn gold() -> Vec<Step> {
        GOLD_KHZ
            .iter()
            .enumerate()
            .map(|(i, &khz)| Step {
                index: i as u8,
                khz,
                millivolts: 0,
                boost: false,
            })
            .collect()
    }

    /// Expected values below were worked with an independent integer transcription of pelt.c (Python), 1000 µs ticks.
    fn run(ticks: u32, busy_us: u64) -> Pelt {
        let mut p = Pelt::new(0);
        for t in 1..=ticks {
            p.update(u64::from(t) * 1000, busy_us);
        }
        p
    }

    #[test]
    fn decay_matches_the_table() {
        assert_eq!(decay_load(1_000_000, 0), 999_999); // 0xffffffff, not 2^32
        assert_eq!(decay_load(1_000_000, 1), 978_572);
        assert_eq!(decay_load(1_000_000, 32), 499_999); // half-life: 32 periods
        assert_eq!(decay_load(1_000_000, 64), 249_999);
        assert_eq!(decay_load(1_000_000, 2016), 0); // 63 half-lives shift it all out
        assert_eq!(decay_load(1_000_000, 2017), 0);
    }

    #[test]
    fn segments() {
        assert_eq!(accumulate_pelt_segments(1, 1024, 0), 1002);
        assert_eq!(accumulate_pelt_segments(2, 1024, 0), 1981);
    }

    #[test]
    fn fully_busy_cpu_climbs_to_1024() {
        let at = |n| run(n, 1000).util_avg();
        assert_eq!(at(1), 0); // 1000 µs is not yet one 1024 µs period: no recompute
        assert_eq!(at(10), 195);
        assert_eq!(at(32), 503); // about half after one half-life
        assert_eq!(at(100), 900);
        assert_eq!(at(345), 1023); // LOAD_AVG_MAX: 1 - 2^-(345/32) of the way
        assert_eq!(at(1000), 1024);
    }

    #[test]
    fn half_busy_settles_near_half() {
        let p = run(500, 500);
        assert_eq!(p.util_avg(), 515);
    }

    #[test]
    fn idle_halves_every_32_ms() {
        let mut p = run(200, 1000);
        assert_eq!(p.util_avg(), 1009);
        for t in 201u32..=232 {
            p.update(u64::from(t) * 1000, 0);
        }
        assert_eq!(p.util_avg(), 513);
        // A long sleep seen in one update decays the same way, and 2016 periods end it.
        let mut q = run(200, 1000);
        q.update(200_000 + 100_000_000, 0);
        assert_eq!(q.util_avg(), 0);
    }

    #[test]
    fn clock_edges() {
        let mut p = Pelt::new(5000);
        assert!(!p.update(5000, 100)); // nothing elapsed
        assert!(!p.update(4000, 100)); // backwards: only resets the clock
        assert!(p.update(6100, 100_000)); // busy cut to the interval, crosses a period
        assert!(p.util_avg() <= SCHED_CAPACITY_SCALE);
    }

    #[test]
    fn headroom_on_the_gold_table() {
        assert_eq!(map_util_perf(512), 640);
        assert_eq!(map_util_perf(1024), 1280);
        // util 512 of 1024: 1.25 * 0.5 * 2553600 = 1596000 kHz -> the 1708800 row (lowest at or above).
        assert_eq!(next_freq_khz(512, 2_553_600), 1_596_000);
        let steps = gold();
        let n = next_step_shared(&steps, usize::MAX, [512, 100]);
        assert_eq!(steps[n].khz, 1_708_800);
        // 80% and over is the top (the tipping point of the 1.25).
        assert_eq!(next_freq_khz(820, 2_553_600), 2_553_600);
        assert_eq!(next_step_shared(&steps, usize::MAX, [820]), 15);
        assert_eq!(next_step_shared(&steps, usize::MAX, [0]), 0);
        // Under a cooling cap.
        assert_eq!(next_step_shared(&steps, 9, [1024]), 9);
        assert_eq!(next_step_shared(&steps, 9, []), 0);
    }

    #[test]
    fn hold_keeps_a_row_without_idle() {
        let steps = gold();
        let mut d = Domain::new(15, 1000);
        let mut h = Hold::default();
        // Low util wants row 0; the CPU never idled (idle_calls stays 0 = saved 0): hold.
        assert_eq!(d.update(&steps, usize::MAX, 10_000, 0, || h.held(0)), None);
        assert_eq!(d.next(), 15);
        // The CPU idled once: the hold lets go.
        assert_eq!(
            d.update(&steps, usize::MAX, 20_000, 0, || h.held(1)),
            Some(0)
        );
        assert_eq!(d.next(), 0);
        // Holding never stops a rise.
        let mut h2 = Hold::default();
        assert_eq!(
            d.update(&steps, usize::MAX, 30_000, 1024, || h2.held(0)),
            Some(15)
        );
    }

    #[test]
    fn hold_state_is_read_only_after_the_rate_limit() {
        let steps = gold();
        let mut d = Domain::new(15, 1000);
        let mut h = Hold::default();
        assert_eq!(d.update(&steps, usize::MAX, 10_000, 0, || h.held(0)), None); // held; no request made, no time stored
        assert_eq!(
            d.update(&steps, usize::MAX, 10_100, 0, || h.held(3)),
            Some(0)
        );
        // Inside the limit the closure is not called at all.
        assert_eq!(
            d.update(&steps, usize::MAX, 10_200, 1024, || panic!(
                "hold read inside the limit"
            )),
            None
        );
    }

    #[test]
    fn rate_limit_drops_a_second_request_inside_1ms() {
        let steps = gold();
        let mut d = Domain::new(0, DEFAULT_RATE_LIMIT_US);
        assert_eq!(
            d.update(&steps, usize::MAX, 100_000, 1024, || false),
            Some(15)
        );
        // Another CPU of the domain ticks 500 µs later wanting the bottom: dropped.
        assert_eq!(d.update(&steps, usize::MAX, 100_500, 0, || false), None);
        assert_eq!(d.update(&steps, usize::MAX, 100_999, 0, || false), None);
        assert_eq!(d.next(), 15);
        // Exactly 1000 µs after the request it passes (delta >= delay).
        assert_eq!(d.update(&steps, usize::MAX, 101_000, 0, || false), Some(0));
    }

    #[test]
    fn same_row_asks_for_nothing() {
        let steps = gold();
        let mut d = Domain::new(7, 1000);
        // util 512 -> row 7 (1708800), which it already runs.
        assert_eq!(d.update(&steps, usize::MAX, 50_000, 512, || false), None);
        // Nothing was requested, so the limit did not start: a change right after goes out.
        assert_eq!(
            d.update(&steps, usize::MAX, 50_001, 1024, || false),
            Some(15)
        );
    }

    #[test]
    fn limits_changed_forces_one_request() {
        let steps = gold();
        let mut d = Domain::new(7, 1000);
        assert_eq!(
            d.update(&steps, usize::MAX, 50_000, 1024, || false),
            Some(15)
        );
        d.limits_changed();
        // Inside the limit, same row, held: still goes out once.
        assert_eq!(d.update(&steps, 9, 50_100, 1024, || true), Some(9));
        d.limits_changed();
        assert_eq!(d.update(&steps, 9, 50_200, 1024, || true), Some(9));
        assert_eq!(d.update(&steps, 9, 50_300, 1024, || true), None);
    }
}
