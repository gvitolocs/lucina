// SPDX-License-Identifier: MPL-2.0

//! The SC7180's CPU clocks and temperature sensors, as register logic with no hardware behind it (D-022).
//!
//! Behaviour follows Linux's `drivers/cpufreq/qcom-cpufreq-hw.c`, `drivers/interconnect/qcom/osm-l3.c` and
//! `drivers/thermal/qcom/tsens*.c` (facts and register layouts, no code: see `docs/research/juno-performance.md`). The
//! firmware fills a table of frequencies and voltages for each clock domain; the OS only ever chooses a row of it by
//! writing its index to one register, and hardware does the rest, voltage included.

#![no_std]

extern crate alloc;

pub mod rules;

use alloc::vec::Vec;

pub use rules::{CpuLoad, BUSY_RUN_FOR_MAX, Step, step_for, wanted_khz};

/// Registers of one clock domain ("OSM", `qcom_soc_data` in Linux), as offsets from the domain's base.
pub const REG_ENABLE: usize = 0x0;
pub const REG_DCVS_CTRL: usize = 0xbc;
pub const REG_FREQ_LUT: usize = 0x110;
pub const REG_VOLT_LUT: usize = 0x114;
pub const REG_CURRENT_VOTE: usize = 0x704;
pub const REG_PERF_STATE: usize = 0x920;
pub const LUT_ROW_SIZE: usize = 32;
pub const LUT_MAX_ROWS: usize = 40;

/// The always-on crystal, which the table's multipliers count in.
pub const XO_KHZ: u32 = 19_200;
/// GPLL0 / 2, the "alternate" clock of the lowest rows.
pub const ALTERNATE_KHZ: u32 = 300_000;

/// Reads a domain's table as Linux does (`qcom_cpufreq_hw_read_lut`): frequency from the multiplier unless the row uses the
/// alternate clock, rows that say "one core" are not for general use, and two equal frequencies in a row end the table.
/// The boost row the table's end marks is left out, because boost stays off.
pub fn read_table(read32: impl Fn(usize) -> u32) -> Vec<Step> {
    const LUT_TURBO_IND: u32 = 1;
    let mut steps = Vec::new();
    let mut prev_khz = 0;
    for row in 0..LUT_MAX_ROWS {
        let freq = read32(REG_FREQ_LUT + row * LUT_ROW_SIZE);
        let volt = read32(REG_VOLT_LUT + row * LUT_ROW_SIZE);
        let src = freq >> 30;
        let lval = freq & 0xff;
        let core_count = (freq >> 16) & 0x7;
        let millivolts = volt & 0xfff;
        let khz = if src != 0 { XO_KHZ * lval } else { ALTERNATE_KHZ };

        if row > 0 && khz == prev_khz {
            break;
        }
        if core_count != LUT_TURBO_IND {
            steps.push(Step {
                index: row as u8,
                khz,
                millivolts,
                boost: false,
            });
        }
        prev_khz = khz;
    }
    steps
}

/// A temperature sensor's status register (`TM_Sn_STATUS`), one per sensor from this offset.
pub const TM_SN_STATUS: usize = 0xa0;

/// The temperature in a status register, in thousandths of a degree, or `None` while it is being updated.
pub fn tsens_millicelsius(status: u32) -> Option<i32> {
    const VALID: u32 = 1 << 21;
    if status & VALID == 0 {
        return None;
    }
    // 12 bits, signed, in tenths of a degree.
    let tenths = ((status << 20) as i32) >> 20;
    Some(tenths * 100)
}

/// The Arm or Qualcomm name of a core from its MIDR, and whether it is the fast kind on this SoC.
pub fn core_name(midr: u64) -> &'static str {
    let implementer = (midr >> 24) & 0xff;
    let part = (midr >> 4) & 0xfff;
    match (implementer, part) {
        (0x41, 0xd05) => "Cortex-A55",
        (0x41, 0xd0b) => "Cortex-A76",
        (0x51, 0x805) => "Kryo 4xx Silver (Cortex-A55)",
        (0x51, 0x804) => "Kryo 4xx Gold (Cortex-A76)",
        _ => "unknown",
    }
}

