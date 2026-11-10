//! LDO votes against Linux's `qcom-rpmh-regulator.c` tables and helpers compiled and run on the host
//! (tests/fixtures/vrm_oracle.txt), and the order of the votes for one regulator.
//! The expected values derive from Linux 7.1 qcom-rpmh-regulator.c, helpers.c and lib/linear_ranges.c (GPL-2.0), see
//! oracle/README.md.

mod common;

use common::db::{self, LDOA9};
use common::{field, parse_num};
use lucina_modem_rpmh::vrm::{self, HwData, Kind, Vreg, VrmError, WCN3990_LDOS};

const FIXTURE: &str = include_str!("fixtures/vrm_oracle.txt");

fn hw_by_name(n: &str) -> HwData {
    match n {
        "pmic5_nldo" => vrm::PMIC5_NLDO,
        "pmic5_pldo" => vrm::PMIC5_PLDO,
        "pmic5_pldo_lv" => vrm::PMIC5_PLDO_LV,
        other => panic!("hw {other}"),
    }
}

fn ranges(hw: &HwData) -> String {
    hw.ranges
        .iter()
        .map(|r| format!("{}:{}:{}:{}", r.min, r.min_sel, r.max_sel, r.step))
        .collect::<Vec<_>>()
        .join(";")
}

#[test]
fn constants_match() {
    let c = |name: &str| {
        let l = FIXTURE
            .lines()
            .find(|l| l.starts_with(&format!("const {name} ")))
            .unwrap();
        parse_num(l.split_whitespace().nth(2).unwrap())
    };
    assert_eq!(vrm::REG_VRM_VOLTAGE, c("RPMH_REGULATOR_REG_VRM_VOLTAGE"));
    assert_eq!(vrm::REG_ENABLE, c("RPMH_REGULATOR_REG_ENABLE"));
    assert_eq!(vrm::REG_VRM_MODE, c("RPMH_REGULATOR_REG_VRM_MODE"));
    assert_eq!(
        vrm::PMIC5_LDO_MODE_RETENTION as u32,
        c("PMIC5_LDO_MODE_RETENTION")
    );
    assert_eq!(vrm::PMIC5_LDO_MODE_LPM as u32, c("PMIC5_LDO_MODE_LPM"));
    assert_eq!(vrm::PMIC5_LDO_MODE_HPM as u32, c("PMIC5_LDO_MODE_HPM"));
    assert_eq!(vrm::RPMH_REGULATOR_MODE_RET, c("RPMH_REGULATOR_MODE_RET"));
    assert_eq!(vrm::RPMH_REGULATOR_MODE_LPM, c("RPMH_REGULATOR_MODE_LPM"));
    assert_eq!(vrm::RPMH_REGULATOR_MODE_AUTO, c("RPMH_REGULATOR_MODE_AUTO"));
    assert_eq!(vrm::RPMH_REGULATOR_MODE_HPM, c("RPMH_REGULATOR_MODE_HPM"));
    assert_eq!(vrm::REGULATOR_MODE_STANDBY, c("REGULATOR_MODE_STANDBY"));
}

#[test]
fn pmic_mode_map_pmic5_ldo_matches() {
    let l = FIXTURE
        .lines()
        .find(|l| l.starts_with("mode_map pmic5_ldo"))
        .unwrap();
    let want: Vec<i32> = l
        .split_whitespace()
        .skip(2)
        .map(|x| x.parse().unwrap())
        .collect();
    assert_eq!(vrm::PMIC_MODE_MAP_PMIC5_LDO.to_vec(), want);
    // the HPM value the plan sends
    assert_eq!(
        vrm::PMIC_MODE_MAP_PMIC5_LDO[vrm::REGULATOR_MODE_NORMAL as usize],
        7
    );
}

#[test]
fn of_map_mode_matches() {
    let mut n = 0;
    for l in FIXTURE
        .lines()
        .filter(|l| l.starts_with("of_map_mode ldo "))
    {
        let w: Vec<&str> = l.split_whitespace().collect();
        assert_eq!(
            vrm::of_map_mode_ldo(w[2].parse().unwrap()),
            w[4].parse::<u32>().unwrap(),
            "{l}"
        );
        n += 1;
    }
    assert_eq!(n, 6);
}

