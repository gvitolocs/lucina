// SPDX-License-Identifier: GPL-2.0-only

//! Juno's CPU-to-memory bandwidth votes (patch 1231): what a CPU frequency step asks of the interconnect, the way Linux v6.12
//! computes it. Behaviour is Linux's (GPL-2.0), the code is written fresh from it; each function names the Linux function it follows.
//!
//! The chain, for one frequency step of a cluster:
//! 1. [`opp_bw`] (`qcom_cpufreq_set_bw`): the OPP at that frequency holds two `opp-peak-kBps` cells, path 0 to DDR and path 1 to L3;
//! 2. [`aggregate`] (`icc_std_aggregate`, `qcom_icc_aggregate`): requests on one node add up in avg and take the largest peak;
//! 3. DDR goes through RPMh: [`bcm_votes`] / [`bcm_mask_vote`] (`bcm_aggregate`, `bcm_aggregate_mask`, `tcs_cmd_gen`) turn the nodes
//!    of a Bus Clock Manager into the two 14-bit numbers of its command;
//! 4. L3 goes through the OSM: [`l3_rate_hz`] and [`l3_index`] (`qcom_osm_l3_set`) pick the LUT row written to the perf-state register.
//!
//! Arithmetic: Linux does `sum_avg * width` and `agg * vote_scale` in u64 and may wrap. Here every product is computed in u128
//! (no input can overflow it) and the vote is clamped to [`VOTE_MAX`] as `tcs_cmd_gen` does. Before the scale multiplication an
//! aggregate above 2^47 is cut to 2^47: with a unit below 2^32 it is above `VOTE_MAX` times the unit either way, so the clamped vote
//! is the same as the unbounded product's (proved). A zero divisor (a unit, buswidth or channel count of 0), which Linux would divide
//! by, gives 0 here; the callers reject such tables. Where Linux uses u32 (`icc_std_aggregate`'s sums wrap), [`aggregate`] saturates
//! in u64, which only differs beyond the range any real request reaches.
//! Where the numbers agree with Linux's u64 ones: whenever Linux's own products do not wrap, that is `sum_avg * width`,
//! `max_peak * width` and `agg * vote_scale` all below 2^64 (bcm-voter.c:102, 106, 111, 114); every sc7180 request is far below
//! (the top CPU cell 8532000 kBps on MC0: agg * scale = 3.4e10). Above that Linux's vote is the wrapped product's and this crate's
//! the true one. The specs that name Linux's values (`raw_vote_spec`, `linux_forced_spec`, `stored_spec`, and the lemmas of
//! rules.rs over them) compute over the integers: they ARE Linux's u64 values inside that no-wrap range, and outside it they are
//! the true values, not Linux's wrapped ones. `buswidth * channels` is computed by Linux in C `int` from two u16
//! fields (icc-rpmh.h:100-101, bcm-voter.c:103), which overflows (undefined) at 2^31; here it is u32, exact up to 65535^2.
//!
//! Which vote: `vote_x_spec` / `vote_y_spec` and what [`bcm_vote`] / [`bcm_votes`] return are the COMMANDED 14-bit numbers,
//! `tcs_cmd_gen`'s clamp applied (bcm-voter.c:140-144, `BCM_TCS_CMD_VOTE_MASK` 0x3fff at include/soc/qcom/tcs.h:68). Linux stores
//! the UNCLAMPED u64 numbers in `bcm->vote_x/vote_y[bucket]` (icc-rpmh.h:132-133, set at bcm-voter.c:112, 115, 120-123) and reads
//! them unclamped twice: in the keepalive test (bcm-voter.c:118-119; proved the same test, rules.rs `the_keepalive_test_is_linuxs`)
//! and in the WAKE-vs-SLEEP comparison of `qcom_icc_bcm_voter_commit` (bcm-voter.c:331-334); `stored_spec` is that stored value,
//! and rules.rs proves the command is its clamp (`the_command_is_the_clamped_stored_vote`) and when the two comparisons differ
//! (`the_wake_sleep_test_on_commands`, for `bcm_aggregate` BCMs). Buckets are indexed as `QCOM_ICC_BUCKET_AMC/WAKE/SLEEP` = 0/1/2,
//! and there are `QCOM_ICC_NUM_BUCKETS` = 3 of them (include/dt-bindings/interconnect/qcom,icc.h:14-17): the `[_; 3]` and
//! `0 <= i < 3` of [`bcm_votes`], and its `i < 2` (AMC and WAKE, bcm-voter.c:120-123).
//!
//! The mask path: [`bcm_mask_vote`] returns Linux's STORED pair of `bcm_aggregate_mask` (bcm-voter.c:72-87: 0 or `enable_mask`);
//! the command clamps it like any other (bcm-voter.c:140-144, applied at 178-179), which is the identity for an `enable_mask` of at
//! most [`VOTE_MAX`] (sc7180's ACV: 8). So for such a mask the WAKE-vs-SLEEP test on commands is Linux's test on stored values
//! (rules.rs `the_mask_path_never_diverges`).
//!
//! Verus proves this very file (`verification/juno-cpubw/rules.rs`, D-011). Proved there, for every input:
//! 1. [`bcm_vote`] never exceeds [`VOTE_MAX`] in either number, [`bcm_mask_vote`] returns only 0 or the enable mask;
//! 2. [`bcm_vote`] is monotonic: no node asking for more (same geometry) lowers x or y; [`bcm_votes`] applies Linux's keepalive rule
//!    (AMC at (0, 0) on a keepalive BCM: AMC and WAKE become (1, 1), SLEEP is left alone) and otherwise returns each bucket's vote;
//! 3. [`l3_index`] is the first index below the last whose LUT entry is at least the rate, else the last;
//! 4. no bandwidth on any node: [`bcm_vote`] gives (0, 0);
//! 5. [`aggregate`] takes the largest peak and the saturating sum of avg; [`opp_bw`] answers only on an exact frequency, with that
//!    OPP's pair; [`opp_pair_known`] is true exactly when some OPP has that pair.
//!
//! What the crate leaves to its callers, as Linux does it outside the functions here:
//! - the `init_avg` / `init_peak` floor on a node's requests (icc-rpmh.c:101-104);
//! - the `valid` bit of a command, which Linux clears when both votes are 0 (bcm-voter.c:137-138);
//! - OPPs are matched in kHz as the table holds them, and the OPP's `available` flag is ignored (every entry counts), where Linux
//!   asks `dev_pm_opp_find_freq_exact(dev, freq_khz * 1000, true)` (qcom-cpufreq-hw.c:75, 84), which walks the frequency-sorted
//!   OPP list and takes the first entry with `available == true` and that frequency (drivers/opp/core.c:512-516): a table that
//!   holds a disabled OPP must leave it out, and Linux returns an error and votes nothing when there is no available match
//!   (qcom-cpufreq-hw.c:85-86);
//! - that cell 0 of the pair is the peak request on every node of the DDR path and cell 1 on the L3 path (`dev_pm_opp_set_opp`,
//!   qcom-cpufreq-hw.c:88, sets each `opp-peak-kBps` cell on the path of the same index);
//! - which BCM takes which vote: Linux uses `bcm_aggregate_mask` exactly when the BCM has a non-zero `enable_mask`, else
//!   `bcm_aggregate` (bcm-voter.c:289-292); for a mask BCM the caller calls [`bcm_mask_vote`] per bucket with `keepalive` set to
//!   the BCM's flag for AMC and WAKE and to false for SLEEP (bcm-voter.c:83-88 force AMC and WAKE only);
//! - the WAKE-vs-SLEEP decision (bcm-voter.c:331-334) on the stored, unclamped votes. A caller that decides on the commanded numbers
//!   decides the same, except when in x or y both WAKE and SLEEP are at or above [`VOTE_MAX`] stored and differ there (and are
//!   equal commanded in both). That case is reachable (rules.rs `the_wake_sleep_case_happens`). There Linux and patch 1231 differ:
//!   Linux writes both WAKE and SLEEP command words, equal (bcm-voter.c:331-359; `rpmh_flush` sends the batches whatever their
//!   data, drivers/soc/qcom/rpmh.c:449-470). Patch 1231 omits them: its `commit` records WAKE and SLEEP in the cache, and
//!   `Cache::valid` and `flush` drop a pair whose two words are equal (drivers/lucina-modem-rpmh/src/rpmh.rs:51-70), so no WAKE or
//!   SLEEP command is written for that BCM. This is unexercised today because the system never sleeps (the WAKE and SLEEP TCSes
//!   never run). Nothing here claims that RPMh then holds the WAKE/SLEEP votes Linux leaves; that is an open gap
//!   (docs/ci/PROOF-COVERAGE.md, list A).
//!
//! Not proved: that the cells in the table are the device tree's, and that the kernel writes the result where Linux does.
//! The callers' side of the obligations above is the kernel's (patch 1231), not proved here.
#![no_std]
#![forbid(unsafe_code)]
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// One CPU OPP from the DT: opp-hz / 1000 and the two opp-peak-kBps cells (path 0 = DDR, path 1 = L3).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OppBw {
    pub khz: u32,
    pub ddr_kbps: u32,
    pub l3_kbps: u32,
}

