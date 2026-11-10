//! lucina-wifi: asks Juno's kernel for Wi-Fi networks and connects to one, through `/dev/lucina-wifi`
//! (kernel patch 0626, `device::qcom_wifi::control`).
//!
//! ```text
//! lucina-wifi status
//! lucina-wifi scan                 waits for the scan and lists what it found
//! lucina-wifi connect [file]       waits until the network is authorised
//! lucina-wifi connect --stdin      the same, with the credentials from stdin
//! lucina-wifi connect --saved      the same, from the initramfs or the disk; nothing to do if there are none (the boot)
//! lucina-wifi disconnect
//! lucina-wifi save --stdin         keeps the credentials on the laptop's EFI partition, for the next boot
//! lucina-wifi saved                says whether there are any; never what they are
//! lucina-wifi forget               removes them from the partition
//! ```
//!
//! The credentials are `psk=<passphrase, or 64 hex digits>` and, if given, `ssid=<name>`, one per line, from
//! `/etc/lucina/wifi.conf` (`scripts/remote-initramfs.sh` puts `GBGO_WIFI_PSK` and `GBGO_WIFI_SSID` there, mode 600), another
//! file, or stdin. This program never prints the passphrase, and the kernel keeps it only until it has derived the key.
//!
//! Without a name it scans and takes the strongest network whose name starts with `FRITZ!Box`, the owner's only one, and says
//! which; if none matches it lists what it heard and stays disconnected.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use lucina_remote::esp;
use std::{
    fs,
    io::{self, Read, Write},
    process::ExitCode,
    thread::sleep,
    time::{Duration, Instant},
};

const DEVICE: &str = "/dev/lucina-wifi";
const CONF: &str = "/etc/lucina/wifi.conf";
const FRITZ: &[u8] = b"FRITZ!Box";
const DISK_WAIT: Duration = Duration::from_secs(120);

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

fn command(line: &str) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(DEVICE)?
        .write_all(line.as_bytes())
}

fn status() -> io::Result<String> {
    fs::read_to_string(DEVICE)
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
}

/// The status with SSIDs as text. A network name is whatever bytes the access point sent, so anything unprintable stays hex.
fn readable(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        match line.strip_prefix("bss ") {
            Some(rest) => {
                let mut w: Vec<&str> = rest.split(' ').collect();
                let name = w.pop().unwrap_or("");
                let raw = unhex(name);
                let shown = match std::str::from_utf8(&raw) {
                    Ok(s) if !s.chars().any(char::is_control) => s.to_string(),
                    _ => format!("hex:{name}"),
                };
                out.push_str(&format!("{}  {}\n", w.join(" "), shown));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
}

/// Waits until the `state` line is one of `wanted` or a `failed` line appears.
fn wait_for(wanted: &[&str], limit: Duration) -> Result<String, String> {
    let started = Instant::now();
    loop {
        let text = status().map_err(|err| format!("{DEVICE}: {err}"))?;
        if let Some(state) = field(&text, "state")
            && wanted.contains(&state)
        {
            return Ok(text);
        }
        if let Some(why) = field(&text, "failed") {
            return Err(why.to_string());
        }
        if started.elapsed() > limit {
            return Err(format!(
                "no answer in {} s:\n{}",
                limit.as_secs(),
                readable(&text)
            ));
        }
        sleep(Duration::from_millis(200));
    }
}

/// The name, if the credentials give one, and the passphrase.
fn parse_conf(text: &str) -> Result<(Option<Vec<u8>>, Vec<u8>), String> {
    let (mut ssid, mut psk) = (None, None);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ssid=") {
            ssid = Some(v.as_bytes().to_vec()).filter(|s| !s.is_empty());
        } else if let Some(v) = line.strip_prefix("psk=") {
            psk = Some(v.as_bytes().to_vec());
        }
    }
    match psk {
        Some(p) if !p.is_empty() => Ok((ssid, p)),
        _ => Err("the credentials need a psk= line".to_string()),
    }
}

