//! Power domains against Linux's `rpmhpd.c` compiled and run on the host (tests/fixtures/pd_oracle.txt), and against
//! hostile command DB contents.
//! The expected values derive from Linux 7.1 rpmhpd.c (GPL-2.0), see oracle/README.md.

mod common;

use common::db::{self, CX, MSS, MX, STD_LEVELS};
use common::{field, parse_num};
use lucina_modem_rpmh::arc::{
    self, ArcError, INT_MAX, Levels, Pds, Send, State, parse_levels, sc7180,
};

const FIXTURE: &str = include_str!("fixtures/pd_oracle.txt");

fn name_index(n: &str) -> usize {
    arc::SC7180
        .iter()
        .position(|d| d.name == n)
        .unwrap_or_else(|| panic!("domain {n}"))
}

#[test]
fn the_domain_table_is_sc7180_rpmhpds() {
    let mut n = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("pd ")) {
        let i: usize = l.split_whitespace().nth(1).unwrap().parse().unwrap();
        let d = &arc::SC7180[i];
        assert_eq!(d.name, field(l, "name"), "pd {i}");
        assert_eq!(d.res_name, field(l, "res"), "pd {i}");
        assert_eq!(d.active_only, field(l, "active_only") == "1", "pd {i}");
        let idx = |k: &str| match field(l, k).parse::<i32>().unwrap() {
            -1 => None,
            v => Some(v as usize),
        };
        assert_eq!(d.peer, idx("peer"), "pd {i} peer");
        assert_eq!(d.parent, idx("parent"), "pd {i} parent");
        assert_eq!(
            d.skip_retention_level,
            field(l, "skip_retention") == "1",
            "pd {i}"
        );
        n += 1;
    }
    assert_eq!(n, 8);
    let c = |name: &str| {
        let l = FIXTURE
            .lines()
            .find(|l| l.starts_with(&format!("const {name} ")))
            .unwrap();
        parse_num(l.split_whitespace().nth(2).unwrap()) as usize
    };
    assert_eq!(sc7180::CX, c("SC7180_CX"));
    assert_eq!(sc7180::CX_AO, c("SC7180_CX_AO"));
    assert_eq!(sc7180::GFX, c("SC7180_GFX"));
    assert_eq!(sc7180::MX, c("SC7180_MX"));
    assert_eq!(sc7180::MX_AO, c("SC7180_MX_AO"));
    assert_eq!(sc7180::LMX, c("SC7180_LMX"));
    assert_eq!(sc7180::LCX, c("SC7180_LCX"));
    assert_eq!(sc7180::MSS, c("SC7180_MSS"));
    assert_eq!(sc7180::COUNT, c("sc7180_num_pds"));
    assert_eq!(arc::MAX_LEVELS, c("RPMH_ARC_MAX_LEVELS"));
    assert_eq!(
        arc::LEVEL_RETENTION as usize,
        c("RPMH_REGULATOR_LEVEL_RETENTION")
    );
}

