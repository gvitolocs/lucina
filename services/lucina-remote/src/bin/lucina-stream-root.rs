//! lucina-stream-root: PID 1 of a test boot with the stream's own user space, from the stream's initramfs (`/sbin/`). The kernel
//! runs it instead of the installed root when `/chosen` has `lucina,reload-root` (kernel patch 1245), which only a reload writes
//! (`lucina-reload --stream-root=overlay|initramfs`, patch 1244). Its arguments are the init's.
//!
//! Mode `overlay`: the installed root (`root=`, checked against `lucina.root.uuid=` as the kernel does) is mounted read-only as the
//! lower layer of an overlay; a tmpfs holds the upper layer, filled from `/lucina-overlay/` (`lucina_remote::testroot`): every
//! file's size and sha256 checked first, then directories, files, links and whiteouts (char device 0/0). The MANIFEST's
//! `installed=` must be the commit the installed root records in `/etc/lucina/commit`. Then the root's `init=` runs in the
//! overlay (chroot). Mode `initramfs`: the initramfs's own init runs.
//!
//! First the marker line (`testroot::marker`) goes to the kernel log, which also keeps it in the saved console, to the console and
//! to `/etc/lucina/test-root` in the test root. Nothing is written to a disk.
//!
//! Like patch 0820, it never leaves the machine without a system: on any error it says why, leaves its mounts up (the `umount` is what
//! hangs the rescue boot, not the refusal) and runs the initramfs's init (`rdinit=`, else `/init`) as the rescue system.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    ffi::{CString, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, PermissionsExt, symlink},
        process::CommandExt,
    },
    path::Path,
    process::Command,
};

use lucina_remote::testroot::{self, Entry, Manifest, TestBoot};
use sha2::{Digest, Sha256};

const BASE: &str = "/lucina-stream";
const LOWER: &str = "/lucina-stream/lower";
const RW: &str = "/lucina-stream/rw";
const UPPER: &str = "/lucina-stream/rw/upper";
const WORK: &str = "/lucina-stream/rw/work";
const ROOT: &str = "/lucina-stream/root";
/// The block devices for the stage: the initramfs's own /dev has only static nodes, and stays as it is for its init.
const DEV: &str = "/lucina-stream/dev";

