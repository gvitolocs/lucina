//! i.MX General Purpose Timer (GPT, 0x0209_8000, SPI 55) as the tick and the clock.
//!
//! Clocked from the 32.768 kHz reference (CLKSRC = 4), which runs at the same rate whatever cpufreq or the
//! previous kernel did to the PLLs. Free-running counter for `now()`, output compare 1 for a 100 Hz tick.
//! Reference: Linux `drivers/clocksource/timer-imx-gpt.c` (v2 register layout, i.MX6).

use super::{gic, Mmio};
use core::sync::atomic::{AtomicU32, Ordering};

const GPT: Mmio = Mmio::at(0x0209_8000);
pub const IRQ: u32 = 32 + 55;

const CR: usize = 0x00;
const PR: usize = 0x04;
const SR: usize = 0x08;
const IR: usize = 0x0c;
const OCR1: usize = 0x10;
const CNT: usize = 0x24;

const CR_EN: u32 = 1 << 0;
const CR_ENMOD: u32 = 1 << 1;
const CR_DBGEN: u32 = 1 << 2;
const CR_WAITEN: u32 = 1 << 3;
const CR_CLKSRC_32K: u32 = 4 << 6;
const CR_FRR: u32 = 1 << 9;
const CR_SWR: u32 = 1 << 15;
const SR_OF1: u32 = 1 << 0;
const IR_OF1IE: u32 = 1 << 0;

pub const FREQ: u32 = 32_768;
pub const HZ: u32 = 100;
const PERIOD: u32 = (FREQ + HZ / 2) / HZ; // 328 counts = 10.01 ms

static TICKS: AtomicU32 = AtomicU32::new(0);

pub fn init() {
    GPT.write(CR, 0);
    GPT.write(IR, 0);
    GPT.write(CR, CR_SWR);
    let mut spins = 0;
    while GPT.read(CR) & CR_SWR != 0 && spins < 100_000 {
        spins += 1;
    }
    GPT.write(PR, 0);
    GPT.write(SR, 0x3f);
    GPT.write(CR, CR_CLKSRC_32K | CR_FRR | CR_ENMOD | CR_WAITEN | CR_DBGEN);
    GPT.write(OCR1, PERIOD);
    GPT.write(IR, IR_OF1IE);
    GPT.modify(CR, 0, CR_EN);
    gic::enable(IRQ);
}

/// Counts of the 32.768 kHz clock since `init` (wraps after 36 hours).
pub fn now() -> u32 {
    GPT.read(CNT)
}

pub fn ticks() -> u32 {
    TICKS.load(Ordering::Relaxed)
}

/// Busy-wait; works with interrupts masked.
pub fn delay_us(us: u32) {
    let counts = ((us as u64 * FREQ as u64).div_ceil(1_000_000) as u32).max(1);
    let start = now();
    while now().wrapping_sub(start) <= counts {}
}

pub fn delay_ms(ms: u32) {
    for _ in 0..ms {
        delay_us(1000);
    }
}

/// Poll `cond` until it holds or `timeout_ms` passes; true if it held.
pub fn poll_until(timeout_ms: u32, mut cond: impl FnMut() -> bool) -> bool {
    let limit = (timeout_ms as u64 * FREQ as u64 / 1000) as u32;
    let start = now();
    loop {
        if cond() {
            return true;
        }
        if now().wrapping_sub(start) > limit {
            return cond();
        }
    }
}

/// Called from the IRQ dispatcher.
pub fn handle_irq() {
    GPT.write(SR, SR_OF1);
    GPT.write(OCR1, GPT.read(OCR1).wrapping_add(PERIOD));
    TICKS.fetch_add(1, Ordering::Relaxed);
}
