//! The complete vote lists: what a modem start needs from RPMh, and what turning on the WCN3990's supplies needs.
//!
//! Each list is plain data, `(name, command)` in the order to send, built from a command DB. Nothing is sent by
//! building one. [`execute`] sends a list through an [`Rsc`]; the kernel decides when.
//!
//! **Not verified on hardware.** The addresses and the ARC levels come from the command DB passed in. On the real
//! machine they are unknown until the kernel prints them (`lucina.wifi=report`); the tests use an invented image in
//! the right format. [`modem_plan`] is active-only; [`modem_votes`] also keeps the sleep and wake sets.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lucina_qcom_soc::cmd_db::CmdDb;
use lucina_qcom_soc::io::{Delay, RegIo};

use crate::arc::{self, ArcError, Pds, sc7180};
use crate::rpmh::Cache;
use crate::rsc::{Rsc, RscError, TcsCmd};
use crate::vrm::{self, VrmError};
use crate::xo::{self, XoError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanError {
    Arc(ArcError),
    Vrm(VrmError),
    Xo(XoError),
}

impl From<ArcError> for PlanError {
    fn from(e: ArcError) -> Self {
        PlanError::Arc(e)
    }
}
impl From<VrmError> for PlanError {
    fn from(e: VrmError) -> Self {
        PlanError::Vrm(e)
    }
}
impl From<XoError> for PlanError {
    fn from(e: XoError) -> Self {
        PlanError::Xo(e)
    }
}

impl core::fmt::Display for PlanError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PlanError::Arc(e) => e.fmt(f),
            PlanError::Vrm(e) => e.fmt(f),
            PlanError::Xo(e) => e.fmt(f),
        }
    }
}

/// A named vote.
pub type Plan = Vec<(String, TcsCmd)>;

/// What `qcom_pas_start` asks of RPMh for the modem (`qcom,sc7180-mpss-pas`) before the firmware boots: the proxy
/// power domains `cx` and `mss` at their top level, with `mx` as `cx`'s parent, then the `xo` clock.
///
/// In order: `mx.lvl` (parent of `cx`, voted first because genpd powers parents first), `cx.lvl`, `mss.lvl`, `xo.lvl`.
/// The ARC data is the top level index of each domain's table, from the command DB.
pub fn modem_plan<R: RegIo>(db: &CmdDb<R>) -> Result<Plan, PlanError> {
    let mut pds = Pds::new();
    pds.load(db, &[sc7180::CX, sc7180::MX, sc7180::MSS])?;
    let sends = pds.proxy_vote_max(&[sc7180::CX, sc7180::MSS])?;
    let mut plan = Plan::new();
    for send in sends.iter().filter(|s| s.state == arc::State::Active) {
        let name = format!("{} active", arc::SC7180[send.domain].res_name);
        plan.push((name, send.cmd()));
    }
    plan.push((format!("{} active", xo::RES_NAME), xo::active_vote(db)?));
    Ok(plan)
}

/// The video block's `cx` (`required-opps` of its operating points) and, as genpd powers parents first, `mx`. Linux votes the level of
/// the clock step; with no aggregation of `cx` across drivers here, a lower vote would undo the modem's proxy vote on the same DRV, so
/// this is the top level, as the modem's. Active set only.
pub fn venus_plan<R: RegIo>(db: &CmdDb<R>) -> Result<Plan, PlanError> {
    let mut pds = Pds::new();
    pds.load(db, &[sc7180::CX, sc7180::MX])?;
    let sends = pds.proxy_vote_max(&[sc7180::CX])?;
    let mut plan = Plan::new();
    for send in sends.iter().filter(|s| s.state == arc::State::Active) {
        let name = format!("{} active", arc::SC7180[send.domain].res_name);
        plan.push((name, send.cmd()));
    }
    Ok(plan)
}

/// What bringing up the WCN3990's five supplies (`regulator_bulk_enable` of `ath10k_regulators[]`) sends: for each
/// supply, in table order, the mode, the voltage and the enable (see [`crate::vrm`] for why that order). Every command waits for
/// its answer.
pub fn wcn3990_plan<R: RegIo>(db: &CmdDb<R>) -> Result<Plan, PlanError> {
    let mut plan = Plan::new();
    for spec in &vrm::WCN3990_LDOS {
        let name = vrm::resource_name(spec.pmic_id, spec.index);
        for send in vrm::ldo_votes(db, spec)? {
            let what = match send.kind {
                vrm::Kind::Mode => "mode",
                vrm::Kind::Voltage => "voltage",
                vrm::Kind::Enable => "enable",
            };
            plan.push((format!("{name} {what}"), send.cmd));
        }
    }
    Ok(plan)
}