/// One trip point of a thermal zone, from the device tree, in thousandths of a degree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trip {
    pub temperature: i32,
    pub hysteresis: i32,
}

/// A thermal zone as `sc7180.dtsi` defines it: its sensor, its trips, and the CPU domain (the cooling device) its cooling
/// maps name. In thousandths of a degree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneDef {
    /// The `thermal-sensors` id on `tsens0`.
    pub sensor: u32,
    /// The passive trips, ascending; both map to `domain`.
    pub passive: [Trip; 2],
    /// The critical trip: recorded, but the step-wise governor takes no action on it (left to hardware).
    pub critical: Trip,
    /// The CPU domain (`&cpuN`) the zone's cooling maps point to.
    pub domain: u32,
}

const PASSIVE: [Trip; 2] = [
    Trip {
        temperature: 90_000,
        hysteresis: 2_000,
    },
    Trip {
        temperature: 95_000,
        hysteresis: 2_000,
    },
];
const CRITICAL: Trip = Trip {
    temperature: 110_000,
    hysteresis: 1_000,
};

/// `sc7180.dtsi`'s `cpu0-thermal` to `cpu7-thermal`: sensors 1 to 6 on the silver domain, 9 and 10 on the gold one.
pub const ZONES: [ZoneDef; 8] = [
    ZoneDef { sensor: 1, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 2, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 3, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 4, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 5, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 6, passive: PASSIVE, critical: CRITICAL, domain: 0 },
    ZoneDef { sensor: 9, passive: PASSIVE, critical: CRITICAL, domain: 6 },
    ZoneDef { sensor: 10, passive: PASSIVE, critical: CRITICAL, domain: 6 },
];

/// How the zone's temperature moved since the last poll, as `get_tz_trend` works it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trend {
    Raising,
    Dropping,
    Stable,
}

/// One zone's step-wise governor, as Linux's `gov_step_wise.c` runs it: each passive trip has its own threshold (released
/// `hysteresis` below the trip), its own target cooling state, and the zone's cooling device (its CPU domain) takes the
/// highest target. The critical trip takes no action here. Cooling states are 0 (none) to `max_state`.
pub struct StepWise {
    trips: [Trip; 2],
    last_temperature: i32,
    /// Per trip, Linux's `td->threshold`: the trip's temperature until reached, then temperature minus hysteresis.
    thresholds: [i32; 2],
    /// Per trip instance, its target cooling state; `None` is `THERMAL_NO_TARGET`.
    targets: [Option<u32>; 2],
    initialized: [bool; 2],
    /// The cooling device's current state.
    cooling: u32,
}

impl StepWise {
    pub fn new(def: &ZoneDef) -> Self {
        StepWise {
            trips: def.passive,
            last_temperature: 0,
            thresholds: [def.passive[0].temperature, def.passive[1].temperature],
            targets: [None, None],
            initialized: [false, false],
            cooling: 0,
        }
    }

    /// One poll of a zone that has its cooling device to itself (a test, or a domain with one zone): [`StepWise::poll_shared`]
    /// with this zone's last result as the device's state.
    pub fn poll(&mut self, temperature: i32, max_state: u32) -> u32 {
        let state = device_state(core::iter::once(self.poll_shared(temperature, self.cooling, max_state)));
        self.cooling = state;
        state
    }

    /// One poll of the zone, as `step_wise_manage` and `thermal_zone_trip_update` run it, against the cooling device's
    /// current state `cur_state` (shared by every zone bound to the device, as Linux's `get_cur_state`). Returns this
    /// zone's request: the highest target of its trips, `None` when none has one (`THERMAL_NO_TARGET`).
    pub fn poll_shared(&mut self, temperature: i32, cur_state: u32, max_state: u32) -> Option<u32> {
        let trend = if temperature > self.last_temperature {
            Trend::Raising
        } else if temperature < self.last_temperature {
            Trend::Dropping
        } else {
            Trend::Stable
        };
        self.last_temperature = temperature;
        for i in 0..2 {
            let trip = self.trips[i];
            if temperature >= trip.temperature {
                self.thresholds[i] = trip.temperature - trip.hysteresis;
            } else if temperature < trip.temperature - trip.hysteresis {
                self.thresholds[i] = trip.temperature;
            }
            let throttle = temperature >= self.thresholds[i];
            let cur = cur_state;
            // `get_target_state`, with the instance's limits THERMAL_NO_LIMIT: lower 0, upper `max_state`.
            let target = if !self.initialized[i] {
                if throttle {
                    Some((cur + 1).min(max_state))
                } else {
                    None
                }
            } else if throttle {
                match trend {
                    Trend::Raising => Some((cur + 1).min(max_state)),
                    Trend::Dropping => Some(
                        cur.saturating_sub(1)
                            .max(1.min(max_state))
                            .min(max_state),
                    ),
                    Trend::Stable => self.targets[i],
                }
            } else {
                match trend {
                    Trend::Dropping => (cur > 0).then_some(0),
                    _ => self.targets[i],
                }
            };
            self.targets[i] = target;
            self.initialized[i] = true;
        }
        self.targets.iter().flatten().max().copied()
    }
}

