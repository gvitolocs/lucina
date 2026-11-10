//! LucinaOS: `mount` and `umount2`, which upstream c-scape lacked (lucina-init, Juno's PID 1 to be, mounts and unmounts).
//!
//! As musl (`src/linux/mount.c`): the five arguments go to the kernel unchanged, NULLs included (a remount, a bind or a
//! propagation change passes no source or type), and the kernel's error lands in errno.

use core::ffi::CStr;
use errno::{set_errno, Errno};
use libc::{c_char, c_int, c_ulong, c_void};

use crate::convert_res;

#[cfg(target_arch = "aarch64")]
#[no_mangle]
unsafe extern "C" fn mount(
    source: *const c_char,
    target: *const c_char,
    fstype: *const c_char,
    flags: c_ulong,
    data: *const c_void,
) -> c_int {
    libc!(libc::mount(source, target, fstype, flags, data));

    let ret: isize;
    // SAFETY: mount(2) reads the strings and the data the caller passed, as with any C library's mount.
    core::arch::asm!(
        "svc 0",
        in("x8") 40usize, // __NR_mount in the generic table AArch64 uses
        inlateout("x0") source as usize => ret,
        in("x1") target as usize,
        in("x2") fstype as usize,
        in("x3") flags as usize,
        in("x4") data as usize,
        options(nostack),
    );
    if ret < 0 {
        set_errno(Errno(-ret as c_int));
        -1
    } else {
        0
    }
}

#[no_mangle]
unsafe extern "C" fn umount2(target: *const c_char, flags: c_int) -> c_int {
    libc!(libc::umount2(target, flags));

    let flags = rustix::mount::UnmountFlags::from_bits_retain(flags as _);
    match convert_res(rustix::mount::unmount(CStr::from_ptr(target), flags)) {
        Some(()) => 0,
        None => -1,
    }
}

#[no_mangle]
unsafe extern "C" fn umount(target: *const c_char) -> c_int {
    libc!(libc::umount(target));

    umount2(target, 0)
}
