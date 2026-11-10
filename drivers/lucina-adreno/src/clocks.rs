// SPDX-License-Identifier: GPL-2.0-only
//! The clocks and the CX power switch the GMU needs: what Linux's `gcc-sc7180.c`, `gpucc-sc7180.c` and `gdsc.c` do when
//! the GMU's clocks and power domain are enabled, in that order.
//!
//! `gpu_cc_gx_gdsc` is not here: on this SoC the GMU switches GX, and Linux's own `power_on` for it does nothing.

use lucina_qcom_soc::io::RegIo;

use crate::{Hal, Result, poll};

// GCC, byte offsets.
pub const GCC_GPU_MISC: u32 = 0x7_1028;
pub const GCC_GPU_CFG_AHB: u32 = 0x7_1004;
pub const GCC_GPU_MEMNOC_GFX: u32 = 0x7_100c;
pub const GCC_GPU_SNOC_DVM_GFX: u32 = 0x7_1018;
pub const GCC_DDRSS_GPU_AXI: u32 = 0x4_452c;
pub const GCC_GPLL_VOTE: u32 = 0x5_2000;
/// The GPLL0 vote itself, as Linux's clock tree holds for the GPU's parents.
const GPLL0: u32 = 1;
const GPLL0_FOR_GPU: u32 = 1 << 15;
const GPLL0_DIV_FOR_GPU: u32 = 1 << 16;

// GPU_CC, byte offsets from its base.
pub const CX_GDSCR: u32 = 0x106c;
pub const GX_GDSCR: u32 = 0x100c;
pub const GDS_HW_CTRL: u32 = 0x1540;
pub const GX_CLAMP_IO: u32 = 0x1508;
pub const CX_GMU_CBCR: u32 = 0x1098;
pub const CXO_CBCR: u32 = 0x109c;
pub const GMU_RCG_CMD: u32 = 0x1120;
pub const PLL1: u32 = 0x100;

const CBCR_ENABLE: u32 = 1;
const CBCR_OFF: u32 = 1 << 31;
/// The RCG's CMD register: `CMD_UPDATE`, `CMD_ROOT_EN`, `CMD_ROOT_OFF` of `clk-rcg2.c`.
const CMD_UPDATE: u32 = 1;
const CMD_ROOT_EN: u32 = 1 << 1;
const CMD_ROOT_OFF: u32 = 1 << 31;
const PWR_ON: u32 = 1 << 31;
const SW_COLLAPSE: u32 = 1;
const GDSC_CONFIG_MASK: u32 = 0x00ff_f006;
/// `en_rest_wait` 2, `en_few_wait` 8 (the defaults of `gdsc.c`) and `clk_dis_wait` 8 (`cx_gdsc`).
const GDSC_CONFIG: u32 = (2 << 20) | (8 << 16) | (8 << 12);

/// What the firmware left, read without touching anything but GCC.
#[derive(Clone, Copy, Debug)]
pub struct GccState {
    pub cfg_ahb: u32,
    pub memnoc: u32,
    pub axi: u32,
    pub snoc_dvm: u32,
    pub gpll_vote: u32,
}

pub fn read_gcc<G: RegIo>(gcc: &G) -> GccState {
    GccState {
        cfg_ahb: gcc.read32(GCC_GPU_CFG_AHB),
        memnoc: gcc.read32(GCC_GPU_MEMNOC_GFX),
        axi: gcc.read32(GCC_DDRSS_GPU_AXI),
        snoc_dvm: gcc.read32(GCC_GPU_SNOC_DVM_GFX),
        gpll_vote: gcc.read32(GCC_GPLL_VOTE),
    }
}

impl GccState {
    /// GPU_CC and the GMU answer only with this clock running: a read without it hangs the bus. Linux turns it on in
    /// `gcc_sc7180_probe` and never off.
    pub fn ahb_on(&self) -> bool {
        self.cfg_ahb & CBCR_ENABLE != 0 && self.cfg_ahb & CBCR_OFF == 0
    }
}

fn set<R: RegIo>(io: &R, off: u32, clear: u32, set: u32) {
    io.write32(off, (io.read32(off) & !clear) | set);
}