/// `__thermal_cdev_update`: the cooling device takes the highest target of every instance bound to it, and 0 when none
/// has a target.
pub fn device_state(requests: impl Iterator<Item = Option<u32>>) -> u32 {
    requests.flatten().max().unwrap_or(0)
}

/// The cooling device of one CPU domain and every zone bound to it (six zones on the Silver domain, two on the Gold one
/// in `sc7180.dtsi`). Each poll runs every zone against the device's one state, then updates it.
pub struct DomainCooling {
    zones: alloc::vec::Vec<(usize, StepWise)>,
    state: u32,
}

impl DomainCooling {
    /// The zones of [`ZONES`] whose `domain` is `first_cpu`.
    pub fn new(first_cpu: u32) -> Self {
        let zones = ZONES
            .iter()
            .enumerate()
            .filter(|(_, z)| z.domain == first_cpu)
            .map(|(i, z)| (i, StepWise::new(z)))
            .collect();
        DomainCooling { zones, state: 0 }
    }

    /// The sensors to read, as indexes into [`ZONES`].
    pub fn zones(&self) -> impl Iterator<Item = usize> + '_ {
        self.zones.iter().map(|(i, _)| *i)
    }

    /// One poll: `temperature(zone)` in millidegrees for each zone index (`None`: not read, the zone is skipped as
    /// Linux skips a zone whose read fails). Returns the device's new cooling state.
    pub fn poll(&mut self, mut temperature: impl FnMut(usize) -> Option<i32>, max_state: u32) -> u32 {
        let cur = self.state;
        let requests: alloc::vec::Vec<Option<u32>> = self
            .zones
            .iter_mut()
            .map(|(i, zone)| match temperature(*i) {
                Some(t) => zone.poll_shared(t, cur, max_state),
                None => zone.targets.iter().flatten().max().copied(),
            })
            .collect();
        self.state = device_state(requests.into_iter());
        self.state
    }

    pub fn state(&self) -> u32 {
        self.state
    }
}

