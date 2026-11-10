//! i.MX6SL clock gates, the EPDC clock parents and the display power domain.
//!
//! The previous kernel set the rates (dividers and muxes stay programmed in the CCM); it may have gated the
//! clocks or powered down unused PLLs and the DISPLAY domain. References: Linux `drivers/clk/imx/clk-imx6sl.c`
//! (gates, muxes), `drivers/clk/imx/clk-pllv3.c`, `clk-pfd.c`, `drivers/pmdomain/imx/gpc.c`.

use crate::arch::{timer, Mmio};

const CCM: Mmio = Mmio::at(0x020c_4000);
const ANATOP: Mmio = Mmio::at(0x020c_8000);
const GPC: Mmio = Mmio::at(0x020d_c000);

const CHSCCDR: usize = 0x34; // epdc_axi_sel [17:15]
const CSCDR2: usize = 0x38; // epdc_pix_sel [17:15]
pub const CCGR1: usize = 0x6c;
pub const CCGR2: usize = 0x70;
pub const CCGR3: usize = 0x74;
pub const CCGR5: usize = 0x7c;

/// Turn a two-bit clock gate fully on (run and wait modes).
pub fn ungate(ccgr: usize, shift: u32) {
    CCM.modify(ccgr, 0, 0b11 << shift);
}

pub fn gate(ccgr: usize, shift: u32) {
    CCM.modify(ccgr, 0b11 << shift, 0);
}

// ANATOP registers have SET/CLR aliases at +4/+8.
const PLL_SYS_528: usize = 0x30; // pll2
const PLL_VIDEO: usize = 0xa0; // pll5
const PFD_480: usize = 0xf0; // pll3 PFDs
const PFD_528: usize = 0x100; // pll2 PFDs
const PLL_POWERDOWN: u32 = 1 << 12;
const PLL_ENABLE: u32 = 1 << 13;
const PLL_BYPASS: u32 = 1 << 16;
const PLL_LOCK: u32 = 1 << 31;

fn pll_on(reg: usize) -> bool {
    let v = ANATOP.read(reg);
    if v & PLL_POWERDOWN == 0 && v & PLL_ENABLE != 0 && v & PLL_BYPASS == 0 && v & PLL_LOCK != 0 {
        return true;
    }
    ANATOP.write(reg + 8, PLL_POWERDOWN);
    let locked = timer::poll_until(10, || ANATOP.read(reg) & PLL_LOCK != 0);
    ANATOP.write(reg + 8, PLL_BYPASS);
    ANATOP.write(reg + 4, PLL_ENABLE);
    locked
}

fn pfd_ungate(reg: usize, n: u32) {
    ANATOP.write(reg + 8, 1 << (8 * n + 7));
}

/// Make sure the parent selected by an EPDC mux runs. Index meaning (both muxes):
/// 0 pll2_bus, 1 pll3_usb_otg, 2 pll5_video_div, 3 pll2_pfd0, 4/5 differ (axi: pll2_pfd2, pll3_pfd2;
/// pix: pll2_pfd1, pll3_pfd1). pll2 and pll3 clock the buses and USB and are always running.
fn parent_on(sel: u32, pix: bool) -> bool {
    match (sel, pix) {
        (2, _) => pll_on(PLL_VIDEO),
        (3, _) => {
            pfd_ungate(PFD_528, 0);
            true
        }
        (4, true) => {
            pfd_ungate(PFD_528, 1);
            true
        }
        (4, false) => {
            pfd_ungate(PFD_528, 2);
            true
        }
        (5, true) => {
            pfd_ungate(PFD_480, 1);
            true
        }
        (5, false) => {
            pfd_ungate(PFD_480, 2);
            true
        }
        _ => ANATOP.read(PLL_SYS_528) & PLL_LOCK != 0 || sel == 1,
    }
}

pub struct EpdcClocks {
    pub axi_sel: u32,
    pub pix_sel: u32,
    pub parents_ok: bool,
    pub domain_ok: bool,
}

/// Clocks and power for the EPDC: parents running, gates on, DISPLAY power domain up.
pub fn epdc_on() -> EpdcClocks {
    let axi_sel = (CCM.read(CHSCCDR) >> 15) & 7;
    let pix_sel = (CCM.read(CSCDR2) >> 15) & 7;
    let parents_ok = parent_on(axi_sel, false) & parent_on(pix_sel, true);
    ungate(CCGR3, 4); // epdc_axi
    ungate(CCGR3, 10); // epdc_pix
    // imx6_pm_domain_power_on: PGC gates the domain off when powered down, then request power-up.
    const PGC_DISP_CTRL: usize = 0x240;
    const CNTR: usize = 0x000;
    const DISP_PUP_REQ: u32 = 1 << 5;
    // The reset clocks of every block in the domain (pd_disp in imx6sl.dtsi) run during the request.
    const OTHERS: [u32; 3] = [2, 6, 8]; // pxp_axi, lcdif_axi, lcdif_pix
    let before = CCM.read(CCGR3);
    for s in OTHERS {
        ungate(CCGR3, s);
    }
    GPC.modify(PGC_DISP_CTRL, 0, 1);
    GPC.modify(CNTR, 0, DISP_PUP_REQ);
    let domain_ok = timer::poll_until(5, || GPC.read(CNTR) & DISP_PUP_REQ == 0);
    timer::delay_us(10);
    for s in OTHERS {
        if before & (0b11 << s) == 0 {
            gate(CCGR3, s);
        }
    }
    EpdcClocks { axi_sel, pix_sel, parents_ok, domain_ok }
}

pub fn epdc_off() {
    gate(CCGR3, 10);
    gate(CCGR3, 4);
}
