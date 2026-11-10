//! The network as it is: interfaces and addresses from the kernel's netlink (what `ip addr` reads), the default route from
//! `/proc/net/route`, and Wi-Fi from `/dev/lucina-wifi`. Nothing here blocks for long: `ip route`'s netlink request hangs
//! on this kernel, so every answer is waited for with a short `poll`, and the shell draws no address it did not read.

use std::collections::BTreeMap;
use std::fs;

/// How long one netlink answer may take; the shell's one thread also draws.
const NETLINK_WAIT_MS: i32 = 100;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interface {
    pub name: String,
    /// IFF_UP and IFF_RUNNING as the kernel says.
    pub up: bool,
    pub mac: Option<String>,
    pub ipv4: Vec<String>,
}

/// A network the Wi-Fi driver heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bss {
    pub ssid: String,
    pub dbm: i32,
    pub mhz: u32,
    pub security: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Wifi {
    pub state: String,
    pub ssid: Option<String>,
    pub failed: Option<String>,
    pub mac: Option<String>,
    pub heard: Vec<Bss>,
}

impl Wifi {
    /// The signal of the network it is on, the strongest access point of that name.
    pub fn signal_dbm(&self) -> Option<i32> {
        let ssid = self.ssid.as_ref()?;
        self.heard
            .iter()
            .filter(|b| &b.ssid == ssid)
            .map(|b| b.dbm)
            .max()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Network {
    pub interfaces: Vec<Interface>,
    /// (interface, gateway) of each default route.
    pub routes: Vec<(String, String)>,
    pub wifi: Option<Wifi>,
}

impl Network {
    pub fn read() -> Network {
        Network {
            interfaces: read_interfaces().unwrap_or_default(),
            routes: fs::read_to_string("/proc/net/route")
                .map(|s| parse_routes(&s))
                .unwrap_or_default(),
            wifi: fs::read_to_string("/dev/lucina-wifi")
                .ok()
                .map(|s| parse_wifi(&s)),
        }
    }

    /// The interface traffic leaves through: one with an address, the default route's first.
    pub fn primary(&self) -> Option<&Interface> {
        let usable = |i: &&Interface| i.up && !i.ipv4.is_empty();
        self.routes
            .iter()
            .find_map(|(name, _)| self.interfaces.iter().find(|i| i.name == *name))
            .filter(usable)
            .or_else(|| self.interfaces.iter().find(usable))
    }

    /// The status bar's text: `eth0 192.168.1.47`, `FRITZ!Box 192.168.1.63` on Wi-Fi, or `offline`.
    pub fn summary(&self) -> String {
        let Some(i) = self.primary() else {
            return "offline".into();
        };
        let address = i.ipv4[0].split('/').next().unwrap_or("");
        let name = match &self.wifi {
            Some(w) if i.name.starts_with("wlan") => w.ssid.as_deref().unwrap_or(&i.name),
            _ => &i.name,
        };
        format!("{name} {address}")
    }
}

/// `/proc/net/route`: default routes (destination 0) as (interface, gateway). Addresses are little-endian hex there.
pub fn parse_routes(text: &str) -> Vec<(String, String)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (name, dest, gw) = (f.next()?, f.next()?, f.next()?);
            if u32::from_str_radix(dest, 16).ok()? != 0 {
                return None;
            }
            let [a, b, c, d] = u32::from_str_radix(gw, 16).ok()?.to_le_bytes();
            Some((name.to_string(), format!("{a}.{b}.{c}.{d}")))
        })
        .collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

