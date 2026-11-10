#[cfg(not(target_os = "wasi"))]
mod dup;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod epoll;
mod isatty;
mod pipe;
mod poll;
mod read;
mod select;
mod splice;
#[cfg(any(target_os = "android", target_os = "linux"))]
mod timerfd;
mod write;

use rustix::event::EventfdFlags;
use rustix::fd::{BorrowedFd, IntoRawFd};

use libc::{c_int, c_long, c_uint};

use crate::convert_res;

#[cfg(not(target_os = "wasi"))]
#[no_mangle]
unsafe extern "C" fn ioctl(fd: c_int, request: c_long, mut args: ...) -> c_int {
    // LucinaOS: every request goes to the kernel as it is, with its one argument, and the kernel's non-negative answer comes
    // back unchanged, as glibc and musl do on 64-bit Linux (musl `src/misc/ioctl.c`). Upstream c-scape knew five requests,
    // panicked on every other one (the GPU's, Venus', evdev's, the framebuffer's, the watchdog's) and took TIOCINQ, which is
    // FIONREAD, for FIONBIO. Reading the argument when the caller passed none is what musl does too: on AArch64 it reads a
    // register or a stack slot that the call does not use.
    let arg = args.next_arg::<usize>();
    libc!(libc::ioctl(fd, request as _, arg));
    let fd = BorrowedFd::borrow_raw(fd);
    match convert_res(rustix::ioctl::ioctl(fd, Raw { request, arg })) {
        Some(r) => r,
        None => -1,
    }
}

/// One ioctl as the caller gave it: the request number and the argument, a pointer or a number.
#[cfg(not(target_os = "wasi"))]
struct Raw {
    request: c_long,
    arg: usize,
}

// SAFETY: the caller of `ioctl` vouches for the request and its argument, as with any C library's ioctl; the kernel may write
// through the argument, so this counts as mutating.
#[cfg(not(target_os = "wasi"))]
unsafe impl rustix::ioctl::Ioctl for Raw {
    type Output = c_int;
    const IS_MUTATING: bool = true;

    fn opcode(&self) -> rustix::ioctl::Opcode {
        self.request as rustix::ioctl::Opcode
    }

    fn as_ptr(&mut self) -> *mut core::ffi::c_void {
        core::ptr::with_exposed_provenance_mut(self.arg)
    }

    unsafe fn output_from_ptr(out: rustix::ioctl::IoctlOutput, _: *mut core::ffi::c_void) -> rustix::io::Result<c_int> {
        Ok(out)
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
#[no_mangle]
unsafe extern "C" fn eventfd(initval: c_uint, flags: c_int) -> c_int {
    libc!(libc::eventfd(initval, flags));
    let flags = EventfdFlags::from_bits(flags.try_into().unwrap()).unwrap();
    match convert_res(rustix::event::eventfd(initval, flags)) {
        Some(fd) => fd.into_raw_fd(),
        None => -1,
    }
}