/// Turns on the GCC side the GPU clock controller needs first: the config bus (always on in Linux's `gcc-sc7180` probe), and
/// GPLL0's vote and its branches to GPU_CC. The way to memory is [`enable_gpu_bus`], later.
pub fn enable_gcc<G: RegIo, H: Hal>(gcc: &G, hal: &H) -> Result<()> {
    set(gcc, GCC_GPU_CFG_AHB, 0, CBCR_ENABLE);
    // "Disable the GPLL0 active input to MM blocks, NPU and GPU via MISC registers."
    set(gcc, GCC_GPU_MISC, 0, 3);
    set(gcc, GCC_GPLL_VOTE, 0, GPLL0 | GPLL0_FOR_GPU | GPLL0_DIV_FOR_GPU);
    hal.delay_us(10);
    hal.barrier();
    poll(
        hal,
        "gcc_gpu_cfg_ahb_clk stays off",
        1000,
        1,
        || gcc.read32(GCC_GPU_CFG_AHB),
        |v| v & CBCR_OFF == 0,
    )?;
    Ok(())
}

/// The GPU's way to memory, `axi` and `memnoc` of the GMU node's clocks, each with `clk_branch_wait`'s halt check. Linux
/// enables them in `a6xx_gmu_resume`'s `clk_bulk_prepare_enable`, after the CX switch is on (`pm_runtime_resume_and_get`)
/// and after the `gmu` and `cxo` clocks, in the device tree's order: call it after [`enable_cx`] and
/// [`enable_gmu_clocks`].
pub fn enable_gpu_bus<G: RegIo, H: Hal>(gcc: &G, hal: &H) -> Result<()> {
    set(gcc, GCC_DDRSS_GPU_AXI, 0, CBCR_ENABLE);
    set(gcc, GCC_GPU_MEMNOC_GFX, 0, CBCR_ENABLE);
    hal.barrier();
    poll(
        hal,
        "gcc_ddrss_gpu_axi_clk stays off",
        200,
        1,
        || gcc.read32(GCC_DDRSS_GPU_AXI),
        |v| v & CBCR_OFF == 0,
    )?;
    poll(
        hal,
        "gcc_gpu_memnoc_gfx_clk stays off",
        200,
        1,
        || gcc.read32(GCC_GPU_MEMNOC_GFX),
        |v| v & CBCR_OFF == 0,
    )?;
    Ok(())
}

/// Turns on the CX power switch, as `gdsc_init` and `gdsc_enable` do.
pub fn enable_cx<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    set(cc, CX_GDSCR, GDSC_CONFIG_MASK, GDSC_CONFIG);
    set(cc, CX_GDSCR, SW_COLLAPSE, 0);
    hal.barrier();
    hal.delay_us(1);
    poll(
        hal,
        "CX GDSC does not come up",
        2000,
        1,
        || cc.read32(GDS_HW_CTRL),
        |v| v & PWR_ON != 0,
    )?;
    // `gdsc_enable` waits 1 us after the power domain is up, so firmware cannot read a stale status.
    hal.delay_us(1);
    Ok(())
}

/// `clk_fabia_pll_configure(&gpu_cc_pll1, ..)` for 360 MHz, as `gpu_cc_sc7180_probe`. Linux's clock tree keeps this PLL out
/// of the GMU's 200 MHz, and so does this driver; it is here because the GMU's firmware may use it.
fn configure_pll1<C: RegIo, H: Hal>(cc: &C, hal: &H) {
    const MODE: u32 = 0x00;
    const L_VAL: u32 = 0x04;
    const USER_CTL: u32 = 0x0c;
    const USER_CTL_U: u32 = 0x10;
    const CONFIG_CTL: u32 = 0x14;
    const CONFIG_CTL_U: u32 = 0x18;
    const TEST_CTL_U: u32 = 0x20;
    const FRAC: u32 = 0x38;
    const UPDATE_BYPASS: u32 = 1 << 23;
    const RESET_N: u32 = 1 << 2;
    cc.write32(PLL1 + L_VAL, 0x12);
    cc.write32(PLL1 + FRAC, 0xc000);
    cc.write32(PLL1 + CONFIG_CTL, 0x2048_5699);
    cc.write32(PLL1 + CONFIG_CTL_U, 0x0000_2067);
    cc.write32(PLL1 + USER_CTL, 0x0000_0001);
    cc.write32(PLL1 + USER_CTL_U, 0x0000_4805);
    cc.write32(PLL1 + TEST_CTL_U, 0x4000_0000);
    hal.barrier();
    set(cc, PLL1 + MODE, 0, UPDATE_BYPASS);
    set(cc, PLL1 + MODE, 0, RESET_N);
    hal.barrier();
}

