//! The AArch64 instructions LucinaBoot needs, and its raw memory access. All of LucinaBoot's `unsafe` is here
//! and in `console.rs`.

use core::{
    arch::asm,
    sync::atomic::{AtomicU8, Ordering},
};

use lucina_boot::Range;
use lucina_fdt::Fdt;

/// The exception level we run at (CurrentEL bits 3:2).
pub fn current_el() -> u64 {
    let el: u64;
    // SAFETY: reading CurrentEL has no side effects and is allowed at EL1 and above.
    unsafe { asm!("mrs {}, CurrentEL", out(reg) el, options(nomem, nostack)) };
    (el >> 2) & 3
}

fn dcache_line() -> u64 {
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL1 (and at EL0 unless SCTLR_EL1.UCT is clear, which does not matter here).
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    4 << ((ctr >> 16) & 0xf)
}

/// Cleans and invalidates `r` to the point of coherency.
///
/// With the MMU off our own accesses do not allocate in the data cache, but lines a previous stage left there
/// could later be written back over what we copy, or hold newer data than memory. Called on every destination
/// before and after writing it, and on what the loader handed over (device tree, ramdisk) before reading it.
pub fn dcache_clean_invalidate(r: Range) {
    let line = dcache_line();
    let mut a = r.start & !(line - 1);
    while a < r.end {
        // SAFETY: a data cache maintenance by address has no effect on memory contents that are coherent;
        // for a stale dirty line it writes the line back, which is harmless before we overwrite the range.
        unsafe { asm!("dc civac, {}", in(reg) a, options(nostack)) };
        a += line;
    }
    // SAFETY: barriers only.
    unsafe { asm!("dsb sy", options(nostack)) };
}

pub fn icache_invalidate_all() {
    // SAFETY: invalidating the instruction cache is always allowed at EL1 and only costs refills.
    unsafe { asm!("ic iallu", "dsb sy", "isb", options(nostack)) };
}

/// The bytes at physical address `start` (identity mapping: the MMU is off).
///
/// # Safety
///
/// The range must be RAM that nothing writes while the slice lives.
pub unsafe fn phys(start: u64, len: u64) -> &'static [u8] {
    // SAFETY: by the caller.
    unsafe { core::slice::from_raw_parts(start as usize as *const u8, len as usize) }
}

/// Writable bytes at physical address `start`.
///
/// # Safety
///
/// The range must be RAM that no other live reference covers.
pub unsafe fn phys_mut(start: u64, len: u64) -> &'static mut [u8] {
    // SAFETY: by the caller.
    unsafe { core::slice::from_raw_parts_mut(start as usize as *mut u8, len as usize) }
}

/// The device tree at `pa`: its `totalsize` read from the header, capped at `limit`.
///
/// # Safety
///
/// `pa` must be the address of readable RAM at least 8 bytes long, as the boot protocol promises for `x0`.
pub unsafe fn device_tree(pa: u64, limit: u64) -> Option<&'static [u8]> {
    // Read what the loader wrote, not stale memory: push its dirty lines out first (see dcache_clean_invalidate).
    dcache_clean_invalidate(Range::new(pa, 8)?);
    // SAFETY: by the caller, for the 8-byte header.
    let head = unsafe { phys(pa, 8) };
    if head[..4] != lucina_fdt::MAGIC.to_be_bytes() {
        return None;
    }
    let total = u64::from(u32::from_be_bytes([head[4], head[5], head[6], head[7]]));
    if total > limit {
        return None;
    }
    dcache_clean_invalidate(Range::new(pa, total)?);
    // SAFETY: the loader handed over `totalsize` bytes; they stay untouched (the plan keeps clear of them).
    Some(unsafe { phys(pa, total) })
}

unsafe extern "C" {
    fn lb_restart_at(new_base: u64, old_base: u64, bytes: u64, dtb: u64) -> !;
}

