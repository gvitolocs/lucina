//! ARMv7-A (Cortex-A9) architecture layer.
//!
//! The module split follows OSTD's `ostd/src/arch/<arch>` (boot, cpu, irq, mm, serial, timer, trap) so this
//! code can become `ostd/src/arch/armv7` in Phase 1 (see `docs/research/pw3-asterinas-armv7.md`).

pub mod gic;
pub mod mm;
pub mod serial;
pub mod timer;
pub mod trap;

core::arch::global_asm!(include_str!("boot.S"), options(raw));

/// Fixed I/O window: the i.MX6 peripherals (AIPS, the A9 MPCore block) sit below 64 MiB physical and are mapped
/// at `IO_VBASE + pa` by `boot.S`.
pub const IO_VBASE: usize = 0xF000_0000;
pub const IO_SIZE: usize = 64 << 20;

/// Virtual address of a peripheral register block in the fixed I/O window.
pub const fn io(pa: usize) -> usize {
    assert!(pa < IO_SIZE);
    IO_VBASE + pa
}

/// A 32-bit memory-mapped register block.
#[derive(Clone, Copy)]
pub struct Mmio(usize);

impl Mmio {
    /// `pa` must be the physical base of a device block inside the I/O window.
    pub const fn at(pa: usize) -> Self {
        Mmio(io(pa))
    }

    pub fn read(self, off: usize) -> u32 {
        // SAFETY: the I/O window maps every address below 64 MiB physical as Device memory.
        unsafe { core::ptr::read_volatile((self.0 + off) as *const u32) }
    }

    pub fn write(self, off: usize, val: u32) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((self.0 + off) as *mut u32, val) }
    }

    pub fn read16(self, off: usize) -> u16 {
        // SAFETY: as above.
        unsafe { core::ptr::read_volatile((self.0 + off) as *const u16) }
    }

    pub fn write16(self, off: usize, val: u16) {
        // SAFETY: as above.
        unsafe { core::ptr::write_volatile((self.0 + off) as *mut u16, val) }
    }

    pub fn modify(self, off: usize, clear: u32, set: u32) {
        self.write(off, (self.read(off) & !clear) | set);
    }
}

/// CPU identification and control (CP15).
pub mod cpu {
    use core::arch::asm;

    pub fn midr() -> u32 {
        let v: u32;
        // SAFETY: reading MIDR has no side effects.
        unsafe { asm!("mrc p15, 0, {}, c0, c0, 0", out(reg) v, options(nomem, nostack)) };
        v
    }

    /// Configuration Base Address Register: physical base of the A9 MPCore private peripherals.
    pub fn cbar() -> u32 {
        let v: u32;
        // SAFETY: CBAR is a read-only Cortex-A9 register.
        unsafe { asm!("mrc p15, 4, {}, c15, c0, 0", out(reg) v, options(nomem, nostack)) };
        v
    }

    pub fn sctlr() -> u32 {
        let v: u32;
        // SAFETY: reading SCTLR has no side effects.
        unsafe { asm!("mrc p15, 0, {}, c1, c0, 0", out(reg) v, options(nomem, nostack)) };
        v
    }

    pub fn cpsr() -> u32 {
        let v: u32;
        // SAFETY: reading CPSR has no side effects.
        unsafe { asm!("mrs {}, cpsr", out(reg) v, options(nomem, nostack)) };
        v
    }

    pub fn enable_irq() {
        // SAFETY: the vectors and the IRQ stack are set up by boot.S before kernel_main.
        unsafe { asm!("cpsie i", options(nomem, nostack)) };
    }

    pub fn disable_irq() {
        // SAFETY: masking interrupts is always allowed in SVC mode.
        unsafe { asm!("cpsid i", options(nomem, nostack)) };
    }

    pub fn wfi() {
        // SAFETY: waits for an interrupt; with IRQs masked it still wakes and falls through.
        unsafe { asm!("dsb", "wfi", options(nomem, nostack)) };
    }

    pub fn dsb() {
        // SAFETY: a barrier.
        unsafe { asm!("dsb", options(nostack)) };
    }

    pub fn halt() -> ! {
        disable_irq();
        loop {
            wfi();
        }
    }
}
