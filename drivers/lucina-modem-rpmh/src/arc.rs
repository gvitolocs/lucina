//! RPMh ARC power domains (`cx`, `mx`, `mss`, ...) on SC7180: which level numbers the firmware has, and the votes
//! Linux sends to raise a domain to its top level.
//!
//! Derived from Linux 7.1 `drivers/pmdomain/qcom/rpmhpd.c` (GPL-2.0; Copyright The Linux Foundation),
//! `drivers/pmdomain/core.c` (genpd, for the order in which a performance state and a power-on reach the domains),
//! `drivers/remoteproc/qcom_q6v5_pas.c` (`qcom_pas_pds_enable`) and `include/dt-bindings/power/qcom,rpmhpd.h`, and from
//! petitstrawberry/scarlet-project-chromebook `drivers/pmdomain/qcom-sc7180-rpmhpd/src/lib.rs` (GPL-2.0-only).
//!
//! # Levels
//!
//! The firmware's command DB holds, for each ARC resource such as `cx.lvl`, a table of `u16` voltage levels
//! (`vlvl`, such as 16 = retention, 64 = low SVS, 256 = nominal), lowest first, possibly padded with zeros.
//! The index in that table is the hardware level (`hlvl`, "corner"), and **the vote is the index, not the voltage
//! level**: `rpmhpd_send_corner` puts `corner` in the command's data. [`parse_levels`] is
//! `rpmhpd_update_level_mapping`. A device tree performance state (`opp-level`, a `vlvl`) is mapped to the first
//! index whose level is at least as large ([`Pds::set_performance_state`]; the top index when none is).
//!
//! # What a proxy vote at the maximum sends, in order
//!
//! `qcom_pas_pds_enable` does, for each proxy power domain of the modem (`cx`, then `mss`;
//! `mpss_resource_init.proxy_pd_names`): `dev_pm_genpd_set_performance_state(pd, INT_MAX)`, then
//! `pm_runtime_get_sync(pd)`. The domain is runtime-suspended, so the first call only remembers the state; the get
//! runs `genpd_runtime_resume`, which restores that state **before** powering on. [`Pds::proxy_vote_max`] replays that:
//!
//! 1. `cx`'s performance state `INT_MAX` is scaled up: parents first. `sc7180_rpmhpds[SC7180_CX]` is
//!    `cx_w_mx_parent`: **cx has mx as its parent** (and `cx_ao` has `mx_ao`). The state reaches `mx`'s
//!    `rpmhpd_set_performance_state` unchanged (no `required-opps` in the shared OPP table, so
//!    `dev_pm_opp_xlate_performance_state` returns it as is), then `cx`'s. Neither domain is enabled yet, so each
//!    only records its `corner` (the top index); nothing is sent.
//! 2. `genpd_power_on(cx)` powers parents first: `rpmhpd_power_on(mx)` sends the `mx.lvl` vote, then
//!    `rpmhpd_power_on(cx)` sends the `cx.lvl` vote.
//! 3. For `mss` (no parent, no peer): the same two steps, which send the `mss.lvl` vote.
//!
//! Each `rpmhpd_aggregate_corner` sends the aggregated corner as ACTIVE_ONLY and, when the domain has a peer (cx, mx,
//! not mss), also as WAKE_ONLY and then SLEEP. The ACTIVE_ONLY send waits for the answer (`rpmh_write`) when the
//! corner rises above the domain's current active corner, which the first vote always does; WAKE_ONLY and SLEEP never
//! wait (`rpmh_write_async`).
//!
//! # Sleep and wake sets
//!
//! Linux also sends, for every domain that has a peer, the same corner as WAKE_ONLY (what RPMh applies when the
//! application processor wakes up again) and the SLEEP corner (the larger of the domain's own and its enabled peer's,
//! with the active-only peer's sleep corner being 0), through `rpmhpd_send_corner` with `rpmh_write_async`. [`Pds`]
//! computes all three ([`Send::state`]); [`Pds::active_cmds`] keeps the ACTIVE_ONLY ones for an active-only caller. The
//! kernel's votes go through [`crate::share::Votes`], which sends all three with [`to_active_sleep`]'s corners and keeps
//! them in the `rpmh.c` cache ([`crate::rpmh::Cache`]), so a flush has the sleep and wake values Linux would write.
//!
//! Before `sync_state` Linux clamps every aggregate to the top corner (`state_synced` false), so the votes above
//! would be the same corner whatever the request. [`Pds::set_state_synced`] sets the flag; it starts false, as in
//! Linux, because nothing here ever calls `rpmhpd_sync_state`.

