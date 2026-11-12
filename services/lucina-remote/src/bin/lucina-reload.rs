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
//!
//! `--stream-root=overlay|initramfs` makes the new boot a test boot with the stream's own user space instead of the installed root,
//! and `--data=none|ro|rw` (default `none`, only with `--stream-root`) says what becomes of /data in it. They pass the words
//! `lucina.reload.root=` and `lucina.reload.data=` to the running kernel, whose kexec path turns them into `/chosen` properties
//! of this one reload (kernel patch 1244): nothing is written to the ESP, the root slots or the trial record, and any later boot is
//! the installed generation (`lucina_remote::testroot`). With `overlay` the commit in `/etc/lucina/commit` of the running system is
//! checked against the staged initramfs's `installed=` first, and a difference is refused here, while the old system still runs:
//! a refusal only after the boot-time stage mounted the installed root can leave the rescue `/init` hung.

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

use lucina_remote::dataclean;
use lucina_remote::stream::{Header, Part, add_kernel_args, arm64_image, drop_kernel_arg, hex};
use lucina_remote::testroot;
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
const LINUX_REBOOT_CMD_RESTART: libc::c_ulong = 0x0123_4567;

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

/// Takes `--stream-root=<mode>` and `--data=<mode>` out of the receive arguments (never the value of an `--add`/`--cmdline` pair) and
/// returns the kernel words they stand for, checked: `None` without `--stream-root`.
fn split_stream_root(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let (mut root, mut data, mut rest) = (None, None, Vec::with_capacity(args.len()));
    let mut args = args.iter();
    while let Some(a) = args.next() {
        if let Some(m) = a.strip_prefix("--stream-root=") {
            match m {
                "overlay" | "initramfs" => root = Some(m.to_string()),
                _ => {
                    return Err(format!(
                        "--stream-root={m}: the modes are overlay and initramfs"
                    ));
                }
            }
        } else if let Some(m) = a.strip_prefix("--data=") {
            match m {
                "none" | "ro" | "rw" => data = Some(m.to_string()),
                _ => return Err(format!("--data={m}: the modes are none, ro and rw")),
            }
        } else {
            rest.push(a.clone());
            if matches!(a.as_str(), "--add" | "--cmdline")
                && let Some(value) = args.next()
            {
                rest.push(value.clone());
            }
        }
    }
    match (root, data) {
        (None, Some(_)) => Err(
            "--data needs --stream-root: on a normal boot /data is mounted as always".to_string(),
        ),
        (None, None) => Ok((None, rest)),
        (Some(r), d) => Ok((
            Some(format!(
                "lucina.reload.root={r} lucina.reload.data={}",
                d.as_deref().unwrap_or("none")
            )),
            rest,
        )),
    }
}

/// The overlay folder of the stream's initramfs, as `scripts/juno-rescue-initramfs.sh --overlay` puts it there.
const OVERLAY_MANIFEST: &str = "/lucina-overlay/MANIFEST";

/// A cpio newc name for comparing: an archive built with `find -printf '%P'` holds no leading `./` or `/`, the callers ask with a
/// root path.
fn cpio_key(name: &str) -> &str {
    let name = name.strip_prefix("./").unwrap_or(name);
    name.strip_prefix('/').unwrap_or(name)
}

/// The 8 hex digits of a newc header field, as the number they say.
fn cpio_field(head: &[u8], at: usize) -> Option<usize> {
    let digits = head.get(at..at + 8)?;
    if !digits.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    usize::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
}

/// The contents of `path` in an uncompressed cpio newc archive: `None` when `data` is not one, is cut short, or does not hold it.
/// An entry is `070701` and 13 fields of 8 hex digits (`filesize` at 54, `namesize` at 94), then the name with its NUL
/// (`namesize` bytes) padded up to 4 bytes, then `filesize` bytes padded up to 4. The walk stops at the `TRAILER!!!` entry.
fn cpio_newc_find<'a>(data: &'a [u8], path: &str) -> Option<&'a [u8]> {
    const HEADER: usize = 110;
    let want = cpio_key(path);
    let mut pos = 0usize;
    loop {
        let head = data.get(pos..pos + HEADER)?;
        if !head.starts_with(b"070701") {
            return None;
        }
        let filesize = cpio_field(head, 54)?;
        let namesize = cpio_field(head, 94)?;
        let name_end = pos + HEADER + namesize;
        let body = (name_end + 3) & !3;
        let name = std::str::from_utf8(data.get(pos + HEADER..name_end)?).ok()?;
        let name = name.trim_end_matches('\0');
        if name == "TRAILER!!!" {
            return None;
        }
        let content = data.get(body..body + filesize)?;
        if cpio_key(name) == want {
            return Some(content);
        }
        pos = (body + filesize + 3) & !3;
    }
}

