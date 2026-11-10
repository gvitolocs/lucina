//! lucina-timesync: sets Juno's wall clock over the network. The laptop's kernel starts in 1970 (it has no driver for the
//! PMIC's real-time clock), so until this has run, nothing should show a time.
//!
//! SNTP (RFC 4330) over UDP: the default gateway first, since a FRITZ!Box answers and needs no name lookup, then public
//! servers. An answer counts only if it echoes our transmit time, comes from a synchronised server, and is later than this
//! program's floor. Then `clock_settime` (kernel patch 0406). Tries again often until it has worked, then hourly.
//!
//! ```text
//! lucina-timesync           runs for good; remote.sh starts it
//! lucina-timesync --once    one round; exit 0 if the clock was set
//! ```
//!
//! SNTP is not authenticated: someone on the network path can lie about the time, within the floor. Nothing here relies on
//! the clock for security.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::io;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::thread::sleep;
use std::time::Duration;

const PUBLIC_SERVERS: [&str; 3] = [
    "dk.pool.ntp.org:123",
    "pool.ntp.org:123",
    "time.cloudflare.com:123",
];
/// Seconds from 1900 (NTP's era) to 1970.
const NTP_UNIX: i64 = 2_208_988_800;
/// 2026-01-01: no answer before this is believed.
const FLOOR_UNIX: i64 = 1_767_225_600;
/// 2100-01-01: nor after this.
const CEILING_UNIX: i64 = 4_102_444_800;
const RETRY: Duration = Duration::from_secs(10);
const RESYNC: Duration = Duration::from_secs(3600);

fn now_ns() -> i128 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
    i128::from(ts.tv_sec) * 1_000_000_000 + i128::from(ts.tv_nsec)
}

/// An NTP timestamp (seconds since 1900, 32-bit fraction) as Unix nanoseconds. Era 0 ends in 2036; later times are in era 1.
fn ntp_to_ns(raw: u64) -> i128 {
    let mut secs = i128::from(raw >> 32);
    if secs < i128::from(FLOOR_UNIX + NTP_UNIX) {
        secs += 1 << 32;
    }
    let frac = (i128::from(raw & 0xffff_ffff) * 1_000_000_000) >> 32;
    (secs - i128::from(NTP_UNIX)) * 1_000_000_000 + frac
}

fn request(token: u64) -> [u8; 48] {
    let mut p = [0u8; 48];
    p[0] = (4 << 3) | 3; // version 4, client
    p[40..48].copy_from_slice(&token.to_be_bytes());
    p
}

/// The clock's correct value as an offset from the local clock, from an answer. `t1` and `t4` are the local clock when the
/// request left and the answer came.
fn offset_ns(answer: &[u8], token: u64, t1: i128, t4: i128) -> Result<i128, String> {
    if answer.len() < 48 {
        return Err(format!("{} bytes", answer.len()));
    }
    let word = |at: usize| u64::from_be_bytes(answer[at..at + 8].try_into().unwrap());
    if answer[0] >> 6 == 3 {
        return Err("the server says it is not synchronised".into());
    }
    if answer[0] & 7 != 4 {
        return Err(format!("mode {}", answer[0] & 7));
    }
    if answer[1] == 0 || answer[1] > 15 {
        return Err(format!("stratum {}", answer[1]));
    }
    if word(24) != token {
        return Err("not an answer to our request".into());
    }
    let (t2, t3) = (ntp_to_ns(word(32)), ntp_to_ns(word(40)));
    let unix = t3 / 1_000_000_000;
    if !(FLOOR_UNIX..CEILING_UNIX).contains(&(unix as i64)) {
        return Err(format!("time {unix} is not believable"));
    }
    Ok(((t2 - t1) + (t3 - t4)) / 2)
}

fn query(server: SocketAddr, token: u64) -> Result<i128, String> {
    let bind = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let run = || -> io::Result<(Vec<u8>, i128, i128)> {
        let sock = UdpSocket::bind(bind)?;
        sock.connect(server)?;
        sock.set_read_timeout(Some(Duration::from_secs(3)))?;
        let t1 = now_ns();
        sock.send(&request(token))?;
        let mut buf = [0u8; 128];
        let n = sock.recv(&mut buf)?;
        Ok((buf[..n].to_vec(), t1, now_ns()))
    };
    let (answer, t1, t4) = run().map_err(|e| e.to_string())?;
    offset_ns(&answer, token, t1, t4)
}

