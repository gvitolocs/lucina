//! Gives the host at the other end of the cable its address, as `udhcpd` does under Linux on this device.
//!
//! `udhcpd` needs packet sockets, which the kernel does not have yet, so the replies are made here, at the Ethernet level,
//! from the frames that the host broadcasts. There is one client, one address, and no router or DNS: the link is for
//! reaching the device, and must not become the host's way out.

use alloc::vec::Vec;

const ETH_HLEN: usize = 14;
const IPV4_HLEN: usize = 20;
const UDP_HLEN: usize = 8;
const BOOTP_LEN: usize = 236;
const COOKIE: [u8; 4] = [99, 130, 83, 99];
const FLAG_BROADCAST: u16 = 0x8000;

const OPTION_PAD: u8 = 0;
const OPTION_NETMASK: u8 = 1;
const OPTION_LEASE: u8 = 51;
const OPTION_TYPE: u8 = 53;
const OPTION_SERVER: u8 = 54;
const OPTION_END: u8 = 255;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;

/// A long lease: this client's address never changes.
const LEASE_SECONDS: u32 = 864_000;

/// The addresses on the link.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub server_mac: [u8; 6],
    pub server_ip: [u8; 4],
    pub client_ip: [u8; 4],
    pub netmask: [u8; 4],
}

impl Link {
    /// Those of LucinaOS on this device: the Kindle is 192.168.15.244 and the host gets 192.168.15.201.
    pub const fn kindle() -> Self {
        Self {
            server_mac: [0xee, 0x49, 0, 0, 0, 1],
            server_ip: [192, 168, 15, 244],
            client_ip: [192, 168, 15, 201],
            netmask: [255, 255, 255, 0],
        }
    }
}

/// The reply to this frame, if it is a DHCP `DISCOVER` or `REQUEST`.
pub fn answer(frame: &[u8], link: &Link) -> Option<Vec<u8>> {
    let request = frame.get(ETH_HLEN..)?;
    let is_udp_ipv4 =
        frame.get(12..14)? == [0x08, 0x00] && *request.first()? == 0x45 && *request.get(9)? == 17;
    if !is_udp_ipv4 {
        return None;
    }
    let udp = request.get(IPV4_HLEN..)?;
    if udp.get(2..4)? != [0, 67] {
        return None;
    }
    let bootp = udp.get(UDP_HLEN..)?;
    if *bootp.first()? != 1 || bootp.get(BOOTP_LEN..BOOTP_LEN + 4)? != COOKIE {
        return None;
    }
    let options = bootp.get(BOOTP_LEN + 4..)?;
    let reply_type = match *find_option(options, OPTION_TYPE)?.first()? {
        DISCOVER => OFFER,
        // A REQUEST that names another server is for that server's lease, not for here.
        REQUEST
            if find_option(options, OPTION_SERVER)
                .is_some_and(|server| server != link.server_ip) =>
        {
            return None;
        }
        REQUEST => ACK,
        _ => return None,
    };
    Some(build_reply(bootp, reply_type, link))
}

