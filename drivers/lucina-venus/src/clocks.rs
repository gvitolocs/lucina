// SPDX-License-Identifier: GPL-2.0-only
//! The video block's clocks and power switches: what Linux's `gcc-sc7180.c` (its two always-on video branches), `videocc-sc7180.c`,
//! `clk-alpha-pll.c` (Fabia), `clk-rcg2.c` (the shared RCG), `clk-branch.c` and `gdsc.c` write, in the order `venus/pm_helpers.c`
//! (v4) asks for them. Offsets are from the start of each block: GCC at 0x100000, videocc at 0xab00000.
//!
//! Linux never sets the core clock's rate before the firmware boots on this chip (AR50 that is not "lite": `core_clks_enable`
//! skips it), so the RCG runs at what it was parked at; the first session's load sets the rate ([`set_core_rate`]).

use lucina_qcom_soc::io::RegIo;

use crate::{Hal, Result, err, poll};

/// GCC: `GCC_VIDEO_AHB_CLK` and `GCC_VIDEO_XO_CLK`, which `gcc_sc7180_probe` turns on for good (`qcom_branch_set_clk_en`, no poll).
pub const GCC_VIDEO_AHB: u32 = 0x0_b004;
pub const GCC_VIDEO_XO: u32 = 0x0_b028;
/// `GCC_VIDEO_AXI_CLK`: no device tree node names it, so Linux leaves it to the hardware. Read for the report only.
pub const GCC_VIDEO_AXI: u32 = 0x0_b01c;

/// videocc, byte offsets.
pub mod cc {
    /// `video_pll0`, a Fabia PLL at 0x42c.
    pub const PLL: u32 = 0x42c;
    pub const PLL_MODE: u32 = PLL;
    pub const PLL_L_VAL: u32 = PLL + 0x04;
    pub const PLL_USER_CTL: u32 = PLL + 0x0c;
    pub const PLL_USER_CTL_U: u32 = PLL + 0x10;
    pub const PLL_FRAC: u32 = PLL + 0x38;
    pub const PLL_OPMODE: u32 = PLL + 0x2c;
    /// `video_cc_venus_clk_src`: CMD, then CFG.
    pub const RCG_CMD: u32 = 0x7f0;
    pub const RCG_CFG: u32 = 0x7f4;
    pub const VENUS_GDSCR: u32 = 0x814;
    pub const VCODEC0_GDSCR: u32 = 0x874;
    /// `core`: `video_cc_venus_ctl_core_clk`.
    pub const CTL_CORE_CBCR: u32 = 0x850;
    /// `vcodec0_core`: `video_cc_vcodec0_core_clk`, `BRANCH_HALT_VOTED`.
    pub const VCODEC0_CORE_CBCR: u32 = 0x890;
    pub const XO_CBCR: u32 = 0x984;
    /// `bus`: `video_cc_venus_ctl_axi_clk`.
    pub const CTL_AXI_CBCR: u32 = 0x9cc;
    /// `vcodec0_bus`: `video_cc_vcodec0_axi_clk`.
    pub const VCODEC0_AXI_CBCR: u32 = 0x9ec;
    /// `iface`: `video_cc_venus_ahb_clk`.
    pub const AHB_CBCR: u32 = 0xa4c;
}

const CBCR_ENABLE: u32 = 1 << 0;
const CBCR_CLK_OFF: u32 = 1 << 31;
/// `NOC_FSM_STATUS` field of a CBCR and its "on" value.
const CBCR_NOC_FSM_SHIFT: u32 = 28;
const CBCR_NOC_FSM_ON: u32 = 2;

const PLL_OUTCTRL: u32 = 1 << 0;
const PLL_RESET_N: u32 = 1 << 2;
const PLL_UPDATE: u32 = 1 << 22;
const PLL_UPDATE_BYPASS: u32 = 1 << 23;
const PLL_ACK_LATCH: u32 = 1 << 29;
const PLL_LOCK_DET: u32 = 1 << 31;
const PLL_OUT_MASK: u32 = 0x7;
const OPMODE_STANDBY: u32 = 0;
const OPMODE_RUN: u32 = 1;

const RCG_UPDATE: u32 = 1 << 0;
const RCG_ROOT_EN: u32 = 1 << 1;
const RCG_ROOT_OFF: u32 = 1 << 31;
/// CFG bits `__clk_rcg2_configure` replaces: the divider, the source, the M/N mode and HW_CLK_CTRL.
const RCG_CFG_MASK: u32 = 0x1f | (7 << 8) | (3 << 12) | (1 << 20);

