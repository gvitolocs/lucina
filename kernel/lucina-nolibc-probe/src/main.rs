//! lucina-nolibc-probe: checks the C library's part of what Juno's programs use, one line per check, so that the same program
//! built on musl and on Eyra (no C library, docs/research/juno-pure-rust.md) can be compared under Juno's kernel.
//!
//! ```text
//! lucina-nolibc-probe [--net]     every check; --net also resolves and reaches a public name
//! ```
//!
//! Output: `PROBE <name> ok <detail>` or `PROBE <name> FAIL <why>`, then `PROBE-DONE <passed>/<total> libc=<which>`.
//! Exit 0 when every check passed. `dlopen` is expected to refuse: every Juno program is static, on musl as on Eyra.

// Juno with no C library: Eyra is the libc and the program start, in Rust.
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

type Check = fn() -> Result<String, String>;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--child") {
        // The spawn check's child: echoes stdin, adds its argument and exits with 7.
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).unwrap();
        print!("{s}+{}", args.get(2).map(String::as_str).unwrap_or(""));
        std::process::exit(7);
    }
    let net = args.iter().any(|a| a == "--net");
    let mut checks: Vec<(&str, Check)> = vec![
        ("threads", threads),
        ("detached-thread", detached_thread),
        ("mutex-condvar", mutex_condvar),
        ("thread-local", thread_local),
        ("files", files),
        ("readdir", readdir),
        ("spawn-pipes", spawn_pipes),
        ("env", env),
        ("clocks", clocks),
        ("sleep", sleep),
        ("alloc-64mib", alloc_big),
        ("alloc-churn", alloc_churn),
        ("tcp-loopback", tcp_loopback),
        ("udp-loopback", udp_loopback),
        ("resolve-local", resolve_local),
        ("getrandom", getrandom),
        ("signal", signal),
        ("ioctl", ioctl),
        ("mmap", mmap),
        ("unwind", unwind),
        ("dlopen-refuses", dlopen_refuses),
        ("printf", printf),
        ("getifaddrs", getifaddrs),
        ("getrusage", getrusage),
        ("gai-strerror", gai_strerror),
        ("sysconf", sysconf),
    ];
    if net {
        checks.push(("resolve-public", resolve_public));
    }
    let mut passed = 0;
    for (name, check) in &checks {
        match check() {
            Ok(detail) => {
                passed += 1;
                println!("PROBE {name} ok {detail}");
            }
            Err(why) => println!("PROBE {name} FAIL {why}"),
        }
    }
    let libc = if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(all(target_env = "gnu", target_arch = "aarch64")) {
        "eyra"
    } else {
        "host"
    };
    println!("PROBE-DONE {passed}/{} libc={libc}", checks.len());
    std::io::stdout().flush().ok();
    std::process::exit(if passed == checks.len() { 0 } else { 1 });
}

fn threads() -> Result<String, String> {
    let n = AtomicUsize::new(0);
    let n = Arc::new(n);
    let handles: Vec<_> = (0..64)
        .map(|i| {
            let n = n.clone();
            std::thread::Builder::new()
                .name(format!("probe-{i}"))
                .stack_size(256 * 1024)
                .spawn(move || {
                    n.fetch_add(i, Ordering::SeqCst);
                    std::thread::current().name().map(|s| s.len()).unwrap_or(0)
                })
                .map_err(|e| e.to_string())
        })
        .collect::<Result<_, _>>()?;
    let names: usize = handles.into_iter().map(|h| h.join().unwrap_or(0)).sum();
    let sum = n.load(Ordering::SeqCst);
    if sum != (0..64).sum::<usize>() || names == 0 {
        return Err(format!("sum {sum}, names {names}"));
    }
    Ok(format!("64 threads, sum {sum}"))
}

