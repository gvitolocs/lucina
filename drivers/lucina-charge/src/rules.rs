//! What may be written to the charger, as plain arithmetic: the battery's limits, the stop rules, the temperature steps, the recharge
//! rule, and the one function that says what a round writes. Verus proves this very file, not a copy of it
//! (`verification/juno-charge/rules.rs`; the attributes are only seen by Verus, rustc ignores them). What each function promises is in
//! its `ensures`.
//!
//! The values are Samsung's own for this pack, from its Windows driver `EmuEC.sys` (`docs/research/juno-charge.md` has every one with
//! its address), except where this file says stricter.
//!
//! Not proved here: that the numbers read are what the gauge holds, that the executor in `control.rs` does what [`plan`] says (its
//! tests check that), and that the chip does what it is told.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The voltage cap: Samsung's table for all three of this laptop's battery models, and what the boot firmware programs (`0x2238`).
/// The gauge itself asks for 8860 mV. Samsung never gives it that, so neither does this.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VMAX_MV: u16 = 8760;
/// The current cap, an owner's decision of 2026-11-01. Samsung charges at 3776 mA.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const DEFAULT_CAP_MA: u16 = 2048;
/// The most this pack is ever given, whatever a caller asks for: Samsung's 0.72 C of the 5320 mAh design capacity (3830 mA), on the
/// register's 64 mA steps.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const DESIGN_MAX_MA: u16 = 3776;
/// A voltage request outside this window stops charging (the pack is 2S, design voltage 7720 mV; the gauge asks for 8860 mV).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const V_REQ_MIN_MV: u16 = 8000;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const V_REQ_MAX_MV: u16 = 8900;
/// The battery's temperature, in the gauge's 0.1 K, where charging is allowed: exactly 0.0..=45.0 °C. Samsung's window is 0..50 °C; 45
/// is the original `gbgo-charge`'s and is stricter.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TEMP_MIN_DK: u16 = 2732;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TEMP_MAX_DK: u16 = 3182;
/// A battery voltage outside this window is not a 2S pack that may be charged: the bus or the gauge is lying, or the pack is too far
/// gone. Samsung stops at 9000 mV; 8860 mV is what the gauge itself asks for, and more than the charger can reach at 8760 mV + 1%.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BATTERY_MIN_MV: u16 = 5000;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BATTERY_MAX_MV: u16 = 8860;
/// Below this Samsung charges at [`PRECHARGE_MA`]: `0x1838`, in its table.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PRECHARGE_BELOW_MV: u16 = 6200;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PRECHARGE_MA: u16 = 512;
/// Above a hot or cold step Samsung stops if a cell is over 4099 mV (`0x1003`, its cell voltages). This reads the pack: two such cells.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SWELL_MAX_MV: u16 = 8198;
/// The hot step's limits: `0x2070` and `0x77b`, on the registers' steps.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const HOT_MV: u16 = 8304;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const HOT_MA: u16 = 1856;
/// The cool step's limit: `0x4fd`, on the register's step.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const COOL_MA: u16 = 1216;
/// The cold step's limit: `0x214`, on the register's step.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const COLD_MA: u16 = 512;
/// The capacity at or below which a full battery may charge again: Samsung's 97% (`0x61`).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const RECHARGE_AT_PERCENT: u16 = 97;
/// How long the gauge may repeat itself exactly while it is being charged.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FROZEN_LIMIT_MS: u64 = 900_000;
/// The longest one charge may last, then charging stops until the adapter comes back or the machine restarts. Samsung allows 18 h at
/// nearly twice this current.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CHARGE_LIMIT_MS: u64 = 12 * 3_600_000;
/// The longest a battery may stay below [`PRECHARGE_BELOW_MV`] while charging: Samsung's 7200 ticks.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PRECHARGE_LIMIT_MS: u64 = 2 * 3_600_000;

/// BatteryStatus bits, as the kernel's sbs-battery.c reads them.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FULLY_CHARGED: u16 = 0x20;
/// BatteryStatus alarms that tell a charger to stop (SBS 1.1, 5.1.21): OVER_CHARGED_ALARM, TERMINATE_CHARGE_ALARM, OVER_TEMP_ALARM.
/// A full battery shows TERMINATE_CHARGE too: `0x40e0` on this laptop.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CHARGE_ALARMS: u16 = 0xd000;

/// What every pass reads from the gauge, in the order it reads it.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub status: u16,
    /// 0.1 K
    pub temp_dk: u16,
    pub battery_mv: u16,
    pub req_ma: u16,
    pub req_mv: u16,
}

