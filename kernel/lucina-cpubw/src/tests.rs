// SPDX-License-Identifier: GPL-2.0-only

use super::*;

/// `arch/arm64/boot/dts/qcom/sc7180.dtsi` (v6.12): `cpu0_opp_table` (silver, line 399) and `cpu6_opp_table` (gold, line 454).
const SILVER: [OppBw; 10] = [
    OppBw {
        khz: 300_000,
        ddr_kbps: 1_200_000,
        l3_kbps: 4_800_000,
    },
    OppBw {
        khz: 576_000,
        ddr_kbps: 1_200_000,
        l3_kbps: 4_800_000,
    },
    OppBw {
        khz: 768_000,
        ddr_kbps: 1_200_000,
        l3_kbps: 4_800_000,
    },
    OppBw {
        khz: 1_017_600,
        ddr_kbps: 1_804_000,
        l3_kbps: 8_908_800,
    },
    OppBw {
        khz: 1_248_000,
        ddr_kbps: 2_188_000,
        l3_kbps: 12_902_400,
    },
    OppBw {
        khz: 1_324_800,
        ddr_kbps: 2_188_000,
        l3_kbps: 12_902_400,
    },
    OppBw {
        khz: 1_516_800,
        ddr_kbps: 3_072_000,
        l3_kbps: 15_052_800,
    },
    OppBw {
        khz: 1_612_800,
        ddr_kbps: 3_072_000,
        l3_kbps: 15_052_800,
    },
    OppBw {
        khz: 1_708_800,
        ddr_kbps: 3_072_000,
        l3_kbps: 15_052_800,
    },
    OppBw {
        khz: 1_804_800,
        ddr_kbps: 4_068_000,
        l3_kbps: 22_425_600,
    },
];

const GOLD_TOP: OppBw = OppBw {
    khz: 2_553_600,
    ddr_kbps: 8_532_000,
    l3_kbps: 23_347_200,
};

#[test]
fn opp_exact_match() {
    // sc7180.dtsi:404-405 and 449-450.
    assert_eq!(opp_bw(&SILVER, 300_000), Some((1_200_000, 4_800_000)));
    assert_eq!(opp_bw(&SILVER, 1_804_800), Some((4_068_000, 22_425_600)));
    // sc7180.dtsi:534-535: the gold prime's top OPP.
    assert_eq!(
        opp_bw(&[GOLD_TOP], 2_553_600),
        Some((8_532_000, 23_347_200))
    );
}

#[test]
fn opp_miss_is_none() {
    assert_eq!(opp_bw(&SILVER, 300_001), None);
    assert_eq!(opp_bw(&SILVER, 299_999), None);
    assert_eq!(opp_bw(&SILVER, 0), None);
    assert_eq!(opp_bw(&[], 300_000), None);
}

#[test]
fn opp_pair_accepts_the_dt_pairs_only() {
    // Every pair of the silver table and the gold top (sc7180.dtsi:404-450, 534-535) is accepted.
    for opp in SILVER {
        assert!(opp_pair_known(&SILVER, opp.ddr_kbps, opp.l3_kbps));
    }
    assert!(opp_pair_known(&[GOLD_TOP], 8_532_000, 23_347_200));
    // The gold top's pair is not a silver one, nor are swapped cells, a cell of one OPP with the other cell of another, or zero.
    assert!(!opp_pair_known(&SILVER, 8_532_000, 23_347_200));
    assert!(!opp_pair_known(&SILVER, 4_800_000, 1_200_000));
    assert!(!opp_pair_known(&SILVER, 1_200_000, 8_908_800));
    assert!(!opp_pair_known(&SILVER, 0, 0));
    assert!(!opp_pair_known(&[], 1_200_000, 4_800_000));
}

#[test]
fn opp_first_match_wins() {
    let t = [
        OppBw {
            khz: 5,
            ddr_kbps: 1,
            l3_kbps: 2,
        },
        OppBw {
            khz: 5,
            ddr_kbps: 3,
            l3_kbps: 4,
        },
    ];
    assert_eq!(opp_bw(&t, 5), Some((1, 2)));
}

#[test]
fn aggregate_sums_avg_and_takes_the_peak() {
    let a = Req {
        avg_kbps: 100,
        peak_kbps: 700,
    };
    let b = Req {
        avg_kbps: 50,
        peak_kbps: 900,
    };
    assert_eq!(
        aggregate(a, b),
        Req {
            avg_kbps: 150,
            peak_kbps: 900
        }
    );
    assert_eq!(
        aggregate(b, a),
        Req {
            avg_kbps: 150,
            peak_kbps: 900
        }
    );
    assert_eq!(aggregate(a, Req::default()), a);
    let full = Req {
        avg_kbps: u64::MAX - 1,
        peak_kbps: 0,
    };
    assert_eq!(
        aggregate(
            full,
            Req {
                avg_kbps: 5,
                peak_kbps: 0
            }
        )
        .avg_kbps,
        u64::MAX
    );
}

