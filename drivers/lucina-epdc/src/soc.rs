//! The SoC around the panel: the controller's clocks, the display power domain and the panel's pads.
//!
//! The clock's dividers and muxes are the ones the previous kernel left: this panel has only ever run on those, and a
//! kexec keeps them. Their values are in [`clock_state`] for the log. A start from cold, without Linux before, needs them
//! set here first.

use crate::{Error, Hw};

const CCM: usize = 0x020c_4000;
const CCM_CBCMR: usize = CCM + 0x18;
const CCM_CHSCCDR: usize = CCM + 0x34;
const CCM_CSCDR2: usize = CCM + 0x38;
const CCM_CCGR3: usize = CCM + 0x74;
const GATE_ON: u32 = 0b11;
const GATE_PXP_AXI: u32 = 2;
const GATE_EPDC_AXI: u32 = 4;
const GATE_LCDIF_AXI: u32 = 6;
const GATE_LCDIF_PIX: u32 = 8;
const GATE_EPDC_PIX: u32 = 10;

const ANATOP: usize = 0x020c_8000;
const ANATOP_CLR: usize = 8;
const ANATOP_PFD_480: usize = ANATOP + 0xf0;
const ANATOP_PFD_528: usize = ANATOP + 0x100;

const GPC: usize = 0x020d_c000;
const GPC_CNTR: usize = GPC;
const GPC_DISP_PUP_REQ: u32 = 1 << 5;
const GPC_PGC_DISP_CTRL: usize = GPC + 0x240;

const IOMUXC: usize = 0x020e_0000;
const ALT_EPDC: u32 = 0;
const ALT_GPIO: u32 = 5;
/// LVE, hysteresis, no pull or keeper, 100 MHz, 40 ohm, fast: what drew on this panel.
const PAD_EPDC: u32 = 0x0041_00b1;
/// The same with a 100k pull-down, for the two lines that must stay low with the panel off.
const PAD_PARKED_LOW: u32 = 0x0041_30b1;
/// The power-good input: LVE, hysteresis, 100k pull-up, open drain, 100 MHz, 60 ohm.
const PAD_POWER_GOOD: u32 = 0x0041_b8a0;
/// `EPDC_PWRSTAT`, as a GPIO input.
const POWER_GOOD_MUX: usize = IOMUXC + 0x0f8;
const POWER_GOOD_PAD: usize = IOMUXC + 0x3e8;

const GPIO2_PSR: usize = 0x020a_0000 + 0x08;
const POWER_GOOD_BIT: u32 = 1 << 13;

/// A pad: its mux and control registers, and whether it stays low when parked.
#[derive(Clone, Copy)]
struct Pad {
    mux: usize,
    ctl: usize,
    parked_low: bool,
}

const fn pad(mux: usize, ctl: usize) -> Pad {
    Pad {
        mux: IOMUXC + mux,
        ctl: IOMUXC + ctl,
        parked_low: false,
    }
}

const fn pad_low(mux: usize, ctl: usize) -> Pad {
    Pad {
        mux: IOMUXC + mux,
        ctl: IOMUXC + ctl,
        parked_low: true,
    }
}

/// The pads that are parked while the panel has no power: driven, they leak about 5.6 mA into it.
const PARKED_PADS: [Pad; 15] = [
    pad(0x090, 0x380),     // D0
    pad(0x094, 0x384),     // D1
    pad(0x0b0, 0x3a0),     // D2
    pad(0x0b4, 0x3a4),     // D3
    pad(0x0b8, 0x3a8),     // D4
    pad(0x0bc, 0x3ac),     // D5
    pad(0x0c0, 0x3b0),     // D6
    pad(0x0c4, 0x3b4),     // D7
    pad(0x0d0, 0x3c0),     // GDCLK
    pad_low(0x0d4, 0x3c4), // GDOE
    pad(0x0dc, 0x3cc),     // GDSP
    pad_low(0x100, 0x3f0), // SDCE0
    pad(0x110, 0x400),     // SDCLK
    pad(0x114, 0x404),     // SDLE
    pad(0x118, 0x408),     // SDOE
];
/// `EPDC_PWRCOM`, which is not parked.
const PWRCOM: Pad = pad(0x0e0, 0x3d0);

