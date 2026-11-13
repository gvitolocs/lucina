//! Interconnect requests on one DRV, kept per consumer and voted per BCM as Linux v6.12 does: `drivers/interconnect/core.c`
//! (`aggregate_requests`, `icc_sync_state`), `drivers/interconnect/qcom/icc-rpmh.c` (`qcom_icc_aggregate`) and
//! `drivers/interconnect/qcom/bcm-voter.c` (`bcm_aggregate`, `bcm_aggregate_mask`, `qcom_icc_bcm_voter_commit`), GPL-2.0.
//! The per-node arithmetic is `lucina_cpubw`'s, which Verus checks.
//!
//! # The sum
//!
//! Every consumer's request ([`Request`], one `icc_path` of Linux) stays in the table until the consumer changes it. A BCM's vote is
//! worked out from all of them each time: per node of the BCM and per bucket its tag names (tag 0 counting as
//! `QCOM_ICC_TAG_ALWAYS`), the requests' avg are added and their peaks maxed (`qcom_icc_aggregate`, `icc-rpmh.c:84-111`), then
//! `bcm_aggregate` (`bcm-voter.c:91-125`) or `bcm_aggregate_mask` (`:61-89`) over the BCM's nodes. So a consumer that lets go
//! (a request of 0) takes only its own part out of the sum: what the others asked for stays.
//!
//! # The floor client
//!
//! Before the interconnect's `sync_state` Linux keeps each node's initial bandwidth as a floor under the sum, in every bucket
//! (`icc-rpmh.c:101-104`, `core.c:292-296`), and drops it in `icc_sync_state` (`core.c:1093-1120`). Linux's initial bandwidth is
//! `INT_MAX` (`core.c:955-961`); here it is what this DRV's TCSes held for the BCM before this kernel's first vote on it (the boot
//! firmware's, patch 0959), and it is applied to the BCM's vote rather than to its nodes, as that is what the firmware left. It is
//! one more consumer of every BCM, [`FLOOR`], whose vote is maxed with the others' and never summed away by them: a consumer's
//! release cannot take the firmware's level from a BCM while the floor client holds it. Sending 0 to a BCM whose floor was taken
//! away silently reset the laptop on 2026-11-05 (Venus, patch 1041), so the floor client stays until [`Icc::sync`], which nothing in
//! the kernel calls yet.
//!
//! # What is sent
//!
//! [`Icc::plan`] records a request and gives, for every BCM on its path, the vote of each bucket; the AMC votes go through an
//! ACTIVE TCS (`bcm::batches`, in VCD order); [`Icc::committed`] then records WAKE and SLEEP in the `rpmh.c` cache and says whether
//! a flush is due: `qcom_icc_bcm_voter_commit` writes WAKE and SLEEP only for a BCM whose two differ (`bcm-voter.c:325-359`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_cpubw::{BcmAux, NodeReq, Req};

use crate::arc::State;
use crate::bcm::{self, Aux, Node};
use crate::rpmh::Cache;

/// `QCOM_ICC_BUCKET_AMC`, `_WAKE`, `_SLEEP`.
pub const AMC: usize = 0;
pub const WAKE: usize = 1;
pub const SLEEP: usize = 2;
pub const BUCKETS: usize = 3;
/// `QCOM_ICC_TAG_AMC`, `_WAKE`, `_ALWAYS` of `dt-bindings/interconnect/qcom,icc.h`: the buckets a request is in.
pub const TAG_AMC: u32 = 1 << AMC;
pub const TAG_WAKE: u32 = 1 << WAKE;
pub const TAG_ALWAYS: u32 = TAG_AMC | TAG_WAKE | 1 << SLEEP;

