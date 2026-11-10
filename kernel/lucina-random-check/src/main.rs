//! Checks of Juno's randomness from user space. Prints a line per check and `random-check: passed` or `random-check: FAILED`.
//!
//! ```text
//! lucina-random-check ready         # a seeded kernel: flags, devices, poll, ioctls, what /proc says
//! lucina-random-check unseeded      # a kernel with no credited source: refuses, then waits, then is seeded
//! lucina-random-check speed         # how fast /dev/urandom is
//! lucina-random-check raw [bytes [file]]  # the hardware's raw output (lucina.prng.raw=on, lucina.rngb.raw=on), or bytes
//!                                   # saved from it: rate and sanity statistics
//! ```
//!
//! **The statistics say only that a source is not broken.** A generator from a known key passes them too; they are no evidence
//! that anyone is unable to predict its output.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::fd::AsRawFd,
    os::unix::fs::OpenOptionsExt,
    process::ExitCode,
    time::Instant,
};

const GRND_NONBLOCK: u32 = 1;
const GRND_RANDOM: u32 = 2;
const GRND_INSECURE: u32 = 4;
const RNDGETENTCNT: libc::c_ulong = 0x8004_5200;
const RNDRESEEDCRNG: libc::c_ulong = 0x5207;
const REPORT: &str = "/proc/lucina-random";
const RAW: &str = "/proc/lucina-random-raw";

struct Checks {
    failed: u32,
}

impl Checks {
    fn check(&mut self, what: &str, ok: bool, detail: impl std::fmt::Display) {
        println!("{} {what}: {detail}", if ok { "ok" } else { "FAILED" });
        if !ok {
            self.failed += 1;
        }
    }
}

/// `Ok(bytes)` or the errno.
fn getrandom(out: &mut [u8], flags: u32) -> Result<usize, i32> {
    // SAFETY: fills a live buffer of its own length.
    let n = unsafe { libc::getrandom(out.as_mut_ptr().cast(), out.len(), flags) };
    if n < 0 { Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0)) } else { Ok(n as usize) }
}

fn report_value(key: &str) -> Option<String> {
    fs::read_to_string(REPORT).ok()?.lines().find_map(|line| {
        line.strip_prefix(key)?.strip_prefix(' ').map(str::to_string)
    })
}

fn entropy_count(file: &File) -> Option<i32> {
    let mut bits = 0i32;
    // SAFETY: the ioctl writes one int to a live one.
    (unsafe { libc::ioctl(file.as_raw_fd(), RNDGETENTCNT as _, &mut bits) } == 0).then_some(bits)
}

fn flags(c: &mut Checks) {
    let mut b = [0u8; 32];
    c.check("an unknown flag is refused", getrandom(&mut b, 0x8) == Err(libc::EINVAL), "EINVAL");
    c.check(
        "INSECURE with RANDOM is refused",
        getrandom(&mut b, GRND_INSECURE | GRND_RANDOM) == Err(libc::EINVAL),
        "EINVAL",
    );
    c.check("INSECURE never waits", getrandom(&mut b, GRND_INSECURE) == Ok(32), "32 bytes");
}