/// Why a pass stops charging.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// In the gauge's 0.1 K.
    Temperature(u16),
    /// The gauge's BatteryStatus has a charge alarm set.
    Alarm(u16),
    FullyCharged,
    Current(u16),
    Voltage(u16),
    BatteryVoltage(u16),
    /// Hot or cold, and the pack too high for that.
    Swelling(u16),
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Stop(Stop),
    /// The register values to write: MaxChargeVoltage, then ChargeCurrent. Both are in mV / mA already (bit n is worth 2^n).
    Charge { mv: u16, ma: u16 },
}

/// Samsung's temperature steps. Each has its own way out, so that a temperature on the edge does not switch the current back and forth.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Cold,
    Cool,
    Normal,
    Hot,
}

/// Why a round writes no setpoint.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Blocked {
    /// Not this chip: only ever a stop, and only if the right IDs have been seen.
    UnknownChip,
    /// Something drives this chip or the bus lies: never again until a restart.
    Latched,
    /// The kernel's own health check says no: the charger's watchdog is left to run out.
    Unhealthy,
    /// The chip holds more than the caps.
    OverCaps,
    /// ChargeOption0 does not have the 175 s watchdog set.
    WatchdogNotSet,
    /// The gauge measures more current than the charger was told.
    OverCurrent,
    /// The gauge repeats itself exactly while being charged.
    Stale,
    /// The one charge has lasted too long.
    Timer,
    /// No adapter: a setpoint would do nothing, and the chip clears ChargeCurrent by itself.
    NoAdapter,
    /// `lucina.charge.window`, the supervised test's end.
    Window,
    /// A stop rule.
    Stop(Stop),
    /// It was full, and has not come down to [`RECHARGE_AT_PERCENT`].
    WaitRecharge,
}

/// What a round writes.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// ChargeCurrent 0, a line on the screen when `announce`.
    Zero { announce: bool },
    /// MaxChargeVoltage, then ChargeCurrent.
    Write { mv: u16, ma: u16 },
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub action: Action,
    pub blocked: Option<Blocked>,
}

/// What a round knows besides the gauge's reading.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// This round's IDs are the BQ25710's.
    pub ids_known: bool,
    /// The right IDs have been seen at least once.
    pub verified: bool,
    pub latched: bool,
    pub healthy: bool,
    /// ChargeOption0, ChargeCurrent and MaxChargeVoltage as read this round.
    pub watchdog_set: bool,
    pub chip_ma: u16,
    pub chip_mv: u16,
    /// ChargerStatus says an adapter is in. Samsung sets nothing without one.
    pub adapter: bool,
    /// A write of ours may have started charging.
    pub charging: bool,
    pub over_current: bool,
    pub fresh: bool,
    pub timer_left: bool,
    pub window_open: bool,
    /// Full, and not back at [`RECHARGE_AT_PERCENT`].
    pub full: bool,
    /// A pass is due: setpoints go out once per pass.
    pub due: bool,
    pub cap_ma: u16,
    pub band: Band,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The numbers themselves: a changed constant would otherwise change what the rest promises, and still verify.
pub open spec fn pinned() -> bool {
    &&& VMAX_MV == 8760
    &&& DEFAULT_CAP_MA == 2048
    &&& DESIGN_MAX_MA == 3776
    &&& TEMP_MIN_DK == 2732
    &&& TEMP_MAX_DK == 3182
    &&& BATTERY_MIN_MV == 5000
    &&& BATTERY_MAX_MV == 8860
    &&& V_REQ_MIN_MV == 8000
    &&& V_REQ_MAX_MV == 8900
    &&& PRECHARGE_BELOW_MV == 6200
    &&& PRECHARGE_MA == 512
    &&& SWELL_MAX_MV == 8198
    &&& HOT_MV == 8304
    &&& HOT_MA == 1856
    &&& COOL_MA == 1216
    &&& COLD_MA == 512
    &&& RECHARGE_AT_PERCENT == 97
    &&& FROZEN_LIMIT_MS == 900_000
    &&& CHARGE_ALARMS == 0xd000
    &&& FULLY_CHARGED == 0x20
}

