// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes kernel/lucina-cpubw/src/lib.rs by
// #[path], so Verus checks `opp_bw`, `opp_pair_known`, `aggregate`, `bcm_vote`, `bcm_votes`, `bcm_mask_vote`, `l3_rate_hz` and `l3_index` as
// the kernel compiles them; each promises its spec in its `ensures`. Numbered as in lib.rs:
//   1. `bcm_vote` is at most VOTE_MAX in both numbers, `bcm_mask_vote` returns only 0 or the enable mask (the `ensures` of both);
//   2. `bcm_vote` is monotonic (the lemmas below, over the specs the `ensures` pin the code to); `bcm_votes` applies Linux's keepalive
//      rule over the three buckets and otherwise returns each bucket's vote (its `ensures`);
//   3. `l3_index` is the first index below the last with a LUT entry >= rate, else the last (its `ensures`);
//   4. no bandwidth: (0, 0) from the per-bucket vote (their `ensures`, restated below over the specs);
//   5. `aggregate` takes the max peak and the saturating sum; `opp_bw` answers only on an exact match, `opp_pair_known` only for
//      a pair some OPP has (their `ensures`);
//   6. the L3 row never goes down as the peak goes up (`the_l3_row_is_monotone`, over the shipped `l3_rate_hz` and `l3_index`);
//   7. the specs give Linux's numbers on worked examples (`the_votes_are_linuxs`, `the_l3_rows_are_linuxs`,
//      `the_l3_rates_are_linuxs`), so a formula changed in a function and its spec alike (scale, buswidth, channels, units) fails.
// The lemmas state the properties over the nodes themselves, so a bug planted in a function and its spec alike still fails here
// (verification/juno-cpubw/mutants.sh).
#![feature(proc_macro_hygiene)]
#[path = "../../kernel/lucina-cpubw/src/lib.rs"]
pub mod cpubw;

use cpubw::*;
use vstd::prelude::*;