/// The CPUs' path to memory, `MASTER_APPSS_PROC` (`gem_noc`) to `SLAVE_EBI1` (`mc_virt`), as `sc7180.c` declares its nodes:
/// `acm_apps0` (in `bcm_sh4`), `qns_llcc` (`bcm_sh0`), `llcc_mc` (none) and `ebi` (`bcm_mc0`, `bcm_acv`). The route is the one
/// `icc_path` finds: `acm_apps0` links to `SLAVE_LLCC`, `qns_llcc` to `MASTER_LLCC`, `llcc_mc` to `SLAVE_EBI1`. Venus's paths
/// are [`bcm::VIDEO_MEM`] and [`bcm::CPU_CFG`].
pub const CPU_DDR: [Node; 4] = [
    Node {
        name: "acm_apps0",
        channels: 1,
        buswidth: 16,
        bcms: &["SH4"],
    },
    Node {
        name: "qns_llcc",
        channels: 1,
        buswidth: 16,
        bcms: &["SH0"],
    },
    Node {
        name: "llcc_mc",
        channels: 2,
        buswidth: 4,
        bcms: &[],
    },
    Node {
        name: "ebi",
        channels: 2,
        buswidth: 4,
        bcms: &["ACV", "MC0"],
    },
];

/// `bcm_acv` of `sc7180.c`: the one BCM with an `enable_mask` (`bcm_aggregate_mask`), and not `keepalive`.
pub const ENABLE_MASKS: [(&str, u32); 1] = [("ACV", 1 << 3)];

/// `qcom_icc_bcm_init`: `vote_scale` is 1000 when the BCM sets none, as no BCM of `sc7180.c` does.
pub const VOTE_SCALE: u64 = 1000;

/// The name the floor client has in reports.
pub const FLOOR: &str = "floor (the boot firmware's)";

/// One `icc_set_bw` on a path by consumer `user`, with the path's tag (0 counts as `QCOM_ICC_TAG_ALWAYS`).
#[derive(Clone, Copy, Debug)]
pub struct Request<U> {
    pub user: U,
    pub path: &'static [Node],
    pub tag: u32,
    pub avg_kbps: u64,
    pub peak_kbps: u64,
}

/// One BCM of the table.
#[derive(Clone, Debug)]
pub struct Bcm {
    pub name: &'static str,
    pub addr: u32,
    pub aux: Aux,
    /// The floor client's vote: what this DRV's TCSes held for the BCM before this kernel's first vote on it.
    pub floor: Option<(u32, u32)>,
    /// What was last sent, by bucket: AMC through an ACTIVE TCS, WAKE and SLEEP into the sleep and wake cache.
    pub sent: [Option<(u32, u32)>; BUCKETS],
}

/// One BCM as [`Icc::lines`] reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub name: &'static str,
    pub addr: u32,
    /// `bcm_aggregate` (or `bcm_aggregate_mask`) by bucket, over the consumers' requests alone.
    pub linux: [(u32, u32); BUCKETS],
    /// The floor client's vote (`None` before it was taken, and after [`Icc::sync`]).
    pub floor: Option<(u32, u32)>,
    pub sent: [Option<(u32, u32)>; BUCKETS],
    /// The largest x and y this DRV's TCSes hold for the BCM now, whoever sent them.
    pub held: (u32, u32),
}

/// What [`Icc::plan`] worked out for one request: the AMC votes to send, and every bucket's vote per BCM, for [`Icc::committed`]
/// once they are sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub amc: Vec<bcm::Vote>,
    votes: Vec<(usize, [(u32, u32); BUCKETS])>,
}

impl Plan {
    /// Each BCM of the path (by name) with the vote of each bucket.
    pub fn votes<'a, U>(
        &'a self,
        icc: &'a Icc<U>,
    ) -> impl Iterator<Item = (&'static str, [(u32, u32); BUCKETS])> + 'a {
        self.votes
            .iter()
            .map(move |&(at, v)| (icc.bcms[at].name, v))
    }
}

/// The BCMs on `path`, each once, in path order.
pub fn names_on(path: &[Node]) -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();
    for b in path.iter().flat_map(|n| n.bcms.iter()) {
        if !names.contains(b) {
            names.push(b);
        }
    }
    names
}