/// The GMU's clock at 200 MHz from GPLL0's divided branch, and the crystal's branch. Runs with CX on. With GCC's AXI and memory NoC
/// branches these are the four `clocks` of the GMU's device tree node: nothing else in GPU_CC or GCC gets asked for, and the SMMU's
/// DVM branch (`gcc_gpu_snoc_dvm_gfx_clk`), which Scarlet turns on, did not even start on the laptop with CX off.
pub fn enable_gmu_clocks<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    configure_pll1(cc, hal);
    enable_gmu_clocks_again(cc, hal)
}

/// [`enable_gmu_clocks`] after a suspend: Linux configures PLL1 once, at the clock controller's probe, and a resume only
/// sets the GMU clock's rate and enables the branches (`a6xx_gmu_resume`: `clk_set_rate(gmu->core_clk, 200000000)`, then
/// `clk_bulk_prepare_enable`).
pub fn enable_gmu_clocks_again<C: RegIo, H: Hal>(cc: &C, hal: &H) -> Result<()> {
    // `clk_rcg2_shared_force_enable_clear`: force the root on, so the source switch below cannot leave the RCG parked.
    set(cc, GMU_RCG_CMD, 0, CMD_ROOT_EN);
    poll(
        hal,
        "the GMU clock's RCG does not turn on",
        500,
        1,
        || cc.read32(GMU_RCG_CMD),
        |v| v & CMD_ROOT_OFF == 0,
    )?;
    // `F(200000000, P_GPLL0_OUT_MAIN_DIV, 1.5, 0, 0)`: source 6, half-integer divider 2 * 1.5 - 1.
    cc.write32(GMU_RCG_CMD + 4, (6 << 8) | 2);
    set(cc, GMU_RCG_CMD, 0, CMD_UPDATE);
    poll(
        hal,
        "the GMU clock's RCG does not update",
        500,
        1,
        || cc.read32(GMU_RCG_CMD),
        |v| v & CMD_UPDATE == 0,
    )?;
    set(cc, GMU_RCG_CMD, CMD_ROOT_EN, 0);
    // "Recommended WAKEUP/SLEEP settings for the gpu_cc_cx_gmu_clk".
    set(cc, CX_GMU_CBCR, 0xff0, 0xff0);
    set(cc, CXO_CBCR, 0, CBCR_ENABLE);
    set(cc, CX_GMU_CBCR, 0, CBCR_ENABLE);
    hal.barrier();
    poll(
        hal,
        "gpu_cc_cxo_clk stays off",
        1000,
        1,
        || cc.read32(CXO_CBCR),
        |v| v & CBCR_OFF == 0,
    )?;
    poll(
        hal,
        "gpu_cc_cx_gmu_clk stays off",
        1000,
        1,
        || cc.read32(CX_GMU_CBCR),
        |v| v & CBCR_OFF == 0,
    )?;
    Ok(())
}

/// `clk_branch_wait` on a disable of a `BRANCH_HALT` branch: the halt bit within 200 us. Linux only warns when it stays on
/// (another master, here the GMU's RSC, may hold the clock), and so does this.
fn branch_off<R: RegIo, H: Hal>(io: &R, hal: &H, off: u32, what: &'static str) {
    set(io, off, CBCR_ENABLE, 0);
    hal.barrier();
    if let Err(e) = poll(hal, what, 200, 1, || io.read32(off), |v| v & CBCR_OFF != 0) {
        hal.log(format_args!("gpu: clocks: {}", e));
    }
}

