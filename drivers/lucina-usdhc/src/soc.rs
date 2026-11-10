//! The clock of uSDHC2. Neither Amazon's kernel nor Linux sets its rate: it is what the bootloader left, so it is read.

pub const CCM_BASE: usize = 0x020c_4000;
pub const ANATOP_BASE: usize = 0x020c_8000;

pub const CCM_CSCMR1: usize = CCM_BASE + 0x1c;
pub const CCM_CSCDR1: usize = CCM_BASE + 0x24;
pub const CCM_CCGR6: usize = CCM_BASE + 0x80;
pub const ANATOP_PLL2: usize = ANATOP_BASE + 0x30;
pub const ANATOP_PFD_528: usize = ANATOP_BASE + 0x100;

/// CG2 of CCGR6: with this off, any access to the controller's registers hangs the bus. Linux leaves it off when the host
/// has been idle for 50 ms, and nothing turns it back on before a kexec.
pub const CCGR6_USDHC2: u32 = 3 << 4;

const CSCMR1_USDHC2_PFD0: u32 = 1 << 17;
const CSCDR1_USDHC2_PODF_SHIFT: u32 = 16;

/// The controller's clock from those four registers, or `None` if they do not make sense.
pub fn base_hz(cscmr1: u32, cscdr1: u32, pll2: u32, pfd_528: u32) -> Option<u32> {
    let pll_hz: u64 = if pll2 & 1 != 0 { 528_000_000 } else { 480_000_000 };
    let frac = if cscmr1 & CSCMR1_USDHC2_PFD0 != 0 {
        pfd_528 & 0x3f
    } else {
        (pfd_528 >> 16) & 0x3f
    };
    // The fractional divider's range.
    if !(12..=35).contains(&frac) {
        return None;
    }
    let podf = ((cscdr1 >> CSCDR1_USDHC2_PODF_SHIFT) & 7) + 1;
    Some((pll_hz * 18 / frac as u64 / podf as u64) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_rates() {
        // PFD2 at 24: 396 MHz, divided by 2.
        assert_eq!(base_hz(0, 1 << 16, 1, 24 << 16), Some(198_000_000));
        // PFD0 at 27: 352 MHz.
        assert_eq!(base_hz(1 << 17, 0, 1, 27), Some(352_000_000));
        assert_eq!(base_hz(0, 7 << 16, 0, 24 << 16), Some(45_000_000));
    }

    #[test]
    fn nonsense_is_no_rate() {
        assert_eq!(base_hz(0, 0, 1, 0), None);
        assert_eq!(base_hz(0, 0, 1, 40 << 16), None);
        assert_eq!(base_hz(1 << 17, 0, 1, 24 << 16), None);
    }
}