#[test]
fn cx_has_mx_as_its_parent_and_mss_has_none() {
    assert_eq!(arc::SC7180[sc7180::CX].parent, Some(sc7180::MX));
    assert_eq!(arc::SC7180[sc7180::CX_AO].parent, Some(sc7180::MX_AO));
    assert_eq!(arc::SC7180[sc7180::MX].parent, None);
    assert_eq!(arc::SC7180[sc7180::MSS].parent, None);
    assert_eq!(arc::SC7180[sc7180::MSS].peer, None);
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

#[test]
fn level_mapping_matches_rpmhpd_update_level_mapping() {
    let mut n = 0;
    for l in FIXTURE.lines().filter(|l| l.starts_with("levels ")) {
        let hex = l
            .split_whitespace()
            .find_map(|w| w.strip_prefix("hex="))
            .unwrap_or("");
        let bytes = unhex(hex);
        let len: usize = field(l, "len").parse().unwrap();
        assert_eq!(bytes.len(), len);
        let skip = field(l, "skip") == "1";
        let ret: i32 = field(l, "ret").parse().unwrap();
        let count: usize = field(l, "count").parse().unwrap();
        let got = parse_levels(&bytes[..bytes.len().min(64)], len, skip);
        if ret != 0 {
            assert!(
                matches!(got, Err(ArcError::TooManyLevels(c)) if c == count),
                "{l}: {got:?}"
            );
        } else if count == 0 {
            // Linux accepts an empty table and then computes level_count - 1 as an unsigned wrap-around. Refused here.
            assert_eq!(got, Err(ArcError::NoLevels), "{l}");
        } else {
            let got = got.unwrap_or_else(|e| panic!("{l}: {e}"));
            assert_eq!(got.count, count, "{l}");
            assert_eq!(
                got.enable_corner,
                field(l, "enable").parse::<usize>().unwrap(),
                "{l}"
            );
            let want: Vec<u32> = field(l, "l")
                .split(',')
                .map(|x| x.parse().unwrap())
                .collect();
            assert_eq!(got.level.to_vec(), want, "{l}");
        }
        n += 1;
    }
    assert!(n >= 11);
}

fn set_tables(name: &str) -> Vec<(&'static str, Vec<u16>)> {
    // the same tables as pd_oracle.c's sets, typed again
    let std = STD_LEVELS.to_vec();
    match name {
        "std" => vec![],
        "mixed" => vec![
            ("cx.lvl", vec![16, 48, 64, 128, 192, 256, 384]),
            ("mx.lvl", vec![16, 48, 128, 256, 384]),
            ("mss.lvl", vec![16, 64, 256]),
        ],
        "zero_first" => ["cx.lvl", "mx.lvl", "mss.lvl"]
            .into_iter()
            .map(|r| (r, vec![0, 48, 64]))
            .collect(),
        "single" => ["cx.lvl", "mx.lvl", "mss.lvl"]
            .into_iter()
            .map(|r| (r, vec![64]))
            .collect(),
        other => {
            let _ = std;
            panic!("set {other}")
        }
    }
}

fn load_set(pds: &mut Pds, set: &str) {
    let tables = set_tables(set);
    let addrs = [
        "cx.lvl", "mx.lvl", "gfx.lvl", "lmx.lvl", "lcx.lvl", "mss.lvl",
    ];
    for (i, d) in arc::SC7180.iter().enumerate() {
        let table = tables
            .iter()
            .find(|(r, _)| *r == d.res_name)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| STD_LEVELS.to_vec());
        let bytes = db::u16s(&table);
        let levels = parse_levels(&bytes, bytes.len(), d.skip_retention_level).unwrap();
        let addr = 0x30000 + 0x10 * addrs.iter().position(|r| *r == d.res_name).unwrap() as u32;
        pds.install(i, addr, levels);
    }
}

fn state_name(s: State) -> &'static str {
    match s {
        State::Sleep => "sleep",
        State::Wake => "wake",
        State::Active => "active",
    }
}

fn send_line(s: &Send) -> String {
    format!(
        "send {} {} addr={:#x} data={} sync={}",
        arc::SC7180[s.domain].name,
        state_name(s.state),
        s.addr,
        s.data,
        u8::from(s.sync)
    )
}

