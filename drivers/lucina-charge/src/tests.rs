//! The rules, the control loop against the simulated gauge and charger (`sim.rs`), the `power_supply` view, and the
//! bus adapter against a model of a GENI engine.

extern crate std;

use std::collections::VecDeque;
use std::string::{String, ToString};
use std::vec::Vec;
use std::{format, vec};

use crate::control::{BACKOFF_MS, Controller, READ_PERIOD_MS, Setpoint, Tick};
use crate::geni::GeniBus;
use crate::sim::{Board, Op};
use crate::supply::{AttrError, BATTERY_ATTRS, Snapshot};
use crate::*;

const V: u8 = bq25710::MAX_CHARGE_VOLTAGE;
const I: u8 = bq25710::CHARGE_CURRENT;
const C: u16 = bq25710::ADDR;

fn reading() -> Reading {
    Reading {
        status: 0x00c0,
        temp_dk: 2732 + 245,
        battery_mv: 7877,
        req_ma: 6916,
        req_mv: 8860,
    }
}

/// Runs `c` for `secs`, a tick whenever one is due (the first at once), the clock moving one second at a time. Every
/// run also holds the caps on every write there was.
fn run(c: &mut Controller, b: &mut Board, secs: u64) -> (Vec<String>, Option<Tick>) {
    run_with(c, b, secs, |_, _| {})
}

fn run_with(
    c: &mut Controller,
    b: &mut Board,
    secs: u64,
    mut each: impl FnMut(u64, &mut Board),
) -> (Vec<String>, Option<Tick>) {
    let mut lines = Vec::new();
    let mut last = None;
    let mut next = b.now_ms;
    for s in 0..secs {
        each(s, b);
        if b.now_ms >= next {
            let now = b.now_ms;
            let tick = c.tick(b, now, &mut |a| lines.push(format!("charge: {a}")));
            next = now + tick.next_ms;
            last = Some(tick);
        }
        b.advance(1000);
    }
    assert_caps(b);
    (lines, last)
}

fn assert_caps(b: &Board) {
    for op in &b.log {
        if let Op::Write(addr, reg, value) = *op {
            assert_eq!(addr, C, "only the charger is ever written");
            match reg {
                bq25710::CHARGE_CURRENT => assert!(value <= 2048 && value % 64 == 0, "{value} mA"),
                bq25710::MAX_CHARGE_VOLTAGE => {
                    assert!(
                        (8000..=8760).contains(&value) && value % 8 == 0,
                        "{value} mV"
                    )
                }
                // ADCOption: one-shot, started, and the range and reserved bits of the sim's reset value.
                0x35 => assert_eq!(value & 0xff00, 0x6000, "ADCOption 0x{value:04x}"),
                _ => panic!("register 0x{reg:02x} written"),
            }
        }
    }
}

fn snapshot_attr(s: &Snapshot, name: &str) -> Result<String, AttrError> {
    let mut text = String::new();
    s.write_battery_attr(name, &mut text)?;
    Ok(text)
}

// ---- The rules ----

#[test]
fn register_math_matches_the_original() {
    assert_eq!(bq25710::voltage_reg(8760), 0x2238);
    assert_eq!(bq25710::voltage_reg(8767), 8760);
    assert_eq!(bq25710::voltage_reg(8759), 8752);
    assert_eq!(bq25710::current_reg(2048), 0x0800);
    assert_eq!(bq25710::current_reg(2047), 1984);
    assert_eq!(bq25710::current_reg(63), 0);
}

#[test]
fn debian_reading_gives_debian_values() {
    assert_eq!(
        decide(&reading(), DEFAULT_CAP_MA, Band::Normal),
        Decision::Charge { mv: 8760, ma: 2048 }
    );
}

#[test]
fn every_refusal_rule() {
    type Change = fn(&mut Reading);
    let cases: [(Change, Stop); 15] = [
        (|r| r.status |= sbs::FULLY_CHARGED, Stop::FullyCharged),
        (|r| r.temp_dk = 2731, Stop::Temperature(2731)), // -0.1 C
        (|r| r.temp_dk = 2732 - 50, Stop::Temperature(2682)),
        (|r| r.temp_dk = 2732 + 451, Stop::Temperature(3183)), // 45.1 C
        (|r| r.temp_dk = 0, Stop::Temperature(0)),
        (|r| r.temp_dk = 0xffff, Stop::Temperature(0xffff)),
        (|r| r.req_ma = 0, Stop::Current(0)),
        (|r| r.req_ma = 0xffff, Stop::Current(0xffff)),
        (|r| r.req_mv = 7999, Stop::Voltage(7999)),
        (|r| r.req_mv = 8901, Stop::Voltage(8901)),
        (|r| r.req_mv = 0, Stop::Voltage(0)),
        (|r| r.req_mv = 0xffff, Stop::Voltage(0xffff)),
        (|r| r.status |= 0x8000, Stop::Alarm(0x80c0)),
        (|r| r.status |= 0x4000, Stop::Alarm(0x40c0)),
        (|r| r.status |= 0x1000, Stop::Alarm(0x10c0)),
    ];
    for (change, want) in cases {
        let mut r = reading();
        change(&mut r);
        assert_eq!(decide(&r, DEFAULT_CAP_MA, Band::Normal), Decision::Stop(want), "{r:?}");
    }
    for mv in [0, 4999, 8861, 0xffff] {
        let mut r = reading();
        r.battery_mv = mv;
        assert_eq!(
            decide(&r, DEFAULT_CAP_MA, Band::Normal),
            Decision::Stop(Stop::BatteryVoltage(mv))
        );
    }
}

#[test]
fn the_edges_of_the_rules_still_charge() {
    for change in [
        (|r| r.temp_dk = 2732) as fn(&mut Reading), // 0.0 C
        |r| r.temp_dk = 2732 + 450,                 // 45.0 C
        |r| r.req_mv = 8000,
        |r| r.req_mv = 8900,
        |r| r.req_ma = 0xfffe,
        |r| r.battery_mv = 5000,
        |r| r.battery_mv = 8860,
        |r| r.status = 0x0f80, // the charge alarms are the only ones that stop: discharge alarms and error codes do not
    ] {
        let mut r = reading();
        change(&mut r);
        assert!(
            matches!(decide(&r, DEFAULT_CAP_MA, Band::Normal), Decision::Charge { .. }),
            "{r:?}"
        );
    }
}

/// Whatever the gauge says, a request that charges stays inside the caps on the registers' steps. All 2^16 current and
/// voltage requests, crossed with the corners of the other.
#[test]
fn no_request_passes_the_caps() {
    let check = |req_mv: u16, req_ma: u16| {
        let r = Reading {
            req_mv,
            req_ma,
            ..reading()
        };
        if let Decision::Charge { mv, ma } = decide(&r, DEFAULT_CAP_MA, Band::Normal) {
            assert!(Setpoint::checked(mv, ma).is_some(), "{mv} mV, {ma} mA");
            assert!((8000..=8760).contains(&mv), "{mv} mV");
            assert!(ma <= 2048, "{ma} mA");
        }
    };
    for v in 0..=u16::MAX {
        for ma in [1, 63, 64, 2047, 2048, 2049, 6916, 0xfffe] {
            check(v, ma);
        }
        for mv in [8000, 8007, 8759, 8760, 8761, 8768, 8860, 8900] {
            check(mv, v);
        }
    }
}

#[test]
fn setpoints_outside_the_caps_do_not_exist() {
    assert!(Setpoint::checked(8760, 2048).is_some());
    assert!(Setpoint::checked(8768, 2048).is_none());
    assert!(Setpoint::checked(8760, 2112).is_none());
    assert!(Setpoint::checked(8759, 2048).is_none());
    assert!(Setpoint::checked(8760, 2047).is_none());
}

#[test]
fn mode_defaults_to_on_and_any_report_or_off_wins() {
    let mode = |s: &str| Mode::from_args(s.split_ascii_whitespace());
    assert_eq!(mode(""), Mode::On);
    assert_eq!(mode("quiet lucina.hid=report"), Mode::On);
    assert_eq!(mode("lucina.charge=report"), Mode::Report);
    assert_eq!(mode("lucina.charge=on"), Mode::On);
    assert_eq!(mode("lucina.charge=off"), Mode::Off);
    assert_eq!(mode("lucina.charge=ON"), Mode::Report);
    assert_eq!(mode("lucina.charge="), Mode::Report);
    assert_eq!(mode("lucina.charge=on lucina.charge=report"), Mode::Report);
    assert_eq!(mode("lucina.charge=report lucina.charge=on"), Mode::Report);
    assert_eq!(mode("lucina.charge=on lucina.charge=off"), Mode::Off);
    // After "--" the arguments are the init's.
    assert_eq!(mode("-- lucina.charge=report"), Mode::On);
    assert_eq!(mode("lucina.charge=on -- lucina.charge=off"), Mode::On);
}

// ---- report: reads, prints, writes nothing ----