const GDSC_PWR_ON: u32 = 1 << 31;
const GDSC_SW_COLLAPSE: u32 = 1 << 0;
const GDSC_HW_CONTROL: u32 = 1 << 1;
/// `gdsc_init`: HW_CONTROL and SW_OVERRIDE cleared, and the three wait fields at their defaults: en_rest 2, en_few 8, clk_dis 2.
const GDSC_INIT_MASK: u32 = 0x00ff_f006;
const GDSC_INIT_VALUE: u32 = (2 << 20) | (8 << 16) | (2 << 12);
/// `STATUS_POLL_TIMEOUT_US`.
const GDSC_TIMEOUT_US: u64 = 2000;

fn set<R: RegIo>(io: &R, off: u32, clear: u32, set: u32) {
    io.write32(off, (io.read32(off) & !clear) | set);
}

/// What the firmware left, for the report: read only, and videocc only if its AHB branch runs (a read behind a clock that is off
/// hangs the bus).
#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    pub gcc_ahb: u32,
    pub gcc_xo: u32,
    pub gcc_axi: u32,
    pub pll_mode: u32,
    pub pll_l: u32,
    pub pll_opmode: u32,
    pub rcg_cmd: u32,
    pub rcg_cfg: u32,
    pub venus_gdscr: u32,
    pub vcodec0_gdscr: u32,
    pub core: u32,
    pub iface: u32,
    pub bus: u32,
    pub vcodec0_core: u32,
    pub vcodec0_bus: u32,
    pub xo: u32,
}

/// Whether a branch whose CBCR reads `v` runs (`clk_branch2_check_halt`).
pub fn branch_on(v: u32) -> bool {
    v & CBCR_CLK_OFF == 0 || (v >> CBCR_NOC_FSM_SHIFT) & 7 == CBCR_NOC_FSM_ON
}

pub fn gcc_ahb_on<G: RegIo>(gcc: &G) -> bool {
    let v = gcc.read32(GCC_VIDEO_AHB);
    v & CBCR_ENABLE != 0 && v & CBCR_CLK_OFF == 0
}

pub fn read<G: RegIo, C: RegIo>(gcc: &G, cc: &C) -> State {
    let mut s =
        State { gcc_ahb: gcc.read32(GCC_VIDEO_AHB), gcc_xo: gcc.read32(GCC_VIDEO_XO), gcc_axi: gcc.read32(GCC_VIDEO_AXI), ..State::default() };
    if !gcc_ahb_on(gcc) {
        return s;
    }
    s.pll_mode = cc.read32(cc::PLL_MODE);
    s.pll_l = cc.read32(cc::PLL_L_VAL);
    s.pll_opmode = cc.read32(cc::PLL_OPMODE);
    s.rcg_cmd = cc.read32(cc::RCG_CMD);
    s.rcg_cfg = cc.read32(cc::RCG_CFG);
    s.venus_gdscr = cc.read32(cc::VENUS_GDSCR);
    s.vcodec0_gdscr = cc.read32(cc::VCODEC0_GDSCR);
    s.core = cc.read32(cc::CTL_CORE_CBCR);
    s.iface = cc.read32(cc::AHB_CBCR);
    s.bus = cc.read32(cc::CTL_AXI_CBCR);
    s.vcodec0_core = cc.read32(cc::VCODEC0_CORE_CBCR);
    s.vcodec0_bus = cc.read32(cc::VCODEC0_AXI_CBCR);
    s.xo = cc.read32(cc::XO_CBCR);
    s
}

/// `gcc_sc7180_probe`'s two video branches, before anything in videocc is touched.
pub fn enable_gcc<G: RegIo, H: Hal>(gcc: &G, hal: &H) {
    set(gcc, GCC_VIDEO_AHB, 0, CBCR_ENABLE);
    set(gcc, GCC_VIDEO_XO, 0, CBCR_ENABLE);
    hal.delay_us(1);
}