/// `qcom_cpufreq_set_bw` -> `dev_pm_opp_find_freq_exact`: the OPP whose frequency equals `khz` exactly (the first, as Linux walks
/// the table in order); None if there is none (Linux then returns an error and sets no bandwidth).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.is_some() ==> exists|i: int| 0 <= i < table@.len() && table@[i].khz == khz
            && r == Some((table@[i].ddr_kbps, table@[i].l3_kbps))
            && forall|j: int| 0 <= j < i ==> table@[j].khz != khz,
        r.is_none() ==> forall|j: int| 0 <= j < table@.len() ==> table@[j].khz != khz,
))]
pub fn opp_bw(table: &[OppBw], khz: u32) -> Option<(u32, u32)> {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= table.len(),
            forall|j: int| 0 <= j < i ==> table@[j].khz != khz,
        decreases table.len() - i
    ))]
    while i < table.len() {
        if table[i].khz == khz {
            return Some((table[i].ddr_kbps, table[i].l3_kbps));
        }
        i += 1;
    }
    None
}

/// Whether (`ddr_kbps`, `l3_kbps`) is one of the table's own `opp-peak-kBps` pairs. Not a Linux function: the check on the static
/// test request of `/proc/lucina-rpmh` (patch 1231), so that the test asks only for what `qcom_cpufreq_set_bw` asks at some clock.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == exists|i: int| 0 <= i < table@.len() && table@[i].ddr_kbps == ddr_kbps && table@[i].l3_kbps == l3_kbps,
))]
pub fn opp_pair_known(table: &[OppBw], ddr_kbps: u32, l3_kbps: u32) -> bool {
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= table.len(),
            forall|j: int| 0 <= j < i ==> !(table@[j].ddr_kbps == ddr_kbps && table@[j].l3_kbps == l3_kbps),
        decreases table.len() - i
    ))]
    while i < table.len() {
        if table[i].ddr_kbps == ddr_kbps && table[i].l3_kbps == l3_kbps {
            return true;
        }
        i += 1;
    }
    false
}