#[test]
fn report_prints_every_step_and_writes_nothing() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::Report);
    let (lines, _) = run(&mut c, &mut b, 1);
    assert_eq!(
        lines,
        [
            "charge: charger 0x09: ManufacturerID 0x0040, DeviceID 0x0089 (BQ25710)",
            "charge: gauge is \"?\" by \"?\", ?, design 7720 mV, spec 0x0031, made 0x5000",
            "charge: gauge 0x0b: status 0x00c0, 24 C, battery 7877 mV, asks 8860 mV 6916 mA",
            "charge: battery: 30%, Charging, 7877 mV, 0 mA, 24.6 C, 1500/5000 mAh of 5320 design, 12 cycles, adapter in", // adapter just in: Charging at once (rules::shown)
            "charge: charger now: ChargeOption0 0xe70e, ChargeCurrent 0 mA, MaxChargeVoltage 8760 mV, ChargerStatus 0x8000",
            "charge: charger also: ChargeOption1 0x0200 ChargeOption2 0x02b7 ChargeOption3 0x0031 ProchotOption0 0x126d ProchotOption1 0x312c InputVoltage 0x0500 MinSystemVoltage 0x1800 IIN_HOST 0x3c00 IIN_DPM 0x4100 ProchotStatus 0x0000 ",
            "charge: report: would write MaxChargeVoltage 8760 mV (0x2238), ChargeCurrent 2048 mA (0x0800); nothing written",
        ]
    );
    assert!(b.writes().is_empty());
}

#[test]
fn report_never_writes_whatever_happens() {
    let scenarios: [fn(&mut Board); 13] = [
        |_| {},
        |b| b.gauge.status |= sbs::FULLY_CHARGED,
        |b| b.gauge.temp_dk = 2732 - 50,
        |b| b.gauge.temp_dk = 2732 + 500,
        |b| b.gauge.req_ma = 0,
        |b| b.gauge.req_ma = 0xffff,
        |b| b.gauge.req_mv = 7000,
        |b| b.gauge.req_mv = 9000,
        |b| b.charger.device_id = 0x0088,
        |b| b.charger.charge_current = 4096, // over the cap: an `on` would stop it
        |b| b.charger.option0 = 0x870e,      // the watchdog off
        |b| b.fail_reads_of = Some((sbs::ADDR, sbs::VOLTAGE)),
        |b| b.offline = true,
    ];
    for scenario in scenarios {
        let mut b = Board::new();
        scenario(&mut b);
        let mut c = Controller::new(Mode::Report);
        let (lines, _) = run(&mut c, &mut b, 3600);
        assert!(b.writes().is_empty(), "{lines:?}");
        assert!(!lines.is_empty());
    }
}

#[test]
fn report_says_what_it_would_stop() {
    let mut b = Board::new();
    b.charger.charge_current = 512; // the firmware left it charging
    b.charger.last_kick_ms = 1 << 40;
    b.gauge.temp_dk = 2732 + 500;
    let mut c = Controller::new(Mode::Report);
    let (lines, _) = run(&mut c, &mut b, 1);
    assert!(
        lines
            .iter()
            .any(|l| l == "charge: stop: temperature 50 C (battery 7877 mV, status 0x00c0)"),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l == "charge: report: would write ChargeCurrent 0; nothing written"),
        "{lines:?}"
    );
    assert!(b.writes().is_empty());
}

#[test]
fn report_prints_again_every_minute() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::Report);
    let (lines, _) = run(&mut c, &mut b, 180);
    let would = lines.iter().filter(|l| l.contains("would write")).count();
    assert_eq!(would, 3, "{lines:?}");
}

// ---- off: the bus is left alone ----

#[test]
fn off_touches_nothing() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::Off);
    let mut lines = Vec::new();
    let tick = c.tick(&mut b, 0, &mut |a| lines.push(a.to_string()));
    assert_eq!(tick.snapshot, Snapshot::EMPTY);
    assert!(b.log.is_empty());
    assert!(lines.is_empty());
}

// ---- on: the schedule ----

#[test]
fn on_writes_voltage_then_current_after_reading_ids_and_gauge() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 1);
    use Op::{Read, Write};
    let first_write = b.log.iter().position(|o| matches!(o, Write(..))).unwrap();
    // The IDs come first of all, and the gauge's requests before any write.
    assert_eq!(b.log[..2], [Read(C, 0xfe), Read(C, 0xff)]);
    for reg in [
        sbs::BATTERY_STATUS,
        sbs::TEMPERATURE,
        sbs::VOLTAGE,
        sbs::CHARGING_CURRENT,
        sbs::CHARGING_VOLTAGE,
    ] {
        assert!(b.log[..first_write].contains(&Read(sbs::ADDR, reg)));
    }
    assert!(b.log[..first_write].contains(&Read(C, bq25710::CHARGE_OPTION_0)));
    assert_eq!(
        b.log[first_write..],
        [
            Write(C, V, 0x2238),
            Read(C, V),
            Write(C, I, 0x0800),
            Read(C, I),
            Read(C, bq25710::CHARGER_STATUS),
        ]
    );
    assert!(lines.contains(
        &"charge: 8760 mV, 2048 mA (gauge asks 8860 mV 6916 mA; battery 7877 mV, 24 C, charger status 0x8400)"
            .to_string()
    ), "{lines:?}");
    assert_eq!(b.charger.charge_current, 2048);
}

#[test]
fn caps_and_rounding_reach_the_registers() {
    let mut b = Board::new();
    b.gauge.req_mv = 8500;
    b.gauge.req_ma = 1000;
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    assert_eq!(b.writes(), [Op::Write(C, V, 8496), Op::Write(C, I, 960)]);
}

#[test]
fn eight_hours_on_a_small_battery_have_no_watchdog_stop_and_a_write_a_minute() {
    let mut b = Board::new();
    b.full_mah = 60_000; // never fills in eight hours at 2 A... almost
    b.charge_uah = 0;
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 8 * 3600);
    assert_eq!(b.charger.expired_at_ms, None);
    assert!(b.charging());
    let writes = b.writes();
    assert_eq!(writes.len(), 2 * (8 * 60), "{}", writes.len());
    // Steady state prints the battery's percent, not the same lines again.
    assert!(lines.len() < 100, "{}", lines.len());
    let charge_lines = lines.iter().filter(|l| l.contains("gauge asks")).count();
    assert_eq!(charge_lines, 1);
}

#[test]
fn charges_to_full_then_stops_and_the_view_says_full() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    let (lines, tick) = run(&mut c, &mut b, 3 * 3600);
    let s = tick.unwrap().snapshot;
    assert_eq!(b.charger.charge_current, 0, "{lines:?}");
    assert_eq!(snapshot_attr(&s, "status").unwrap(), "Full");
    assert_eq!(snapshot_attr(&s, "capacity").unwrap(), "100");
    assert!(lines.iter().any(|l| l.contains("stop: fully charged")));
}

#[test]
fn a_stopped_charger_is_written_zero_every_pass_like_the_original() {
    let mut b = Board::new();
    b.gauge.status |= sbs::FULLY_CHARGED;
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 180);
    assert_eq!(b.writes(), [Op::Write(C, I, 0); 3]);
}

/// Each stop rule, arriving while the charger charges at 2 A: ChargeCurrent = 0 on the next tick, within 10 s.
#[test]
fn stop_rules_act_within_a_tick_while_charging() {
    let rules: [fn(&mut Board); 9] = [
        |b| b.gauge.status |= sbs::FULLY_CHARGED,
        |b| b.gauge.temp_dk = 2731,
        |b| b.gauge.temp_dk = 2732 + 451,
        |b| b.gauge.req_ma = 0,
        |b| b.gauge.req_ma = 0xffff,
        |b| b.gauge.req_mv = 7999,
        |b| b.gauge.req_mv = 8901,
        |b| b.gauge.status |= 0x4000,
        |b| b.gauge.battery_mv = 9000,
    ];
    for rule in rules {
        let mut b = Board::new();
        let mut c = Controller::new(Mode::On);
        let (_, _) = run(&mut c, &mut b, 30);
        assert!(b.charging());
        rule(&mut b);
        let (lines, _) = run(&mut c, &mut b, 10);
        assert_eq!(b.charger.charge_current, 0, "{lines:?}");
        assert_eq!(b.writes().last(), Some(&Op::Write(C, I, 0)));
        assert!(
            lines.iter().any(|l| l.contains("charge: stop: ")),
            "{lines:?}"
        );
    }
}

#[test]
fn charging_resumes_at_the_next_pass_when_the_reason_goes_away() {
    let mut b = Board::new();
    b.gauge.temp_dk = 2732 + 500;
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(!b.charging());
    b.gauge.temp_dk = 2732 + 300;
    run(&mut c, &mut b, 60);
    assert_eq!(b.charger.charge_current, 2048);
}

// ---- on: refusals ----