fn gate<H: Hw>(hw: &mut H, shift: u32, on: bool) {
    let val = hw.read32(CCM_CCGR3);
    let val = if on {
        val | (GATE_ON << shift)
    } else {
        val & !(GATE_ON << shift)
    };
    hw.write32(CCM_CCGR3, val);
}

/// Turns on the parents of the two clocks, as the previous kernel may have gated them, whichever they are.
fn parents_on<H: Hw>(hw: &mut H) {
    // pll3_pfd1 and pll3_pfd2 are the parents on this device, and nothing else uses them.
    let axi_sel = (hw.read32(CCM_CHSCCDR) >> 15) & 7;
    let pix_sel = (hw.read32(CCM_CSCDR2) >> 15) & 7;
    let pfd = |n: u32| 1u32 << (8 * n + 7);
    match axi_sel {
        3 => hw.write32(ANATOP_PFD_528 + ANATOP_CLR, pfd(0)),
        4 => hw.write32(ANATOP_PFD_528 + ANATOP_CLR, pfd(2)),
        5 => hw.write32(ANATOP_PFD_480 + ANATOP_CLR, pfd(2)),
        _ => {}
    }
    match pix_sel {
        3 => hw.write32(ANATOP_PFD_528 + ANATOP_CLR, pfd(0)),
        4 => hw.write32(ANATOP_PFD_528 + ANATOP_CLR, pfd(1)),
        5 => hw.write32(ANATOP_PFD_480 + ANATOP_CLR, pfd(1)),
        _ => {}
    }
}

/// Turns on the controller's clocks. Its registers must not be touched before this.
pub fn clocks_on<H: Hw>(hw: &mut H) {
    parents_on(hw);
    gate(hw, GATE_EPDC_AXI, true);
    gate(hw, GATE_EPDC_PIX, true);
}

/// Whether the controller's clocks are on, so that its registers can be touched.
pub fn clocks_are_on<H: Hw>(hw: &mut H) -> bool {
    let gates = hw.read32(CCM_CCGR3);
    gates & (GATE_ON << GATE_EPDC_AXI) != 0 && gates & (GATE_ON << GATE_EPDC_PIX) != 0
}

/// Turns off the controller's clocks, the opposite order.
pub fn clocks_off<H: Hw>(hw: &mut H) {
    gate(hw, GATE_EPDC_PIX, false);
    gate(hw, GATE_EPDC_AXI, false);
}

/// Powers up the display domain, with the reset clocks of everything in it running. The other kernel leaves it on.
pub fn power_domain_on<H: Hw>(hw: &mut H) -> Result<(), Error> {
    let before = hw.read32(CCM_CCGR3);
    const OTHERS: [u32; 3] = [GATE_PXP_AXI, GATE_LCDIF_AXI, GATE_LCDIF_PIX];
    for shift in OTHERS {
        gate(hw, shift, true);
    }
    let pgc = hw.read32(GPC_PGC_DISP_CTRL);
    hw.write32(GPC_PGC_DISP_CTRL, pgc | 1);
    let cntr = hw.read32(GPC_CNTR);
    hw.write32(GPC_CNTR, cntr | GPC_DISP_PUP_REQ);
    let mut done = false;
    for _ in 0..500 {
        if hw.read32(GPC_CNTR) & GPC_DISP_PUP_REQ == 0 {
            done = true;
            break;
        }
        hw.delay_us(10);
    }
    hw.delay_us(10);
    for shift in OTHERS {
        if before & (GATE_ON << shift) == 0 {
            gate(hw, shift, false);
        }
    }
    if done {
        Ok(())
    } else {
        Err(Error::PowerDomain)
    }
}