/// To the kernel log (and so the saved console) and to the console.
/// The initramfs's own /dev may have no `kmsg` node, so the stage's devtmpfs ([`DEV`]) comes first.
fn say(line: &str) {
    for name in ["kmsg", "console"] {
        let open = |dir: &str| {
            fs::OpenOptions::new()
                .write(true)
                .open(format!("{dir}/{name}"))
        };
        if let Ok(mut f) = open(DEV).or_else(|_| open("/dev")) {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }
}

fn errno(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

fn c(s: &str) -> Result<CString, String> {
    CString::new(s).map_err(|_| format!("{s:?} has a NUL"))
}

/// What this stage mounted, in order: [`undo`] takes back exactly these.
static MOUNTED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn mount(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<(), String> {
    let (s, t, f) = (c(source)?, c(target)?, c(fstype)?);
    let d = data.map(c).transpose()?;
    // SAFETY: The strings outlive the call; `data` is a NUL-terminated string or null.
    let r = unsafe {
        libc::mount(
            s.as_ptr(),
            t.as_ptr(),
            f.as_ptr(),
            flags,
            d.as_ref().map_or(core::ptr::null(), |d| d.as_ptr().cast()),
        )
    };
    if r != 0 {
        return Err(errno(&format!("mount {fstype} {source} on {target}")));
    }
    if let Ok(mut m) = MOUNTED.lock() {
        m.push(target.to_string());
    }
    Ok(())
}

/// A whiteout: a character device numbered 0/0, the form this kernel's overlayfs (and Linux's) reads as one.
fn whiteout(path: &str) -> Result<(), String> {
    let p = c(path)?;
    // SAFETY: The string outlives the call; mknodat takes integers otherwise. As a system call: Eyra need not have the wrapper.
    let r = unsafe {
        libc::syscall(
            libc::SYS_mknodat,
            libc::AT_FDCWD as libc::c_long,
            p.as_ptr(),
            libc::S_IFCHR as libc::c_long,
            0 as libc::c_long,
        )
    };
    if r != 0 {
        return Err(errno(&format!("whiteout {path}")));
    }
    Ok(())
}

fn chroot_here() -> Result<(), String> {
    // SAFETY: A static NUL-terminated string. As a system call: Eyra need not have the wrapper.
    if unsafe { libc::syscall(libc::SYS_chroot, c".".as_ptr()) } != 0 {
        return Err(errno("chroot"));
    }
    Ok(())
}

fn sha256_of(path: &Path) -> Result<(u64, [u8; 32]), String> {
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (mut h, mut n, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let r = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if r == 0 {
            break;
        }
        h.update(&buf[..r]);
        n += r as u64;
    }
    Ok((n, h.finalize().into()))
}

/// Every file of the overlay is there with its size and sha256, before anything is mounted or written.
fn verify(m: &Manifest) -> Result<(), String> {
    for e in &m.entries {
        if let Entry::File {
            size, sha256, path, ..
        } = e
        {
            let src = format!("{}/root{path}", testroot::OVERLAY_DIR);
            let (n, sum) = sha256_of(Path::new(&src))?;
            if n != *size || sum != *sha256 {
                return Err(format!(
                    "{path}: {n} bytes, sha256 {}; the MANIFEST says {size} bytes, {}",
                    lucina_remote::stream::hex(&sum),
                    lucina_remote::stream::hex(sha256)
                ));
            }
        }
    }
    Ok(())
}

/// A directory of the upper layer that the MANIFEST does not list: as it is in the installed root, so that the merged directory
/// keeps its mode and owner, or 0755 if the installed root has none.
fn ensure_dir(path: &str) -> Result<(), String> {
    let up = format!("{UPPER}{path}");
    if Path::new(&up).is_dir() {
        return Ok(());
    }
    let lower = fs::metadata(format!("{LOWER}{path}"))
        .ok()
        .filter(|m| m.is_dir());
    let mode = lower.as_ref().map_or(0o755, |m| m.mode() & 0o7777);
    fs::DirBuilder::new()
        .mode(mode)
        .create(&up)
        .map_err(|e| format!("{up}: {e}"))?;
    fs::set_permissions(&up, fs::Permissions::from_mode(mode)).map_err(|e| format!("{up}: {e}"))?;
    if let Some(m) = lower {
        let p = c(&up)?;
        // SAFETY: The string outlives the call.
        unsafe { libc::chown(p.as_ptr(), m.uid(), m.gid()) };
    }
    Ok(())
}

fn apply(m: &Manifest) -> Result<(), String> {
    for e in testroot::apply_order(&m.entries) {
        let path = e.path();
        for parent in testroot::parents(path) {
            ensure_dir(parent)?;
        }
        let up = format!("{UPPER}{path}");
        match e {
            Entry::Dir { mode, .. } => {
                if !Path::new(&up).is_dir() {
                    fs::DirBuilder::new()
                        .mode(*mode)
                        .create(&up)
                        .map_err(|e| format!("{up}: {e}"))?;
                }
                fs::set_permissions(&up, fs::Permissions::from_mode(*mode))
                    .map_err(|e| format!("{up}: {e}"))?;
            }
            Entry::File { mode, .. } => {
                let src = format!("{}/root{path}", testroot::OVERLAY_DIR);
                fs::copy(&src, &up).map_err(|e| format!("{src} to {up}: {e}"))?;
                fs::set_permissions(&up, fs::Permissions::from_mode(*mode))
                    .map_err(|e| format!("{up}: {e}"))?;
            }
            Entry::Link { target, .. } => symlink(target, &up).map_err(|e| format!("{up}: {e}"))?,
            Entry::Whiteout { .. } => whiteout(&up)?,
        }
    }
    Ok(())
}

/// The kernel arguments the installed root is started with.
struct RootArgs {
    device: String,
    fstype: String,
    uuid: String,
    init: String,
}

fn root_args(cmdline: &str) -> Result<RootArgs, String> {
    let arg = |k: &str| testroot::kernel_arg(cmdline, k).map(str::to_string);
    Ok(RootArgs {
        device: arg("root").ok_or(
            "no root= on the command line: there is no installed root to lay the overlay on",
        )?,
        fstype: arg("rootfstype").unwrap_or_else(|| "ext2".into()),
        uuid: arg("lucina.root.uuid").ok_or("no lucina.root.uuid= on the command line")?,
        init: arg("init").ok_or("no init= on the command line")?,
    })
}

/// The overlay, mounted at [`ROOT`], with `/dev` in it and the marker line in [`testroot::MARKER_FILE`] (written into the upper
/// layer before the mount: no copy-up needed). On an error the caller undoes the mounts.
fn set_up(m: &Manifest, args: &RootArgs, marker: &str) -> Result<(), String> {
    let device = match args.device.strip_prefix("/dev/") {
        Some(name) => format!("{DEV}/{name}"),
        None => return Err(format!("root={} is not a /dev block device", args.device)),
    };
    // The same check as patch 0820: the root partition holds the image the boot names.
    let mut first = vec![0u8; 2048];
    File::open(&device)
        .and_then(|mut f| f.read_exact(&mut first))
        .map_err(|e| format!("{}: {e}", args.device))?;
    let found = testroot::superblock_uuid(&first).unwrap_or_default();
    if found != args.uuid {
        return Err(format!(
            "{} holds image {found}, not {} that the boot names",
            args.device, args.uuid
        ));
    }
    for d in [BASE, LOWER, RW, ROOT] {
        fs::create_dir_all(d).map_err(|e| format!("{d}: {e}"))?;
    }
    mount(
        &device,
        LOWER,
        &args.fstype,
        libc::MS_RDONLY | libc::MS_NOATIME,
        None,
    )?;
    let commit_file = format!("{LOWER}{}", testroot::COMMIT_FILE);
    let commit = fs::read_to_string(&commit_file)
        .map_err(|e| format!("the installed root records no commit ({}: {e}): the overlay cannot be checked against it", testroot::COMMIT_FILE))?;
    if commit.trim() != m.installed {
        return Err(format!(
            "the overlay is for installed commit {}, the installed root is {}",
            m.installed,
            commit.trim()
        ));
    }
    mount("tmpfs", RW, "tmpfs", 0, Some("mode=0755"))?;
    for d in [UPPER, WORK] {
        fs::create_dir_all(d).map_err(|e| format!("{d}: {e}"))?;
    }
    // The upper root itself: as the installed root's, or the merged root would take the tmpfs's.
    if let Ok(meta) = fs::metadata(LOWER) {
        let _ = fs::set_permissions(UPPER, fs::Permissions::from_mode(meta.mode() & 0o7777));
    }
    apply(m)?;
    for d in testroot::parents(testroot::MARKER_FILE) {
        ensure_dir(d)?;
    }
    let file = format!("{UPPER}{}", testroot::MARKER_FILE);
    fs::write(&file, format!("{marker}\n")).map_err(|e| format!("{file}: {e}"))?;
    mount(
        "overlay",
        ROOT,
        "overlay",
        0,
        Some(&format!("lowerdir={LOWER},upperdir={UPPER},workdir={WORK}")),
    )?;
    if !Path::new(&format!("{ROOT}/dev")).is_dir() {
        return Err("the overlaid root has no /dev".into());
    }
    let init = format!("{ROOT}{}", args.init);
    if !fs::metadata(&init).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0) {
        return Err(format!("the overlaid root has no program {}", args.init));
    }
    // What the kernel mounts for a root partition (fs::init_in_first_process); init.sh mounts the rest.
    mount("devtmpfs", &format!("{ROOT}/dev"), "devtmpfs", 0, None)?;
    Ok(())
}

fn undo() {
    // Not unmounted: what hung the rescue boot on this kernel was the stage's `umount` of the lower layer, not the refusal (QEMU,
    // 2026-11-12; scripts/qemu-stream-root.sh (h)). The initramfs's init reaches its end with the mounts left up. What stays is
    // read-only (the installed root) or RAM, under /lucina-stream, and is said.
    let mounted = MOUNTED
        .lock()
        .map(|mut m| std::mem::take(&mut *m))
        .unwrap_or_default();
    if !mounted.is_empty() {
        say(&format!(
            "lucina-stream-root: left mounted for the rescue system: {}",
            mounted.join(", ")
        ));
    }
}

fn write_marker(at_root: &str, line: &str) {
    let dir = format!("{at_root}/etc/lucina");
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(
        format!("{at_root}{}", testroot::MARKER_FILE),
        format!("{line}\n"),
    );
}

/// The initramfs's own init, with the same arguments. It does not return; if even that cannot run, a shell.
fn run_initramfs_init(cmdline: &str, args: &[OsString]) -> ! {
    let init = testroot::kernel_arg(cmdline, "rdinit")
        .unwrap_or("/init")
        .to_string();
    let e = Command::new(&init).arg0(&init).args(args).exec();
    say(&format!("lucina-stream-root: {init}: {e}; a shell"));
    let e = Command::new("/bin/sh").arg0("/bin/sh").exec();
    say(&format!("lucina-stream-root: /bin/sh: {e}"));
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn overlay(
    test: &TestBoot,
    cmdline: &str,
    args: &[OsString],
) -> Result<std::convert::Infallible, String> {
    let text = fs::read_to_string(format!("{}/MANIFEST", testroot::OVERLAY_DIR)).map_err(|e| {
        format!(
            "the stream's initramfs has no {}/MANIFEST ({e})",
            testroot::OVERLAY_DIR
        )
    })?;
    let m = testroot::parse(&text)?;
    let root = root_args(cmdline)?;
    verify(&m)?;
    let line = testroot::marker(
        test.root,
        test.data,
        Some(&m.installed),
        Some(&m.stream),
        m.files,
        m.whiteouts,
    );
    if let Err(e) = set_up(&m, &root, &line) {
        undo();
        return Err(e);
    }
    say(&line);
    // The initramfs stays reachable for the way back if the init cannot be started.
    let old_root = File::open("/").map_err(|e| format!("/: {e}"))?;
    std::env::set_current_dir(ROOT).map_err(|e| format!("{ROOT}: {e}"))?;
    chroot_here()?;
    let _ = std::env::set_current_dir("/");
    let e = Command::new(&root.init).arg0(&root.init).args(args).exec();
    // Back to the initramfs.
    use std::os::fd::AsRawFd;
    // SAFETY: `old_root` is an open directory.
    unsafe { libc::fchdir(old_root.as_raw_fd()) };
    let _ = chroot_here();
    let _ = std::env::set_current_dir("/");
    undo();
    Err(format!("{}: {e}", root.init))
}

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // The block devices and /dev/kmsg for the stage. It stays (as the mounts of a refusal do, see `undo`).
    let _ = fs::create_dir_all(DEV).is_ok() && {
        let (s, t) = (c"devtmpfs", c"/lucina-stream/dev");
        // SAFETY: Static NUL-terminated strings; no data.
        unsafe { libc::mount(s.as_ptr(), t.as_ptr(), s.as_ptr(), 0, core::ptr::null()) == 0 }
    };
    // /proc for the command line and the device tree; the init mounts its own.
    let _ = fs::create_dir_all("/proc");
    let proc_was_there = Path::new("/proc/cmdline").exists();
    if !proc_was_there {
        let _ = mount("proc", "/proc", "proc", 0, None);
        // Not one of the overlay's mounts.
        if let Ok(mut m) = MOUNTED.lock() {
            m.clear();
        }
    }
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let test = testroot::test_boot();
    let Some(test) = test else {
        say(
            "lucina-stream-root: no lucina,reload-root in /chosen: not a test boot, the initramfs's own init runs",
        );
        run_initramfs_init(&cmdline, &args);
    };
    match test.root {
        "overlay" => {
            let e = match overlay(&test, &cmdline, &args) {
                Ok(never) => match never {},
                Err(e) => e,
            };
            say(&format!(
                "lucina-stream-root: test root refused: {e}: the initramfs takes over as the rescue system (data {})",
                test.data.name()
            ));
        }
        _ => {
            let manifest = fs::read_to_string(format!("{}/MANIFEST", testroot::OVERLAY_DIR))
                .ok()
                .and_then(|t| testroot::parse(&t).ok());
            let line = testroot::marker(
                test.root,
                test.data,
                manifest.as_ref().map(|m| m.installed.as_str()),
                manifest.as_ref().map(|m| m.stream.as_str()),
                0,
                0,
            );
            say(&line);
            write_marker("", &line);
        }
    }
    // /proc stays: the initramfs's own init mounts its own over it. No umount here at all: in QEMU (2026-11-12) it was the stage's
    // umounts (of /proc with the lower layer mounted, or of the lower layer) that hung, never the refusal — the rescue init runs to its
    // end while those mounts stay.
    run_initramfs_init(&cmdline, &args);
}