#[test]
fn scenarios_match_the_c_driver_send_for_send() {
    // split the fixture into scenarios and their ops
    type Op = (String, Vec<String>);
    let mut scen: Vec<(String, Vec<Op>)> = Vec::new();
    for l in FIXTURE.lines() {
        if let Some(n) = l.strip_prefix("scenario ") {
            scen.push((n.to_string(), Vec::new()));
        } else if let Some(op) = l.strip_prefix("op ") {
            scen.last_mut()
                .unwrap()
                .1
                .push((op.to_string(), Vec::new()));
        } else if l.starts_with("send ") || l.starts_with("st ") || l.starts_with("load ret") {
            scen.last_mut()
                .unwrap()
                .1
                .last_mut()
                .unwrap()
                .1
                .push(l.to_string());
        }
    }
    assert!(scen.len() >= 13, "{} scenarios", scen.len());
    for n in ["modem_release_synced", "modem_release_unsynced", "modem_release_mixed"] {
        assert!(scen.iter().any(|(s, _)| s == n), "scenario {n}");
    }
    for (name, ops) in &scen {
        let mut pds = Pds::new();
        let mut last = [(0u32, 0u32, false); 8];
        for (op, want) in ops {
            let w: Vec<&str> = op.split_whitespace().collect();
            let mut out = Vec::new();
            let mut lines = Vec::new();
            match w[0] {
                "load" => {
                    load_set(&mut pds, w[1]);
                    lines.push("load ret=0".to_string());
                    last = [(0, 0, false); 8];
                }
                "synced" => pds.set_state_synced(w[1] == "1"),
                "perf" => pds.set_performance_state(name_index(w[1]), parse_num(w[2]), &mut out),
                "on" => pds.power_on(name_index(w[1]), &mut out),
                "off" => pds.power_off(name_index(w[1]), &mut out),
                "pas" => {
                    let idx: Vec<usize> = w[1..].iter().map(|d| name_index(d)).collect();
                    out = pds.proxy_vote_max(&idx).unwrap();
                }
                "unpas" => {
                    let idx: Vec<usize> = w[1..].iter().map(|d| name_index(d)).collect();
                    out = pds.proxy_unvote(&idx);
                }
                other => panic!("op {other}"),
            }
            lines.extend(out.iter().map(send_line));
            for (i, prev) in last.iter_mut().enumerate() {
                let (c, a) = pds.corners(i);
                let now = (c, a, pds.is_enabled(i));
                if now != *prev {
                    lines.push(format!(
                        "st {} corner={} active={} enabled={}",
                        arc::SC7180[i].name,
                        now.0,
                        now.1,
                        u8::from(now.2)
                    ));
                    *prev = now;
                }
            }
            assert_eq!(&lines, want, "scenario {name}, op `{op}`");
        }
    }
}

// ---- expectations written by hand, from reading rpmhpd.c and core.c ----

fn standin() -> Vec<u8> {
    db::standin()
}

#[test]
fn a_proxy_vote_at_the_top_sends_mx_then_cx_then_mss() {
    let bytes = standin();
    let cmd_db = db::open(&bytes);
    let mut pds = Pds::new();
    pds.load(&cmd_db, &[sc7180::CX, sc7180::MX, sc7180::MSS])
        .unwrap();
    let sends = pds.proxy_vote_max(&[sc7180::CX, sc7180::MSS]).unwrap();
    let got: Vec<(usize, State, u32, u32, bool)> = sends
        .iter()
        .map(|s| (s.domain, s.state, s.addr, s.data, s.sync))
        .collect();
    use State::*;
    // 8 levels: the top index is 7. mx and cx have peers, so also wake and sleep; mss has none.
    assert_eq!(
        got,
        vec![
            (sc7180::MX, Active, MX, 7, true),
            (sc7180::MX, Wake, MX, 7, false),
            (sc7180::MX, Sleep, MX, 7, false),
            (sc7180::CX, Active, CX, 7, true),
            (sc7180::CX, Wake, CX, 7, false),
            (sc7180::CX, Sleep, CX, 7, false),
            (sc7180::MSS, Active, MSS, 7, true),
        ]
    );
    let cmds = Pds::active_cmds(&sends);
    assert_eq!(cmds.len(), 3);
    assert!(cmds.iter().all(|c| c.wait && c.data == 7));
    assert_eq!(
        cmds.iter().map(|c| c.addr).collect::<Vec<_>>(),
        vec![MX, CX, MSS]
    );
}

#[test]
fn the_vote_is_the_level_index_not_the_voltage_level() {
    let bytes = db::image(&[(
        3,
        vec![
            db::entry("cx.lvl", CX, db::u16s(&[16, 64, 256, 0, 0, 0])),
            db::entry("mx.lvl", MX, db::u16s(&[16, 64, 256, 384])),
            db::entry("mss.lvl", MSS, db::u16s(&[16, 128])),
        ],
    )]);
    let cmd_db = db::open(&bytes);
    let mut pds = Pds::new();
    pds.load(&cmd_db, &[sc7180::CX, sc7180::MX, sc7180::MSS])
        .unwrap();
    assert_eq!(
        pds.levels(sc7180::CX).unwrap().count,
        3,
        "zero padding is cut off"
    );
    let cmds = Pds::active_cmds(&pds.proxy_vote_max(&[sc7180::CX, sc7180::MSS]).unwrap());
    assert_eq!(
        cmds.iter().map(|c| (c.addr, c.data)).collect::<Vec<_>>(),
        vec![(MX, 3), (CX, 2), (MSS, 1)]
    );
}

