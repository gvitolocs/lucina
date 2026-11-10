//! lucina-bootlog: saves this boot's kernel log and a hardware report to the USB stick, so that a laptop test can be
//! read on the Mac afterwards instead of from photos of a scrolling screen. /etc/lucina/remote.sh starts it.
//!
//! It waits up to a minute for a USB disk with a FAT file system that holds the folder `lucinaos/`, makes
//! `lucinaos/juno-log-<N>.txt` there (the next free N) and for five minutes appends the new kernel log lines every 10 s,
//! each time flushed through to the stick, since the owner may just pull the power. A report follows whenever it has
//! changed, and at the end. It only ever writes that one file: other disks are never mounted writable, and it neither
//! formats nor partitions anything. Without such a stick it writes nothing and ends.
//!
//! The crashes the kernel saved in RAM (/sys/fs/pstore) go on the stick too, as `lucinaos/juno-crash-<boot>-<seq>.txt`, with
//! the whole log of a boot that ended badly as `lucinaos/juno-lastlog-<boot>.txt`, since a power cut would lose them.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Write},
    os::unix::fs::{FileTypeExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
    thread::sleep,
    time::{Duration, Instant},
};

use lucina_bootlog::{
    LOG_DIR, crash_file_name, format_record, is_usb_device_line, is_usb_disk_name,
    last_console_file_name, log_file_name, next_log_number, order_candidates, parse_saved_status,
};

const MOUNT_POINT: &str = "/mnt/lucina-stick";
const STICK_WAIT: Duration = Duration::from_secs(60);
const STICK_RETRY: Duration = Duration::from_secs(2);
const LOG_FOR: Duration = Duration::from_secs(300);
const LOG_EVERY: Duration = Duration::from_secs(10);

/// Files whose content goes in the report, unchanged from report to report if nothing happened.
const REPORT_FILES: &[&str] = &["/proc/version", "/proc/cmdline", "/proc/mounts"];