#[test]
fn wrong_ids_write_nothing_ever() {
    for (manufacturer, device) in [(0x0040, 0x0088), (0x0041, 0x0089), (0xffff, 0xffff), (0, 0)] {
        let mut b = Board::new();
        b.charger.manufacturer_id = manufacturer;
        b.charger.device_id = device;
        let mut c = Controller::new(Mode::On);
        let (lines, tick) = run(&mut c, &mut b, 3600);
        assert!(b.writes().is_empty(), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("not a BQ25710: nothing is written to it"))
        );
        // The battery is still shown.
        assert_eq!(tick.unwrap().snapshot.capacity, Some(30));
    }
}

#[test]
fn unreadable_ids_write_nothing() {
    for reg in [bq25710::MANUFACTURER_ID, bq25710::DEVICE_ID] {
        let mut b = Board::new();
        b.fail_reads_of = Some((C, reg));
        let mut c = Controller::new(Mode::On);
        let (lines, _) = run(&mut c, &mut b, 600);
        assert!(b.writes().is_empty(), "{lines:?}");
        assert!(
            lines[0].starts_with(&format!("charge: error: read 0x09 reg 0x{reg:02x}: ")),
            "{lines:?}"
        );
    }
}

#[test]
fn ids_changing_while_charging_set_current_zero_once() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(b.charging());
    let before = b.writes().len();
    b.charger.device_id = 0xffff;
    let (lines, _) = run(&mut c, &mut b, 600);
    assert_eq!(b.writes()[before..], [Op::Write(C, I, 0)], "{lines:?}");
    assert!(!b.charging());
}

#[test]
fn any_unreadable_gauge_word_writes_nothing_if_not_charging() {
    for reg in [
        sbs::BATTERY_STATUS,
        sbs::TEMPERATURE,
        sbs::VOLTAGE,
        sbs::CHARGING_CURRENT,
        sbs::CHARGING_VOLTAGE,
    ] {
        let mut b = Board::new();
        b.fail_reads_of = Some((sbs::ADDR, reg));
        let mut c = Controller::new(Mode::On);
        let (lines, _) = run(&mut c, &mut b, 600);
        assert!(b.writes().is_empty(), "{lines:?}");
    }
}

#[test]
fn an_unreadable_gauge_while_charging_sets_current_zero_at_once_then_resumes() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(b.charging());
    b.fail_reads_of = Some((sbs::ADDR, sbs::TEMPERATURE));
    let (lines, _) = run(&mut c, &mut b, 10);
    assert_eq!(b.charger.charge_current, 0, "{lines:?}");
    assert!(
        lines.iter().any(|l| l == "charge: wrote ChargeCurrent 0"),
        "{lines:?}"
    );
    b.fail_reads_of = None;
    run(&mut c, &mut b, 10);
    assert_eq!(b.charger.charge_current, 2048);
}

#[test]
fn a_charger_the_firmware_left_charging_is_stopped_when_the_gauge_goes_silent() {
    let mut b = Board::new();
    b.charger.charge_current = 512;
    b.charger.last_kick_ms = 100_000;
    b.fail_reads_of = Some((sbs::ADDR, sbs::VOLTAGE));
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    assert_eq!(b.writes(), [Op::Write(C, I, 0)]);
}

#[test]
fn a_gauge_nack_is_retried_and_does_not_stop_charging() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    let before = b.writes().len();
    b.nack_next = 2;
    let (lines, _) = run(&mut c, &mut b, 10);
    assert_eq!(b.writes().len(), before, "{lines:?}");
    assert!(b.charging());
    assert!(!lines.iter().any(|l| l.contains("error")));
}

#[test]
fn a_timeout_is_not_retried_and_a_dead_bus_is_polled_slowly() {
    let mut b = Board::new();
    b.offline = true;
    let mut c = Controller::new(Mode::On);
    let (lines, tick) = run(&mut c, &mut b, 600);
    // One read per tick: three at 10 s, then one a minute.
    let reads = b.log.iter().filter(|o| matches!(o, Op::Read(..))).count();
    assert!((3..=3 + 10).contains(&reads), "{reads}");
    assert_eq!(tick.unwrap().next_ms, BACKOFF_MS);
    // Printed once, not every time.
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(b.writes().is_empty());
}

#[test]
fn a_bus_that_dies_while_charging_leaves_it_to_the_watchdog_within_175_s() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(b.charging());
    let last = b.charger.last_kick_ms;
    b.offline = true;
    let (lines, tick) = run(&mut c, &mut b, 400);
    assert_eq!(b.charger.expired_at_ms, Some(last + 175_000), "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.contains("watchdog stops charging within 175 s"))
    );
    assert_eq!(tick.unwrap().snapshot, Snapshot::EMPTY);
}

#[test]
fn a_kernel_that_stops_ticking_leaves_it_to_the_watchdog_within_175_s() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(b.charging());
    let last = b.charger.last_kick_ms;
    b.advance(175_000);
    assert!(!b.charging());
    assert_eq!(b.charger.expired_at_ms, Some(last + 175_000));
}

#[test]
fn a_watchdog_that_is_not_set_refuses_to_charge() {
    for option0 in [0x870e, 0xa70e, 0xc70e] {
        let mut b = Board::new();
        b.charger.option0 = option0;
        let mut c = Controller::new(Mode::On);
        let (lines, _) = run(&mut c, &mut b, 300);
        assert!(b.writes().is_empty(), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("the 175 s watchdog is not set"))
        );
    }
}

#[test]
fn an_unreadable_watchdog_refuses_to_charge() {
    let mut b = Board::new();
    b.fail_reads_of = Some((C, bq25710::CHARGE_OPTION_0));
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 300);
    assert!(b.writes().is_empty(), "{lines:?}");
}

#[test]
fn a_charger_holding_more_than_the_caps_is_stopped_and_latched() {
    for over in [
        (|b| b.charger.charge_current = 2112) as fn(&mut Board),
        |b| b.charger.max_charge_voltage = 8768,
    ] {
        let mut b = Board::new();
        let mut c = Controller::new(Mode::On);
        run(&mut c, &mut b, 30);
        over(&mut b);
        let (lines, _) = run(&mut c, &mut b, 10);
        assert_eq!(b.charger.charge_current, 0, "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("over the caps")),
            "{lines:?}"
        );
        // Latched: nothing but 0 from now on, even if the chip looks fine again.
        b.charger.max_charge_voltage = 8760;
        let before = b.writes().len();
        let (lines, _) = run(&mut c, &mut b, 600);
        assert!(
            b.writes()[before..]
                .iter()
                .all(|w| *w == Op::Write(C, I, 0))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("refusing to charge until reboot")),
            "{lines:?}"
        );
    }
}

#[test]
fn a_voltage_that_does_not_read_back_never_gets_a_current() {
    let mut b = Board::new();
    b.charger.max_charge_voltage = 8400;
    b.ignore_voltage_writes = true;
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 600);
    assert_eq!(b.charger.charge_current, 0, "{lines:?}");
    assert_eq!(
        b.writes()[..2],
        [Op::Write(C, V, 0x2238), Op::Write(C, I, 0)],
        "{lines:?}"
    );
    // Latched: the voltage is never tried again, and no current ever went out.
    assert!(
        b.writes()
            .iter()
            .filter(|w| matches!(w, Op::Write(_, V, _)))
            .count()
            == 1
    );
    assert!(
        !b.writes()
            .iter()
            .any(|w| matches!(w, Op::Write(_, I, v) if *v != 0))
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("register 0x15 written 0x2238, reads 0x20d0")),
        "{lines:?}"
    );
}

#[test]
fn a_current_that_reads_back_higher_is_stopped_and_latched() {
    let mut b = Board::new();
    b.current_reads_as = Some(4096);
    let mut c = Controller::new(Mode::On);
    // Reads over the cap before any write: latched at the first tick, and the current never goes up.
    let (lines, _) = run(&mut c, &mut b, 600);
    assert!(
        !b.writes()
            .iter()
            .any(|w| matches!(w, Op::Write(_, I, v) if *v != 0)),
        "{lines:?}"
    );

    let mut b = Board::new();
    b.gauge.req_ma = 1000; // 960 mA asked, reads 1024: under the cap, over what was written
    b.current_reads_as = Some(1024);
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 1);
    assert_eq!(b.charger.charge_current, 0, "{lines:?}");
    assert_eq!(b.writes().last(), Some(&Op::Write(C, I, 0)));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("register 0x14 written 0x03c0, reads 0x0400")),
        "{lines:?}"
    );
}

#[test]
fn a_failed_write_tries_to_stop_and_writes_no_current_after_a_failed_voltage() {
    let mut b = Board::new();
    b.fail_writes_of = Some((C, V));
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 1);
    assert_eq!(b.writes(), [Op::Write(C, V, 0x2238)], "{lines:?}");
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("charge: error: write 0x09 reg 0x15: "))
    );

    let mut b = Board::new();
    b.fail_writes_of = Some((C, I));
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 1);
    // A failed current write may have reached the chip: 0 is tried, and fails here too.
    assert_eq!(
        b.writes(),
        [
            Op::Write(C, V, 0x2238),
            Op::Write(C, I, 2048),
            Op::Write(C, I, 0)
        ],
        "{lines:?}"
    );
}