/// `video_cc_sc7180_probe`: the PLL's configuration (`clk_fabia_pll_configure` writes only the nonzero values: L, alpha and the two
/// user controls, then UPDATE_BYPASS and RESET_N), the XO branch always on, `gdsc_init` for both switches and `clk_rcg2_shared_init`
/// parking the RCG on the crystal. Returns the RCG's CFG as the firmware left it, Linux's `parked_cfg`.
pub fn probe<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<u32> {
    cc.write32(cc::PLL_L_VAL, 0x1f);
    cc.write32(cc::PLL_FRAC, 0x4000);
    cc.write32(cc::PLL_USER_CTL, 0x0000_0001);
    cc.write32(cc::PLL_USER_CTL_U, 0x0000_4805);
    set(cc, cc::PLL_MODE, 0, PLL_UPDATE_BYPASS);
    set(cc, cc::PLL_MODE, 0, PLL_RESET_N);
    set(cc, cc::XO_CBCR, 0, CBCR_ENABLE);
    for gdscr in [cc::VENUS_GDSCR, cc::VCODEC0_GDSCR] {
        set(cc, gdscr, GDSC_INIT_MASK, GDSC_INIT_VALUE);
    }
    let parked = cc.read32(cc::RCG_CFG);
    rcg_write_cfg(cc, hal, 0)?;
    Ok(parked)
}

/// The shared RCG's way of changing CFG: root on, CFG, update, root off (`clk_rcg2_shared_enable` and `_disable`).
fn rcg_write_cfg<C: RegIo, H: Hal>(cc: &C, hal: &H, cfg: u32) -> Result<()> {
    set(cc, cc::RCG_CMD, 0, RCG_ROOT_EN);
    poll(hal, "the video RCG's root does not come on", 500, 1, || cc.read32(cc::RCG_CMD), |v| v & RCG_ROOT_OFF == 0)?;
    cc.write32(cc::RCG_CFG, cfg);
    set(cc, cc::RCG_CMD, 0, RCG_UPDATE);
    poll(hal, "the video RCG does not take its configuration", 500, 1, || cc.read32(cc::RCG_CMD), |v| v & RCG_UPDATE == 0)?;
    set(cc, cc::RCG_CMD, RCG_ROOT_EN, 0);
    Ok(())
}

/// `gdsc_enable` (no reset, no clamp, no retention here): clear SW_COLLAPSE, wait for PWR_ON, 1 us.
pub fn gdsc_on<C: RegIo, H: Hal>(cc: &C, hal: &H, gdscr: u32) -> Result<()> {
    set(cc, gdscr, GDSC_SW_COLLAPSE, 0);
    poll(hal, "a video power switch does not come on", GDSC_TIMEOUT_US, 1, || cc.read32(gdscr), |v| v & GDSC_PWR_ON != 0)?;
    hal.delay_us(1);
    Ok(())
}

/// `gdsc_disable`: set SW_COLLAPSE, wait for PWR_ON to clear.
pub fn gdsc_off<C: RegIo, H: Hal>(cc: &C, hal: &H, gdscr: u32) -> Result<()> {
    set(cc, gdscr, 0, GDSC_SW_COLLAPSE);
    poll(hal, "a video power switch does not go off", GDSC_TIMEOUT_US, 1, || cc.read32(gdscr), |v| v & GDSC_PWR_ON == 0)?;
    Ok(())
}

/// `gdsc_set_hwmode`: the hardware switches vcodec0 itself (`true`), or software again (`false`, then wait for PWR_ON, as the hardware
/// may have collapsed it meanwhile).
pub fn gdsc_hw_mode<C: RegIo, H: Hal>(cc: &C, hal: &H, gdscr: u32, hw: bool) -> Result<()> {
    if hw {
        set(cc, gdscr, 0, GDSC_HW_CONTROL);
        hal.delay_us(1);
        return Ok(());
    }
    set(cc, gdscr, GDSC_HW_CONTROL, 0);
    hal.delay_us(1);
    poll(hal, "vcodec0 does not come back from hardware control", GDSC_TIMEOUT_US, 1, || cc.read32(gdscr), |v| v & GDSC_PWR_ON != 0)?;
    Ok(())
}

/// `clk_branch2_enable`: set the enable bit, wait up to 200 us for the clock to run.
pub fn branch_on_wait<C: RegIo, H: Hal>(cc: &C, hal: &H, cbcr: u32) -> Result<()> {
    set(cc, cbcr, 0, CBCR_ENABLE);
    poll(hal, "a video clock does not start", 200, 1, || cc.read32(cbcr), branch_on)?;
    Ok(())
}