/// `clk_bulk_disable_unprepare` of the GMU's clocks in `a6xx_gmu_stop`, the array's reverse order: memnoc and axi
/// (`BRANCH_VOTED`: no wait on a disable, `udelay(10)`), cxo and gmu (`BRANCH_HALT`), then what the clock tree drops with the
/// last user of the GMU clock: `clk_rcg2_shared_disable` parks the RCG on its safe source, the crystal (source 0), with the
/// root forced on around the switch, and the GCC branches feeding it from GPLL0 go off (`BRANCH_HALT_DELAY`). GPLL0's own
/// vote and `gcc_gpu_cfg_ahb_clk` stay: other drivers share the one and Linux never turns the other off.
pub fn disable_gmu_clocks<G: RegIo, C: RegIo, H: Hal>(gcc: &G, cc: &C, hal: &H) -> Result<()> {
    set(gcc, GCC_GPU_MEMNOC_GFX, CBCR_ENABLE, 0);
    hal.delay_us(10);
    set(gcc, GCC_DDRSS_GPU_AXI, CBCR_ENABLE, 0);
    hal.delay_us(10);
    branch_off(cc, hal, CXO_CBCR, "gpu_cc_cxo_clk stays on");
    branch_off(cc, hal, CX_GMU_CBCR, "gpu_cc_cx_gmu_clk stays on");
    set(cc, GMU_RCG_CMD, 0, CMD_ROOT_EN);
    cc.write32(GMU_RCG_CMD + 4, 0);
    set(cc, GMU_RCG_CMD, 0, CMD_UPDATE);
    let parked = poll(hal, "the GMU clock's RCG does not park", 500, 1, || cc.read32(GMU_RCG_CMD), |v| v & CMD_UPDATE == 0);
    set(cc, GMU_RCG_CMD, CMD_ROOT_EN, 0);
    parked?;
    set(gcc, GCC_GPLL_VOTE, GPLL0_FOR_GPU | GPLL0_DIV_FOR_GPU, 0);
    hal.delay_us(10);
    hal.barrier();
    Ok(())
}

/// `gdsc_disable` of `gx_gdsc` (`pm_runtime_put_sync(gmu->gxpd)` in `a6xx_gmu_stop`): "Make sure the GX domain is off before
/// turning off the GMU (CX) domain. Usually the GMU does this but only if the shutdown sequence was successful." The switch's
/// collapse bit, its status within 500 us, then the IO clamp (`CLAMP_IO`).
pub fn disable_gx<C: RegIo, H: Hal>(cc: &C, hal: &H) {
    set(cc, GX_GDSCR, 0, SW_COLLAPSE);
    hal.barrier();
    if let Err(e) = poll(hal, "GX GDSC stays on", 500, 1, || cc.read32(GX_GDSCR), |v| v & PWR_ON == 0) {
        hal.log(format_args!("gpu: clocks: {}", e));
    }
    set(cc, GX_CLAMP_IO, 0, 1);
    hal.barrier();
}

/// `gdsc_disable` of the votable `cx_gdsc`, as genpd turns it off when the GMU and the SMMU are suspended: this CPU's vote
/// withdrawn, no status poll (another master may hold it on), and the 500 us that keep an enable right after it sane.
pub fn disable_cx<C: RegIo, H: Hal>(cc: &C, hal: &H) {
    set(cc, CX_GDSCR, 0, SW_COLLAPSE);
    hal.barrier();
    hal.delay_us(500);
}