#[test]
fn an_adapter_pulled_is_not_an_error() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    b.charger.ac = false;
    b.charger.charge_current = 0;
    b.current_override = Some(-1500);
    let (lines, tick) = run(&mut c, &mut b, 120);
    let s = tick.unwrap().snapshot;
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("error") || l.contains("unexpected")),
        "{lines:?}"
    );
    assert_eq!(snapshot_attr(&s, "status").unwrap(), "Discharging");
    assert_eq!(snapshot_attr(&s, "current_now").unwrap(), "-1500000");
    assert_eq!(s.ac_online, Some(false));
    // Back in: charging resumes at the next pass.
    b.charger.ac = true;
    b.current_override = None;
    run(&mut c, &mut b, 60);
    assert_eq!(b.charger.charge_current, 2048);
}

// ---- The power_supply view ----

#[test]
fn view_is_in_linux_units() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::Report);
    let (_, tick) = run(&mut c, &mut b, 1);
    let s = tick.unwrap().snapshot;
    for (name, want) in [
        ("type", "Battery"),
        ("status", "Charging"), // the adapter is in and the dry time is under 5 s
        ("present", "1"),
        ("technology", "Li-ion"),
        ("capacity", "30"),
        ("voltage_now", "7877000"),
        ("current_now", "0"),
        ("temp", "246"),
        ("charge_now", "1500000"),
        ("charge_full", "5000000"),
        ("charge_full_design", "5320000"),
        ("cycle_count", "12"),
    ] {
        assert_eq!(snapshot_attr(&s, name).unwrap(), want, "{name}");
    }
    for name in BATTERY_ATTRS {
        assert!(snapshot_attr(&s, name).is_ok(), "{name}");
    }
    let mut text = String::new();
    s.write_adapter_attr("online", &mut text).unwrap();
    assert_eq!(text, "1");
    let mut text = String::new();
    s.write_adapter_attr("type", &mut text).unwrap();
    assert_eq!(text, "Mains");
}

#[test]
fn view_shows_charging_while_charging() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    let (_, tick) = run(&mut c, &mut b, 20);
    let s = tick.unwrap().snapshot;
    assert_eq!(snapshot_attr(&s, "status").unwrap(), "Charging");
    assert_eq!(snapshot_attr(&s, "current_now").unwrap(), "2048000");
}

#[test]
fn view_has_no_data_before_a_reading_and_no_charges_in_energy_mode() {
    let s = Snapshot::EMPTY;
    assert_eq!(snapshot_attr(&s, "present").unwrap(), "0");
    assert_eq!(snapshot_attr(&s, "capacity"), Err(AttrError::NoData));
    assert_eq!(snapshot_attr(&s, "status"), Err(AttrError::NoData));
    assert_eq!(snapshot_attr(&s, "nonsense"), Err(AttrError::UnknownAttr));

    let mut b = Board::new();
    b.capacity_mode_bit = true;
    let mut c = Controller::new(Mode::Report);
    let (_, tick) = run(&mut c, &mut b, 1);
    let s = tick.unwrap().snapshot;
    assert_eq!(snapshot_attr(&s, "charge_now"), Err(AttrError::NoData));
    assert_eq!(snapshot_attr(&s, "capacity").unwrap(), "30");
}

#[test]
fn view_status_follows_sbs_battery() {
    let view = |status, current| Snapshot {
        battery_status: Some(status),
        current_ma: current,
        ..Snapshot::EMPTY
    };
    let text = |status, current| view(status, current).status_text();
    assert_eq!(text(0x00c0, Some(0)), Some("Not charging"));
    assert_eq!(text(0x00c0, Some(-800)), Some("Discharging"));
    assert_eq!(text(0x0080, Some(2067)), Some("Charging"));
    assert_eq!(text(0x00a0, Some(0)), Some("Full"));
    assert_eq!(text(0x00a0, Some(30)), Some("Charging"));
    assert_eq!(text(0x00e0, Some(-30)), Some("Discharging"));
    assert_eq!(text(0x00c0, None), Some("Discharging"));
    assert_eq!(view(0x00c0, None).capacity, None);
}

#[test]
fn capacity_over_100_shows_100() {
    let s = Snapshot {
        capacity: Some(103),
        ..Snapshot::EMPTY
    };
    assert_eq!(snapshot_attr(&s, "capacity").unwrap(), "100");
}

#[test]
fn a_read_period_is_ten_seconds() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::Report);
    let (_, tick) = run(&mut c, &mut b, 1);
    assert_eq!(tick.unwrap().next_ms, READ_PERIOD_MS);
    assert_eq!(READ_PERIOD_MS, 10_000);
}

// ---- The bus adapter on the GENI master ----

/// Just enough of a GENI engine running I2C firmware: commands execute at once against the simulated board, whose
/// register pointer the write message sets; `M_CMD0`'s `STOP_STRETCH` bit is recorded per command.
struct GeniModel {
    regs: Vec<u32>,
    board: Board,
    status: u32,
    rx: VecDeque<u32>,
    pending_write: Option<(u8, usize, Vec<u8>)>,
    pointer: Option<u8>,
    commands: Vec<(u32, bool)>,
    nack: bool,
}

struct Shared(std::rc::Rc<std::cell::RefCell<GeniModel>>);

impl lucina_geni::Regs for Shared {
    fn read32(&mut self, offset: usize) -> u32 {
        let mut m = self.0.borrow_mut();
        match offset {
            0x610 => m.status,
            0x804 => m.rx.len() as u32,
            0x780 => m.rx.pop_front().unwrap_or(0),
            _ => m.regs[offset / 4],
        }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        let mut m = self.0.borrow_mut();
        match offset {
            0x618 => m.status &= !value,
            0x600 => m.command(value),
            0x700 => m.tx(value),
            _ => m.regs[offset / 4] = value,
        }
    }
}

impl GeniModel {
    fn new(board: Board) -> Self {
        let mut regs = vec![0; 0x4000 / 4];
        regs[0x68 / 4] = (3 << 8) | 0x41;
        regs[0x48 / 4] = 0x21;
        regs[0x278 / 4] = 0x0050_3018;
        regs[0xe24 / 4] = (32 << 24) | (16 << 16);
        regs[0xe28 / 4] = (32 << 24) | (16 << 16);
        GeniModel {
            regs,
            board,
            status: 0,
            rx: VecDeque::new(),
            pending_write: None,
            pointer: None,
            commands: Vec::new(),
            nack: false,
        }
    }

    fn command(&mut self, cmd0: u32) {
        let opcode = cmd0 >> 27;
        let addr = ((cmd0 >> 9) & 0x7f) as u8;
        self.commands.push((opcode, cmd0 & (1 << 2) != 0));
        if self.nack {
            self.status |= (1 << 3) | (1 << 10); // CMD_FAILURE, GP_IRQ_1
            return;
        }
        match opcode {
            1 => {
                let len = self.regs[0x26c / 4] as usize;
                self.pending_write = Some((addr, len, Vec::new()));
                self.status |= 1 << 30; // TX watermark
            }
            2 => {
                let len = self.regs[0x270 / 4];
                if len != 2 {
                    // A block read: this model has no such thing, and says so as a chip would.
                    self.status |= (1 << 3) | (1 << 10);
                    return;
                }
                let reg = self.pointer.expect("a read with no register written first");
                let value = self.board.read_word(u16::from(addr), reg).unwrap();
                self.rx.push_back(u32::from(value));
                self.status |= (1 << 27) | 1; // RX_FIFO_LAST, CMD_DONE
            }
            _ => panic!("opcode {opcode}"),
        }
    }

    fn tx(&mut self, word: u32) {
        let (addr, len, mut data) = self.pending_write.take().expect("no write command");
        data.extend_from_slice(&word.to_le_bytes()[..(len - data.len()).min(4)]);
        if data.len() < len {
            self.pending_write = Some((addr, len, data));
            return;
        }
        match *data.as_slice() {
            [reg] => self.pointer = Some(reg),
            [reg, lo, hi] => {
                let value = u16::from_le_bytes([lo, hi]);
                self.board.write_word(u16::from(addr), reg, value).unwrap();
            }
            _ => panic!("write of {} bytes", data.len()),
        }
        self.status &= !(1 << 30);
        self.status |= 1;
    }
}

struct Clk(u64);

impl lucina_geni::Clock for Clk {
    fn now_us(&mut self) -> u64 {
        self.0 += 10;
        self.0
    }
}

fn geni_bus(
    board: Board,
) -> (
    GeniBus<Shared, Clk>,
    std::rc::Rc<std::cell::RefCell<GeniModel>>,
) {
    let model = std::rc::Rc::new(std::cell::RefCell::new(GeniModel::new(board)));
    let mut regs = Shared(model.clone());
    let probe = lucina_geni::Probe::read(&mut regs, 0x2000_0000);
    let i2c = lucina_geni::I2c::new(&mut regs, &probe, lucina_geni::Speed::Fast).unwrap();
    assert_eq!(i2c.timing(), lucina_geni::Timing::Firmware);
    (
        GeniBus {
            regs,
            clock: Clk(0),
            i2c,
        },
        model,
    )
}