use alloc::vec::Vec;

use lucina_qcom_soc::cmd_db::{CmdDb, HW_ARC};
use lucina_qcom_soc::io::RegIo;

use crate::rsc::TcsCmd;

/// `RPMH_ARC_MAX_LEVELS`.
pub const MAX_LEVELS: usize = 32;
/// `RPMH_REGULATOR_LEVEL_RETENTION`, skipped for domains with `skip_retention_level`.
pub const LEVEL_RETENTION: u32 = 16;
/// `INT_MAX`, what `qcom_pas_pds_enable` asks for.
pub const INT_MAX: u32 = i32::MAX as u32;

/// Domain indexes of `dt-bindings/power/qcom,rpmhpd.h` for SC7180.
pub mod sc7180 {
    pub const CX: usize = 0;
    pub const CX_AO: usize = 1;
    pub const GFX: usize = 2;
    pub const MX: usize = 3;
    pub const MX_AO: usize = 4;
    pub const LMX: usize = 5;
    pub const LCX: usize = 6;
    pub const MSS: usize = 7;
    pub const COUNT: usize = 8;
}
use sc7180::*;

/// `enum rpmh_state`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum State {
    Sleep = 0,
    Wake = 1,
    Active = 2,
}

/// The constant part of a `struct rpmhpd`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Desc {
    /// `pd.name`.
    pub name: &'static str,
    /// `res_name`, the command DB id.
    pub res_name: &'static str,
    pub active_only: bool,
    /// `peer`: the active-only twin, or the full domain for an active-only one.
    pub peer: Option<usize>,
    /// `parent`, as an index.
    pub parent: Option<usize>,
    pub skip_retention_level: bool,
}

const fn d(
    name: &'static str,
    res_name: &'static str,
    active_only: bool,
    peer: Option<usize>,
    parent: Option<usize>,
) -> Desc {
    Desc {
        name,
        res_name,
        active_only,
        peer,
        parent,
        skip_retention_level: false,
    }
}

/// `sc7180_rpmhpds`: `cx_w_mx_parent`, `cx_ao_w_mx_parent`, `gfx`, `mx`, `mx_ao`, `lmx`, `lcx`, `mss`.
pub const SC7180: [Desc; COUNT] = [
    d("cx", "cx.lvl", false, Some(CX_AO), Some(MX)),
    d("cx_ao", "cx.lvl", true, Some(CX), Some(MX_AO)),
    d("gfx", "gfx.lvl", false, None, None),
    d("mx", "mx.lvl", false, Some(MX_AO), None),
    d("mx_ao", "mx.lvl", true, Some(MX), None),
    d("lmx", "lmx.lvl", false, None, None),
    d("lcx", "lcx.lvl", false, None, None),
    d("mss", "mss.lvl", false, None, None),
];

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ArcError {
    /// The command DB has no entry (or one with address 0) for this resource (`-ENODEV`).
    Missing(&'static str),
    /// The entry is not an ARC resource (`RPMh slave ID mismatch`, `-EINVAL`).
    NotArc { res: &'static str, hw: u32 },
    /// More than 32 levels (`-EINVAL`).
    TooManyLevels(usize),
    /// No level at all. Linux accepts that and then computes `level_count - 1` as an unsigned wrap.
    NoLevels,
    /// The domain index is not an SC7180 domain, or its levels were not loaded.
    NotLoaded(usize),
}

impl core::fmt::Display for ArcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            ArcError::Missing(res) => write!(f, "command DB has no address for {res}"),
            ArcError::NotArc { res, hw } => write!(f, "{res} is hardware type {hw}, not ARC"),
            ArcError::TooManyLevels(n) => write!(f, "{n} ARC levels, at most {MAX_LEVELS}"),
            ArcError::NoLevels => f.write_str("empty ARC level table"),
            ArcError::NotLoaded(i) => write!(f, "power domain {i} has no levels loaded"),
        }
    }
}