fn ready(c: &mut Checks) {
    flags(c);
    let (mut a, mut b) = ([0u8; 64], [0u8; 64]);
    c.check("getrandom answers", getrandom(&mut a, 0) == Ok(64), "64 bytes");
    c.check("NONBLOCK answers", getrandom(&mut b, GRND_NONBLOCK) == Ok(64), "64 bytes");
    c.check("two draws differ", a != b, "different");
    c.check("RANDOM answers", getrandom(&mut b, GRND_RANDOM) == Ok(64), "64 bytes");
    c.check("no draw is zero", a != [0u8; 64], "non-zero");

    let mut long = vec![0u8; 3 * 4096 + 17];
    let n = File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut long));
    c.check("/dev/urandom reads", n.is_ok(), long.len());
    let n = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/random")
        .and_then(|mut f| f.read_exact(&mut long));
    c.check("/dev/random reads, also without blocking", n.is_ok(), long.len());

    let random = File::open("/dev/random").unwrap();
    let mut fd = libc::pollfd { fd: random.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one live pollfd.
    let n = unsafe { libc::poll(&mut fd, 1, 1000) };
    c.check("poll says readable", n == 1 && fd.revents & libc::POLLIN != 0, format!("{n} {:#x}", fd.revents));
    c.check("the ioctl counts 256 bits", entropy_count(&random) == Some(256), format!("{:?}", entropy_count(&random)));

    let before = report_value("written_bytes").and_then(|v| v.parse::<u64>().ok());
    let wrote = OpenOptions::new().write(true).open("/dev/urandom").and_then(|mut f| f.write_all(&[1u8; 100]));
    let after = report_value("written_bytes").and_then(|v| v.parse::<u64>().ok());
    c.check(
        "a write goes into the pool, and counts for nothing",
        wrote.is_ok() && after.is_some_and(|a| Some(a - 100) == before) && entropy_count(&random) == Some(256),
        format!("{before:?} to {after:?}"),
    );

    let before = report_value("generation").and_then(|v| v.parse::<u64>().ok());
    // SAFETY: an ioctl without a pointer.
    let n = unsafe { libc::ioctl(random.as_raw_fd(), RNDRESEEDCRNG as _) };
    let after = report_value("generation").and_then(|v| v.parse::<u64>().ok());
    c.check("RNDRESEEDCRNG moves to a new key", n == 0 && after > before, format!("{before:?} to {after:?}"));

    let exists = fs::metadata("/proc/device-tree/chosen/rng-seed").is_ok();
    c.check("the seed is gone from the device tree", !exists, "no /proc/device-tree/chosen/rng-seed");

    // SAFETY: AT_RANDOM, if it is there, is 16 bytes the kernel put on the stack.
    let at_random = unsafe {
        let p = libc::getauxval(libc::AT_RANDOM) as *const u8;
        (!p.is_null()).then(|| std::slice::from_raw_parts(p, 16).to_vec())
    };
    c.check("AT_RANDOM is there", at_random.is_some_and(|b| b.iter().any(|x| *x != 0)), "16 bytes");

    for key in ["init", "credited_bits", "ready_from", "ready_us", "boot_seed_source", "boot_seed_bits", "hardware", "hardware_first_us", "jitter_bits", "generation"] {
        println!("  {key}: {}", report_value(key).unwrap_or_else(|| "?".to_string()));
    }
    c.check("/proc says Ready", report_value("init").as_deref() == Some("Ready"), "Ready");
}

