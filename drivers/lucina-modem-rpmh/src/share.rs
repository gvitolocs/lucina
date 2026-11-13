//! One table of every ARC power domain and XO vote this kernel makes on the application processor's DRV, aggregated across the
//! drivers that make them, as Linux's genpd, `rpmhpd.c` and the clock framework do across consumers.
//!
//! Derived from Linux 7.1 `drivers/pmdomain/qcom/rpmhpd.c`, `drivers/pmdomain/core.c`, `drivers/clk/qcom/clk-rpmh.c`,
//! `drivers/clk/clk.c` and `drivers/remoteproc/qcom_q6v5_pas.c` (GPL-2.0). The rules are in [`crate::vote_rules`], which
//! Verus checks; this file keeps the table and turns each change into the commands Linux would send. Line numbers below are
//! Linux v6.12's.
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
//!   before `sync_state` (`rpmhpd.c:703-707`): until then nothing goes below what the boot firmware may have left.
//! * [`Votes::sync`] is `rpmhpd_sync_state`: every domain is sent what its clients hold, 0 for one nobody holds.
//! * `cx` and `mx` have an active-only peer (`cx_ao`, `mx_ao`, [`PEERS`]) on the same resource, so each send is ACTIVE, WAKE and
//!   SLEEP (`rpmhpd_aggregate_corner`, `rpmhpd.c:692-739`): ACTIVE and WAKE the larger of the two domains' corners, SLEEP the
//!   larger of their sleep corners, which `to_active_sleep` (`:672-681`, [`arc::to_active_sleep`]) makes 0 for the active-only
//!   one. A vote on the full domain therefore holds the resource asleep too, and a vote on a peer only while the application
//!   processor is awake. `gfx`, `lmx`, `lcx` and `mss` have no peer and get ACTIVE only. Every value goes into the `rpmh.c` cache
//!   ([`Votes::cache`]), whose flush writes the resources whose SLEEP and WAKE values differ.
//! * XO is the two clocks of `clk-rpmh.c` on `xo.lvl`, each counted as the clock framework counts prepares (`clk.c:1112-1141`,
//!   `:1052-1071`): `bi_tcxo` ([`Votes::xo`]), valid in SLEEP, WAKE and ACTIVE, and `bi_tcxo_ao` ([`Votes::xo_ao`]), valid in WAKE
//!   and ACTIVE only (`clk-rpmh.c:81-83`, `:100-101`). The resource's state is the union of the two (`:195-197`), and each state
//!   whose bit changed is sent, SLEEP then WAKE then ACTIVE, the ACTIVE one waiting when anything is on (`:152-185`). On this SoC
//!   GCC holds `bi_tcxo` for good: GPLL0's parent is `bi_tcxo`, and every running QUP, UFS and USB clock comes from GPLL0. So a
//!   remote processor's handover lets go of its own reference and XO stays on.
//!
//! The device tree's consumers that have no driver voting here (GCC, the serial engines, UFS, USB, the display, SD) are one client,
//! [`Client::Tree`], whose level is the highest of the consumers that hold a domain ([`Votes::vote_consumer`]), as genpd takes the
//! highest of its devices' performance states (`_genpd_reeval_performance_state`, `drivers/pmdomain/core.c:313`). Each consumer
//! is kept on its own, so it can be lowered or let go without touching the others.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_qcom_soc::cmd_db::{CmdDb, HW_ARC};
use lucina_qcom_soc::io::RegIo;

use crate::arc::{self, INT_MAX, MAX_LEVELS, SC7180, State, sc7180};
use crate::rpmh::Cache;
use crate::rsc::TcsCmd;
use crate::vote_rules::{self, CLIENTS, Table};
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
    /// The device tree's consumers that no driver here votes for ([`Votes::vote_consumer`]).
    Tree = 3,
    /// GCC, which holds XO as the parent of its PLLs.
    Gcc = 4,
}

/// Names for the report, by client number.
pub const CLIENT_NAMES: [&str; CLIENTS] = ["modem", "adsp", "venus", "tree", "gcc", "-", "-", "-"];

/// The domains that take votes: the full ones of `sc7180_rpmhpds`.
pub const DOMAINS: [usize; 6] = [
    sc7180::CX,
    sc7180::MX,
    sc7180::GFX,
    sc7180::LCX,
    sc7180::LMX,
    sc7180::MSS,
];

/// The active-only peers of `sc7180_rpmhpds` (`cx_ao_w_mx_parent`, `mx_ao`), which take votes too: one holds the resource of its
/// full domain while the application processor is awake and asks nothing of its sleep. No driver here votes them yet.
pub const PEERS: [usize; 2] = [sc7180::CX_AO, sc7180::MX_AO];

fn takes_votes(d: usize) -> bool {
    DOMAINS.contains(&d) || PEERS.contains(&d)
}

/// The domain whose resource `d` votes on: the full domain for an active-only peer, which shares its address (`rpmhpd_probe`
/// gives both the same `res_name`).
fn owner(d: usize) -> usize {
    if SC7180[d].active_only {
        SC7180[d].peer.unwrap_or(d)
    } else {
        d
    }
}

