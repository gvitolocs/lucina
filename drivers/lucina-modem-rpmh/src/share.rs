//! One table of every ARC power domain and XO vote this kernel makes on the application processor's DRV, aggregated across the
//! drivers that make them, as Linux's genpd, `rpmhpd.c` and the clock framework do across consumers.
//!
//! Derived from Linux 7.1 `drivers/pmdomain/qcom/rpmhpd.c`, `drivers/pmdomain/core.c`, `drivers/clk/qcom/clk-rpmh.c`,
//! `drivers/clk/clk.c` and `drivers/remoteproc/qcom_q6v5_pas.c` (GPL-2.0). The rules are in [`crate::vote_rules`], which
//! Verus checks; this file keeps the table and turns each change into the commands Linux would send.
//!
//! # Why one table
//!
//! RPMh adds up votes per DRV, not per driver: everything this kernel sends on the apps DRV is one voter, and the last command to
//! an address wins. Each driver used to send its own votes (the modem's proxies, the audio DSP's, Venus's), so a lower vote from
//! one would have undone another's, and nothing could ever be lowered: `cx` and `mx` stayed at their top level for good.
//! Linux keeps one `struct rpmhpd` per domain with a performance state per consumer and sends the highest. This is that.
//!
//! # What is sent when
//!
//! * A client's vote ([`Votes::vote`], [`Votes::proxy`]) changes its entry; the domain and its parent are worked out again and
//!   sent when the corner changed, the parent first when it goes up and last when it goes down (genpd's order).
//! * Until [`Votes::sync`] every domain that is sent at all is sent its top corner, as `rpmhpd_aggregate_corner` clamps
//!   before `sync_state`: until then nothing goes below what the boot firmware may have left.
//! * [`Votes::sync`] is `rpmhpd_sync_state`: every domain is sent what its clients hold, 0 for one nobody holds.
//! * `cx` and `mx` have an active-only peer (`cx_ao`, `mx_ao`), so each send is ACTIVE, WAKE and SLEEP; nobody here votes the
//!   peers, so the three carry the same corner. `gfx`, `lmx`, `lcx` and `mss` have no peer and get ACTIVE only.
//! * XO ([`Votes::xo`]) is a reference count: on for the first holder, off after the last, each as SLEEP, WAKE, ACTIVE
//!   (`clk_rpmh_send_aggregate_command`). On this SoC GCC holds it for good: GPLL0's parent is `bi_tcxo`, and every running QUP,
//!   UFS and USB clock comes from GPLL0. So a remote processor's handover lets go of its own reference and XO stays on.
//!
//! The device tree's consumers that have no driver voting here (GCC, the serial engines, UFS, USB, the display, SD) are one client,
//! [`Client::Tree`], holding the highest level any of them can ask for ([`tree_floor`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_qcom_soc::cmd_db::{CmdDb, HW_ARC};
use lucina_qcom_soc::io::RegIo;

use crate::arc::{self, sc7180, State, INT_MAX, MAX_LEVELS, SC7180};
use crate::rpmh::Cache;
use crate::rsc::TcsCmd;
use crate::vote_rules::{self, Table, CLIENTS};
use crate::xo;

/// Who holds a vote.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Client {
    /// The modem's proxy votes (`cx`, `mss`, XO) between its start and its handover.
    Modem = 0,
    /// The audio DSP's proxy votes (`lcx`, `lmx`, XO) between its start and its handover.
    Adsp = 1,
    /// The video block's `cx`, at the level of its clock.
    Venus = 2,
    /// The device tree's consumers that no driver here votes for ([`tree_floor`]).
    Tree = 3,
    /// GCC, which holds XO as the parent of its PLLs.
    Gcc = 4,
}

/// Names for the report, by client number.
pub const CLIENT_NAMES: [&str; CLIENTS] = ["modem", "adsp", "venus", "tree", "gcc", "-", "-", "-"];

