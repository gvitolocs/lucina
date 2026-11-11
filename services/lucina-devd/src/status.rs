//! What `status` reports: battery, power, network, CPUs, memory, generation. Every source is
//! optional: a missing file gives `None`, never an error.

use std::fs;
use std::path::Path;

/// Everything `status` knows, read from the files under `root` ("/" on the laptop).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Status {
    pub battery: Option<Battery>,
    /// "charger" | "battery": a Mains/USB supply with online == 1, else the battery.
    pub power_source: Option<String>,
    pub wifi: Option<Wifi>,
    /// Non-wlan, non-lo interfaces that have an address.
    pub ethernet: Vec<Iface>,
    pub cpus: Vec<Cpu>,
    pub memory_mib: Option<Memory>,
    pub uptime_s: Option<u64>,
    pub generation: Option<String>,
    /// `/proc/version`, first line, trimmed.
    pub kernel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Battery {
    pub percent: Option<u8>,
    pub status: Option<String>,
    /// `current_now` in µA, as mA, sign kept.
    pub current_ma: Option<i64>,
    /// `voltage_now` in µV, as mV.
    pub voltage_mv: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Wifi {
    pub state: Option<String>,
    pub ssid: Option<String>,
    /// The strongest bss whose ssid matches.
    pub dbm: Option<i32>,
    /// The IPv4 of the wlan interface.
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Iface {
    pub name: String,
    pub address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Cpu {
    pub id: usize,
    pub mhz: Option<u64>,
}

/// `MemTotal` and `MemAvailable` in MiB.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
}

/// Everything, from the real files under `root`.
pub fn read(root: &Path) -> Status {
    let read = |rel: &str| fs::read_to_string(root.join(rel)).ok();
    let battery = battery(root);
    let power_source = power_source(root, battery.is_some());
    let (ethernet, wlan_address) = interfaces();
    let wifi = read("dev/lucina-wifi").map(|s| {
        let mut w = parse_wifi(&s);
        w.address = wlan_address;
        w
    });
    Status {
        battery,
        power_source,
        wifi,
        ethernet,
        cpus: cpus(root),
        memory_mib: read("proc/meminfo").and_then(|s| parse_meminfo(&s)),
        uptime_s: read("proc/uptime").and_then(|s| parse_uptime(&s)),
        generation: read("mnt/lucina-apps/lucinaos/apps/CURRENT").and_then(|s| parse_generation(&s)),
        kernel: read("proc/version").and_then(|s| parse_kernel(&s)),
    }
}

/// The first `/sys/class/power_supply/*` of type Battery, as lucina-shell's `battery()` chooses it.
fn battery(root: &Path) -> Option<Battery> {
    let dir = fs::read_dir(root.join("sys/class/power_supply")).ok()?;
    for e in dir.flatten() {
        let p = e.path();
        let read = |name: &str| {
            fs::read_to_string(p.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if read("type").as_deref() != Some("Battery") {
            continue;
        }
        return Some(Battery {
            percent: read("capacity").and_then(|v| v.parse().ok()),
            status: read("status"),
            current_ma: read("current_now").and_then(|v| v.parse::<i64>().ok()).map(|ua| ua / 1000),
            voltage_mv: read("voltage_now").and_then(|v| v.parse::<i64>().ok()).map(|uv| uv / 1000),
        });
    }
    None
}

/// "charger" when a Mains/USB supply is online, else "battery" when there is one.
fn power_source(root: &Path, has_battery: bool) -> Option<String> {
    let dir = fs::read_dir(root.join("sys/class/power_supply")).ok()?;
    for e in dir.flatten() {
        let p = e.path();
        let read = |name: &str| {
            fs::read_to_string(p.join(name))
                .ok()
                .map(|s| s.trim().to_string())
        };
        if matches!(read("type").as_deref(), Some("Mains") | Some("USB")) && read("online").as_deref() == Some("1") {
            return Some("charger".to_string());
        }
    }
    has_battery.then(|| "battery".to_string())
}

/// Interfaces and their IPv4 addresses via getifaddrs, as the kernel lists them: the wlan
/// interface's address goes to Wi-Fi, the rest that have one are the ethernet list.
fn interfaces() -> (Vec<Iface>, Option<String>) {
    let mut out = Vec::new();
    let mut wlan_address = None;
    // SAFETY: getifaddrs writes a linked list into `ifs`; each node is read once and freed below.
    let mut ifs: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifs) } != 0 {
        return (out, wlan_address);
    }
    let mut cur = ifs;
    while let Some(node) = unsafe { cur.as_ref() } {
        let name = unsafe { std::ffi::CStr::from_ptr(node.ifa_name) }
            .to_string_lossy()
            .into_owned();
        let addr = unsafe { node.ifa_addr.as_ref() };
        if let Some(a) = addr
            && a.sa_family as i32 == libc::AF_INET
        {
            // SAFETY: AF_INET, so the address is a sockaddr_in.
            let sin = unsafe { &*(a as *const _ as *const libc::sockaddr_in) };
            let o = sin.sin_addr.s_addr.to_ne_bytes();
            let ip = format!("{}.{}.{}.{}", o[0], o[1], o[2], o[3]);
            if name == "lo" {
            } else if name.starts_with("wlan") {
                if wlan_address.is_none() {
                    wlan_address = Some(ip);
                }
            } else {
                match out.iter_mut().find(|i| i.name == name) {
                    Some(i) => {
                        if i.address.is_none() {
                            i.address = Some(ip);
                        }
                    }
                    None => out.push(Iface {
                        name,
                        address: Some(ip),
                    }),
                }
            }
        }
        cur = node.ifa_next;
    }
    // SAFETY: the list came from getifaddrs.
    unsafe { libc::freeifaddrs(ifs) };
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, wlan_address)
}

/// `/dev/lucina-wifi`'s status: `key value` lines (kernel patch 0626), as lucina-shell's
/// `parse_wifi` reads it. It holds no passphrase, ever.
fn parse_wifi(text: &str) -> Wifi {
    let mut state = None;
    let mut ssid = None;
    let mut heard: Vec<(String, i32)> = Vec::new();
    for line in text.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "state" => state = Some(value.to_string()),
            "ssid" => ssid = Some(ssid_text(value)),
            "bss" => {
                let f: Vec<&str> = value.split(' ').collect();
                if let [_, _, dbm, _, bss_ssid] = f[..] {
                    heard.push((ssid_text(bss_ssid), dbm.parse().unwrap_or(0)));
                }
            }
            _ => {}
        }
    }
    let dbm = ssid
        .as_ref()
        .and_then(|s| heard.iter().filter(|(b, _)| b == s).map(|(_, d)| *d).max());
    Wifi {
        state,
        ssid,
        dbm,
        address: None,
    }
}