fn max_vote(a: (u32, u32), b: Option<(u32, u32)>) -> (u32, u32) {
    b.map_or(a, |b| (a.0.max(b.0), a.1.max(b.1)))
}

/// The interconnect requests of every consumer on one DRV and what each BCM was sent.
pub struct Icc<U> {
    /// Every path a consumer may ask for: a BCM's nodes are the nodes of these that name it.
    paths: &'static [&'static [Node]],
    pub bcms: Vec<Bcm>,
    /// The BCMs the command DB does not have, with why: a path through one is not voted.
    pub missing: Vec<(&'static str, String)>,
    pub requests: Vec<Request<U>>,
    /// `synced_state` of `core.c:29`: once set, the floor client holds nothing.
    synced: bool,
    /// Sends, and the last error.
    pub commits: u64,
    pub error: Option<String>,
}

impl<U: Copy + PartialEq + core::fmt::Debug> Icc<U> {
    /// A table for the BCMs of `paths`, none loaded yet ([`Icc::add`], [`Icc::add_missing`]).
    pub fn new(paths: &'static [&'static [Node]]) -> Self {
        Icc {
            paths,
            bcms: Vec::new(),
            missing: Vec::new(),
            requests: Vec::new(),
            synced: false,
            commits: 0,
            error: None,
        }
    }

    /// Every BCM of the paths, each once.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        for path in self.paths {
            for name in names_on(path) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// `qcom_icc_bcm_init` of one BCM: its address and aux data from the command DB.
    pub fn add(&mut self, name: &'static str, addr: u32, aux: Aux) {
        self.bcms.push(Bcm {
            name,
            addr,
            aux,
            floor: None,
            sent: [None; BUCKETS],
        });
    }

    /// A BCM the command DB does not have.
    pub fn add_missing(&mut self, name: &'static str, why: String) {
        self.missing.push((name, why));
    }

    /// Whether the interconnect's `sync_state` was done ([`Icc::sync`]).
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// `icc_sync_state` for the floor client: from now on it holds nothing, and each BCM is what its consumers ask. Nothing in the
    /// kernel calls this yet; the votes it changes are sent with the next request on each BCM.
    pub fn sync(&mut self) {
        self.synced = true;
    }

    /// The floor client's vote on `b`: its level until [`Icc::sync`].
    fn floor_vote(&self, b: &Bcm) -> Option<(u32, u32)> {
        if self.synced { None } else { b.floor }
    }

    /// `qcom_icc_aggregate` per node of the BCM, for each bucket, then `bcm_aggregate` (`bcm_aggregate_mask` for one with an
    /// `enable_mask`) with `tcs_cmd_gen`'s clamp, over the consumers' requests only. `extra` stands in for the requests of the
    /// same users, as if they were made. A node whose geometry does not fit 16 bits gives the BCM no votes rather than a
    /// truncated one.
    pub fn linux_votes(&self, b: &Bcm, extra: &[Request<U>]) -> [(u32, u32); BUCKETS] {
        let mut nodes: Vec<&Node> = Vec::new();
        for n in self
            .paths
            .iter()
            .flat_map(|p| p.iter())
            .filter(|n| n.bcms.contains(&b.name))
        {
            if !nodes.iter().any(|m| m.name == n.name) {
                nodes.push(n);
            }
        }
        let requests = self
            .requests
            .iter()
            .filter(|r| !extra.iter().any(|e| e.user == r.user))
            .chain(extra.iter());
        let mut geometry = Vec::new();
        for n in &nodes {
            match (u16::try_from(n.buswidth), u16::try_from(n.channels)) {
                (Ok(buswidth), Ok(channels)) => geometry.push((buswidth, channels)),
                _ => return [(0, 0); BUCKETS],
            }
        }
        let mut per_bucket = [Vec::new(), Vec::new(), Vec::new()];
        for (bucket, reqs) in per_bucket.iter_mut().enumerate() {
            for (n, &(buswidth, channels)) in nodes.iter().zip(geometry.iter()) {
                let req = requests
                    .clone()
                    .filter(|r| {
                        let tag = if r.tag == 0 { TAG_ALWAYS } else { r.tag };
                        tag & (1 << bucket) != 0 && r.path.iter().any(|p| p.name == n.name)
                    })
                    .fold(Req::default(), |acc, r| {
                        lucina_cpubw::aggregate(
                            acc,
                            Req {
                                avg_kbps: r.avg_kbps,
                                peak_kbps: r.peak_kbps,
                            },
                        )
                    });
                reqs.push(NodeReq {
                    buswidth,
                    channels,
                    req,
                });
            }
        }
        let keepalive = bcm::KEEPALIVE.contains(&b.name);
        if let Some(&(_, mask)) = ENABLE_MASKS.iter().find(|(name, _)| *name == b.name) {
            // bcm_aggregate_mask: keepalive forces AMC and WAKE only.
            return [
                lucina_cpubw::bcm_mask_vote(mask, &per_bucket[AMC], keepalive),
                lucina_cpubw::bcm_mask_vote(mask, &per_bucket[WAKE], keepalive),
                lucina_cpubw::bcm_mask_vote(mask, &per_bucket[SLEEP], false),
            ];
        }
        let aux = BcmAux {
            unit: b.aux.unit,
            width: b.aux.width,
            vote_scale: VOTE_SCALE,
        };
        lucina_cpubw::bcm_votes(
            aux,
            [&per_bucket[AMC], &per_bucket[WAKE], &per_bucket[SLEEP]],
            keepalive,
        )
    }

    /// The vote of each bucket for `b`: the consumers' ([`Icc::linux_votes`]), maxed with the floor client's.
    pub fn votes(&self, b: &Bcm, extra: &[Request<U>]) -> [(u32, u32); BUCKETS] {
        let floor = self.floor_vote(b);
        self.linux_votes(b, extra).map(|v| max_vote(v, floor))
    }

    /// `icc_set_bw` on `req.path` for `req.user`: the request replaces the user's last one, and the floor client takes its vote on
    /// each BCM of the path it had none on yet from `slots` (`Rsc::slots`, read before this request is sent). Returns the votes
    /// to send; nothing is recorded as sent until [`Icc::committed`].
    pub fn plan(
        &mut self,
        req: Request<U>,
        slots: &[(u32, u32, u32, u32)],
    ) -> Result<Plan, String> {
        let names = names_on(req.path);
        if let Some((name, why)) = self.missing.iter().find(|(name, _)| names.contains(name)) {
            return Err(format!("{}: {}", name, why));
        }
        match self.requests.iter_mut().find(|r| r.user == req.user) {
            Some(r) => *r = req,
            None => self.requests.push(req),
        }
        let mut plan = Plan {
            amc: Vec::new(),
            votes: Vec::new(),
        };
        for name in names {
            let Some(at) = self.bcms.iter().position(|b| b.name == name) else {
                continue;
            };
            let addr = self.bcms[at].addr;
            self.bcms[at]
                .floor
                .get_or_insert_with(|| bcm::not_lower(addr, 0, 0, slots));
            let v = self.votes(&self.bcms[at], &[]);
            let b = &self.bcms[at];
            plan.amc.push(bcm::Vote {
                addr: b.addr,
                vcd: b.aux.vcd,
                x: v[AMC].0,
                y: v[AMC].1,
            });
            plan.votes.push((at, v));
        }
        Ok(plan)
    }

    /// The AMC votes of `plan` were sent: record them, put WAKE and SLEEP in `cache` for each BCM whose pair changed, and say
    /// whether the cache's valid set changed, so a flush is due (`rpmh_flush` writes a resource only where SLEEP and WAKE differ).
    pub fn committed(&mut self, plan: &Plan, cache: &mut Cache) -> bool {
        self.commits += 1;
        let mut flush = false;
        for &(at, v) in &plan.votes {
            let b = &mut self.bcms[at];
            b.sent[AMC] = Some(v[AMC]);
            if b.sent[WAKE] == Some(v[WAKE]) && b.sent[SLEEP] == Some(v[SLEEP]) {
                continue;
            }
            let differed = b.sent[WAKE].is_some() && b.sent[WAKE] != b.sent[SLEEP];
            flush |= differed || v[WAKE] != v[SLEEP];
            cache.record(
                State::Wake,
                b.addr,
                bcm::command_data(true, v[WAKE].0, v[WAKE].1),
            );
            cache.record(
                State::Sleep,
                b.addr,
                bcm::command_data(true, v[SLEEP].0, v[SLEEP].1),
            );
            b.sent[WAKE] = Some(v[WAKE]);
            b.sent[SLEEP] = Some(v[SLEEP]);
        }
        flush
    }

    /// The BCMs of `path` as they would be voted with `extra` in place of the requests of the same users, for a report of votes not
    /// sent; `slots` gives what the TCSes hold.
    pub fn lines(
        &self,
        path: &[Node],
        extra: &[Request<U>],
        slots: &[(u32, u32, u32, u32)],
    ) -> Vec<Line> {
        let names = names_on(path);
        self.bcms
            .iter()
            .filter(|b| names.contains(&b.name))
            .map(|b| Line {
                name: b.name,
                addr: b.addr,
                linux: self.linux_votes(b, extra),
                floor: self.floor_vote(b),
                sent: b.sent,
                held: bcm::not_lower(b.addr, 0, 0, slots),
            })
            .collect()
    }

    /// The BCM at `addr`, if it is one of this table's.
    pub fn name_of(&self, addr: u32) -> Option<&'static str> {
        self.bcms.iter().find(|b| b.addr == addr).map(|b| b.name)
    }

    /// Every consumer's request, then per BCM: its consumers, what Linux's sum gives in each bucket, the floor client, and what was
    /// sent in each bucket.
    pub fn report(&self, out: &mut String) {
        out.push_str(&format!(
            "## Interconnect requests on this DRV ({} commits; floor client {})\n",
            self.commits,
            if self.synced {
                "dropped (synced)"
            } else {
                "holds every BCM (not synced)"
            }
        ));
        for r in &self.requests {
            out.push_str(&format!(
                "{:?}: {} -> {}, tag {}, avg {} peak {} kBps\n",
                r.user,
                r.path.first().map_or("?", |n| n.name),
                r.path.last().map_or("?", |n| n.name),
                r.tag,
                r.avg_kbps,
                r.peak_kbps
            ));
        }
        for b in &self.bcms {
            let v = self.linux_votes(b, &[]);
            let users: Vec<String> = self
                .requests
                .iter()
                .filter(|r| r.path.iter().any(|n| n.bcms.contains(&b.name)))
                .map(|r| format!("{:?}", r.user))
                .collect();
            out.push_str(&format!(
                "BCM {} {:#x}: consumers {:?} and the {}: {:?}; Linux amc {:?} wake {:?} sleep {:?}; sent amc {:?} wake {:?} sleep {:?}\n",
                b.name,
                b.addr,
                users,
                FLOOR,
                self.floor_vote(b),
                v[AMC],
                v[WAKE],
                v[SLEEP],
                b.sent[AMC],
                b.sent[WAKE],
                b.sent[SLEEP]
            ));
        }
        for (name, why) in &self.missing {
            out.push_str(&format!("BCM {}: {}\n", name, why));
        }
        if let Some(err) = &self.error {
            out.push_str(&format!("last error: {}\n", err));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum User {
        VideoMem,
        CpuCfg,
        Cpus,
    }

    const PATHS: [&[Node]; 3] = [&bcm::VIDEO_MEM, &bcm::CPU_CFG, &CPU_DDR];

    /// The laptop's command DB aux data of each BCM (`struct bcm_db`: unit, width, vcd), as `/proc/lucina-rpmh` printed it on
    /// 2026-11-11 (`evidence/juno-power/cpubw-static-ddr-2026-11-11.txt`), at the laptop's addresses.
    fn laptop() -> Icc<User> {
        let mut icc = Icc::new(&PATHS);
        let aux = |unit, width, vcd| Aux { unit, width, vcd };
        for name in icc.names() {
            let (addr, a) = match name {
                "MC0" => (0x50000, aux(0x1f47d0, 16, 0)),
                "SH0" => (0x5003c, aux(0x10c8e0, 16, 1)),
                "SH4" => (0x50028, aux(0x10c8e0, 16, 1)),
                "MM1" => (0x50088, aux(0x0cd140, 32, 4)),
                "MM2" => (0x5008c, aux(0x0cd140, 32, 4)),
                "SN12" => (0x5007c, aux(0x081650, 8, 3)),
                "CN0" => (0x500a8, aux(0x7530, 4, 5)),
                "ACV" => (0x5000c, aux(1, 0, 11)),
                other => panic!("no aux for {other}"),
            };
            icc.add(name, addr, a);
        }
        icc
    }

    fn req(user: User, tag: u32, avg_kbps: u64, peak_kbps: u64) -> Request<User> {
        let path: &'static [Node] = match user {
            User::VideoMem => &bcm::VIDEO_MEM,
            User::CpuCfg => &bcm::CPU_CFG,
            User::Cpus => &CPU_DDR,
        };
        Request {
            user,
            path,
            tag,
            avg_kbps,
            peak_kbps,
        }
    }

    /// One request, sent: the votes of each BCM of its path, by name.
    fn set(
        icc: &mut Icc<User>,
        cache: &mut Cache,
        r: Request<User>,
        slots: &[(u32, u32, u32, u32)],
    ) -> Vec<(&'static str, [(u32, u32); 3])> {
        let plan = icc.plan(r, slots).unwrap();
        let votes = plan.votes(icc).collect();
        icc.committed(&plan, cache);
        votes
    }

    fn of(votes: &[(&'static str, [(u32, u32); 3])], name: &str) -> [(u32, u32); 3] {
        votes.iter().find(|(n, _)| *n == name).unwrap().1
    }

    /// The CPUs' top DDR request of the laptop's device tree (peak 8 532 000 kBps, tag 3 = AMC and WAKE), alone, worked out by
    /// hand from `bcm_aggregate` (bcm-voter.c:91-125) with the laptop's units:
    /// SH4 and SH0 (buswidth 16, width 16, unit 1 100 000): y = 8 532 000 * 16 / 16 * 1000 / 1 100 000 = 7756;
    /// MC0 (`ebi`, buswidth 4, width 16, unit 2 050 000): 8 532 000 * 16 / 4 * 1000 / 2 050 000 = 16 647, cut to 0x3fff;
    /// ACV (`bcm_aggregate_mask`, :61-89): the enable mask 8 where a bucket has a request. SLEEP has none: tag 3 leaves it out.
    /// The values the laptop sent on 2026-11-11 with this request were the same y (7756, 16383, 8).
    #[test]
    fn a_cpu_ddr_request_is_voted_as_bcm_voter_does() {
        let mut icc = laptop();
        let mut cache = Cache::new();
        let plan = icc
            .plan(req(User::Cpus, TAG_AMC | TAG_WAKE, 0, 8_532_000), &[])
            .unwrap();
        let v: Vec<_> = plan.votes(&icc).collect();
        assert_eq!(of(&v, "SH4"), [(0, 7756), (0, 7756), (0, 0)]);
        assert_eq!(of(&v, "SH0"), [(0, 7756), (0, 7756), (0, 0)]);
        assert_eq!(of(&v, "MC0"), [(0, 16383), (0, 16383), (0, 0)]);
        assert_eq!(of(&v, "ACV"), [(0, 8), (0, 8), (0, 0)]);
        // The AMC votes go out as they are, and WAKE and SLEEP differ for all four: a flush is due and would write them.
        assert_eq!(
            plan.amc
                .iter()
                .map(|b| (b.addr, b.x, b.y))
                .collect::<Vec<_>>(),
            [
                (0x50028, 0, 7756),
                (0x5003c, 0, 7756),
                (0x5000c, 0, 8),
                (0x50000, 0, 16383),
            ]
        );
        assert!(icc.committed(&plan, &mut cache));
        assert_eq!(cache.valid().count(), 4);
        assert!(cache.valid().any(|e| e
            == (
                0x50000,
                bcm::command_data(true, 0, 0),
                bcm::command_data(true, 0, 16383)
            )));
    }

    /// `qcom_icc_aggregate` (icc-rpmh.c:84-111): on a node two consumers' avg add up and their peaks are maxed; a consumer that
    /// lets go takes only its own part away. MC0's `ebi` (buswidth 4, two channels, width 16, unit 2 050 000):
    /// x = sum * 16 / 8 * 1000 / 2 050 000.
    #[test]
    fn requests_are_summed_and_a_release_lowers_the_sum() {
        let mut icc = laptop();
        let mut cache = Cache::new();
        // Venus's video-mem (tag 0: every bucket) avg 1 000 000, the CPUs' DDR path (tag 3) avg 2 000 000 peak 4 000 000.
        set(
            &mut icc,
            &mut cache,
            req(User::VideoMem, 0, 1_000_000, 0),
            &[],
        );
        let v = set(
            &mut icc,
            &mut cache,
            req(User::Cpus, TAG_AMC | TAG_WAKE, 2_000_000, 4_000_000),
            &[],
        );
        // AMC and WAKE: 3 000 000 * 2 * 1000 / 2 050 000 = 2926; y = 4 000 000 * 4 * 1000 / 2 050 000 = 7804.
        // SLEEP: Venus alone, 1 000 000 * 2 * 1000 / 2 050 000 = 975.
        assert_eq!(of(&v, "MC0"), [(2926, 7804), (2926, 7804), (975, 0)]);
        // Venus lets go: the CPUs' 2 000 000 alone, 1951; SLEEP has nothing left, (0, 0).
        let v = set(&mut icc, &mut cache, req(User::VideoMem, 0, 0, 0), &[]);
        assert_eq!(of(&v, "MC0"), [(1951, 7804), (1951, 7804), (0, 0)]);
        // The CPUs too: nothing at all, and MC0's keepalive (bcm-voter.c:118-124) keeps AMC and WAKE at (1, 1), not SLEEP.
        let v = set(
            &mut icc,
            &mut cache,
            req(User::Cpus, TAG_AMC | TAG_WAKE, 0, 0),
            &[],
        );
        assert_eq!(of(&v, "MC0"), [(1, 1), (1, 1), (0, 0)]);
        // SH4 is on both the CPUs' DDR path and Venus's cpu-cfg (`acm_apps0`): the two add up there too.
        set(
            &mut icc,
            &mut cache,
            req(User::Cpus, TAG_AMC | TAG_WAKE, 1_100_000, 0),
            &[],
        );
        let v = set(
            &mut icc,
            &mut cache,
            req(User::CpuCfg, 0, 1_100_000, 0),
            &[],
        );
        // (1 100 000 + 1 100 000) * 16 / 16 * 1000 / 1 100 000 = 2000; SLEEP has cpu-cfg's 1000 only.
        assert_eq!(of(&v, "SH4"), [(2000, 0), (2000, 0), (1000, 0)]);
    }

    /// The floor client: what the boot firmware held for a BCM is one more consumer, maxed with the sum in every bucket, so no
    /// consumer's release can send a BCM below it. Venus's release of 2026-11-05 (patch 1041), which sent 0 to ACV, SH4, SN12,
    /// MM1 and MM2 and reset the laptop, sends the firmware's levels here.
    #[test]
    fn a_release_never_goes_below_the_floor_client() {
        let mut icc = laptop();
        let mut cache = Cache::new();
        let held = |addr, x, y| (0u32, 0u32, addr, bcm::command_data(true, x, y));
        let slots = [
            held(0x5000c, 0, 8),
            held(0x50028, 1, 0),
            held(0x5007c, 1, 0),
            held(0x50088, 2, 0),
            held(0x5008c, 2, 0),
        ];
        set(
            &mut icc,
            &mut cache,
            req(User::VideoMem, 0, 3_000_000, 3_000_000),
            &slots,
        );
        set(
            &mut icc,
            &mut cache,
            req(User::CpuCfg, 0, 1000, 1000),
            &slots,
        );
        let mut sent = set(&mut icc, &mut cache, req(User::VideoMem, 0, 0, 0), &slots);
        sent.extend(set(
            &mut icc,
            &mut cache,
            req(User::CpuCfg, 0, 0, 0),
            &slots,
        ));
        for (name, floor) in [
            ("ACV", (0, 8)),
            ("SH4", (1, 0)),
            ("SN12", (1, 0)),
            ("MM1", (2, 0)),
            ("MM2", (2, 0)),
        ] {
            assert_eq!(of(&sent, name), [floor; 3], "{name}");
        }
        // MC0's floor was (0, 0): its keepalive is what Linux sends, (1, 1) awake and nothing asleep.
        assert_eq!(of(&sent, "MC0"), [(1, 1), (1, 1), (0, 0)]);
        // The floor is taken once, at the BCM's first vote: TCSes rewritten since (by our own votes) do not raise it.
        let later = [held(0x50088, 100, 100)];
        let v = set(&mut icc, &mut cache, req(User::VideoMem, 0, 0, 0), &later);
        assert_eq!(of(&v, "MM1"), [(2, 0); 3]);
        assert!(!icc.synced());
    }

    /// `icc_sync_state` (core.c:1093-1120): the floor client lets go, and each BCM is what its consumers ask. Nothing in the
    /// kernel calls it in this patch.
    #[test]
    fn sync_drops_the_floor_client() {
        let mut icc = laptop();
        let mut cache = Cache::new();
        let slots = [(0, 0, 0x50088, bcm::command_data(true, 2, 0))];
        assert_eq!(
            of(
                &set(&mut icc, &mut cache, req(User::VideoMem, 0, 0, 0), &slots),
                "MM1"
            ),
            [(2, 0); 3]
        );
        icc.sync();
        assert_eq!(
            of(
                &set(&mut icc, &mut cache, req(User::VideoMem, 0, 0, 0), &slots),
                "MM1"
            ),
            [(0, 0); 3]
        );
    }

    #[test]
    fn a_path_through_a_missing_bcm_is_refused_and_lines_report_the_rest() {
        let mut icc = laptop();
        icc.add_missing("CN0", String::from("BCM not in cmd-db"));
        assert!(icc.plan(req(User::CpuCfg, 0, 1, 1), &[]).is_err());
        assert!(icc.requests.is_empty(), "a refused request is not kept");
        let slots = [(0, 0, 0x50000, bcm::command_data(true, 3, 4))];
        let lines = icc.lines(
            &CPU_DDR,
            &[req(User::Cpus, TAG_AMC | TAG_WAKE, 0, 8_532_000)],
            &slots,
        );
        let mc0 = lines.iter().find(|l| l.name == "MC0").unwrap();
        assert_eq!(
            (mc0.linux[AMC], mc0.floor, mc0.held),
            ((0, 16383), None, (3, 4))
        );
        let mut out = String::new();
        icc.report(&mut out);
        assert!(
            out.contains("floor client holds every BCM (not synced)"),
            "{out}"
        );
        assert!(out.contains("BCM CN0: BCM not in cmd-db"), "{out}");
    }
}
