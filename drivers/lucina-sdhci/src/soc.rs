//! The SC7180's clocks and pins for SDCC2, the SD slot.
//!
//! From Linux `gcc-sc7180.c` (`gcc_sdcc2_ahb_clk`, `gcc_sdcc2_apps_clk`, `gcc_sdcc2_apps_clk_src`, `gpll0`),
//! `pinctrl-sc7180.c` (`sdc2_*`, GPIO 69 in the west tile) and the laptop's device tree (`sdc2-on`, `cd-gpios`). A window
//! here is one page of MMIO; every read tells what the firmware left and changes nothing.

use crate::{Clock, Error, Regs};

/// From the GCC's base: the page with the SDCC2 clocks.
pub const GCC_SDCC2_PAGE: usize = 0x14000;
const APPS_CBCR: usize = 0x004;
const AHB_CBCR: usize = 0x008;
const RCG_CMD: usize = 0x00c;
const RCG_CFG: usize = 0x010;
const CBCR_ENABLE: u32 = 1 << 0;
/// Linux's `CBCR_CLK_OFF`: set, the branch reads stopped (`clk_branch2_check_halt`, `clk-branch.c:44-59`).
const CBCR_OFF: u32 = 1 << 31;
/// `clk_branch_wait`'s `count = 200` reads of a branch that has not answered (`:80`), and the gap between them is
/// the one thing this crate cannot keep: Linux puts a `udelay(1)` there, and a disable has no clock to wait with.
pub const HALT_CHECKS: usize = 200;
const RCG_UPDATE: u32 = 1 << 0;
/// GPLL0_OUT_EVEN (300 MHz, parent 6) divided by 3: half-integer divider field 2 * 3 - 1.
const RCG_CFG_100MHZ: u32 = (6 << 8) | 5;
/// The rate the RCG is set to; the SDHCI divider makes the card clock from it. So the base clock is known, which is what
/// `SDHCI_QUIRK_CAP_CLOCK_BASE_BROKEN` asks for.
pub const CORE_CLOCK_HZ: u32 = 100_000_000;

/// From the GCC's base: GPLL0's mode register; the first page holds nothing else the driver uses.
pub const GCC_GPLL0_PAGE: usize = 0x0;
const GPLL0_MODE: usize = 0x0;
const PLL_LOCK_DET: u32 = 1 << 31;

/// From the south tile's base: SDC2's drive and pull register.
pub const TLMM_SDC2_PAGE: usize = 0x7b000;
const SDC2_HDRV_PULL: usize = 0x0;
/// `gpio_n`'s page from its tile's base.
pub const fn tlmm_gpio_page(n: usize) -> usize {
    n * 0x1000
}
const GPIO_CTL: usize = 0x0;
const GPIO_IO: usize = 0x4;
const GPIO_IN: u32 = 1 << 0;

/// What the firmware left running for SDCC2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clocks {
    pub ahb: u32,
    pub apps: u32,
    pub rcg_cfg: u32,
}

impl Clocks {
    /// Reads the clock registers, which live in the GCC and are safe whatever SDCC2's state.
    pub fn read(gcc: &impl Regs) -> Self {
        Self {
            ahb: gcc.read32(AHB_CBCR),
            apps: gcc.read32(APPS_CBCR),
            rcg_cfg: gcc.read32(RCG_CFG),
        }
    }

    /// The controller's registers are only safe to touch with both branches running: on this SoC an access to a
    /// block with its clock stopped can stall the bus or reset the machine.
    pub fn running(&self) -> bool {
        self.ahb & CBCR_OFF == 0 && self.apps & CBCR_OFF == 0
    }
}

