//! i.MX watchdog WDOG1 (0x020B_C000): keep it fed when the previous OS started it, and reset the SoC.
//!
//! Reference: Linux `drivers/watchdog/imx2_wdt.c`. Once WCR.WDE is set it cannot be cleared, so a kernel that
//! follows Linux (lucina-update arms it for OTA trial boots) has to service it.

use crate::arch::{timer, Mmio};

const WDOG1: Mmio = Mmio::at(0x020b_c000);
const WCR: usize = 0x00;
const WSR: usize = 0x02;
const WMCR: usize = 0x08;

const WCR_WDE: u16 = 1 << 2;
const WCR_SRS: u16 = 1 << 4;
const WCR_WDA: u16 = 1 << 5;

pub fn init() -> bool {
    // The power-down counter resets the SoC 16 s after a reset unless it is disabled; the bootloader and
    // Linux clear it, and it can only be cleared, so writing 0 again is harmless.
    WDOG1.write16(WMCR, 0);
    service();
    running()
}

pub fn running() -> bool {
    WDOG1.read16(WCR) & WCR_WDE != 0
}

/// The service sequence; a no-op for a watchdog that was never started.
pub fn service() {
    if running() {
        WDOG1.write16(WSR, 0x5555);
        WDOG1.write16(WSR, 0xaaaa);
    }
}

/// Software reset through the watchdog, as `imx2_wdt_restart` does (timeout 0.5 s, assert both resets).
pub fn reset() -> ! {
    crate::arch::cpu::disable_irq();
    let wcr = WCR_WDE;
    WDOG1.write16(WCR, wcr);
    WDOG1.write16(WCR, wcr);
    timer::delay_ms(500);
    // If SRS did not take, let the watchdog time out instead.
    WDOG1.write16(WCR, WCR_WDE | WCR_SRS | WCR_WDA);
    crate::arch::cpu::halt()
}