#[test]
fn a_device_tree_level_maps_to_the_first_index_at_least_as_large() {
    let mut pds = Pds::new();
    load_set(&mut pds, "std");
    let mut out = Vec::new();
    pds.power_on(sc7180::MSS, &mut out);
    pds.set_state_synced(true);
    for (level, want) in [
        (0, 0),
        (16, 0),
        (17, 1),
        (48, 1),
        (49, 2),
        (128, 3),
        (383, 7),
        (384, 7),
        (385, 7),
        (INT_MAX, 7),
    ] {
        out.clear();
        pds.set_performance_state(sc7180::MSS, level, &mut out);
        assert_eq!(pds.corners(sc7180::MSS).0, want, "level {level}");
        assert_eq!(out.last().unwrap().data, want);
    }
}

#[test]
fn setting_a_level_on_a_domain_that_is_off_sends_nothing() {
    let mut pds = Pds::new();
    load_set(&mut pds, "std");
    let mut out = Vec::new();
    pds.set_performance_state(sc7180::CX, INT_MAX, &mut out);
    assert!(out.is_empty());
    assert_eq!(pds.corners(sc7180::CX), (7, 0));
}

// ---- hostile command DB contents ----

#[test]
fn a_missing_or_wrong_resource_is_an_error() {
    let mut pds = Pds::new();
    let bytes = db::image(&[(3, vec![db::entry("mx.lvl", MX, db::u16s(&[16, 64]))])]);
    let d = db::open(&bytes);
    assert_eq!(
        pds.load(&d, &[sc7180::CX]),
        Err(ArcError::Missing("cx.lvl"))
    );
    // address 0 counts as missing, as in Linux (`cmd_db_read_addr` returns 0 for "no such id")
    let bytes = db::image(&[(3, vec![db::entry("cx.lvl", 0, db::u16s(&[16, 64]))])]);
    assert_eq!(
        pds.load(&db::open(&bytes), &[sc7180::CX]),
        Err(ArcError::Missing("cx.lvl"))
    );
    // a VRM resource under an ARC name
    let bytes = db::image(&[(4, vec![db::entry("cx.lvl", 0x40000, db::u16s(&[16, 64]))])]);
    assert_eq!(
        pds.load(&db::open(&bytes), &[sc7180::CX]),
        Err(ArcError::NotArc {
            res: "cx.lvl",
            hw: 4
        })
    );
    // domains that were not loaded cannot be used
    assert_eq!(
        pds.proxy_vote_max(&[sc7180::CX]),
        Err(ArcError::NotLoaded(sc7180::CX))
    );
    assert_eq!(pds.proxy_vote_max(&[99]), Err(ArcError::NotLoaded(99)));
    // cx needs its parent mx loaded too
    let mut pds = Pds::new();
    let levels = parse_levels(&db::u16s(&[16, 64]), 4, false).unwrap();
    pds.install(sc7180::CX, CX, levels);
    assert_eq!(
        pds.proxy_vote_max(&[sc7180::CX]),
        Err(ArcError::NotLoaded(sc7180::MX))
    );
}

