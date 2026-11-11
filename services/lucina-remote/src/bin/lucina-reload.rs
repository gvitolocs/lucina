//! lucina-reload: replaces the running LucinaOS kernel from the network, in RAM.
//!
//! On the machine that builds (needs no LucinaOS):
//!
//! ```text
//! lucina-reload pack --kernel kernel.Image --initrd initramfs.cpio.gz [--cmdline "..."] | ssh root@host lucina-reload
//! ```
//!
//! On LucinaOS, with no arguments, it reads the stream (`lucina_remote::stream`) from stdin into RAM, checks both
//! SHA-256 sums, hands the files to the kernel (`kexec_file_load`), reports on stdout and exits 0, so that ssh gets a
//! proper exit status. A background process then enters the new kernel (`reboot(LINUX_REBOOT_CMD_KEXEC)`) a second
//! later. Any failure leaves the running kernel as it was and exits non-zero. Nothing is written to disk.
//!
//! Before it reads the stream, it refuses a reload of a kernel that has run longer than 600 s: `kexec_file_load`
//! stages the new kernel and the initramfs in physically contiguous memory, which a long-running, fragmented
//! kernel may not have, and a failed allocation can crash it. The refusal is printed to stderr and appended to
//! `/dev/kmsg`, and the exit code is 75 (EX_TEMPFAIL), not 1. `--any-uptime` skips the check, and a kernel that
//! writes `pages` to `/proc/lucina-kexec-staging` (staging page by page) is never refused.
//!
//! `--add "<kernel arguments>"` adds to the running command line (an argument replaces those with its key), and
//! `--cmdline "<whole line>"` replaces it; the stream's own `cmdline`, if any, comes after these. Without any of them
//! the running command line stays. A new line without `/etc/lucina/remote.sh` boots a machine without SSH.
//!
//! `lucina-reload base <address>...` prints which of the DRAM bases the running kernel was built for: a new image must
//! have been built for that one.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::CString,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    path::Path,
    process::ExitCode,
    time::Duration,
};

use lucina_remote::stream::{Header, Part, add_kernel_args, arm64_image, drop_kernel_arg, hex};
use sha2::{Digest, Sha256};

/// RAM: the root file system of LucinaOS is the unpacked initramfs.
const STAGING_DIR: &str = "/tmp/lucina-reload";
const DELAY_BEFORE_JUMP: Duration = Duration::from_secs(1);

/// The longest a kernel may have run for a reload to be allowed. `kexec_file_load` stages the new kernel and the
/// initramfs in one physically contiguous block each (128 MiB for a 68 MB initramfs), and each file is read into
/// a heap buffer of its size, which is also physically contiguous. A kernel that has run a while has fragmented
/// memory: a reload at 440 s of uptime worked, at 1183 s it failed with ENOMEM (docs/research/juno-kexec-enomem.md).
/// `--any-uptime` overrides this limit.
const MAX_UPTIME_S: u64 = 600;

/// `libc` does not name this one on every target.
#[cfg(target_arch = "aarch64")]
const SYS_KEXEC_FILE_LOAD: libc::c_long = 294;
#[cfg(not(target_arch = "aarch64"))]
const SYS_KEXEC_FILE_LOAD: libc::c_long = 320; // x86-64: only so that the host build links `pack`

const LINUX_REBOOT_MAGIC1: libc::c_ulong = 0xfee1_dead;
const LINUX_REBOOT_MAGIC2: libc::c_ulong = 0x2812_1969;
const LINUX_REBOOT_CMD_KEXEC: libc::c_ulong = 0x4558_4543;