/// What `rpmhpd_update_level_mapping` leaves in a `struct rpmhpd`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Levels {
    /// `level[]`, `vlvl` by `hlvl`.
    pub level: [u32; MAX_LEVELS],
    /// `level_count`, after trailing zero padding is cut off.
    pub count: usize,
    /// `enable_corner`: the first index whose level is not 0.
    pub enable_corner: usize,
}

/// `rpmhpd_update_level_mapping`. `aux` is the auxiliary data read from the command DB, of which `data_len` bytes
/// are the table (`aux` may be shorter if the caller's buffer was; it must hold `2 * (data_len / 2)` bytes of it).
/// Little-endian `u16` entries; an odd trailing byte is ignored, as `level_count >>= 1` does.
pub fn parse_levels(aux: &[u8], data_len: usize, skip_retention: bool) -> Result<Levels, ArcError> {
    let mut count = data_len >> 1;
    if count > MAX_LEVELS {
        return Err(ArcError::TooManyLevels(count));
    }
    if count == 0 || aux.len() < count * 2 {
        return Err(ArcError::NoLevels);
    }
    let mut level = [0u32; MAX_LEVELS];
    let mut enable_corner = 0usize;
    for i in 0..count {
        let v = u32::from(u16::from_le_bytes([aux[2 * i], aux[2 * i + 1]]));
        if skip_retention && v == LEVEL_RETENTION {
            continue;
        }
        level[i] = v;

        // Remember the first corner with non-zero level.
        if level[enable_corner] == 0 && level[i] != 0 {
            enable_corner = i;
        }

        // The AUX data may be zero padded. These 0 valued entries at the end of the map must be ignored.
        if i > 0 && level[i] == 0 {
            count = i;
            break;
        }
    }
    Ok(Levels {
        level,
        count,
        enable_corner,
    })
}

/// `to_active_sleep` (Linux v6.12 `drivers/pmdomain/qcom/rpmhpd.c:672-681`): the `(active, sleep)` corners a vote of `corner` on
/// domain `idx` asks for. The active corner is the vote; the sleep corner is the vote too, except on an active-only domain
/// (`cx_ao`, `mx_ao`), which asks for nothing while the application processor sleeps.
pub fn to_active_sleep(idx: usize, corner: u32) -> (u32, u32) {
    if SC7180[idx].active_only {
        (corner, 0)
    } else {
        (corner, corner)
    }
}

/// One `rpmhpd_send_corner`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Send {
    /// The domain the send is made for (an index of [`sc7180`]); a peer's votes are made through its twin.
    pub domain: usize,
    pub state: State,
    /// The resource address (`pd->addr`).
    pub addr: u32,
    /// The corner (a level index).
    pub data: u32,
    /// Wait for the answer: `rpmh_write` rather than `rpmh_write_async`.
    pub sync: bool,
}

impl Send {
    /// As a command for the active set; `wait` carries `sync`.
    pub fn cmd(&self) -> TcsCmd {
        TcsCmd {
            addr: self.addr,
            data: self.data,
            wait: self.sync,
        }
    }
}

#[derive(Clone, Copy)]
struct Domain {
    loaded: bool,
    addr: u32,
    levels: Levels,
    corner: u32,
    active_corner: u32,
    enabled: bool,
    state_synced: bool,
    /// genpd: `genpd->performance_state`.
    performance_state: u32,
    /// genpd: the performance state the modem's device asked of this domain (`gpd_data->performance_state`).
    dev_state: u32,
    /// genpd: this domain's link to its parent (`link->performance_state`).
    link_state: u32,
}