verus! {

/// The same nodes, each asking for no less: same geometry, avg and peak at least as large.
pub open spec fn asks_no_less(a: Seq<NodeReq>, b: Seq<NodeReq>) -> bool {
    a.len() == b.len() && forall|i: int| 0 <= i < a.len() ==> a[i].buswidth == b[i].buswidth
        && a[i].channels == b[i].channels && a[i].req.avg_kbps <= b[i].req.avg_kbps && a[i].req.peak_kbps <= b[i].req.peak_kbps
}

/// `bcm_div` never goes down when the numerator goes up.
proof fn div_is_monotone(n1: int, n2: int, base: int)
    requires
        0 <= n1 <= n2,
        0 <= base,
    ensures
        div_spec(n1, base) <= div_spec(n2, base),
{
    if base > 0 && n1 != 0 {
        if n2 < base {
        } else if n1 >= base {
            vstd::arithmetic::div_mod::lemma_div_is_ordered(n1, n2, base);
        } else {
            vstd::arithmetic::div_mod::lemma_div_is_ordered(base, n2, base);
            vstd::arithmetic::div_mod::lemma_div_by_self(base);
        }
    }
}

/// The product with a non-negative constant never goes down.
proof fn times_is_monotone(a: int, b: int, m: int)
    requires
        0 <= a <= b,
        0 <= m,
    ensures
        a * m <= b * m,
{
    vstd::arithmetic::mul::lemma_mul_inequality(a, b, m);
}

proof fn avg_term_is_monotone(aux: BcmAux, a: NodeReq, b: NodeReq)
    requires
        a.buswidth == b.buswidth,
        a.channels == b.channels,
        a.req.avg_kbps <= b.req.avg_kbps,
    ensures
        avg_term_spec(aux, a) <= avg_term_spec(aux, b),
{
    times_is_monotone(a.req.avg_kbps as int, b.req.avg_kbps as int, aux.width as int);
    div_is_monotone(
        a.req.avg_kbps as int * aux.width as int,
        b.req.avg_kbps as int * aux.width as int,
        a.buswidth as int * a.channels as int,
    );
}

proof fn peak_term_is_monotone(aux: BcmAux, a: NodeReq, b: NodeReq)
    requires
        a.buswidth == b.buswidth,
        a.req.peak_kbps <= b.req.peak_kbps,
    ensures
        peak_term_spec(aux, a) <= peak_term_spec(aux, b),
{
    times_is_monotone(a.req.peak_kbps as int, b.req.peak_kbps as int, aux.width as int);
    div_is_monotone(
        a.req.peak_kbps as int * aux.width as int,
        b.req.peak_kbps as int * aux.width as int,
        a.buswidth as int,
    );
}

proof fn agg_avg_is_monotone(aux: BcmAux, a: Seq<NodeReq>, b: Seq<NodeReq>, n: int)
    requires
        asks_no_less(a, b),
        0 <= n <= a.len(),
    ensures
        agg_avg_spec(aux, a, n) <= agg_avg_spec(aux, b, n),
    decreases n,
{
    if n > 0 {
        agg_avg_is_monotone(aux, a, b, n - 1);
        avg_term_is_monotone(aux, a[n - 1], b[n - 1]);
    }
}

proof fn agg_peak_is_monotone(aux: BcmAux, a: Seq<NodeReq>, b: Seq<NodeReq>, n: int)
    requires
        asks_no_less(a, b),
        0 <= n <= a.len(),
    ensures
        agg_peak_spec(aux, a, n) <= agg_peak_spec(aux, b, n),
    decreases n,
{
    if n > 0 {
        agg_peak_is_monotone(aux, a, b, n - 1);
        peak_term_is_monotone(aux, a[n - 1], b[n - 1]);
    }
}

/// An aggregate that goes up never lowers its vote.
proof fn vote_is_monotone(aux: BcmAux, g1: int, g2: int)
    requires
        0 <= g1 <= g2,
    ensures
        vote_spec(g1, aux) <= vote_spec(g2, aux),
{
    let s = scale_spec(aux);
    times_is_monotone(g1, g2, s);
    div_is_monotone(g1 * s, g2 * s, aux.unit as int);
}

/// 2. Without keepalive, nodes asking for no less never lower x or y (so raising one node's peak, the others fixed, never
/// lowers y; raising its avg never lowers x).
proof fn the_vote_is_monotone(aux: BcmAux, a: Seq<NodeReq>, b: Seq<NodeReq>)
    requires
        asks_no_less(a, b),
    ensures
        vote_final_x_spec(aux, a, false) <= vote_final_x_spec(aux, b, false),
        vote_final_y_spec(aux, a, false) <= vote_final_y_spec(aux, b, false),
{
    agg_avg_is_monotone(aux, a, b, a.len() as int);
    agg_peak_is_monotone(aux, a, b, a.len() as int);
    vote_is_monotone(aux, agg_avg_spec(aux, a, a.len() as int), agg_avg_spec(aux, b, a.len() as int));
    vote_is_monotone(aux, agg_peak_spec(aux, a, a.len() as int), agg_peak_spec(aux, b, a.len() as int));
}

/// 1. The vote and the final numbers are at most VOTE_MAX.
proof fn the_vote_is_at_most_vote_max(aux: BcmAux, nodes: Seq<NodeReq>, keepalive: bool)
    ensures
        vote_final_x_spec(aux, nodes, keepalive) <= VOTE_MAX,
        vote_final_y_spec(aux, nodes, keepalive) <= VOTE_MAX,
{
}

/// 4. With nothing asked on any node, every term is 0, so is the vote, and without keepalive the final numbers are (0, 0).
proof fn nothing_asked_nothing_voted(aux: BcmAux, nodes: Seq<NodeReq>, n: int)
    requires
        forall|i: int| 0 <= i < nodes.len() ==> nodes[i].req.avg_kbps == 0 && nodes[i].req.peak_kbps == 0,
        0 <= n <= nodes.len(),
    ensures
        agg_avg_spec(aux, nodes, n) == 0,
        agg_peak_spec(aux, nodes, n) == 0,
    decreases n,
{
    if n > 0 {
        nothing_asked_nothing_voted(aux, nodes, n - 1);
    }
}

proof fn nothing_asked_no_vote(aux: BcmAux, nodes: Seq<NodeReq>)
    requires
        forall|i: int| 0 <= i < nodes.len() ==> nodes[i].req.avg_kbps == 0 && nodes[i].req.peak_kbps == 0,
    ensures
        vote_final_x_spec(aux, nodes, false) == 0,
        vote_final_y_spec(aux, nodes, false) == 0,
{
    nothing_asked_nothing_voted(aux, nodes, nodes.len() as int);
    assert(0 * scale_spec(aux) == 0) by (nonlinear_arith);
}

/// 3. `l3_index` over the LUT's fields: the first index of the prefix whose entry is at least the rate, else the end of the prefix.
proof fn the_l3_index_is_the_first_hit(lut: Seq<u64>, rate: u64, n: int)
    requires
        0 <= n <= lut.len(),
    ensures
        0 <= l3_first_spec(lut, rate, n) <= n,
        forall|j: int| 0 <= j < l3_first_spec(lut, rate, n) ==> lut[j] < rate,
        l3_first_spec(lut, rate, n) == n || lut[l3_first_spec(lut, rate, n)] >= rate,
    decreases n,
{
    if n > 0 {
        the_l3_index_is_the_first_hit(lut, rate, n - 1);
        let p = l3_first_spec(lut, rate, n - 1);
        if p == n - 1 && lut[n - 1] < rate {
            assert(forall|j: int| 0 <= j < n ==> lut[j] < rate);
        }
    }
}

/// 6. The first hit never moves down when the rate goes up (for any LUT, sorted or not).
proof fn the_l3_first_hit_is_monotone(lut: Seq<u64>, r1: u64, r2: u64, n: int)
    requires
        r1 <= r2,
        0 <= n <= lut.len(),
    ensures
        l3_first_spec(lut, r1, n) <= l3_first_spec(lut, r2, n),
    decreases n,
{
    if n > 0 {
        the_l3_first_hit_is_monotone(lut, r1, r2, n - 1);
        the_l3_index_is_the_first_hit(lut, r1, n - 1);
        the_l3_index_is_the_first_hit(lut, r2, n - 1);
    }
}

/// 6. Over the shipped `l3_rate_hz` and `l3_index` (their `ensures` determine them fully): the L3 row of a larger peak is never
/// lower. This is what makes the kernel's `max` of two rows (patch 1231, the domains' row and the static test request's) the row
/// of the larger peak, which is what `qcom_osm_l3_set` writes after the node takes the max of its peaks.
fn the_l3_row_is_monotone(lut: &[u64], p1: u64, p2: u64, buswidth: u32)
    requires
        p1 <= p2,
        lut@.len() > 0,
{
    let r1 = l3_rate_hz(p1, buswidth);
    let r2 = l3_rate_hz(p2, buswidth);
    proof {
        if buswidth > 0 {
            vstd::arithmetic::mul::lemma_mul_inequality(p1 as int, p2 as int, 1000);
            vstd::arithmetic::div_mod::lemma_div_is_ordered(p1 as int * 1000, p2 as int * 1000, buswidth as int);
        }
    }
    assert(r1 <= r2);
    let i1 = l3_index(lut, r1);
    let i2 = l3_index(lut, r2);
    proof {
        the_l3_first_hit_is_monotone(lut@, r1, r2, lut@.len() - 1);
    }
    assert(i1.unwrap() <= i2.unwrap());
}

/// The nodes of tests.rs: sc7180.c `ebi` (buswidth 4, channels 2; BCM MC0) and `qns_llcc` (buswidth 16, channels 1; BCM SH0).
pub open spec fn ebi_spec(avg: u64, peak: u64) -> NodeReq {
    NodeReq { buswidth: 4, channels: 2, req: Req { avg_kbps: avg, peak_kbps: peak } }
}

pub open spec fn llcc_spec(avg: u64, peak: u64) -> NodeReq {
    NodeReq { buswidth: 16, channels: 1, req: Req { avg_kbps: avg, peak_kbps: peak } }
}

pub open spec fn node_spec(buswidth: u16, avg: u64) -> NodeReq {
    NodeReq { buswidth, channels: 1, req: Req { avg_kbps: avg, peak_kbps: 0 } }
}

/// cmd-db aux with width 4, as in tests.rs (unit 1000, scale 1000 there).
pub open spec fn aux_spec(unit: u32, vote_scale: u64) -> BcmAux {
    BcmAux { unit, width: 4, vote_scale }
}

/// 7. The specs the `ensures` pin the code to give Linux's numbers, each worked by hand from bcm-voter.c:102-115 (term =
/// bcm_div(sum_avg * width, buswidth * channels) and bcm_div(max_peak * width, buswidth); vote = bcm_div(agg * vote_scale, unit))
/// and tcs_cmd_gen's clamp (bcm-voter.c:140-144). A bug planted in a function and its spec alike changes one of these numbers.
proof fn the_votes_are_linuxs()
    ensures
        // tests.rs `the_narrowest_node_decides`: CN0 on qnm_snoc (8) and qhs_venus_cfg (4), 125 kBps each:
        // max(125 * 4 / 8, 125 * 4 / 4) = 125, times 1000 over 1000.
        vote_x_spec(aux_spec(1000, 1000), seq![node_spec(8, 125), node_spec(4, 125)]) == 125,
        // avg over buswidth * channels: ebi avg 2500: 2500 * 4 / (4 * 2) = 1250.
        vote_x_spec(aux_spec(1000, 1000), seq![ebi_spec(2500, 0)]) == 1250,
        vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(2500, 0)]) == 0,
        // peak over buswidth only: ebi 2500 * 4 / 4 = 2500, qns_llcc 2500 * 4 / 16 = 625.
        vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 2500)]) == 2500,
        vote_y_spec(aux_spec(1000, 1000), seq![llcc_spec(0, 2500)]) == 625,
        // tcs_cmd_gen: 0x3fff stays, 0x4000 is cut to 0x3fff.
        vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 0x3fff)]) == 0x3fff,
        vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 0x4000)]) == 0x3fff,
        // A vote_scale of 0 is icc-rpmh.c:183's 1000: ebi(2500, 7) gives (1250, 7), as with 1000 (scale 1 would give (1, 1)).
        vote_x_spec(aux_spec(1000, 0), seq![ebi_spec(2500, 7)]) == 1250,
        vote_y_spec(aux_spec(1000, 0), seq![ebi_spec(2500, 7)]) == 7,
        // bcm_div: a small non-zero vote rounds up to 1 (bcm-voter.c:53): ebi peak 1 with unit 1_000_000: 1 * 1000 / 1e6 -> 1.
        vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 1)]) == 1,
        // sc7180.dtsi CPU OPP pairs, DDR cell on MC0 (ebi) and SH0 (qns_llcc), avg 0 (no opp-avg-kBps).
        // With tests.rs's unit 1000 the lowest pair (1200000, 4800000) is cut: 1200000 * 4 / 4 * 1000 / 1000 > 0x3fff.
        vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 1_200_000)]) == 0x3fff,
        // With a unit of 1_000_000 (illustrative; the laptop's cmd-db unit is not recorded in the repo):
        // 1200000 * 4 / 4 * 1000 / 1e6 = 1200 on MC0, 1200000 * 4 / 16 * 1000 / 1e6 = 300 on SH0;
        // the top pair (8532000, 23347200): 8532000 on MC0 -> 8532, on SH0 8532000 * 4 / 16 = 2133000 -> 2133.
        vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 1_200_000)]) == 1200,
        vote_y_spec(aux_spec(1_000_000, 1000), seq![llcc_spec(0, 1_200_000)]) == 300,
        vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 8_532_000)]) == 8532,
        vote_y_spec(aux_spec(1_000_000, 1000), seq![llcc_spec(0, 8_532_000)]) == 2133,
        vote_x_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 8_532_000)]) == 0,
{
    assert(vote_x_spec(aux_spec(1000, 1000), seq![node_spec(8, 125), node_spec(4, 125)]) == 125) by (compute_only);
    assert(vote_x_spec(aux_spec(1000, 1000), seq![ebi_spec(2500, 0)]) == 1250) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(2500, 0)]) == 0) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 2500)]) == 2500) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![llcc_spec(0, 2500)]) == 625) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 0x3fff)]) == 0x3fff) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 0x4000)]) == 0x3fff) by (compute_only);
    assert(vote_x_spec(aux_spec(1000, 0), seq![ebi_spec(2500, 7)]) == 1250) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 0), seq![ebi_spec(2500, 7)]) == 7) by (compute_only);
    assert(vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 1)]) == 1) by (compute_only);
    assert(vote_y_spec(aux_spec(1000, 1000), seq![ebi_spec(0, 1_200_000)]) == 0x3fff) by (compute_only);
    assert(vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 1_200_000)]) == 1200) by (compute_only);
    assert(vote_y_spec(aux_spec(1_000_000, 1000), seq![llcc_spec(0, 1_200_000)]) == 300) by (compute_only);
    assert(vote_y_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 8_532_000)]) == 8532) by (compute_only);
    assert(vote_y_spec(aux_spec(1_000_000, 1000), seq![llcc_spec(0, 8_532_000)]) == 2133) by (compute_only);
    assert(vote_x_spec(aux_spec(1_000_000, 1000), seq![ebi_spec(0, 8_532_000)]) == 0) by (compute_only);
}