/// Whether a reload is allowed on a kernel that has run `uptime_s` seconds.
///
/// `staging` is the trimmed contents of `/proc/lucina-kexec-staging`, if that file exists: a later kernel writes
/// `pages` there when it stages the kexec buffers page by page, and such a kernel needs no contiguous memory, so
/// it is never refused. Otherwise the reload is allowed only with `any_uptime` (the `--any-uptime` flag) or while
/// the uptime is within `MAX_UPTIME_S`.
fn reload_allowed(uptime_s: f64, staging: Option<&str>, any_uptime: bool) -> Result<(), String> {
    if staging == Some("pages") || any_uptime || uptime_s <= MAX_UPTIME_S as f64 {
        return Ok(());
    }
    let n = uptime_s as u64;
    Err(format!(
        "this kernel has run {n} s (more than {MAX_UPTIME_S}): kexec stages the new kernel in contiguous memory, which a kernel that has run a while may not have, and a failed allocation can crash it (LucinaOS docs/research/juno-kexec-enomem.md). Reboot to the installed generation first (reboot -f), then reload; --any-uptime skips this check"
    ))
}

/// Takes `--any-uptime` out of the receive arguments, so that the `--add`/`--cmdline` pairs are parsed unchanged:
/// the value that follows `--add` or `--cmdline` is never mistaken for the flag.
fn split_any_uptime(args: &[String]) -> (bool, Vec<String>) {
    let (mut any_uptime, mut rest) = (false, Vec::with_capacity(args.len()));
    let mut args = args.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--add" | "--cmdline" => {
                rest.push(a.clone());
                if let Some(value) = args.next() {
                    rest.push(value.clone());
                }
            }
            "--any-uptime" => any_uptime = true,
            _ => rest.push(a.clone()),
        }
    }
    (any_uptime, rest)
}

/// The seconds in the first number of `/proc/uptime`, or 0 if the file cannot be read or parsed.
fn proc_uptime_s() -> f64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|s| s.split_whitespace().next()?.parse().ok())
        .unwrap_or(0.0)
}

/// What the running kernel writes to `/proc/lucina-kexec-staging`, or `None` if the file is missing.
fn kexec_staging() -> Option<String> {
    match fs::read_to_string("/proc/lucina-kexec-staging") {
        Ok(s) => Some(s.trim().to_string()),
        Err(_) => None,
    }
}

/// A failure of the receive path. A refused reload exits 75 (EX_TEMPFAIL) so that ssh reports it distinctly from
/// the 1 of any other failure.
enum ReceiveError {
    Refused(String),
    Failed(String),
}

impl From<String> for ReceiveError {
    fn from(e: String) -> Self {
        ReceiveError::Failed(e)
    }
}

fn pack(args: &[String]) -> Result<(), String> {
    let (mut kernel, mut initrd, mut cmdline) = (None, None, None);
    let mut args = args.iter();
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{a} needs a value"));
        match a.as_str() {
            "--kernel" => kernel = Some(value()?.clone()),
            "--initrd" => initrd = Some(value()?.clone()),
            "--cmdline" => cmdline = Some(value()?.clone().into_bytes()),
            _ => return Err(format!("unknown argument {a:?}")),
        }
    }
    let read = |path: Option<String>, what: &str| -> Result<Vec<u8>, String> {
        let path = path.ok_or_else(|| format!("no --{what}"))?;
        fs::read(&path).map_err(|e| format!("{path}: {e}"))
    };
    let kernel = read(kernel, "kernel")?;
    let initrd = read(initrd, "initrd")?;
    if arm64_image(&kernel).is_none() {
        return Err("the kernel is not an arm64 image".into());
    }
    let header = Header {
        kernel: Part::of(&kernel),
        initrd: Part::of(&initrd),
        cmdline,
    };
    let mut out = io::stdout().lock();
    let write = |out: &mut io::StdoutLock, data: &[u8]| {
        out.write_all(data).map_err(|e| format!("stdout: {e}"))
    };
    write(&mut out, &header.encode())?;
    write(&mut out, &kernel)?;
    write(&mut out, &initrd)?;
    out.flush().map_err(|e| format!("stdout: {e}"))
}