fn mutex_condvar() -> Result<String, String> {
    let pair = Arc::new((Mutex::new(0u32), Condvar::new()));
    let p2 = pair.clone();
    let t = std::thread::spawn(move || {
        for _ in 0..1000 {
            let (m, c) = &*p2;
            *m.lock().unwrap() += 1;
            c.notify_one();
        }
    });
    let (m, c) = &*pair;
    let mut g = m.lock().unwrap();
    let start = Instant::now();
    while *g < 1000 {
        let (ng, to) = c.wait_timeout(g, Duration::from_secs(5)).unwrap();
        g = ng;
        if to.timed_out() && start.elapsed() > Duration::from_secs(5) {
            return Err(format!("stuck at {}", *g));
        }
    }
    drop(g);
    t.join().map_err(|_| "join".to_string())?;
    Ok("1000 notifies".into())
}

fn thread_local() -> Result<String, String> {
    thread_local!(static V: std::cell::Cell<u32> = const { std::cell::Cell::new(1) });
    V.with(|v| v.set(5));
    let other = std::thread::spawn(|| V.with(|v| v.get())).join().map_err(|_| "join")?;
    let here = V.with(|v| v.get());
    if here != 5 || other != 1 {
        return Err(format!("here {here}, other {other}"));
    }
    Ok("per thread".into())
}

fn scratch_dir() -> std::path::PathBuf {
    let base = std::env::var_os("TMPDIR").map(Into::into).unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join(format!("lucina-nolibc-probe-{}", std::process::id()))
}

fn files() -> Result<String, String> {
    let d = scratch_dir();
    std::fs::create_dir_all(&d).map_err(|e| format!("mkdir: {e}"))?;
    let a = d.join("a");
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    {
        let mut f = std::fs::File::create(&a).map_err(|e| format!("create: {e}"))?;
        f.write_all(&data).map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync: {e}"))?;
    }
    let b = d.join("b");
    std::fs::rename(&a, &b).map_err(|e| format!("rename: {e}"))?;
    let back = std::fs::read(&b).map_err(|e| format!("read: {e}"))?;
    let md = std::fs::metadata(&b).map_err(|e| format!("stat: {e}"))?;
    if back != data || md.len() != data.len() as u64 {
        return Err(format!("read {} bytes, stat {}", back.len(), md.len()));
    }
    std::os::unix::fs::symlink(&b, d.join("l")).map_err(|e| format!("symlink: {e}"))?;
    let target = std::fs::read_link(d.join("l")).map_err(|e| format!("readlink: {e}"))?;
    if target != b {
        return Err("readlink".into());
    }
    Ok(format!("{} bytes", back.len()))
}

fn readdir() -> Result<String, String> {
    let d = scratch_dir();
    let n = std::fs::read_dir(&d).map_err(|e| format!("{e}"))?.count();
    std::fs::remove_dir_all(&d).map_err(|e| format!("rm: {e}"))?;
    let procs = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?.count();
    if n != 2 {
        return Err(format!("{n} entries, wanted 2"));
    }
    Ok(format!("2 entries, /proc {procs}"))
}