/// The stop rules all pass.
pub open spec fn safe_reading(r: Reading) -> bool {
    &&& TEMP_MIN_DK <= r.temp_dk <= TEMP_MAX_DK
    &&& r.status & CHARGE_ALARMS == 0
    &&& r.status & FULLY_CHARGED == 0
    &&& r.req_ma != 0
    &&& r.req_ma != 0xffff
    &&& V_REQ_MIN_MV <= r.req_mv <= V_REQ_MAX_MV
    &&& BATTERY_MIN_MV <= r.battery_mv <= BATTERY_MAX_MV
}

/// The limits on a setpoint: never over what the battery asks, never over the design or the caller's cap, on the registers' steps, and
/// inside the step the temperature calls for.
pub open spec fn within_limits(mv: u16, ma: u16, r: Reading, cap_ma: u16, band: Band) -> bool {
    &&& mv <= r.req_mv
    &&& mv <= VMAX_MV
    &&& ma <= r.req_ma
    &&& ma <= cap_ma
    &&& ma <= DESIGN_MAX_MA
    &&& mv % 8 == 0
    &&& ma % 64 == 0
    &&& (band == Band::Hot ==> mv <= HOT_MV && ma <= HOT_MA && r.battery_mv <= SWELL_MAX_MV)
    &&& (band == Band::Cold ==> ma <= COLD_MA && r.battery_mv <= SWELL_MAX_MV)
    &&& (band == Band::Cool ==> ma <= COOL_MA)
    &&& (r.battery_mv < PRECHARGE_BELOW_MV ==> ma <= PRECHARGE_MA)
}

}

/// MaxChargeVoltage for `mv`: rounded down to the register's 8 mV step.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= mv,
        r % 8 == 0,
        mv - r < 8,
))]
pub const fn voltage_reg(mv: u16) -> u16 {
    mv - mv % 8
}

/// ChargeCurrent for `ma`: rounded down to the register's 64 mA step.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= ma,
        r % 64 == 0,
        ma - r < 64,
))]
pub const fn current_reg(ma: u16) -> u16 {
    ma - ma % 64
}

#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= a,
        r <= b,
        r == a || r == b,
))]
const fn smaller(a: u16, b: u16) -> u16 {
    if a < b { a } else { b }
}

/// The step after `band` at `temp_dk`, with Samsung's edges (0.1 K, its `CheckBatteryTemp`).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        temp_dk > 3140 ==> r == Band::Hot,
        temp_dk < 2782 ==> r == Band::Cold,
        temp_dk <= 2881 ==> r != Band::Normal && r != Band::Hot,
        temp_dk >= 3122 ==> r == Band::Hot || r == Band::Normal,
))]
pub const fn next_band(band: Band, temp_dk: u16) -> Band {
    if temp_dk > 3140 {
        return Band::Hot;
    }
    if temp_dk < 2782 {
        return Band::Cold;
    }
    if temp_dk <= 2881 {
        return match band {
            Band::Cold if temp_dk < 2831 => Band::Cold,
            _ => Band::Cool,
        };
    }
    match band {
        Band::Hot if temp_dk >= 3122 => Band::Hot,
        Band::Cool if temp_dk <= 2930 => Band::Cool,
        Band::Hot | Band::Cool | Band::Cold | Band::Normal => Band::Normal,
    }
}

/// Whether the battery counts as full after this reading: from the moment the gauge says so until the gauge has stopped saying so and
/// the capacity is back at [`RECHARGE_AT_PERCENT`]. A capacity not read keeps it full.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        status & FULLY_CHARGED != 0 ==> r,
        full && capacity is None ==> r,
        full && (capacity matches Some(c) && c > RECHARGE_AT_PERCENT) ==> r,
        !full && status & FULLY_CHARGED == 0 ==> !r,
))]
pub const fn full_after(full: bool, status: u16, capacity: Option<u16>) -> bool {
    if status & FULLY_CHARGED != 0 {
        return true;
    }
    if !full {
        return false;
    }
    match capacity {
        Some(c) => c > RECHARGE_AT_PERCENT,
        None => true,
    }
}

/// Whether the gauge's numbers may still be believed while charging: they have changed within [`FROZEN_LIMIT_MS`]. A clock that runs
/// backwards counts as stale.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> last_change_ms <= now_ms && now_ms - last_change_ms <= FROZEN_LIMIT_MS,
))]
pub const fn gauge_fresh(now_ms: u64, last_change_ms: u64) -> bool {
    last_change_ms <= now_ms && now_ms - last_change_ms <= FROZEN_LIMIT_MS
}