#[test]
fn word_read_is_register_then_repeated_start_then_two_bytes() {
    let (mut bus, model) = geni_bus(Board::new());
    assert_eq!(bus.read_word(0x0b, sbs::CHARGING_VOLTAGE), Ok(8860));
    let m = model.borrow();
    // A write with STOP_STRETCH (no stop: the read follows on a repeated start), then a read that ends with a stop.
    assert_eq!(m.commands, [(1, true), (2, false)]);
    assert_eq!(m.board.log, [Op::Read(0x0b, sbs::CHARGING_VOLTAGE)]);
}

#[test]
fn word_write_is_register_then_low_byte_then_high_byte() {
    let (mut bus, model) = geni_bus(Board::new());
    assert_eq!(bus.write_word(C, V, 0x2238), Ok(()));
    let m = model.borrow();
    assert_eq!(m.commands, [(1, false)]);
    assert_eq!(m.board.log, [Op::Write(C, V, 0x2238)]);
}

#[test]
fn nack_is_retryable_and_timeout_is_not() {
    type B = GeniBus<Shared, Clk>;
    assert!(B::may_retry(&lucina_geni::Error::Nack { data: false }));
    assert!(B::may_retry(&lucina_geni::Error::ArbitrationLost));
    assert!(!B::may_retry(&lucina_geni::Error::Timeout));
    assert!(!B::may_retry(&lucina_geni::Error::Stuck));

    let (mut bus, model) = geni_bus(Board::new());
    model.borrow_mut().nack = true;
    model.borrow_mut().regs[0x910 / 4] = 0;
    let e = bus.read_word(0x0b, sbs::VOLTAGE);
    assert!(
        matches!(
            e,
            Err(lucina_geni::Error::Nack { .. }) | Err(lucina_geni::Error::Stuck)
        ),
        "{e:?}"
    );
}

/// The whole of a report round on the real I2C master code: IDs, gauge, and no write command with a data phase.
#[test]
fn report_round_over_the_geni_master() {
    let (mut bus, model) = geni_bus(Board::new());
    let mut c = Controller::new(Mode::Report);
    let mut lines = Vec::new();
    let tick = c.tick(&mut bus, 0, &mut |a| lines.push(a.to_string()));
    assert_eq!(tick.snapshot.capacity, Some(30));
    assert!(lines.iter().any(|l| l.contains("(BQ25710)")), "{lines:?}");
    assert!(model.borrow().board.writes().is_empty());
}

#[test]
fn on_round_over_the_geni_master_writes_the_values() {
    let (mut bus, model) = geni_bus(Board::new());
    let mut c = Controller::new(Mode::On);
    let mut lines = Vec::new();
    c.tick(&mut bus, 0, &mut |a| lines.push(a.to_string()));
    assert_eq!(
        model.borrow().board.charger.charge_current,
        2048,
        "{lines:?}"
    );
    assert_eq!(model.borrow().board.charger.max_charge_voltage, 8760);
}

// ---- Samsung's rules and the supervised test ----

#[test]
fn samsung_temperature_steps_with_their_own_ways_out() {
    let walk = |start, temps: &[u16]| {
        let mut band = start;
        temps.iter().map(|&t| { band = next_band(band, t); band }).collect::<Vec<_>>()
    };
    use Band::{Cold, Cool, Hot, Normal};
    assert_eq!(walk(Normal, &[2977, 3140, 3141, 3122, 3121]), [Normal, Normal, Hot, Hot, Normal]);
    assert_eq!(walk(Normal, &[2882, 2881, 2930, 2931]), [Normal, Cool, Cool, Normal]);
    assert_eq!(walk(Normal, &[2781, 2830, 2831, 2881, 2882]), [Cold, Cold, Cool, Cool, Cool]);
    assert_eq!(walk(Cold, &[2882]), [Normal]);
    assert_eq!(walk(Cool, &[2782, 2781]), [Cool, Cold]);
}

#[test]
fn temperature_steps_lower_the_setpoint() {
    let at = |band, battery_mv| {
        let r = Reading { battery_mv, ..reading() };
        decide(&r, DEFAULT_CAP_MA, band)
    };
    assert_eq!(at(Band::Hot, 7877), Decision::Charge { mv: 8304, ma: 1856 });
    assert_eq!(at(Band::Cool, 7877), Decision::Charge { mv: 8760, ma: 1216 });
    assert_eq!(at(Band::Cold, 7877), Decision::Charge { mv: 8760, ma: 512 });
    assert_eq!(at(Band::Normal, 6199), Decision::Charge { mv: 8760, ma: 512 });
    assert_eq!(at(Band::Hot, 8199), Decision::Stop(Stop::Swelling(8199)));
    assert_eq!(at(Band::Cold, 8199), Decision::Stop(Stop::Swelling(8199)));
}

#[test]
fn a_full_battery_waits_for_97_percent() {
    assert!(rules::full_after(false, 0x40e0, Some(99)));
    assert!(rules::full_after(true, 0x00c0, Some(98)));
    assert!(rules::full_after(true, 0x00c0, None));
    assert!(!rules::full_after(true, 0x00c0, Some(97)));
    assert!(!rules::full_after(false, 0x00c0, Some(100)));

    let mut b = Board::new();
    b.full_mah = 1000;
    b.charge_uah = 990_000;
    b.gauge.status = 0x40e0;
    b.gauge.req_ma = 0;
    b.gauge.req_mv = 0;
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 30);
    assert!(!b.charging());
    // The gauge lets go at 99%, and asks again: not yet.
    b.gauge.status = 0x00c0;
    b.gauge.req_ma = 6916;
    b.gauge.req_mv = 8860;
    b.log.clear();
    let (lines, _) = run(&mut c, &mut b, 120);
    assert!(!b.charging(), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("charges again at 97%")), "{lines:?}");
    assert!(!b.writes().iter().any(|op| matches!(op, Op::Write(_, _, v) if *v != 0)));
    b.charge_uah = 970_000;
    b.log.clear();
    run(&mut c, &mut b, 61);
    assert!(b.writes().contains(&Op::Write(C, I, 2048)), "{:?}", b.writes());
}

#[test]
fn unhealthy_kernel_writes_no_setpoint_and_the_watchdog_ends_charging() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    assert!(b.charging());
    let mut lines = Vec::new();
    // A failing machine that cannot even stop: the bus stays quiet about setpoints.
    b.log.clear();
    let now = b.now_ms;
    c.tick_checked(&mut b, now, false, &mut |a| lines.push(format!("{a}")));
    assert_eq!(b.writes(), [Op::Write(C, I, 0)]);
    assert!(!b.charging());
    for _ in 0..30 {
        b.advance(10_000);
        let now = b.now_ms;
        c.tick_checked(&mut b, now, false, &mut |a| lines.push(format!("{a}")));
    }
    assert_eq!(b.writes(), [Op::Write(C, I, 0)], "nothing feeds the watchdog");
    assert!(lines.iter().any(|l| l.contains("not healthy")), "{lines:?}");
    b.advance(10_000);
    let now = b.now_ms;
    c.tick_checked(&mut b, now, true, &mut |_| {});
    assert!(b.charging(), "health back, charging back");
}

#[test]
fn a_thread_held_up_refreshes_nothing_on_its_first_round_back() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    b.advance(200_000);
    assert!(!b.charging(), "the watchdog ran out");
    b.log.clear();
    let now = b.now_ms;
    c.tick(&mut b, now, &mut |_| {});
    assert!(b.writes().iter().all(|op| *op == Op::Write(C, I, 0)), "{:?}", b.writes());
    b.advance(10_000);
    let now = b.now_ms;
    c.tick(&mut b, now, &mut |_| {});
    assert!(b.charging());
}

#[test]
fn the_gauge_measuring_too_much_stops_and_latches() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    b.current_override = Some(2048 + 204 + 65);
    let (lines, _) = run(&mut c, &mut b, 100);
    assert!(!b.charging());
    assert!(lines.iter().any(|l| l.contains("the gauge measures 2317 mA")), "{lines:?}");
    b.current_override = None;
    run(&mut c, &mut b, 300);
    assert!(!b.charging(), "until reboot");
    // Just inside does not count.
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    b.current_override = Some(2048 + 204 + 64);
    run(&mut c, &mut b, 300);
    assert!(b.charging());
}

#[test]
fn a_gauge_that_stops_changing_while_charged_stops_and_latches() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    b.charge_uah = 0;
    b.full_mah = 60_000; // large enough that the fixed-point charge moves slowly
    // Everything the check looks at held still.
    b.capacity_mode_bit = true;
    b.current_override = Some(2000);
    let (lines, _) = run(&mut c, &mut b, 901);
    assert!(b.charging(), "{lines:?}");
    let (lines, _) = run(&mut c, &mut b, 30);
    assert!(!b.charging());
    assert!(lines.iter().any(|l| l.contains("have not changed")), "{lines:?}");
    b.gauge.battery_mv += 1;
    run(&mut c, &mut b, 300);
    assert!(!b.charging(), "until reboot");
}

