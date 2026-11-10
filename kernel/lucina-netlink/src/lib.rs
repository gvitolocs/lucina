//! lucina-netlink: `getifaddrs` as musl 1.2.5 builds it (`src/network/getifaddrs.c`, `netlink.c`), for Juno's programs with
//! no C library (Eyra's c-gull hands this list out as `struct ifaddrs`). Upstream c-gull had no `getifaddrs`.
//!
//! One NETLINK_ROUTE socket, a dump of the links (`RTM_GETLINK`, seq 1) and then of the addresses (`RTM_GETADDR`, seq 2),
//! each read with `MSG_DONTWAIT` until `NLMSG_DONE`; `NLMSG_ERROR` or nothing to read fails the whole call, as in musl.
//! - A link gives one entry with an `AF_PACKET` address (`IFLA_ADDRESS`, `IFLA_BROADCAST`, at most 8 bytes), its index, name
//!   (`IFLA_IFNAME`, under 17 bytes) and flags. A link without a name is left out.
//! - An address of a known link gives one entry with that link's name and flags (or its own `IFA_LABEL`), `IFA_ADDRESS`, or
//!   `IFA_LOCAL` with `IFA_ADDRESS` as the other end of a point-to-point link, `IFA_BROADCAST`, and the netmask from the
//!   prefix length. IPv6 link-local and multicast link-local addresses carry the link's index as their scope.
//! - Entries keep the order the kernel sent them in. musl also copies each link's `IFLA_STATS` into `ifa_data`; nothing on
//!   Juno reads it, and this leaves it out.
//!
//! The message and attribute walking is [`walk`], proved by Verus (`verification/juno-netlink`).

pub mod walk;

use walk::{align4, attribute, le16, le32, message};

/// An address as `struct ifaddrs` carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SockAddr {
    V4([u8; 4]),
    V6 { addr: [u8; 16], scope_id: u32 },
    /// `sockaddr_ll`: the link's index, its hardware type, and the hardware address.
    Packet { ifindex: i32, hatype: u16, addr: Vec<u8> },
}

/// One `struct ifaddrs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Vec<u8>,
    pub flags: u32,
    pub addr: Option<SockAddr>,
    pub netmask: Option<SockAddr>,
    pub broadaddr: Option<SockAddr>,
    pub dstaddr: Option<SockAddr>,
}

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;
/// `IFNAMSIZ + 1`, musl's name buffer.
const NAME_ROOM: usize = 17;
const AF_INET: u8 = 2;
const AF_INET6: u8 = 10;

/// What the dumps have given so far: the named links by index, and the entries in order.
#[derive(Default)]
pub struct Builder {
    links: Vec<(i32, Vec<u8>, u32)>,
    pub entries: Vec<Entry>,
}

/// What one batch of received bytes said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    More,
    Done,
    Failed,
}

impl Builder {
    /// Takes every message in `buf[..len]` (musl's loop over `NLMSG_OK` / `NLMSG_NEXT`).
    pub fn take(&mut self, buf: &[u8], len: usize) -> Step {
        let len = len.min(buf.len());
        let mut pos = 0;
        while let Some((n, kind)) = message(buf, pos, len) {
            match kind {
                NLMSG_DONE => return Step::Done,
                NLMSG_ERROR => return Step::Failed,
                RTM_NEWLINK => self.link(&buf[pos..pos + n]),
                RTM_NEWADDR => self.address(&buf[pos..pos + n]),
                _ => {}
            }
            match align4(n).and_then(|a| pos.checked_add(a)) {
                Some(next) => pos = next,
                None => break,
            }
        }
        Step::More
    }

    /// The attributes of a message whose fixed part (after the 16-byte header) is `fixed` bytes: (type, data).
    fn attributes(msg: &[u8], fixed: usize) -> Vec<(u16, &[u8])> {
        let mut out = Vec::new();
        let Some(start) = align4(fixed).and_then(|f| f.checked_add(16)) else { return out };
        let mut pos = start;
        while let Some((n, kind)) = attribute(msg, pos, msg.len()) {
            out.push((kind, &msg[pos + 4..pos + n]));
            match align4(n).and_then(|a| pos.checked_add(a)) {
                Some(next) => pos = next,
                None => break,
            }
        }
        out
    }

    fn link(&mut self, msg: &[u8]) {
        // struct ifinfomsg: family, pad, type (u16), index (i32), flags (u32), change (u32).
        if msg.len() < 16 + 16 {
            return;
        }
        let hatype = le16(msg, 18);
        let index = le32(msg, 20) as i32;
        let flags = le32(msg, 24);
        let mut e = Entry { name: Vec::new(), flags, addr: None, netmask: None, broadaddr: None, dstaddr: None };
        let mut named = false;
        for (kind, data) in Self::attributes(msg, 16) {
            match kind {
                IFLA_IFNAME if data.len() < NAME_ROOM => {
                    e.name = c_string(data);
                    named = true;
                }
                IFLA_ADDRESS => {
                    if let Some(a) = lladdr(data, index, hatype) {
                        e.addr = Some(a);
                    }
                }
                IFLA_BROADCAST => {
                    if let Some(a) = lladdr(data, index, hatype) {
                        e.broadaddr = Some(a);
                    }
                }
                _ => {}
            }
        }
        if named {
            self.links.push((index, e.name.clone(), flags));
            self.entries.push(e);
        }
    }