// The aux values of drivers/lucina-modem-rpmh/src/bcm.rs's tests: unit 1000, width 4, scale 1000.
const AUX: BcmAux = BcmAux {
    unit: 1000,
    width: 4,
    vote_scale: 1000,
};

/// `sc7180.c`: `qns_llcc` (line 1045: channels 1, buswidth 16), on BCM SH0 (line 1254, keepalive).
fn llcc(avg: u64, peak: u64) -> NodeReq {
    NodeReq {
        buswidth: 16,
        channels: 1,
        req: Req {
            avg_kbps: avg,
            peak_kbps: peak,
        },
    }
}

/// `sc7180.c`: `ebi` (line 1061: channels 2, buswidth 4), on BCM MC0 (line 1247, keepalive) and ACV (line 1239, enable_mask BIT(3)).
fn ebi(avg: u64, peak: u64) -> NodeReq {
    NodeReq {
        buswidth: 4,
        channels: 2,
        req: Req {
            avg_kbps: avg,
            peak_kbps: peak,
        },
    }
}

#[test]
fn bcm_div_rounds_small_votes_up() {
    // bcm.rs `small_votes_round_up`.
    assert_eq!(bcm_div(0, 7), 0);
    assert_eq!(bcm_div(3, 7), 1);
    assert_eq!(bcm_div(7, 7), 1);
    assert_eq!(bcm_div(14, 7), 2);
    assert_eq!(bcm_div(15, 7), 2);
    assert_eq!(bcm_div(5, 0), 0);
}

#[test]
fn the_narrowest_node_decides() {
    // bcm.rs `the_narrowest_node_decides`: CN0 on qnm_snoc (8 bytes) and qhs_venus_cfg (4 bytes): 125 kBps -> 125.
    let cn0 = [
        NodeReq {
            buswidth: 8,
            channels: 1,
            req: Req {
                avg_kbps: 125,
                peak_kbps: 0,
            },
        },
        NodeReq {
            buswidth: 4,
            channels: 1,
            req: Req {
                avg_kbps: 125,
                peak_kbps: 0,
            },
        },
    ];
    assert_eq!(bcm_vote(AUX, &cn0), (125, 0));
    // MC0 on ebi, two channels of 4 bytes: 2500 * 4 / 8 = 1250.
    assert_eq!(bcm_vote(AUX, &[ebi(2500, 0)]), (1250, 0));
}

#[test]
fn peak_divides_by_the_buswidth_only() {
    // peak 2500 on ebi: 2500 * 4 / 4 = 2500; on qns_llcc: 2500 * 4 / 16 = 625.
    assert_eq!(bcm_vote(AUX, &[ebi(0, 2500)]), (0, 2500));
    assert_eq!(bcm_vote(AUX, &[llcc(0, 2500)]), (0, 625));
}

#[test]
fn idle_bucket_votes_nothing() {
    // The per-bucket vote has no keepalive: idle or empty gives (0, 0).
    assert_eq!(bcm_vote(AUX, &[llcc(0, 0)]), (0, 0));
    assert_eq!(bcm_vote(AUX, &[]), (0, 0));
}

#[test]
fn keepalive_with_amc_zero_forces_amc_and_wake() {
    // bcm-voter.c:118-124: AMC (0, 0) on a keepalive BCM -> AMC and WAKE (1, 1); SLEEP keeps its own vote.
    let idle = [llcc(0, 0)];
    let sleep = [ebi(2500, 0)];
    assert_eq!(
        bcm_votes(AUX, [&idle, &idle, &sleep], true),
        [(1, 1), (1, 1), (1250, 0)]
    );
    // WAKE's own vote is overwritten too, SLEEP idle stays (0, 0).
    let wake = [ebi(0, 2500)];
    assert_eq!(
        bcm_votes(AUX, [&idle, &wake, &idle], true),
        [(1, 1), (1, 1), (0, 0)]
    );
    assert_eq!(
        bcm_votes(AUX, [&[], &[], &[]], true),
        [(1, 1), (1, 1), (0, 0)]
    );
}

#[test]
fn keepalive_with_amc_asking_changes_nothing() {
    let amc = [ebi(2500, 0)];
    let idle = [llcc(0, 0)];
    assert_eq!(
        bcm_votes(AUX, [&amc, &idle, &idle], true),
        [(1250, 0), (0, 0), (0, 0)]
    );
}