/// Whether the gauge's current is more than the charger was told: 10% and one register step over, once it has had time to settle.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> measured_ma > 0,
        !r ==> measured_ma as int <= chip_ma as int + chip_ma as int / 10 + 64,
))]
pub const fn current_over(measured_ma: i16, chip_ma: u16) -> bool {
    let limit = chip_ma as i32 + (chip_ma / 10) as i32 + 64;
    measured_ma as i32 > limit
}

/// The stop rules and the limits. When several reasons hold, the one named is the temperature, a gauge alarm, fully charged, the
/// current, the voltage request, the battery voltage, swelling; the decision is the same.
#[cfg_attr(verus_keep_ghost, verus_spec(d =>
    ensures
        pinned(),
        d matches Decision::Charge { mv, ma } ==> safe_reading(*r) && within_limits(mv, ma, *r, cap_ma, band),
        safe_reading(*r) && !(band == Band::Hot || band == Band::Cold) ==> d is Charge,
))]
pub fn decide(r: &Reading, cap_ma: u16, band: Band) -> Decision {
    if r.temp_dk < TEMP_MIN_DK || r.temp_dk > TEMP_MAX_DK {
        return Decision::Stop(Stop::Temperature(r.temp_dk));
    }
    if r.status & CHARGE_ALARMS != 0 {
        return Decision::Stop(Stop::Alarm(r.status));
    }
    if r.status & FULLY_CHARGED != 0 {
        return Decision::Stop(Stop::FullyCharged);
    }
    if r.req_ma == 0 || r.req_ma == 0xffff {
        return Decision::Stop(Stop::Current(r.req_ma));
    }
    if r.req_mv < V_REQ_MIN_MV || r.req_mv > V_REQ_MAX_MV {
        return Decision::Stop(Stop::Voltage(r.req_mv));
    }
    if r.battery_mv < BATTERY_MIN_MV || r.battery_mv > BATTERY_MAX_MV {
        return Decision::Stop(Stop::BatteryVoltage(r.battery_mv));
    }
    let hot_or_cold = match band {
        Band::Hot | Band::Cold => true,
        Band::Cool | Band::Normal => false,
    };
    if hot_or_cold && r.battery_mv > SWELL_MAX_MV {
        return Decision::Stop(Stop::Swelling(r.battery_mv));
    }
    let (band_mv, band_ma) = match band {
        Band::Hot => (HOT_MV, HOT_MA),
        Band::Cold => (VMAX_MV, COLD_MA),
        Band::Cool => (VMAX_MV, COOL_MA),
        Band::Normal => (VMAX_MV, DESIGN_MAX_MA),
    };
    let mv = smaller(r.req_mv, band_mv);
    let mut ma = smaller(smaller(r.req_ma, cap_ma), band_ma);
    if r.battery_mv < PRECHARGE_BELOW_MV {
        ma = smaller(ma, PRECHARGE_MA);
    }
    Decision::Charge {
        mv: voltage_reg(mv),
        ma: current_reg(ma),
    }
}

/// A blocked round: a stop if the charger may be charging, else nothing or, when `refresh`, the original's stop on every pass.
#[cfg_attr(verus_keep_ghost, verus_spec(p =>
    ensures
        p.blocked == Some(why),
        !(p.action is Write),
        (f.charging || f.chip_ma != 0) && f.verified ==> p.action == (Action::Zero { announce: true }),
        !f.verified ==> p.action == Action::Nothing,
        !refresh && !(f.charging || f.chip_ma != 0) ==> p.action == Action::Nothing,
))]
const fn blocked(f: &Facts, why: Blocked, refresh: bool) -> Plan {
    let action = if !f.verified {
        Action::Nothing
    } else if f.charging || f.chip_ma != 0 {
        Action::Zero { announce: true }
    } else if refresh && f.due {
        Action::Zero { announce: false }
    } else {
        Action::Nothing
    };
    Plan {
        action,
        blocked: Some(why),
    }
}

