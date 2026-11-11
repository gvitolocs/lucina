//! What Settings shows: the machine as the kernel describes it, read when the page is built. Nothing is invented: a source
//! that is not there gives a row saying so, and a value not read is left out. No row ever holds a Wi-Fi passphrase: the only
//! Wi-Fi source is the status of `/dev/lucina-wifi`, which does not have it.

use std::fs;

use crate::backlight::{self, Backlight};
use crate::net::Network;
use crate::system::{self, Battery};
use crate::ui::{self, SettingsPage};

/// What Settings shows about the screen.
pub struct Display {
    pub width: u32,
    pub height: u32,
    pub format: String,
    pub driver: String,
}

type Rows = Vec<(String, String)>;

fn row(k: &str, v: impl Into<String>) -> (String, String) {
    (k.to_string(), v.into())
}

fn signal_words(dbm: i32) -> &'static str {
    match dbm {
        d if d >= -55 => "excellent",
        d if d >= -67 => "good",
        d if d >= -75 => "fair",
        _ => "weak",
    }
}

pub fn network_rows(n: &Network) -> Rows {
    let mut rows = Vec::new();
    for i in &n.interfaces {
        let state = if i.up { "up" } else { "down" };
        let addresses = if i.ipv4.is_empty() {
            "no address".to_string()
        } else {
            i.ipv4.join(", ")
        };
        rows.push(row(&i.name, format!("{state}, {addresses}")));
        if let Some(mac) = &i.mac {
            rows.push(row(&format!("{} hardware", i.name), mac.clone()));
        }
    }
    if n.interfaces.is_empty() {
        rows.push(row("Interfaces", "none"));
    }
    for (name, gateway) in &n.routes {
        rows.push(row("Gateway", format!("{gateway} on {name}")));
    }
    match &n.wifi {
        None => rows.push(row("Wi-Fi", "off")),
        Some(w) => {
            rows.push(row("Wi-Fi", w.state.clone()));
            if let Some(ssid) = &w.ssid {
                rows.push(row("Wi-Fi network", ssid.clone()));
            }
            if let Some(dbm) = w.signal_dbm() {
                rows.push(row("Signal", format!("{dbm} dBm, {}", signal_words(dbm))));
            }
            if let Some(b) = w
                .ssid
                .as_ref()
                .and_then(|s| w.heard.iter().find(|b| &b.ssid == s))
            {
                rows.push(row("Frequency", format!("{} MHz, {}", b.mhz, b.security)));
            }
            if let Some(why) = &w.failed {
                rows.push(row("Wi-Fi failed", why.clone()));
            }
            let others: Vec<String> = w
                .heard
                .iter()
                .filter(|b| w.ssid.as_ref() != Some(&b.ssid))
                .take(3)
                .map(|b| format!("{} ({} dBm)", b.ssid, b.dbm))
                .collect();
            if !others.is_empty() {
                rows.push(row("Also heard", others.join(", ")));
            }
        }
    }
    rows
}

pub fn battery_rows(b: Option<&Battery>, adapter_online: Option<bool>) -> Rows {
    let Some(b) = b else {
        return vec![row("Battery", "not read (lucina.charge=report reads it)")];
    };
    let mut rows = vec![row("Charge", b.text())];
    if let Some(online) = adapter_online {
        rows.push(row(
            "Adapter",
            if online { "connected" } else { "not connected" },
        ));
    }
    let get = |name: &str| {
        b.details
            .iter()
            .find(|(k, _)| k == name)
            .and_then(|(_, v)| v.parse::<i64>().ok())
    };
    if let Some(v) = get("voltage_now") {
        rows.push(row("Voltage", format!("{:.2} V", v as f64 / 1e6)));
    }
    if let Some(v) = get("current_now") {
        rows.push(row("Current", format!("{:.2} A", v as f64 / 1e6)));
    }
    if let Some(v) = get("temp") {
        rows.push(row("Temperature", format!("{:.1} °C", v as f64 / 10.0)));
    }
    if let (Some(now), Some(full)) = (get("charge_now"), get("charge_full")) {
        rows.push(row(
            "Capacity",
            format!("{} of {} mAh", now / 1000, full / 1000),
        ));
    }
    if let (Some(full), Some(design)) = (get("charge_full"), get("charge_full_design"))
        && design > 0
    {
        rows.push(row(
            "Health",
            format!(
                "{} % of the {} mAh it was made with",
                full * 100 / design,
                design / 1000
            ),
        ));
    }
    if let Some(v) = get("cycle_count") {
        rows.push(row("Cycles", v.to_string()));
    }
    if let Some((_, t)) = b.details.iter().find(|(k, _)| k == "technology") {
        rows.push(row("Type", t.clone()));
    }
    rows
}

