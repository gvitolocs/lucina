//! The RSC driver against Linux's `rpmh-rsc.c` compiled and run on the host (tests/fixtures/rsc_oracle.txt, made by
//! oracle/gen.sh): register offsets, bit values, `rpmh_probe_tcs_config` and the exact register writes of
//! `rpmh_rsc_send_data` + `tcs_tx_done`.
//! The expected values derive from Linux 7.1 rpmh-rsc.c (GPL-2.0), see oracle/README.md.

mod common;

use common::{CONFIG_8_TCS_3_CMDS, CountDelay, ID_2_7, Ram, field, parse_num};
use lucina_modem_rpmh::rsc::{self, Rsc, RscConfig, RscError, TcsCmd, TcsRequest};

const FIXTURE: &str = include_str!("fixtures/rsc_oracle.txt");

fn reg_const(name: &str) -> u32 {
    match name {
        "RSC_DRV_TCS_OFFSET" => rsc::reg::TCS_OFFSET,
        "RSC_DRV_CMD_OFFSET" => rsc::reg::CMD_OFFSET,
        "DRV_SOLVER_CONFIG" => rsc::reg::DRV_SOLVER_CONFIG,
        "DRV_PRNT_CHLD_CONFIG" => rsc::reg::DRV_PRNT_CHLD_CONFIG,
        "RSC_DRV_IRQ_ENABLE" => rsc::reg::IRQ_ENABLE,
        "RSC_DRV_IRQ_STATUS" => rsc::reg::IRQ_STATUS,
        "RSC_DRV_IRQ_CLEAR" => rsc::reg::IRQ_CLEAR,
        "RSC_DRV_CMD_WAIT_FOR_CMPL" => rsc::reg::CMD_WAIT_FOR_CMPL,
        "RSC_DRV_CONTROL" => rsc::reg::CONTROL,
        "RSC_DRV_STATUS" => rsc::reg::STATUS,
        "RSC_DRV_CMD_ENABLE" => rsc::reg::CMD_ENABLE,
        "RSC_DRV_CMD_MSGID" => rsc::reg::CMD_MSGID,
        "RSC_DRV_CMD_ADDR" => rsc::reg::CMD_ADDR,
        "RSC_DRV_CMD_DATA" => rsc::reg::CMD_DATA,
        "RSC_DRV_CMD_STATUS" => rsc::reg::CMD_STATUS,
        "RSC_DRV_CMD_RESP_DATA" => rsc::reg::CMD_RESP_DATA,
        other => panic!("unknown register {other}"),
    }
}

#[test]
fn register_offsets_are_the_2_7_table() {
    let mut n = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("reg ")) {
        let mut w = l.split_whitespace().skip(1);
        let (name, want) = (w.next().unwrap(), parse_num(w.next().unwrap()));
        assert_eq!(reg_const(name), want, "{name}");
        n += 1;
    }
    assert_eq!(n, 16, "every entry of rpmh_rsc_reg_offset_ver_2_7");
}

#[test]
fn the_3_0_table_differs_where_this_crate_refuses_it() {
    // A layout-3 DRV has other offsets for the command registers: reading it with the 2.7 table would write to the
    // wrong registers, which is why probe refuses a major version of 3.
    let mut differs = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("reg30 ")) {
        let mut w = l.split_whitespace().skip(1);
        let (name, v30) = (w.next().unwrap(), parse_num(w.next().unwrap()));
        if reg_const(name) != v30 {
            differs += 1;
        }
    }
    assert!(differs >= 8, "3.0 differs from 2.7 in {differs} registers");
}

