//! Exception and interrupt entry from `boot.S`.

use super::{gic, timer};
use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Saved by `exc_entry`: r0-r12, then the address the exception returns to.
#[repr(C)]
pub struct Frame {
    pub r: [u32; 13],
    pub pc: u32,
}

const KINDS: [&str; 8] = ["reset", "undefined", "svc", "prefetch abort", "data abort", "?", "irq", "fiq"];

static EXPECT_UNDEF: AtomicBool = AtomicBool::new(false);
static UNDEF_SEEN: AtomicU32 = AtomicU32::new(0);
static SPURIOUS: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn undef_probe();
}

/// Run one UNDEFINED instruction and check that the handler saw it and resumed after it.
pub fn self_test() -> bool {
    EXPECT_UNDEF.store(true, Ordering::SeqCst);
    // SAFETY: undef_probe is `udf #0; bx lr`; the handler skips the udf while EXPECT_UNDEF is set.
    unsafe { undef_probe() };
    !EXPECT_UNDEF.load(Ordering::SeqCst) && UNDEF_SEEN.load(Ordering::SeqCst) == 1
}

fn dfsr_dfar() -> (u32, u32) {
    let (s, a): (u32, u32);
    // SAFETY: reading the fault status/address registers has no side effects.
    unsafe {
        asm!("mrc p15, 0, {}, c5, c0, 0", out(reg) s, options(nomem, nostack));
        asm!("mrc p15, 0, {}, c6, c0, 0", out(reg) a, options(nomem, nostack));
    }
    (s, a)
}

fn ifsr_ifar() -> (u32, u32) {
    let (s, a): (u32, u32);
    // SAFETY: as above.
    unsafe {
        asm!("mrc p15, 0, {}, c5, c0, 1", out(reg) s, options(nomem, nostack));
        asm!("mrc p15, 0, {}, c6, c0, 2", out(reg) a, options(nomem, nostack));
    }
    (s, a)
}

/// Returns the address to resume at; fatal exceptions do not return.
#[unsafe(no_mangle)]
extern "C" fn rust_exception(kind: u32, spsr: u32, frame: &mut Frame) -> u32 {
    if kind == 1 && EXPECT_UNDEF.swap(false, Ordering::SeqCst) {
        UNDEF_SEEN.fetch_add(1, Ordering::SeqCst);
        return frame.pc + 4;
    }
    let name = KINDS.get(kind as usize).copied().unwrap_or("?");
    crate::println!("\n*** {} exception at pc {:#010x}, spsr {:#010x}", name, frame.pc, spsr);
    match kind {
        3 => {
            let (s, a) = ifsr_ifar();
            crate::println!("    IFSR {:#x} IFAR {:#010x}", s, a);
        }
        4 => {
            let (s, a) = dfsr_dfar();
            crate::println!("    DFSR {:#x} DFAR {:#010x}", s, a);
        }
        _ => {}
    }
    for (i, r) in frame.r.iter().enumerate() {
        crate::print!("    r{:<2} {:#010x}", i, r);
        if i % 4 == 3 {
            crate::println!();
        }
    }
    crate::println!();
    crate::fatal("cpu exception")
}

#[unsafe(no_mangle)]
extern "C" fn rust_irq() {
    loop {
        let iar = gic::ack();
        let irq = iar & 0x3ff;
        if irq >= gic::SPURIOUS - 3 {
            if irq == gic::SPURIOUS {
                SPURIOUS.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        match irq {
            timer::IRQ => {
                timer::handle_irq();
                crate::drivers::wdog::service();
            }
            other => {
                crate::println!("irq {}: no handler, masked", other);
                gic::disable(other);
            }
        }
        gic::eoi(iar);
    }
}
