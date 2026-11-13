//! RPMh votes for a modem cold start on the Qualcomm SC7180 (Samsung Galaxy Book Go): the modem DSP, which hosts the
//! WCN3990 Wi-Fi firmware, needs power domain, clock and regulator votes sent to the RPMh hardware before it boots.
//! This crate turns "vote on this" into exact register writes of the RSC (Resource State Coordinator), following
//! Linux 7.1. It does no I/O of its own: registers go through [`lucina_qcom_soc::io::RegIo`] and waits through
//! [`lucina_qcom_soc::io::Delay`], and resource addresses come from the command DB
//! ([`lucina_qcom_soc::cmd_db::CmdDb`]).
//!
//! * [`rsc`]: the DRV's TCS registers, layout 2.7. [`rsc::Rsc::probe`] is `rpmh_rsc_probe`/`rpmh_probe_tcs_config`;
//!   [`rsc::Rsc::send`] is `rpmh_rsc_send_data` plus the body of `tcs_tx_done`, polled instead of interrupt driven.
//!   [`rsc::Rsc::write_ctrl`] writes a sleep or wake command without a trigger.
//! * [`rpmh`]: the sleep and wake cache and flush of `rpmh.c`.
//! * [`arc`]: power domains (`rpmhpd.c`): the level table in the command DB, level index votes, aggregation with a
//!   peer, the parent `mx` of `cx`, and the order of a proxy vote at the top level (`qcom_pas_pds_enable`).
//! * [`vrm`]: LDOs (`qcom-rpmh-regulator.c`): hardware data, voltage selectors, mode map, and the order
//!   mode / voltage / enable.
//! * [`xo`]: the `bi_tcxo` ARC clock vote (`clk-rpmh.c`).
//! * [`bcm`]: bus clock votes for an interconnect path (`icc-rpmh.c`, `bcm-voter.c`), never lower than one already in a TCS.
//! * [`icc`]: every consumer's interconnect request on the DRV, summed per BCM as `bcm-voter.c` does, with the boot firmware's
//!   level as a floor client of its own until the interconnect's `sync_state`.
//! * [`share`]: one table of every ARC and XO vote on the apps DRV, aggregated across drivers as Linux's genpd, `rpmhpd.c` and
//!   the clock framework do; its rules are [`vote_rules`], which Verus checks.
//! * [`plan`]: [`plan::modem_plan`] and [`plan::wcn3990_plan`], the complete lists as `(name, command)`, and
//!   [`plan::modem_votes`] with the sleep and wake cache.
//!
//! The addresses and levels a plan uses are the command DB's; on the real machine they are unknown until the kernel
//! reads and prints them. Nothing in this crate has run on hardware, and the tests run against a software model of
//! the RSC (`tests/common/sim.rs`) and the output of Linux's own C compiled on the host (`oracle/`).
//!
//! The derivation of each file (Linux files, licences) is in its own header. The crate is a derivative of GPL-2.0
//! code.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod arc;
pub mod bcm;
pub mod icc;
pub mod plan;
pub mod rpmh;
pub mod rsc;
pub mod share;
pub mod tree;
pub mod vote_rules;
pub mod vrm;
pub mod xo;

pub use rsc::{Quirks, Rsc, RscConfig, RscError, TcsCmd, TcsRequest};