/// The domain whose parent `d` is: `cx` is `cx_w_mx_parent`, `cx_ao` is `cx_ao_w_mx_ao_parent`.
fn child_of(d: usize) -> Option<usize> {
    DOMAINS
        .iter()
        .chain(PEERS.iter())
        .copied()
        .find(|&c| SC7180[c].parent == Some(d))
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
    /// The corner last sent as ACTIVE (and WAKE), `None` before the first. Kept on the full domain for its peer too.
    sent: Option<usize>,
    /// The corner last sent as SLEEP, for a domain with a peer.
    sent_sleep: Option<usize>,
}

const NO_DOM: Dom = Dom {
    addr: 0,
    table: None,
    on: [false; CLIENTS],
    want: [0; CLIENTS],
    sent: None,
    sent_sleep: None,
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
            ShareError::Domain(res) => {
                write!(f, "{res}: no usable ARC level table in the command DB")
            }
            ShareError::Xo => f.write_str("command DB has no address for xo.lvl"),
        }
    }
}

/// `BIT(state)` for each state `bi_tcxo` is valid in (`clk-rpmh.c:81-83`).
const XO_STATES: u8 = 1 << State::Sleep as u8 | 1 << State::Wake as u8 | 1 << State::Active as u8;
/// The same for `bi_tcxo_ao` (`clk-rpmh.c:100-101`).
const XO_AO_STATES: u8 = 1 << State::Wake as u8 | 1 << State::Active as u8;

/// The table.
pub struct Votes {
    dom: [Dom; sc7180::COUNT],
    synced: bool,
    xo_addr: u32,
    /// Who holds `bi_tcxo` and `bi_tcxo_ao`.
    xo_hold: [bool; CLIENTS],
    xo_ao_hold: [bool; CLIENTS],
    /// `last_sent_aggr_state` (0 at start, as `kzalloc`'s), and whether any XO change was asked for yet.
    xo_last: u8,
    xo_called: bool,
    /// The device tree's consumers that hold a domain, each as [`Votes::vote_consumer`] last gave it.
    tree: Vec<Consumer>,
    /// Every WAKE and SLEEP value sent, for `rpmh_flush`.
    pub cache: Cache,
}

impl Votes {
    /// `rpmhpd_probe` for [`DOMAINS`] (and their peers) and `clk-rpmh`'s lookup of XO, from the command DB. Nothing is sent.
    pub fn load<R: RegIo>(db: &CmdDb<R>) -> Result<Self, ShareError> {
        let mut v = Votes::new(xo::active_vote(db).map_err(|_| ShareError::Xo)?.addr);
        for d in DOMAINS {
            let res = SC7180[d].res_name;
            let entry = db
                .find(res)
                .filter(|e| e.addr != 0 && e.slave_id() == HW_ARC)
                .ok_or(ShareError::Domain(res))?;
            let mut buf = [0u8; MAX_LEVELS * 2];
            let n = db.aux(entry, &mut buf);
            let levels = arc::parse_levels(
                &buf[..n],
                entry.data_len as usize,
                SC7180[d].skip_retention_level,
            )
            .map_err(|_| ShareError::Domain(res))?;
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
            xo_ao_hold: [false; CLIENTS],
            xo_last: 0,
            xo_called: false,
            tree: Vec::new(),
            cache: Cache::new(),
        }
    }

    /// Put a domain's address and level table in place, and its active-only peer's, which is the same resource.
    pub fn install(&mut self, d: usize, addr: u32, levels: arc::Levels) -> Result<(), ShareError> {
        let table = Table {
            level: levels.level,
            count: levels.count,
            enable_corner: levels.enable_corner,
        };
        if !vote_rules::table_good(&table) {
            return Err(ShareError::Domain(SC7180[d].res_name));
        }
        for x in [Some(d), SC7180[d].peer.filter(|p| PEERS.contains(p))]
            .into_iter()
            .flatten()
        {
            self.dom[x].addr = addr;
            self.dom[x].table = Some(table);
        }
        Ok(())
    }

    pub fn synced(&self) -> bool {
        self.synced
    }

    /// The level table of domain `d`.
    pub fn table(&self, d: usize) -> Option<&Table> {
        self.dom.get(d).and_then(|x| x.table.as_ref())
    }

    /// The corner last sent as ACTIVE (and WAKE) to the resource of domain `d`.
    pub fn sent(&self, d: usize) -> Option<usize> {
        self.dom.get(d).and_then(|_| self.dom[owner(d)].sent)
    }

    /// The corner last sent as SLEEP to the resource of domain `d` (`None` for a domain without a peer, which gets none).
    pub fn sent_sleep(&self, d: usize) -> Option<usize> {
        self.dom.get(d).and_then(|_| self.dom[owner(d)].sent_sleep)
    }

    /// Whether XO was last sent on in the ACTIVE state (`None` before any XO change).
    pub fn xo_sent(&self) -> Option<bool> {
        self.xo_called
            .then_some(self.xo_last & 1 << State::Active as u8 != 0)
    }

    /// `(on, state)` of domain `d` as genpd sees it: its clients and its child.
    fn state(&self, d: usize) -> (bool, u32) {
        let (child_on, child_state) = match child_of(d) {
            Some(c) => self.state(c),
            None => (false, 0),
        };
        vote_rules::domain_state(&self.dom[d].on, &self.dom[d].want, child_on, child_state)
    }

    /// The corner domain `d` itself asks for, from its clients and its child.
    fn corner(&self, d: usize) -> Option<usize> {
        let t = self.dom[d].table.as_ref()?;
        let (child_on, child_state) = match child_of(d) {
            Some(c) => self.state(c),
            None => (false, 0),
        };
        Some(vote_rules::domain_corner(
            t,
            self.synced,
            &self.dom[d].on,
            &self.dom[d].want,
            child_on,
            child_state,
        ))
    }