const EMPTY: Domain = Domain {
    loaded: false,
    addr: 0,
    levels: Levels {
        level: [0; MAX_LEVELS],
        count: 0,
        enable_corner: 0,
    },
    corner: 0,
    active_corner: 0,
    enabled: false,
    state_synced: false,
    performance_state: 0,
    dev_state: 0,
    link_state: 0,
};

/// The SC7180 RPMh power domains as `rpmhpd.c` keeps them. Nothing here talks to hardware: every operation returns
/// the [`Send`]s that Linux would make.
pub struct Pds {
    dom: [Domain; COUNT],
}

impl Default for Pds {
    fn default() -> Self {
        Self::new()
    }
}

impl Pds {
    pub fn new() -> Self {
        Pds {
            dom: [EMPTY; COUNT],
        }
    }

    /// The `rpmhpd_probe` part for the given domains and their peers: address from the command DB, the ARC type
    /// check, and the level table. The `_ao` peer of a domain shares its resource and so gets the same address and
    /// levels. Domains not named stay unloaded and cannot be used.
    pub fn load<R: RegIo>(&mut self, db: &CmdDb<R>, which: &[usize]) -> Result<(), ArcError> {
        for &idx in which {
            let desc = SC7180.get(idx).ok_or(ArcError::NotLoaded(idx))?;
            let entry = db
                .find(desc.res_name)
                .filter(|e| e.addr != 0)
                .ok_or(ArcError::Missing(desc.res_name))?;
            let hw = entry.slave_id();
            if hw != HW_ARC {
                return Err(ArcError::NotArc {
                    res: desc.res_name,
                    hw,
                });
            }
            let mut buf = [0u8; MAX_LEVELS * 2];
            let n = db.aux(entry, &mut buf);
            let len = entry.data_len as usize;
            let levels = parse_levels(&buf[..n], len, desc.skip_retention_level)?;
            self.install(idx, entry.addr, levels);
            if let Some(peer) = desc.peer {
                self.install(peer, entry.addr, levels);
            }
        }
        Ok(())
    }

    /// Put a domain's address and levels in place without a command DB.
    pub fn install(&mut self, idx: usize, addr: u32, levels: Levels) {
        let dom = &mut self.dom[idx];
        dom.loaded = true;
        dom.addr = addr;
        dom.levels = levels;
    }

    /// Set the `state_synced` flag of every domain (`rpmhpd_sync_state` does that, then re-aggregates; this does not).
    pub fn set_state_synced(&mut self, synced: bool) {
        for dom in &mut self.dom {
            dom.state_synced = synced;
        }
    }

    pub fn levels(&self, idx: usize) -> Option<&Levels> {
        self.dom.get(idx).filter(|d| d.loaded).map(|d| &d.levels)
    }

    pub fn addr(&self, idx: usize) -> Option<u32> {
        self.dom.get(idx).filter(|d| d.loaded).map(|d| d.addr)
    }

    /// Current `corner` and `active_corner` of a domain.
    pub fn corners(&self, idx: usize) -> (u32, u32) {
        (self.dom[idx].corner, self.dom[idx].active_corner)
    }

    pub fn is_enabled(&self, idx: usize) -> bool {
        self.dom[idx].enabled
    }

    /// `to_active_sleep`.
    fn to_active_sleep(idx: usize, corner: u32) -> (u32, u32) {
        to_active_sleep(idx, corner)
    }

