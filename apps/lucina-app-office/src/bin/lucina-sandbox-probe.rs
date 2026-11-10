//! What this kernel really does to shut an app in: each test in a child of its own, printing what happened. Measures, changes nothing
//! that outlives the child except a test file in `/tmp`, removed at the end. Run as root on Juno, with the network up:
//!   lucina-sandbox-probe [<address of something that answers on the network, default 192.168.1.1:80>]

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::ffi::CString;
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const SECRET: &str = "/tmp/lucina-probe-secret";
const UID: libc::uid_t = 1000;

fn errno() -> String {
    std::io::Error::last_os_error().to_string()
}

fn check(name: &str, rc: libc::c_int) -> bool {
    if rc == 0 {
        println!("  {name}: ok");
        true
    } else {
        println!("  {name}: refused: {}", errno());
        false
    }
}

/// Runs `f` in a child and waits; a child that dies is reported.
fn test(name: &str, f: impl FnOnce()) {
    println!("{name}");
    // SAFETY: nothing else runs in this process, and the child ends by _exit.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        f();
        // SAFETY: the end of the child.
        unsafe { libc::_exit(0) };
    }
    if pid < 0 {
        println!("  fork: {}", errno());
        return;
    }
    let mut status = 0;
    // SAFETY: a valid pointer, and our own child.
    unsafe { libc::waitpid(pid, &mut status, 0) };
    if libc::WIFSIGNALED(status) {
        println!("  child killed by signal {}", libc::WTERMSIG(status));
    }
}

fn can_read(path: &str) -> String {
    match std::fs::read(path) {
        Ok(_) => "READABLE".into(),
        Err(e) => format!("refused ({e})"),
    }
}

fn reach(address: SocketAddr) -> String {
    match TcpStream::connect_timeout(&address, Duration::from_secs(3)) {
        Ok(_) => "CONNECTED".into(),
        Err(e) => format!("no ({e})"),
    }
}

/// `connect` with no port of its own asks the kernel for one, and that step may be what refuses. Binding first goes round it.
fn reach_from_own_port(address: SocketAddr) -> String {
    let SocketAddr::V4(to) = address else { return "IPv4 only".into() };
    // SAFETY: valid pointers to sockaddr_in values of the size given.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return format!("no socket ({})", errno());
        }
        let mut from: libc::sockaddr_in = std::mem::zeroed();
        from.sin_family = libc::AF_INET as libc::sa_family_t;
        from.sin_port = 40000u16.to_be();
        let size = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        if libc::bind(fd, &from as *const _ as *const libc::sockaddr, size) != 0 {
            return format!("bind refused ({})", errno());
        }
        let mut dest: libc::sockaddr_in = std::mem::zeroed();
        dest.sin_family = libc::AF_INET as libc::sa_family_t;
        dest.sin_port = to.port().to_be();
        dest.sin_addr.s_addr = u32::from(*to.ip()).to_be();
        let rc = libc::connect(fd, &dest as *const _ as *const libc::sockaddr, size);
        let result = if rc == 0 { "CONNECTED".to_string() } else { format!("no ({})", errno()) };
        libc::close(fd);
        result
    }
}

fn udp_from_own_port(address: SocketAddr) -> String {
    let socket = match std::net::UdpSocket::bind("0.0.0.0:40001") {
        Ok(s) => s,
        Err(e) => return format!("bind refused ({e})"),
    };
    match socket.send_to(b"probe", address) {
        Ok(n) => format!("SENT {n} bytes"),
        Err(e) => format!("no ({e})"),
    }
}

fn can_open_socket() -> String {
    // SAFETY: no pointers.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return format!("refused ({})", errno());
    }
    // SAFETY: ours.
    unsafe { libc::close(fd) };
    "OPENED".into()
}

fn drop_to_user() -> bool {
    // SAFETY: no pointers.
    unsafe {
        check("setgroups(0)", libc::setgroups(0, std::ptr::null()))
            & check("setresgid", libc::setresgid(UID, UID, UID))
            & check("setresuid", libc::setresuid(UID, UID, UID))
    }
}

