// SPDX-License-Identifier: MPL-2.0

//! Reproduces the Gold pair's flapping on the host: the old governor (`CpuLoad::tick` + `wanted_khz` + `step_for`, called as
//! `kernel/core/src/device/qcom_cpufreq.rs` does on each tick) and the schedutil one (`schedutil.rs`) get the same synthetic
//! load, and the target changes per second are counted.
//!
//! Load: two CPUs of one domain, 1 ms ticks (the second CPU 500 µs after the first), each busy a fraction of every tick
//! (a base of 10 to 90 %, the brief's 20, 50 and 90 among them plus a uniform jitter of +-15 points, the shared ping-pong of ~100,000 wakes/s), 10 s simulated.
//! The old governor samples "busy now" at the tick (`!is_cpu_idle()`), so it sees the fraction as the odds of a busy sample.
//!
//! A base whose util lands on a table boundary flaps with either governor (schedutil at 20 %: util 209-211 straddles the
//! 652.8 MHz row); the sweep shows that this is the exception.
//!
//! Run: `cargo run -q -p lucina-cpufreq --example flapping`

use lucina_cpufreq::schedutil::{DEFAULT_RATE_LIMIT_US, Domain, Hold, Pelt};
use lucina_cpufreq::{CpuLoad, Step, step_for, wanted_khz};

/// The sc7180 `cpu6_opp_table` (`opp-hz`, kHz).
const GOLD_KHZ: [u32; 16] = [
    300_000, 652_800, 825_600, 979_200, 1_113_600, 1_267_200, 1_555_200, 1_708_800, 1_843_200,
    1_900_800, 1_996_800, 2_112_000, 2_208_000, 2_323_200, 2_400_000, 2_553_600,
];
const SECONDS: u64 = 10;
const TICK_US: u64 = 1000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The busy fraction of one tick: `base` +-15 points.
fn fraction(rng: &mut Rng, base: f64) -> f64 {
    (base + (rng.next() - 0.5) * 0.30).clamp(0.0, 1.0)
}

/// The old governor on two CPUs of a domain, as `on_tick`, `Domain::update` and `Domain::target` run it.
fn old(steps: &[Step], base: f64) -> u64 {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let top = steps.last().unwrap().khz;
    let mut load = [CpuLoad::default(); 2];
    let mut wanted = [u32::MAX; 2];
    let mut current = steps.len() - 1;
    let mut changes = 0;
    for tick in 0..SECONDS * 1000 {
        for cpu in 0..2 {
            let busy = rng.next() < fraction(&mut rng, base);
            load[cpu].tick(0, busy);
            let now_khz = steps[current].khz;
            let mut w = wanted_khz(load[cpu], now_khz, top);
            if load[cpu].busy_run > 0 {
                w = w.max(now_khz);
            }
            wanted[cpu] = w;
            let target = step_for(steps, *wanted.iter().max().unwrap(), usize::MAX);
            if target != current {
                current = target;
                changes += 1;
            }
        }
        let _ = tick;
    }
    changes
}

/// The schedutil governor: PELT per CPU, the domain's max, the hold, the 1 ms rate limit.
fn new(steps: &[Step], base: f64) -> u64 {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut pelt = [Pelt::new(0); 2];
    let mut hold = [Hold::default(); 2];
    let mut idle_calls = [0u64; 2];
    let mut domain = Domain::new(steps.len() - 1, DEFAULT_RATE_LIMIT_US);
    let mut changes = 0;
    for tick in 1..=SECONDS * 1000 {
        for cpu in 0..2 {
            let now = tick * TICK_US + cpu as u64 * TICK_US / 2;
            let busy_us = (fraction(&mut rng, base) * TICK_US as f64).round() as u64;
            // The CPU entered idle in this tick unless it was busy all of it.
            if busy_us < TICK_US {
                idle_calls[cpu] += 1;
            }
            pelt[cpu].update(now, busy_us);
            let util = pelt.iter().map(Pelt::util_avg).max().unwrap();
            if domain
                .update(steps, usize::MAX, now, util, || {
                    hold[cpu].held(idle_calls[cpu])
                })
                .is_some()
            {
                changes += 1;
            }
        }
    }
    changes
}

fn main() {
    let steps: Vec<Step> = GOLD_KHZ
        .iter()
        .enumerate()
        .map(|(i, &khz)| Step {
            index: i as u8,
            khz,
            millivolts: 0,
            boost: false,
        })
        .collect();
    println!(
        "Gold pair, 2 CPUs, 1 ms ticks, {SECONDS} s, busy fraction per tick = base +-15 points"
    );
    println!(
        "{:>6} {:>14} {:>16}",
        "base", "old chg/s", "schedutil chg/s"
    );
    for base in [0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90] {
        let o = old(&steps, base);
        let n = new(&steps, base);
        println!(
            "{:>5.0}% {:>14.1} {:>16.1}",
            base * 100.0,
            o as f64 / SECONDS as f64,
            n as f64 / SECONDS as f64
        );
    }
}