/// Finds an option's value.
fn find_option(mut options: &[u8], code: u8) -> Option<&[u8]> {
    loop {
        match *options.first()? {
            OPTION_PAD => options = &options[1..],
            OPTION_END => return None,
            found => {
                let len = *options.get(1)? as usize;
                let value = options.get(2..2 + len)?;
                if found == code {
                    return Some(value);
                }
                options = &options[2 + len..];
            }
        }
    }
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in bytes.chunks(2) {
        let word = u16::from_be_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]);
        sum += u32::from(word);
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn build_reply(request: &[u8], reply_type: u8, link: &Link) -> Vec<u8> {
    let xid = &request[4..8];
    let flags = u16::from_be_bytes([request[10], request[11]]);
    let chaddr = &request[28..44];
    let client_mac = &request[28..34];

    let mut bootp = alloc::vec![0; BOOTP_LEN];
    bootp[0] = 2;
    bootp[1] = 1;
    bootp[2] = 6;
    bootp[4..8].copy_from_slice(xid);
    bootp[10..12].copy_from_slice(&flags.to_be_bytes());
    bootp[16..20].copy_from_slice(&link.client_ip);
    bootp[20..24].copy_from_slice(&link.server_ip);
    bootp[28..44].copy_from_slice(chaddr);
    bootp.extend_from_slice(&COOKIE);
    bootp.extend_from_slice(&[OPTION_TYPE, 1, reply_type]);
    bootp.extend_from_slice(&[OPTION_SERVER, 4]);
    bootp.extend_from_slice(&link.server_ip);
    bootp.extend_from_slice(&[OPTION_LEASE, 4]);
    bootp.extend_from_slice(&LEASE_SECONDS.to_be_bytes());
    bootp.extend_from_slice(&[OPTION_NETMASK, 4]);
    bootp.extend_from_slice(&link.netmask);
    bootp.push(OPTION_END);
    // Some clients refuse a reply shorter than a BOOTP message's classic 300 bytes.
    if bootp.len() < 300 {
        bootp.resize(300, OPTION_PAD);
    }

    // A client that asked for broadcast may not yet take unicast; one that did not gets it at its new address.
    let (eth_dst, ip_dst): ([u8; 6], [u8; 4]) = if flags & FLAG_BROADCAST != 0 {
        ([0xff; 6], [0xff; 4])
    } else {
        let mut mac = [0; 6];
        mac.copy_from_slice(client_mac);
        (mac, link.client_ip)
    };

    let udp_len = (UDP_HLEN + bootp.len()) as u16;
    let ip_len = IPV4_HLEN as u16 + udp_len;
    let mut frame = Vec::with_capacity(ETH_HLEN + ip_len as usize);
    frame.extend_from_slice(&eth_dst);
    frame.extend_from_slice(&link.server_mac);
    frame.extend_from_slice(&[0x08, 0x00]);

    let mut ip = [0u8; IPV4_HLEN];
    ip[0] = 0x45;
    ip[2..4].copy_from_slice(&ip_len.to_be_bytes());
    ip[8] = 64;
    ip[9] = 17;
    ip[12..16].copy_from_slice(&link.server_ip);
    ip[16..20].copy_from_slice(&ip_dst);
    let sum = checksum(&ip);
    ip[10..12].copy_from_slice(&sum.to_be_bytes());
    frame.extend_from_slice(&ip);

    // A UDP checksum of zero means none, which IPv4 allows.
    frame.extend_from_slice(&[0, 67, 0, 68]);
    frame.extend_from_slice(&udp_len.to_be_bytes());
    frame.extend_from_slice(&[0, 0]);
    frame.extend_from_slice(&bootp);
    frame
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const CLIENT_MAC: [u8; 6] = [0xee, 0x49, 0, 0, 0, 0];

    fn request(kind: u8, flags: u16) -> Vec<u8> {
        request_naming(kind, flags, &[])
    }

    /// A request with these options before its message type.
    fn request_naming(kind: u8, flags: u16, options: &[u8]) -> Vec<u8> {
        let mut bootp = vec![0u8; BOOTP_LEN];
        bootp[0] = 1;
        bootp[1] = 1;
        bootp[2] = 6;
        bootp[4..8].copy_from_slice(&[1, 2, 3, 4]);
        bootp[10..12].copy_from_slice(&flags.to_be_bytes());
        bootp[28..34].copy_from_slice(&CLIENT_MAC);
        bootp.extend_from_slice(&COOKIE);
        bootp.extend_from_slice(&[12, 3, b'm', b'a', b'c']);
        bootp.extend_from_slice(options);
        bootp.extend_from_slice(&[OPTION_TYPE, 1, kind, OPTION_END]);

        let udp_len = (UDP_HLEN + bootp.len()) as u16;
        let mut frame = vec![0xff; 6];
        frame.extend_from_slice(&CLIENT_MAC);
        frame.extend_from_slice(&[0x08, 0x00]);
        let mut ip = [0u8; IPV4_HLEN];
        ip[0] = 0x45;
        ip[9] = 17;
        ip[16..20].copy_from_slice(&[255; 4]);
        frame.extend_from_slice(&ip);
        frame.extend_from_slice(&[0, 68, 0, 67]);
        frame.extend_from_slice(&udp_len.to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(&bootp);
        frame
    }

    fn option(reply: &[u8], code: u8) -> Option<Vec<u8>> {
        let mut options = &reply[ETH_HLEN + IPV4_HLEN + UDP_HLEN + BOOTP_LEN + 4..];
        loop {
            match options[0] {
                OPTION_END => return None,
                c if c == code => return Some(options[2..2 + options[1] as usize].to_vec()),
                _ => options = &options[2 + options[1] as usize..],
            }
        }
    }

    #[test]
    fn a_discover_gets_an_offer_of_the_hosts_address() {
        let reply = answer(&request(DISCOVER, 0), &Link::kindle()).unwrap();
        assert_eq!(option(&reply, OPTION_TYPE).unwrap(), [OFFER]);
        assert_eq!(option(&reply, OPTION_SERVER).unwrap(), [192, 168, 15, 244]);
        assert_eq!(option(&reply, OPTION_NETMASK).unwrap(), [255, 255, 255, 0]);
        let bootp = &reply[ETH_HLEN + IPV4_HLEN + UDP_HLEN..];
        assert_eq!(bootp[0], 2);
        assert_eq!(bootp[4..8], [1, 2, 3, 4]);
        assert_eq!(bootp[16..20], [192, 168, 15, 201]);
        assert_eq!(bootp[28..34], CLIENT_MAC);
    }

    #[test]
    fn a_request_gets_an_ack() {
        let reply = answer(&request(REQUEST, 0), &Link::kindle()).unwrap();
        assert_eq!(option(&reply, OPTION_TYPE).unwrap(), [ACK]);
    }

    #[test]
    fn a_request_is_answered_only_if_it_names_this_server_or_none() {
        let link = Link::kindle();
        let ours = [OPTION_SERVER, 4, 192, 168, 15, 244];
        let reply = answer(&request_naming(REQUEST, 0, &ours), &link).unwrap();
        assert_eq!(option(&reply, OPTION_TYPE).unwrap(), [ACK]);
        let other = [OPTION_SERVER, 4, 192, 168, 15, 1];
        assert!(answer(&request_naming(REQUEST, 0, &other), &link).is_none());
        // A DISCOVER names no one, and a server option there changes nothing.
        assert!(answer(&request_naming(DISCOVER, 0, &other), &link).is_some());
    }

    #[test]
    fn there_is_no_router_or_dns_so_the_link_does_not_become_the_way_out() {
        let reply = answer(&request(DISCOVER, 0), &Link::kindle()).unwrap();
        assert_eq!(option(&reply, 3), None);
        assert_eq!(option(&reply, 6), None);
    }

    #[test]
    fn a_client_asking_for_broadcast_gets_it_and_the_others_get_unicast() {
        let reply = answer(&request(DISCOVER, FLAG_BROADCAST), &Link::kindle()).unwrap();
        assert_eq!(reply[..6], [0xff; 6]);
        assert_eq!(reply[ETH_HLEN + 16..ETH_HLEN + 20], [255; 4]);
        let reply = answer(&request(DISCOVER, 0), &Link::kindle()).unwrap();
        assert_eq!(reply[..6], CLIENT_MAC);
        assert_eq!(reply[ETH_HLEN + 16..ETH_HLEN + 20], [192, 168, 15, 201]);
    }

    #[test]
    fn the_ip_header_checksum_verifies_and_lengths_add_up() {
        let reply = answer(&request(DISCOVER, 0), &Link::kindle()).unwrap();
        let ip = &reply[ETH_HLEN..ETH_HLEN + IPV4_HLEN];
        assert_eq!(checksum(ip), 0);
        assert_eq!(
            usize::from(u16::from_be_bytes([ip[2], ip[3]])),
            reply.len() - ETH_HLEN
        );
        let udp = &reply[ETH_HLEN + IPV4_HLEN..];
        assert_eq!(usize::from(u16::from_be_bytes([udp[4], udp[5]])), udp.len());
        assert!(reply.len() <= 1514);
    }

    #[test]
    fn other_frames_are_not_answered() {
        let link = Link::kindle();
        assert!(answer(&[], &link).is_none());
        assert!(answer(&[0; 60], &link).is_none());
        // An ARP frame.
        let mut frame = request(DISCOVER, 0);
        frame[12..14].copy_from_slice(&[0x08, 0x06]);
        assert!(answer(&frame, &link).is_none());
        // Another UDP port.
        let mut frame = request(DISCOVER, 0);
        frame[ETH_HLEN + IPV4_HLEN + 3] = 53;
        assert!(answer(&frame, &link).is_none());
        // A DHCP reply from something else, DECLINE, RELEASE, and a frame cut short.
        let mut frame = request(DISCOVER, 0);
        frame[ETH_HLEN + IPV4_HLEN + UDP_HLEN] = 2;
        assert!(answer(&frame, &link).is_none());
        assert!(answer(&request(4, 0), &link).is_none());
        assert!(answer(&request(7, 0), &link).is_none());
        let frame = request(DISCOVER, 0);
        assert!(answer(&frame[..ETH_HLEN + IPV4_HLEN + 10], &link).is_none());
    }

    #[test]
    fn options_that_run_off_the_end_are_not_read_past_it() {
        let mut frame = request(DISCOVER, 0);
        let at = ETH_HLEN + IPV4_HLEN + UDP_HLEN + BOOTP_LEN + 4;
        frame.truncate(at + 2);
        frame[at + 1] = 200;
        assert!(answer(&frame, &Link::kindle()).is_none());
    }
}
