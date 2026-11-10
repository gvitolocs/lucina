//! Where a host listens and whom it answers, shared by both of Juno's hosts (the RustDesk one, `--host --direct`, and this
//! one). Neither encrypts, so both listen only on this laptop's private addresses and take a connection only from the network
//! of the address it reached, or only from the one machine `--only` names. Moved here from lucina-app-desk's `host/direct.rs`
//! unchanged, so that the two cannot drift apart.

use std::net::{IpAddr, Ipv4Addr};

/// One private address of this laptop and its network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lan {
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
}

impl Lan {
    pub fn holds(&self, peer: Ipv4Addr) -> bool {
        let m = u32::from(self.mask);
        u32::from(self.ip) & m == u32::from(peer) & m
    }
}

/// Whether a connection that reached `lan` may go on.
pub fn allowed(peer: IpAddr, lan: Lan, only: Option<IpAddr>) -> bool {
    let IpAddr::V4(v4) = peer else { return false };
    lan.holds(v4) && only.is_none_or(|o| o == peer)
}

/// This machine's private IPv4 addresses. Never a public one, whatever the network gave it.
pub fn lan_addresses() -> Result<Vec<Lan>, String> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: fills in a list that is freed below.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(format!("getifaddrs: {}", std::io::Error::last_os_error()));
    }
    let mut out = Vec::new();
    let mut at = head;
    while !at.is_null() {
        // SAFETY: a node of the list getifaddrs returned, not freed yet. An address of family AF_INET is a sockaddr_in.
        unsafe {
            let i = &*at;
            if !i.ifa_addr.is_null() && !i.ifa_netmask.is_null() && (*i.ifa_addr).sa_family as i32 == libc::AF_INET {
                let ip = Ipv4Addr::from(u32::from_be((*i.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr));
                let mask = Ipv4Addr::from(u32::from_be((*i.ifa_netmask.cast::<libc::sockaddr_in>()).sin_addr.s_addr));
                if ip.is_private() && !out.iter().any(|l: &Lan| l.ip == ip) {
                    out.push(Lan { ip, mask });
                }
            }
            at = i.ifa_next;
        }
    }
    // SAFETY: the list from getifaddrs, once.
    unsafe { libc::freeifaddrs(head) };
    Ok(out)
}

/// Fills `buf` from the kernel's randomness (`getrandom`), as the RustDesk host makes its password.
pub fn random_bytes(buf: &mut [u8]) -> Result<(), String> {
    let mut got = 0;
    while got < buf.len() {
        // SAFETY: writes inside `buf`.
        let n = unsafe { libc::getrandom(buf[got..].as_mut_ptr().cast(), buf.len() - got, 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("getrandom: {e}"));
        }
        got += n as usize;
    }
    Ok(())
}

/// 128 bits from the kernel as 32 hex digits: the browser host's password, as voxl's `new_password` makes it. It goes into a
/// link, so no grouping and nothing a URL would have to escape.
pub fn new_token() -> Result<String, String> {
    let mut raw = [0u8; 16];
    random_bytes(&mut raw)?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: Lan = Lan { ip: Ipv4Addr::new(192, 168, 1, 47), mask: Ipv4Addr::new(255, 255, 255, 0) };

    #[test]
    fn only_this_network_gets_in_and_only_the_named_machine_when_there_is_one() {
        let buildhost: IpAddr = "192.168.1.55".parse().unwrap();
        assert!(allowed(buildhost, HOME, None));
        assert!(!allowed("192.168.2.55".parse().unwrap(), HOME, None));
        assert!(!allowed("8.8.8.8".parse().unwrap(), HOME, None));
        assert!(!allowed("::1".parse().unwrap(), HOME, None));
        assert!(allowed(buildhost, HOME, Some(buildhost)));
        assert!(!allowed("192.168.1.24".parse().unwrap(), HOME, Some(buildhost)));
        // Named, and not on this network: still no.
        let far: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(!allowed(far, HOME, Some(far)));
    }

    #[test]
    fn tokens_are_32_hex_digits_and_new_each_time() {
        let (a, b) = (new_token().unwrap(), new_token().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
    }
}