/// One request on an icc node: avg and peak in kBps (icc units).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Req {
    pub avg_kbps: u64,
    pub peak_kbps: u64,
}

/// `icc_std_aggregate` (drivers/interconnect/core.c) / `qcom_icc_aggregate`: avg sums (saturating), peak is the max.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.peak_kbps == max_spec(a.peak_kbps as int, b.peak_kbps as int),
        r.avg_kbps == min_spec(a.avg_kbps as int + b.avg_kbps as int, u64::MAX as int),
))]
pub fn aggregate(a: Req, b: Req) -> Req {
    let avg_kbps = if a.avg_kbps > u64::MAX - b.avg_kbps {
        u64::MAX
    } else {
        a.avg_kbps + b.avg_kbps
    };
    let peak_kbps = if a.peak_kbps >= b.peak_kbps {
        a.peak_kbps
    } else {
        b.peak_kbps
    };
    Req {
        avg_kbps,
        peak_kbps,
    }
}

/// cmd-db aux data of a BCM (unit, width) and the BCM's `vote_scale` (`icc-rpmh`'s default 1000 when 0).
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BcmAux {
    pub unit: u32,
    pub width: u16,
    pub vote_scale: u64,
}

/// One icc node attached to the BCM: buswidth, channels and that node's aggregated request in one bucket.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeReq {
    pub buswidth: u16,
    pub channels: u16,
    pub req: Req,
}

