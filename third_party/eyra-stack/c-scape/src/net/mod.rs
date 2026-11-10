//! LucinaOS: the socket calls as musl makes them (`src/network/*.c`): each one goes to the kernel with its arguments as they
//! are, and the kernel's error lands in errno. Upstream c-scape converted every argument into rustix's types first, and
//! panicked on anything it did not know: a socket option outside its list (`getsockopt`/`setsockopt`), a flag bit it had no
//! name for (`recv`, `send` and the rest unwrapped `from_bits`), ancillary data (`sendmsg`/`recvmsg`: SCM_RIGHTS, which
//! passes descriptors between Juno's programs), a `shutdown` kind; and it changed the length of some AF_UNIX addresses. A
//! panic there ends the program ("non-unwinding panic", seen in lucina-timesync on QEMU).
//!
//! On AArch64 the C library's `struct msghdr`, `sockaddr` and option layouts are the kernel's, so nothing is translated.

mod inet;

use core::ffi::c_void;
use libc::{c_int, ssize_t};

use crate::lucina_raw::sys;

// The generic system call numbers AArch64 uses (include/uapi/asm-generic/unistd.h).
const SOCKET: usize = 198;
const SOCKETPAIR: usize = 199;
const BIND: usize = 200;
const LISTEN: usize = 201;
const ACCEPT: usize = 202;
const CONNECT: usize = 203;
const GETSOCKNAME: usize = 204;
const GETPEERNAME: usize = 205;
const SENDTO: usize = 206;
const RECVFROM: usize = 207;
const SETSOCKOPT: usize = 208;
const GETSOCKOPT: usize = 209;
const SHUTDOWN: usize = 210;
const SENDMSG: usize = 211;
const RECVMSG: usize = 212;
const ACCEPT4: usize = 242;

#[inline]
fn i(v: c_int) -> usize {
    v as isize as usize
}

#[no_mangle]
unsafe extern "C" fn accept(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int {
    libc!(libc::accept(fd, addr, len));
    sys(ACCEPT, i(fd), addr as usize, len as usize, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn accept4(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t, flags: c_int) -> c_int {
    libc!(libc::accept4(fd, addr, len, flags));
    sys(ACCEPT4, i(fd), addr as usize, len as usize, i(flags), 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn bind(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int {
    libc!(libc::bind(fd, addr, len));
    sys(BIND, i(fd), addr as usize, len as usize, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn connect(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int {
    libc!(libc::connect(fd, addr, len));
    sys(CONNECT, i(fd), addr as usize, len as usize, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn getpeername(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int {
    libc!(libc::getpeername(fd, addr, len));
    sys(GETPEERNAME, i(fd), addr as usize, len as usize, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn getsockname(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int {
    libc!(libc::getsockname(fd, addr, len));
    sys(GETSOCKNAME, i(fd), addr as usize, len as usize, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn getsockopt(
    fd: c_int,
    level: c_int,
    optname: c_int,
    optval: *mut c_void,
    optlen: *mut libc::socklen_t,
) -> c_int {
    libc!(libc::getsockopt(fd, level, optname, optval, optlen));
    sys(GETSOCKOPT, i(fd), i(level), i(optname), optval as usize, optlen as usize, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn setsockopt(
    fd: c_int,
    level: c_int,
    optname: c_int,
    optval: *const c_void,
    optlen: libc::socklen_t,
) -> c_int {
    libc!(libc::setsockopt(fd, level, optname, optval, optlen));
    sys(SETSOCKOPT, i(fd), i(level), i(optname), optval as usize, optlen as usize, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn listen(fd: c_int, backlog: c_int) -> c_int {
    libc!(libc::listen(fd, backlog));
    sys(LISTEN, i(fd), i(backlog), 0, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn recv(fd: c_int, ptr: *mut c_void, len: usize, flags: c_int) -> ssize_t {
    libc!(libc::recv(fd, ptr, len, flags));
    sys(RECVFROM, i(fd), ptr as usize, len, i(flags), 0, 0)
}

#[no_mangle]
unsafe extern "C" fn recvfrom(
    fd: c_int,
    buf: *mut c_void,
    len: usize,
    flags: c_int,
    from: *mut libc::sockaddr,
    from_len: *mut libc::socklen_t,
) -> ssize_t {
    libc!(libc::recvfrom(fd, buf, len, flags, from, from_len));
    sys(RECVFROM, i(fd), buf as usize, len, i(flags), from as usize, from_len as usize)
}

#[no_mangle]
unsafe extern "C" fn recvmsg(sockfd: c_int, msg: *mut libc::msghdr, flags: c_int) -> ssize_t {
    libc!(libc::recvmsg(sockfd, msg, flags));
    sys(RECVMSG, i(sockfd), msg as usize, i(flags), 0, 0, 0)
}

#[no_mangle]
unsafe extern "C" fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> ssize_t {
    libc!(libc::send(fd, buf, len, flags));
    sys(SENDTO, i(fd), buf as usize, len, i(flags), 0, 0)
}

#[no_mangle]
unsafe extern "C" fn sendto(
    fd: c_int,
    buf: *const c_void,
    len: usize,
    flags: c_int,
    to: *const libc::sockaddr,
    to_len: libc::socklen_t,
) -> ssize_t {
    libc!(libc::sendto(fd, buf, len, flags, to, to_len));
    sys(SENDTO, i(fd), buf as usize, len, i(flags), to as usize, to_len as usize)
}

#[no_mangle]
unsafe extern "C" fn sendmsg(sockfd: c_int, msg: *const libc::msghdr, flags: c_int) -> ssize_t {
    libc!(libc::sendmsg(sockfd, msg, flags));
    sys(SENDMSG, i(sockfd), msg as usize, i(flags), 0, 0, 0)
}

#[no_mangle]
unsafe extern "C" fn shutdown(fd: c_int, how: c_int) -> c_int {
    libc!(libc::shutdown(fd, how));
    sys(SHUTDOWN, i(fd), i(how), 0, 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    libc!(libc::socket(domain, type_, protocol));
    sys(SOCKET, i(domain), i(type_), i(protocol), 0, 0, 0) as c_int
}

#[no_mangle]
unsafe extern "C" fn socketpair(domain: c_int, type_: c_int, protocol: c_int, sv: *mut [c_int; 2]) -> c_int {
    libc!(libc::socketpair(domain, type_, protocol, (*sv).as_mut_ptr()));
    sys(SOCKETPAIR, i(domain), i(type_), i(protocol), sv as usize, 0, 0) as c_int
}

#[allow(non_upper_case_globals)]
#[no_mangle]
static in6addr_any: libc::in6_addr = libc::in6_addr {
    s6_addr: { [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
};
#[allow(non_upper_case_globals)]
#[no_mangle]
static in6addr_loopback: libc::in6_addr = libc::in6_addr {
    s6_addr: { [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1] },
};