fn adapter_online() -> Option<bool> {
    for e in fs::read_dir("/sys/class/power_supply").ok()?.flatten() {
        if let Ok(v) = fs::read_to_string(e.path().join("online")) {
            return Some(v.trim() == "1");
        }
    }
    None
}

fn size_text(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= 1024 * MIB {
        format!("{:.1} GiB", bytes as f64 / (1024 * MIB) as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

/// Mounted file systems worth naming, from `/proc/mounts` lines: (mount point, type).
pub fn parse_mounts(text: &str) -> Vec<(String, String)> {
    const SKIP: [&str; 8] = [
        "proc", "sysfs", "devpts", "devtmpfs", "cgroup2", "cgroup", "configfs", "pstore",
    ];
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_, point, kind) = (f.next()?, f.next()?, f.next()?);
            (!SKIP.contains(&kind)).then(|| (point.to_string(), kind.to_string()))
        })
        .collect()
}

fn free_and_total(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs fills the zeroed struct, and the path is a C string.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    let unit = s.f_frsize as u64;
    Some((s.f_bavail as u64 * unit, s.f_blocks as u64 * unit))
}

fn storage_rows() -> Rows {
    let mut rows = Vec::new();
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    for (point, kind) in parse_mounts(&mounts) {
        let value = match free_and_total(&point) {
            Some((free, total)) if total > 0 => {
                format!("{kind}, {} free of {}", size_text(free), size_text(total))
            }
            _ => kind,
        };
        rows.push(row(&point, value));
    }
    let mut disks: Vec<String> = fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| {
            (n.starts_with("sd")
                || n.starts_with("mmcblk")
                || n.starts_with("ufs")
                || n.starts_with("vd"))
                && !n.ends_with(|c: char| c.is_ascii_digit())
        })
        .collect();
    disks.sort();
    rows.push(row(
        "Disks",
        if disks.is_empty() {
            "none visible: the system runs from RAM".to_string()
        } else {
            disks.join(", ")
        },
    ));
    rows
}

fn memory_rows() -> Rows {
    match system::memory() {
        Some((total, available)) => vec![
            row("Total", format!("{total} MiB")),
            row("Available", format!("{available} MiB")),
            row("In use", format!("{} MiB", total.saturating_sub(available))),
        ],
        None => vec![row("Memory", "not readable")],
    }
}

/// How many CPUs the device tree says the machine has.
fn cpus_in_device_tree() -> Option<usize> {
    let n = fs::read_dir("/proc/device-tree/cpus")
        .ok()?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("cpu@"))
        .count();
    (n > 0).then_some(n)
}

fn cpu_rows() -> Rows {
    let online = system::cpus();
    let mut rows = vec![row(
        "CPUs",
        match cpus_in_device_tree() {
            Some(all) => format!("{online} running, of {all}"),
            None => format!("{online} running"),
        },
    )];
    if let Some(m) = system::model() {
        rows.push(row("Machine", m));
    }
    if let Some(chip) = system::soc() {
        rows.push(row("Chip", chip));
    }
    if let Ok(s) = fs::read_to_string("/proc/loadavg") {
        let load: Vec<&str> = s.split_whitespace().take(3).collect();
        rows.push(row("Load", load.join("  ")));
    }
    let up = system::uptime_s();
    rows.push(row(
        "Uptime",
        format!("{}:{:02}:{:02}", up / 3600, up % 3600 / 60, up % 60),
    ));
    rows
}

fn build_rows() -> Rows {
    let mut rows = vec![row("Kernel", system::kernel())];
    if let Ok(v) = fs::read_to_string("/proc/version") {
        rows.push(row("Version", v.trim()));
    }
    rows.push(row(
        "Shell",
        format!(
            "lucina-shell {} ({})",
            env!("CARGO_PKG_VERSION"),
            option_env!("LUCINA_SHELL_BUILD").unwrap_or("no commit recorded")
        ),
    ));
    if let Ok(line) = fs::read_to_string("/proc/cmdline") {
        let args: Vec<&str> = line
            .split_whitespace()
            .take_while(|a| *a != "--")
            .filter(|a| a.starts_with("lucina."))
            .collect();
        rows.push(row("Boot options", args.join(" ")));
    }
    rows
}

/// `lucina-crash`'s report as rows. The text comes from a crash log: only printable characters reach the screen.
pub fn crash_rows(report: Option<&str>) -> Rows {
    let Some(report) = report else {
        return vec![row("Crash log", "lucina-crash is not available")];
    };
    let lines: Vec<String> = report
        .lines()
        .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|l| !l.trim().is_empty())
        .take(12)
        .collect();
    if lines.is_empty() {
        return vec![row("Crash log", "nothing reported")];
    }
    lines.iter().map(|l| row("", l.clone())).collect()
}