fn spawn_pipes() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let mut child = std::process::Command::new(&exe)
        .args(["--child", "arg"])
        .env("PROBE_CHILD", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    child.stdin.take().unwrap().write_all(b"in").map_err(|e| format!("stdin: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("wait: {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout);
    if s != "in+arg" || out.status.code() != Some(7) {
        return Err(format!("got {s:?}, status {:?}", out.status.code()));
    }
    Ok("echo and exit 7".into())
}

fn env() -> Result<String, String> {
    // SAFETY: no other thread reads the environment while the checks run one after the other.
    unsafe { std::env::set_var("LUCINA_PROBE", "x=y") };
    let v = std::env::var("LUCINA_PROBE").map_err(|e| e.to_string())?;
    let path = std::env::var("PATH").unwrap_or_default();
    if v != "x=y" {
        return Err(v);
    }
    Ok(format!("PATH {} chars", path.len()))
}

fn clocks() -> Result<String, String> {
    let wall = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?;
    let a = Instant::now();
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid timespec for the call to write.
    let r = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    let b = Instant::now();
    if r != 0 || b < a {
        return Err(format!("boottime {r}"));
    }
    Ok(format!("wall {} s, boot {} s", wall.as_secs(), ts.tv_sec))
}

fn sleep() -> Result<String, String> {
    let a = Instant::now();
    std::thread::sleep(Duration::from_millis(50));
    let ms = a.elapsed().as_millis();
    if !(50..500).contains(&ms) {
        return Err(format!("{ms} ms"));
    }
    Ok(format!("50 ms took {ms} ms"))
}

fn alloc_big() -> Result<String, String> {
    let mut v = vec![0u8; 64 << 20];
    for i in (0..v.len()).step_by(4096) {
        v[i] = (i >> 12) as u8;
    }
    let sum: u64 = v.iter().step_by(4096).map(|&b| b as u64).sum();
    drop(v);
    Ok(format!("touched, sum {sum}"))
}

fn alloc_churn() -> Result<String, String> {
    let handles: Vec<_> = (0..4)
        .map(|t| {
            std::thread::spawn(move || {
                let mut keep = Vec::new();
                for i in 0..20_000usize {
                    let v = vec![t as u8; (i * 37) % 4000 + 1];
                    if i % 7 == 0 {
                        keep.push(v);
                    }
                    if keep.len() > 500 {
                        keep.drain(..250);
                    }
                }
                keep.len()
            })
        })
        .collect();
    let kept: usize = handles.into_iter().map(|h| h.join().unwrap_or(0)).sum();
    Ok(format!("4 threads, {kept} kept"))
}

fn tcp_loopback() -> Result<String, String> {
    let l = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    let addr = l.local_addr().map_err(|e| e.to_string())?;
    let t = std::thread::spawn(move || -> std::io::Result<usize> {
        let (mut s, _) = l.accept()?;
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0;
        loop {
            let k = s.read(&mut buf)?;
            if k == 0 {
                break;
            }
            n += k;
        }
        s.write_all(&(n as u64).to_le_bytes())?;
        Ok(n)
    });
    let mut c = TcpStream::connect(addr).map_err(|e| format!("connect: {e}"))?;
    c.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let chunk = vec![7u8; 64 * 1024];
    for _ in 0..32 {
        c.write_all(&chunk).map_err(|e| format!("write: {e}"))?;
    }
    c.shutdown(std::net::Shutdown::Write).map_err(|e| format!("shutdown: {e}"))?;
    let mut n = [0u8; 8];
    c.read_exact(&mut n).map_err(|e| format!("read: {e}"))?;
    let server = t.join().map_err(|_| "join")?.map_err(|e| format!("server: {e}"))?;
    if u64::from_le_bytes(n) != 2 << 20 || server != 2 << 20 {
        return Err(format!("{} bytes", u64::from_le_bytes(n)));
    }
    Ok("2 MiB".into())
}

fn udp_loopback() -> Result<String, String> {
    let a = UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    let b = UdpSocket::bind("127.0.0.1:0").map_err(|e| format!("bind: {e}"))?;
    b.set_read_timeout(Some(Duration::from_secs(5))).ok();
    a.send_to(b"ping", b.local_addr().unwrap()).map_err(|e| format!("send: {e}"))?;
    let mut buf = [0u8; 16];
    let (n, from) = b.recv_from(&mut buf).map_err(|e| format!("recv: {e}"))?;
    if &buf[..n] != b"ping" || from != a.local_addr().unwrap() {
        return Err("mismatch".into());
    }
    Ok("ping".into())
}

fn resolve_local() -> Result<String, String> {
    let lit: Vec<_> = ("192.168.1.1", 123).to_socket_addrs().map_err(|e| format!("literal: {e}"))?.collect();
    let local = ("localhost", 80).to_socket_addrs().map(|a| a.count()).unwrap_or(0);
    if lit.len() != 1 {
        return Err(format!("literal gave {}", lit.len()));
    }
    Ok(format!("literal ok, localhost {local}"))
}

fn resolve_public() -> Result<String, String> {
    let addrs: Vec<_> = ("pool.ntp.org", 123).to_socket_addrs().map_err(|e| format!("{e}"))?.collect();
    let t = TcpStream::connect_timeout(&("one.one.one.one", 443).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("none")?, Duration::from_secs(5))
        .map(|_| "reached 1.1.1.1:443")
        .map_err(|e| format!("connect: {e}"))?;
    Ok(format!("pool.ntp.org {} addresses, {t}", addrs.len()))
}

fn getrandom() -> Result<String, String> {
    let mut a = [0u8; 32];
    let mut b = [0u8; 32];
    // SAFETY: the buffers are valid for their lengths.
    let (ra, rb) = unsafe { (libc::getrandom(a.as_mut_ptr().cast(), 32, 0), libc::getrandom(b.as_mut_ptr().cast(), 32, 0)) };
    if ra != 32 || rb != 32 || a == b {
        return Err(format!("{ra} {rb}"));
    }
    Ok("32 bytes twice".into())
}

static SIGNALS: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_usr1(_: libc::c_int) {
    SIGNALS.fetch_add(1, Ordering::SeqCst);
}

fn signal() -> Result<String, String> {
    // SAFETY: a zeroed sigaction with a handler that only touches an atomic is valid; raise sends it to this thread.
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_usr1 as *const () as usize;
        libc::sigemptyset(&mut sa.sa_mask);
        if libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut()) != 0 {
            return Err("sigaction".into());
        }
        libc::raise(libc::SIGUSR1);
        libc::kill(libc::getpid(), libc::SIGUSR1);
    }
    let start = Instant::now();
    while SIGNALS.load(Ordering::SeqCst) < 2 && start.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let n = SIGNALS.load(Ordering::SeqCst);
    if n < 2 {
        return Err(format!("{n} of 2 delivered"));
    }
    Ok("raise and kill".into())
}