    /// `rpmhpd_aggregate_corner`: send the aggregated corner as ACTIVE_ONLY and, for a domain with a peer, also as
    /// WAKE_ONLY and SLEEP.
    pub fn aggregate_corner(&mut self, idx: usize, corner: u32, out: &mut Vec<Send>) {
        let peer = SC7180[idx].peer;
        let pd = self.dom[idx];

        let (this_active, this_sleep) = if pd.state_synced {
            Self::to_active_sleep(idx, corner)
        } else {
            // Clamp to highest corner if sync_state hasn't happened.
            let top = (pd.levels.count as u32).saturating_sub(1);
            (top, top)
        };

        let (mut peer_active, mut peer_sleep) = (0, 0);
        if let Some(p) = peer
            && self.dom[p].enabled
        {
            let peer_enabled_corner = self.dom[p]
                .corner
                .max(self.dom[p].levels.enable_corner as u32);
            (peer_active, peer_sleep) = Self::to_active_sleep(p, peer_enabled_corner);
        }

        let active = this_active.max(peer_active);
        out.push(Send {
            domain: idx,
            state: State::Active,
            addr: pd.addr,
            data: active,
            // Wait for an ack only when we are increasing the perf state of the power domain.
            sync: active > pd.active_corner,
        });
        self.dom[idx].active_corner = active;

        if let Some(p) = peer {
            self.dom[p].active_corner = active;
            out.push(Send {
                domain: idx,
                state: State::Wake,
                addr: pd.addr,
                data: active,
                sync: false,
            });
            let sleep = this_sleep.max(peer_sleep);
            out.push(Send {
                domain: idx,
                state: State::Sleep,
                addr: pd.addr,
                data: sleep,
                sync: false,
            });
        }
    }

    /// `rpmhpd_power_on`.
    pub fn power_on(&mut self, idx: usize, out: &mut Vec<Send>) {
        let corner = self.dom[idx]
            .corner
            .max(self.dom[idx].levels.enable_corner as u32);
        self.aggregate_corner(idx, corner, out);
        self.dom[idx].enabled = true;
    }

    /// `rpmhpd_power_off`.
    pub fn power_off(&mut self, idx: usize, out: &mut Vec<Send>) {
        self.aggregate_corner(idx, 0, out);
        self.dom[idx].enabled = false;
    }

    /// `rpmhpd_set_performance_state`: map `level` (a `vlvl`) to the first index whose level is not below it, the top
    /// index if there is none, raise it to `enable_corner` when the domain is on, and aggregate when it is on.
    pub fn set_performance_state(&mut self, idx: usize, level: u32, out: &mut Vec<Send>) {
        let pd = self.dom[idx];
        let count = pd.levels.count;
        let mut i = (0..count)
            .find(|&i| level <= pd.levels.level[i])
            .unwrap_or(count);
        // If the level requested is more than that supported by the max corner, just set it to max anyway.
        if i == count {
            i = count.saturating_sub(1);
        }
        let mut corner = i as u32;
        if pd.enabled {
            // Ensure that the domain isn't turn off
            corner = corner.max(pd.levels.enable_corner as u32);
            self.aggregate_corner(idx, corner, out);
        }
        self.dom[idx].corner = corner;
    }

    /// genpd `_genpd_reeval_performance_state`.
    fn reeval(&self, idx: usize, state: u32) -> u32 {
        let cur = self.dom[idx].performance_state;
        if state >= cur {
            return state;
        }
        let mut s = state.max(self.dom[idx].dev_state);
        for (c, desc) in SC7180.iter().enumerate() {
            if desc.parent == Some(idx) {
                s = s.max(self.dom[c].link_state);
            }
        }
        s
    }

    /// genpd `_genpd_set_parent_state`: translate (identity here), record in the link, re-evaluate the parent.
    fn set_parent_state(&mut self, idx: usize, parent: usize, state: u32, out: &mut Vec<Send>) {
        self.dom[idx].link_state = state;
        let parent_state = self.reeval(parent, state);
        self.genpd_set_performance_state(parent, parent_state, out);
    }

    /// genpd `_genpd_set_performance_state`: scaling up goes to the parents first, scaling down to the parents last.
    fn genpd_set_performance_state(&mut self, idx: usize, state: u32, out: &mut Vec<Send>) {
        let cur = self.dom[idx].performance_state;
        if state == cur {
            return;
        }
        let parent = SC7180[idx].parent;
        if state > cur
            && let Some(p) = parent
        {
            self.set_parent_state(idx, p, state, out);
        }
        self.set_performance_state(idx, state, out);
        if state < cur
            && let Some(p) = parent
        {
            self.set_parent_state(idx, p, state, out);
        }
        self.dom[idx].performance_state = state;
    }