fn set_clock(offset: i128) -> io::Result<()> {
    let target = now_ns() + offset;
    let ts = libc::timespec {
        tv_sec: (target / 1_000_000_000) as _,
        tv_nsec: (target % 1_000_000_000) as libc::c_long,
    };
    // SAFETY: `ts` is a valid timespec.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The default gateways from `/proc/net/route`.
fn gateways() -> Vec<SocketAddr> {
    std::fs::read_to_string("/proc/net/route")
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if u32::from_str_radix(f.get(1)?, 16).ok()? != 0 {
                return None;
            }
            let [a, b, c, d] = u32::from_str_radix(f.get(2)?, 16).ok()?.to_le_bytes();
            format!("{a}.{b}.{c}.{d}:123").parse().ok()
        })
        .collect()
}

/// One round: the first server that gives a good answer sets the clock.
fn round(token: u64) -> bool {
    let routes = gateways();
    if routes.is_empty() {
        return false;
    }
    let public = PUBLIC_SERVERS.iter().flat_map(|name| {
        name.to_socket_addrs()
            .map(|a| a.take(2).collect::<Vec<_>>())
            .unwrap_or_default()
    });
    for server in routes.into_iter().chain(public) {
        match query(server, token) {
            Ok(offset) => match set_clock(offset) {
                Ok(()) => {
                    let line = format!(
                        "lucina-timesync: clock set from {server}, moved by {} s\n",
                        offset / 1_000_000_000
                    );
                    eprint!("{line}");
                    // In the kernel log too, where the saved log and the other clock's line are. One write is one record.
                    let _ = std::fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/kmsg")
                        .and_then(|mut kmsg| std::io::Write::write_all(&mut kmsg, line.as_bytes()));
                    return true;
                }
                Err(e) => {
                    eprintln!("lucina-timesync: clock_settime: {e}");
                    return false;
                }
            },
            Err(e) => eprintln!("lucina-timesync: {server}: {e}"),
        }
    }
    false
}

fn token(n: u64) -> u64 {
    // Not a secret: it only ties an answer to this request, and mixes the time in.
    (now_ns() as u64).rotate_left(17) ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1
}

fn main() {
    let once = std::env::args().nth(1).as_deref() == Some("--once");
    let mut n = 0;
    loop {
        n += 1;
        let ok = round(token(n));
        if once {
            std::process::exit(if ok { 0 } else { 1 });
        }
        sleep(if ok { RESYNC } else { RETRY });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2026-11-01 13:05:00 UTC.
    const WHEN: i64 = 1_793_538_300;

    fn answer(token: u64, unix: i64) -> [u8; 48] {
        let mut a = [0u8; 48];
        a[0] = (4 << 3) | 4;
        a[1] = 2;
        a[24..32].copy_from_slice(&token.to_be_bytes());
        let t = ((unix + NTP_UNIX) as u64) << 32;
        a[32..40].copy_from_slice(&t.to_be_bytes());
        a[40..48].copy_from_slice(&t.to_be_bytes());
        a
    }

    #[test]
    fn offset_from_a_good_answer() {
        // The local clock says 10 s after the epoch, the request took a second each way.
        let t1 = 10 * 1_000_000_000;
        let t4 = 12 * 1_000_000_000;
        let got = offset_ns(&answer(7, WHEN), 7, t1, t4).unwrap();
        assert_eq!(got, (i128::from(WHEN) - 11) * 1_000_000_000);
    }

    #[test]
    fn bad_answers_set_nothing() {
        let good = answer(7, WHEN);
        assert!(offset_ns(&good, 8, 0, 0).is_err(), "someone else's answer");
        assert!(offset_ns(&good[..40], 7, 0, 0).is_err(), "short");
        let mut a = good;
        a[0] |= 3 << 6;
        assert!(offset_ns(&a, 7, 0, 0).is_err(), "unsynchronised");
        let mut a = good;
        a[1] = 0;
        assert!(offset_ns(&a, 7, 0, 0).is_err(), "kiss of death");
        let mut a = good;
        a[0] = (4 << 3) | 3;
        assert!(offset_ns(&a, 7, 0, 0).is_err(), "not a server");
        assert!(offset_ns(&answer(7, 1000), 7, 0, 0).is_err(), "1970");
    }

    #[test]
    fn eras() {
        // After 2036 the seconds wrap: they still mean the future.
        let raw = (((2_200_000_000i64 + NTP_UNIX) % (1i64 << 32)) as u64) << 32;
        assert_eq!(ntp_to_ns(raw), 2_200_000_000i128 * 1_000_000_000);
        // The fraction is half a second.
        assert_eq!(
            ntp_to_ns(1u64 << 31),
            ((1i128 << 32) - i128::from(NTP_UNIX)) * 1_000_000_000 + 500_000_000
        );
    }

    #[test]
    fn request_is_a_client_packet() {
        let p = request(0x0102_0304_0506_0708);
        assert_eq!(p[0], 0x23);
        assert_eq!(&p[40..48], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