fn ioctl() -> Result<String, String> {
    let mut fds = [0i32; 2];
    // SAFETY: fds has room for two descriptors; FIONREAD writes one c_int.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) != 0 {
            return Err("pipe2".into());
        }
        libc::write(fds[1], b"abc".as_ptr().cast(), 3);
        let mut n: libc::c_int = 0;
        let r = libc::ioctl(fds[0], libc::FIONREAD, &mut n);
        libc::close(fds[0]);
        libc::close(fds[1]);
        if r != 0 || n != 3 {
            return Err(format!("ioctl {r}, {n} bytes"));
        }
    }
    Ok("FIONREAD 3".into())
}

fn mmap() -> Result<String, String> {
    // SAFETY: an anonymous private mapping of one page, written and unmapped here only.
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0);
        if p == libc::MAP_FAILED {
            return Err("mmap".into());
        }
        *(p as *mut u64) = 0x1234;
        let v = *(p as *const u64);
        libc::munmap(p, 4096);
        if v != 0x1234 {
            return Err("readback".into());
        }
    }
    Ok("one page".into())
}

fn unwind() -> Result<String, String> {
    if cfg!(panic = "abort") {
        return Ok("panic=abort build, skipped".into());
    }
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(|| {
        let v: Vec<u32> = Vec::new();
        std::hint::black_box(&v)[3]
    });
    let _ = std::panic::take_hook();
    match r {
        Err(_) => Ok("caught".into()),
        Ok(_) => Err("no panic".into()),
    }
}

fn dlopen_refuses() -> Result<String, String> {
    // SAFETY: dlopen and dlsym take NUL-terminated names; null results are the expected answers, and dlerror's message is a
    // NUL-terminated string that stays valid until the next dl call.
    unsafe {
        let h = libc::dlopen(c"libfoo.so".as_ptr(), libc::RTLD_NOW);
        if !h.is_null() {
            return Err("dlopen gave a handle".into());
        }
        let e = libc::dlerror();
        if e.is_null() {
            return Err("dlopen: no dlerror message".into());
        }
        let why = std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned();
        // What LucinaNet's font code asks: is an optional symbol there? A static program says no, and does not crash.
        let s = libc::dlsym(libc::RTLD_DEFAULT, c"FT_Get_Var_Design_Coordinates".as_ptr());
        if !s.is_null() {
            return Err("dlsym found a symbol that is not there".into());
        }
        if libc::dlerror().is_null() {
            return Err("dlsym: no dlerror message".into());
        }
        Ok(format!("null ({why}), dlsym null"))
    }
}