/// 7. `l3_index`'s spec on tests.rs's LUT (sc7180 L3 rows 300, 576, 768, 1017.6 MHz): osm-l3.c:141-144, the first entry >= rate
/// below the last, else the last; and the first of equal entries.
proof fn the_l3_rows_are_linuxs()
    ensures
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 0, 3) == 0,
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 300_000_000, 3) == 0,
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 576_000_000, 3) == 1,
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 576_000_001, 3) == 2,
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 1_017_600_000, 3) == 3,
        l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 2_000_000_000, 3) == 3,
        l3_first_spec(seq![5u64, 10, 10, 10], 10, 3) == 1,
{
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 0, 3) == 0) by (compute_only);
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 300_000_000, 3) == 0) by (compute_only);
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 576_000_000, 3) == 1) by (compute_only);
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 576_000_001, 3) == 2) by (compute_only);
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 1_017_600_000, 3) == 3) by (compute_only);
    assert(l3_first_spec(seq![300_000_000u64, 576_000_000, 768_000_000, 1_017_600_000], 2_000_000_000, 3) == 3) by (compute_only);
    assert(l3_first_spec(seq![5u64, 10, 10, 10], 10, 3) == 1) by (compute_only);
}

/// 7. The shipped `l3_rate_hz` gives osm-l3.c:138-139's rate: icc_units_to_bps (kBps * 1000) over the buswidth, here sc7180's
/// L3 buswidth of 32 (tests.rs `l3_rate`): the lowest L3 cell 4800000 -> 150 MHz, the top 23347200 -> 729.6 MHz, 1 -> 31, 0 -> 0.
fn the_l3_rates_are_linuxs() {
    let lowest = l3_rate_hz(4_800_000, 32);
    assert(lowest == 150_000_000);
    let top = l3_rate_hz(23_347_200, 32);
    assert(top == 729_600_000);
    let one = l3_rate_hz(1, 32);
    assert(one == 31);
    let zero = l3_rate_hz(0, 32);
    assert(zero == 0);
}

} // verus!

fn main() {}