/// The domains that take votes: the full ones of `sc7180_rpmhpds` (the `_ao` peers are nobody's here).
pub const DOMAINS: [usize; 6] = [sc7180::CX, sc7180::MX, sc7180::GFX, sc7180::LCX, sc7180::LMX, sc7180::MSS];

/// The domain whose parent `d` is: `cx` is `cx_w_mx_parent`.
fn child_of(d: usize) -> Option<usize> {
    DOMAINS.iter().copied().find(|&c| SC7180[c].parent == Some(d))
}

/// One command, ready for the RSC (ACTIVE) or the sleep and wake cache (WAKE, SLEEP).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub res: &'static str,
    pub state: State,
    pub cmd: TcsCmd,
}

#[derive(Copy, Clone, Debug)]
struct Dom {
    addr: u32,
    table: Option<Table>,
    on: [bool; CLIENTS],
    want: [u32; CLIENTS],
    /// The corner last sent as ACTIVE, `None` before the first.
    sent: Option<usize>,
}

const NO_DOM: Dom = Dom {
    addr: 0,
    table: None,
    on: [false; CLIENTS],
    want: [0; CLIENTS],
    sent: None,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareError {
    /// A domain's command DB entry is missing, not ARC, or its level table unusable.
    Domain(&'static str),
    Xo,
}

impl core::fmt::Display for ShareError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ShareError::Domain(res) => write!(f, "{res}: no usable ARC level table in the command DB"),
            ShareError::Xo => f.write_str("command DB has no address for xo.lvl"),
        }
    }
}

/// The table.
pub struct Votes {
    dom: [Dom; sc7180::COUNT],
    synced: bool,
    xo_addr: u32,
    xo_hold: [bool; CLIENTS],
    xo_sent: Option<bool>,
    /// Every WAKE and SLEEP value sent, for `rpmh_flush`.
    pub cache: Cache,
}

impl Votes {
    /// `rpmhpd_probe` for [`DOMAINS`] and `clk-rpmh`'s lookup of XO, from the command DB. Nothing is sent.
    pub fn load<R: RegIo>(db: &CmdDb<R>) -> Result<Self, ShareError> {
        let mut v = Votes {
            dom: [NO_DOM; sc7180::COUNT],
            synced: false,
            xo_addr: xo::active_vote(db).map_err(|_| ShareError::Xo)?.addr,
            xo_hold: [false; CLIENTS],
            xo_sent: None,
            cache: Cache::new(),
        };
        for d in DOMAINS {
            let res = SC7180[d].res_name;
            let entry = db.find(res).filter(|e| e.addr != 0 && e.slave_id() == HW_ARC).ok_or(ShareError::Domain(res))?;
            let mut buf = [0u8; MAX_LEVELS * 2];
            let n = db.aux(entry, &mut buf);
            let levels =
                arc::parse_levels(&buf[..n], entry.data_len as usize, SC7180[d].skip_retention_level).map_err(|_| ShareError::Domain(res))?;
            v.install(d, entry.addr, levels)?;
        }
        Ok(v)
    }

    /// A table without a command DB, for tests.
    pub fn new(xo_addr: u32) -> Self {
        Votes {
            dom: [NO_DOM; sc7180::COUNT],
            synced: false,
            xo_addr,
            xo_hold: [false; CLIENTS],
            xo_sent: None,
            cache: Cache::new(),
        }
    }