#[test]
fn bit_values_match() {
    let c = |name: &str| {
        let l = FIXTURE
            .lines()
            .find(|l| l.starts_with(&format!("const {name} ")))
            .unwrap_or_else(|| panic!("no const {name}"));
        parse_num(l.split_whitespace().nth(2).unwrap())
    };
    assert_eq!(rsc::TCS_AMC_MODE_ENABLE, c("TCS_AMC_MODE_ENABLE"));
    assert_eq!(rsc::TCS_AMC_MODE_TRIGGER, c("TCS_AMC_MODE_TRIGGER"));
    assert_eq!(rsc::CMD_MSGID_LEN, c("CMD_MSGID_LEN"));
    assert_eq!(rsc::CMD_MSGID_RESP_REQ, c("CMD_MSGID_RESP_REQ"));
    assert_eq!(rsc::CMD_MSGID_WRITE, c("CMD_MSGID_WRITE"));
    assert_eq!(rsc::CMD_STATUS_ISSUED, c("CMD_STATUS_ISSUED"));
    assert_eq!(rsc::CMD_STATUS_COMPL, c("CMD_STATUS_COMPL"));
    assert_eq!(rsc::DRV_NUM_TCS_MASK, c("DRV_NUM_TCS_MASK"));
    assert_eq!(rsc::DRV_NUM_TCS_SHIFT, c("DRV_NUM_TCS_SHIFT"));
    assert_eq!(rsc::DRV_NCPT_MASK, c("DRV_NCPT_MASK"));
    assert_eq!(rsc::DRV_NCPT_SHIFT, c("DRV_NCPT_SHIFT"));
    assert_eq!(rsc::reg::RSC_DRV_ID, c("RSC_DRV_ID"));
    assert_eq!(rsc::MAX_CMDS_PER_TCS as u32, c("MAX_CMDS_PER_TCS"));
    assert_eq!(rsc::MAX_TCS_PER_TYPE, c("MAX_TCS_PER_TYPE"));
    assert_eq!(rsc::TCS_TYPE_NR as u32, c("TCS_TYPE_NR"));
    assert_eq!(rsc::MAX_TCS_NR as u32, c("MAX_TCS_NR"));
    assert_eq!(rsc::SLEEP_TCS as u32, c("SLEEP_TCS"));
    assert_eq!(rsc::WAKE_TCS as u32, c("WAKE_TCS"));
    assert_eq!(rsc::ACTIVE_TCS as u32, c("ACTIVE_TCS"));
    assert_eq!(rsc::CONTROL_TCS as u32, c("CONTROL_TCS"));
}

fn ram_for(config: u32) -> Ram {
    let ram = Ram::new(0x10000);
    ram.poke(rsc::reg::RSC_DRV_ID, ID_2_7);
    ram.poke(rsc::reg::DRV_PRNT_CHLD_CONFIG, config);
    ram
}

#[test]
fn probe_matches_rpmh_probe_tcs_config() {
    let mut checked = 0;
    for l in FIXTURE
        .lines()
        .filter(|l| l.starts_with("probe ") && l.contains(" ret="))
    {
        let n = l.split_whitespace().nth(1).unwrap();
        let cfg: Vec<u32> = field(l, "cfg")
            .split(',')
            .map(|x| x.parse().unwrap())
            .collect();
        if cfg.len() != 8 {
            // A wrong-length qcom,tcs-config cannot be written down in RscConfig; the type is the check.
            continue;
        }
        let config = parse_num(field(l, "config"));
        let rc = RscConfig {
            drv_id: field(l, "drv_id").parse().unwrap(),
            tcs_offset: parse_num(field(l, "offset")),
            tcs_config: [
                (cfg[0], cfg[1]),
                (cfg[2], cfg[3]),
                (cfg[4], cfg[5]),
                (cfg[6], cfg[7]),
            ],
            window: 0x10000,
            bus: None,
        };
        let ret: i32 = field(l, "ret").parse().unwrap();
        let got = Rsc::probe(ram_for(config), &rc);
        if ret != 0 {
            assert!(
                matches!(got, Err(RscError::BadConfig(_))),
                "probe {n}: Linux fails with {ret}, got {:?}",
                got.err()
            );
            checked += 1;
            continue;
        }
        let rsc = got.unwrap_or_else(|e| panic!("probe {n}: Linux accepts, got {e}"));
        for g in FIXTURE
            .lines()
            .filter(|g| g.starts_with(&format!("probe {n} group")))
        {
            let ty: usize = g
                .split_whitespace()
                .nth(2)
                .unwrap()
                .strip_prefix("group")
                .unwrap()
                .parse()
                .unwrap();
            let grp = rsc.group(ty);
            assert_eq!(
                grp.configured,
                field(g, "configured") == "1",
                "probe {n} group {ty}"
            );
            assert_eq!(
                grp.num_tcs,
                field(g, "num").parse::<u32>().unwrap(),
                "probe {n} group {ty} num"
            );
            assert_eq!(
                grp.ncpt,
                field(g, "ncpt").parse::<u32>().unwrap(),
                "probe {n} group {ty} ncpt"
            );
            assert_eq!(
                grp.mask,
                parse_num(field(g, "mask")),
                "probe {n} group {ty} mask"
            );
            assert_eq!(
                grp.offset,
                field(g, "offset").parse::<u32>().unwrap(),
                "probe {n} group {ty} offset"
            );
        }
        let tail = FIXTURE
            .lines()
            .find(|g| g.starts_with(&format!("probe {n} num_tcs")))
            .unwrap();
        assert_eq!(
            rsc.num_tcs(),
            field(tail, "num_tcs").parse::<u32>().unwrap(),
            "probe {n} num_tcs"
        );
        checked += 1;
    }
    assert!(checked >= 9, "{checked} probe cases");
}