/// From a status: the strongest BSS whose name starts with `FRITZ!Box`, as (name, dBm), and how many matched.
fn pick_fritz(status: &str) -> Option<(Vec<u8>, i32, usize)> {
    let mut best: Option<(Vec<u8>, i32)> = None;
    let mut matched = 0;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("bss ") else {
            continue;
        };
        let w: Vec<&str> = rest.split(' ').collect();
        let [_, _, dbm, _, name] = w[..] else {
            continue;
        };
        let (name, Ok(dbm)) = (unhex(name), dbm.parse::<i32>()) else {
            continue;
        };
        if name.starts_with(FRITZ) {
            matched += 1;
            if best.as_ref().is_none_or(|(_, b)| dbm > *b) {
                best = Some((name, dbm));
            }
        }
    }
    best.map(|(name, dbm)| (name, dbm, matched))
}

/// A line in the kernel log as well as here: this runs in the background at boot, with nobody watching its output.
fn tell(line: &str) {
    println!("{line}");
    if let Ok(mut log) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
        let _ = writeln!(log, "lucina-wifi: {line}");
    }
}

/// The name to join when the credentials give none.
fn find_network() -> Result<Vec<u8>, String> {
    wait_for(&["idle", "connected"], Duration::from_secs(180))?;
    command(
        "scan
",
    )
    .map_err(|err| format!("{DEVICE}: {err}"))?;
    sleep(Duration::from_millis(300));
    let status = wait_for(&["idle", "connected"], Duration::from_secs(60))?;
    match pick_fritz(&status) {
        Some((name, dbm, matched)) => {
            tell(&format!(
                "taking {} at {dbm} dBm, the strongest of {matched} FRITZ!Box entries",
                String::from_utf8_lossy(&name)
            ));
            Ok(name)
        }
        None => {
            tell(&format!(
                "no FRITZ!Box network heard, staying disconnected:
{}",
                readable(&status)
            ));
            Err("no FRITZ!Box network".to_string())
        }
    }
}

/// A WPA passphrase is 8 to 63 printable characters, and a PSK 64 hex digits. Better to hear it now than at every boot.
fn check_psk(psk: &[u8]) -> Result<(), String> {
    let printable = psk.iter().all(|b| (0x20..0x7f).contains(b));
    let fits =
        (8..=63).contains(&psk.len()) || (psk.len() == 64 && psk.iter().all(u8::is_ascii_hexdigit));
    if printable && fits {
        Ok(())
    } else {
        Err(format!(
            "a passphrase is 8 to 63 printable characters, or 64 hex digits; this is {} bytes",
            psk.len()
        ))
    }
}

fn connect(text: &str) -> Result<(), String> {
    let (ssid, psk) = parse_conf(text)?;
    let ssid = match ssid {
        Some(ssid) => ssid,
        None => find_network()?,
    };
    let line = format!("connect {} {}\n", hex(&ssid), hex(&psk));
    command(&line).map_err(|err| format!("{DEVICE}: {err}"))?;
    println!("connecting");
    sleep(Duration::from_millis(300));
    print!(
        "{}",
        readable(&wait_for(&["connected"], Duration::from_secs(300))?)
    );
    Ok(())
}