fn main() {
    let address: SocketAddr = std::env::args().nth(1).unwrap_or_else(|| "192.168.1.1:80".into()).parse().expect("an address and a port");
    std::fs::write(SECRET, "secret").expect("write the test file");
    std::fs::set_permissions(SECRET, std::fs::Permissions::from_mode(0o600)).expect("mode");
    // SAFETY: no pointers.
    println!("running as uid {} euid {}", unsafe { libc::getuid() }, unsafe { libc::geteuid() });

    test("baseline as root", || {
        println!("  0600 file: {}", can_read(SECRET));
        println!("  network: {}", reach(address));
    });

    test("another user: file permissions", || {
        if drop_to_user() {
            println!("  uid now {}", unsafe { libc::getuid() });
            println!("  0600 root file: {}", can_read(SECRET));
            println!("  /dev/fb0: {}", std::fs::OpenOptions::new().write(true).open("/dev/fb0").map_or_else(|e| format!("refused ({e})"), |_| "OPENED".into()));
            println!("  /proc/1/environ: {}", can_read("/proc/1/environ"));
            println!("  new file in /: {}", std::fs::write("/lucina-probe", "x").map_or_else(|e| format!("refused ({e})"), |_| "WRITTEN".into()));
            println!("  a socket: {}", can_open_socket());
            println!("  network: {}", reach(address));
            println!("  network from a port of its own: {}", reach_from_own_port(address));
            println!("  UDP from a port of its own: {}", udp_from_own_port(address));
            // SAFETY: no pointers.
            println!("  back to root: {}", if unsafe { libc::setuid(0) } == 0 { "ALLOWED" } else { "refused" });
        }
    });

    for (name, flag) in [
        ("unshare NEWNET", libc::CLONE_NEWNET),
        ("unshare NEWNS", libc::CLONE_NEWNS),
        ("unshare NEWUSER", libc::CLONE_NEWUSER),
        ("unshare NEWPID", libc::CLONE_NEWPID),
        ("unshare NEWIPC", libc::CLONE_NEWIPC),
        ("unshare NEWUTS", libc::CLONE_NEWUTS),
    ] {
        test(name, || {
            // SAFETY: no pointers.
            if check("unshare", unsafe { libc::unshare(flag) }) && flag == libc::CLONE_NEWNET {
                println!("  a socket: {}", can_open_socket());
                println!("  network: {}", reach(address));
            }
        });
    }

    test("seccomp and no_new_privs", || {
        // SAFETY: integer arguments only.
        unsafe {
            check("no_new_privs", libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));
            println!("  no_new_privs reads {}", libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0));
            check("seccomp strict", libc::prctl(libc::PR_SET_SECCOMP, 1, 0, 0, 0));
        }
    });

    test("capabilities: drop all, stay root", || {
        let header = libc_cap_header();
        let data = [CapData { effective: 0, permitted: 0, inheritable: 0 }; 2];
        // SAFETY: valid pointers to the kernel's layout.
        let rc = unsafe { libc::syscall(libc::SYS_capset, &header, data.as_ptr()) };
        if check("capset", rc as libc::c_int) {
            println!("  0600 root file (owner, so expected readable): {}", can_read(SECRET));
            println!("  /etc/shadow-like root file through a chmod 0: {}", {
                let _ = std::fs::set_permissions(SECRET, std::fs::Permissions::from_mode(0));
                can_read(SECRET)
            });
            println!("  a socket: {}", can_open_socket());
            println!("  mount: {}", try_mount());
            println!("  setuid(1000) then back: {}", {
                // SAFETY: no pointers.
                if unsafe { libc::setuid(UID) } == 0 { "ALLOWED with no capability" } else { "refused" }
            });
        }
    });
    let _ = std::fs::set_permissions(SECRET, std::fs::Permissions::from_mode(0o600));

    test("read-only bind mount and chroot", || {
        // SAFETY: no pointers.
        if check("unshare NEWNS", unsafe { libc::unshare(libc::CLONE_NEWNS) }) {
            println!("  bind mount: {}", try_mount());
            let dir = CString::new("/tmp").expect("no NUL");
            let flags = libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY;
            // SAFETY: valid strings.
            let rc = unsafe { libc::mount(dir.as_ptr(), dir.as_ptr(), std::ptr::null(), flags, std::ptr::null()) };
            if check("remount read-only", rc) {
                println!("  write there: {}", std::fs::write("/tmp/lucina-probe-ro", "x").map_or_else(|e| format!("refused ({e})"), |_| "WRITTEN".into()));
            }
        }
        let root = CString::new("/tmp").expect("no NUL");
        // SAFETY: a valid string.
        if check("chroot", unsafe { libc::chroot(root.as_ptr()) }) {
            println!("  /bin/sh after it: {}", if std::path::Path::new("/bin/sh").exists() { "STILL THERE" } else { "gone" });
        }
    });

    let _ = std::fs::remove_file(SECRET);
    let _ = std::fs::remove_file("/tmp/lucina-probe-ro");
    println!("done");
}

fn try_mount() -> String {
    let (from, to) = (CString::new("/tmp").expect("no NUL"), CString::new("/tmp").expect("no NUL"));
    // SAFETY: valid strings.
    let rc = unsafe { libc::mount(from.as_ptr(), to.as_ptr(), std::ptr::null(), libc::MS_BIND, std::ptr::null()) };
    if rc == 0 { "ALLOWED".into() } else { format!("refused ({})", errno()) }
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

fn libc_cap_header() -> CapHeader {
    // _LINUX_CAPABILITY_VERSION_3.
    CapHeader { version: 0x2008_0522, pid: 0 }
}