    /// Put a domain's address and level table in place.
    pub fn install(&mut self, d: usize, addr: u32, levels: arc::Levels) -> Result<(), ShareError> {
        let table = Table {
            level: levels.level,
            count: levels.count,
            enable_corner: levels.enable_corner,
        };
        if !vote_rules::table_good(&table) {
            return Err(ShareError::Domain(SC7180[d].res_name));
        }
        self.dom[d].addr = addr;
        self.dom[d].table = Some(table);
        Ok(())
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    /// The level table of domain `d`.
    pub fn table(&self, d: usize) -> Option<&Table> {
        self.dom.get(d).and_then(|x| x.table.as_ref())
    }

    /// The corner last sent to domain `d`.
    pub fn sent(&self, d: usize) -> Option<usize> {
        self.dom.get(d).and_then(|x| x.sent)
    }

    /// Whether XO was last sent on.
    pub fn xo_sent(&self) -> Option<bool> {
        self.xo_sent
    }

    /// `(on, state)` of domain `d` as genpd sees it: its clients and its child.
    fn state(&self, d: usize) -> (bool, u32) {
        let (child_on, child_state) = match child_of(d) {
            Some(c) => self.state(c),
            None => (false, 0),
        };
        vote_rules::domain_state(&self.dom[d].on, &self.dom[d].want, child_on, child_state)
    }

    /// The corner domain `d` should be sent now.
    fn corner(&self, d: usize) -> Option<usize> {
        let t = self.dom[d].table.as_ref()?;
        let (child_on, child_state) = match child_of(d) {
            Some(c) => self.state(c),
            None => (false, 0),
        };
        Some(vote_rules::domain_corner(t, self.synced, &self.dom[d].on, &self.dom[d].want, child_on, child_state))
    }

    /// The sends of `rpmhpd_aggregate_corner` for domain `d` at `corner`, if it differs from what was sent.
    fn send(&mut self, d: usize, force: bool, out: &mut Vec<Cmd>) {
        let Some(corner) = self.corner(d) else {
            return;
        };
        if !force && self.dom[d].sent == Some(corner) {
            return;
        }
        let res = SC7180[d].res_name;
        let addr = self.dom[d].addr;
        let last = self.dom[d].sent.unwrap_or(0);
        let data = corner as u32;
        out.push(Cmd {
            res,
            state: State::Active,
            cmd: TcsCmd { addr, data, wait: vote_rules::wait_for_answer(corner, last) },
        });
        self.cache.record(State::Active, addr, data);
        if SC7180[d].peer.is_some() {
            for state in [State::Wake, State::Sleep] {
                out.push(Cmd { res, state, cmd: TcsCmd { addr, data, wait: false } });
                self.cache.record(state, addr, data);
            }
        }
        self.dom[d].sent = Some(corner);
    }

    /// `client` asks for performance state `state` (a voltage level, `opp-level`) on domain `d` and holds it on (`on`), or lets
    /// go. Returns the commands, in order. A domain that is not loaded takes no votes.
    pub fn vote(&mut self, client: Client, d: usize, on: bool, state: u32) -> Vec<Cmd> {
        let mut out = Vec::new();
        if self.dom.get(d).is_none_or(|x| x.table.is_none()) || !DOMAINS.contains(&d) {
            return out;
        }
        let c = client as usize;
        self.dom[d].on[c] = on;
        self.dom[d].want[c] = if on { state } else { 0 };
        let parent = SC7180[d].parent.filter(|p| self.dom[*p].table.is_some());
        match parent {
            Some(p) => {
                let up = self.corner(p).is_some_and(|n| self.dom[p].sent.is_none_or(|s| n > s));
                if up {
                    self.send(p, false, &mut out);
                    self.send(d, false, &mut out);
                } else {
                    self.send(d, false, &mut out);
                    self.send(p, false, &mut out);
                }
            }
            None => self.send(d, false, &mut out),
        }
        out
    }

    /// `qcom_pas_pds_enable` (`on`: each domain at `INT_MAX`, which maps to its top corner) or `qcom_pas_pds_disable` (each at 0
    /// and off) for a remote processor's proxy domains, in order.
    pub fn proxy(&mut self, client: Client, domains: &[usize], on: bool) -> Vec<Cmd> {
        let mut out = Vec::new();
        for &d in domains {
            out.extend(self.vote(client, d, on, if on { INT_MAX } else { 0 }));
        }
        out
    }

    /// `client` takes (`on`) or lets go of XO. Commands only when the resource changes: the first holder's `clk_rpmh_prepare`,
    /// the last one's `clk_rpmh_unprepare`; SLEEP and WAKE first, ACTIVE last, which waits when it turns XO on.
    pub fn xo(&mut self, client: Client, on: bool) -> Vec<Cmd> {
        self.xo_hold[client as usize] = on;
        let now = vote_rules::xo_on(&self.xo_hold);
        let mut out = Vec::new();
        if self.xo_sent == Some(now) {
            return out;
        }
        let data = if now { xo::ON_VALUE } else { 0 };
        let addr = self.xo_addr + xo::ARC_EN_OFFSET;
        for state in [State::Sleep, State::Wake, State::Active] {
            out.push(Cmd {
                res: xo::RES_NAME,
                state,
                cmd: TcsCmd { addr, data, wait: now && state == State::Active },
            });
            self.cache.record(state, addr, data);
        }
        self.xo_sent = Some(now);
        out
    }

    /// `rpmhpd_sync_state`: from now on each domain gets what its clients hold, and every loaded domain is sent once now, whether
    /// or not it changed, as Linux sends them all.
    pub fn sync(&mut self) -> Vec<Cmd> {
        self.sync_except(&[])
    }

    /// [`Votes::sync`], leaving the domains in `keep` unsent: what the boot firmware voted on them stands until a client votes.
    pub fn sync_except(&mut self, keep: &[usize]) -> Vec<Cmd> {
        self.synced = true;
        let mut out = Vec::new();
        // Parents last: they only go down here or stay.
        for d in [sc7180::CX, sc7180::GFX, sc7180::LCX, sc7180::LMX, sc7180::MSS, sc7180::MX] {
            if !keep.contains(&d) {
                self.send(d, true, &mut out);
            }
        }
        out
    }

    /// Who holds what, and what was sent.
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(if self.synced { "synced: yes\n" } else { "synced: no (every domain sent is at its top)\n" });
        for d in DOMAINS {
            let x = &self.dom[d];
            let Some(t) = x.table.as_ref() else {
                s.push_str(&format!("{}: not loaded\n", SC7180[d].res_name));
                continue;
            };
            let levels: Vec<u32> = t.level[..t.count].to_vec();
            let sent = match x.sent {
                Some(c) => format!("corner {} (level {})", c, t.level[c]),
                None => String::from("nothing yet (the firmware's vote stands)"),
            };
            s.push_str(&format!("{} at {:#x}: levels {:?}; sent {}\n", SC7180[d].res_name, x.addr, levels, sent));
            for c in 0..CLIENTS {
                if x.on[c] {
                    s.push_str(&format!("  {} holds level {}\n", CLIENT_NAMES[c], x.want[c]));
                }
            }
        }
        let holders: Vec<&str> = (0..CLIENTS).filter(|&c| self.xo_hold[c]).map(|c| CLIENT_NAMES[c]).collect();
        s.push_str(&format!(
            "xo.lvl at {:#x}: {}; held by {:?}\n",
            self.xo_addr,
            match self.xo_sent {
                Some(true) => "sent on",
                Some(false) => "sent off",
                None => "nothing yet",
            },
            holders
        ));
        s
    }
}