fn printf() -> Result<String, String> {
    let mut buf = [0u8; 64];
    // SAFETY: snprintf writes at most buf.len() bytes, NUL included.
    let n = unsafe { libc::snprintf(buf.as_mut_ptr().cast(), buf.len(), c"%d-%s-%.2f".as_ptr(), 42, c"x".as_ptr(), 1.5f64) };
    let s = std::str::from_utf8(&buf[..n.max(0) as usize]).map_err(|e| e.to_string())?;
    if s != "42-x-1.50" {
        return Err(s.to_string());
    }
    Ok(s.to_string())
}

fn getifaddrs() -> Result<String, String> {
    let mut v4 = Vec::new();
    let mut entries = 0;
    // SAFETY: walks the list getifaddrs returned and frees it once; an AF_INET address is a sockaddr_in.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return Err(format!("{}", std::io::Error::last_os_error()));
        }
        let mut at = head;
        while !at.is_null() {
            let i = &*at;
            entries += 1;
            if !i.ifa_addr.is_null() && (*i.ifa_addr).sa_family as i32 == libc::AF_INET && !i.ifa_netmask.is_null() {
                let name = std::ffi::CStr::from_ptr(i.ifa_name).to_string_lossy().into_owned();
                let ip = std::net::Ipv4Addr::from(u32::from_be((*i.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr));
                let mask = u32::from_be((*i.ifa_netmask.cast::<libc::sockaddr_in>()).sin_addr.s_addr).count_ones();
                v4.push(format!("{name} {ip}/{mask}"));
            }
            at = i.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    if v4.is_empty() {
        return Err(format!("{entries} entries, no IPv4 address"));
    }
    Ok(format!("{entries} entries; {}", v4.join(", ")))
}

fn getrusage() -> Result<String, String> {
    // SAFETY: getrusage writes one struct rusage.
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) } != 0 {
        return Err(format!("{}", std::io::Error::last_os_error()));
    }
    Ok(format!("user {}.{:06} s, max rss {} KiB", u.ru_utime.tv_sec, u.ru_utime.tv_usec, u.ru_maxrss))
}

/// A thread nobody joins, which ends on its own: origin then tells the kernel, with `set_tid_address(NULL)`, not to write into
/// the stack it frees. Juno's kernel panicked on that call before patch 1150.
fn detached_thread() -> Result<String, String> {
    let done = Arc::new(AtomicUsize::new(0));
    for _ in 0..4 {
        let d = done.clone();
        drop(std::thread::spawn(move || {
            d.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let start = Instant::now();
    while done.load(Ordering::SeqCst) < 4 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Long enough for the four to have exited, not only run.
    std::thread::sleep(Duration::from_millis(200));
    let n = done.load(Ordering::SeqCst);
    if n < 4 {
        return Err(format!("{n} of 4 ran"));
    }
    Ok("4 detached threads ran and ended".into())
}

/// Every getaddrinfo error has a message: Rust's std asks for it whenever a lookup fails (EAI_AGAIN when no server answers).
fn gai_strerror() -> Result<String, String> {
    let mut out = Vec::new();
    for code in [libc::EAI_AGAIN, libc::EAI_NONAME, libc::EAI_FAIL, libc::EAI_SYSTEM, -9999] {
        // SAFETY: gai_strerror returns a NUL-terminated static string.
        let s = unsafe { std::ffi::CStr::from_ptr(libc::gai_strerror(code)) }.to_string_lossy().into_owned();
        if s.is_empty() {
            return Err(format!("{code}: empty"));
        }
        out.push(s);
    }
    Ok(out.join(" / "))
}

/// A name sysconf does not know is EINVAL and -1, not the end of the program.
fn sysconf() -> Result<String, String> {
    // SAFETY: sysconf takes any int.
    let (cpus, page, bogus) = unsafe { (libc::sysconf(libc::_SC_NPROCESSORS_ONLN), libc::sysconf(libc::_SC_PAGESIZE), libc::sysconf(123_456)) };
    if cpus < 1 || page < 4096 || bogus != -1 {
        return Err(format!("cpus {cpus}, page {page}, unknown name {bogus}"));
    }
    Ok(format!("{cpus} CPUs, page {page}, unknown name -1"))
}