/// Runs `lucina-crash`. Slow compared with a frame, so only when the section is opened.
pub fn read_crash_report() -> Option<String> {
    let out = crate::spawn::output(&mut std::process::Command::new("/sbin/lucina-crash")).ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What the power button does: `/proc/lucina-power` says whether the kernel has it on.
pub fn power_rows(state: Option<&str>) -> Rows {
    let on = state.is_some_and(|s| s.lines().any(|l| l == "mode on"));
    let mut rows = vec![
        if on {
            row(
                "Power button",
                "Press: screen off. Press again: back as it was",
            )
        } else {
            row("Power button", "Not in use: lucina.power=on turns it on")
        },
        row("Hold it", "The firmware's hard power-off, always"),
        row("Shut down", "On the Home screen"),
    ];
    if let Some(us) = state
        .into_iter()
        .flat_map(str::lines)
        .find_map(|l| l.strip_prefix("press_to_light_us "))
    {
        rows.push(row(
            "Last wake",
            format!("{us} us from the key to the backlight"),
        ));
    }
    rows
}

/// The rows of the Power page that can be changed, in the order `main.rs` steps them: the light, the two screen-off timers,
/// "Sleep after" and "Standby after" (the owner's, 2026-11-08). How many of [`screen_rows`] can be changed: the rest only say
/// something.
pub const ADJUSTABLE_ROWS: usize = 5;

pub fn screen_rows(b: &Backlight) -> Rows {
    vec![
        row(
            "Brightness",
            format!("{} %", backlight::percent(b.level, b.max)),
        ),
        row(
            "Screen off on battery",
            backlight::choice_text(b.battery_min),
        ),
        row(
            "Screen off on the charger",
            backlight::choice_text(b.charger_min),
        ),
        row("Sleep after", backlight::choice_text(b.sleep_min)),
        row("Standby after", backlight::choice_text(b.standby_min)),
        row(
            "Screen kept on by",
            b.held_by.clone().unwrap_or_else(|| "nothing".into()),
        ),
    ]
}

/// The section the screen's rows are in.
pub const SCREEN_SECTION: &str = "Power";

pub fn page(
    d: &Display,
    k: f32,
    inputs: &[(String, String)],
    zone: &str,
    network: &Network,
    crashes: Option<&str>,
    screen: Option<&Backlight>,
) -> SettingsPage {
    let mut input: Rows = inputs
        .iter()
        .map(|(n, name)| row(n, name.clone()))
        .collect();
    if input.is_empty() {
        input.push(row("Devices", "none yet"));
    }
    input.push(row("Layout", "US"));
    let screen = screen.map(screen_rows).unwrap_or_default();
    let mut power = screen.clone();
    power.extend(power_rows(
        fs::read_to_string("/proc/lucina-power").ok().as_deref(),
    ));
    let sections = vec![
        ("Network", network_rows(network)),
        (
            "Battery",
            battery_rows(system::battery().as_ref(), adapter_online()),
        ),
        (SCREEN_SECTION, power),
        ("Storage", storage_rows()),
        ("Memory", memory_rows()),
        ("CPU", cpu_rows()),
        ("Build", build_rows()),
        ("Crashes", crash_rows(crashes)),
        (
            "Display",
            vec![
                row("Resolution", format!("{} × {}", d.width, d.height)),
                row("Pixel format", d.format.clone()),
                row("Output", d.driver.clone()),
                row("Scale", format!("{k:.2}")),
                row("Rendering", "software, one thread"),
            ],
        ),
        ("Input", input),
        (
            "About",
            vec![
                row("System", format!("{}, LucinaOS", ui::system_name())),
                row("Time zone", zone),
                row(
                    "Kernel build",
                    format!("Lucina {} (LucinaOS)", ui::system_name()),
                ),
                row("Built with", "Rust, drawn in software"),
                row("Fonts", "DejaVu Sans, DejaVu Sans Mono"),
            ],
        ),
    ];
    let adjustable = sections
        .iter()
        .position(|(n, _)| *n == SCREEN_SECTION)
        .filter(|_| !screen.is_empty())
        .map(|i| (i, ADJUSTABLE_ROWS));
    SettingsPage {
        sections,
        adjustable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{Interface, parse_wifi};

    #[test]
    fn power_section_says_what_the_button_does() {
        let on = power_rows(Some("mode on\nstate awake\npress_to_light_us 412\n"));
        assert!(on[0].1.starts_with("Press"));
        assert!(on.iter().any(|r| r.0 == "Last wake" && r.1.contains("412")));
        for state in [None, Some("mode off\n"), Some("mode report\n")] {
            let off = power_rows(state);
            assert!(off[0].1.starts_with("Not in use"), "{state:?}");
            assert!(off.iter().all(|r| r.0 != "Last wake"));
        }
    }

    #[test]
    fn screen_rows_lead_the_power_page() {
        let d = Display {
            width: 1920,
            height: 1080,
            format: "XRGB8888".into(),
            driver: "test".into(),
        };
        let n = Network::default();
        let b = Backlight::unattached(100, 50, 5, 0);
        let p = page(&d, 1.0, &[], "UTC", &n, None, Some(&b));
        let (s, rows) = p.adjustable.unwrap();
        assert_eq!(rows, 5);
        let (name, power) = &p.sections[s];
        assert_eq!(*name, SCREEN_SECTION);
        assert_eq!(power[0], row("Brightness", "50 %"));
        assert_eq!(power[1], row("Screen off on battery", "5 minutes"));
        assert_eq!(power[2], row("Screen off on the charger", "Never"));
        assert_eq!(power[3], row("Sleep after", "15 minutes"));
        assert_eq!(power[4], row("Standby after", "1 hour"));
        assert_eq!(power[5], row("Screen kept on by", "nothing"));
        assert_eq!(power[6].0, "Power button");
        // Without a device the page is as it was.
        let p = page(&d, 1.0, &[], "UTC", &n, None, None);
        assert_eq!(p.adjustable, None);
        let power = &p
            .sections
            .iter()
            .find(|(n, _)| *n == SCREEN_SECTION)
            .unwrap()
            .1;
        assert_eq!(power[0].0, "Power button");
    }

    #[test]
    fn network_section() {
        let mut n = Network::default();
        n.interfaces = vec![Interface {
            name: "eth0".into(),
            up: true,
            mac: Some("00:e0:4c:36:05:98".into()),
            ipv4: vec!["192.168.1.47/24".into()],
        }];
        n.routes = vec![("eth0".into(), "192.168.1.1".into())];
        let rows = network_rows(&n);
        assert!(rows.contains(&row("eth0", "up, 192.168.1.47/24")));
        assert!(rows.contains(&row("Gateway", "192.168.1.1 on eth0")));
        assert!(rows.contains(&row("Wi-Fi", "off")));
        n.wifi = Some(parse_wifi(
            "state connected\nssid 46\nbss aa 5180 -60 wpa2 46\nbss bb 2412 -80 wpa2 47\n",
        ));
        let rows = network_rows(&n);
        assert!(rows.contains(&row("Wi-Fi", "connected")));
        assert!(rows.contains(&row("Wi-Fi network", "F")));
        assert!(rows.contains(&row("Signal", "-60 dBm, good")));
        assert!(rows.contains(&row("Frequency", "5180 MHz, wpa2")));
        assert!(rows.contains(&row("Also heard", "G (-80 dBm)")));
    }

    #[test]
    fn wifi_rows_hold_no_passphrase() {
        // The status has no such line, and a line that is not known goes nowhere.
        let mut n = Network::default();
        n.wifi = Some(parse_wifi("state connected\npsk hunter2\nsecret hunter2\n"));
        let text = format!("{:?}", network_rows(&n));
        assert!(!text.contains("hunter2"));
    }

    #[test]
    fn battery_section() {
        let b = Battery {
            percent: 100,
            status: "Full".into(),
            details: [
                ("voltage_now", "8655000"),
                ("temp", "258"),
                ("charge_now", "5475000"),
                ("charge_full", "5475000"),
                ("charge_full_design", "5320000"),
                ("cycle_count", "31"),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        };
        let rows = battery_rows(Some(&b), Some(true));
        assert!(rows.contains(&row("Charge", "100% full")));
        assert!(rows.contains(&row("Adapter", "connected")));
        assert!(
            rows.contains(&row("Voltage", "8.65 V")) || rows.contains(&row("Voltage", "8.66 V"))
        );
        assert!(rows.contains(&row("Temperature", "25.8 °C")));
        assert!(rows.contains(&row("Capacity", "5475 of 5475 mAh")));
        assert!(rows.contains(&row("Health", "102 % of the 5320 mAh it was made with")));
        assert!(rows.contains(&row("Cycles", "31")));
        assert_eq!(battery_rows(None, None).len(), 1);
    }

    #[test]
    fn mounts_skip_the_pseudo_ones() {
        let m = "rootfs / ramfs rw 0 0\nproc /proc proc rw 0 0\ntmpfs /dev/shm tmpfs rw 0 0\n";
        assert_eq!(
            parse_mounts(m),
            vec![
                ("/".to_string(), "ramfs".to_string()),
                ("/dev/shm".to_string(), "tmpfs".to_string())
            ]
        );
    }

    #[test]
    fn crash_text_is_cleaned() {
        assert_eq!(
            crash_rows(Some("boot 2, clean\n\x1b[31mred\x07\n\n")),
            vec![row("", "boot 2, clean"), row("", "[31mred")]
        );
        assert_eq!(crash_rows(None).len(), 1);
    }
}
