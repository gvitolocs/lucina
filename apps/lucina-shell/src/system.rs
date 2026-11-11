//! What the status bar and Settings show, read from the kernel's Linux ABI (`/proc`, `/sys`,
//! `uname`, the clocks). Every source is optional: Asterinas does not have all of them yet, and a
//! missing one shows as unknown rather than failing.

use std::fs;

use crate::net::Network;
use lucina_app::time::Zone;

/// The status bar's right side. What is not known is not shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// Empty until the wall clock has been set.
    pub clock: String,
    pub battery: Option<Battery>,
    pub network: String,
    pub online: bool,
    /// Who captures from the webcam, from `/proc/lucina-camera`: the program's name. `None` when
    /// nothing captures (or the kernel has no camera file).
    pub camera: Option<String>,
}

/// `CLOCK_REALTIME` before this (2020-01-01) means nobody set the clock.
const CLOCK_SET_AFTER: i64 = 1_577_836_800;

/// The wall clock as `HH:MM ZZZ` in the configured zone. The kernel starts in 1970 on the laptop, which has no real-time clock
/// driver, until `lucina-timesync` has set it: then this is empty, as no time is better than a wrong one.
pub fn clock_text(zone: &Zone, realtime_s: i64) -> String {
    if realtime_s >= CLOCK_SET_AFTER {
        zone.clock(realtime_s)
    } else {
        String::new()
    }
}

/// Whether the wall clock has been set, and the time.
pub fn wall_clock() -> Option<i64> {
    let now = clock(libc::CLOCK_REALTIME);
    (now >= CLOCK_SET_AFTER).then_some(now)
}

fn clock(id: libc::clockid_t) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec.
    unsafe { libc::clock_gettime(id, &mut ts) };
    ts.tv_sec
}

/// Seconds to the next minute boundary of the clock shown, so it can be redrawn then.
pub fn until_next_minute(zone: &Zone) -> u64 {
    match wall_clock() {
        Some(now) => zone.until_next_minute(now),
        None => 60,
    }
}

/// The battery, from the first `/sys/class/power_supply/*` of type Battery (`lucina.charge=report` on the laptop).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    /// `/sys`'s `status`: Charging, Discharging, Full, Not charging.
    pub status: String,
    /// Every other attribute the kernel gives, as (name, value).
    pub details: Vec<(String, String)>,
}

impl Battery {
    /// `83%`, `83% charging`, `100% full`.
    pub fn text(&self) -> String {
        match self.status.as_str() {
            "Charging" => format!("{}% charging", self.percent),
            "Full" => format!("{}% full", self.percent),
            "Not charging" => format!("{}% plugged in, not charging", self.percent),
            _ => format!("{}%", self.percent),
        }
    }
}

pub fn battery() -> Option<Battery> {
    let dir = fs::read_dir("/sys/class/power_supply").ok()?;
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
        let details = [
            "voltage_now",
            "current_now",
            "temp",
            "charge_now",
            "charge_full",
            "charge_full_design",
            "cycle_count",
            "technology",
        ]
        .iter()
        .filter_map(|n| read(n).map(|v| (n.to_string(), v)))
        .collect();
        return Some(Battery {
            percent: read("capacity")?.parse().ok()?,
            status: read("status").unwrap_or_default(),
            details,
        });
    }
    None
}

pub fn uptime_s() -> u64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split('.').next().and_then(|n| n.trim().parse().ok()))
        .unwrap_or_else(|| clock(libc::CLOCK_MONOTONIC).max(0) as u64)
}

pub fn status(zone: &Zone, network: &Network) -> Status {
    status_with(zone, network, battery())
}

/// [`status`] with a battery reading the caller already has (the shell reads it once a minute, not on every status pass).
pub fn status_with(zone: &Zone, network: &Network, battery: Option<Battery>) -> Status {
    Status {
        clock: clock_text(zone, clock(libc::CLOCK_REALTIME)),
        battery,
        network: network.summary(),
        online: network.primary().is_some(),
        camera: lucina_app::camera_mark::read().map(|(_, program)| program),
    }
}

/// `uname -srm`.
// `c_char` is i8 on x86-64 and u8 on AArch64: the cast is needed on one of them.
#[allow(clippy::unnecessary_cast)]
pub fn kernel() -> String {
    // SAFETY: uname fills the struct; the fields are NUL-terminated.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut u) } < 0 {
        return "unknown".into();
    }
    let f = |s: &[libc::c_char]| {
        let b: Vec<u8> = s
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&b).into_owned()
    };
    format!("{} {} {}", f(&u.sysname), f(&u.release), f(&u.machine))
}