/// A network name is whatever bytes the access point sent, hex-encoded; control characters never count.
fn ssid_text(hex: &str) -> String {
    let bytes: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect();
    String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

/// `/proc/lucina-cpu` if it exists: `cpu <id> ... <khz> kHz ...` lines, mhz per CPU. Else the
/// online CPUs from `/sys/devices/system/cpu/online` or `/proc/cpuinfo`, with mhz None.
fn cpus(root: &Path) -> Vec<Cpu> {
    if let Ok(s) = fs::read_to_string(root.join("proc/lucina-cpu")) {
        let v = parse_lucina_cpu(&s);
        if !v.is_empty() {
            return v;
        }
    }
    let n = fs::read_to_string(root.join("sys/devices/system/cpu/online"))
        .ok()
        .and_then(|s| parse_cpu_online(&s))
        .or_else(|| {
            fs::read_to_string(root.join("proc/cpuinfo"))
                .ok()
                .map(|s| parse_cpuinfo_count(&s))
        })
        .unwrap_or(0);
    (0..n).map(|id| Cpu { id, mhz: None }).collect()
}

fn parse_lucina_cpu(text: &str) -> Vec<Cpu> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            if f.next()? != "cpu" {
                return None;
            }
            let id: usize = f.next()?.parse().ok()?;
            let mut mhz = None;
            let fields: Vec<&str> = l.split_whitespace().collect();
            for w in 0..fields.len().saturating_sub(1) {
                if fields[w + 1] == "kHz"
                    && let Ok(khz) = fields[w].parse::<u64>()
                {
                    mhz = Some(khz / 1000);
                }
            }
            Some(Cpu { id, mhz })
        })
        .collect()
}

/// `0-3,5` as a count.
fn parse_cpu_online(text: &str) -> Option<usize> {
    let mut n = 0usize;
    for part in text.trim().split(',') {
        if let Some((a, b)) = part.split_once('-') {
            n += b.parse::<usize>().ok()?.saturating_sub(a.parse().ok()?) + 1;
        } else {
            n += 1;
        }
    }
    Some(n)
}

fn parse_cpuinfo_count(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("processor"))
        .count()
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in MiB, as lucina-shell's `parse_meminfo`.
fn parse_meminfo(s: &str) -> Option<Memory> {
    let field = |name: &str| {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kib| kib / 1024)
    };
    Some(Memory {
        total: field("MemTotal:")?,
        available: field("MemAvailable:").or_else(|| field("MemFree:"))?,
    })
}