fn unseeded(c: &mut Checks) {
    flags(c);
    let mut b = [0u8; 32];
    let random = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open("/dev/random").unwrap();
    let was_ready = report_value("init").as_deref() == Some("Ready");
    c.check("not seeded when this starts", !was_ready, report_value("init").unwrap_or_default());
    c.check("NONBLOCK says try again", getrandom(&mut b, GRND_NONBLOCK) == Err(libc::EAGAIN), "EAGAIN");
    let mut read = &random;
    let refused = read.read(&mut b).err().and_then(|e| e.raw_os_error());
    c.check("/dev/random with O_NONBLOCK says try again", refused == Some(libc::EAGAIN), format!("{refused:?}"));
    let mut fd = libc::pollfd { fd: random.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: one live pollfd.
    let n = unsafe { libc::poll(&mut fd, 1, 0) };
    c.check("poll says not readable", n == 0, n);

    let started = Instant::now();
    c.check("a plain getrandom waits and then answers", getrandom(&mut b, 0) == Ok(32), format!("after {} ms", started.elapsed().as_millis()));
    c.check("it did wait", started.elapsed().as_millis() >= 500, format!("{} ms", started.elapsed().as_millis()));
    c.check("NONBLOCK answers now", getrandom(&mut b, GRND_NONBLOCK) == Ok(32), "32 bytes");
    c.check("/proc says Ready", report_value("init").as_deref() == Some("Ready"), "Ready");
    println!("  ready_from: {}", report_value("ready_from").unwrap_or_default());
    println!("  jitter_bits: {}", report_value("jitter_bits").unwrap_or_default());
}

fn speed(c: &mut Checks) {
    let mut f = File::open("/dev/urandom").unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let started = Instant::now();
    for _ in 0..16 {
        f.read_exact(&mut buf).unwrap();
    }
    let s = started.elapsed().as_secs_f64();
    println!("/dev/urandom: 16 MiB in {:.3} s, {:.1} MiB/s", s, 16.0 / s);
    let started = Instant::now();
    let mut b = [0u8; 32];
    for _ in 0..10_000 {
        getrandom(&mut b, 0).unwrap();
    }
    println!("getrandom of 32 bytes: {:.2} us each", started.elapsed().as_secs_f64() * 100.0);
    c.check("speed measured", true, "");
}

/// Monobit, runs (as far as they go on this much), and chi-square of bytes. **Sanity only.**
fn statistics(c: &mut Checks, data: &[u8]) {
    let bits = (data.len() * 8) as f64;
    let ones: u64 = data.iter().map(|b| b.count_ones() as u64).sum();
    let share = ones as f64 / bits;
    // Four standard deviations of a fair coin: 1 in 16,000 for a good source.
    let limit = 4.0 * 0.5 / bits.sqrt();
    c.check("monobit", (share - 0.5).abs() < limit, format!("{share:.6} ones, limit 0.5 +- {limit:.6}"));

    let mut runs = 1u64;
    let mut last = data[0] & 1;
    for byte in data {
        for i in 0..8 {
            let bit = (byte >> i) & 1;
            if bit != last {
                runs += 1;
                last = bit;
            }
        }
    }
    let expected = bits / 2.0;
    let limit = 4.0 * (bits / 4.0).sqrt();
    c.check("runs", (runs as f64 - expected).abs() < limit, format!("{runs}, expected {expected:.0} +- {limit:.0}"));

    let mut counts = [0u64; 256];
    for byte in data {
        counts[*byte as usize] += 1;
    }
    let e = data.len() as f64 / 256.0;
    let chi2: f64 = counts.iter().map(|&n| (n as f64 - e).powi(2) / e).sum();
    // 255 degrees of freedom: mean 255, sigma about 22.6.
    c.check("bytes, chi-square", (chi2 - 255.0).abs() < 4.0 * 22.6, format!("{chi2:.1}, expected 255 +- 90"));

    let mut repeats = 0u32;
    let words: Vec<&[u8]> = data.chunks_exact(4).collect();
    for w in words.windows(2) {
        repeats += (w[0] == w[1]) as u32;
    }
    c.check("no word repeats at once", repeats == 0, repeats);
}

fn raw(c: &mut Checks, want: usize, path: &str) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            c.check(path, false, e);
            return;
        }
    };
    let mut data = vec![0u8; want];
    let started = Instant::now();
    let mut have = 0;
    while have < want {
        match file.read(&mut data[have..]) {
            Ok(0) => break,
            Ok(n) => have += n,
            Err(e) => {
                c.check("raw read", false, e);
                return;
            }
        }
    }
    let s = started.elapsed().as_secs_f64();
    c.check("raw read", have == want, format!("{have} bytes in {s:.3} s, {:.2} MiB/s", have as f64 / s / (1 << 20) as f64));
    data.truncate(have);
    if !data.is_empty() {
        statistics(c, &data);
    }
    println!("These statistics say only that the source is not broken, not that it cannot be predicted.");
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut c = Checks { failed: 0 };
    match args.get(1).map(String::as_str) {
        Some("ready") => ready(&mut c),
        Some("unseeded") => unseeded(&mut c),
        Some("speed") => speed(&mut c),
        Some("raw") => raw(
            &mut c,
            args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1 << 20),
            args.get(3).map_or(RAW, String::as_str),
        ),
        _ => {
            eprintln!("usage: lucina-random-check ready|unseeded|speed|raw [bytes [file]]");
            return ExitCode::from(2);
        }
    }
    if c.failed == 0 {
        println!("random-check: passed");
        ExitCode::SUCCESS
    } else {
        println!("random-check: FAILED ({})", c.failed);
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statistics_catch_a_broken_source() {
        let mut c = Checks { failed: 0 };
        statistics(&mut c, &vec![0x55u8; 1 << 16]);
        assert!(c.failed >= 2);
    }

    #[test]
    fn statistics_pass_the_host_kernel() {
        let mut data = vec![0u8; 1 << 20];
        File::open("/dev/urandom").unwrap().read_exact(&mut data).unwrap();
        let mut c = Checks { failed: 0 };
        statistics(&mut c, &data);
        assert_eq!(c.failed, 0);
    }
}