/// What this round writes. A setpoint goes out only from here, only when every check holds, and only within the limits of [`decide`].
/// A charger that may be charging gets a stop instead, on every round until it reads 0, unless the chip's IDs were never seen.
#[cfg_attr(verus_keep_ghost, verus_spec(p =>
    ensures
        p.action matches Action::Write { mv, ma } ==> {
            &&& f.ids_known && f.healthy && !f.latched && f.watchdog_set && f.fresh && f.timer_left && f.window_open
            &&& !f.over_current && !f.full && f.due && f.adapter
            &&& f.chip_ma <= DEFAULT_CAP_MA && f.chip_mv <= VMAX_MV
            &&& safe_reading(*r)
            &&& within_limits(mv, ma, *r, f.cap_ma, f.band)
        },
        !f.verified ==> !(p.action is Zero),
        !f.healthy && f.ids_known ==> !(p.action is Write),
        !f.healthy && f.ids_known && !(f.charging || f.chip_ma != 0) ==> p.action == Action::Nothing,
))]
pub fn plan(f: &Facts, r: &Reading) -> Plan {
    if !f.ids_known {
        return blocked(f, Blocked::UnknownChip, false);
    }
    if f.latched {
        return blocked(f, Blocked::Latched, false);
    }
    if !f.healthy {
        return blocked(f, Blocked::Unhealthy, false);
    }
    if f.chip_ma > DEFAULT_CAP_MA || f.chip_mv > VMAX_MV {
        return blocked(f, Blocked::OverCaps, false);
    }
    if !f.watchdog_set {
        return blocked(f, Blocked::WatchdogNotSet, false);
    }
    if f.over_current {
        return blocked(f, Blocked::OverCurrent, false);
    }
    if !f.fresh {
        return blocked(f, Blocked::Stale, false);
    }
    if !f.adapter {
        return blocked(f, Blocked::NoAdapter, false);
    }
    if !f.window_open {
        return blocked(f, Blocked::Window, false);
    }
    if !f.timer_left {
        return blocked(f, Blocked::Timer, false);
    }
    match decide(r, f.cap_ma, f.band) {
        Decision::Stop(why) => blocked(f, Blocked::Stop(why), true),
        Decision::Charge { .. } if f.full => blocked(f, Blocked::WaitRecharge, true),
        Decision::Charge { mv, ma } if f.due => Plan {
            action: Action::Write { mv, ma },
            blocked: None,
        },
        Decision::Charge { .. } => Plan {
            action: Action::Nothing,
            blocked: None,
        },
    }
}

/// The gauge's temperature, in tenths of a kelvin, in tenths of a degree Celsius, as Linux's `sbs-battery.c` gives it
/// (`TEMP_KELVIN_TO_CELSIUS`, 2731). For the health line of patch 1058.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == temp_dk as int - 2731,
))]
pub fn deci_celsius(temp_dk: u16) -> i32 {
    temp_dk as i32 - 2731
}

/// How long the adapter may be in without current into the battery before the status says so: the PD negotiation and the
/// first charge write take well under this (owner, 2026-11-08: "on Windows it's instant").
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const DRY_LIMIT_MS: u64 = 5000;

/// What the battery's status shows, Windows-like: it follows the cable at once.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Charging,
    /// The adapter is in but no current has gone into the battery for [`DRY_LIMIT_MS`]: a fault, or held back.
    NotCharging,
    Discharging,
    Full,
}

/// The status while the charger's AC_STAT is known: with the adapter in it is Charging at once (Full when the gauge says
/// full), and Not charging only after [`DRY_LIMIT_MS`] without a positive battery current; with the adapter out it is
/// Discharging at once, whatever the gauge still says.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        !online ==> r == Shown::Discharging,
        online && full ==> r == Shown::Full,
        online && !full && current_ma > 0 ==> r == Shown::Charging,
        r == Shown::NotCharging ==> online && !full && current_ma <= 0 && dry_ms >= DRY_LIMIT_MS,
        r == Shown::Charging ==> online,
        online && !full && dry_ms < DRY_LIMIT_MS ==> r == Shown::Charging,
        online && !full && current_ma <= 0 && dry_ms >= DRY_LIMIT_MS ==> r == Shown::NotCharging,
))]
pub fn shown(online: bool, full: bool, current_ma: i16, dry_ms: u64) -> Shown {
    if !online {
        Shown::Discharging
    } else if full {
        Shown::Full
    } else if current_ma > 0 || dry_ms < DRY_LIMIT_MS {
        Shown::Charging
    } else {
        Shown::NotCharging
    }
}

/// Whether the adapter counts as in: the PD controller's cable state as soon as it reports an attach or a detach (the owner,
/// 2026-11-08: "set it when the laptop sees the cable, don't wait for the power to be negotiated"), else the charger's AC_STAT.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        cable.is_some() ==> r == cable,
        cable.is_none() ==> r == ac,
))]
pub fn cable_online(cable: Option<bool>, ac: Option<bool>) -> Option<bool> {
    match cable {
        Some(c) => Some(c),
        None => ac,
    }
}