/// `clk_branch2_disable`: clear the enable bit; wait for it to stop unless the branch is `BRANCH_HALT_VOTED` (then 10 us).
pub fn branch_off<C: RegIo, H: Hal>(cc: &C, hal: &H, cbcr: u32, voted: bool) -> Result<()> {
    set(cc, cbcr, CBCR_ENABLE, 0);
    if voted {
        hal.delay_us(10);
        return Ok(());
    }
    poll(hal, "a video clock does not stop", 200, 1, || cc.read32(cbcr), |v| v & CBCR_CLK_OFF != 0)?;
    Ok(())
}

/// `alpha_pll_fabia_enable`, the manual path (the PLL's FSM is not used): out of standby, run, lock, outputs on.
pub fn pll_on<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    let mode = cc.read32(cc::PLL_MODE);
    if cc.read32(cc::PLL_OPMODE) & OPMODE_RUN != 0 && mode & PLL_OUTCTRL != 0 {
        return Ok(());
    }
    set(cc, cc::PLL_MODE, PLL_OUTCTRL, 0);
    cc.write32(cc::PLL_OPMODE, OPMODE_STANDBY);
    set(cc, cc::PLL_MODE, 0, PLL_RESET_N);
    cc.write32(cc::PLL_OPMODE, OPMODE_RUN);
    poll(hal, "the video PLL does not lock", 1500, 1, || cc.read32(cc::PLL_MODE), |v| v & PLL_LOCK_DET != 0)?;
    set(cc, cc::PLL_USER_CTL, 0, PLL_OUT_MASK);
    set(cc, cc::PLL_MODE, 0, PLL_OUTCTRL);
    Ok(())
}

/// `alpha_pll_fabia_disable`.
pub fn pll_off<C: RegIo>(cc: &C) {
    set(cc, cc::PLL_MODE, PLL_OUTCTRL, 0);
    set(cc, cc::PLL_USER_CTL, PLL_OUT_MASK, 0);
    cc.write32(cc::PLL_OPMODE, OPMODE_STANDBY);
}

/// One step of the core clock: the device tree's operating points with the freq table of `videocc-sc7180.c` behind them, the PLL's L
/// and alpha that `alpha_pll_round_rate` picks for the PLL rate, and the CX level the operating point requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub hz: u32,
    pub pll_l: u32,
    pub pll_alpha: u32,
    /// CFG: source 1 (the PLL) and the divider field (`2 * div - 1`).
    pub cfg: u32,
    /// `opp-level` of the `required-opps`: LOW_SVS 64, SVS 128, SVS_L1 192, NOM 256, TURBO 384.
    pub cx_level: u32,
}

pub const STEPS: [Step; 5] = [
    Step { hz: 150_000_000, pll_l: 0x1f, pll_alpha: 0x4000, cfg: 0x107, cx_level: 64 },
    Step { hz: 270_000_000, pll_l: 0x23, pll_alpha: 0x2800, cfg: 0x104, cx_level: 128 },
    Step { hz: 340_000_000, pll_l: 0x23, pll_alpha: 0x6aab, cfg: 0x103, cx_level: 192 },
    Step { hz: 434_000_000, pll_l: 0x2d, pll_alpha: 0x3556, cfg: 0x103, cx_level: 256 },
    Step { hz: 500_000_097, pll_l: 0x34, pll_alpha: 0x1556, cfg: 0x103, cx_level: 384 },
];

/// The lowest step at or above `hz` (`dev_pm_opp_find_freq_ceil`), the top one above all.
pub fn step_for(hz: u64) -> Step {
    *STEPS.iter().find(|s| u64::from(s.hz) >= hz).unwrap_or(&STEPS[STEPS.len() - 1])
}

/// `alpha_pll_fabia_set_rate` then the RCG's set_rate (`__clk_rcg2_shared_set_rate`) with the RCG running: new L and alpha, latch,
/// then the RCG's source and divider. The caller raises CX first when going up and lowers it after when going down, as the OPP core
/// does.
pub fn set_core_rate<C: RegIo, H: Hal>(cc: &C, hal: &H, step: Step) -> Result<()> {
    cc.write32(cc::PLL_L_VAL, step.pll_l);
    cc.write32(cc::PLL_FRAC, step.pll_alpha);
    // __clk_alpha_pll_update_latch with UPDATE_BYPASS set: request, wait for the ack, release, wait for it to drop.
    set(cc, cc::PLL_MODE, 0, PLL_UPDATE);
    hal.delay_us(1);
    poll(hal, "the video PLL does not latch its rate", 1500, 1, || cc.read32(cc::PLL_MODE), |v| v & PLL_ACK_LATCH != 0)?;
    set(cc, cc::PLL_MODE, PLL_UPDATE, 0);
    poll(hal, "the video PLL's latch does not drop", 1500, 1, || cc.read32(cc::PLL_MODE), |v| v & PLL_ACK_LATCH == 0)?;
    hal.delay_us(10);
    // The RCG's new parent runs before the switch (`__clk_set_parent_before`): with the RCG parked on the crystal the PLL is off,
    // and an RCG update to a source that does not run never completes.
    pll_on(cc, hal)?;
    let cfg = (cc.read32(cc::RCG_CFG) & !RCG_CFG_MASK) | step.cfg;
    rcg_write_cfg(cc, hal, cfg)
}