struct Case {
    name: String,
    config: u32,
    cfg: [u32; 8],
    ops: Vec<String>,
    rets: Vec<i32>,
    writes: Vec<(u32, u32)>,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    let mut cur: Option<Case> = None;
    for l in FIXTURE.lines() {
        if let Some(name) = l.strip_prefix("case ") {
            cur = Some(Case {
                name: name.to_string(),
                config: 0,
                cfg: [0; 8],
                ops: Vec::new(),
                rets: Vec::new(),
                writes: Vec::new(),
            });
        } else if let Some(c) = cur.as_mut() {
            let mut w = l.split_whitespace();
            match w.next() {
                Some("cfg") => {
                    c.config = parse_num(w.next().unwrap());
                    for (i, x) in w.enumerate() {
                        c.cfg[i] = x.parse().unwrap();
                    }
                }
                Some("poke" | "send" | "tx_done" | "ctrl" | "invalidate") => c.ops.push(l.to_string()),
                Some("ret") => c.rets.push(w.next().unwrap().parse().unwrap()),
                Some("w") => c
                    .writes
                    .push((parse_num(w.next().unwrap()), parse_num(w.next().unwrap()))),
                Some("end") => out.push(cur.take().unwrap()),
                other => panic!("unexpected fixture line {other:?}"),
            }
        }
    }
    out
}

fn parse_cmds(words: &[&str]) -> Vec<TcsCmd> {
    words
        .iter()
        .map(|w| {
            let mut p = w.split(':');
            TcsCmd {
                addr: parse_num(p.next().unwrap()),
                data: p.next().unwrap().parse().unwrap(),
                wait: p.next().unwrap() == "1",
            }
        })
        .collect()
}

#[test]
fn register_writes_match_send_data_and_tx_done() {
    let cases = cases();
    assert!(cases.len() >= 7, "{} cases", cases.len());
    for c in &cases {
        let ram = ram_for(c.config);
        let rc = RscConfig {
            drv_id: 2,
            tcs_offset: 0xd00,
            tcs_config: [
                (c.cfg[0], c.cfg[1]),
                (c.cfg[2], c.cfg[3]),
                (c.cfg[4], c.cfg[5]),
                (c.cfg[6], c.cfg[7]),
            ],
            window: 0x10000,
            bus: None,
        };
        // the oracle pokes registers behind the driver's back; so does the replay, through the same Ram
        let mut rsc = Rsc::probe(&ram, &rc).unwrap();
        // plain RAM has no command status; Linux 7.1 does not read it
        rsc.quirks.check_cmd_status = false;
        ram.clear_writes();
        let delay = CountDelay::default();
        let mut rets = Vec::new();
        for op in &c.ops {
            let w: Vec<&str> = op.split_whitespace().collect();
            match w[0] {
                "poke" => ram.poke(parse_num(w[1]), parse_num(w[2])),
                "send" => {
                    let cmds = parse_cmds(&w[2..]);
                    let req = TcsRequest {
                        wait_for_compl: w[1] == "1",
                        cmds: &cmds,
                    };
                    // Linux's send returns once the TCS is triggered. Here a request whose TCS never reports done
                    // is the same state, so a timeout counts as "sent".
                    rets.push(match rsc.send(&req, &delay, 0) {
                        Ok(()) | Err(RscError::Timeout { .. }) => 0,
                        Err(RscError::Busy) => -16,
                        Err(e) => panic!("{}: unexpected {e}", c.name),
                    });
                }
                "tx_done" => {
                    rsc.reap();
                }
                "ctrl" => {
                    rets.push(
                        match rsc.write_ctrl(
                            w[1].parse().unwrap(),
                            parse_num(w[3]),
                            parse_num(w[4]),
                            w[2] == "1",
                        ) {
                            Ok(()) => 0,
                            Err(RscError::Busy) => -12,
                            Err(e) => panic!("{}: unexpected {e}", c.name),
                        },
                    );
                }
                "invalidate" => rsc.invalidate_ctrl().unwrap(),
                _ => unreachable!(),
            }
        }
        assert_eq!(rets, c.rets, "{}: return values", c.name);
        assert_eq!(ram.writes(), c.writes, "{}: register writes", c.name);
    }
}

#[test]
fn the_probe_write_is_irq_enable_with_the_active_mask() {
    let ram = ram_for(CONFIG_8_TCS_3_CMDS);
    let _ = Rsc::probe(&ram, &RscConfig { bus: None, ..RscConfig::GALAXY_BOOK_GO_DRV2 }).unwrap();
    // rpmh_rsc_probe: writel_relaxed(drv->tcs[ACTIVE_TCS].mask, drv->tcs_base + drv->regs[RSC_DRV_IRQ_ENABLE])
    assert_eq!(ram.writes(), vec![(0xd00, 0b11)]);
}