/// Copies the first `file_bytes` of LucinaBoot from `old_base` to `new_base` and starts the copy.
///
/// # Safety
///
/// `[new_base, new_base + whole image)` must be free RAM that does not overlap the running copy.
pub unsafe fn restart_at(new_base: u64, old_base: u64, file_bytes: u64, dtb: u64) -> ! {
    // SAFETY: by the caller; lb_restart_at only touches the two ranges and registers.
    unsafe { lb_restart_at(new_base, old_base, file_bytes, dtb) }
}

/// Jumps to an arm64 kernel image at `entry` with `x0 = dtb`, `x1..x3 = 0`, interrupts masked, MMU still off.
///
/// # Safety
///
/// The image and device tree must be in place and cleaned to the point of coherency.
pub unsafe fn enter_kernel(entry: u64, dtb: u64) -> ! {
    // SAFETY: by the caller.
    unsafe {
        asm!(
            "msr daifset, #0xf",
            "dsb sy",
            "isb",
            "mov x1, xzr",
            "mov x2, xzr",
            "mov x3, xzr",
            "br x4",
            in("x0") dtb,
            in("x4") entry,
            options(noreturn, nostack),
        )
    }
}

/// How PSCI calls reach the firmware, from the device tree's `/psci` `method`.
static PSCI: AtomicU8 = AtomicU8::new(0);
const PSCI_NONE: u8 = 0;
const PSCI_HVC: u8 = 1;
const PSCI_SMC: u8 = 2;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

pub fn psci_init(fdt: &Fdt<'_>) {
    let method = fdt
        .property("/psci", "method")
        .ok()
        .flatten()
        .and_then(|p| p.as_str().ok());
    let conduit = match method {
        Some("hvc") => PSCI_HVC,
        Some("smc") => PSCI_SMC,
        _ => PSCI_NONE,
    };
    // Relaxed loads and stores are plain LDR/STR; nothing here needs exclusives, which Device memory may not
    // support while the MMU is off.
    PSCI.store(conduit, Ordering::Relaxed);
}

/// Counter ticks per second (CNTFRQ_EL0, set by firmware).
fn counter() -> (u64, u64) {
    let (frq, now): (u64, u64);
    // SAFETY: the generic timer registers are readable at EL1.
    unsafe {
        asm!("mrs {}, cntfrq_el0", out(reg) frq, options(nomem, nostack));
        asm!("isb", "mrs {}, cntpct_el0", out(reg) now, options(nomem, nostack));
    }
    (frq, now)
}

/// Microseconds on the generic timer (CNTPCT_EL0 / CNTFRQ_EL0), 0 when firmware left CNTFRQ unset.
pub fn now_us() -> u64 {
    let (frq, now) = counter();
    if frq == 0 {
        return 0;
    }
    (u128::from(now) * 1_000_000 / u128::from(frq)) as u64
}

pub fn delay_ms(ms: u64) {
    let (frq, start) = counter();
    if frq == 0 {
        return;
    }
    let ticks = frq / 1000 * ms;
    while counter().1.wrapping_sub(start) < ticks {}
}

/// Resets the machine through PSCI (QEMU with `--no-reboot` exits; ABL then starts Android again on the
/// phone), or waits for ever when there is no PSCI.
pub fn reset() -> ! {
    let id = PSCI_SYSTEM_RESET;
    // SAFETY: SYSTEM_RESET does not return on success; on failure it returns an error code in x0, which we
    // ignore before parking the CPU.
    unsafe {
        match PSCI.load(Ordering::Relaxed) {
            PSCI_HVC => asm!("hvc #0", inout("x0") id => _, out("x1") _, out("x2") _, out("x3") _),
            PSCI_SMC => asm!("smc #0", inout("x0") id => _, out("x1") _, out("x2") _, out("x3") _),
            _ => {}
        }
    }
    loop {
        // SAFETY: waiting for an event has no side effects.
        unsafe { asm!("wfe", options(nomem, nostack)) };
    }
}