/// Copies exactly `part.size` bytes to `path`, checking the sum on the way.
fn receive_part(input: &mut impl Read, path: &Path, part: &Part) -> Result<(), String> {
    let mut out = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut left = part.size;
    let mut buf = vec![0u8; 1 << 16];
    while left > 0 {
        let want = left.min(buf.len() as u64) as usize;
        let n = match input.read(&mut buf[..want]) {
            Ok(0) => return Err(format!("the stream ended {left} bytes early")),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("stdin: {e}")),
        };
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", path.display()))?;
        left -= n as u64;
    }
    let sum: [u8; 32] = hasher.finalize().into();
    if sum != part.sha256 {
        return Err(format!(
            "{}: sha256 is {}, the header says {}",
            path.display(),
            hex(&sum),
            hex(&part.sha256)
        ));
    }
    Ok(())
}

fn kexec_file_load(kernel: &Path, initrd: &Path, cmdline: Option<&[u8]>) -> Result<(), String> {
    let open = |p: &Path| File::open(p).map_err(|e| format!("{}: {e}", p.display()));
    let (kernel, initrd) = (open(kernel)?, open(initrd)?);
    let cmdline = cmdline
        .map(|c| CString::new(c).map_err(|_| "the command line has a NUL byte".to_string()))
        .transpose()?;
    let (ptr, len) = cmdline.as_ref().map_or((core::ptr::null(), 0), |c| {
        (c.as_ptr(), c.as_bytes_with_nul().len())
    });
    // SAFETY: The file descriptors are open and the pointer is valid for `len` bytes or null with `len` 0.
    let ret = unsafe {
        libc::syscall(
            SYS_KEXEC_FILE_LOAD,
            kernel.as_raw_fd() as libc::c_long,
            initrd.as_raw_fd() as libc::c_long,
            len as libc::c_ulong,
            ptr,
            0 as libc::c_ulong,
        )
    };
    if ret != 0 {
        return Err(format!("kexec_file_load: {}", io::Error::last_os_error()));
    }
    Ok(())
}

/// The command line the new kernel gets: the running one unless told otherwise, always without `lucina.trial`.
fn new_cmdline(args: &[String], from_stream: Option<Vec<u8>>) -> Result<Option<Vec<u8>>, String> {
    let mut result = from_stream;
    let mut args = args.iter();
    while let Some(a) = args.next() {
        let value = args.next().ok_or_else(|| format!("{a} needs a value"))?;
        match a.as_str() {
            "--cmdline" => result = Some(value.clone().into_bytes()),
            "--add" => {
                let current = fs::read_to_string("/proc/cmdline")
                    .map_err(|e| format!("/proc/cmdline: {e}"))?;
                result = Some(add_kernel_args(&current, value).into_bytes());
            }
            _ => return Err(format!("unknown argument {a:?}")),
        }
    }
    // Without --cmdline or --add the running line stays, but never the trial flag: see `drop_kernel_arg`.
    let line = match result {
        Some(line) => String::from_utf8_lossy(&line).into_owned(),
        None => fs::read_to_string("/proc/cmdline").map_err(|e| format!("/proc/cmdline: {e}"))?,
    };
    Ok(Some(drop_boot_only_args(&line).into_bytes()))
}

/// The command line without the words only a loader boot has: `lucina.trial` (see `drop_kernel_arg`) and `lucina.loader_ms`
/// (the loader's stage times of the running boot: a reload has no loader stage, and a stale word would stand next to a new one).
fn drop_boot_only_args(line: &str) -> String {
    drop_kernel_arg(&drop_kernel_arg(line, "lucina.trial"), "lucina.loader_ms")
}

