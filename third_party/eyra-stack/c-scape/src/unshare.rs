//! LucinaOS: `unshare`, which upstream c-scape lacked (LucinaOffice's sandbox and its probe). As musl (`src/linux/unshare.c`):
//! the flags go to the kernel unchanged and its error lands in errno.

use errno::{set_errno, Errno};
use libc::c_int;

#[cfg(target_arch = "aarch64")]
#[no_mangle]
unsafe extern "C" fn unshare(flags: c_int) -> c_int {
    libc!(libc::unshare(flags));

    let ret: isize;
    // SAFETY: unshare(2) takes only the flags.
    core::arch::asm!(
        "svc 0",
        in("x8") 97usize, // __NR_unshare in the generic table AArch64 uses
        inlateout("x0") flags as isize as usize => ret,
        options(nostack),
    );
    if ret < 0 {
        set_errno(Errno(-ret as c_int));
        -1
    } else {
        0
    }
}