/// One device tree node that holds a domain without a driver here voting for it: the node, the domain, and the highest
/// performance state (`opp-level`) its `required-opps` and OPP table can ask for (0 when it has none, which holds the domain at
/// its first non-zero corner, as `genpd_power_on` with no performance state does).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consumer {
    pub node: String,
    pub domain: usize,
    pub state: u32,
}

/// The [`Client::Tree`] vote of each domain: `Some(highest state)` where any consumer holds it. Linux would vote each consumer's
/// current OPP; the highest of its table is never below that, which is the side to err on with no driver to follow the clock.
pub fn tree_floor(consumers: &[Consumer]) -> [Option<u32>; sc7180::COUNT] {
    let mut floor = [None; sc7180::COUNT];
    for c in consumers {
        if let Some(f) = floor.get_mut(c.domain) {
            *f = Some(f.map_or(c.state, |x: u32| x.max(c.state)));
        }
    }
    floor
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The laptop's own tables for cx, mx, gfx and mss, read on 2026-11-05 from its command DB (`wifi: cmd-db`, `gpu: cmd-db`).
    /// For lcx and lmx only the top corner (5) is known from the log (`audio: rpmh`); their levels here are made up.
    fn laptop() -> Votes {
        let mut v = Votes::new(0x30090);
        let t = |l: &[u32]| {
            let mut level = [0u32; MAX_LEVELS];
            level[..l.len()].copy_from_slice(l);
            arc::Levels { level, count: l.len(), enable_corner: 1 }
        };
        v.install(sc7180::CX, 0x30000, t(&[0, 16, 64, 128, 192, 256, 384])).unwrap();
        v.install(sc7180::MX, 0x30010, t(&[0, 16, 256, 384])).unwrap();
        v.install(sc7180::GFX, 0x30050, t(&[0, 16, 64, 128, 192, 256, 320, 384, 416])).unwrap();
        v.install(sc7180::LCX, 0x30030, t(&[0, 16, 64, 128, 192, 256])).unwrap();
        v.install(sc7180::LMX, 0x30040, t(&[0, 16, 64, 128, 192, 256])).unwrap();
        v.install(sc7180::MSS, 0x30060, t(&[0, 16, 64, 128, 192, 256, 320, 384])).unwrap();
        v
    }

    fn active(cmds: &[Cmd]) -> Vec<(&'static str, u32)> {
        cmds.iter().filter(|c| c.state == State::Active).map(|c| (c.res, c.cmd.data)).collect()
    }

    #[test]
    fn the_modem_start_sends_what_it_sent_before() {
        // Today's log: mx.lvl 0x3, cx.lvl 0x6, mss.lvl 0x7, xo.lvl 0x3.
        let mut v = laptop();
        let mut cmds = v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        cmds.extend(v.xo(Client::Modem, true));
        assert_eq!(active(&cmds), [("mx.lvl", 3), ("cx.lvl", 6), ("mss.lvl", 7), ("xo.lvl", 3)]);
        // cx and mx carry WAKE and SLEEP too, mss does not.
        assert_eq!(cmds.iter().filter(|c| c.res == "cx.lvl").count(), 3);
        assert_eq!(cmds.iter().filter(|c| c.res == "mss.lvl").count(), 1);
        assert!(cmds.iter().filter(|c| c.state == State::Active).all(|c| c.cmd.wait));
    }

    #[test]
    fn before_sync_a_release_keeps_the_top() {
        let mut v = laptop();
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        // Unchanged corners send nothing.
        assert!(v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], false).is_empty());
        assert_eq!((v.sent(sc7180::CX), v.sent(sc7180::MSS)), (Some(6), Some(7)));
    }

    #[test]
    fn sync_sends_the_holders_and_zero_for_the_rest() {
        let mut v = laptop();
        v.xo(Client::Gcc, true);
        v.vote(Client::Tree, sc7180::CX, true, 256);
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        v.xo(Client::Modem, true);
        // Handover.
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], false);
        assert!(v.xo(Client::Modem, false).is_empty(), "GCC still holds XO");
        let cmds = v.sync();
        assert_eq!(
            active(&cmds),
            [("cx.lvl", 5), ("gfx.lvl", 0), ("lcx.lvl", 0), ("lmx.lvl", 0), ("mss.lvl", 0), ("mx.lvl", 2)]
        );
        assert_eq!(v.xo_sent(), Some(true));
    }

    #[test]
    fn a_proxy_after_sync_raises_parent_first_and_its_release_lowers_parent_last() {
        let mut v = laptop();
        v.vote(Client::Tree, sc7180::CX, true, 256);
        v.sync();
        let up = v.proxy(Client::Modem, &[sc7180::CX], true);
        assert_eq!(active(&up), [("mx.lvl", 3), ("cx.lvl", 6)]);
        let down = v.proxy(Client::Modem, &[sc7180::CX], false);
        assert_eq!(active(&down), [("cx.lvl", 5), ("mx.lvl", 2)]);
        assert!(down.iter().all(|c| !c.cmd.wait), "going down never waits");
    }

    #[test]
    fn venus_and_the_modem_hold_cx_together() {
        let mut v = laptop();
        v.vote(Client::Tree, sc7180::CX, true, 256);
        v.sync();
        v.proxy(Client::Modem, &[sc7180::CX], true);
        v.vote(Client::Venus, sc7180::CX, true, 128);
        // Venus lowering its own vote does not take the modem's away.
        assert!(active(&v.vote(Client::Venus, sc7180::CX, true, 64)).is_empty());
        assert_eq!(v.sent(sc7180::CX), Some(6));
        v.proxy(Client::Modem, &[sc7180::CX], false);
        assert_eq!(v.sent(sc7180::CX), Some(5));
        v.vote(Client::Tree, sc7180::CX, false, 0);
        assert_eq!(v.sent(sc7180::CX), Some(2), "Venus's 64 alone");
        v.vote(Client::Venus, sc7180::CX, false, 0);
        assert_eq!((v.sent(sc7180::CX), v.sent(sc7180::MX)), (Some(0), Some(0)));
    }

    #[test]
    fn partial_sync_keeps_cx_mx_at_the_top_and_gfx_unsent() {
        let mut v = laptop();
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], false);
        v.vote(Client::Tree, sc7180::CX, true, INT_MAX);
        let cmds = v.sync_except(&[sc7180::GFX]);
        assert_eq!(active(&cmds), [("cx.lvl", 6), ("lcx.lvl", 0), ("lmx.lvl", 0), ("mss.lvl", 0), ("mx.lvl", 3)]);
        assert_eq!(v.sent(sc7180::GFX), None);
    }

    #[test]
    fn a_holder_at_level_zero_keeps_the_first_non_zero_corner() {
        let mut v = laptop();
        v.vote(Client::Tree, sc7180::LCX, true, 0);
        v.sync();
        assert_eq!(v.sent(sc7180::LCX), Some(1));
    }

    #[test]
    fn xo_is_counted() {
        let mut v = Votes::new(0x30090);
        assert_eq!(active(&v.xo(Client::Gcc, true)), [("xo.lvl", 3)]);
        assert!(v.xo(Client::Modem, true).is_empty());
        assert!(v.xo(Client::Gcc, false).is_empty());
        let off = v.xo(Client::Modem, false);
        assert_eq!(
            off.iter().map(|c| (c.state, c.cmd.data, c.cmd.wait)).collect::<Vec<_>>(),
            [(State::Sleep, 0, false), (State::Wake, 0, false), (State::Active, 0, false)]
        );
    }

    #[test]
    fn a_falling_table_is_refused() {
        let mut v = Votes::new(0);
        let mut level = [0u32; MAX_LEVELS];
        level[..3].copy_from_slice(&[0, 256, 128]);
        assert!(v.install(sc7180::CX, 0x30000, arc::Levels { level, count: 3, enable_corner: 1 }).is_err());
        assert!(v.vote(Client::Tree, sc7180::CX, true, 64).is_empty());
    }

    /// 20 000 random votes, proxies, XO changes and one sync against a model written apart from the table: what Linux would send
    /// for every domain, from the holders alone. Covers what Verus does not: the table's own bookkeeping (the parent worked out
    /// again, nothing sent when nothing changed, the last corner sent).
    #[test]
    fn random_votes_match_an_independent_model() {
        const STATES: [u32; 9] = [0, 16, 64, 128, 192, 256, 384, 416, INT_MAX];
        const VOTERS: [Client; 4] = [Client::Modem, Client::Adsp, Client::Venus, Client::Tree];
        const ALL: [Client; 5] = [Client::Modem, Client::Adsp, Client::Venus, Client::Tree, Client::Gcc];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x % n as u64) as usize
        };
        let mut v = laptop();
        let mut holds = [[None::<u32>; CLIENTS]; sc7180::COUNT];
        let mut touched = [false; sc7180::COUNT];
        let mut xo = [false; CLIENTS];
        let mut xo_called = false;
        let mut synced = false;
        for step in 0..20_000 {
            let before: Vec<Option<usize>> = (0..sc7180::COUNT).map(|d| v.sent(d)).collect();
            let roll = next(100);
            let (what, cmds) = if roll < 70 {
                let (c, d, on, s) = (VOTERS[next(4)], DOMAINS[next(6)], next(2) == 0, STATES[next(9)]);
                holds[d][c as usize] = on.then_some(s);
                touched[d] = true;
                (format!("vote {c:?} {d} {on} {s}"), v.vote(c, d, on, s))
            } else if roll < 85 {
                let (c, d1, d2, on) = (VOTERS[next(4)], DOMAINS[next(6)], DOMAINS[next(6)], next(2) == 0);
                for d in [d1, d2] {
                    holds[d][c as usize] = on.then_some(INT_MAX);
                    touched[d] = true;
                }
                (format!("proxy {c:?} {d1} {d2} {on}"), v.proxy(c, &[d1, d2], on))
            } else if roll < 95 || synced {
                let (c, on) = (ALL[next(5)], next(2) == 0);
                xo[c as usize] = on;
                xo_called = true;
                (format!("xo {c:?} {on}"), v.xo(c, on))
            } else {
                synced = true;
                (String::from("sync"), v.sync())
            };
            touched[sc7180::MX] |= touched[sc7180::CX];
            // Waiting only on the way up.
            for cmd in cmds.iter().filter(|c| c.state == State::Active && c.res != xo::RES_NAME) {
                let d = DOMAINS.into_iter().find(|&d| SC7180[d].res_name == cmd.res).unwrap();
                let last = before[d].unwrap_or(0) as u32;
                assert_eq!(cmd.cmd.wait, cmd.cmd.data > last, "step {step}, {what}: wait on {} {}", cmd.res, cmd.cmd.data);
            }
            for d in DOMAINS {
                let t = *v.table(d).unwrap();
                // The holders of d and, for mx, of its child cx.
                let mut held: Vec<u32> = holds[d].iter().flatten().copied().collect();
                if d == sc7180::MX {
                    held.extend(holds[sc7180::CX].iter().flatten().copied());
                }
                let top = t.count - 1;
                let model = if !synced {
                    top
                } else if held.is_empty() {
                    0
                } else {
                    let s = *held.iter().max().unwrap();
                    let c = (0..t.count).find(|&i| t.level[i] >= s).unwrap_or(top);
                    c.max(t.enable_corner)
                };
                let sent = v.sent(d);
                if synced {
                    assert_eq!(sent, Some(model), "step {step}, {what}: domain {d}");
                    for s in &held {
                        assert!(t.level[model] >= *s || model == top, "step {step}, {what}: domain {d} below {s}");
                    }
                } else if touched[d] {
                    assert_eq!(sent, Some(top), "step {step}, {what}: domain {d} before the sync");
                } else {
                    assert_eq!(sent, None, "step {step}, {what}: domain {d} sent untouched");
                }
            }
            let any = xo.iter().any(|&h| h);
            assert_eq!(v.xo_sent(), xo_called.then_some(any), "step {step}, {what}: xo");
        }
        assert!(synced, "the run never synced");
    }

    #[test]
    fn the_tree_floor_is_the_highest() {
        let c = |node: &str, domain, state| Consumer { node: node.into(), domain, state };
        let f = tree_floor(&[c("gcc", sc7180::CX, 0), c("usb", sc7180::CX, 256), c("qup", sc7180::CX, 64), c("x", 99, 1)]);
        assert_eq!(f[sc7180::CX], Some(256));
        assert_eq!(f[sc7180::MX], None);
    }
}