#[test]
fn hostile_aux_tables_return_errors_and_do_not_panic() {
    // lengths around the limits, with the entry's own length field as the only truth
    for len in [0usize, 1, 2, 3, 63, 64, 65, 66, 67, 200, 1000, 0xffff] {
        let aux: Vec<u8> = (0..len).map(|i| (i * 7 + 1) as u8).collect();
        let bytes = db::image(&[(
            3,
            vec![
                db::entry("cx.lvl", CX, aux),
                db::entry("mx.lvl", MX, vec![]),
            ],
        )]);
        let d = db::open(&bytes);
        let mut pds = Pds::new();
        let r = pds.load(&d, &[sc7180::CX]);
        match len >> 1 {
            0 => assert_eq!(r, Err(ArcError::NoLevels), "len {len}"),
            c if c > 32 => assert_eq!(r, Err(ArcError::TooManyLevels(c)), "len {len}"),
            _ => assert!(r.is_ok(), "len {len}: {r:?}"),
        }
        // whatever loaded, using the domain does not panic
        let mut out = Vec::new();
        pds.set_performance_state(sc7180::CX, INT_MAX, &mut out);
        let _ = pds.proxy_vote_max(&[sc7180::CX]);
    }
    // the mx entry above has an empty table
    let bytes = db::image(&[(3, vec![db::entry("mx.lvl", MX, vec![])])]);
    assert_eq!(
        Pds::new().load(&db::open(&bytes), &[sc7180::MX]),
        Err(ArcError::NoLevels)
    );
}

#[test]
fn random_aux_tables_never_panic_and_keep_their_invariants() {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..20000 {
        let len = (next() % 80) as usize;
        // many zeros and small numbers, so that padding and retention show up
        let bytes: Vec<u8> = (0..len)
            .map(|_| match next() % 4 {
                0 => 0,
                1 => 16,
                _ => next() as u8,
            })
            .collect();
        for skip in [false, true] {
            let avail = &bytes[..bytes.len().min(64)];
            if let Ok(Levels {
                level,
                count,
                enable_corner,
            }) = parse_levels(avail, len, skip)
            {
                assert!((1..=32).contains(&count));
                assert!(enable_corner < count || (enable_corner == 0 && level[0] == 0));
                assert!(
                    level[count..].iter().all(|&v| v == 0),
                    "nothing past the count"
                );
                // a Pds built from it behaves
                let mut pds = Pds::new();
                let levels = Levels {
                    level,
                    count,
                    enable_corner,
                };
                for i in 0..8 {
                    pds.install(i, 0x30000 + 0x10 * i as u32, levels);
                }
                let mut out = Vec::new();
                pds.set_performance_state(sc7180::MX, (next() & 0x1ff) as u32, &mut out);
                pds.power_on(sc7180::MX, &mut out);
                pds.set_performance_state(sc7180::MX, (next() & 0x1ff) as u32, &mut out);
                pds.power_off(sc7180::MX, &mut out);
                let top = (count - 1) as u32;
                assert!(
                    out.iter().all(|s| s.data <= top),
                    "no vote above the top index"
                );
            }
        }
    }
}

#[test]
fn a_truncated_or_garbage_image_is_not_a_database() {
    // CmdDb itself refuses these; load must never see a table out of range
    let mut bytes = standin();
    let full = bytes.len();
    for cut in [
        0,
        4,
        db::HEADER - 4,
        db::HEADER,
        db::HEADER + 30,
        full - 40,
        full - 4,
    ] {
        let words: Vec<u32> = bytes[..cut / 4 * 4]
            .chunks(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let size = (words.len() * 4) as u32;
        if words.len() < 2 {
            continue;
        }
        if let Some(d) = lucina_qcom_soc::cmd_db::CmdDb::probe(db::ImageIo(words), size) {
            let mut pds = Pds::new();
            let _ = pds.load(&d, &[sc7180::CX, sc7180::MX, sc7180::MSS]);
        }
    }
    // garbage in the entry lengths and offsets
    let mut seed = 12345u32;
    for _ in 0..2000 {
        let i = HEADER_ENTRY_START + (seed as usize % (bytes.len() - HEADER_ENTRY_START));
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        bytes[i] = (seed >> 24) as u8;
        let d = db::open(&bytes);
        let mut pds = Pds::new();
        let _ = pds.load(&d, &[sc7180::CX, sc7180::MX, sc7180::MSS]);
    }
}

// from the first accelerator header on: the magic stays, everything after it is garbage
const HEADER_ENTRY_START: usize = 8;