/// `/proc/uptime`'s first number, whole seconds.
fn parse_uptime(s: &str) -> Option<u64> {
    s.split('.').next()?.trim().parse().ok()
}

/// The generation in use: the one line of `\lucinaos\apps\CURRENT` on the mounted ESP
/// (`/mnt/lucina-apps`), as lucina-shell's `appset::current_folder` reads it.
fn parse_generation(s: &str) -> Option<String> {
    let name = s.lines().next()?.trim();
    let ok = name.len() == 14
        && name.starts_with("g-")
        && name[2..]
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    ok.then(|| name.to_string())
}

/// `/proc/version`, first line, trimmed.
fn parse_kernel(s: &str) -> Option<String> {
    let line = s.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lucina-devd-status-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_root_gives_default() {
        let root = temp_root("empty");
        let s = read(&root);
        assert!(s.battery.is_none());
        assert!(s.power_source.is_none());
        assert!(s.wifi.is_none());
        // Interfaces come from the live kernel via getifaddrs, not from `root`.
        assert!(s.cpus.is_empty());
        assert!(s.memory_mib.is_none());
        assert!(s.uptime_s.is_none());
        assert!(s.generation.is_none());
        assert!(s.kernel.is_none());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn reads_the_fake_files() {
        let root = temp_root("full");
        let ps = root.join("sys/class/power_supply");
        fs::create_dir_all(ps.join("ADP1")).unwrap();
        fs::write(ps.join("ADP1/type"), "Mains\n").unwrap();
        fs::write(ps.join("ADP1/online"), "1\n").unwrap();
        fs::create_dir_all(ps.join("BAT0")).unwrap();
        fs::write(ps.join("BAT0/type"), "Battery\n").unwrap();
        fs::write(ps.join("BAT0/capacity"), "87\n").unwrap();
        fs::write(ps.join("BAT0/status"), "Charging\n").unwrap();
        fs::write(ps.join("BAT0/current_now"), "-1200000\n").unwrap();
        fs::write(ps.join("BAT0/voltage_now"), "12345000\n").unwrap();
        fs::create_dir_all(root.join("proc")).unwrap();
        fs::write(
            root.join("proc/meminfo"),
            "MemTotal:        4028140 kB\nMemFree:         3900000 kB\nMemAvailable:    3950000 kB\n",
        )
        .unwrap();
        fs::write(root.join("proc/uptime"), "12345.67 999.0\n").unwrap();
        fs::write(root.join("proc/version"), "Lucina 1.0 (juno) #1\nsecond line\n").unwrap();
        fs::create_dir_all(root.join("dev")).unwrap();
        fs::write(
            root.join("dev/lucina-wifi"),
            "state connected\nssid 46524954\nbss aa 5180 -60 wpa2 46524954\nbss bb 2412 -80 wpa2 47\n",
        )
        .unwrap();
        let s = read(&root);
        let b = s.battery.unwrap();
        assert_eq!(b.percent, Some(87));
        assert_eq!(b.status.as_deref(), Some("Charging"));
        assert_eq!(b.current_ma, Some(-1200));
        assert_eq!(b.voltage_mv, Some(12345));
        assert_eq!(s.power_source.as_deref(), Some("charger"));
        let w = s.wifi.unwrap();
        assert_eq!(w.state.as_deref(), Some("connected"));
        assert_eq!(w.ssid.as_deref(), Some("FRIT"));
        assert_eq!(w.dbm, Some(-60));
        let m = s.memory_mib.unwrap();
        assert_eq!((m.total, m.available), (3933, 3857));
        assert_eq!(s.uptime_s, Some(12345));
        assert_eq!(s.kernel.as_deref(), Some("Lucina 1.0 (juno) #1"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parsers() {
        assert_eq!(parse_uptime("42.9 1\n"), Some(42));
        assert_eq!(parse_uptime(""), None);
        assert_eq!(parse_generation("g-0123456789ab\n"), Some("g-0123456789ab".to_string()));
        assert_eq!(parse_generation("g-XYZ\n"), None);
        assert_eq!(parse_generation(""), None);
        assert_eq!(
            parse_lucina_cpu("cpu 0 1804800 kHz by cycle count\ncpu 6 2553600 kHz by cycle count\ndomain 0 auto\n"),
            vec![
                Cpu { id: 0, mhz: Some(1804) },
                Cpu { id: 6, mhz: Some(2553) }
            ]
        );
        assert_eq!(parse_cpu_online("0-7\n"), Some(8));
        assert_eq!(parse_cpu_online("0-3,5\n"), Some(5));
        assert_eq!(parse_cpuinfo_count("processor\t: 0\nprocessor\t: 1\n"), 2);
        assert_eq!(parse_wifi("state idle\n").dbm, None);
    }
}