/// Gives the panel's pads to the controller. Only with the panel's logic supply on.
pub fn pads_on<H: Hw>(hw: &mut H) {
    for pad in PARKED_PADS.iter().chain(core::iter::once(&PWRCOM)) {
        hw.write32(pad.ctl, PAD_EPDC);
        hw.write32(pad.mux, ALT_EPDC);
    }
}

/// Takes the panel's pads from the controller, for a panel without power.
pub fn pads_park<H: Hw>(hw: &mut H) {
    for pad in &PARKED_PADS {
        hw.write32(pad.mux, ALT_GPIO);
        hw.write32(
            pad.ctl,
            if pad.parked_low {
                PAD_PARKED_LOW
            } else {
                PAD_EPDC
            },
        );
    }
}

/// Makes the power-good pin readable.
pub fn power_good_setup<H: Hw>(hw: &mut H) {
    hw.write32(POWER_GOOD_PAD, PAD_POWER_GOOD);
    hw.write32(POWER_GOOD_MUX, ALT_GPIO);
}

/// Whether the panel's rails say they are good.
pub fn power_good<H: Hw>(hw: &mut H) -> bool {
    hw.read32(GPIO2_PSR) & POWER_GOOD_BIT != 0
}

/// The registers that set the controller's clocks, for the log.
pub fn clock_state<H: Hw>(hw: &mut H) -> [(&'static str, u32); 5] {
    [
        ("CCGR3", hw.read32(CCM_CCGR3)),
        ("CHSCCDR", hw.read32(CCM_CHSCCDR)),
        ("CSCDR2", hw.read32(CCM_CSCDR2)),
        ("CBCMR", hw.read32(CCM_CBCMR)),
        ("PFD_480", hw.read32(ANATOP_PFD_480)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Model;

    #[test]
    fn the_pads_go_to_the_controller_and_back() {
        let mut hw = Model::new();
        pads_on(&mut hw);
        assert_eq!(hw.get(IOMUXC + 0x090), ALT_EPDC);
        assert_eq!(hw.get(IOMUXC + 0x380), PAD_EPDC);
        assert_eq!(hw.get(PWRCOM.mux), ALT_EPDC);
        pads_park(&mut hw);
        assert_eq!(hw.get(IOMUXC + 0x090), ALT_GPIO);
        assert_eq!(hw.get(IOMUXC + 0x0d4), ALT_GPIO);
        assert_eq!(hw.get(IOMUXC + 0x3c4), PAD_PARKED_LOW);
        assert_eq!(hw.get(IOMUXC + 0x3f0), PAD_PARKED_LOW);
        assert_eq!(hw.get(PWRCOM.mux), ALT_EPDC);
    }

    #[test]
    fn the_clocks_turn_on_and_off_and_leave_others_alone() {
        let mut hw = Model::new();
        hw.set(CCM_CCGR3, 0x0000_0003);
        clocks_on(&mut hw);
        assert_eq!(hw.get(CCM_CCGR3), 0x0000_0003 | (3 << 4) | (3 << 10));
        clocks_off(&mut hw);
        assert_eq!(hw.get(CCM_CCGR3), 0x0000_0003);
    }

    #[test]
    fn the_power_domain_comes_up_and_restores_the_other_clocks() {
        let mut hw = Model::new();
        hw.set(CCM_CCGR3, 3 << 4);
        power_domain_on(&mut hw).unwrap();
        assert_eq!(hw.get(CCM_CCGR3), 3 << 4);
        assert_eq!(hw.get(GPC_PGC_DISP_CTRL) & 1, 1);
    }

    #[test]
    fn a_power_domain_that_stays_down_is_an_error() {
        let mut hw = Model::new();
        hw.stuck_power_domain = true;
        assert_eq!(power_domain_on(&mut hw), Err(Error::PowerDomain));
    }
}
