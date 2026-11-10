//! LucinaOS: one system call with up to six arguments, as musl's `__syscall_ret` hands it back: a negative answer becomes
//! errno and -1. Used where musl calls the kernel straight and upstream c-scape panicked instead (unknown `syscall` numbers,
//! clocks, `prctl` operations, socket options and flags).

use errno::{set_errno, Errno};
use libc::c_int;

#[cfg(target_arch = "aarch64")]
#[inline]
pub(crate) unsafe fn sys(nr: usize, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize) -> isize {
    let ret: isize;
    // SAFETY: the caller passes what its C function was given, as any C library does.
    core::arch::asm!(
        "svc 0",
        in("x8") nr,
        inlateout("x0") a => ret,
        in("x1") b,
        in("x2") c,
        in("x3") d,
        in("x4") e,
        in("x5") f,
        options(nostack),
    );
    if ret < 0 {
        set_errno(Errno(-ret as c_int));
        -1
    } else {
        ret
    }
}
