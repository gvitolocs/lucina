//! The panel: an E Ink ED060KD1C2 (Carta 1.2), scanned landscape, and the controller's values for it.
//!
//! The numbers are Amazon's `PANEL_MODE_ED060TC1_3CE_CARTA_1_2`, as in the device tree of the Linux on this device. The
//! horizontal scan's two porches are in that Linux's order, the opposite of Amazon's, as that is the order that ran here.

/// The panel's width in pixels, as the controller scans it.
pub const WIDTH: u32 = 1448;
/// The panel's height in pixels.
pub const HEIGHT: u32 = 1072;
/// The nominal refresh rate, in the waveform file's header too.
pub const REFRESH_HZ: u32 = 85;

/// The working buffer: 16 bits per pixel, the controller's own state.
pub const WORKING_BUFFER_SIZE: usize = (WIDTH * HEIGHT * 2) as usize;
/// The update buffer: 8 bits per pixel, what the controller reads.
pub const UPDATE_BUFFER_SIZE: usize = (WIDTH * HEIGHT) as usize;

const HFRONT: u32 = 104;
const HBACK: u32 = 16;
const HSYNC: u32 = 26;
const VFRONT: u32 = 4;
const VBACK: u32 = 4;
const VSYNC: u32 = 2;
const VSCAN_HOLDOFF: u32 = 4;
const SDOED_WIDTH: u32 = 10;
const SDOED_DELAY: u32 = 20;
const SDOEZ_WIDTH: u32 = 10;
const SDOEZ_DELAY: u32 = 20;
const GDCLK_HP_OFFS: u32 = 562;
const GDSP_OFFS: u32 = 662;
const GDOE_OFFS: u32 = 0;
const GDCLK_OFFS: u32 = 225;
const NUM_CE: u32 = 3;

const PIXELS_PER_SDCLK_4: u32 = 2;
const SDCFG_SDCLK_HOLD: u32 = 0x20_0000;
const SDCFG_SDSHR: u32 = 0x10_0000;
const SDCFG_FLIP_PIXELS: u32 = 0x4000;
const GDCFG_GDRL: u32 = 0x10;
const POL_SDLE_HIGH: u32 = 0x2;
const POL_SDOE_HIGH: u32 = 0x4;
const POL_GDOE_HIGH: u32 = 0x8;

/// `EPDC_RES`.
pub const RES: u32 = (HEIGHT << 16) | WIDTH;
/// `EPDC_FIFOCTRL`: the init, high and low levels.
pub const FIFOCTRL: u32 = (100 << 16) | (200 << 8) | 100;
/// `EPDC_TCE_CTRL`.
pub const TCE_CTRL: u32 = (VSCAN_HOLDOFF << 16) | PIXELS_PER_SDCLK_4;
/// `EPDC_TCE_HSCAN1`.
pub const TCE_HSCAN1: u32 = (HSYNC << 16) | HSYNC;
/// `EPDC_TCE_HSCAN2`.
pub const TCE_HSCAN2: u32 = (HBACK << 16) | HFRONT;
/// `EPDC_TCE_VSCAN`.
pub const TCE_VSCAN: u32 = (VBACK << 16) | (VFRONT << 8) | VSYNC;
/// `EPDC_TCE_OE`.
pub const TCE_OE: u32 =
    (SDOED_WIDTH << 24) | (SDOED_DELAY << 16) | (SDOEZ_WIDTH << 8) | SDOEZ_DELAY;
/// `EPDC_TCE_TIMING2`.
pub const TCE_TIMING2: u32 = (GDCLK_HP_OFFS << 16) | GDSP_OFFS;
/// `EPDC_TCE_TIMING3`.
pub const TCE_TIMING3: u32 = (GDOE_OFFS << 16) | GDCLK_OFFS;
/// `EPDC_TCE_SDCFG`.
pub const TCE_SDCFG: u32 =
    SDCFG_SDCLK_HOLD | SDCFG_SDSHR | (NUM_CE << 16) | SDCFG_FLIP_PIXELS | (WIDTH / NUM_CE);
/// `EPDC_TCE_GDCFG`.
pub const TCE_GDCFG: u32 = GDCFG_GDRL;
/// `EPDC_TCE_POLARITY`.
pub const TCE_POLARITY: u32 = POL_SDLE_HIGH | POL_SDOE_HIGH | POL_GDOE_HIGH;

#[cfg(test)]
mod tests {
    use super::*;

    /// The words the Linux on this device computes from its device tree, by hand from its driver.
    #[test]
    fn the_register_words_are_those_that_ran_on_the_device() {
        assert_eq!(RES, 0x0430_05a8);
        assert_eq!(FIFOCTRL, 0x0064_c864);
        assert_eq!(TCE_CTRL, 0x0004_0002);
        assert_eq!(TCE_HSCAN1, 0x001a_001a);
        assert_eq!(TCE_HSCAN2, 0x0010_0068);
        assert_eq!(TCE_VSCAN, 0x0004_0402);
        assert_eq!(TCE_OE, 0x0a14_0a14);
        assert_eq!(TCE_TIMING2, 0x0232_0296);
        assert_eq!(TCE_TIMING3, 0x0000_00e1);
        assert_eq!(TCE_SDCFG, 0x0033_41e2);
        assert_eq!(TCE_GDCFG, 0x0000_0010);
        assert_eq!(TCE_POLARITY, 0x0000_000e);
    }
}