/// `core_power_v4(POWER_ON)` after the votes: the venus switch, then `core` (with the PLL and the RCG under it, the RCG back at
/// `parked`), `iface` and `bus`.
pub fn venus_on<C: RegIo, H: Hal>(cc: &C, hal: &H, parked: u32) -> Result<()> {
    gdsc_on(cc, hal, cc::VENUS_GDSCR)?;
    // The clock framework enables the current parent first: the PLL only when the parked CFG takes the RCG from it.
    if (parked >> 8) & 7 == 1 {
        pll_on(cc, hal)?;
    }
    rcg_write_cfg(cc, hal, parked)?;
    branch_on_wait(cc, hal, cc::CTL_CORE_CBCR)?;
    branch_on_wait(cc, hal, cc::AHB_CBCR)?;
    branch_on_wait(cc, hal, cc::CTL_AXI_CBCR)?;
    Ok(())
}

/// `core_power_v4(POWER_OFF)`: `bus`, `iface`, `core`, the RCG parked on the crystal (its CFG returned for the next start), the PLL off,
/// then the venus switch.
pub fn venus_off<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<u32> {
    branch_off(cc, hal, cc::CTL_AXI_CBCR, false)?;
    branch_off(cc, hal, cc::AHB_CBCR, false)?;
    branch_off(cc, hal, cc::CTL_CORE_CBCR, false)?;
    let parked = cc.read32(cc::RCG_CFG);
    rcg_write_cfg(cc, hal, 0)?;
    pll_off(cc);
    gdsc_off(cc, hal, cc::VENUS_GDSCR)?;
    Ok(parked)
}

/// `poweron_coreid`: vcodec0's switch on, in software control, its two clocks, then hardware control.
pub fn vcodec0_on<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    // Left in hardware control by a session that did not end (or a kernel before this one): software control first, as
    // `gdsc_init` would have it, so that the switch obeys SW_COLLAPSE.
    if cc.read32(cc::VCODEC0_GDSCR) & GDSC_HW_CONTROL != 0 {
        set(cc, cc::VCODEC0_GDSCR, GDSC_HW_CONTROL, 0);
        hal.delay_us(1);
    }
    gdsc_on(cc, hal, cc::VCODEC0_GDSCR)?;
    gdsc_hw_mode(cc, hal, cc::VCODEC0_GDSCR, false)?;
    branch_on_wait(cc, hal, cc::VCODEC0_CORE_CBCR)?;
    branch_on_wait(cc, hal, cc::VCODEC0_AXI_CBCR)?;
    gdsc_hw_mode(cc, hal, cc::VCODEC0_GDSCR, true)
}

/// `poweroff_coreid`: software control again, the clocks off in reverse, the switch off.
pub fn vcodec0_off<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    gdsc_hw_mode(cc, hal, cc::VCODEC0_GDSCR, false)?;
    branch_off(cc, hal, cc::VCODEC0_AXI_CBCR, false)?;
    branch_off(cc, hal, cc::VCODEC0_CORE_CBCR, true)?;
    gdsc_off(cc, hal, cc::VCODEC0_GDSCR)
}

/// The core clock's step from the RCG's CFG, for the report: `None` on the crystal or anything not in the table.
pub fn step_of(cfg: u32, pll_l: u32) -> Option<Step> {
    if (cfg >> 8) & 7 != 1 {
        return None;
    }
    STEPS.iter().copied().find(|s| s.cfg == cfg & 0x71f && s.pll_l == pll_l)
}

/// Refuses a clock state this driver does not know how to leave: the RCG on another source than the crystal or the PLL.
pub fn check_parked(parked: u32) -> Result<()> {
    match (parked >> 8) & 7 {
        0 | 1 => Ok(()),
        _ => err("the video RCG was left on a source this driver does not know", parked),
    }
}
