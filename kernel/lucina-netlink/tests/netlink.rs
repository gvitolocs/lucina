//! lucina-netlink against the original: on the build host, the C library's own `getifaddrs` over the same kernel is the
//! oracle; hand-made messages cover what a host may not have (a point-to-point link, a label, cut-short messages).

use lucina_netlink::{Builder, SockAddr, Step, interfaces, netmask};

/// What the host's C library lists: (name, family, address bytes, netmask bytes), AF_INET, AF_INET6 and AF_PACKET.
fn host_list() -> Vec<(Vec<u8>, i32, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    // SAFETY: walks the list the host's getifaddrs returns and frees it once.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        assert_eq!(libc::getifaddrs(&mut head), 0);
        let mut at = head;
        while !at.is_null() {
            let i = &*at;
            let name = std::ffi::CStr::from_ptr(i.ifa_name).to_bytes().to_vec();
            let bytes = |sa: *mut libc::sockaddr| -> (i32, Vec<u8>) {
                if sa.is_null() {
                    return (-1, vec![]);
                }
                match (*sa).sa_family as i32 {
                    libc::AF_INET => (libc::AF_INET, (*sa.cast::<libc::sockaddr_in>()).sin_addr.s_addr.to_ne_bytes().to_vec()),
                    libc::AF_INET6 => (libc::AF_INET6, (*sa.cast::<libc::sockaddr_in6>()).sin6_addr.s6_addr.to_vec()),
                    libc::AF_PACKET => {
                        let ll = &*sa.cast::<libc::sockaddr_ll>();
                        (libc::AF_PACKET, ll.sll_addr[..ll.sll_halen as usize].to_vec())
                    }
                    f => (f, vec![]),
                }
            };
            let (family, addr) = bytes(i.ifa_addr);
            let (_, mask) = bytes(i.ifa_netmask);
            out.push((name, family, addr, mask));
            at = i.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out.sort();
    out
}

fn ours_list() -> Vec<(Vec<u8>, i32, Vec<u8>, Vec<u8>)> {
    let bytes = |a: &Option<SockAddr>| -> (i32, Vec<u8>) {
        match a {
            None => (-1, vec![]),
            Some(SockAddr::V4(b)) => (libc::AF_INET, b.to_vec()),
            Some(SockAddr::V6 { addr, .. }) => (libc::AF_INET6, addr.to_vec()),
            Some(SockAddr::Packet { addr, .. }) => (libc::AF_PACKET, addr.clone()),
        }
    };
    let mut out: Vec<_> = interfaces()
        .unwrap()
        .iter()
        .map(|e| {
            let (family, addr) = bytes(&e.addr);
            (e.name.clone(), family, addr, bytes(&e.netmask).1)
        })
        .collect();
    out.sort();
    out
}

#[test]
fn same_list_as_the_hosts_c_library() {
    let host = host_list();
    let ours = ours_list();
    assert!(host.iter().any(|(_, f, _, _)| *f == libc::AF_INET), "the host has an IPv4 address: {host:?}");
    assert_eq!(ours, host);
}

/// A netlink message: header with `kind`, then `fixed`, then (type, data) attributes, each padded to 4.
fn msg(kind: u16, fixed: &[u8], attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut m = vec![0u8; 16];
    m.extend_from_slice(fixed);
    while m.len() % 4 != 0 {
        m.push(0);
    }
    for (t, d) in attrs {
        m.extend_from_slice(&((4 + d.len()) as u16).to_le_bytes());
        m.extend_from_slice(&t.to_le_bytes());
        m.extend_from_slice(d);
        while m.len() % 4 != 0 {
            m.push(0);
        }
    }
    let n = m.len() as u32;
    m[0..4].copy_from_slice(&n.to_le_bytes());
    m[4..6].copy_from_slice(&kind.to_le_bytes());
    m
}

fn ifinfo(index: i32, hatype: u16, flags: u32) -> Vec<u8> {
    let mut f = vec![0u8, 0];
    f.extend_from_slice(&hatype.to_le_bytes());
    f.extend_from_slice(&index.to_le_bytes());
    f.extend_from_slice(&flags.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f
}

fn ifaddr(family: u8, prefixlen: u8, index: u32) -> Vec<u8> {
    let mut f = vec![family, prefixlen, 0, 0];
    f.extend_from_slice(&index.to_le_bytes());
    f
}

#[test]
fn links_addresses_point_to_point_and_labels() {
    let mut buf = Vec::new();
    buf.extend(msg(16, &ifinfo(2, 1, 0x1043), &[(3, b"wlan0\0"), (1, &[2, 0, 0, 0, 0, 7]), (2, &[0xff; 6])]));
    // A link with no name: left out.
    buf.extend(msg(16, &ifinfo(3, 1, 0), &[(1, &[1, 2, 3, 4, 5, 6])]));
    buf.extend(msg(16, &ifinfo(4, 65534, 0x11), &[(3, b"ppp0\0")]));
    buf.extend(msg(20, &ifaddr(2, 24, 2), &[(1, &[192, 168, 1, 47]), (2, &[192, 168, 1, 47]), (4, &[192, 168, 1, 255])]));
    // Point to point: IFA_ADDRESS (the other end) then IFA_LOCAL (ours).
    buf.extend(msg(20, &ifaddr(2, 32, 4), &[(1, &[10, 0, 0, 1]), (2, &[10, 0, 0, 2]), (3, b"ppp0:x\0")]));
    // An address of an unknown link: left out. IPv6 link-local: scope is the index.
    buf.extend(msg(20, &ifaddr(2, 8, 9), &[(1, &[1, 1, 1, 1])]));
    let ll: [u8; 16] = [0xfe, 0x80, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
    buf.extend(msg(20, &ifaddr(10, 64, 2), &[(1, &ll)]));
    let mut b = Builder::default();
    assert_eq!(b.take(&buf, buf.len()), Step::More);
    let done = msg(3, &[0, 0, 0, 0], &[]);
    assert_eq!(b.take(&done, done.len()), Step::Done);
    let e = &b.entries;
    assert_eq!(e.len(), 5);
    assert_eq!(e[0].name, b"wlan0");
    assert_eq!(e[0].addr, Some(SockAddr::Packet { ifindex: 2, hatype: 1, addr: vec![2, 0, 0, 0, 0, 7] }));
    assert_eq!(e[1].name, b"ppp0");
    assert_eq!((e[2].name.as_slice(), e[2].flags), (&b"wlan0"[..], 0x1043));
    assert_eq!(e[2].addr, Some(SockAddr::V4([192, 168, 1, 47])));
    assert_eq!(e[2].netmask, Some(SockAddr::V4([255, 255, 255, 0])));
    assert_eq!(e[2].broadaddr, Some(SockAddr::V4([192, 168, 1, 255])));
    // IFA_LOCAL after IFA_ADDRESS: as musl, the earlier one becomes the destination.
    assert_eq!(e[3].name, b"ppp0:x");
    assert_eq!(e[3].addr, Some(SockAddr::V4([10, 0, 0, 2])));
    assert_eq!(e[3].dstaddr, Some(SockAddr::V4([10, 0, 0, 1])));
    assert_eq!(e[4].addr, Some(SockAddr::V6 { addr: ll, scope_id: 2 }));
    let mut m64 = [0u8; 16];
    m64[..8].fill(0xff);
    assert_eq!(e[4].netmask, Some(SockAddr::V6 { addr: m64, scope_id: 0 }));
}

#[test]
fn errors_and_cut_messages() {
    let mut b = Builder::default();
    let err = msg(2, &[0, 0, 0, 0], &[]);
    assert_eq!(b.take(&err, err.len()), Step::Failed);
    // A message that says it is longer than what came: nothing taken, no panic.
    let mut cut = msg(16, &ifinfo(2, 1, 0), &[(3, b"eth0\0")]);
    let n = cut.len();
    cut.truncate(n - 2);
    assert_eq!(b.take(&cut, cut.len()), Step::More);
    assert!(b.entries.is_empty());
    // An attribute longer than its message: the attributes stop there.
    let mut bad = msg(16, &ifinfo(2, 1, 0), &[(3, b"eth0\0")]);
    bad[32..34].copy_from_slice(&200u16.to_le_bytes());
    assert_eq!(b.take(&bad, bad.len()), Step::More);
    assert!(b.entries.is_empty());
}

#[test]
fn netmasks_as_gen_netmask() {
    assert_eq!(netmask(2, 0), Some(SockAddr::V4([0, 0, 0, 0])));
    assert_eq!(netmask(2, 20), Some(SockAddr::V4([255, 255, 240, 0])));
    assert_eq!(netmask(2, 40), Some(SockAddr::V4([255, 255, 255, 255])));
    assert_eq!(netmask(7, 8), None);
}