#[test]
fn hardware_data_matches() {
    for l in FIXTURE.lines().filter(|l| l.starts_with("hw ")) {
        let name = l.split_whitespace().nth(1).unwrap();
        let hw = hw_by_name(name);
        assert_eq!(hw.name, name);
        assert_eq!(ranges(&hw), field(l, "ranges"), "{name}");
        assert_eq!(
            hw.ranges.len(),
            field(l, "n_ranges").parse::<usize>().unwrap(),
            "{name}"
        );
        assert_eq!(
            hw.n_voltages,
            field(l, "n_voltages").parse::<u32>().unwrap(),
            "{name}"
        );
        assert_eq!(
            hw.hpm_min_load_ua,
            field(l, "hpm_min_load_uA").parse::<u32>().unwrap(),
            "{name}"
        );
        assert_eq!(field(l, "mode_map"), "pmic_mode_map_pmic5_ldo");
        assert_eq!(hw.pmic_mode_map, &vrm::PMIC_MODE_MAP_PMIC5_LDO);
        // n_voltages is what the ranges add up to
        let total: u32 = hw.ranges.iter().map(|r| r.max_sel - r.min_sel + 1).sum();
        assert_eq!(total, hw.n_voltages, "{name}");
    }
}

#[test]
fn the_five_wifi_ldos_have_the_hardware_data_and_names_of_the_driver_tables() {
    for s in &WCN3990_LDOS {
        let compat = match s.pmic_id {
            "a" => "pm6150",
            "c" => "pm6150l",
            other => panic!("pmic id {other}"),
        };
        let l = FIXTURE
            .lines()
            .find(|l| l.starts_with(&format!("vreg {compat} ldo{} ", s.index)))
            .unwrap_or_else(|| panic!("no table entry for {compat} ldo{}", s.index));
        assert_eq!(field(l, "hw"), s.hw.name, "{}", s.supply);
        assert_eq!(
            field(l, "res"),
            vrm::resource_name(s.pmic_id, s.index),
            "{}",
            s.supply
        );
        assert_eq!(field(l, "regulator_type"), "0", "VRM");
        assert_eq!(ranges(&s.hw), field(l, "ranges"));
        assert_eq!(field(l, "mode_map"), "pmic_mode_map_pmic5_ldo");
        assert_eq!(
            field(l, "of_map_mode"),
            "rpmh_regulator_pmic4_ldo_of_map_mode"
        );
    }
}

#[test]
fn resource_names_follow_the_two_formats() {
    assert_eq!(vrm::resource_name("a", 9), "ldoa9");
    assert_eq!(vrm::resource_name("c", 10), "ldoc10");
    assert_eq!(vrm::resource_name("_E1", 9), "L9_E1");
    // the "_E" must be within the first four characters; a one-character id is never "_E"
    assert_eq!(vrm::resource_name("ab_E", 3), "L3ab_E");
    assert_eq!(vrm::resource_name("abc_E", 3), "ldoabc_E3");
    let l = FIXTURE
        .lines()
        .find(|l| l.starts_with("name fmt1 a 9 "))
        .unwrap();
    assert!(l.ends_with("ldoa9"));
    let l = FIXTURE
        .lines()
        .find(|l| l.starts_with("name fmt _E1 9 "))
        .unwrap();
    assert!(l.ends_with("L9_E1"));
}

#[test]
fn voltage_listing_and_mapping_match_the_linux_helpers() {
    let mut lists = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("list ")) {
        let w: Vec<&str> = l.split_whitespace().collect();
        let hw = hw_by_name(w[1]);
        let sel: u32 = w[2].parse().unwrap();
        let want: i64 = w[3].parse().unwrap();
        let got = hw.list_voltage(sel).map_or(-22, i64::from);
        assert_eq!(got, want, "{l}");
        lists += 1;
    }
    let mut maps = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("map ")) {
        let w: Vec<&str> = l.split_whitespace().collect();
        let hw = hw_by_name(w[1]);
        let (min, max): (i64, i64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
        let want_sel: i64 = field(l, "sel").parse().unwrap();
        // Linux takes ints; a negative or zero request is outside every range here
        let got = if min < 0 || max < 0 {
            None
        } else {
            hw.map_voltage(min as u32, max as u32)
        };
        assert_eq!(got.map_or(-22, i64::from), want_sel, "{l}");
        if let Some(sel) = got {
            let uv = hw.list_voltage(sel).unwrap();
            assert_eq!(uv, field(l, "uv").parse::<u32>().unwrap());
            assert_eq!(uv.div_ceil(1000), field(l, "mv").parse::<u32>().unwrap());
        }
        maps += 1;
    }
    assert!(
        lists >= 30 && maps >= 50,
        "{lists} listings, {maps} mappings"
    );
}