fn receive(args: &[String]) -> Result<(), ReceiveError> {
    // Before any of the stream is read: a refused reload must not consume the files from the sender.
    let (any_uptime, args) = split_any_uptime(args);
    let staging = kexec_staging();
    reload_allowed(proc_uptime_s(), staging.as_deref(), any_uptime)
        .map_err(ReceiveError::Refused)?;
    let mut input = BufReader::with_capacity(1 << 16, io::stdin().lock());
    let header = Header::read(&mut input).map_err(|e| e.to_string())?;
    let cmdline = new_cmdline(&args, header.cmdline.clone())?;
    let dir = Path::new(STAGING_DIR);
    fs::create_dir_all(dir).map_err(|e| format!("{STAGING_DIR}: {e}"))?;
    let (kernel, initrd) = (dir.join("kernel"), dir.join("initrd"));
    let result = (|| {
        receive_part(&mut input, &kernel, &header.kernel)?;
        receive_part(&mut input, &initrd, &header.initrd)?;
        if !input
            .fill_buf()
            .map_err(|e| format!("stdin: {e}"))?
            .is_empty()
        {
            return Err("more data after the initramfs".to_string());
        }
        kexec_file_load(&kernel, &initrd, cmdline.as_deref())
    })();
    // The kernel has its own copy now, and a failed reload should not keep the memory.
    let _ = fs::remove_dir_all(dir);
    result?;
    if let Some(line) = &cmdline {
        let line = String::from_utf8_lossy(line);
        println!("lucina-reload: new command line: {line}");
        if !line.contains("remote.sh") && !line.contains("lucina-sshd") {
            println!("lucina-reload: WARNING: this command line does not start the SSH server");
        }
    }
    println!(
        "lucina-reload: kernel {} bytes sha256 {}, initramfs {} bytes sha256 {}: verified and loaded; entering it in {} s",
        header.kernel.size,
        hex(&header.kernel.sha256),
        header.initrd.size,
        hex(&header.initrd.sha256),
        DELAY_BEFORE_JUMP.as_secs()
    );
    Ok(())
}

/// Prints the candidate that the kernel accepts. An image of only a header is enough for the check, and it is unloaded
/// again.
fn probe_base(candidates: &[String]) -> Result<(), String> {
    const KEXEC_FILE_UNLOAD: libc::c_ulong = 1;
    const KEXEC_FILE_NO_INITRAMFS: libc::c_ulong = 4;
    const TEXT_OFFSET: u64 = 0x20_0000;
    let dir = Path::new(STAGING_DIR);
    fs::create_dir_all(dir).map_err(|e| format!("{STAGING_DIR}: {e}"))?;
    let path = dir.join("probe");
    for candidate in candidates {
        let base = u64::from_str_radix(candidate.trim_start_matches("0x"), 16)
            .map_err(|_| format!("{candidate:?} is not a hexadecimal address"))?;
        let mut head = [0u8; 64];
        head[8..16].copy_from_slice(&TEXT_OFFSET.to_le_bytes());
        head[16..24].copy_from_slice(&64u64.to_le_bytes());
        head[0x28..0x30].copy_from_slice(&base.to_le_bytes());
        head[0x38..0x3c].copy_from_slice(b"ARM\x64");
        fs::write(&path, head).map_err(|e| format!("{}: {e}", path.display()))?;
        let file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        // SAFETY: The file descriptor is open; there is no command line and no initramfs.
        let ret = unsafe {
            libc::syscall(
                SYS_KEXEC_FILE_LOAD,
                file.as_raw_fd() as libc::c_long,
                -1 as libc::c_long,
                0 as libc::c_ulong,
                core::ptr::null::<libc::c_char>(),
                KEXEC_FILE_NO_INITRAMFS,
            )
        };
        if ret == 0 {
            // SAFETY: Unloading takes no pointers.
            unsafe {
                libc::syscall(
                    SYS_KEXEC_FILE_LOAD,
                    -1 as libc::c_long,
                    -1 as libc::c_long,
                    0 as libc::c_ulong,
                    core::ptr::null::<libc::c_char>(),
                    KEXEC_FILE_UNLOAD,
                );
            }
            let _ = fs::remove_dir_all(dir);
            println!("{candidate}");
            return Ok(());
        }
    }
    let _ = fs::remove_dir_all(dir);
    Err("the kernel accepts none of these DRAM bases".into())
}

