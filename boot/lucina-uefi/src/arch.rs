//! The AArch64 instructions LucinaUEFI needs: the exception level, cache maintenance and the jump into the
//! kernel, which turns the MMU off (UEFI runs with it on, the arm64 boot protocol wants it off) and, when the
//! firmware left us at EL2, drops to EL1 (Asterinas only runs at EL1).

use core::arch::{asm, global_asm};

use lucina_boot::Range;

/// The exception level we run at (CurrentEL bits 3:2).
pub fn current_el() -> u8 {
    let el: u64;
    // SAFETY: reading CurrentEL has no side effects and is allowed at EL1 and above.
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    ((el >> 2) & 3) as u8
}

fn dcache_line() -> u64 {
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL1 and EL2.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    4 << ((ctr >> 16) & 0xf)
}

/// Cleans `r` to the point of coherency, so it is in memory once the MMU and the data cache are off.
pub fn dcache_clean(r: Range) {
    let line = dcache_line();
    let mut a = r.start & !(line - 1);
    while a < r.end {
        // SAFETY: cleaning a line writes it back if it is dirty; memory contents do not change.
        unsafe { asm!("dc cvac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: a barrier only.
    unsafe { asm!("dsb sy", options(nostack)) };
}

global_asm!(
    r#"
    .text
    .balign 64
    .global lucina_uefi_jump
// x0 = kernel entry, x1 = device tree, x2 = CurrentEL (1 or 2). Uses no memory once the MMU is off: everything
// it executes was cleaned to the point of coherency by the caller, and there are no literal loads.
lucina_uefi_jump:
    msr     daifset, #0xf
    mov     x19, x0
    mov     x20, x1
    cmp     x2, #2
    b.eq    2f

    // EL1: MMU and data cache off (SCTLR_EL1.M, .C), as the boot protocol requires.
    mrs     x3, sctlr_el1
    bic     x3, x3, #1
    bic     x3, x3, #(1 << 2)
    msr     sctlr_el1, x3
    isb
    ic      iallu
    tlbi    vmalle1
    dsb     sy
    isb
    mov     x0, x20
    mov     x1, xzr
    mov     x2, xzr
    mov     x3, xzr
    br      x19

2:  // EL2: MMU and data cache off at EL2 first (SCTLR_EL2.M, .C).
    mrs     x3, sctlr_el2
    bic     x3, x3, #1
    bic     x3, x3, #(1 << 2)
    msr     sctlr_el2, x3
    isb
    ic      iallu
    tlbi    alle2
    dsb     sy
    isb
    // HCR_EL2: only RW (EL1 is AArch64). E2H = 0 before any *_EL1 access, which E2H = 1 would redirect to EL2;
    // no traps, no stage 2 translation.
    mov     x3, #(1 << 31)
    msr     hcr_el2, x3
    isb
    msr     vttbr_el2, xzr
    msr     hstr_el2, xzr
    // CPTR_EL2: no FP/SIMD traps (RES1 bits 13:12, 9:0).
    mov     x3, #0x33ff
    msr     cptr_el2, x3
    // EL1 may use the physical counter and timer; the virtual one (Asterinas' timer) has no offset.
    mrs     x3, cnthctl_el2
    orr     x3, x3, #3
    msr     cnthctl_el2, x3
    msr     cntvoff_el2, xzr
    // EL1 sees the real CPU identity.
    mrs     x3, midr_el1
    msr     vpidr_el2, x3
    mrs     x3, mpidr_el1
    msr     vmpidr_el2, x3
    // GICv3 system registers (ID_AA64PFR0_EL1.GIC): ICC_SRE_EL2.SRE and .Enable, so EL1 can use ICC_*_EL1;
    // no virtual interface (ICH_HCR_EL2 = 0).
    mrs     x3, id_aa64pfr0_el1
    ubfx    x3, x3, #24, #4
    cbz     x3, 3f
    mrs     x3, S3_4_C12_C9_5
    orr     x3, x3, #0x1
    orr     x3, x3, #0x8
    msr     S3_4_C12_C9_5, x3
    isb
    msr     S3_4_C12_C11_0, xzr
3:  // SCTLR_EL1: MMU and caches off, RES1 bits (29, 28, 23, 22, 20, 11).
    movz    x3, #0x0800
    movk    x3, #0x30d0, lsl #16
    msr     sctlr_el1, x3
    // CPACR_EL1: FP/SIMD enabled at EL1.
    mov     x3, #(3 << 20)
    msr     cpacr_el1, x3
    // Enter EL1h with D, A, I, F masked at the kernel entry.
    mov     x3, #0x3c5
    msr     spsr_el2, x3
    msr     elr_el2, x19
    mov     x0, x20
    mov     x1, xzr
    mov     x2, xzr
    mov     x3, xzr
    isb
    eret
"#
);

unsafe extern "C" {
    fn lucina_uefi_jump(entry: u64, dtb: u64, el: u64) -> !;
}

/// Starts an arm64 kernel image at `entry` with `x0 = dtb`, `x1..x3 = 0`, interrupts masked, MMU and data cache
/// off, at EL1 (dropping from EL2 when `el` is 2).
///
/// # Safety
///
/// Boot services must have been exited. The kernel, its device tree and initramfs, and this loader's own code
/// must be cleaned to the point of coherency; UEFI's identity mapping must be in place (it is until the MMU goes
/// off, and the trampoline continues at the same addresses).
pub unsafe fn enter_kernel(entry: u64, dtb: u64, el: u8) -> ! {
    // SAFETY: by the caller.
    unsafe { lucina_uefi_jump(entry, dtb, u64::from(el)) }
}

pub fn wait_forever() -> ! {
    loop {
        // SAFETY: waiting for an event has no side effects.
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}

/// Microseconds on the architected counter.
pub fn counter_us() -> u64 {
    let (count, freq): (u64, u64);
    // SAFETY: reading the counter and its frequency has no side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) count);
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq);
    }
    (count as u128 * 1_000_000 / freq.max(1) as u128) as u64
}

/// Milliseconds on the architected counter, which runs from the machine's reset: where a boot's time went.
pub fn counter_ms() -> u64 {
    let (count, freq): (u64, u64);
    // SAFETY: reading the counter and its frequency has no side effects.
    unsafe {
        core::arch::asm!("mrs {}, cntpct_el0", out(reg) count, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack));
    }
    count / (freq / 1000).max(1)
}