/// A commit as the MANIFEST and `/etc/lucina/commit` hold it: 40 lowercase hex digits.
fn is_commit(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The `installed=` commit of a MANIFEST's first line, wherever it stands among the fields; `None` when it is absent or not a commit.
fn manifest_installed(first_line: &str) -> Option<&str> {
    let value = first_line
        .split(' ')
        .find_map(|field| field.strip_prefix("installed="))?;
    is_commit(value).then_some(value)
}

/// Why an overlay stream must not be started on this system, or `None` when it may. `running` is the trimmed
/// [`testroot::COMMIT_FILE`] of the running system (`None` when it is not there), `initramfs` the staged file the kernel would get.
/// A compressed initramfs has no newc header at its start, so its MANIFEST cannot be read here and it is refused.
fn overlay_refusal(running: Option<&str>, initramfs: &[u8]) -> Option<String> {
    let Some(running) = running.filter(|c| is_commit(c)) else {
        let what = match running {
            None => "the file is not there".to_string(),
            Some(c) => format!("it holds {c:?}"),
        };
        return Some(format!(
            "the running system records no commit in {} ({what}): an overlay built for another installed commit cannot be told from one built for this system, and the root image only carries that file when scripts/juno-root-image.sh wrote it",
            testroot::COMMIT_FILE
        ));
    };
    let manifest = match cpio_newc_find(initramfs, OVERLAY_MANIFEST) {
        Some(m) => m,
        None => {
            return Some(format!(
                "the staged initramfs has no {OVERLAY_MANIFEST} to read (an uncompressed cpio newc archive is what is read here): the installed commit its overlay was built for is unknown, this system is {running}"
            ));
        }
    };
    let first_line = std::str::from_utf8(manifest)
        .ok()
        .and_then(|text| text.lines().next());
    let Some(line) = first_line else {
        return Some(format!(
            "{OVERLAY_MANIFEST} of the staged initramfs is empty or not text: its installed= commit cannot be read, this system is {running}"
        ));
    };
    let Some(installed) = manifest_installed(line) else {
        return Some(format!(
            "{OVERLAY_MANIFEST} of the staged initramfs names no installed=<40-hex> commit ({line:?}), this system is {running}"
        ));
    };
    if installed != running {
        return Some(format!(
            "the overlay is built for installed commit {installed}, this running system is {running}: refusing before the kexec, so the old system stays up"
        ));
    }
    None
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
/// (the loader's stage times of the running boot: a reload has no loader stage, and a stale word would stand next to a new one),
/// nor the test-root words, which only `--stream-root` adds, for this one reload.
fn drop_boot_only_args(line: &str) -> String {
    let line = drop_kernel_arg(&drop_kernel_arg(line, "lucina.trial"), "lucina.loader_ms");
    drop_kernel_arg(
        &drop_kernel_arg(&line, "lucina.reload.root"),
        "lucina.reload.data",
    )
}

fn receive(args: &[String]) -> Result<(), ReceiveError> {
    // Before any of the stream is read: a refused reload must not consume the files from the sender.
    let (stream_root, args) = split_stream_root(args)?;
    let (any_uptime, args) = split_any_uptime(&args);
    let staging = kexec_staging();
    reload_allowed(proc_uptime_s(), staging.as_deref(), any_uptime)
        .map_err(ReceiveError::Refused)?;
    let mut input = BufReader::with_capacity(1 << 16, io::stdin().lock());
    let header = Header::read(&mut input).map_err(|e| e.to_string())?;
    let cmdline = new_cmdline(&args, header.cmdline.clone())?;
    let cmdline = match (cmdline, &stream_root) {
        (Some(line), Some(words)) => {
            Some(add_kernel_args(&String::from_utf8_lossy(&line), words).into_bytes())
        }
        (line, _) => line,
    };
    let dir = Path::new(STAGING_DIR);
    fs::create_dir_all(dir).map_err(|e| format!("{STAGING_DIR}: {e}"))?;
    let (kernel, initrd) = (dir.join("kernel"), dir.join("initrd"));
    // The mode the words above ask the kernel for, not one that only stands inside an `--add` value.
    let overlay = stream_root
        .as_deref()
        .is_some_and(|words| words.starts_with("lucina.reload.root=overlay"));
    let result: Result<(), ReceiveError> = (|| {
        receive_part(&mut input, &kernel, &header.kernel)?;
        receive_part(&mut input, &initrd, &header.initrd)?;
        if !input
            .fill_buf()
            .map_err(|e| format!("stdin: {e}"))?
            .is_empty()
        {
            return Err("more data after the initramfs".to_string().into());
        }
        if overlay {
            // Both files are staged and complete, so this is the last point where a refusal leaves the running system untouched.
            let staged = fs::read(&initrd).map_err(|e| format!("{}: {e}", initrd.display()))?;
            let running = fs::read_to_string(testroot::COMMIT_FILE)
                .ok()
                .map(|text| text.trim().to_string());
            // The buffer goes here, before `kexec_file_load` asks the kernel for contiguous memory.
            let why = overlay_refusal(running.as_deref(), &staged);
            drop(staged);
            if let Some(why) = why {
                return Err(ReceiveError::Refused(why));
            }
        }
        kexec_file_load(&kernel, &initrd, cmdline.as_deref()).map_err(ReceiveError::Failed)
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
            // /data has no journal: without its clean mark the next boot spends a full e2fsck on it. Best effort, capped at
            // 5 s, from `/` so that the sender's own working directory does not hold /data open.
            // SAFETY: `chdir` reads a NUL-terminated string that lives through the call.
            unsafe { libc::chdir(c"/".as_ptr()) };
            let outcome = dataclean::clean_before_restart();
            if let Ok(mut kmsg) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
                let _ = kmsg
                    .write_all(format!("<4>lucina-reload: {}\n", outcome.describe()).as_bytes());
            }
            // SAFETY: The arguments are integers; on success this does not return.
            unsafe {
                libc::syscall(
                    libc::SYS_reboot,
                    LINUX_REBOOT_MAGIC1,
                    LINUX_REBOOT_MAGIC2,
                    LINUX_REBOOT_CMD_KEXEC,
                    0 as libc::c_ulong,
                );
            }
            // The kexec failed. /data is marked clean and out of use, so a plain restart through the firmware is orderly; running on
            // would leave writes going to the root's /data mount point.
            let failed = io::Error::last_os_error();
            if let Ok(mut kmsg) = fs::OpenOptions::new().write(true).open("/dev/kmsg") {
                let _ = kmsg.write_all(
                    format!("<4>lucina-reload: kexec failed ({failed}), restarting through the firmware\n").as_bytes(),
                );
            }
            // SAFETY: The arguments are integers; on success this does not return.
            unsafe {
                libc::syscall(
                    libc::SYS_reboot,
                    LINUX_REBOOT_MAGIC1,
                    LINUX_REBOOT_MAGIC2,
                    LINUX_REBOOT_CMD_RESTART,
                    0 as libc::c_ulong,
                );
            }
            // Not even that: /data goes back to a normal read-write mount, not clean.
            let _ = dataclean::reopen_after_failed_restart();
            // SAFETY: Ends this child without running destructors.
            unsafe { libc::_exit(1) }
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
        Some(a) if a.starts_with("--stream-root=") || a.starts_with("--data=") => {
            receive(&args).and_then(|()| jump_in_background().map_err(ReceiveError::Failed))
        }
        Some(other) => Err(ReceiveError::Failed(format!(
            "unknown argument {other:?}; usage: lucina-reload pack --kernel <image> --initrd <file> [--cmdline <text>], base <address>..., or to receive: no arguments, --add <kernel arguments>, --cmdline <whole line>, --any-uptime, --stream-root=overlay|initramfs [--data=none|ro|rw]"
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
    fn stream_root_becomes_the_two_words() {
        let args = strings(&[
            "--add",
            "--data=rw",
            "--stream-root=overlay",
            "--any-uptime",
        ]);
        assert_eq!(
            split_stream_root(&args).unwrap(),
            (
                Some("lucina.reload.root=overlay lucina.reload.data=none".to_string()),
                strings(&["--add", "--data=rw", "--any-uptime"])
            )
        );
        let args = strings(&["--data=ro", "--stream-root=initramfs"]);
        assert_eq!(
            split_stream_root(&args).unwrap().0.unwrap(),
            "lucina.reload.root=initramfs lucina.reload.data=ro"
        );
    }

    #[test]
    fn stream_root_refuses_what_it_does_not_know() {
        for a in [
            &["--stream-root=ram"][..],
            &["--stream-root=overlay", "--data=yes"],
            &["--data=ro"],
        ] {
            assert!(split_stream_root(&strings(a)).is_err(), "{a:?}");
        }
        assert_eq!(split_stream_root(&[]).unwrap(), (None, Vec::new()));
    }

    #[test]
    fn a_running_test_boots_words_never_go_along() {
        assert_eq!(
            drop_boot_only_args("a lucina.reload.root=overlay lucina.reload.data=rw -- x"),
            "a -- x"
        );
    }

    #[test]
    fn the_flag_as_a_pair_value_is_kept() {
        let args = strings(&["--cmdline", "--any-uptime"]);
        assert_eq!(
            split_any_uptime(&args),
            (false, strings(&["--cmdline", "--any-uptime"]))
        );
    }

    const I: &str = "1111111111111111111111111111111111111111";
    const S: &str = "2222222222222222222222222222222222222222";
    const MIXED: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4";
    const TRAILER: &str = "TRAILER!!!";

    /// A cpio newc entry as `cpio -o -H newc` writes it: the magic, 13 fields of 8 hex digits, the name with its NUL padded up to
    /// 4 bytes, then its content padded up to 4.
    fn newc(name: &str, content: &[u8]) -> Vec<u8> {
        let fields: [u32; 13] = [
            3,         // c_ino
            0o100_644, // c_mode
            0,
            0,                    // uid, gid
            1,                    // c_nlink
            0x6900_0000,          // c_mtime
            content.len() as u32, // c_filesize
            0,
            0, // c_major, c_minor
            0,
            0,                       // c_rmajor, c_rminor
            (name.len() + 1) as u32, // c_namesize, with the NUL
            0,                       // c_check
        ];
        let mut out = b"070701".to_vec();
        for f in fields {
            out.extend_from_slice(format!("{f:08x}").as_bytes());
        }
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.resize((out.len() + 3) & !3, 0);
        out.extend_from_slice(content);
        out.resize((out.len() + 3) & !3, 0);
        out
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = entries
            .iter()
            .flat_map(|(name, content)| newc(name, content))
            .collect::<Vec<u8>>();
        out.extend(newc(TRAILER, b""));
        out
    }

    fn manifest(installed: &str) -> Vec<u8> {
        format!("lucina-overlay 1 installed={installed} stream={S} files=1 whiteouts=0 bytes=4\nF 0644 4 {S}{S} /x\n")
            .into_bytes()
    }

    /// An archive of three entries, whose name and content lengths sit in different padding classes.
    fn three_entries(installed: &str) -> Vec<u8> {
        archive(&[
            ("init", b"#!/bin/sh\n"),
            ("lucina-overlay/MANIFEST", &manifest(installed)),
            ("lucina-overlay/root/x", b"1234"),
        ])
    }

    #[test]
    fn cpio_newc_finds_a_file_in_a_three_entry_archive() {
        let a = three_entries(I);
        assert_eq!(cpio_newc_find(&a, "init"), Some(b"#!/bin/sh\n".as_slice()));
        assert_eq!(
            cpio_newc_find(&a, "/lucina-overlay/root/x"),
            Some(b"1234".as_slice())
        );
        let found = cpio_newc_find(&a, OVERLAY_MANIFEST).unwrap();
        assert_eq!(found, manifest(I).as_slice());
        assert_eq!(
            manifest_installed(std::str::from_utf8(found).unwrap().lines().next().unwrap()),
            Some(I)
        );
    }

    #[test]
    fn cpio_newc_has_nothing_for_a_path_it_does_not_hold() {
        assert_eq!(
            cpio_newc_find(&three_entries(I), "/etc/lucina/commit"),
            None
        );
        assert_eq!(
            cpio_newc_find(&three_entries(I), "lucina-overlay/root/xy"),
            None
        );
        assert_eq!(cpio_newc_find(&[], "/x"), None);
    }

    #[test]
    fn cpio_newc_stops_at_the_trailer() {
        // An entry after `TRAILER!!!` is not part of the archive, however well it is built.
        let mut a = newc(TRAILER, b"");
        a.extend(newc("lucina-overlay/MANIFEST", &manifest(I)));
        assert_eq!(cpio_newc_find(&a, OVERLAY_MANIFEST), None);
    }

    #[test]
    fn cpio_newc_refuses_a_cut_or_a_lying_header() {
        let entry = newc("lucina-overlay/MANIFEST", &manifest(I));
        // A header of fewer than its 110 bytes is not one, however little is missing.
        for len in 0..110 {
            assert_eq!(
                cpio_newc_find(&entry[..len], OVERLAY_MANIFEST),
                None,
                "{len} bytes"
            );
        }
        assert_eq!(cpio_newc_find(&entry[..133], OVERLAY_MANIFEST), None); // its name is cut
        assert_eq!(cpio_newc_find(&entry[..135], OVERLAY_MANIFEST), None); // or the padding before its content
        assert_eq!(
            cpio_newc_find(&entry, OVERLAY_MANIFEST),
            Some(manifest(I).as_slice())
        );
        // A header that promises more than the archive holds.
        for at in [54, 94] {
            let mut lie = entry.clone();
            lie[at..at + 8].copy_from_slice(b"0000ffff");
            assert_eq!(
                cpio_newc_find(&lie, OVERLAY_MANIFEST),
                None,
                "field at {at}"
            );
        }
        // Not a cpio archive at all: a compressed initramfs is not walked here.
        assert_eq!(cpio_newc_find(&gzip_like(), OVERLAY_MANIFEST), None);
    }

    /// What a gzip'd initramfs starts with: no newc header, so nothing is found in it.
    fn gzip_like() -> Vec<u8> {
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0];
        out.extend_from_slice(b"\x8c\x99m\xcbP\x00\x02\xff");
        out
    }

    #[test]
    fn the_manifest_commit_is_read_wherever_it_stands() {
        let good = format!("lucina-overlay 1 installed={I} stream={S} files=0 whiteouts=0 bytes=0");
        assert_eq!(manifest_installed(&good), Some(I));
        // The fields in another order, and an extra one: `installed=` is still read.
        let other =
            format!("lucina-overlay 1 bytes=0 installed={I} files=0 stream={S} whiteouts=0 x=9");
        assert_eq!(manifest_installed(&other), Some(I));
        assert_eq!(
            manifest_installed("lucina-overlay 1 files=0 whiteouts=0 bytes=0"),
            None
        );
        assert_eq!(
            manifest_installed("lucina-overlay 1 installed=0123456789abcdef stream=x"),
            None
        );
        // The same sha too short, and in uppercase: neither is a commit.
        assert_eq!(
            manifest_installed(&format!(
                "lucina-overlay 1 installed={} stream=x",
                &MIXED[..39]
            )),
            None
        );
        assert_eq!(
            manifest_installed(&format!("lucina-overlay 1 installed={MIXED} stream=x")),
            Some(MIXED)
        );
        assert_eq!(
            manifest_installed(&format!(
                "lucina-overlay 1 installed={} stream=x",
                MIXED.to_uppercase()
            )),
            None
        );
        assert_eq!(manifest_installed(""), None);
    }

    #[test]
    fn an_overlay_for_this_system_is_not_refused() {
        assert_eq!(overlay_refusal(Some(I), &three_entries(I)), None);
    }

    #[test]
    fn every_other_overlay_case_is_refused_before_the_kexec() {
        // The running system records nothing.
        for running in [None, Some(""), Some("no commit here")] {
            let why = overlay_refusal(running, &three_entries(I)).unwrap();
            assert!(why.contains(testroot::COMMIT_FILE), "{why}");
            assert!(why.contains("records no commit"), "{why}");
        }
        // No MANIFEST, and no commit in it.
        let no_manifest = archive(&[("init", b"x")]);
        let why = overlay_refusal(Some(I), &no_manifest).unwrap();
        assert!(why.contains(OVERLAY_MANIFEST), "{why}");
        assert!(why.contains(I), "{why}: it names the commit it does know");
        let no_field = archive(&[(
            "lucina-overlay/MANIFEST",
            b"lucina-overlay 1 stream=2222222222222222222222222222222222222222 files=0 whiteouts=0 bytes=0\n",
        )]);
        let why = overlay_refusal(Some(I), &no_field).unwrap();
        assert!(why.contains("installed="), "{why}");
        assert!(why.contains(I), "{why}");
        // A compressed archive: its MANIFEST cannot be read without a decompressor.
        let why = overlay_refusal(Some(I), &gzip_like()).unwrap();
        assert!(why.contains(OVERLAY_MANIFEST), "{why}");
        // The mismatch names both commits.
        let why = overlay_refusal(Some(I), &three_entries(S)).unwrap();
        assert!(why.contains(S) && why.contains(I), "{why}");
        assert!(
            why.contains("the overlay is built for installed commit")
                && why.contains("refusing before the kexec"),
            "{why}"
        );
    }
}