pub fn cpus() -> usize {
    // SAFETY: sysconf has no memory effects.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 { n as usize } else { 1 }
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in MiB.
pub fn memory() -> Option<(u64, u64)> {
    parse_meminfo(&fs::read_to_string("/proc/meminfo").ok()?)
}

pub fn parse_meminfo(s: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        s.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .map(|kib| kib / 1024)
    };
    Some((
        field("MemTotal:")?,
        field("MemAvailable:").or_else(|| field("MemFree:"))?,
    ))
}

/// The chip, from the last string of the device tree's `compatible` (`qcom,sc7180` on the laptop).
pub fn soc() -> Option<String> {
    let raw = fs::read("/proc/device-tree/compatible").ok()?;
    let last = raw.split(|&b| b == 0).rfind(|s| !s.is_empty())?;
    Some(String::from_utf8_lossy(last).into_owned())
}

/// The machine's name from the device tree the firmware passed.
/// "Generation 42": the installed generation the loader booted, from the kernel command line (`lucina.generation=`), or `None`
/// when the command line does not say (QEMU, a reload stream).
pub fn generation_line() -> Option<String> {
    let cmdline = fs::read_to_string("/proc/cmdline").ok()?;
    generation_of(&cmdline).map(|g| format!("Generation {g}"))
}

/// The generation in a kernel command line.
pub fn generation_of(cmdline: &str) -> Option<u32> {
    cmdline.split_whitespace().find_map(|w| {
        w.strip_prefix("lucina.generation=")
            .or_else(|| w.strip_prefix("lucina.gen="))?
            .parse()
            .ok()
    })
}

pub fn model() -> Option<String> {
    let read = |name: &str| {
        fs::read(format!("/proc/device-tree/{name}"))
            .or_else(|_| fs::read(format!("/sys/firmware/devicetree/base/{name}")))
            .ok()
    };
    machine_name(read("model").as_deref(), read("compatible").as_deref())
}

/// `model`, else the first string of `compatible`, as Linux's `of_flat_dt_get_machine_name`.
pub fn machine_name(model: Option<&[u8]>, compatible: Option<&[u8]>) -> Option<String> {
    let first = |raw: &[u8]| {
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let s = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    };
    model.and_then(first).or_else(|| compatible.and_then(first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_only_when_set() {
        // 2026-11-01 13:05:00 UTC.
        let z = Zone::utc();
        assert_eq!(clock_text(&z, 1_793_538_300), "13:05 UTC");
        assert_eq!(
            clock_text(&Zone::parse("CET-1").unwrap(), 1_793_538_300),
            "14:05 CET"
        );
        // The kernel's 1970: nothing, not a wrong time.
        assert_eq!(clock_text(&z, 12), "");
        assert_eq!(clock_text(&z, 0), "");
    }

    #[test]
    fn battery_words() {
        let b = |percent, status: &str| Battery {
            percent,
            status: status.into(),
            details: Vec::new(),
        };
        assert_eq!(b(83, "Discharging").text(), "83%");
        assert_eq!(b(83, "Charging").text(), "83% charging");
        assert_eq!(b(100, "Full").text(), "100% full");
        assert_eq!(b(90, "Not charging").text(), "90% plugged in, not charging");
    }

    #[test]
    fn machine_names() {
        let n = |m: Option<&[u8]>, c: Option<&[u8]>| machine_name(m, c);
        assert_eq!(
            n(
                Some(b"Samsung Galaxy Book Go\0"),
                Some(b"samsung,galaxy-book-go\0qcom,sc7180\0")
            ),
            Some("Samsung Galaxy Book Go".to_string())
        );
        // QEMU virt's tree has no `model`.
        assert_eq!(
            n(None, Some(b"linux,dummy-virt\0")),
            Some("linux,dummy-virt".to_string())
        );
        assert_eq!(n(Some(b"\0"), Some(b"a,b\0c,d\0")), Some("a,b".to_string()));
        assert_eq!(n(None, None), None);
    }

    #[test]
    fn meminfo() {
        let s = "MemTotal:        4028140 kB\nMemFree:         3900000 kB\nMemAvailable:    3950000 kB\n";
        assert_eq!(parse_meminfo(s), Some((3933, 3857)));
        assert_eq!(
            parse_meminfo("MemTotal: 2048 kB\nMemFree: 1024 kB\n"),
            Some((2, 1))
        );
        assert_eq!(parse_meminfo(""), None);
    }
}