fn main() -> ExitCode {
    let started = Instant::now();
    let Some(dev) = wait_for_stick(started) else {
        println!("lucina-bootlog: no stick with {LOG_DIR}/ found, nothing written");
        return ExitCode::SUCCESS;
    };
    match log_to_stick(&dev, started) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            println!("lucina-bootlog: stopped writing to {dev}: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Looks for the stick until the time is up, and returns its device with the file system mounted writable.
fn wait_for_stick(started: Instant) -> Option<String> {
    fs::create_dir_all(MOUNT_POINT).ok()?;
    loop {
        let mut names: Vec<String> = fs::read_dir("/dev")
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_usb_disk_name(name))
            .collect();
        order_candidates(&mut names);
        for name in names {
            let dev = format!("/dev/{name}");
            if holds_log_dir(&dev) && mount(&dev, false).is_ok() {
                return Some(dev);
            }
        }
        if started.elapsed() >= STICK_WAIT {
            return None;
        }
        sleep(STICK_RETRY);
    }
}

/// Mounts read-only to look. A disk that is not the stick has then seen no write at all.
fn holds_log_dir(dev: &str) -> bool {
    if mount(dev, true).is_err() {
        return false;
    }
    let found = Path::new(MOUNT_POINT)
        .join(LOG_DIR)
        .metadata()
        .is_ok_and(|meta| meta.is_dir());
    let _ = unmount();
    found
}

fn mount(dev: &str, read_only: bool) -> io::Result<()> {
    let flags = if read_only { libc::MS_RDONLY } else { 0 };
    let dev = CString::new(dev)?;
    let target = CString::new(MOUNT_POINT)?;
    // SAFETY: the strings live through the call; vfat takes no binary data, so the last argument may be null.
    let status = unsafe {
        libc::mount(
            dev.as_ptr(),
            target.as_ptr(),
            c"vfat".as_ptr(),
            flags,
            std::ptr::null(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn unmount() -> io::Result<()> {
    let target = CString::new(MOUNT_POINT)?;
    // SAFETY: the string lives through the call.
    if unsafe { libc::umount(target.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Copies what the kernel saved from the last boots, once each. A failure here does not stop the log.
fn save_crashes(dir: &Path) {
    let Ok(status) = fs::read_to_string("/sys/fs/pstore/status") else {
        return;
    };
    let saved = parse_saved_status(&status);
    let copy = |source: String, name: String| {
        let path = dir.join(name);
        if path.exists() {
            return;
        }
        let result = fs::read(&source).and_then(|text| {
            let mut file = File::create(&path)?;
            file.write_all(&text)?;
            file.sync_all()
        });
        match result {
            Ok(()) => println!("lucina-bootlog: saved {}", path.display()),
            Err(err) => println!("lucina-bootlog: {}: {err}", path.display()),
        }
    };
    for (slot, seq, boot) in saved.records {
        copy(
            format!("/sys/fs/pstore/dmesg-lucina-{slot}"),
            crash_file_name(boot, seq),
        );
    }
    if saved.last_boot == "crashed" || saved.last_boot == "vanished" {
        copy(
            "/sys/fs/pstore/console-lucina-0".to_string(),
            last_console_file_name(saved.boot.saturating_sub(1)),
        );
    }
}

fn log_to_stick(dev: &str, started: Instant) -> io::Result<()> {
    let dir = Path::new(MOUNT_POINT).join(LOG_DIR);
    save_crashes(&dir);
    let (path, mut file) = create_log_file(&dir)?;
    println!(
        "lucina-bootlog: writing {} on {dev}",
        path.strip_prefix(MOUNT_POINT).unwrap_or(&path).display()
    );
    let mut kmsg = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/kmsg")?;
    let mut usb_lines = Vec::new();
    let mut last_report = String::new();
    loop {
        let log = drain_kmsg(&mut kmsg, &mut usb_lines);
        let report = stable_report(&usb_lines);
        let finished = started.elapsed() >= LOG_FOR;
        let mut text = log;
        if finished || report != last_report {
            text.push_str(&report_header());
            text.push_str(&report);
            last_report = report;
        }
        if !text.is_empty() {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        if finished {
            break;
        }
        sleep(LOG_EVERY);
    }
    drop(file);
    // The log is complete and the stick clean; the mount is not needed any more.
    unmount()?;
    println!("lucina-bootlog: done");
    Ok(())
}

fn create_log_file(dir: &Path) -> io::Result<(PathBuf, File)> {
    let names: Vec<String> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let path = dir.join(log_file_name(next_log_number(&names)));
    let file = OpenOptions::new()
        .append(true)
        .create_new(true)
        .open(&path)?;
    Ok((path, file))
}

/// Reads every record that is waiting. Lines about USB devices are kept for the report.
fn drain_kmsg(kmsg: &mut File, usb_lines: &mut Vec<String>) -> String {
    let mut text = String::new();
    let mut record = vec![0; 8192];
    loop {
        match kmsg.read(&mut record) {
            Ok(0) => break,
            Ok(len) => {
                let line = format_record(&record[..len]);
                if is_usb_device_line(&line) {
                    usb_lines.push(line.clone());
                }
                text.push_str(&line);
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => break,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.raw_os_error() == Some(libc::EPIPE) => {
                text.push_str("lucina-bootlog: older kernel log lines were overwritten\n");
            }
            Err(err) => {
                text.push_str(&format!("lucina-bootlog: reading /dev/kmsg: {err}\n"));
                break;
            }
        }
    }
    text
}

/// The report's first lines, which differ every time and so are no reason to write a new report.
fn report_header() -> String {
    let uptime = fs::read_to_string("/proc/uptime").unwrap_or_else(|err| format!("{err}\n"));
    format!("\n=== report at uptime {}", uptime.trim_end()) + " ===\n"
}

fn stable_report(usb_lines: &[String]) -> String {
    let mut text = String::from("--- USB devices (from the kernel log)\n");
    usb_lines.iter().for_each(|line| text.push_str(line));
    text.push_str("--- /dev/input\n");
    text.push_str(&input_devices());
    text.push_str("--- block devices\n");
    text.push_str(&block_devices());
    text.push_str("--- network interfaces\n");
    text.push_str(&interfaces());
    for path in REPORT_FILES {
        match fs::read_to_string(path) {
            Ok(content) => {
                text.push_str(&format!("--- {path}\n{}", ensure_newline(content)));
            }
            Err(err) => text.push_str(&format!("--- {path}: {err}\n")),
        }
    }
    text.push_str("=== end of report ===\n\n");
    text
}

fn ensure_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn block_devices() -> String {
    let mut names: Vec<String> = fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_block_device()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    names.join(" ") + "\n"
}

fn input_devices() -> String {
    let Ok(entries) = fs::read_dir("/dev/input") else {
        return String::from("none\n");
    };
    let mut lines: Vec<String> = entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            format!("{}: {}\n", path.display(), input_name(&path))
        })
        .collect();
    lines.sort();
    lines.concat()
}

/// EVIOCGNAME, or the reason there is none.
fn input_name(path: &Path) -> String {
    use std::os::fd::AsRawFd;

    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) => return err.to_string(),
    };
    let mut name = [0u8; 256];
    // _IOC(_IOC_READ, 'E', 0x06, 256)
    let request = (2u32 << 30) | ((name.len() as u32) << 16) | ((b'E' as u32) << 8) | 0x06;
    // SAFETY: the buffer is as long as the request says and lives through the call.
    let status = unsafe { libc::ioctl(file.as_raw_fd(), request as _, name.as_mut_ptr()) };
    if status < 0 {
        return io::Error::last_os_error().to_string();
    }
    CStr::from_bytes_until_nul(&name).map_or_else(
        |_| String::from("(no name)"),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Every interface with its flags and addresses, as `getifaddrs` gives them.
fn interfaces() -> String {
    let mut list = std::ptr::null_mut();
    // SAFETY: `list` is a valid place for the result, which is released below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return format!("getifaddrs: {}\n", io::Error::last_os_error());
    }
    let mut lines = Vec::new();
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: `entry` is a node of the list from getifaddrs, not yet freed, and so are its pointers.
        let (name, flags, addr, next) = unsafe {
            let e = &*entry;
            (
                CStr::from_ptr(e.ifa_name).to_string_lossy().into_owned(),
                e.ifa_flags,
                sockaddr_text(e.ifa_addr),
                e.ifa_next,
            )
        };
        lines.push(format!("{name} flags={flags:#x} {addr}\n"));
        entry = next;
    }
    // SAFETY: the list came from getifaddrs and is not used afterwards.
    unsafe { libc::freeifaddrs(list) };
    if lines.is_empty() {
        return String::from("none\n");
    }
    lines.sort();
    lines.concat()
}

/// # Safety
///
/// `addr` is null or points to a socket address of the kind its family says.
unsafe fn sockaddr_text(addr: *const libc::sockaddr) -> String {
    if addr.is_null() {
        return String::from("no address");
    }
    // SAFETY: as the function's contract says.
    unsafe {
        match (*addr).sa_family as i32 {
            libc::AF_INET => {
                let ip = (*addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr;
                format!("inet {}", std::net::Ipv4Addr::from(u32::from_be(ip)))
            }
            libc::AF_INET6 => {
                let ip = (*addr.cast::<libc::sockaddr_in6>()).sin6_addr.s6_addr;
                format!("inet6 {}", std::net::Ipv6Addr::from(ip))
            }
            libc::AF_PACKET => {
                let link = &*addr.cast::<libc::sockaddr_ll>();
                let mac = &link.sll_addr[..(link.sll_halen as usize).min(link.sll_addr.len())];
                let hex: Vec<String> = mac.iter().map(|b| format!("{b:02x}")).collect();
                format!("link {}", hex.join(":"))
            }
            family => format!("family {family}"),
        }
    }
}