/// Sets the core clock to [`CORE_CLOCK_HZ`] and enables both branches (Linux: `clk_set_rate`, then `clk_prepare_enable`).
///
/// The loop is these two rows' halt check: `BRANCH_HALT` is one of the types `clk_branch_wait` polls on the way up
/// (`drivers/clk/qcom/clk-branch.c:77-79`, whatever the vote), and what that poll looks for is `CLK_OFF` clear
/// (`clk_branch2_check_halt`, `:54-57`) - [`Clocks::running`], the same bit [`disable_clocks`] waits for the other
/// way. Linux's enabling test also accepts the NOC state machine field (`:57`); this waits for the bit alone, which
/// is stricter, so a branch that only the FSM claims is not taken for one that runs.
pub fn enable_clocks(gcc: &impl Regs, gpll0: &impl Regs, clock: &impl Clock) -> Result<(), Error> {
    if gpll0.read32(GPLL0_MODE) & PLL_LOCK_DET == 0 {
        return Err(Error::Unusable("GPLL0 is not running"));
    }
    gcc.write32(RCG_CFG, RCG_CFG_100MHZ);
    gcc.write32(RCG_CMD, gcc.read32(RCG_CMD) | RCG_UPDATE);
    gcc.write32(AHB_CBCR, gcc.read32(AHB_CBCR) | CBCR_ENABLE);
    gcc.write32(APPS_CBCR, gcc.read32(APPS_CBCR) | CBCR_ENABLE);
    for _ in 0..1000 {
        if Clocks::read(gcc).running() && gcc.read32(RCG_CMD) & RCG_UPDATE == 0 {
            return Ok(());
        }
        clock.delay_us(10);
    }
    Err(Error::Timeout("the SDCC2 clocks"))
}

/// The two halves of a sleep that touch the GCC (patch 1356).
///
/// Linux's list is the bulk array, `core` at index 0 (`drivers/mmc/host/sdhci-msm.c:2473-2479`) and `iface` at index
/// 1 (`:2464-2470`); `cal` and `sleep` are optional and sc7180's device tree names neither
/// (`arch/arm64/boot/dts/qcom/sc7180.dtsi:2815-2818`, three clocks: iface, core, xo), and the `"bus"` voter
/// (`sdhci-msm.c:2452-2461`) and `"xo"` (`:2520-2526`) are not in the bulk list and stay on - which is why these two
/// functions know only the two branches.
///
/// Both branches are `BRANCH_HALT` and not voted: `.halt_check = BRANCH_HALT` at `gcc-sc7180.c:1767`
/// (`gcc_sdcc2_ahb_clk`, `halt_reg` 0x14008 at `:1766`) and `:1780` (`gcc_sdcc2_apps_clk`, `halt_reg` 0x14004 at
/// `:1779`), each with its `enable_reg` on the same register and `enable_mask` `BIT(0)` (`:1769-1770`,
/// `:1782-1783`). `BRANCH_HALT` is one of the types `clk_branch_wait` *polls*, in both directions
/// (`drivers/clk/qcom/clk-branch.c:77-79`) - the 10 µs delay of `:75-76` is only for `BRANCH_HALT_DELAY` and for a
/// voted clock going down - so a disable writes the bit and then looks for `CLK_OFF`, which 1360 had missed and said
/// waits for nothing. The halt test on the way down is `CLK_OFF` set and no FSM clause (`:59`).
pub fn disable_clocks(gcc: &impl Regs) -> Clocks {
    // `clk_bulk_disable` walks the array backwards (`drivers/clk/clk-bulk.c:211-216`), so `iface` first, and each
    // `clk_disable` waits for its own branch before the next one is touched.
    gcc.write32(AHB_CBCR, gcc.read32(AHB_CBCR) & !CBCR_ENABLE);
    wait_for_halt(gcc, AHB_CBCR);
    gcc.write32(APPS_CBCR, gcc.read32(APPS_CBCR) & !CBCR_ENABLE);
    wait_for_halt(gcc, APPS_CBCR);
    Clocks::read(gcc)
}

