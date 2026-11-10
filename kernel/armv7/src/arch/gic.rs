//! Cortex-A9 MPCore interrupt controller (GIC v1 / PL390 programming model).
//!
//! Distributor at PERIPHBASE + 0x1000, CPU interface at PERIPHBASE + 0x100; PERIPHBASE comes from CBAR
//! (0x00A0_0000 on the i.MX6SL and on QEMU's i.MX6Q). The previous kernel leaves interrupts enabled and
//! routed, so `init` starts from a clean distributor. Reference: Linux `drivers/irqchip/irq-gic.c`.

use super::{cpu, Mmio};
use core::sync::atomic::{AtomicUsize, Ordering};

static DIST: AtomicUsize = AtomicUsize::new(0);
static CPUI: AtomicUsize = AtomicUsize::new(0);

const GICD_CTLR: usize = 0x000;
const GICD_TYPER: usize = 0x004;
const GICD_ISENABLER: usize = 0x100;
const GICD_ICENABLER: usize = 0x180;
const GICD_ICPENDR: usize = 0x280;
const GICD_IPRIORITYR: usize = 0x400;
const GICD_ITARGETSR: usize = 0x800;

const GICC_CTLR: usize = 0x00;
const GICC_PMR: usize = 0x04;
const GICC_BPR: usize = 0x08;
const GICC_IAR: usize = 0x0c;
const GICC_EOIR: usize = 0x10;

pub const SPURIOUS: u32 = 1023;

fn dist() -> Mmio {
    Mmio::at(DIST.load(Ordering::Relaxed))
}

fn cpui() -> Mmio {
    Mmio::at(CPUI.load(Ordering::Relaxed))
}

/// Number of interrupt lines the distributor implements.
pub fn lines() -> u32 {
    32 * ((dist().read(GICD_TYPER) & 0x1f) + 1)
}

/// The A9 MPCore block on every i.MX6 (`arm,cortex-a9-gic` at 0x00a01000 in imx6sl.dtsi and imx6qdl.dtsi).
const IMX6_PERIPHBASE: usize = 0x00a0_0000;

pub fn init() -> usize {
    // QEMU's i.MX6 models leave CBAR at 0; the hardware reports the real base.
    let periph = match cpu::cbar() as usize & !0x1fff {
        0 => IMX6_PERIPHBASE,
        p => p,
    };
    DIST.store(periph + 0x1000, Ordering::Relaxed);
    CPUI.store(periph + 0x100, Ordering::Relaxed);
    let d = dist();
    let c = cpui();

    d.write(GICD_CTLR, 0);
    let n = lines() as usize;
    for i in 0..n / 32 {
        d.write(GICD_ICENABLER + 4 * i, 0xffff_ffff);
        d.write(GICD_ICPENDR + 4 * i, 0xffff_ffff);
    }
    for i in 0..n / 4 {
        d.write(GICD_IPRIORITYR + 4 * i, 0xa0a0_a0a0);
    }
    // SPIs to CPU 0 (the A9 here is the only core).
    for i in 8..n / 4 {
        d.write(GICD_ITARGETSR + 4 * i, 0x0101_0101);
    }
    d.write(GICD_CTLR, 1);

    c.write(GICC_PMR, 0xf0);
    c.write(GICC_BPR, 0);
    c.write(GICC_CTLR, 1);
    periph
}

pub fn enable(irq: u32) {
    dist().write(GICD_ISENABLER + 4 * (irq as usize / 32), 1 << (irq % 32));
}

pub fn disable(irq: u32) {
    dist().write(GICD_ICENABLER + 4 * (irq as usize / 32), 1 << (irq % 32));
}

/// Acknowledge the highest-priority pending interrupt; `SPURIOUS` when there is none.
pub fn ack() -> u32 {
    cpui().read(GICC_IAR)
}

pub fn eoi(iar: u32) {
    cpui().write(GICC_EOIR, iar);
}