#[test]
fn one_charge_has_a_limit_and_the_adapter_resets_it() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    b.full_mah = 60_000;
    let mut wobble = 0;
    let each = |_s: u64, b: &mut Board| {
        wobble += 1;
        b.gauge.battery_mv = 7877 + (wobble % 7);
    };
    let (lines, _) = run_with(&mut c, &mut b, 12 * 3600 + 30, each);
    assert!(!b.charging());
    assert!(lines.iter().any(|l| l.contains("lasted too long")));
    // Pulled and put back.
    b.charger.ac = false;
    b.charger.charge_current = 0;
    run(&mut c, &mut b, 20);
    b.charger.ac = true;
    run(&mut c, &mut b, 20);
    assert!(b.charging());
}

#[test]
fn a_plug_put_back_charges_on_the_next_tick_and_no_adapter_means_no_setpoint() {
    let mut b = Board::new();
    let mut c = Controller::new(Mode::On);
    run(&mut c, &mut b, 1);
    b.charger.ac = false;
    b.charger.charge_current = 0;
    b.log.clear();
    run(&mut c, &mut b, 300);
    assert!(b.writes().is_empty(), "{:?}", b.writes());
    b.charger.ac = true;
    run(&mut c, &mut b, 11);
    assert!(b.charging());
}

#[test]
fn the_cap_and_the_window_of_a_supervised_test() {
    let args = "lucina.charge=on lucina.charge.cap=512 lucina.charge.window=600 lucina.charge.trace=1000 -- lucina.charge.cap=9";
    let o = Options::from_args(args.split_ascii_whitespace());
    assert_eq!(o, Options { mode: Mode::On, cap_ma: 512, window_ms: Some(600_000), trace_ms: Some(1000), adc_ms: Some(1000), iin_on: true });
    let o = Options::from_args("lucina.charge.cap=9999 lucina.charge.trace=1".split_ascii_whitespace());
    assert_eq!(o, Options { mode: Mode::On, cap_ma: 2048, window_ms: None, trace_ms: Some(200), adc_ms: Some(1000), iin_on: true });
    let o = Options::from_args("lucina.charge.cap=x".split_ascii_whitespace());
    assert_eq!(o.cap_ma, 2048);

    let mut b = Board::new();
    let mut c = Controller::with_options(Options { mode: Mode::On, cap_ma: 512, window_ms: Some(600_000), trace_ms: Some(1000), adc_ms: Some(1000), iin_on: false });
    let (lines, tick) = run(&mut c, &mut b, 599);
    assert_eq!(tick.unwrap().next_ms, 1000);
    assert_eq!(b.charger.charge_current, 512);
    assert!(lines.iter().any(|l| l.contains("t 5000 ms: 24.6 C, 7877 mV")), "{lines:?}");
    let (lines, _) = run(&mut c, &mut b, 10);
    assert!(!b.charging());
    assert!(lines.iter().any(|l| l.contains("window is over")), "{lines:?}");
    b.log.clear();
    run(&mut c, &mut b, 200);
    assert!(b.writes().is_empty());
}

/// What `plan` promises, against every combination of the facts it looks at. Verus proves it; this runs it.
#[test]
fn plan_writes_only_when_every_check_holds() {
    for bits in 0u32..1 << 11 {
        let on = |n: u32| bits & (1 << n) != 0;
        let f = Facts {
            ids_known: on(0),
            verified: true,
            latched: !on(1),
            healthy: on(2),
            watchdog_set: on(3),
            chip_ma: 0,
            chip_mv: 8760,
            adapter: on(4),
            charging: false,
            over_current: !on(5),
            fresh: on(6),
            timer_left: on(7),
            window_open: on(8),
            full: !on(9),
            due: on(10),
            cap_ma: 2048,
            band: Band::Normal,
        };
        let p = plan(&f, &reading());
        let all = bits == (1 << 11) - 1;
        assert_eq!(matches!(p.action, Action::Write { .. }), all, "{f:?}");
    }
}

// ---- The charger's ADC ----

fn adc_controller(args: &str) -> Controller {
    Controller::with_options(Options::from_args(args.split_ascii_whitespace()))
}

/// One round, then one conversion; the lines printed and the wait closure's calls.
fn adc_round(c: &mut Controller, b: &mut Board) -> (Snapshot, Vec<String>, u32) {
    let mut lines = Vec::new();
    let now = b.now_ms;
    c.tick(b, now, &mut |a| lines.push(format!("charge: {a}")));
    let mut waits = 0;
    let s = c.adc(b, now, &mut |_| waits += 1, &mut |a| lines.push(format!("charge: {a}")));
    (s, lines, waits)
}

#[test]
fn start_value_keeps_range_and_reserved_bits() {
    for old in 0..=u16::MAX {
        for ch in [0x53, 0x73, 0xff] {
            let r = adc::start_value(old, ch);
            assert_eq!(r & 0x3f00, old & 0x3f00);
            assert_eq!(r & 0x8000, 0);
            assert_eq!(r & 0x4000, 0x4000);
            assert_eq!(r & 0xff, ch);
        }
    }
}

#[test]
fn decode_scales_like_the_datasheet() {
    let a = adc::Adc::decode(0xb900, 0x0700, 0x5d5b, false, false);
    assert_eq!((a.vbus_mv, a.iin_ma, a.vsys_mv, a.vbat_mv, a.psys_mv), (Some(15040), 350, 8832, 8704, None));
    assert_eq!(a.power_mw(), Some(5264));
    assert_eq!(adc::Adc::decode(0xb900, 0x0700, 0x5d5b, true, false).iin_ma, 175);
    let none = adc::Adc::decode(0x0010, 0x0700, 0x5d5b, false, true);
    assert_eq!((none.vbus_mv, none.power_mw(), none.psys_mv), (None, None, Some(192)));
}

#[test]
fn adc_converts_with_the_adapter_and_writes_only_adcoption() {
    let mut b = Board::new();
    let mut c = adc_controller("lucina.charge=report");
    let (s, lines, waits) = adc_round(&mut c, &mut b);
    assert_eq!(b.writes(), [Op::Write(C, 0x35, 0x6053)]);
    assert_eq!(b.adc_option, 0x2053);
    assert_eq!(waits, 2);
    let mut text = String::new();
    s.write_adapter_attr("lucina_adc", &mut text).unwrap();
    assert_eq!(text, "t_ms 0 vbus_mv 15040 iin_ma 350 power_mw 5264 vsys_mv 8832 vbat_mv 8704 psys_mv -");
    let mut text = String::new();
    s.write_adapter_attr("power_now", &mut text).unwrap();
    assert_eq!(text, "5264000");
    assert!(lines.iter().any(|l| l.contains("adc: VBUS 15040 mV, IIN 350 mA, 5264 mW in")), "{lines:?}");
    assert_caps(&b);
}

#[test]
fn adc_on_battery_in_low_power_writes_nothing() {
    let mut b = Board::new();
    b.charger.ac = false;
    let mut c = adc_controller("lucina.charge=report");
    let mut all = Vec::new();
    for _ in 0..5 {
        let (s, lines, _) = adc_round(&mut c, &mut b);
        assert!(s.adc.is_none());
        all.extend(lines);
        b.advance(1000);
    }
    assert!(b.writes().is_empty());
    assert_eq!(all.iter().filter(|l| l.contains("low power")).count(), 1, "{all:?}");
    let mut text = String::new();
    assert!(Snapshot::EMPTY.write_adapter_attr("power_now", &mut text).is_err());
}

#[test]
fn adc_never_feeds_the_watchdog() {
    let mut b = Board::new();
    let mut c = adc_controller("lucina.charge=on");
    let now = b.now_ms;
    c.tick(&mut b, now, &mut |_| {});
    assert!(b.charging());
    for _ in 0..300 {
        let now = b.now_ms;
        c.adc(&mut b, now, &mut |_| {}, &mut |_| {});
        b.advance(1000);
    }
    assert!(!b.charging());
    assert_eq!(b.charger.expired_at_ms, Some(175_000));
}

#[test]
fn adc_in_off_mode_touches_nothing() {
    let mut b = Board::new();
    let mut c = adc_controller("lucina.charge=off");
    assert_eq!(c.adc_period_ms(), None);
    let s = c.adc(&mut b, 0, &mut |_| {}, &mut |_| {});
    assert!(s.adc.is_none() && b.log.is_empty());
}

#[test]
fn adc_with_wrong_ids_touches_nothing() {
    let mut b = Board::new();
    b.charger.device_id = 0x0088;
    let mut c = adc_controller("lucina.charge=report");
    adc_round(&mut c, &mut b);
    assert!(!b.log.iter().any(|op| matches!(op, Op::Read(_, 0x35) | Op::Write(_, 0x35, _))));
}

#[test]
fn adc_timeout_gives_no_reading() {
    let mut b = Board::new();
    b.adc_never_done = true;
    let mut c = adc_controller("lucina.charge=report");
    let (s, lines, waits) = adc_round(&mut c, &mut b);
    assert!(s.adc.is_none());
    assert_eq!(waits, adc::POLLS);
    assert!(lines.iter().any(|l| l.contains("ADC_START still set after 120 ms")), "{lines:?}");
}