    /// `rpmhpd_aggregate_corner` for the resource of domain `d`: its `(active, sleep)` corners. Until `sync_state` both are the top
    /// corner (`rpmhpd.c:703-707`). After it, each of the full domain and its peer that is on counts with `to_active_sleep` of its
    /// corner (`:701-702`, `:709-713`, the peer's `max(corner, enable_corner)` being what [`Votes::corner`] gives a domain that is
    /// on), the larger of each pair being sent (`:715`, `:732`); one that is off counts 0, as `rpmhpd_power_off` (`:759-773`) and an
    /// unenabled peer (`:709`) do.
    fn corners(&self, d: usize) -> Option<(usize, usize)> {
        let full = owner(d);
        let t = self.dom[full].table.as_ref()?;
        if !self.synced {
            let top = t.count - 1;
            return Some((top, top));
        }
        let (mut active, mut sleep) = (0u32, 0u32);
        for x in [Some(full), SC7180[full].peer].into_iter().flatten() {
            if self.dom[x].table.is_none() || !self.state(x).0 {
                continue;
            }
            let (a, s) = arc::to_active_sleep(x, self.corner(x)? as u32);
            active = active.max(a);
            sleep = sleep.max(s);
        }
        Some((active as usize, sleep as usize))
    }

    /// The sends of `rpmhpd_aggregate_corner` for the resource of domain `d`, if they differ from what was sent: ACTIVE (waiting
    /// when the corner rises, `rpmhpd.c:717-718`), then for a domain with a peer WAKE with the active corner and SLEEP with the
    /// sleep corner, neither waiting (`:724-735`).
    fn send(&mut self, d: usize, force: bool, out: &mut Vec<Cmd>) {
        let d = owner(d);
        let Some((active, sleep)) = self.corners(d) else {
            return;
        };
        let peer = SC7180[d].peer.is_some();
        if !force
            && self.dom[d].sent == Some(active)
            && (!peer || self.dom[d].sent_sleep == Some(sleep))
        {
            return;
        }
        let res = SC7180[d].res_name;
        let addr = self.dom[d].addr;
        let last = self.dom[d].sent.unwrap_or(0);
        out.push(Cmd {
            res,
            state: State::Active,
            cmd: TcsCmd {
                addr,
                data: active as u32,
                wait: vote_rules::wait_for_answer(active, last),
            },
        });
        self.cache.record(State::Active, addr, active as u32);
        if peer {
            for (state, corner) in [(State::Wake, active), (State::Sleep, sleep)] {
                out.push(Cmd {
                    res,
                    state,
                    cmd: TcsCmd {
                        addr,
                        data: corner as u32,
                        wait: false,
                    },
                });
                self.cache.record(state, addr, corner as u32);
            }
            self.dom[d].sent_sleep = Some(sleep);
        }
        self.dom[d].sent = Some(active);
    }