/// At boot: the initramfs's credentials, or the disk's. The disk may take a while to appear, and may never (a kernel without
/// UFS); neither is an error, as there is nothing to connect with.
fn connect_saved() -> Result<(), String> {
    if let Ok(text) = fs::read_to_string(CONF) {
        return connect(&text);
    }
    let started = Instant::now();
    let saved = loop {
        match esp::read_saved() {
            Ok(saved) => break saved,
            Err(_) if started.elapsed() < DISK_WAIT => sleep(Duration::from_secs(1)),
            Err(err) => {
                tell(&format!("no saved credentials to read: {err}"));
                return Ok(());
            }
        }
    };
    match saved {
        Some(saved) => {
            tell("using the saved credentials");
            connect(&String::from_utf8_lossy(&saved))
        }
        None => Ok(()),
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let device = |err: io::Error| format!("{DEVICE}: {err}");
    match args.first().map(String::as_str) {
        None | Some("status") => print!("{}", readable(&status().map_err(device)?)),
        Some("scan") => {
            command("scan\n").map_err(device)?;
            sleep(Duration::from_millis(300));
            print!(
                "{}",
                readable(&wait_for(&["idle", "connected"], Duration::from_secs(30))?)
            );
        }
        Some("connect") => {
            if args.get(1).is_some_and(|a| a == "--saved") {
                return connect_saved();
            }
            let text = match args.get(1).map(String::as_str) {
                Some("--stdin") => {
                    let mut text = String::new();
                    io::stdin()
                        .read_to_string(&mut text)
                        .map_err(|err| err.to_string())?;
                    text
                }
                Some(path) => fs::read_to_string(path).map_err(|err| format!("{path}: {err}"))?,
                None => fs::read_to_string(CONF).map_err(|err| format!("{CONF}: {err}"))?,
            };
            connect(&text)?;
        }
        Some("disconnect") => command("disconnect\n").map_err(device)?,
        Some("save") => {
            if args.get(1).is_none_or(|a| a != "--stdin") {
                return Err("save takes its credentials from stdin: save --stdin".to_string());
            }
            let mut text = String::new();
            io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| err.to_string())?;
            let (ssid, psk) = parse_conf(&text)?;
            check_psk(&psk)?;
            let mut saved = Vec::new();
            saved.extend_from_slice(b"psk=");
            saved.extend_from_slice(&psk);
            saved.push(b'\n');
            if let Some(ssid) = &ssid {
                saved.extend_from_slice(b"ssid=");
                saved.extend_from_slice(ssid);
                saved.push(b'\n');
            }
            esp::save(&saved)?;
            tell("the credentials are saved: the next boot with lucina.wifi=on connects by itself");
        }
        Some("saved") => match esp::read_saved()? {
            Some(saved) => {
                let (ssid, _) = parse_conf(&String::from_utf8_lossy(&saved))?;
                match ssid {
                    Some(ssid) => println!("saved, for {}", String::from_utf8_lossy(&ssid)),
                    None => println!("saved, for the strongest FRITZ!Box"),
                }
            }
            None => println!("nothing saved"),
        },
        Some("forget") => {
            if esp::forget()? {
                tell("the saved credentials are gone");
            } else {
                println!("nothing saved");
            }
        }
        Some("migrate") => {
            if esp::migrate()? {
                tell("the credentials moved from the EFI partition to /data");
            } else {
                println!("nothing saved on the EFI partition");
            }
        }
        Some(other) => {
            return Err(format!(
                "unknown command {other}: status, scan, connect, disconnect, save, saved, forget, migrate"
            ));
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("lucina-wifi: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bss(name: &str, dbm: i32) -> String {
        format!(
            "bss 112233445566 2412 {dbm} wpa2-psk {}\n",
            hex(name.as_bytes())
        )
    }

    #[test]
    fn the_strongest_fritz_box_wins_and_others_do_not_count() {
        let status = format!(
            "state idle\n{}{}{}",
            bss("FRITZ!Box 7590 XY", -70),
            bss("Neighbour", -30),
            bss("FRITZ!Box 7590 XY", -50)
        );
        let (name, dbm, matched) = pick_fritz(&status).unwrap();
        assert_eq!(name, b"FRITZ!Box 7590 XY");
        assert_eq!((dbm, matched), (-50, 2));
    }

    #[test]
    fn no_fritz_box_is_none() {
        assert!(pick_fritz(&format!("state idle\n{}", bss("Neighbour", -30))).is_none());
        assert!(pick_fritz("state idle\nbss broken\n").is_none());
    }

    #[test]
    fn a_passphrase_is_checked_before_it_is_kept() {
        assert!(check_psk(b"long enough").is_ok());
        assert!(check_psk(b"short").is_err());
        assert!(check_psk(&[b'a'; 64]).is_ok());
        assert!(check_psk(&[b'g'; 64]).is_err());
        assert!(check_psk(&[b'a'; 65]).is_err());
        assert!(check_psk(b"has\x01a control").is_err());
        assert!(
            !check_psk(b"has\x01a control")
                .unwrap_err()
                .contains("control")
        );
    }

    #[test]
    fn the_name_is_optional_and_the_passphrase_is_not() {
        let (ssid, psk) = parse_conf("psk=secret\n").unwrap();
        assert!(ssid.is_none());
        assert_eq!(psk, b"secret");
        let (ssid, _) = parse_conf("ssid=Home\npsk=secret\n").unwrap();
        assert_eq!(ssid.unwrap(), b"Home");
        assert!(parse_conf("ssid=Home\n").is_err());
    }
}