#[test]
fn adc_that_changes_a_charge_register_is_turned_off_for_good() {
    let mut b = Board::new();
    b.adc_changes_reg = Some((bq25710::MAX_CHARGE_VOLTAGE, 0x2000));
    let mut c = adc_controller("lucina.charge=report");
    let (s, lines, _) = adc_round(&mut c, &mut b);
    assert!(s.adc.is_none());
    assert!(lines.iter().any(|l| l.contains("ADC off until restart")), "{lines:?}");
    b.log.clear();
    for _ in 0..5 {
        adc_round(&mut c, &mut b);
        b.advance(1000);
    }
    assert!(!b.writes().iter().any(|op| matches!(op, Op::Write(_, 0x35, _))));
}

#[test]
fn adc_allows_chargecurrent_falling_to_zero() {
    let mut b = Board::new();
    let mut c = adc_controller("lucina.charge=on");
    let now = b.now_ms;
    c.tick(&mut b, now, &mut |_| {});
    assert!(b.charging());
    b.adc_changes_reg = Some((bq25710::CHARGE_CURRENT, 0));
    let s = c.adc(&mut b, now, &mut |_| {}, &mut |_| {});
    assert!(s.adc.is_some());
}

#[test]
fn adc_continuous_firmware_needs_no_write() {
    let mut b = Board::new();
    b.adc_option = 0x8000 | 0x2000 | 0x53;
    let mut c = adc_controller("lucina.charge=report");
    let (s, _, waits) = adc_round(&mut c, &mut b);
    assert!(b.writes().is_empty());
    assert_eq!(waits, 0);
    assert_eq!(s.adc.map(|(_, a)| a.iin_ma), Some(350));
}

#[test]
fn adc_psys_only_when_firmware_enabled_it() {
    let mut b = Board::new();
    b.option1 = 0x1200;
    let mut c = adc_controller("lucina.charge=report");
    let (s, _, _) = adc_round(&mut c, &mut b);
    assert_eq!(b.writes(), [Op::Write(C, 0x35, 0x6073)]);
    assert!(s.adc.is_some_and(|(_, a)| a.psys_mv.is_some()));
}

#[test]
fn adc_option_parsing() {
    let o = |a: &str| Options::from_args(a.split_ascii_whitespace()).adc_ms;
    assert_eq!(o("lucina.charge.adc=off"), None);
    assert_eq!(o("lucina.charge.adc=50"), Some(200));
    assert_eq!(o("lucina.charge.adc=999999"), Some(60_000));
    assert_eq!(o(""), Some(1000));
    assert_eq!(Controller::new(Mode::On).adc_period_ms(), None);
}
// ---- The USB-C power path, read only ----

#[test]
fn pdic_probe_reads_only_and_wakes_the_chip_first() {
    struct Fake(Vec<(u8, Vec<u8>)>);
    impl pdic::RegRead for Fake {
        type Error = ();
        fn read_reg(&mut self, addr: u8, reg: &[u8], buf: &mut [u8]) -> Result<(), ()> {
            self.0.push((addr, reg.to_vec()));
            match (addr, reg) {
                (0x33, [0x00, 0x20]) => buf.copy_from_slice(&[21, 0, 21, 0]),
                (0x33, [0x00, 0x50]) => buf[0] = 0x07,
                (0x14, [0x01]) => buf[0] = 0x01,
                (0x14, [0x15]) => return Err(()),
                _ => buf.fill(0),
            }
            Ok(())
        }
    }
    let mut bus = Fake(Vec::new());
    let p = pdic::Port::read(&mut bus);
    assert!(bus.0[..5].iter().all(|r| *r == (0x33, vec![0x00, 0x08])));
    assert!(!bus.0.iter().any(|(a, r)| *a == 0x14 && (0x03..=0x05).contains(&r[0])));
    assert!(!bus.0.iter().any(|(a, r)| *a == 0x33 && r == &[0x00, 0x30]));
    assert_eq!(p.state_name(), "attached as sink");
    let text = format!("{p}");
    assert!(text.starts_with("PD state 15 00 15 00 (attached as sink), RID 0x07; MUIC DeviceID 0x01"), "{text}");
    assert!(text.contains("VbusValid ?"), "{text}");
}

#[test]
fn the_firmware_s_boot_current_over_the_cap_is_stopped_then_replaced_not_latched() {
    let mut b = Board::new();
    b.charger.charge_current = 2880;
    b.charger.max_charge_voltage = 8696;
    let mut c = Controller::new(Mode::On);
    let (lines, _) = run(&mut c, &mut b, 30);
    assert!(lines.iter().any(|l| l.contains("over the caps")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("until reboot")), "{lines:?}");
    assert_eq!(b.writes()[0], Op::Write(C, I, 0));
    assert_eq!(b.charger.charge_current, 2048);
    assert_eq!(b.charger.max_charge_voltage, 8760);
}

// ---- The USB-C consumer switch, EmuEC's set_CP_Switch ----

/// A PD controller (`0x33`) that answers as one that holds the consumer switch: the status at `0x0014` reports
/// `consumer`, the water byte at `0x0060` reports `water`, and a command window write of `CONSUMER_ON` turns it on.
struct FakePd {
    log: Vec<(bool /* write */, u8, Vec<u8>)>,
    consumer: u8,
    water: u8,
    fail_writes: bool,
}

fn fresh_pd() -> FakePd {
    FakePd {
        log: vec![],
        consumer: 0,
        water: 0,
        fail_writes: false,
    }
}

impl pdic::RegRead for FakePd {
    type Error = ();
    fn read_reg(&mut self, addr: u8, reg: &[u8], buf: &mut [u8]) -> Result<(), ()> {
        self.log.push((false, addr, reg.to_vec()));
        match (addr, reg) {
            (0x33, [0x00, 0x14]) => buf.copy_from_slice(&[0, self.consumer, 0, 0]),
            (0x33, [0x00, 0x60]) => buf[0] = self.water,
            _ => buf.fill(0),
        }
        Ok(())
    }
}

impl pdic::RegWrite for FakePd {
    fn write_reg(&mut self, addr: u8, reg: &[u8], data: &[u8]) -> Result<(), ()> {
        self.log.push((true, addr, data.to_vec()));
        if addr == 0x33 && reg == &[0x00, 0x10] {
            if self.fail_writes {
                return Err(());
            }
            if data == pdic::CONSUMER_ON {
                self.consumer = 1;
            }
        }
        Ok(())
    }
}

fn wrote(bus: &FakePd, data: &[u8]) -> bool {
    bus.log.iter().any(|op| op.0 && op.2 == data)
}

fn consumer_on_writes(bus: &FakePd) -> usize {
    bus.log
        .iter()
        .filter(|op| op.0 && op.2 == pdic::CONSUMER_ON)
        .count()
}

fn pdic_port(state0: u8, byte3: u8) -> pdic::Port {
    pdic::Port {
        func_state: Some([state0, 0, 0, byte3]),
        rid: Some(7),
        muic: [None; 8],
    }
}

fn pdic_round(
    c: &mut pdic::Consumer,
    buses: &mut [Option<&mut FakePd>; 2],
    ports: &[pdic::Port; 2],
    lines: &mut Vec<String>,
) {
    c.round(buses, ports, &mut |_| {}, &mut |l| lines.push(format!("{l}")));
}

#[test]
fn consumer_switch_report_mode_never_writes() {
    let mut a = fresh_pd();
    let mut b = fresh_pd();
    let mut c = pdic::Consumer::new(false);
    let mut lines = Vec::new();
    let ports = [pdic_port(7, 0), pdic_port(21, 0)];
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    drop(buses);
    assert!(a.log.iter().all(|op| !op.0), "{:?}", a.log);
    assert!(b.log.iter().all(|op| !op.0), "{:?}", b.log);
    assert_eq!(lines.iter().filter(|l| l.contains("would ask")).count(), 1, "{lines:?}");
    let before = lines.len();
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert_eq!(lines.len(), before, "{lines:?}");
}

#[test]
fn consumer_switch_on_sends_emuec_bytes_to_the_sink_port_only() {
    let mut a = fresh_pd();
    let mut b = fresh_pd();
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(7, 0), pdic_port(21, 0)];
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert!(wrote(&b, &pdic::CONSUMER_ON), "{:?}", b.log);
    assert!(!wrote(&a, &pdic::CONSUMER_ON), "{:?}", a.log);
    assert!(wrote(&a, &pdic::GET_CP_SWITCH), "{:?}", a.log);
    assert!(
        lines.iter().any(|l| l.contains("consumer switch on sent")),
        "{lines:?}"
    );
    assert_eq!(b.consumer, 1);
    assert_eq!(a.consumer, 0);
}