/// The position in `steps` (ascending) that a cooling state allows: state 0 is the top row, each step of cooling one row
/// lower.
pub fn cap_for(cooling: u32, rows: usize) -> usize {
    rows.saturating_sub(1)
        .saturating_sub(cooling as usize)
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec};

    use super::*;

    /// A domain's table as registers: (src, lval, core count, millivolts) per row.
    fn table(rows: &[(u32, u32, u32, u32)]) -> impl Fn(usize) -> u32 {
        let mut regs = BTreeMap::new();
        for (i, &(src, lval, cores, mv)) in rows.iter().enumerate() {
            regs.insert(REG_FREQ_LUT + i * LUT_ROW_SIZE, (src << 30) | (cores << 16) | lval);
            regs.insert(REG_VOLT_LUT + i * LUT_ROW_SIZE, mv);
        }
        move |offset| *regs.get(&offset).unwrap_or(&0)
    }

    #[test]
    fn alternate_clock_then_multiples_of_the_crystal_then_end() {
        let steps = read_table(table(&[(0, 0, 8, 600), (1, 34, 8, 650), (1, 52, 8, 700), (1, 52, 8, 700)]));
        assert_eq!(
            steps,
            vec![
                Step { index: 0, khz: 300_000, millivolts: 600, boost: false },
                Step { index: 1, khz: 652_800, millivolts: 650, boost: false },
                Step { index: 2, khz: 998_400, millivolts: 700, boost: false },
            ]
        );
    }

    #[test]
    fn one_core_rows_are_left_out_including_the_boost() {
        let steps = read_table(table(&[
            (1, 34, 8, 650),
            (1, 52, 1, 700), // one core, in the middle: not usable
            (1, 62, 8, 750),
            (1, 133, 1, 950), // one core, last: the boost, filtered as boost stays off
            (1, 133, 1, 950),
        ]));
        assert_eq!(steps.iter().map(|s| s.index).collect::<Vec<_>>(), vec![0, 2]);
        assert_eq!(steps[1].khz, 1_190_400);
        assert!(steps.iter().all(|s| !s.boost));
    }

    #[test]
    fn a_table_of_forty_different_rows_stops_at_forty() {
        let rows: Vec<_> = (0..40).map(|i| (1, 10 + i, 8, 600)).collect();
        assert_eq!(read_table(table(&rows)).len(), 40);
    }

    #[test]
    fn tsens_values() {
        assert_eq!(tsens_millicelsius(0), None);
        assert_eq!(tsens_millicelsius((1 << 21) | 455), Some(45_500));
        // Negative: 12-bit two's complement, tenths.
        assert_eq!(tsens_millicelsius((1 << 21) | 0xff6), Some(-1_000));
    }

    #[test]
    fn core_names() {
        assert_eq!(core_name(0x51df_805e), "Kryo 4xx Silver (Cortex-A55)");
        assert_eq!(core_name(0x51df_804e), "Kryo 4xx Gold (Cortex-A76)");
        assert_eq!(core_name(0x410f_d0b1), "Cortex-A76");
        assert_eq!(core_name(0), "unknown");
    }

    #[test]
    fn load_rises_while_busy_and_falls_to_nothing_while_idle() {
        let mut load = CpuLoad::default();
        for _ in 0..3 {
            load.tick(0, true);
        }
        assert_eq!(load.busy_run, 3);
        for _ in 0..100 {
            load.tick(0, true);
        }
        assert!(load.util > 1000 && load.util <= 1024);
        load.tick(0, false);
        assert_eq!(load.busy_run, 0);
        load.tick(127, false);
        assert_eq!(load.util, 0);
    }

    #[test]
    fn a_sleep_counts_as_idle_ticks() {
        let mut load = CpuLoad { util: 1024, busy_run: 9 };
        load.tick(16, true);
        assert_eq!(load.busy_run, 1);
        assert!(load.util < 500, "{}", load.util);
    }

    #[test]
    fn load_stays_in_range() {
        let mut load = CpuLoad { util: 1024, busy_run: 0 };
        load.tick(0, true);
        assert_eq!(load.util, 1024);
        let mut load = CpuLoad { util: 1, busy_run: 0 };
        load.tick(0, false);
        assert_eq!(load.util, 0);
        load.tick(u64::MAX, false);
        assert_eq!(load.util, 0);
    }

    #[test]
    fn wanted_clock() {
        let run = |util, busy_run| CpuLoad { util, busy_run };
        // Busy for a while: the top, whatever the average says yet.
        assert_eq!(wanted_khz(run(100, 2), 300_000, 2_553_600), 2_553_600);
        // Half of 1 GHz in use: 625 MHz.
        assert_eq!(wanted_khz(run(512, 0), 1_000_000, 2_553_600), 625_000);
        assert_eq!(wanted_khz(run(1024, 0), 2_553_600, 2_553_600), 2_553_600);
        assert_eq!(wanted_khz(run(0, 0), 2_553_600, 2_553_600), 0);
    }

    fn steps(khz: &[u32]) -> Vec<Step> {
        khz.iter()
            .enumerate()
            .map(|(i, &khz)| Step { index: i as u8, khz, millivolts: 600, boost: false })
            .collect()
    }

    #[test]
    fn step_for_a_clock() {
        let s = steps(&[300_000, 600_000, 1_000_000]);
        assert_eq!(step_for(&s, 0, 2), 0);
        assert_eq!(step_for(&s, 300_000, 2), 0);
        assert_eq!(step_for(&s, 300_001, 2), 1);
        assert_eq!(step_for(&s, 2_000_000, 2), 2);
        // A cap wins.
        assert_eq!(step_for(&s, 2_000_000, 1), 1);
        assert_eq!(step_for(&s, 900_000, 0), 0);
    }

    fn zone() -> StepWise {
        StepWise::new(&ZONES[0])
    }

    #[test]
    fn rising_through_each_trip_steps_the_cooling_state_up_one_at_a_time() {
        let mut z = zone();
        assert_eq!(z.poll(85_000, 9), 0, "below both trips: nothing");
        assert_eq!(z.poll(91_000, 9), 1, "past 90 C, rising: one step");
        assert_eq!(z.poll(92_000, 9), 2, "still rising: one more step");
        assert_eq!(z.poll(96_000, 9), 3, "past 95 C: the second trip joins, one step");
        assert_eq!(z.poll(97_000, 9), 4);
    }

    #[test]
    fn falling_below_trip_minus_hysteresis_steps_down_and_releases() {
        let mut z = zone();
        for t in [91_000, 92_000, 96_000, 97_000] {
            z.poll(t, 9);
        }
        assert_eq!(z.poll(94_000, 9), 3, "falling but still throttled: one step down");
        assert_eq!(z.poll(89_000, 9), 2, "within the hysteresis of 90 C: still throttled, one step down");
        assert_eq!(z.poll(89_000, 9), 2, "stable within the hysteresis: held");
        assert_eq!(z.poll(87_000, 9), 0, "below 88 C: released to no cooling");
    }

    #[test]
    fn a_stable_temperature_keeps_the_state() {
        let mut z = zone();
        z.poll(85_000, 9);
        assert_eq!(z.poll(91_000, 9), 1);
        assert_eq!(z.poll(91_000, 9), 1, "stable at the trip: no change");
        assert_eq!(z.poll(96_000, 9), 2);
        assert_eq!(z.poll(96_000, 9), 2, "stable past the second trip: no change");
    }

    #[test]
    fn the_critical_trip_takes_no_action() {
        let mut z = zone();
        z.poll(85_000, 9);
        // 110 C is the critical trip: the governor only steps the passive trips, one per poll, no shutdown.
        assert_eq!(z.poll(110_000, 9), 1);
        assert_eq!(z.poll(111_000, 9), 2);
        assert_eq!(ZONES[0].critical.temperature, 110_000);
    }

    #[test]
    fn zones_are_independent() {
        let mut silver = StepWise::new(&ZONES[0]);
        let mut gold = StepWise::new(&ZONES[6]);
        assert_eq!(silver.poll(96_000, 9), 1);
        assert_eq!(gold.poll(80_000, 9), 0, "the other zone's heat does not touch this one");
        assert_eq!(silver.poll(97_000, 9), 2);
        assert_eq!(gold.poll(80_000, 9), 0);
        assert_eq!(cap_for(silver.poll(80_000, 9), 10), 9);
        assert_eq!(cap_for(gold.poll(80_000, 9), 10), 9);
    }

    #[test]
    fn no_target_anywhere_puts_the_device_to_zero() {
        // __thermal_cdev_update: no instance with a target gives state 0, not the old state.
        assert_eq!(device_state([None, None].into_iter()), 0);
        assert_eq!(device_state([None, Some(3), Some(1)].into_iter()), 3);
    }

    #[test]
    fn zones_of_one_domain_share_the_device_state() {
        let mut silver = DomainCooling::new(0);
        assert_eq!(silver.zones().count(), 6);
        assert_eq!(DomainCooling::new(6).zones().count(), 2);
        let hot = |t: i32| move |zone: usize| Some(if zone == 0 { t } else { 60_000 });
        assert_eq!(silver.poll(hot(85_000), 9), 0);
        assert_eq!(silver.poll(hot(91_000), 9), 1, "one hot zone raises the shared device");
        assert_eq!(silver.poll(hot(92_000), 9), 2, "from the device's state, one more step");
        // The cool zones see the device at 2, not at their own 0: dropping or stable, below their trips, no target.
        assert_eq!(silver.poll(hot(87_000), 9), 0, "the hot zone falls below 88 C: released");
        assert_eq!(silver.state(), 0);
    }
}