    /// genpd `genpd_set_performance_state` for the modem's device in domain `idx`.
    fn dev_set_performance_state(&mut self, idx: usize, state: u32, out: &mut Vec<Send>) {
        if self.dom[idx].dev_state == state {
            return;
        }
        self.dom[idx].dev_state = state;
        let state = self.reeval(idx, state);
        self.genpd_set_performance_state(idx, state, out);
    }

    /// genpd `genpd_power_on`: parents first.
    fn genpd_power_on(&mut self, idx: usize, out: &mut Vec<Send>) {
        if self.dom[idx].enabled {
            return;
        }
        if let Some(p) = SC7180[idx].parent {
            self.genpd_power_on(p, out);
        }
        self.power_on(idx, out);
    }

    /// genpd `genpd_power_off`: a domain with a subdomain still on stays on; otherwise it goes off, then its parent.
    fn genpd_power_off(&mut self, idx: usize, out: &mut Vec<Send>) {
        if !self.dom[idx].enabled {
            return;
        }
        let child_on = SC7180
            .iter()
            .enumerate()
            .any(|(c, desc)| desc.parent == Some(idx) && self.dom[c].enabled);
        if child_on {
            return;
        }
        self.power_off(idx, out);
        if let Some(p) = SC7180[idx].parent {
            self.genpd_power_off(p, out);
        }
    }

    /// `qcom_pas_pds_disable` for the given proxy domains, in order: `dev_pm_genpd_set_performance_state(pd, 0)`, then
    /// `pm_runtime_put`, which powers the domain off and then its parent. Every send Linux makes, in order.
    ///
    /// Before `sync_state` Linux's `rpmhpd_aggregate_corner` clamps every vote to the top corner, so this sends the top
    /// corner again until then; `rpmhpd_sync_state` later aggregates once more and the domains reach 0. This kernel never
    /// runs `sync_state`: call [`Pds::set_state_synced`] first to get the state Linux ends in.
    pub fn proxy_unvote(&mut self, proxies: &[usize]) -> Vec<Send> {
        let mut out = Vec::new();
        for &idx in proxies {
            if !self.dom.get(idx).is_some_and(|d| d.loaded) {
                continue;
            }
            self.dev_set_performance_state(idx, 0, &mut out);
            self.genpd_power_off(idx, &mut out);
        }
        out
    }

    /// `qcom_pas_pds_enable` for the given proxy domains, in order; every send Linux makes, in order.
    pub fn proxy_vote_max(&mut self, proxies: &[usize]) -> Result<Vec<Send>, ArcError> {
        for &idx in proxies {
            if !self.dom.get(idx).is_some_and(|d| d.loaded) {
                return Err(ArcError::NotLoaded(idx));
            }
            // A parent that is not loaded would be voted with address 0.
            if let Some(p) = SC7180[idx].parent
                && !self.dom[p].loaded
            {
                return Err(ArcError::NotLoaded(p));
            }
        }
        let mut out = Vec::new();
        for &idx in proxies {
            // dev_pm_genpd_set_performance_state(pd, INT_MAX): the device is runtime-suspended, so the state is only
            // remembered and applied by genpd_runtime_resume, before the power-on.
            self.dev_set_performance_state(idx, INT_MAX, &mut out);
            self.genpd_power_on(idx, &mut out);
        }
        Ok(out)
    }

    /// Only the ACTIVE_ONLY sends, as commands (what an active-only driver sends of `sends`).
    pub fn active_cmds(sends: &[Send]) -> Vec<TcsCmd> {
        sends
            .iter()
            .filter(|s| s.state == State::Active)
            .map(Send::cmd)
            .collect()
    }
}