#[test]
fn consumer_switch_once_per_attach() {
    let mut a = fresh_pd();
    let mut b = fresh_pd();
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(7, 0), pdic_port(21, 0)];
    {
        let mut buses = [Some(&mut a), Some(&mut b)];
        pdic_round(&mut c, &mut buses, &ports, &mut lines);
        for _ in 0..3 {
            pdic_round(&mut c, &mut buses, &ports, &mut lines);
        }
    }
    assert_eq!(consumer_on_writes(&a) + consumer_on_writes(&b), 1);

    let detached = [pdic_port(7, 0), pdic_port(0, 0)];
    {
        let mut buses = [Some(&mut a), Some(&mut b)];
        pdic_round(&mut c, &mut buses, &detached, &mut lines);
    }
    assert_eq!(consumer_on_writes(&a) + consumer_on_writes(&b), 1);

    b.consumer = 0;
    let reattached = [pdic_port(7, 0), pdic_port(21, 0)];
    {
        let mut buses = [Some(&mut a), Some(&mut b)];
        pdic_round(&mut c, &mut buses, &reattached, &mut lines);
    }
    assert_eq!(consumer_on_writes(&a) + consumer_on_writes(&b), 2);
}

#[test]
fn consumer_switch_never_with_both_ports_sink() {
    let mut a = fresh_pd();
    let mut b = fresh_pd();
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(21, 0), pdic_port(21, 0)];
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert!(!wrote(&a, &pdic::CONSUMER_ON), "{:?}", a.log);
    assert!(!wrote(&b, &pdic::CONSUMER_ON), "{:?}", b.log);
}

#[test]
fn consumer_switch_never_on_fault() {
    for (byte3, water) in [(0x20, 0u8), (0x40, 0), (0, 0x08)] {
        let mut a = fresh_pd();
        let mut b = FakePd {
            water,
            ..fresh_pd()
        };
        let mut c = pdic::Consumer::new(true);
        let mut lines = Vec::new();
        let ports = [pdic_port(7, 0), pdic_port(21, byte3)];
        let mut buses = [Some(&mut a), Some(&mut b)];
        pdic_round(&mut c, &mut buses, &ports, &mut lines);
        assert!(!wrote(&a, &pdic::CONSUMER_ON), "byte3 {byte3:02x} water {water:02x}");
        assert!(!wrote(&b, &pdic::CONSUMER_ON), "byte3 {byte3:02x} water {water:02x}");
        assert_eq!(b.consumer, 0, "byte3 {byte3:02x} water {water:02x}");
    }
}

#[test]
fn consumer_switch_not_when_already_on() {
    let mut a = fresh_pd();
    let mut b = FakePd {
        consumer: 1,
        ..fresh_pd()
    };
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(7, 0), pdic_port(21, 0)];
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert!(!wrote(&a, &pdic::CONSUMER_ON), "{:?}", a.log);
    assert!(!wrote(&b, &pdic::CONSUMER_ON), "{:?}", b.log);
}

#[test]
fn consumer_switch_not_when_status_unreadable() {
    let mut a = fresh_pd();
    let mut b = FakePd {
        fail_writes: true,
        ..fresh_pd()
    };
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(7, 0), pdic_port(21, 0)];
    let mut buses = [Some(&mut a), Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert!(!wrote(&a, &pdic::CONSUMER_ON), "{:?}", a.log);
    assert!(!wrote(&b, &pdic::CONSUMER_ON), "{:?}", b.log);
    assert_eq!(a.consumer, 0);
    assert_eq!(b.consumer, 0);
}

#[test]
fn consumer_switch_not_when_other_port_unknown() {
    let mut b = fresh_pd();
    let mut c = pdic::Consumer::new(true);
    let mut lines = Vec::new();
    let ports = [pdic_port(0, 0), pdic_port(21, 0)];
    let mut buses: [Option<&mut FakePd>; 2] = [None, Some(&mut b)];
    pdic_round(&mut c, &mut buses, &ports, &mut lines);
    assert!(!wrote(&b, &pdic::CONSUMER_ON), "{:?}", b.log);
    assert_eq!(b.consumer, 0);
}

#[test]
fn sink_states_and_faults() {
    for (state, sink) in [
        (14u8, false),
        (15, true),
        (21, true),
        (25, true),
        (26, false),
        (29, true),
        (30, false),
    ] {
        assert_eq!(pdic_rules::is_sink_state(state), sink, "state {state}");
    }
    assert!(!pdic_rules::has_fault(0x19, 0));
    assert!(pdic_rules::has_fault(0x20, 0));
    assert!(pdic_rules::has_fault(0x40, 0));
    assert!(pdic_rules::has_fault(0, 0x08));
}

#[test]
fn iin_host_from_the_chosen_pdo_as_emuec_sets_it() {
    use crate::pdic_rules::iin_host;
    // 12 V 3 A: the value the firmware leaves after a cold boot.
    assert_eq!(iin_host(12000, 3000), Some(0x3c00));
    assert_eq!(iin_host(5000, 3000), Some(0x3c00));
    assert_eq!(iin_host(9000, 3000), Some(55 << 8)); // 2770 mA cap
    assert_eq!(iin_host(15000, 3000), Some(0x2e00)); // 2300 mA cap
    assert_eq!(iin_host(12000, 2000), Some(40 << 8)); // the PDO's own current
    assert_eq!(iin_host(0, 1000), None);
}

#[test]
fn iin_write_only_a_confirmed_limit_with_the_adapter_in() {
    use crate::pdic_rules::iin_write;
    // After a replug the chip holds its own 0x4100; the contract's 3000 mA goes out once.
    assert_eq!(iin_write(Some(0x3c00), 0x4100, true), Some(0x3c00));
    assert_eq!(iin_write(Some(0x3c00), 0x3c00, true), None);
    assert_eq!(iin_write(Some(0x3c00), 0x4100, false), None);
    assert_eq!(iin_write(None, 0x4100, true), None);
    assert_eq!(iin_write(Some(0x4100), 0x0900, true), None); // above 3000 mA: never
    assert_eq!(iin_write(Some(0x3c40), 0x4100, true), None); // not a 50 mA step
    assert_eq!(iin_write(Some(0), 0x4100, true), None);
}

#[test]
fn iin_option_is_on_unless_turned_off() {
    let o = Options::from_args(["lucina.charge=on"].into_iter());
    assert!(o.iin_on);
    let o = Options::from_args(["lucina.charge.iin=off"].into_iter());
    assert!(!o.iin_on);
    let o = Options::from_args(["lucina.charge.iin=report"].into_iter());
    assert!(!o.iin_on);
}

#[test]
fn status_follows_the_cable_at_once() {
    let s = |online, status, current, dry| Snapshot {
        battery_status: Some(status),
        current_ma: Some(current),
        ac_online: Some(online),
        dry_ms: Some(dry),
        ..Snapshot::EMPTY
    };
    // Unplugged: Discharging at once, though the gauge still says charging.
    assert_eq!(s(false, 0x0080, 2000, 0).status_text(), Some("Discharging"));
    // Plugged in during the negotiation: Charging at once, before current flows.
    assert_eq!(s(true, 0x00c0, -500, 300).status_text(), Some("Charging"));
    // Plugged in, no current for 5 s: a fault, said.
    assert_eq!(s(true, 0x00c0, 0, 5000).status_text(), Some("Not charging"));
    assert_eq!(s(true, 0x0080, 2063, 9000).status_text(), Some("Charging"));
    assert_eq!(s(true, 0x00a0, 0, 60000).status_text(), Some("Full"));
}

#[test]
fn the_cable_sets_the_status_before_ac_stat() {
    let s = |cable, ac| Snapshot {
        battery_status: Some(0x00c0),
        current_ma: Some(-500),
        ac_online: ac,
        cable,
        dry_ms: Some(0),
        ..Snapshot::EMPTY
    };
    // Plugged: the PD controller sees the cable before VBUS and AC_STAT follow.
    assert_eq!(s(Some(true), Some(false)).status_text(), Some("Charging"));
    assert_eq!(s(Some(true), None).online(), Some(true));
    // Unplugged: on battery at once, though AC_STAT still holds while VBUS decays.
    assert_eq!(s(Some(false), Some(true)).status_text(), Some("Discharging"));
    assert_eq!(s(Some(false), Some(true)).online(), Some(false));
    // No cable state yet: AC_STAT as before.
    assert_eq!(s(None, Some(true)).online(), Some(true));
}

#[test]
fn cable_change_reports_attach_and_detach_once() {
    use crate::pdic::{Consumer, Port};
    let port = |state: u8| Port { func_state: Some([state, 0, 0, 0]), ..Port::default() };
    let mut c = Consumer::new(true);
    assert_eq!(c.cable_change(&[port(0x07), port(0x15)], [true, true]), Some(true));
    assert_eq!(c.cable_change(&[port(0x07), port(0x15)], [true, true]), None);
    assert_eq!(c.cable_change(&[port(0x07), port(0x00)], [true, true]), Some(false));
    // A port that could not be read leaves it as it was.
    assert_eq!(c.cable_change(&[port(0x07), Port::default()], [true, true]), None);
}