    fn address(&mut self, msg: &[u8]) {
        // struct ifaddrmsg: family, prefixlen, flags, scope (u8 each), index (u32).
        if msg.len() < 16 + 8 {
            return;
        }
        let family = msg[16];
        let prefixlen = msg[17];
        let index = le32(msg, 20) as i32;
        let Some((_, name, flags)) = self.links.iter().find(|(i, _, _)| *i == index) else { return };
        let mut e = Entry { name: name.clone(), flags: *flags, addr: None, netmask: None, broadaddr: None, dstaddr: None };
        for (kind, data) in Self::attributes(msg, 8) {
            match kind {
                IFA_ADDRESS => {
                    // After an IFA_LOCAL, IFA_ADDRESS is the other end.
                    if e.addr.is_some() {
                        set(&mut e.dstaddr, family, data, index);
                    } else {
                        set(&mut e.addr, family, data, index);
                    }
                }
                IFA_BROADCAST => set(&mut e.broadaddr, family, data, index),
                IFA_LOCAL => {
                    // IFA_LOCAL after IFA_ADDRESS: a point-to-point link; what came first is the other end.
                    if e.addr.is_some() {
                        e.dstaddr = e.addr.take();
                    }
                    set(&mut e.addr, family, data, index);
                }
                IFA_LABEL if data.len() < NAME_ROOM => e.name = c_string(data),
                _ => {}
            }
        }
        if e.addr.is_some() {
            e.netmask = netmask(family, prefixlen);
        }
        self.entries.push(e);
    }
}

/// The bytes before the first NUL.
fn c_string(data: &[u8]) -> Vec<u8> {
    data.iter().take_while(|&&b| b != 0).copied().collect()
}

/// musl's `copy_lladdr`: at most 8 bytes of hardware address.
fn lladdr(data: &[u8], ifindex: i32, hatype: u16) -> Option<SockAddr> {
    if data.len() > 8 {
        return None;
    }
    Some(SockAddr::Packet { ifindex, hatype, addr: data.to_vec() })
}

/// musl's `copy_addr`: an IPv4 or IPv6 address of the family's length, or nothing.
fn ip(family: u8, data: &[u8], ifindex: i32) -> Option<SockAddr> {
    match family {
        AF_INET if data.len() >= 4 => Some(SockAddr::V4([data[0], data[1], data[2], data[3]])),
        AF_INET6 if data.len() >= 16 => {
            let mut addr = [0u8; 16];
            addr.copy_from_slice(&data[..16]);
            // IN6_IS_ADDR_LINKLOCAL (fe80::/10) and IN6_IS_ADDR_MC_LINKLOCAL (ff02::/16).
            let link_local = (addr[0] == 0xfe && (addr[1] & 0xc0) == 0x80) || (addr[0] == 0xff && (addr[1] & 0x0f) == 0x02);
            Some(SockAddr::V6 { addr, scope_id: if link_local { ifindex as u32 } else { 0 } })
        }
        _ => None,
    }
}

fn set(slot: &mut Option<SockAddr>, family: u8, data: &[u8], ifindex: i32) {
    if let Some(a) = ip(family, data, ifindex) {
        *slot = Some(a);
    }
}

/// musl's `gen_netmask`: `prefixlen` one bits, capped at the address's length.
pub fn netmask(family: u8, prefixlen: u8) -> Option<SockAddr> {
    let len = match family {
        AF_INET => 4,
        AF_INET6 => 16,
        _ => return None,
    };
    let bits = (prefixlen as usize).min(8 * len);
    let mut m = [0u8; 16];
    for b in m.iter_mut().take(bits / 8) {
        *b = 0xff;
    }
    if bits / 8 < len && bits % 8 != 0 {
        m[bits / 8] = 0xffu8 << (8 - bits % 8);
    }
    ip(family, &m, 0)
}

/// The request musl sends: a 16-byte header and a `struct rtgenmsg`, padded to 20 bytes.
pub fn request(kind: u16, seq: u32) -> [u8; 20] {
    let mut r = [0u8; 20];
    r[0..4].copy_from_slice(&20u32.to_le_bytes());
    r[4..6].copy_from_slice(&kind.to_le_bytes());
    r[6..8].copy_from_slice(&0x0301u16.to_le_bytes()); // NLM_F_DUMP | NLM_F_REQUEST
    r[8..12].copy_from_slice(&seq.to_le_bytes());
    // rtgen_family 0: AF_UNSPEC.
    r
}

/// Every interface and address, as `getifaddrs` lists them. `Err` with the errno when the kernel refused or said nothing.
pub fn interfaces() -> Result<Vec<Entry>, i32> {
    // SAFETY: plain socket calls on a descriptor this function owns and closes; the buffer outlives every recv.
    unsafe {
        let fd = libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_CLOEXEC, libc::NETLINK_ROUTE);
        if fd < 0 {
            return Err(errno());
        }
        let mut b = Builder::default();
        let mut buf = vec![0u8; 8192];
        let mut result = Ok(());
        for (seq, kind) in [(1u32, RTM_GETLINK), (2, RTM_GETADDR)] {
            let req = request(kind, seq);
            if libc::send(fd, req.as_ptr().cast(), req.len(), 0) < 0 {
                result = Err(errno());
                break;
            }
            loop {
                let r = libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT);
                if r <= 0 {
                    result = Err(if r < 0 { errno() } else { libc::EIO });
                    break;
                }
                match b.take(&buf, r as usize) {
                    Step::More => continue,
                    Step::Done => break,
                    Step::Failed => {
                        result = Err(libc::EIO);
                        break;
                    }
                }
            }
            if result.is_err() {
                break;
            }
        }
        libc::close(fd);
        result.map(|()| b.entries)
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO)
}