/// `clk_branch_wait`'s poll for a branch on its way down (`clk-branch.c:80-88`): [`HALT_CHECKS`] reads of the CBCR,
/// looking for `CLK_OFF` - [`CBCR_OFF`] here, and the test of `clk_branch2_check_halt` for the disable direction
/// (`:59`), which is that bit set and no FSM clause. Linux puts a `udelay(1)` between the reads; this crate has no
/// clock to wait with, so the reads run back to back, which still bounds the loop and still stops before the next
/// write reaches the GCC. A branch that never answers is what Linux answers with `-EBUSY` (`:87-89`), and
/// `clk_branch2_disable` throws that away (`:133-136`), so nothing here keeps it either: [`disable_clocks`] hands
/// the caller the CBCR as it reads afterwards, and the hook says what it finds there. Two branches run out is 400
/// reads back to back, which is inside the bound a suspend hook has to return within, whatever the hardware says.
fn wait_for_halt(gcc: &impl Regs, cbcr: usize) {
    for _ in 0..HALT_CHECKS {
        if gcc.read32(cbcr) & CBCR_OFF != 0 {
            return;
        }
    }
}

/// The way back: `clk_bulk_prepare_enable` in the array's own order (`clk-bulk.c:87-90` and
/// `include/linux/clk.h:1162-1172`), which is `core` before `iface` - the opposite of what this driver's bring-up
/// writes (`enable_clocks` below, `soc.rs:82-83`) - and then the rate set again, which is what Linux's
/// `dev_pm_opp_set_rate(dev, msm_host->clk_rate)` does for the core clock (`sdhci-msm.c:2733`). The RCG is left
/// where bring-up put it, as in [`enable_clocks`], because only the two branches were gated.
pub fn restore_clocks(gcc: &impl Regs, gpll0: &impl Regs, clock: &impl Clock) -> Result<(), Error> {
    if gpll0.read32(GPLL0_MODE) & PLL_LOCK_DET == 0 {
        return Err(Error::Unusable("GPLL0 is not running"));
    }
    gcc.write32(APPS_CBCR, gcc.read32(APPS_CBCR) | CBCR_ENABLE);
    gcc.write32(AHB_CBCR, gcc.read32(AHB_CBCR) | CBCR_ENABLE);
    gcc.write32(RCG_CFG, RCG_CFG_100MHZ);
    gcc.write32(RCG_CMD, gcc.read32(RCG_CMD) | RCG_UPDATE);
    for _ in 0..1000 {
        if Clocks::read(gcc).running() && gcc.read32(RCG_CMD) & RCG_UPDATE == 0 {
            return Ok(());
        }
        clock.delay_us(10);
    }
    Err(Error::Timeout("the SDCC2 clocks"))
}

/// `sdc2-on` from the device tree: clock no pull, 16 mA; command and data pull-up, 10 mA.
pub fn configure_pins(sdc2: &impl Regs) {
    const PULL_UP: u32 = 3;
    const fn drive(ma: u32) -> u32 {
        ma / 2 - 1
    }
    let mask = (3 << 14) | (7 << 6) | (3 << 11) | (7 << 3) | (3 << 9) | 7;
    let value = (drive(16) << 6) | (PULL_UP << 11) | (drive(10) << 3) | (PULL_UP << 9) | drive(10);
    sdc2.write32(
        SDC2_HDRV_PULL,
        (sdc2.read32(SDC2_HDRV_PULL) & !mask) | value,
    );
}

/// `pinconf-sd-cd`: a GPIO input with a pull-up, 2 mA.
pub fn configure_card_detect(gpio: &impl Regs) {
    const PULL_UP: u32 = 3;
    gpio.write32(GPIO_CTL, (gpio.read32(GPIO_CTL) & !0x3ff) | PULL_UP);
}

/// What the card-detect pin says now: its control register, its level, and whether that means a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardDetect {
    pub ctl: u32,
    pub level: bool,
    pub present: bool,
}

impl CardDetect {
    pub fn read(gpio: &impl Regs, active_low: bool) -> Self {
        let level = gpio.read32(GPIO_IO) & GPIO_IN != 0;
        Self {
            ctl: gpio.read32(GPIO_CTL),
            level,
            present: level != active_low,
        }
    }
}