/// `BCM_TCS_CMD_VOTE_MASK` from include/soc/qcom/tcs.h:68.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const VOTE_MAX: u32 = 0x3fff;

/// Above this an aggregate is cut before the scale multiplication (see the module comment).
#[cfg_attr(verus_keep_ghost, verus_verify)]
const AGG_CAP: u128 = 0x8000_0000_0000;

/// `bcm_div` in u128: a non-zero numerator below the base rounds up to 1; a zero base gives 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == div_spec(num as int, base as int),
))]
fn div128(num: u128, base: u128) -> u128 {
    if base == 0 {
        0
    } else if num != 0 && num < base {
        1
    } else {
        num / base
    }
}

/// `bcm_div` (bcm-voter.c): a non-zero numerator below the base rounds up to 1 (a zero base gives 0, where Linux divides by zero).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == div_spec(num as int, base as int),
))]
pub fn bcm_div(num: u64, base: u64) -> u64 {
    div128(num as u128, base as u128) as u64
}

/// `num * mul` for a u64 and a u16: fits in a u128.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == a as int * m as int,
))]
fn mul_u64_u16(a: u64, m: u16) -> u128 {
    #[cfg(verus_keep_ghost)]
    proof! {
        vstd::arithmetic::mul::lemma_mul_upper_bound(a as int, u64::MAX as int, m as int, u16::MAX as int);
        assert(u64::MAX as int * u16::MAX as int <= u128::MAX as int) by (compute);
    }
    (a as u128) * (m as u128)
}

/// `a * b` for two u16: fits in a u32.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == a as int * b as int,
))]
fn mul_u16_u16(a: u16, b: u16) -> u32 {
    #[cfg(verus_keep_ghost)]
    proof! {
        vstd::arithmetic::mul::lemma_mul_upper_bound(a as int, u16::MAX as int, b as int, u16::MAX as int);
        assert(u16::MAX as int * u16::MAX as int <= u32::MAX as int) by (compute);
    }
    (a as u32) * (b as u32)
}

/// `agg * vote_scale / unit` (`bcm_div`), cut to [`VOTE_MAX`] as `tcs_cmd_gen` does.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r as int == vote_spec(agg as int, aux),
))]
fn scaled_vote(agg: u128, aux: BcmAux) -> u32 {
    let scale: u128 = if aux.vote_scale == 0 {
        1000
    } else {
        aux.vote_scale as u128
    };
    let a = if agg > AGG_CAP { AGG_CAP } else { agg };
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_cap_keeps_vote(agg as int, scale_spec(aux), aux.unit as int);
        vstd::arithmetic::mul::lemma_mul_upper_bound(a as int, AGG_CAP as int, scale as int, u64::MAX as int);
        assert(AGG_CAP as int * u64::MAX as int <= u128::MAX as int) by (compute);
    }
    let v = div128(a * scale, aux.unit as u128);
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(scale as int == scale_spec(aux));
        assert(a as int == if agg as int > 0x8000_0000_0000int { 0x8000_0000_0000int } else { agg as int });
        assert(v as int == div_spec(a as int * scale as int, aux.unit as int));
    }
    if v > VOTE_MAX as u128 {
        VOTE_MAX
    } else {
        v as u32
    }
}

/// `bcm_aggregate` (bcm-voter.c) for ONE bucket, then `tcs_cmd_gen`'s clamp to [`VOTE_MAX`]: returns (vote_x, vote_y). Per node the
/// avg term is `bcm_div(avg * width, buswidth * channels)` and the peak term `bcm_div(peak * width, buswidth)`; the BCM takes the
/// largest of each, times `vote_scale`, over `unit`. The keepalive is not here: Linux applies it across buckets, see [`bcm_votes`].
/// The result is the commanded (clamped) vote, not Linux's stored `bcm->vote_x/vote_y` (module comment).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r.0 <= VOTE_MAX,
        r.1 <= VOTE_MAX,
        r.0 as int == vote_final_x_spec(aux, nodes@, false),
        r.1 as int == vote_final_y_spec(aux, nodes@, false),
))]
pub fn bcm_vote(aux: BcmAux, nodes: &[NodeReq]) -> (u32, u32) {
    let mut agg_avg: u128 = 0;
    let mut agg_peak: u128 = 0;
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= nodes.len(),
            agg_avg as int == agg_avg_spec(aux, nodes@, i as int),
            agg_peak as int == agg_peak_spec(aux, nodes@, i as int),
        decreases nodes.len() - i
    ))]
    while i < nodes.len() {
        let n = nodes[i];
        let avg = div128(
            mul_u64_u16(n.req.avg_kbps, aux.width),
            mul_u16_u16(n.buswidth, n.channels) as u128,
        );
        let peak = div128(mul_u64_u16(n.req.peak_kbps, aux.width), n.buswidth as u128);
        if avg > agg_avg {
            agg_avg = avg;
        }
        if peak > agg_peak {
            agg_peak = peak;
        }
        i += 1;
    }
    let x = scaled_vote(agg_avg, aux);
    let y = scaled_vote(agg_peak, aux);
    (x, y)
}

