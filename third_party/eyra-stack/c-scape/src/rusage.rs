//! LucinaOS: `getrusage`, which upstream c-scape lacked (rav1d asks it, through lucina-app-video and lucina-app-desk).
//! As musl on 64-bit (`src/misc/getrusage.c`): the kernel's `struct rusage` is the C library's, so the call goes straight
//! through and the kernel's error lands in errno.

use errno::{set_errno, Errno};
use libc::c_int;

#[cfg(target_arch = "aarch64")]
#[no_mangle]
unsafe extern "C" fn getrusage(who: c_int, usage: *mut libc::rusage) -> c_int {
    libc!(libc::getrusage(who, usage));

    let ret: isize;
    // SAFETY: getrusage(2) writes one struct rusage where the caller said, as with any C library's getrusage.
    core::arch::asm!(
        "svc 0",
        in("x8") 165usize, // __NR_getrusage in the generic table AArch64 uses
        inlateout("x0") who as isize as usize => ret,
        in("x1") usage as usize,
        options(nostack),
    );
    if ret < 0 {
        set_errno(Errno(-ret as c_int));
        -1
    } else {
        0
    }
}