/// Send a plan in order, stopping at the first failure. Returns the index of the failed step with the error.
pub fn execute<R: RegIo>(
    rsc: &mut Rsc<R>,
    plan: &[(String, TcsCmd)],
    delay: &impl Delay,
    timeout_ms: u32,
) -> Result<(), (usize, RscError)> {
    for (i, (_, cmd)) in plan.iter().enumerate() {
        rsc.send_cmd(cmd, delay, timeout_ms).map_err(|e| (i, e))?;
    }
    Ok(())
}

/// A modem start split as `qcom_pas_start` does it, with the cache of everything Linux's votes record.
pub struct ModemVotes {
    /// The proxy power domains at their top level, active set, in order.
    pub proxies: Plan,
    /// `clk_prepare_enable(xo)`, active set.
    pub xo: (String, TcsCmd),
    /// What `rpmh_write` and `rpmh_write_async` cached for sleep and wake.
    pub cache: Cache,
    /// `qcom_pas_pds_disable` of the same proxies, active set, in order: for a failed start (and, once votes are
    /// aggregated across drivers, the `handover`). Computed with `state_synced`, the state Linux ends in.
    pub release: Plan,
    /// `clk_disable_unprepare(xo)`, active set.
    pub xo_off: (String, TcsCmd),
    /// [`ModemVotes::cache`] after the release and the XO off: what is left for sleep and wake.
    pub release_cache: Cache,
}

/// [`modem_plan`] with the sleep and wake values kept.
pub fn modem_votes<R: RegIo>(db: &CmdDb<R>) -> Result<ModemVotes, PlanError> {
    let mut pds = Pds::new();
    pds.load(db, &[sc7180::CX, sc7180::MX, sc7180::MSS])?;
    let sends = pds.proxy_vote_max(&[sc7180::CX, sc7180::MSS])?;
    let mut proxies = Plan::new();
    let mut cache = Cache::new();
    for send in &sends {
        cache.record(send.state, send.addr, send.data);
        if send.state == arc::State::Active {
            proxies.push((
                format!("{} active", arc::SC7180[send.domain].res_name),
                send.cmd(),
            ));
        }
    }
    // clk_rpmh_send_aggregate_command: the "on" value for each state, sleep first.
    let xo_cmd = xo::active_vote(db)?;
    for state in [arc::State::Sleep, arc::State::Wake, arc::State::Active] {
        cache.record(state, xo_cmd.addr, xo_cmd.data);
    }

    // The way back, from the same domain state.
    let mut release_cache = cache.clone();
    let xo_off = xo::off_vote(db)?;
    for state in [arc::State::Sleep, arc::State::Wake, arc::State::Active] {
        release_cache.record(state, xo_off.addr, xo_off.data);
    }
    pds.set_state_synced(true);
    let mut release = Plan::new();
    for send in pds.proxy_unvote(&[sc7180::CX, sc7180::MSS]) {
        release_cache.record(send.state, send.addr, send.data);
        if send.state == arc::State::Active {
            release.push((
                format!("{} release", arc::SC7180[send.domain].res_name),
                send.cmd(),
            ));
        }
    }
    Ok(ModemVotes {
        proxies,
        xo: (format!("{} active", xo::RES_NAME), xo_cmd),
        cache,
        release,
        xo_off: (format!("{} off", xo::RES_NAME), xo_off),
        release_cache,
    })
}

/// What `qcom_pas_start` asks of RPMh for the audio DSP (`qcom,sc7180-adsp-pas`, `sm8250_adsp_resource`): the proxy power domains
/// `lcx` and `lmx` at their top level, which have no parents here, then the `xo` clock. Active set only.
pub fn adsp_plan<R: RegIo>(db: &CmdDb<R>) -> Result<Plan, PlanError> {
    let mut pds = Pds::new();
    pds.load(db, &[sc7180::LCX, sc7180::LMX])?;
    let sends = pds.proxy_vote_max(&[sc7180::LCX, sc7180::LMX])?;
    let mut plan = Plan::new();
    for send in sends.iter().filter(|s| s.state == arc::State::Active) {
        let name = format!("{} active", arc::SC7180[send.domain].res_name);
        plan.push((name, send.cmd()));
    }
    plan.push((format!("{} active", xo::RES_NAME), xo::active_vote(db)?));
    Ok(plan)
}