/// `bcm_aggregate` (bcm-voter.c:118-124) over the three buckets of one BCM, index 0 = AMC, 1 = WAKE, 2 = SLEEP (qcom,icc.h:14-17): each bucket's
/// vote as [`bcm_vote`] gives it, then Linux's keepalive rule once: it looks at the AMC bucket only, and when that is (0, 0) on a
/// BCM with keepalive it sets AMC and WAKE to (1, 1) and leaves SLEEP alone. Stated per bucket as bcm-voter.c:118-124 reads: bucket
/// i is (1, 1) when i is AMC or WAKE and [`forced_spec`] holds for the AMC bucket, else that bucket's own vote (`bucket_vote_spec`);
/// every number is at most [`VOTE_MAX`]. These are the commanded numbers; Linux's stored ones are `stored_spec` (module comment). Linux tests the AMC vote before `tcs_cmd_gen`'s clamp; the clamp keeps 0 at 0 and nothing else at 0, so testing the
/// clamped vote is the same test (`the_keepalive_test_is_linuxs` in rules.rs).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        forall|i: int| 0 <= i < 3 ==> r@[i].0 <= VOTE_MAX && r@[i].1 <= VOTE_MAX,
        forall|i: int| 0 <= i < 3 ==> r@[i].0 as int == bucket_vote_spec(aux, buckets@[0]@, buckets@[i]@, keepalive, i).0,
        forall|i: int| 0 <= i < 3 ==> r@[i].1 as int == bucket_vote_spec(aux, buckets@[0]@, buckets@[i]@, keepalive, i).1,
))]
pub fn bcm_votes(aux: BcmAux, buckets: [&[NodeReq]; 3], keepalive: bool) -> [(u32, u32); 3] {
    let mut r = [
        bcm_vote(aux, buckets[0]),
        bcm_vote(aux, buckets[1]),
        bcm_vote(aux, buckets[2]),
    ];
    if keepalive && r[0].0 == 0 && r[0].1 == 0 {
        r[0] = (1, 1);
        r[1] = (1, 1);
    }
    r
}

/// `bcm_aggregate_mask` (bcm-voter.c) for one bucket: any non-zero avg or peak on any node -> (0, enable_mask), else (0, 0);
/// `keepalive` -> (enable_mask, enable_mask). Linux forces the mask on the AMC and WAKE buckets only (bcm-voter.c:83-88): a caller
/// passes `keepalive = false` for SLEEP: the parameter means "this bucket is forced", not Linux's `bcm->keepalive` (which
/// [`bcm_votes`] takes). Only for a BCM with a non-zero `enable_mask` (bcm-voter.c:289-292). sc7180's one mask BCM, ACV
/// (enable_mask BIT(3), sc7180.c:1239-1245), has no keepalive. The result is Linux's stored pair; the command is its clamp, the
/// identity for `enable_mask <= VOTE_MAX` (module comment).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (0u32, 0u32) || r == (0u32, enable_mask) || r == (enable_mask, enable_mask),
        keepalive ==> r == (enable_mask, enable_mask),
        !keepalive ==> r == (if exists|i: int| 0 <= i < nodes@.len() && (nodes@[i].req.avg_kbps != 0 || nodes@[i].req.peak_kbps != 0)
            { (0u32, enable_mask) } else { (0u32, 0u32) }),
        r == mask_vote_spec(enable_mask, nodes@, keepalive),
))]
pub fn bcm_mask_vote(enable_mask: u32, nodes: &[NodeReq], keepalive: bool) -> (u32, u32) {
    if keepalive {
        return (enable_mask, enable_mask);
    }
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= nodes.len(),
            !keepalive,
            forall|j: int| 0 <= j < i ==> nodes@[j].req.avg_kbps == 0 && nodes@[j].req.peak_kbps == 0,
        decreases nodes.len() - i
    ))]
    while i < nodes.len() {
        if nodes[i].req.avg_kbps != 0 || nodes[i].req.peak_kbps != 0 {
            return (0, enable_mask);
        }
        i += 1;
    }
    (0, 0)
}