    /// `client` asks for performance state `state` (a voltage level, `opp-level`) on domain `d` (one of [`DOMAINS`] or
    /// [`PEERS`]) and holds it on (`on`), or lets go. Returns the commands, in order. A domain that is not loaded takes no votes.
    pub fn vote(&mut self, client: Client, d: usize, on: bool, state: u32) -> Vec<Cmd> {
        let mut out = Vec::new();
        if self.dom.get(d).is_none_or(|x| x.table.is_none()) || !takes_votes(d) {
            return out;
        }
        let c = client as usize;
        self.dom[d].on[c] = on;
        self.dom[d].want[c] = if on { state } else { 0 };
        let parent = SC7180[d].parent.filter(|p| self.dom[*p].table.is_some());
        match parent {
            Some(p) => {
                let up = self
                    .corners(p)
                    .is_some_and(|(n, _)| self.dom[owner(p)].sent.is_none_or(|s| n > s));
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

    /// The device tree consumer `node` holds domain `d` at performance state `state` (`on`), or lets go: genpd's vote of one
    /// device (`genpd_set_performance_state`, `drivers/pmdomain/core.c:470`), the domain taking the highest of its devices'
    /// (`_genpd_reeval_performance_state`, `:313`). The consumers are cast together as [`Client::Tree`]: at the highest state of
    /// those that hold `d`, and on while any does.
    pub fn vote_consumer(&mut self, node: &str, d: usize, on: bool, state: u32) -> Vec<Cmd> {
        let at = self
            .tree
            .iter()
            .position(|c| c.node == node && c.domain == d);
        match (at, on) {
            (Some(i), true) => self.tree[i].state = state,
            (Some(i), false) => {
                self.tree.remove(i);
            }
            (None, true) => self.tree.push(Consumer {
                node: String::from(node),
                domain: d,
                state,
            }),
            (None, false) => {}
        }
        let held = self
            .tree
            .iter()
            .filter(|c| c.domain == d)
            .map(|c| c.state)
            .max();
        self.vote(Client::Tree, d, held.is_some(), held.unwrap_or(0))
    }

    /// The device tree consumers that hold a domain now.
    pub fn consumers(&self) -> &[Consumer] {
        &self.tree
    }

    /// `client` takes (`on`) or lets go of `bi_tcxo`, the XO clock valid in all three states: a consumer's `clk_prepare` or
    /// `clk_unprepare`, the first and last of which reach `clk_rpmh_prepare` and `clk_rpmh_unprepare` (`clk-rpmh.c:213-232`).
    pub fn xo(&mut self, client: Client, on: bool) -> Vec<Cmd> {
        self.xo_hold[client as usize] = on;
        self.xo_send()
    }

    /// The same for `bi_tcxo_ao`, which holds XO while the application processor is awake only.
    pub fn xo_ao(&mut self, client: Client, on: bool) -> Vec<Cmd> {
        self.xo_ao_hold[client as usize] = on;
        self.xo_send()
    }

    /// `clk_rpmh_aggregate_state_send_command` and `clk_rpmh_send_aggregate_command` (`clk-rpmh.c:152-211`): the union of the two
    /// clocks' states, and for each state from SLEEP to ACTIVE whose bit changed since the last send, one command, the ACTIVE one
    /// waiting when any state is on. `cmd.data` starts at 0 and becomes the "on" value at the first state that is on (`:154`,
    /// `:166-167`).
    fn xo_send(&mut self) -> Vec<Cmd> {
        let full = if vote_rules::xo_on(&self.xo_hold) {
            XO_STATES
        } else {
            0
        };
        let ao = if vote_rules::xo_on(&self.xo_ao_hold) {
            XO_AO_STATES
        } else {
            0
        };
        let aggr = full | ao;
        let addr = self.xo_addr + xo::ARC_EN_OFFSET;
        let mut out = Vec::new();
        let mut data = 0;
        for state in [State::Sleep, State::Wake, State::Active] {
            let bit = 1 << state as u8;
            if (self.xo_last ^ aggr) & bit == 0 {
                continue;
            }
            if aggr & bit != 0 {
                data = xo::ON_VALUE;
            }
            out.push(Cmd {
                res: xo::RES_NAME,
                state,
                cmd: TcsCmd {
                    addr,
                    data,
                    wait: aggr != 0 && state == State::Active,
                },
            });
            self.cache.record(state, addr, data);
        }
        self.xo_last = aggr;
        self.xo_called = true;
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
        // Parents last: they only go down here or stay. A peer is sent with its full domain, the one resource.
        for d in [
            sc7180::CX,
            sc7180::GFX,
            sc7180::LCX,
            sc7180::LMX,
            sc7180::MSS,
            sc7180::MX,
        ] {
            if !keep.contains(&d) {
                self.send(d, true, &mut out);
            }
        }
        out
    }

    /// The resource at `addr`, if it is one of this table's.
    pub fn name_of(&self, addr: u32) -> Option<&'static str> {
        if addr == self.xo_addr + xo::ARC_EN_OFFSET {
            return Some(xo::RES_NAME);
        }
        DOMAINS
            .iter()
            .find(|&&d| self.dom[d].table.is_some() && self.dom[d].addr == addr)
            .map(|&d| SC7180[d].res_name)
    }

    /// Who holds what, and what was sent in each state.
    pub fn report(&self) -> String {
        let mut s = String::new();
        s.push_str(if self.synced {
            "synced: yes\n"
        } else {
            "synced: no (every domain sent is at its top)\n"
        });
        for d in DOMAINS {
            let x = &self.dom[d];
            let Some(t) = x.table.as_ref() else {
                s.push_str(&format!("{}: not loaded\n", SC7180[d].res_name));
                continue;
            };
            let levels: Vec<u32> = t.level[..t.count].to_vec();
            let sent = match (x.sent, x.sent_sleep) {
                (Some(c), Some(z)) => format!(
                    "active {c} wake {c} sleep {z} (levels {}, {})",
                    t.level[c], t.level[z]
                ),
                (Some(c), None) => format!(
                    "active {c} (level {}); no sleep or wake set (no peer)",
                    t.level[c]
                ),
                _ => String::from("nothing yet (the firmware's vote stands)"),
            };
            s.push_str(&format!(
                "{} at {:#x}: levels {:?}; sent {}\n",
                SC7180[d].res_name, x.addr, levels, sent
            ));
            for y in [Some(d), SC7180[d].peer.filter(|p| PEERS.contains(p))]
                .into_iter()
                .flatten()
            {
                for (c, client) in CLIENT_NAMES.iter().enumerate() {
                    if self.dom[y].on[c] {
                        s.push_str(&format!(
                            "  {} holds {} at level {}\n",
                            client, SC7180[y].name, self.dom[y].want[c]
                        ));
                    }
                }
                for k in self.tree.iter().filter(|k| k.domain == y) {
                    s.push_str(&format!(
                        "    tree: {} holds {} at level {}\n",
                        k.node, SC7180[y].name, k.state
                    ));
                }
            }
        }
        let holders = |h: &[bool; CLIENTS]| -> Vec<&str> {
            (0..CLIENTS)
                .filter(|&c| h[c])
                .map(|c| CLIENT_NAMES[c])
                .collect()
        };
        let on = |state: State| {
            if self.xo_last & 1 << state as u8 != 0 {
                "on"
            } else {
                "off"
            }
        };
        s.push_str(&format!(
            "xo.lvl at {:#x}: {}; bi_tcxo held by {:?}, bi_tcxo_ao held by {:?}\n",
            self.xo_addr,
            if self.xo_called {
                format!(
                    "sent sleep {} wake {} active {}",
                    on(State::Sleep),
                    on(State::Wake),
                    on(State::Active)
                )
            } else {
                String::from("nothing yet")
            },
            holders(&self.xo_hold),
            holders(&self.xo_ao_hold)
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

/// The [`Client::Tree`] level of each domain: `Some(highest state)` where any consumer holds it. Linux would vote each consumer's
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
            arc::Levels {
                level,
                count: l.len(),
                enable_corner: 1,
            }
        };
        v.install(sc7180::CX, 0x30000, t(&[0, 16, 64, 128, 192, 256, 384]))
            .unwrap();
        v.install(sc7180::MX, 0x30010, t(&[0, 16, 256, 384]))
            .unwrap();
        v.install(
            sc7180::GFX,
            0x30050,
            t(&[0, 16, 64, 128, 192, 256, 320, 384, 416]),
        )
        .unwrap();
        v.install(sc7180::LCX, 0x30030, t(&[0, 16, 64, 128, 192, 256]))
            .unwrap();
        v.install(sc7180::LMX, 0x30040, t(&[0, 16, 64, 128, 192, 256]))
            .unwrap();
        v.install(
            sc7180::MSS,
            0x30060,
            t(&[0, 16, 64, 128, 192, 256, 320, 384]),
        )
        .unwrap();
        v
    }

    fn active(cmds: &[Cmd]) -> Vec<(&'static str, u32)> {
        cmds.iter()
            .filter(|c| c.state == State::Active)
            .map(|c| (c.res, c.cmd.data))
            .collect()
    }

    #[test]
    fn the_modem_start_sends_what_it_sent_before() {
        // Today's log: mx.lvl 0x3, cx.lvl 0x6, mss.lvl 0x7, xo.lvl 0x3.
        let mut v = laptop();
        let mut cmds = v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        cmds.extend(v.xo(Client::Modem, true));
        assert_eq!(
            active(&cmds),
            [("mx.lvl", 3), ("cx.lvl", 6), ("mss.lvl", 7), ("xo.lvl", 3)]
        );
        // cx and mx carry WAKE and SLEEP too, mss does not.
        assert_eq!(cmds.iter().filter(|c| c.res == "cx.lvl").count(), 3);
        assert_eq!(cmds.iter().filter(|c| c.res == "mss.lvl").count(), 1);
        assert!(
            cmds.iter()
                .filter(|c| c.state == State::Active)
                .all(|c| c.cmd.wait)
        );
    }

    #[test]
    fn before_sync_a_release_keeps_the_top() {
        let mut v = laptop();
        v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true);
        // Unchanged corners send nothing.
        assert!(
            v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], false)
                .is_empty()
        );
        assert_eq!(
            (v.sent(sc7180::CX), v.sent(sc7180::MSS)),
            (Some(6), Some(7))
        );
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
            [
                ("cx.lvl", 5),
                ("gfx.lvl", 0),
                ("lcx.lvl", 0),
                ("lmx.lvl", 0),
                ("mss.lvl", 0),
                ("mx.lvl", 2)
            ]
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
        assert_eq!(
            active(&cmds),
            [
                ("cx.lvl", 6),
                ("lcx.lvl", 0),
                ("lmx.lvl", 0),
                ("mss.lvl", 0),
                ("mx.lvl", 3)
            ]
        );
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
            off.iter()
                .map(|c| (c.state, c.cmd.data, c.cmd.wait))
                .collect::<Vec<_>>(),
            [
                (State::Sleep, 0, false),
                (State::Wake, 0, false),
                (State::Active, 0, false)
            ]
        );
    }

    #[test]
    fn a_falling_table_is_refused() {
        let mut v = Votes::new(0);
        let mut level = [0u32; MAX_LEVELS];
        level[..3].copy_from_slice(&[0, 256, 128]);
        assert!(
            v.install(
                sc7180::CX,
                0x30000,
                arc::Levels {
                    level,
                    count: 3,
                    enable_corner: 1
                }
            )
            .is_err()
        );
        assert!(v.vote(Client::Tree, sc7180::CX, true, 64).is_empty());
    }

    /// 20 000 random votes, proxies, XO changes and one sync against a model written apart from the table: what Linux would send
    /// for every domain, from the holders alone. Covers what Verus does not: the table's own bookkeeping (the parent worked out
    /// again, nothing sent when nothing changed, the last corner sent).
    #[test]
    fn random_votes_match_an_independent_model() {
        const STATES: [u32; 9] = [0, 16, 64, 128, 192, 256, 384, 416, INT_MAX];
        const VOTERS: [Client; 4] = [Client::Modem, Client::Adsp, Client::Venus, Client::Tree];
        const ALL: [Client; 5] = [
            Client::Modem,
            Client::Adsp,
            Client::Venus,
            Client::Tree,
            Client::Gcc,
        ];
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
                let (c, d, on, s) = (
                    VOTERS[next(4)],
                    DOMAINS[next(6)],
                    next(2) == 0,
                    STATES[next(9)],
                );
                holds[d][c as usize] = on.then_some(s);
                touched[d] = true;
                (format!("vote {c:?} {d} {on} {s}"), v.vote(c, d, on, s))
            } else if roll < 85 {
                let (c, d1, d2, on) = (
                    VOTERS[next(4)],
                    DOMAINS[next(6)],
                    DOMAINS[next(6)],
                    next(2) == 0,
                );
                for d in [d1, d2] {
                    holds[d][c as usize] = on.then_some(INT_MAX);
                    touched[d] = true;
                }
                (
                    format!("proxy {c:?} {d1} {d2} {on}"),
                    v.proxy(c, &[d1, d2], on),
                )
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
            for cmd in cmds
                .iter()
                .filter(|c| c.state == State::Active && c.res != xo::RES_NAME)
            {
                let d = DOMAINS
                    .into_iter()
                    .find(|&d| SC7180[d].res_name == cmd.res)
                    .unwrap();
                let last = before[d].unwrap_or(0) as u32;
                assert_eq!(
                    cmd.cmd.wait,
                    cmd.cmd.data > last,
                    "step {step}, {what}: wait on {} {}",
                    cmd.res,
                    cmd.cmd.data
                );
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
                        assert!(
                            t.level[model] >= *s || model == top,
                            "step {step}, {what}: domain {d} below {s}"
                        );
                    }
                } else if touched[d] {
                    assert_eq!(
                        sent,
                        Some(top),
                        "step {step}, {what}: domain {d} before the sync"
                    );
                } else {
                    assert_eq!(sent, None, "step {step}, {what}: domain {d} sent untouched");
                }
            }
            let any = xo.iter().any(|&h| h);
            assert_eq!(
                v.xo_sent(),
                xo_called.then_some(any),
                "step {step}, {what}: xo"
            );
        }
        assert!(synced, "the run never synced");
    }

    #[test]
    fn the_tree_floor_is_the_highest() {
        let c = |node: &str, domain, state| Consumer {
            node: node.into(),
            domain,
            state,
        };
        let f = tree_floor(&[
            c("gcc", sc7180::CX, 0),
            c("usb", sc7180::CX, 256),
            c("qup", sc7180::CX, 64),
            c("x", 99, 1),
        ]);
        assert_eq!(f[sc7180::CX], Some(256));
        assert_eq!(f[sc7180::MX], None);
    }

    fn all(cmds: &[Cmd]) -> Vec<(&'static str, State, u32)> {
        cmds.iter().map(|c| (c.res, c.state, c.cmd.data)).collect()
    }

    /// The laptop's device tree consumers as `/proc/lucina-rpmh` listed them on 2026-11-07 (`evidence/juno-rpmh/proc-lucina-rpmh.txt`):
    /// all on `cx`.
    const LAPTOP_TREE: [(&str, u32); 16] = [
        ("/soc@0/clock-controller@100000", 0),
        ("/soc@0/geniqup@8c0000/i2c@880000", 64),
        ("/soc@0/geniqup@8c0000/i2c@888000", 64),
        ("/soc@0/geniqup@8c0000/serial@88c000", 256),
        ("/soc@0/geniqup@8c0000/i2c@890000", 64),
        ("/soc@0/geniqup@8c0000/i2c@894000", 64),
        ("/soc@0/geniqup@ac0000/i2c@a80000", 64),
        ("/soc@0/geniqup@ac0000/i2c@a84000", 64),
        ("/soc@0/geniqup@ac0000/i2c@a8c000", 64),
        ("/soc@0/geniqup@ac0000/i2c@a90000", 64),
        ("/soc@0/ufshc@1d84000", 0),
        ("/soc@0/phy@1d87000", 0),
        ("/soc@0/mmc@8804000", 256),
        ("/soc@0/usb@a6f8800", 256),
        (
            "/soc@0/display-subsystem@ae00000/display-controller@ae01000",
            256,
        ),
        ("/soc@0/display-subsystem@ae00000/dsi@ae94000", 192),
    ];

    /// The boot as the kernel runs it: GCC's XO, the modem's and the audio DSP's proxies, Venus at its top, the handovers, then
    /// the sync of `lucina.rpmh=linux`. `per_consumer` casts the tree as one vote per device tree consumer (this patch); without it
    /// the tree is one vote at `tree_floor`, as before.
    fn laptop_boot(per_consumer: bool) -> (Votes, Vec<Cmd>) {
        let mut v = laptop();
        let mut cmds = v.xo(Client::Gcc, true);
        cmds.extend(v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], true));
        cmds.extend(v.xo(Client::Modem, true));
        cmds.extend(v.proxy(Client::Adsp, &[sc7180::LCX, sc7180::LMX], true));
        cmds.extend(v.xo(Client::Adsp, true));
        cmds.extend(v.vote(Client::Venus, sc7180::CX, true, 384));
        cmds.extend(v.xo(Client::Modem, false));
        cmds.extend(v.proxy(Client::Modem, &[sc7180::CX, sc7180::MSS], false));
        cmds.extend(v.xo(Client::Adsp, false));
        cmds.extend(v.proxy(Client::Adsp, &[sc7180::LCX, sc7180::LMX], false));
        cmds.extend(v.vote(Client::Venus, sc7180::CX, false, 0));
        if per_consumer {
            for (node, level) in LAPTOP_TREE {
                cmds.extend(v.vote_consumer(node, sc7180::CX, true, level));
            }
        } else {
            let consumers: Vec<Consumer> = LAPTOP_TREE
                .iter()
                .map(|&(node, state)| Consumer {
                    node: node.into(),
                    domain: sc7180::CX,
                    state,
                })
                .collect();
            let floor = tree_floor(&consumers);
            for d in DOMAINS {
                if let Some(level) = floor[d] {
                    cmds.extend(v.vote(Client::Tree, d, true, level));
                }
            }
        }
        cmds.extend(v.sync());
        (v, cmds)
    }

    /// The net effect of patch 1344 on the laptop: every ACTIVE value, and every command, is what the kernel sent before it.
    #[test]
    fn the_laptop_boot_sends_todays_values() {
        let (v, cmds) = laptop_boot(true);
        let (_, before) = laptop_boot(false);
        assert_eq!(all(&cmds), all(&before));
        assert_eq!(
            active(&cmds),
            [
                ("xo.lvl", 3),
                ("mx.lvl", 3),
                ("cx.lvl", 6),
                ("mss.lvl", 7),
                ("lcx.lvl", 5),
                ("lmx.lvl", 5),
                // rpmhpd_sync_state: the tree's 256 on cx (corner 5, with mx at 256 = corner 2), 0 for the rest.
                ("cx.lvl", 5),
                ("gfx.lvl", 0),
                ("lcx.lvl", 0),
                ("lmx.lvl", 0),
                ("mss.lvl", 0),
                ("mx.lvl", 2),
            ]
        );
        // What the laptop's report said after the sync (cx corner 5, mx corner 2, XO on for GCC).
        assert_eq!(
            (v.sent(sc7180::CX), v.sent(sc7180::MX), v.xo_sent()),
            (Some(5), Some(2), Some(true))
        );
        // The cache holds a sleep and a wake value for cx, mx and XO, the same ones: with no vote on cx_ao, mx_ao or
        // bi_tcxo_ao, Linux's sleep corner is the active one (to_active_sleep of a full domain), so a flush writes nothing.
        let entries: Vec<_> = v.cache.entries().collect();
        assert!(entries.contains(&(0x30000, Some(5), Some(5))));
        assert!(entries.contains(&(0x30010, Some(2), Some(2))));
        assert!(entries.contains(&(0x30090, Some(3), Some(3))));
        assert_eq!(v.cache.valid().count(), 0);
    }

    /// `rpmhpd_aggregate_corner` with an active-only peer (rpmhpd.c:692-739): a vote on cx_ao holds cx.lvl awake only, and its
    /// parent mx_ao holds mx.lvl the same way.
    #[test]
    fn a_vote_on_an_active_only_peer_has_a_sleep_corner_of_zero() {
        let mut v = laptop();
        v.sync();
        let up = v.vote(Client::Venus, sc7180::CX_AO, true, 256);
        // Parent first on the way up: mx_ao at 256 (corner 2) then cx_ao at 256 (corner 5); WAKE the active corner, SLEEP 0.
        assert_eq!(
            all(&up),
            [
                ("mx.lvl", State::Active, 2),
                ("mx.lvl", State::Wake, 2),
                ("mx.lvl", State::Sleep, 0),
                ("cx.lvl", State::Active, 5),
                ("cx.lvl", State::Wake, 5),
                ("cx.lvl", State::Sleep, 0),
            ]
        );
        assert_eq!(
            (v.sent(sc7180::CX), v.sent_sleep(sc7180::CX)),
            (Some(5), Some(0))
        );
        let valid: Vec<_> = v.cache.valid().collect();
        assert_eq!(
            valid,
            [(0x30000, 0, 5), (0x30010, 0, 2)],
            "rpmh_flush would write cx and mx: sleep 0, wake the corner"
        );
        // The full domain at 64 (corner 2) as well: ACTIVE and WAKE stay at the peer's 5, SLEEP is the full domain's 2.
        let full = v.vote(Client::Tree, sc7180::CX, true, 64);
        assert_eq!(
            all(&full),
            [
                ("cx.lvl", State::Active, 5),
                ("cx.lvl", State::Wake, 5),
                ("cx.lvl", State::Sleep, 2),
                ("mx.lvl", State::Active, 2),
                ("mx.lvl", State::Wake, 2),
                ("mx.lvl", State::Sleep, 2),
            ]
        );
        // The peer lets go: the full domain's corner in all three.
        v.vote(Client::Venus, sc7180::CX_AO, false, 0);
        assert_eq!(
            (v.sent(sc7180::CX), v.sent_sleep(sc7180::CX)),
            (Some(2), Some(2))
        );
        assert_eq!(
            (v.sent(sc7180::MX), v.sent_sleep(sc7180::MX)),
            (Some(2), Some(2))
        );
        // And the full domain: all 0.
        v.vote(Client::Tree, sc7180::CX, false, 0);
        assert_eq!(
            (
                v.sent(sc7180::CX),
                v.sent_sleep(sc7180::CX),
                v.sent(sc7180::MX)
            ),
            (Some(0), Some(0), Some(0))
        );
    }

    /// Before `sync_state` a peer's vote is clamped like any other: the top corner in all three states (rpmhpd.c:703-707).
    #[test]
    fn before_sync_a_peer_vote_is_the_top_asleep_too() {
        let mut v = laptop();
        let cmds = v.vote(Client::Modem, sc7180::CX_AO, true, 64);
        assert_eq!(
            all(&cmds),
            [
                ("mx.lvl", State::Active, 3),
                ("mx.lvl", State::Wake, 3),
                ("mx.lvl", State::Sleep, 3),
                ("cx.lvl", State::Active, 6),
                ("cx.lvl", State::Wake, 6),
                ("cx.lvl", State::Sleep, 6),
            ]
        );
        // The domains without a peer get no sleep or wake set.
        let mss = v.proxy(Client::Modem, &[sc7180::MSS], true);
        assert_eq!(all(&mss), [("mss.lvl", State::Active, 7)]);
        assert_eq!(v.sent_sleep(sc7180::MSS), None);
    }

    /// `clk_rpmh_send_aggregate_command` over `bi_tcxo` and `bi_tcxo_ao` (clk-rpmh.c:152-211): each state whose bit changed, SLEEP
    /// first, ACTIVE last and waiting while anything is on.
    #[test]
    fn xo_is_two_clocks_on_one_resource() {
        let mut v = Votes::new(0x30090);
        let s = |cmds: &[Cmd]| {
            cmds.iter()
                .map(|c| (c.state, c.cmd.data, c.cmd.wait))
                .collect::<Vec<_>>()
        };
        // bi_tcxo_ao alone: WAKE and ACTIVE on; SLEEP was never on, so it is not sent.
        assert_eq!(
            s(&v.xo_ao(Client::Gcc, true)),
            [(State::Wake, 3, false), (State::Active, 3, true)]
        );
        // bi_tcxo too: only SLEEP changes.
        assert_eq!(s(&v.xo(Client::Modem, true)), [(State::Sleep, 3, false)]);
        // bi_tcxo let go: SLEEP back to 0, WAKE and ACTIVE stay for the active-only clock.
        assert_eq!(s(&v.xo(Client::Modem, false)), [(State::Sleep, 0, false)]);
        assert_eq!(
            v.cache.valid().collect::<Vec<_>>(),
            [(0x30090, 0, 3)],
            "rpmh_flush would write XO off asleep, on awake"
        );
        // The last holder: WAKE and ACTIVE off, not waiting.
        assert_eq!(
            s(&v.xo_ao(Client::Gcc, false)),
            [(State::Wake, 0, false), (State::Active, 0, false)]
        );
        assert_eq!(v.xo_sent(), Some(false));
        // GCC's boot-long hold of bi_tcxo, as the kernel takes it: all three on, ACTIVE waiting; another holder changes nothing.
        let mut v = Votes::new(0x30090);
        assert_eq!(
            s(&v.xo(Client::Gcc, true)),
            [
                (State::Sleep, 3, false),
                (State::Wake, 3, false),
                (State::Active, 3, true)
            ]
        );
        assert!(v.xo_ao(Client::Venus, true).is_empty());
        assert!(v.xo(Client::Adsp, true).is_empty());
        assert!(v.xo(Client::Adsp, false).is_empty());
    }

    /// genpd's per-device performance states: the domain takes the highest of its consumers (core.c:313), so one consumer
    /// lowered or let go leaves the others' levels.
    #[test]
    fn tree_consumers_are_counted_one_by_one() {
        let mut v = laptop();
        v.sync();
        v.vote_consumer("usb", sc7180::CX, true, 256);
        v.vote_consumer("qup", sc7180::CX, true, 64);
        assert_eq!(v.sent(sc7180::CX), Some(5));
        // Lowering one that is not the highest changes nothing.
        assert!(v.vote_consumer("qup", sc7180::CX, true, 16).is_empty());
        // The highest lets go: the next one's level (16 = corner 1), with mx following it down, parent last.
        let down = v.vote_consumer("usb", sc7180::CX, false, 0);
        assert_eq!(active(&down), [("cx.lvl", 1), ("mx.lvl", 1)]);
        assert_eq!(v.consumers().len(), 1);
        v.vote_consumer("qup", sc7180::CX, false, 0);
        assert_eq!((v.sent(sc7180::CX), v.sent(sc7180::MX)), (Some(0), Some(0)));
        // A consumer at level 0 holds the first non-zero corner, as genpd_power_on does.
        v.vote_consumer("gcc", sc7180::CX, true, 0);
        assert_eq!(v.sent(sc7180::CX), Some(1));
    }

    #[test]
    fn the_report_names_holders_states_and_the_cache() {
        let mut v = laptop();
        v.xo(Client::Gcc, true);
        v.vote_consumer("/soc@0/usb@a6f8800", sc7180::CX, true, 256);
        v.vote(Client::Venus, sc7180::CX_AO, true, 64);
        v.sync();
        let r = v.report();
        assert!(r.contains("cx.lvl at 0x30000: levels [0, 16, 64, 128, 192, 256, 384]; sent active 5 wake 5 sleep 5"), "{r}");
        assert!(
            r.contains(
                "  tree holds cx at level 256\n    tree: /soc@0/usb@a6f8800 holds cx at level 256\n"
            ),
            "{r}"
        );
        assert!(r.contains("  venus holds cx_ao at level 64\n"), "{r}");
        assert!(r.contains("gfx.lvl at 0x30050: levels [0, 16, 64, 128, 192, 256, 320, 384, 416]; sent active 0 (level 0); no sleep"), "{r}");
        assert!(r.contains("xo.lvl at 0x30090: sent sleep on wake on active on; bi_tcxo held by [\"gcc\"], bi_tcxo_ao held by []"), "{r}");
        let c = v
            .cache
            .report(|a| String::from(v.name_of(a).unwrap_or("?")));
        assert!(
            c.contains("0x30000 cx.lvl: sleep 0x5 wake 0x5; flush writes nothing\n"),
            "{c}"
        );
        assert!(
            c.contains("0x30050 gfx.lvl: sleep - wake 0x0; flush writes nothing\n"),
            "{c}"
        );
    }
}