/// Leaves a background process that reboots into the loaded kernel, so that this one can exit and ssh can report it.
fn jump_in_background() -> Result<(), String> {
    io::stdout().flush().map_err(|e| e.to_string())?;
    // SAFETY: This program has one thread, so the child may do anything.
    match unsafe { libc::fork() } {
        -1 => Err(format!("fork: {}", io::Error::last_os_error())),
        0 => {
            // SAFETY: `setsid` takes no pointers; `close` ends the use of stdio, which ssh waits on.
            unsafe {
                libc::setsid();
                for fd in 0..=2 {
                    libc::close(fd);
                }
            }
            std::thread::sleep(DELAY_BEFORE_JUMP);
            // SAFETY: The arguments are integers; on success this does not return.
            unsafe {
                libc::syscall(
                    libc::SYS_reboot,
                    LINUX_REBOOT_MAGIC1,
                    LINUX_REBOOT_MAGIC2,
                    LINUX_REBOOT_CMD_KEXEC,
                    0 as libc::c_ulong,
                );
                libc::_exit(1)
            }
        }
        _ => Ok(()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result: Result<(), ReceiveError> = match args.first().map(String::as_str) {
        Some("pack") => pack(&args[1..]).map_err(ReceiveError::Failed),
        Some("base") => probe_base(&args[1..]).map_err(ReceiveError::Failed),
        None | Some("--add" | "--cmdline" | "--any-uptime") => {
            receive(&args).and_then(|()| jump_in_background().map_err(ReceiveError::Failed))
        }
        Some(other) => Err(ReceiveError::Failed(format!(
            "unknown argument {other:?}; usage: lucina-reload pack --kernel <image> --initrd <file> [--cmdline <text>], base <address>..., or to receive: no arguments, --add <kernel arguments>, --cmdline <whole line>, --any-uptime"
        ))),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(ReceiveError::Refused(e)) => {
            eprintln!("lucina-reload: REFUSED: {e}");
            let line = format!("<4>lucina-reload: REFUSED: {e}\n");
            let _ = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/dev/kmsg")
                .and_then(|mut kmsg| kmsg.write_all(line.as_bytes()));
            ExitCode::from(75)
        }
        Err(ReceiveError::Failed(e)) => {
            eprintln!("lucina-reload: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_reload_drops_the_loaders_words() {
        assert_eq!(
            super::drop_boot_only_args(
                "a=1 lucina.loader_ms=1,2,3,4 lucina.trial=on:2 b -- /init lucina.loader_ms=9"
            ),
            "a=1 b -- /init lucina.loader_ms=9"
        );
        assert_eq!(super::drop_boot_only_args("lucina.loader_ms=1,2,3,4"), "");
        assert_eq!(
            super::drop_boot_only_args("a lucina.loader_msx=1"),
            "a lucina.loader_msx=1"
        );
    }

    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_kernel_that_stages_pages_is_never_refused() {
        assert!(reload_allowed(5000.0, Some("pages"), false).is_ok());
    }

    #[test]
    fn the_limit_is_inclusive() {
        assert!(reload_allowed(600.0, None, false).is_ok());
        assert!(reload_allowed(600.5, None, false).is_err());
    }

    #[test]
    fn any_uptime_skips_the_check() {
        assert!(reload_allowed(5000.0, None, true).is_ok());
    }

    #[test]
    fn only_pages_lifts_the_limit() {
        assert!(reload_allowed(5000.0, Some("contiguous"), false).is_err());
    }

    #[test]
    fn the_refusal_names_the_uptime_in_whole_seconds() {
        let e = reload_allowed(1183.7, None, false).unwrap_err();
        assert!(e.contains("1183 s"), "{e}");
    }

    #[test]
    fn any_uptime_is_removed_without_touching_the_pairs() {
        let args = strings(&["--add", "x", "--any-uptime"]);
        assert_eq!(split_any_uptime(&args), (true, strings(&["--add", "x"])));
    }

    #[test]
    fn any_uptime_alone() {
        let args = strings(&["--any-uptime"]);
        assert_eq!(split_any_uptime(&args), (true, Vec::new()));
    }

    #[test]
    fn the_flag_as_a_pair_value_is_kept() {
        let args = strings(&["--cmdline", "--any-uptime"]);
        assert_eq!(
            split_any_uptime(&args),
            (false, strings(&["--cmdline", "--any-uptime"]))
        );
    }
}