/// `qcom_osm_l3_set` (osm-l3.c:138-139): rate = `icc_units_to_bps(dst->peak_bw) / qn->buswidth`, the DESTINATION node's
/// aggregated peak over the SOURCE node's buswidth (osm-l3.c:134); the caller passes those two. `icc_units_to_bps(bw)` is
/// `(bw) * 1000ULL` (include/linux/interconnect-provider.h:12): a u64 product, exact for Linux's u32 `peak_bw` (the CPU's top L3
/// cell 23347200 * 1000 does not fit u32 but fits u64). Here the product is u128: a peak above u64::MAX / 1000, which Linux's u32
/// cannot hold, or a buswidth of 0, where Linux divides by zero, gives u64::MAX; [`l3_index`] then picks the last row unless some
/// earlier entry is u64::MAX itself.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        buswidth > 0 && peak_kbps as int * 1000 / buswidth as int <= u64::MAX as int ==> r as int == peak_kbps as int * 1000 / buswidth as int,
        buswidth > 0 && peak_kbps as int * 1000 / buswidth as int > u64::MAX as int ==> r == u64::MAX,
        buswidth == 0 ==> r == u64::MAX,
))]
pub fn l3_rate_hz(peak_kbps: u64, buswidth: u32) -> u64 {
    if buswidth == 0 {
        return u64::MAX;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        vstd::arithmetic::mul::lemma_mul_upper_bound(peak_kbps as int, u64::MAX as int, 1000, 1000);
        assert(u64::MAX as int * 1000 <= u128::MAX as int) by (compute);
    }
    let rate = (peak_kbps as u128) * 1000 / (buswidth as u128);
    if rate > u64::MAX as u128 {
        u64::MAX
    } else {
        rate as u64
    }
}

/// `qcom_osm_l3_set` (osm-l3.c:141-144): the first index in 0..len-1 whose LUT entry (Hz) is >= `rate_hz`, else len-1 (the last).
/// Empty LUT -> None (Linux's `max_state - 1` would wrap there; its probe sets `max_state` from the LUT it reads, osm-l3.c:207-226).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        lut_hz@.len() == 0 ==> r.is_none(),
        lut_hz@.len() > 0 ==> r.is_some(),
        lut_hz@.len() > 0 ==> r.unwrap() < lut_hz@.len(),
        lut_hz@.len() > 0 ==> forall|j: int| 0 <= j < r.unwrap() ==> lut_hz@[j] < rate_hz,
        lut_hz@.len() > 0 ==> r.unwrap() == lut_hz@.len() - 1 || lut_hz@[r.unwrap() as int] >= rate_hz,
        lut_hz@.len() > 0 ==> r == Some(l3_first_spec(lut_hz@, rate_hz, lut_hz@.len() - 1) as usize),
))]
pub fn l3_index(lut_hz: &[u64], rate_hz: u64) -> Option<usize> {
    if lut_hz.is_empty() {
        return None;
    }
    let last = lut_hz.len() - 1;
    let mut i = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= last,
            last + 1 == lut_hz.len(),
            forall|j: int| 0 <= j < i ==> lut_hz@[j] < rate_hz,
            l3_first_spec(lut_hz@, rate_hz, i as int) == i as int,
        decreases last - i
    ))]
    while i < last {
        if lut_hz[i] >= rate_hz {
            #[cfg(verus_keep_ghost)]
            proof! {
                lemma_l3_first_stays(lut_hz@, rate_hz, i as int, last as int);
            }
            return Some(i);
        }
        i += 1;
    }
    Some(last)
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn max_spec(a: int, b: int) -> int {
    if a >= b { a } else { b }
}