/// A network name is whatever bytes the access point sent: control characters never reach the screen.
fn ssid_text(hex: &str) -> String {
    String::from_utf8_lossy(&unhex(hex))
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// `/dev/lucina-wifi`'s status: `key value` lines (kernel patch 0626). It holds no passphrase, ever.
pub fn parse_wifi(text: &str) -> Wifi {
    let mut w = Wifi::default();
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "state" => w.state = value.to_string(),
            "ssid" => w.ssid = Some(ssid_text(value)),
            "failed" => w.failed = Some(value.to_string()),
            "mac" => w.mac = Some(value.to_string()),
            "bss" => {
                let f: Vec<&str> = value.split(' ').collect();
                if let [_, mhz, dbm, security, ssid] = f[..] {
                    w.heard.push(Bss {
                        ssid: ssid_text(ssid),
                        dbm: dbm.parse().unwrap_or(0),
                        mhz: mhz.parse().unwrap_or(0),
                        security: security.to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    w.heard.sort_by_key(|b| -b.dbm);
    w
}

const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;
const IFLA_ADDRESS: u16 = 1;
const IFLA_IFNAME: u16 = 3;
const IFA_LOCAL: u16 = 2;
const IFA_ADDRESS: u16 = 1;
const IFF_UP: u32 = 1;
const IFF_RUNNING: u32 = 0x40;
const AF_INET: u8 = 2;

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// The routing attributes in `data`, as (type, value).
fn attributes(mut data: &[u8]) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let len = u16::from_ne_bytes([data[0], data[1]]) as usize;
        let kind = u16::from_ne_bytes([data[2], data[3]]) & 0x3fff;
        if len < 4 || len > data.len() {
            break;
        }
        out.push((kind, &data[4..len]));
        data = &data[align4(len).min(data.len())..];
    }
    out
}

fn c_string(v: &[u8]) -> String {
    let end = v.iter().position(|&b| b == 0).unwrap_or(v.len());
    String::from_utf8_lossy(&v[..end]).into_owned()
}

/// Folds netlink messages into `found`, by interface index. Returns whether the dump ended.
pub fn parse_netlink(mut buf: &[u8], found: &mut BTreeMap<u32, Interface>) -> bool {
    while buf.len() >= 16 {
        let len = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let kind = u16::from_ne_bytes([buf[4], buf[5]]);
        if len < 16 || len > buf.len() {
            return true;
        }
        let body = &buf[16..len];
        match kind {
            NLMSG_DONE | NLMSG_ERROR => return true,
            RTM_NEWLINK if body.len() >= 16 => {
                let index = u32::from_ne_bytes([body[4], body[5], body[6], body[7]]);
                let flags = u32::from_ne_bytes([body[8], body[9], body[10], body[11]]);
                let i = found.entry(index).or_default();
                i.up = flags & IFF_UP != 0 && flags & IFF_RUNNING != 0;
                for (k, v) in attributes(&body[16..]) {
                    match k {
                        IFLA_IFNAME => i.name = c_string(v),
                        IFLA_ADDRESS if v.len() == 6 => {
                            let hex: Vec<String> = v.iter().map(|b| format!("{b:02x}")).collect();
                            i.mac = Some(hex.join(":"));
                        }
                        _ => {}
                    }
                }
            }
            RTM_NEWADDR if body.len() >= 8 && body[0] == AF_INET => {
                let prefix = body[1];
                let index = u32::from_ne_bytes([body[4], body[5], body[6], body[7]]);
                let attrs = attributes(&body[8..]);
                let addr = [IFA_LOCAL, IFA_ADDRESS]
                    .iter()
                    .find_map(|want| attrs.iter().find(|(k, _)| k == want));
                if let Some((_, v)) = addr
                    && v.len() == 4
                {
                    found
                        .entry(index)
                        .or_default()
                        .ipv4
                        .push(format!("{}.{}.{}.{}/{prefix}", v[0], v[1], v[2], v[3]));
                }
            }
            _ => {}
        }
        buf = &buf[align4(len).min(buf.len())..];
    }
    false
}

fn dump(fd: libc::c_int, kind: u16, found: &mut BTreeMap<u32, Interface>) -> Option<()> {
    // struct nlmsghdr, then rtgenmsg padded to four bytes.
    let mut req = [0u8; 20];
    req[0..4].copy_from_slice(&20u32.to_ne_bytes());
    req[4..6].copy_from_slice(&kind.to_ne_bytes());
    req[6..8].copy_from_slice(&(NLM_F_REQUEST | NLM_F_DUMP).to_ne_bytes());
    req[16] = libc::AF_UNSPEC as u8;
    // SAFETY: `req` is valid for its length.
    if unsafe { libc::send(fd, req.as_ptr().cast(), req.len(), 0) } < 0 {
        return None;
    }
    let mut buf = [0u8; 8192];
    for _ in 0..16 {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut p, 1, NETLINK_WAIT_MS) } <= 0 {
            return None;
        }
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
        if n <= 0 {
            return None;
        }
        if parse_netlink(&buf[..n as usize], found) {
            return Some(());
        }
    }
    None
}

/// Every interface but loopback, by name.
fn read_interfaces() -> Option<Vec<Interface>> {
    // SAFETY: plain socket call; the descriptor is closed below.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if fd < 0 {
        return None;
    }
    let mut found = BTreeMap::new();
    let done = dump(fd, RTM_GETLINK, &mut found).and_then(|()| dump(fd, RTM_GETADDR, &mut found));
    // SAFETY: `fd` is ours.
    unsafe { libc::close(fd) };
    done?;
    let mut v: Vec<Interface> = found
        .into_values()
        .filter(|i| i.name != "lo" && !i.name.is_empty())
        .collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, ip: &[&str]) -> Interface {
        Interface {
            name: name.into(),
            up: true,
            mac: None,
            ipv4: ip.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn default_routes() {
        let s = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t0101A8C0\t0003\t0\t0\t0\t00000000\t0\t0\t0\n\
                 eth0\t0001A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0\n";
        assert_eq!(
            parse_routes(s),
            vec![("eth0".to_string(), "192.168.1.1".to_string())]
        );
    }

    #[test]
    fn summary_says_what_is_there() {
        let mut n = Network::default();
        assert_eq!(n.summary(), "offline");
        n.interfaces = vec![iface("eth0", &[]), iface("wlan0", &["192.168.1.63/24"])];
        assert_eq!(n.summary(), "wlan0 192.168.1.63");
        n.wifi = Some(parse_wifi("state connected\nssid 46524954\n"));
        assert_eq!(n.summary(), "FRIT 192.168.1.63");
        n.interfaces = vec![
            iface("eth0", &["192.168.1.47/24"]),
            iface("wlan0", &["192.168.1.63/24"]),
        ];
        n.routes = vec![("eth0".into(), "192.168.1.1".into())];
        assert_eq!(n.summary(), "eth0 192.168.1.47");
        // Down, or no address: not online whatever the route says.
        n.interfaces[0].up = false;
        n.interfaces[1].ipv4.clear();
        assert_eq!(n.summary(), "offline");
    }

    #[test]
    fn wifi_status() {
        let w = parse_wifi(
            "state connected\nmac 02:00:00:00:00:01\nssid 46\n\
             bss aa:aa 2412 -70 wpa2 46\nbss bb:bb 5180 -48 wpa2 46\nbss cc:cc 5180 -30 open 0a47\n",
        );
        assert_eq!(w.state, "connected");
        assert_eq!(w.signal_dbm(), Some(-48));
        // Control characters in a name are dropped; the strongest comes first.
        assert_eq!(w.heard[0].ssid, "G");
        assert_eq!(w.heard[1].mhz, 5180);
        assert_eq!(parse_wifi("state idle\n").signal_dbm(), None);
    }

    #[test]
    fn netlink_dump() {
        let mut msg = Vec::new();
        // RTM_NEWLINK eth0, up and running, with a MAC address.
        let mut body = vec![0u8; 16];
        body[4..8].copy_from_slice(&2u32.to_ne_bytes());
        body[8..12].copy_from_slice(&(IFF_UP | IFF_RUNNING).to_ne_bytes());
        body.extend_from_slice(&[9, 0, 3, 0, b'e', b't', b'h', b'0', 0, 0, 0, 0]);
        body.extend_from_slice(&[10, 0, 1, 0, 0, 0xe0, 0x4c, 0x36, 5, 0x98, 0, 0]);
        let push = |msg: &mut Vec<u8>, kind: u16, body: &[u8]| {
            msg.extend_from_slice(&(16 + body.len() as u32).to_ne_bytes());
            msg.extend_from_slice(&kind.to_ne_bytes());
            msg.extend_from_slice(&[0; 10]);
            msg.extend_from_slice(body);
        };
        push(&mut msg, RTM_NEWLINK, &body);
        // RTM_NEWADDR 192.168.1.47/24 on it.
        let mut addr = vec![AF_INET, 24, 0, 0];
        addr.extend_from_slice(&2u32.to_ne_bytes());
        addr.extend_from_slice(&[8, 0, 2, 0, 192, 168, 1, 47]);
        push(&mut msg, RTM_NEWADDR, &addr);
        let mut found = BTreeMap::new();
        assert!(!parse_netlink(&msg, &mut found));
        let i = &found[&2];
        assert_eq!(i.name, "eth0");
        assert!(i.up);
        assert_eq!(i.mac.as_deref(), Some("00:e0:4c:36:05:98"));
        assert_eq!(i.ipv4, vec!["192.168.1.47/24"]);
        // A done message, and one cut short, end the dump.
        let mut done = Vec::new();
        push(&mut done, NLMSG_DONE, &[]);
        assert!(parse_netlink(&done, &mut found));
        assert!(parse_netlink(
            &[200, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            &mut found
        ));
    }
}
