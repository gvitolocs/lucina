//! The complete vote lists, the XO vote against `clk-rpmh.c` run on the host (tests/fixtures/clk_oracle.txt), and the
//! lists sent through the driver into the RSC model.

mod common;

use common::db::{self, CX, LCX, LDOA9, LDOC1, LDOC2, LDOC10, LDOC11, LMX, MSS, MX, XO};
use common::sim::SimRsc;
use common::{CountDelay, field, parse_num};
use lucina_modem_rpmh::TcsCmd;
use lucina_modem_rpmh::plan::{self, PlanError};
use lucina_modem_rpmh::rsc::{Rsc, RscConfig, RscError};
use lucina_modem_rpmh::vrm::VrmError;
use lucina_modem_rpmh::xo::{self, XoError};

const CLK: &str = include_str!("fixtures/clk_oracle.txt");

fn t(addr: u32, data: u32) -> TcsCmd {
    TcsCmd {
        addr,
        data,
        wait: true,
    }
}

#[test]
fn the_xo_vote_is_bi_tcxo_div2s_active_set() {
    // the oracle ran clk_rpmh_prepare with the command DB address 0x30060
    let l = CLK
        .lines()
        .find(|l| l.starts_with("clk res_name="))
        .unwrap();
    assert_eq!(field(l, "res_name"), xo::RES_NAME);
    assert_eq!(
        field(l, "res_addr").parse::<u32>().unwrap(),
        xo::ARC_EN_OFFSET
    );
    assert_eq!(field(l, "res_on_val").parse::<u32>().unwrap(), xo::ON_VALUE);
    assert!(
        CLK.contains("table RPMH_CXO_CLK=0 is_bi_tcxo_div2=1"),
        "the modem's clock index 0 is bi_tcxo_div2"
    );
    assert_eq!(field(l, "valid_state_mask"), "0x7");
    let prepare: Vec<&str> = CLK.lines().filter(|l| l.starts_with("prepare ")).collect();
    // Linux sends sleep, wake, active, in that order; active is last and the only one that waits
    let states: Vec<&str> = prepare
        .iter()
        .map(|l| l.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(states, ["sleep", "wake", "active"]);
    let active = prepare[2];
    let bytes = db::image(&[(3, vec![db::entry("xo.lvl", 0x30060, db::u16s(&[1, 2]))])]);
    let got = xo::active_vote(&db::open(&bytes)).unwrap();
    assert_eq!(got.addr, parse_num(field(active, "addr")));
    assert_eq!(got.data, field(active, "data").parse::<u32>().unwrap());
    assert_eq!(got.wait, field(active, "sync") == "1");
    // the sleep and wake sets carry the same address and value but never wait
    for l in &prepare[..2] {
        assert_eq!(field(l, "addr"), field(active, "addr"));
        assert_eq!(field(l, "data"), field(active, "data"));
        assert_eq!(field(l, "sync"), "0");
    }
}

#[test]
fn a_missing_xo_resource_is_an_error() {
    let bytes = db::image(&[(3, vec![db::entry("cx.lvl", CX, db::u16s(&[1, 2]))])]);
    assert_eq!(xo::active_vote(&db::open(&bytes)), Err(XoError::Missing));
    let bytes = db::image(&[(3, vec![db::entry("xo.lvl", 0, db::u16s(&[1, 2]))])]);
    assert_eq!(xo::active_vote(&db::open(&bytes)), Err(XoError::Missing));
}

#[test]
fn the_modem_plan_is_mx_cx_mss_xo() {
    let bytes = db::standin();
    let p = plan::modem_plan(&db::open(&bytes)).unwrap();
    let got: Vec<(&str, TcsCmd)> = p.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    // top ARC index of the invented tables (8 levels) is 7; XO "on" is 3; every vote waits for the answer
    assert_eq!(
        got,
        vec![
            ("mx.lvl active", t(MX, 7)),
            ("cx.lvl active", t(CX, 7)),
            ("mss.lvl active", t(MSS, 7)),
            ("xo.lvl active", t(XO, 3)),
        ]
    );
}

#[test]
fn the_audio_dsps_plan_is_lcx_lmx_xo_with_no_parents_as_linux_has_them() {
    let bytes = db::standin();
    let p = plan::adsp_plan(&db::open(&bytes)).unwrap();
    let got: Vec<(&str, TcsCmd)> = p.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    assert_eq!(
        got,
        vec![
            ("lcx.lvl active", t(LCX, 7)),
            ("lmx.lvl active", t(LMX, 7)),
            ("xo.lvl active", t(XO, 3)),
        ]
    );
}

#[test]
fn the_wifi_plan_is_mode_voltage_enable_for_each_of_the_five_supplies() {
    let bytes = db::standin();
    let p = plan::wcn3990_plan(&db::open(&bytes)).unwrap();
    let got: Vec<(&str, TcsCmd)> = p.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    let mut want = Vec::new();
    for (name, addr, mv) in [
        ("ldoa9", LDOA9, 488),
        ("ldoc1", LDOC1, 1616),
        ("ldoc2", LDOC2, 1168),
        ("ldoc10", LDOC10, 3000),
        ("ldoc11", LDOC11, 3000),
    ] {
        want.push((format!("{name} mode"), t(addr + 8, 7)));
        want.push((format!("{name} voltage"), t(addr, mv)));
        want.push((format!("{name} enable"), t(addr + 4, 1)));
    }
    let want: Vec<(&str, TcsCmd)> = want.iter().map(|(n, c)| (n.as_str(), *c)).collect();
    assert_eq!(got, want);
}

#[test]
fn plans_report_what_is_missing() {
    let bytes = db::image(&[(3, vec![db::entry("cx.lvl", CX, db::u16s(&[16, 64]))])]);
    let d = db::open(&bytes);
    assert!(matches!(plan::modem_plan(&d), Err(PlanError::Arc(_))));
    assert!(
        matches!(plan::wcn3990_plan(&d), Err(PlanError::Vrm(VrmError::Missing(n))) if n == "ldoa9")
    );
    // everything but the clock
    let bytes = db::image(&[(
        3,
        vec![
            db::entry("cx.lvl", CX, db::u16s(&db::STD_LEVELS)),
            db::entry("mx.lvl", MX, db::u16s(&db::STD_LEVELS)),
            db::entry("mss.lvl", MSS, db::u16s(&db::STD_LEVELS)),
        ],
    )]);
    assert_eq!(
        plan::modem_plan(&db::open(&bytes)),
        Err(PlanError::Xo(XoError::Missing))
    );
}

fn rsc_for_plan() -> (SimRsc, RscConfig) {
    (SimRsc::galaxy_book_go(), RscConfig { bus: None, ..RscConfig::GALAXY_BOOK_GO_DRV2 })
}

#[test]
fn both_plans_go_through_the_driver_into_the_model_without_a_violation() {
    let bytes = db::standin();
    let d = db::open(&bytes);
    let (sim, cfg) = rsc_for_plan();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let delay = CountDelay::default();
    for plan in [
        plan::modem_plan(&d).unwrap(),
        plan::wcn3990_plan(&d).unwrap(),
    ] {
        plan::execute(&mut rsc, &plan, &delay, 10).unwrap();
        sim.assert_clean();
        // the model saw each vote, in order, as one single-command write with a response requested
        let seen: Vec<(u32, u32, u32)> = sim
            .executed()
            .iter()
            .rev()
            .take(plan.len())
            .rev()
            .map(|e| e.cmds[0])
            .collect();
        let want: Vec<(u32, u32, u32)> = plan
            .iter()
            .map(|(_, c)| (0x10108, c.addr, c.data))
            .collect();
        assert_eq!(seen, want);
    }
    assert_eq!(sim.executed().len(), 4 + 15);
    assert_eq!(rsc.in_use(), 0);
}

#[test]
fn a_plan_stops_at_the_step_that_fails() {
    let bytes = db::standin();
    let d = db::open(&bytes);
    let (sim, cfg) = rsc_for_plan();
    let mut rsc = Rsc::probe(&sim, &cfg).unwrap();
    let p = plan::modem_plan(&d).unwrap();
    // make the second request time out: stuck from the start, so the first one does
    sim.set(|s| s.stuck = true);
    let e = plan::execute(&mut rsc, &p, &CountDelay::default(), 0).unwrap_err();
    assert_eq!(e, (0, RscError::Timeout { tcs: 0 }));
    assert_eq!(
        sim.executed().len(),
        1,
        "nothing after the failed step was sent"
    );
}

// rpmh_flush on what a modem start caches: a resource with equal sleep and wake values is skipped, the rest go to the
// SLEEP and WAKE TCSes without a trigger, sleep then wake, and a second flush invalidates first.
#[test]
fn sleep_and_wake_sets_follow_rpmh_flush() {
    use lucina_modem_rpmh::arc::State;
    use lucina_modem_rpmh::rpmh::Cache;

    let mut c = Cache::new();
    c.record(State::Sleep, 0x30000, 1);
    c.record(State::Wake, 0x30000, 5);
    c.record(State::Active, 0x30000, 6); // the active vote replaces the wake value
    c.record(State::Sleep, 0x30010, 3);
    c.record(State::Wake, 0x30010, 3); // equal: skipped
    c.record(State::Active, 0x30020, 9); // no sleep value: skipped
    assert_eq!(c.valid().collect::<Vec<_>>(), vec![(0x30000, 1, 6)]);
}

const PD: &str = include_str!("fixtures/pd_oracle.txt");

// qcom_pas_pds_disable after sync_state, as pd_oracle.c's "modem_release_synced" ran it through rpmhpd.c, then
// clk_disable_unprepare(xo) as clk_oracle.c ran clk_rpmh_unprepare.
#[test]
fn the_modem_release_is_rpmhpds_unvote_then_xo_off() {
    // the oracle's addresses: 0x30000 + 0x10 * (cx, mx, gfx, lmx, lcx, mss), xo at 0x30060
    let (cx, mx, mss, xo_addr) = (0x30000, 0x30010, 0x30050, 0x30060);
    let bytes = db::image(&[(
        3,
        vec![
            db::entry("cx.lvl", cx, db::u16s(&db::STD_LEVELS)),
            db::entry("mx.lvl", mx, db::u16s(&db::STD_LEVELS)),
            db::entry("mss.lvl", mss, db::u16s(&db::STD_LEVELS)),
            db::entry("xo.lvl", xo_addr, db::u16s(&[1, 2, 3, 0])),
        ],
    )]);
    let v = plan::modem_votes(&db::open(&bytes)).unwrap();

    let scenario: Vec<&str> = PD
        .lines()
        .skip_while(|l| *l != "scenario modem_release_synced")
        .take_while(|l| *l != "end")
        .collect();
    let want: Vec<(String, TcsCmd)> = scenario
        .iter()
        .skip_while(|l| !l.starts_with("op unpas"))
        .filter(|l| l.starts_with("send ") && l.split_whitespace().nth(2) == Some("active"))
        .map(|l| {
            let dom = l.split_whitespace().nth(1).unwrap();
            (
                format!("{dom}.lvl release"),
                TcsCmd {
                    addr: parse_num(field(l, "addr")),
                    data: field(l, "data").parse().unwrap(),
                    wait: field(l, "sync") == "1",
                },
            )
        })
        .collect();
    assert!(!want.is_empty());
    assert_eq!(v.release, want);

    // xo.lvl, data 0, no wait: clk_rpmh_unprepare's active line
    let unprepare: Vec<&str> = CLK
        .lines()
        .filter(|l| l.starts_with("unprepare "))
        .collect();
    let active = unprepare
        .iter()
        .find(|l| l.split_whitespace().nth(1) == Some("active"))
        .unwrap();
    assert_eq!(v.xo_off.0, "xo.lvl off");
    assert_eq!(
        v.xo_off.1,
        TcsCmd {
            addr: xo_addr,
            data: 0,
            wait: false
        }
    );
    assert_eq!(v.xo_off.1.addr, parse_num(field(active, "addr")));
    assert_eq!(
        v.xo_off.1.data,
        field(active, "data").parse::<u32>().unwrap()
    );
    assert_eq!(v.xo_off.1.wait, field(active, "sync") == "1");

    // nothing of the start is left for sleep and wake: no XO or proxy entry keeps its on-value
    let on: Vec<(u32, u32)> = v
        .proxies
        .iter()
        .map(|(_, c)| (c.addr, c.data))
        .chain([(v.xo.1.addr, v.xo.1.data)])
        .collect();
    for (addr, sleep, wake) in v.release_cache.valid() {
        assert!(
            !on.iter()
                .any(|&(a, d)| a == addr && (sleep == d || wake == d)),
            "{addr:#x} sleep={sleep} wake={wake} still holds an on-value"
        );
    }
    assert_eq!(v.release_cache.valid().count(), 0);
    assert_ne!(v.release_cache, v.cache);
}