/// The same with `dev_pm_genpd_synced_poweroff`, as `a6xx_recover` asks before it starts a hung GMU again: the switch's status
/// polled off for `STATUS_POLL_TIMEOUT_US` (2 ms, after the 1 us the GDS controller needs). Whether CX really went off: Linux
/// reports "cx gdsc didn't collapse" and goes on.
pub fn collapse_cx<C: RegIo, H: Hal>(cc: &C, hal: &H) -> bool {
    set(cc, CX_GDSCR, 0, SW_COLLAPSE);
    hal.barrier();
    hal.delay_us(1);
    poll(hal, "CX GDSC does not collapse", 2000, 1, || cc.read32(GDS_HW_CTRL), |v| v & PWR_ON == 0).is_ok()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::cell::{Cell, RefCell};
    use std::vec::Vec as StdVec;

    use super::*;

    /// Records every write; reads return what was written with the "off"/"update" bits clear, so polls succeed. An
    /// unwritten register reads as powered on.
    struct Trace(RefCell<StdVec<(u32, u32)>>);

    impl RegIo for Trace {
        fn read32(&self, off: u32) -> u32 {
            self.0
                .borrow()
                .iter()
                .rev()
                .find(|(o, _)| *o == off)
                .map(|(_, v)| v & !(CBCR_OFF | CMD_UPDATE))
                .unwrap_or(PWR_ON)
        }
        fn write32(&self, off: u32, value: u32) {
            self.0.borrow_mut().push((off, value));
        }
    }

    struct Hal<'a> {
        delays: &'a RefCell<StdVec<u32>>,
        now: Cell<u64>,
    }

    impl crate::Hal for Hal<'_> {
        fn now_us(&self) -> u64 {
            let t = self.now.get();
            self.now.set(t + 100);
            t
        }
        fn delay_us(&self, us: u32) {
            self.delays.borrow_mut().push(us);
        }
        fn barrier(&self) {}
        fn log(&self, _args: core::fmt::Arguments) {}
    }

    fn hal(delays: &RefCell<StdVec<u32>>) -> Hal<'_> {
        Hal {
            delays,
            now: Cell::new(0),
        }
    }

    #[test]
    fn the_bus_branches_are_enabled_and_halt_checked() {
        let gcc = Trace(RefCell::new(StdVec::new()));
        let delays = RefCell::new(StdVec::new());
        enable_gpu_bus(&gcc, &hal(&delays)).unwrap();
        let trace = gcc.0.borrow();
        let axi_at = trace.iter().position(|(o, v)| *o == GCC_DDRSS_GPU_AXI && v & CBCR_ENABLE != 0).unwrap();
        let memnoc_at = trace.iter().position(|(o, v)| *o == GCC_GPU_MEMNOC_GFX && v & CBCR_ENABLE != 0).unwrap();
        // The device tree's clock order: axi, then memnoc.
        assert!(axi_at < memnoc_at);
    }

    #[test]
    fn gcc_votes_gpll0_and_leaves_the_bus_branches_for_later() {
        let gcc = Trace(RefCell::new(StdVec::new()));
        let delays = RefCell::new(StdVec::new());
        enable_gcc(&gcc, &hal(&delays)).unwrap();
        let trace = gcc.0.borrow();
        let vote = trace
            .iter()
            .find(|(o, _)| *o == GCC_GPLL_VOTE)
            .map(|(_, v)| *v)
            .unwrap();
        assert_eq!(
            vote & (GPLL0 | GPLL0_FOR_GPU | GPLL0_DIV_FOR_GPU),
            GPLL0 | GPLL0_FOR_GPU | GPLL0_DIV_FOR_GPU
        );
        // The way to memory is not touched here: Linux enables it after the CX switch (`enable_gpu_bus`).
        assert!(!trace.iter().any(|(o, _)| *o == GCC_DDRSS_GPU_AXI || *o == GCC_GPU_MEMNOC_GFX));
        assert_eq!(delays.borrow().first(), Some(&10));
    }

    #[test]
    fn cx_waits_one_us_after_the_status_poll() {
        let cc = Trace(RefCell::new(StdVec::new()));
        let delays = RefCell::new(StdVec::new());
        enable_cx(&cc, &hal(&delays)).unwrap();
        // One us before the poll and one after it, as `gdsc_enable` does.
        assert_eq!(&delays.borrow()[..], &[1, 1]);
    }

    #[test]
    fn the_gmu_rcg_is_force_enabled_around_the_source_switch() {
        let cc = Trace(RefCell::new(StdVec::new()));
        let delays = RefCell::new(StdVec::new());
        enable_gmu_clocks(&cc, &hal(&delays)).unwrap();
        let trace = cc.0.borrow();
        let cmd: StdVec<u32> = trace
            .iter()
            .filter(|(o, _)| *o == GMU_RCG_CMD)
            .map(|(_, v)| *v)
            .collect();
        // ROOT_EN set, UPDATE set, ROOT_EN cleared, in that order.
        assert_eq!(cmd[0] & CMD_ROOT_EN, CMD_ROOT_EN);
        assert_eq!(cmd[1] & CMD_UPDATE, CMD_UPDATE);
        assert_eq!(cmd[2] & CMD_ROOT_EN, 0);
        // The CFG write sits between the ROOT_EN set and the UPDATE set.
        let root_en_at = trace
            .iter()
            .position(|(o, v)| *o == GMU_RCG_CMD && v & CMD_ROOT_EN != 0)
            .unwrap();
        let cfg_at = trace
            .iter()
            .position(|(o, _)| *o == GMU_RCG_CMD + 4)
            .unwrap();
        let update_at = trace
            .iter()
            .position(|(o, v)| *o == GMU_RCG_CMD && v & CMD_UPDATE != 0)
            .unwrap();
        assert!(root_en_at < cfg_at && cfg_at < update_at);
        assert_eq!(
            trace
                .iter()
                .find(|(o, _)| *o == GMU_RCG_CMD + 4)
                .map(|(_, v)| *v),
            Some((6 << 8) | 2)
        );
    }
}