pub open spec fn min_spec(a: int, b: int) -> int {
    if a <= b { a } else { b }
}

/// `bcm_div` over the integers.
pub open spec fn div_spec(num: int, base: int) -> int {
    if base == 0 {
        0
    } else if num != 0 && num < base {
        1
    } else {
        num / base
    }
}

pub open spec fn scale_spec(aux: BcmAux) -> int {
    if aux.vote_scale == 0 { 1000 } else { aux.vote_scale as int }
}

/// The vote of an aggregate: `bcm_div(agg * scale, unit)` cut to `VOTE_MAX`.
pub open spec fn vote_spec(agg: int, aux: BcmAux) -> int {
    min_spec(VOTE_MAX as int, div_spec(agg * scale_spec(aux), aux.unit as int))
}

pub open spec fn avg_term_spec(aux: BcmAux, n: NodeReq) -> int {
    div_spec(n.req.avg_kbps as int * aux.width as int, n.buswidth as int * n.channels as int)
}

pub open spec fn peak_term_spec(aux: BcmAux, n: NodeReq) -> int {
    div_spec(n.req.peak_kbps as int * aux.width as int, n.buswidth as int)
}

/// The largest avg term over the first `n` nodes.
pub open spec fn agg_avg_spec(aux: BcmAux, nodes: Seq<NodeReq>, n: int) -> int
    decreases n,
{
    if n <= 0 { 0 } else { max_spec(agg_avg_spec(aux, nodes, n - 1), avg_term_spec(aux, nodes[n - 1])) }
}

/// The largest peak term over the first `n` nodes.
pub open spec fn agg_peak_spec(aux: BcmAux, nodes: Seq<NodeReq>, n: int) -> int
    decreases n,
{
    if n <= 0 { 0 } else { max_spec(agg_peak_spec(aux, nodes, n - 1), peak_term_spec(aux, nodes[n - 1])) }
}

/// vote_x / vote_y of one bucket before the keepalive.
pub open spec fn vote_x_spec(aux: BcmAux, nodes: Seq<NodeReq>) -> int {
    vote_spec(agg_avg_spec(aux, nodes, nodes.len() as int), aux)
}

pub open spec fn vote_y_spec(aux: BcmAux, nodes: Seq<NodeReq>) -> int {
    vote_spec(agg_peak_spec(aux, nodes, nodes.len() as int), aux)
}

/// The keepalive: both zero gives (1, 1).
pub open spec fn forced_spec(aux: BcmAux, nodes: Seq<NodeReq>, keepalive: bool) -> bool {
    keepalive && vote_x_spec(aux, nodes) == 0 && vote_y_spec(aux, nodes) == 0
}

pub open spec fn vote_final_x_spec(aux: BcmAux, nodes: Seq<NodeReq>, keepalive: bool) -> int {
    if forced_spec(aux, nodes, keepalive) { 1 } else { vote_x_spec(aux, nodes) }
}

pub open spec fn vote_final_y_spec(aux: BcmAux, nodes: Seq<NodeReq>, keepalive: bool) -> int {
    if forced_spec(aux, nodes, keepalive) { 1 } else { vote_y_spec(aux, nodes) }
}

/// Bucket `i` of [`bcm_votes`], commanded: bcm-voter.c:118-124 (AMC and WAKE forced to (1, 1) when the AMC vote is (0, 0) on a
/// keepalive BCM; SLEEP and every unforced bucket their own vote).
pub open spec fn bucket_vote_spec(aux: BcmAux, amc: Seq<NodeReq>, own: Seq<NodeReq>, keepalive: bool, i: int) -> (int, int) {
    if i < 2 && forced_spec(aux, amc, keepalive) { (1, 1) } else { (vote_x_spec(aux, own), vote_y_spec(aux, own)) }
}

/// Linux's unclamped vote of an aggregate: `bcm_div(agg * vote_scale, unit)` (bcm-voter.c:111-115), over the integers.
pub open spec fn raw_vote_spec(agg: int, aux: BcmAux) -> int {
    div_spec(agg * scale_spec(aux), aux.unit as int)
}