#[test]
fn no_keepalive_changes_nothing() {
    let idle = [llcc(0, 0)];
    let sleep = [ebi(0, 2500)];
    assert_eq!(
        bcm_votes(AUX, [&idle, &idle, &sleep], false),
        [(0, 0), (0, 0), (0, 2500)]
    );
}

#[test]
fn acv_enable_mask() {
    // bcm.rs `keepalive_and_acv`: ACV with 1 kBps gives (0, 8), idle (0, 0).
    assert_eq!(bcm_mask_vote(1 << 3, &[ebi(1, 0)], false), (0, 8));
    assert_eq!(bcm_mask_vote(1 << 3, &[ebi(0, 1)], false), (0, 8));
    assert_eq!(bcm_mask_vote(1 << 3, &[ebi(0, 0)], false), (0, 0));
    assert_eq!(bcm_mask_vote(1 << 3, &[], false), (0, 0));
    assert_eq!(bcm_mask_vote(1 << 3, &[ebi(0, 0)], true), (8, 8));
}

#[test]
fn votes_are_clamped_to_14_bits() {
    // bcm.rs `commands_and_clamps`: unit 1, width 32, 1 << 40 kBps.
    let aux = BcmAux {
        unit: 1,
        width: 32,
        vote_scale: 1000,
    };
    let node = NodeReq {
        buswidth: 32,
        channels: 1,
        req: Req {
            avg_kbps: 1 << 40,
            peak_kbps: 1 << 40,
        },
    };
    assert_eq!(bcm_vote(aux, &[node]), (VOTE_MAX, VOTE_MAX));
    let huge = NodeReq {
        buswidth: 1,
        channels: 1,
        req: Req {
            avg_kbps: u64::MAX,
            peak_kbps: u64::MAX,
        },
    };
    let wide = BcmAux {
        unit: 1,
        width: u16::MAX,
        vote_scale: u64::MAX,
    };
    assert_eq!(bcm_vote(wide, &[huge]), (VOTE_MAX, VOTE_MAX));
    // The exact value on the edge: 0x3fff * unit / scale-1000 is VOTE_MAX, one more kBps-unit over is cut.
    assert_eq!(bcm_vote(AUX, &[ebi(0, 0x3fff)]), (0, 0x3fff));
    assert_eq!(bcm_vote(AUX, &[ebi(0, 0x4000)]), (0, 0x3fff));
}

#[test]
fn a_zero_scale_means_1000() {
    let zero = BcmAux {
        unit: 1000,
        width: 4,
        vote_scale: 0,
    };
    assert_eq!(
        bcm_vote(zero, &[ebi(2500, 7)]),
        bcm_vote(AUX, &[ebi(2500, 7)])
    );
}

#[test]
fn a_zero_divisor_votes_nothing() {
    let no_unit = BcmAux {
        unit: 0,
        width: 4,
        vote_scale: 1000,
    };
    assert_eq!(bcm_vote(no_unit, &[ebi(9, 9)]), (0, 0));
    let no_width = NodeReq {
        buswidth: 0,
        channels: 0,
        req: Req {
            avg_kbps: 9,
            peak_kbps: 9,
        },
    };
    assert_eq!(bcm_vote(AUX, &[no_width]), (0, 0));
}

#[test]
fn l3_rate() {
    // kBps * 1000 / buswidth.
    assert_eq!(l3_rate_hz(4_800_000, 32), 150_000_000);
    assert_eq!(l3_rate_hz(0, 32), 0);
    assert_eq!(l3_rate_hz(1, 32), 31);
    assert_eq!(l3_rate_hz(u64::MAX, 1), u64::MAX);
    assert_eq!(l3_rate_hz(5, 0), u64::MAX);
}

#[test]
fn l3_index_picks_the_first_entry_at_or_above() {
    let lut = [300_000_000, 576_000_000, 768_000_000, 1_017_600_000];
    assert_eq!(l3_index(&lut, 0), Some(0), "below the first");
    assert_eq!(
        l3_index(&lut, 300_000_000),
        Some(0),
        "an exact hit on the first"
    );
    assert_eq!(l3_index(&lut, 576_000_000), Some(1), "an exact hit");
    assert_eq!(l3_index(&lut, 576_000_001), Some(2), "between");
    assert_eq!(
        l3_index(&lut, 1_017_600_000),
        Some(3),
        "an exact hit on the last"
    );
    assert_eq!(l3_index(&lut, 2_000_000_000), Some(3), "above the last");
    assert_eq!(l3_index(&[7], 100), Some(0), "one entry");
    assert_eq!(l3_index(&[], 100), None);
}

#[test]
fn l3_index_takes_the_first_of_equal_entries() {
    assert_eq!(l3_index(&[5, 10, 10, 10], 10), Some(1));
}