fn cmds(sends: &[vrm::Send]) -> Vec<(Kind, u32, u32, bool)> {
    sends
        .iter()
        .map(|s| (s.kind, s.cmd.addr, s.cmd.data, s.cmd.wait))
        .collect()
}

#[test]
fn one_regulator_is_mode_then_voltage_then_enable_all_waiting() {
    let spec = WCN3990_LDOS[0];
    let mut v = Vreg::new(LDOA9, spec.hw, false);
    let mut out = Vec::new();
    v.register(spec.min_uv, spec.max_uv, spec.initial_mode, &mut out)
        .unwrap();
    // registration: the voltage is only remembered; the initial mode (HPM) is sent
    assert_eq!(cmds(&out), vec![(Kind::Mode, LDOA9 + 8, 7, true)]);
    out.clear();
    v.enable(&mut out).unwrap();
    // the first enable sends the remembered voltage (488 mV), then the enable
    assert_eq!(
        cmds(&out),
        vec![
            (Kind::Voltage, LDOA9, 488, true),
            (Kind::Enable, LDOA9 + 4, 1, true)
        ]
    );
}

#[test]
fn the_wifi_supplies_get_the_votes_worked_out_from_the_device_tree() {
    // (resource, address, mV): minimum of the constraint, rounded up to the selector's step
    let bytes = db::standin();
    let d = db::open(&bytes);
    let want = [
        ("ldoa9", db::LDOA9, 488),
        ("ldoc1", db::LDOC1, 1616),
        ("ldoc2", db::LDOC2, 1168),
        ("ldoc10", db::LDOC10, 3000),
        ("ldoc11", db::LDOC11, 3000),
    ];
    for (spec, (name, addr, mv)) in WCN3990_LDOS.iter().zip(want) {
        assert_eq!(vrm::resource_name(spec.pmic_id, spec.index), name);
        let sends = vrm::ldo_votes(&d, spec).unwrap();
        assert_eq!(
            cmds(&sends),
            vec![
                (Kind::Mode, addr + 8, 7, true),
                (Kind::Voltage, addr, mv, true),
                (Kind::Enable, addr + 4, 1, true),
            ],
            "{name}"
        );
    }
}

#[test]
fn voltage_changes_wait_only_when_they_raise_and_disable_never_waits() {
    let hw = vrm::PMIC5_PLDO;
    let mut v = Vreg::new(0x40000, hw, false);
    let mut out = Vec::new();
    v.set_voltage_sel(100, &mut out).unwrap();
    assert!(
        out.is_empty(),
        "before the first enable the voltage is cached"
    );
    v.enable(&mut out).unwrap();
    out.clear();
    v.set_voltage_sel(120, &mut out).unwrap();
    v.set_voltage_sel(110, &mut out).unwrap();
    v.disable(&mut out).unwrap();
    let data = |sel: u32| hw.list_voltage(sel).unwrap().div_ceil(1000);
    assert_eq!(
        cmds(&out),
        vec![
            (Kind::Voltage, 0x40000, data(120), true),
            (Kind::Voltage, 0x40000, data(110), false),
            (Kind::Enable, 0x40004, 0, false),
        ]
    );
    // qcom,always-wait-for-ack makes everything wait
    let mut v = Vreg::new(0x40000, hw, true);
    out.clear();
    v.set_voltage_sel(100, &mut out).unwrap();
    v.enable(&mut out).unwrap();
    v.set_voltage_sel(90, &mut out).unwrap();
    v.disable(&mut out).unwrap();
    assert!(out.iter().all(|s| s.cmd.wait), "{out:?}");
}

#[test]
fn a_regulator_enabled_without_a_voltage_sends_no_voltage() {
    let mut v = Vreg::new(0x40000, vrm::PMIC5_NLDO, false);
    let mut out = Vec::new();
    v.enable(&mut out).unwrap();
    assert_eq!(cmds(&out), vec![(Kind::Enable, 0x40004, 1, true)]);
}

