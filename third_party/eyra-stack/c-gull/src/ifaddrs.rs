//! LucinaOS: `getifaddrs` and `freeifaddrs`, which upstream c-gull lacked (lucina-desk-web lists the laptop's addresses with
//! them). The list is musl's (kernel/lucina-netlink); as in musl, each entry is one allocation holding its `struct ifaddrs`,
//! its addresses and its name, and `freeifaddrs` frees them one by one.

use alloc::boxed::Box;
use core::mem::zeroed;
use core::ptr::null_mut;
use errno::{set_errno, Errno};
use libc::{c_char, c_int};
use lucina_netlink::SockAddr;

#[repr(C)]
struct Node {
    /// First, so that a `*mut ifaddrs` of the list is the node's address.
    ifa: libc::ifaddrs,
    addr: libc::sockaddr_storage,
    netmask: libc::sockaddr_storage,
    ifu: libc::sockaddr_storage,
    name: [c_char; 17],
}

/// Writes `sa` into `out` and returns it as a `struct sockaddr`.
unsafe fn fill(sa: &SockAddr, out: &mut libc::sockaddr_storage) -> *mut libc::sockaddr {
    let p = (out as *mut libc::sockaddr_storage).cast::<u8>();
    match sa {
        SockAddr::V4(b) => {
            let s = &mut *p.cast::<libc::sockaddr_in>();
            s.sin_family = libc::AF_INET as libc::sa_family_t;
            s.sin_addr.s_addr = u32::from_ne_bytes(*b);
        }
        SockAddr::V6 { addr, scope_id } => {
            let s = &mut *p.cast::<libc::sockaddr_in6>();
            s.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            s.sin6_addr.s6_addr = *addr;
            s.sin6_scope_id = *scope_id;
        }
        SockAddr::Packet { ifindex, hatype, addr } => {
            let s = &mut *p.cast::<libc::sockaddr_ll>();
            s.sll_family = libc::AF_PACKET as u16;
            s.sll_ifindex = *ifindex;
            s.sll_hatype = *hatype;
            let n = addr.len().min(s.sll_addr.len());
            s.sll_halen = n as u8;
            s.sll_addr[..n].copy_from_slice(&addr[..n]);
        }
    }
    p.cast()
}

#[no_mangle]
unsafe extern "C" fn getifaddrs(ifap: *mut *mut libc::ifaddrs) -> c_int {
    libc!(libc::getifaddrs(ifap));

    let entries = match lucina_netlink::interfaces() {
        Ok(e) => e,
        Err(e) => {
            set_errno(Errno(e));
            return -1;
        }
    };
    let mut first: *mut libc::ifaddrs = null_mut();
    let mut last: *mut Node = null_mut();
    for e in &entries {
        let node = Box::into_raw(Box::new(zeroed::<Node>()));
        let n = &mut *node;
        let len = e.name.len().min(n.name.len() - 1);
        for (d, s) in n.name.iter_mut().zip(&e.name[..len]) {
            *d = *s as c_char;
        }
        n.ifa.ifa_name = n.name.as_mut_ptr();
        n.ifa.ifa_flags = e.flags;
        if let Some(a) = &e.addr {
            n.ifa.ifa_addr = fill(a, &mut n.addr);
        }
        if let Some(a) = &e.netmask {
            n.ifa.ifa_netmask = fill(a, &mut n.netmask);
        }
        // One union in C: the other end of a point-to-point link, or the broadcast address.
        if let Some(a) = e.dstaddr.as_ref().or(e.broadaddr.as_ref()) {
            n.ifa.ifa_ifu = fill(a, &mut n.ifu);
        }
        if last.is_null() {
            first = &mut n.ifa;
        } else {
            (*last).ifa.ifa_next = &mut n.ifa;
        }
        last = node;
    }
    *ifap = first;
    0
}

#[no_mangle]
unsafe extern "C" fn freeifaddrs(ifa: *mut libc::ifaddrs) {
    libc!(libc::freeifaddrs(ifa));

    let mut at = ifa;
    while !at.is_null() {
        let next = (*at).ifa_next;
        drop(Box::from_raw(at.cast::<Node>()));
        at = next;
    }
}