/// Linux's own keepalive test on the stored AMC vote (bcm-voter.c:118-119).
pub open spec fn linux_forced_spec(aux: BcmAux, amc: Seq<NodeReq>, keepalive: bool) -> bool {
    keepalive && raw_vote_spec(agg_avg_spec(aux, amc, amc.len() as int), aux) == 0
        && raw_vote_spec(agg_peak_spec(aux, amc, amc.len() as int), aux) == 0
}

/// `bcm->vote_x[i]`, `bcm->vote_y[i]` as `bcm_aggregate` leaves them (bcm-voter.c:111-124): unclamped.
pub open spec fn stored_spec(aux: BcmAux, amc: Seq<NodeReq>, own: Seq<NodeReq>, keepalive: bool, i: int) -> (int, int) {
    if i < 2 && linux_forced_spec(aux, amc, keepalive) {
        (1, 1)
    } else {
        (raw_vote_spec(agg_avg_spec(aux, own, own.len() as int), aux), raw_vote_spec(agg_peak_spec(aux, own, own.len() as int), aux))
    }
}

/// `bcm_aggregate_mask` for one bucket (bcm-voter.c:71-88), `keepalive` meaning "this bucket is forced".
pub open spec fn mask_vote_spec(enable_mask: u32, nodes: Seq<NodeReq>, keepalive: bool) -> (u32, u32) {
    if keepalive {
        (enable_mask, enable_mask)
    } else if exists|i: int| 0 <= i < nodes.len() && (nodes[i].req.avg_kbps != 0 || nodes[i].req.peak_kbps != 0) {
        (0u32, enable_mask)
    } else {
        (0u32, 0u32)
    }
}

/// The first index below `n` whose LUT entry is at least `rate`, else `n`.
pub open spec fn l3_first_spec(lut: Seq<u64>, rate: u64, n: int) -> int
    decreases n,
{
    if n <= 0 {
        0
    } else if l3_first_spec(lut, rate, n - 1) < n - 1 {
        l3_first_spec(lut, rate, n - 1)
    } else if lut[n - 1] >= rate {
        n - 1
    } else {
        n
    }
}

/// A hit at `i` (nothing before it) stays the first for every longer prefix.
pub proof fn lemma_l3_first_stays(lut: Seq<u64>, rate: u64, i: int, n: int)
    requires
        0 <= i < n <= lut.len(),
        l3_first_spec(lut, rate, i) == i,
        lut[i] >= rate,
    ensures
        l3_first_spec(lut, rate, n) == i,
    decreases n,
{
    if n > i + 1 {
        lemma_l3_first_stays(lut, rate, i, n - 1);
    }
}

/// An aggregate above `AGG_CAP` votes the same as `AGG_CAP` (the unit is below 2^32 and the scale at least 1).
pub proof fn lemma_cap_keeps_vote(agg: int, scale: int, unit: int)
    requires
        0 <= agg,
        1 <= scale,
        0 <= unit < 0x1_0000_0000,
    ensures
        min_spec(VOTE_MAX as int, div_spec((if agg > 0x8000_0000_0000int { 0x8000_0000_0000int } else { agg }) * scale, unit))
            == min_spec(VOTE_MAX as int, div_spec(agg * scale, unit)),
{
    if agg > 0x8000_0000_0000int && unit > 0 {
        let cap = 0x8000_0000_0000int;
        vstd::arithmetic::mul::lemma_mul_inequality(1, scale, cap);
        vstd::arithmetic::mul::lemma_mul_inequality(1, scale, agg);
        assert(VOTE_MAX as int * unit <= VOTE_MAX as int * 0x1_0000_0000int) by (nonlinear_arith)
            requires unit <= 0x1_0000_0000int;
        assert(cap * scale >= VOTE_MAX as int * unit);
        assert(agg * scale >= VOTE_MAX as int * unit);
        assert(cap * scale >= unit);
        assert(agg * scale >= unit);
        assert((cap * scale) / unit >= VOTE_MAX as int) by (nonlinear_arith)
            requires cap * scale >= VOTE_MAX as int * unit, unit > 0;
        assert((agg * scale) / unit >= VOTE_MAX as int) by (nonlinear_arith)
            requires agg * scale >= VOTE_MAX as int * unit, unit > 0;
    }
}

} // verus!

#[cfg(test)]
mod tests;
