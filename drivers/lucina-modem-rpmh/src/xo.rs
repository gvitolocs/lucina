//! The XO (crystal oscillator) clock vote, `bi_tcxo` of the RPMh clock controller.
//!
//! Derived from Linux 7.1 `drivers/clk/qcom/clk-rpmh.c` (GPL-2.0; Copyright The Linux Foundation) and
//! `include/dt-bindings/clock/qcom,rpmh.h`. No Scarlet source covers it.
//!
//! The modem's `clocks = <&rpmhcc RPMH_CXO_CLK>` (index 0) is, on SC7180, `clk_rpmh_bi_tcxo_div2`:
//! `DEFINE_CLK_RPMH_ARC(bi_tcxo, "xo.lvl", 0x3, 2)`. It is an ARC resource, so the enable register is at offset 0
//! of `xo.lvl` ([`ARC_EN_OFFSET`], `CLK_RPMH_ARC_EN_OFFSET`) and the "on" value is 3 ([`ON_VALUE`]): a level
//! **index**, like the power domain corners, not a frequency. The pas driver calls `clk_prepare_enable(pas->xo)`
//! right after the proxy power domains (`qcom_pas_start`), which reaches `clk_rpmh_prepare` ->
//! `clk_rpmh_aggregate_state_send_command(c, true)` -> `clk_rpmh_send_aggregate_command`.
//!
//! # Sleep and wake sets
//!
//! The non-`_ao` clock is valid in all three states, and `clk_rpmh_send_aggregate_command` loops over the states
//! from SLEEP to ACTIVE_ONLY, sending the "on" value for every state whose bit changed: first SLEEP and then WAKE_ONLY,
//! both through `rpmh_write_async`, and last ACTIVE_ONLY through `rpmh_write` (`wait = cmd_state && state ==
//! ACTIVE_ONLY`). [`active_vote`] is that last ACTIVE_ONLY vote; the SLEEP and WAKE values go through
//! [`crate::rpmh::Cache`], which `plan::modem_votes` fills and the kernel flushes into the sleep and wake TCSes.

use lucina_qcom_soc::cmd_db::CmdDb;
use lucina_qcom_soc::io::RegIo;

use crate::rsc::TcsCmd;

/// `bi_tcxo`'s `res_name`.
pub const RES_NAME: &str = "xo.lvl";
/// `CLK_RPMH_ARC_EN_OFFSET`.
pub const ARC_EN_OFFSET: u32 = 0;
/// `res_on_val` of `DEFINE_CLK_RPMH_ARC(bi_tcxo, "xo.lvl", 0x3, 2)`.
pub const ON_VALUE: u32 = 0x3;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum XoError {
    /// The command DB has no `xo.lvl`, or gives it address 0 (`missing RPMh resource address`).
    Missing,
}

impl core::fmt::Display for XoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("command DB has no address for xo.lvl")
    }
}

/// `clk_rpmh_unprepare` of the only user: `clk_rpmh_send_aggregate_command` sends 0 for each state whose bit changed,
/// SLEEP and WAKE_ONLY through `rpmh_write_async` and ACTIVE_ONLY through `rpmh_write` without waiting (`wait` is
/// `cmd_state && ACTIVE_ONLY`, and `cmd_state` is 0). This is that ACTIVE_ONLY command.
pub fn off_vote<R: RegIo>(db: &CmdDb<R>) -> Result<TcsCmd, XoError> {
    let on = active_vote(db)?;
    Ok(TcsCmd {
        addr: on.addr,
        data: 0,
        wait: false,
    })
}

/// The ACTIVE_ONLY enable vote for `bi_tcxo_div2`, waiting for the answer as the driver does.
pub fn active_vote<R: RegIo>(db: &CmdDb<R>) -> Result<TcsCmd, XoError> {
    let entry = db
        .find(RES_NAME)
        .filter(|e| e.addr != 0)
        .ok_or(XoError::Missing)?;
    Ok(TcsCmd {
        addr: entry.addr + ARC_EN_OFFSET,
        data: ON_VALUE,
        wait: true,
    })
}