#[test]
fn modes_map_through_the_pmic5_ldo_table_and_repeat_sends_nothing() {
    let mut v = Vreg::new(0x40000, vrm::PMIC5_NLDO, false);
    let mut out = Vec::new();
    for (mode, code) in [
        (vrm::REGULATOR_MODE_NORMAL, 7),
        (vrm::REGULATOR_MODE_IDLE, 4),
        (vrm::REGULATOR_MODE_STANDBY, 3),
    ] {
        out.clear();
        v.set_mode(mode, &mut out).unwrap();
        assert_eq!(
            cmds(&out),
            vec![(Kind::Mode, 0x40008, code, true)],
            "mode {mode}"
        );
        out.clear();
        v.set_mode(mode, &mut out).unwrap();
        assert!(out.is_empty(), "same mode again");
    }
    // FAST has no code on an LDO (-EINVAL in the table); the rest are not modes
    for bad in [vrm::REGULATOR_MODE_FAST, 0, 3, 5, 6, 7, 9, 0x10, u32::MAX] {
        assert_eq!(
            v.set_mode(bad, &mut out),
            Err(VrmError::BadMode(bad)),
            "mode {bad:#x}"
        );
    }
}

#[test]
fn registration_applies_the_constraints_like_the_regulator_core() {
    let mut out = Vec::new();
    // impossible: a range the hardware cannot do
    let mut v = Vreg::new(0x40000, vrm::PMIC5_NLDO, false);
    assert_eq!(
        v.register(3_000_000, 3_400_000, vrm::RPMH_REGULATOR_MODE_HPM, &mut out),
        Err(VrmError::Unsupportable {
            min_uv: 3_000_000,
            max_uv: 3_400_000
        })
    );
    // an initial mode the driver does not map
    let mut v = Vreg::new(0x40000, vrm::PMIC5_NLDO, false);
    assert_eq!(
        v.register(488_000, 800_000, vrm::RPMH_REGULATOR_MODE_AUTO, &mut out),
        Err(VrmError::BadMode(vrm::RPMH_REGULATOR_MODE_AUTO))
    );
    // no voltage constraint: only the mode
    let mut v = Vreg::new(0x40000, vrm::PMIC5_NLDO, false);
    v.register(0, 0, vrm::RPMH_REGULATOR_MODE_LPM, &mut out)
        .unwrap();
    assert_eq!(cmds(&out), vec![(Kind::Mode, 0x40008, 4, true)]);
}

#[test]
fn a_missing_resource_is_named() {
    let bytes = db::image(&[(
        4,
        vec![
            db::entry("ldoa9", LDOA9, vec![]),
            db::entry("ldoc1", 0, vec![]),
        ],
    )]);
    let d = db::open(&bytes);
    assert!(vrm::ldo_votes(&d, &WCN3990_LDOS[0]).is_ok());
    // address 0 is as good as absent
    assert_eq!(
        vrm::ldo_votes(&d, &WCN3990_LDOS[1]),
        Err(VrmError::Missing("ldoc1".into()))
    );
    assert_eq!(
        vrm::ldo_votes(&d, &WCN3990_LDOS[3]),
        Err(VrmError::Missing("ldoc10".into()))
    );
}

#[test]
fn the_millivolt_vote_rounds_up_like_div_round_up() {
    // None of the five rails' voltages is off a millivolt, so check the rounding on invented hardware data.
    static ODD: [vrm::LinearRange; 1] = [vrm::LinearRange {
        min: 1_000_250,
        min_sel: 0,
        max_sel: 10,
        step: 500,
    }];
    let hw = HwData {
        name: "odd",
        ranges: &ODD,
        n_voltages: 11,
        hpm_min_load_ua: 0,
        pmic_mode_map: &vrm::PMIC_MODE_MAP_PMIC5_LDO,
    };
    let mut v = Vreg::new(0x40000, hw, false);
    let mut out = Vec::new();
    v.set_voltage_sel(1, &mut out).unwrap(); // 1_000_750 uV
    v.enable(&mut out).unwrap();
    assert_eq!(cmds(&out)[0], (Kind::Voltage, 0x40000, 1001, true));
}
