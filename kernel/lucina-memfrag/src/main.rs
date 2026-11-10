//! lucina-memfrag: leaves the guest's free memory in single pages, then opens many pipes and fills each, so that a kernel
//! whose pipe buffers need a contiguous block runs out where one that takes a page at a time does not
//! (scripts/qemu-crash.sh, scenario `frag`).
//!
//! `lucina-memfrag [free-kB-to-leave] [count] [pipes|tcp]`: maps and touches anonymous memory until `MemFree` is below the
//! first argument (default 96 MiB), frees every second page of it, waits for the frees to land, then
//! - `pipes` (the default): makes up to `count` pipes (default 1000), writes 64 KiB into each and keeps them open;
//! - `tcp` (Juno boot 11, patch 1053): listens on 127.0.0.1, makes up to `count` connections to itself, sends 16 KiB each
//!   way on each and keeps them open, so that a kernel whose TCP buffers need a contiguous block runs out where one that
//!   takes them a page at a time gives ENOMEM (or drops a SYN) at worst.
//! Prints one line and exits 0.

use std::{thread::sleep, time::Duration};

const PAGE: usize = 4096;
/// Small enough that the last step cannot run memory out: the kernel needs the rest.
const CHUNK: usize = 8 << 20;
const PIPE_BYTES: usize = 65536;

fn mem_free_kb() -> usize {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("MemFree:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb| kb.parse().ok())
        })
        .unwrap_or(0)
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `lucina-memfrag malloc [rounds]` (Juno boot 88, 2026-11-08): what libc-bench did before the laptop hung. 600 fork+execs
/// of this program (as its `start_exec_noop`), then `rounds` (default 6: warm-up plus 5) of 2,000,000 allocate-and-free
/// pairs of 64 bytes through musl's malloc. One line per step with its time and `MemFree`.
fn malloc_loop(rounds: usize) {
    let exe = std::env::current_exe().unwrap_or_else(|_| "/test/lucina-memfrag".into());
    let t = std::time::Instant::now();
    let mut failed = 0;
    for _ in 0..600 {
        match std::process::Command::new(&exe).arg("noop").status() {
            Ok(s) if s.success() => {}
            _ => failed += 1,
        }
    }
    println!("memfrag: exec 600 in {} ms, {failed} failed, free {} kB", t.elapsed().as_millis(), mem_free_kb());
    for round in 0..rounds {
        let t = std::time::Instant::now();
        for i in 0..2_000_000u32 {
            std::hint::black_box(Box::new([i as u8; 64]));
        }
        println!("memfrag: malloc round {round} in {} ms, free {} kB", t.elapsed().as_millis(), mem_free_kb());
    }
    println!("memfrag: malloc ok {rounds}");
}

/// `lucina-memfrag unmap [rounds]` (patch 1181's test): maps 512 MiB, writes every page, unmaps it, back to back, as a
/// JavaScript heap does on a page load. Prints the worst amount given back but not yet free (MemFree below where it started,
/// at the moment nothing is mapped) and the most `DeferredFrees` seen. A kernel that frees late runs out of memory here.
fn unmap_churn(rounds: usize) {
    const SIZE: usize = 512 << 20;
    let deferred = || {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("DeferredFrees:").map(|v| v.trim().parse::<u64>().unwrap_or(0))))
            .unwrap_or(0)
    };
    let start_kb = mem_free_kb();
    let (mut worst_kb, mut most_deferred) = (0usize, 0u64);
    for round in 0..rounds {
        // SAFETY: an anonymous private mapping at an address the kernel chooses; nothing else refers to it.
        let p = unsafe {
            libc::mmap(core::ptr::null_mut(), SIZE, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if p == libc::MAP_FAILED {
            println!("memfrag: unmap: mmap failed in round {round} (errno {})", errno());
            return;
        }
        let p = p.cast::<u8>();
        for off in (0..SIZE).step_by(PAGE) {
            // SAFETY: inside the mapping just made, which is readable and writable.
            unsafe { p.add(off).write_volatile(round as u8) };
        }
        // SAFETY: the whole mapping just made, which nothing refers to any more.
        unsafe { libc::munmap(p.cast(), SIZE) };
        let lag = start_kb.saturating_sub(mem_free_kb());
        worst_kb = worst_kb.max(lag);
        most_deferred = most_deferred.max(deferred());
    }
    sleep(Duration::from_secs(3));
    println!(
        "memfrag: unmap ok {rounds}: worst {} MiB given back but not free, most deferred {most_deferred}, {} MiB below the start after 3 s",
        worst_kb >> 10,
        start_kb.saturating_sub(mem_free_kb()) >> 10
    );
}

/// `lucina-memfrag hog` (patch 1118's test): maps and touches memory 8 MiB at a time and never gives it back, until the OOM
/// killer ends it. One line every 256 MiB.
fn hog() {
    let mut mib = 0usize;
    loop {
        // SAFETY: an anonymous private mapping at an address the kernel chooses; nothing else refers to it.
        let p = unsafe {
            libc::mmap(core::ptr::null_mut(), CHUNK, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if p == libc::MAP_FAILED {
            println!("memfrag: hog: mmap failed at {mib} MiB (errno {}); touching what it has", errno());
            sleep(Duration::from_secs(1));
            continue;
        }
        let p = p.cast::<u8>();
        for off in (0..CHUNK).step_by(PAGE) {
            // SAFETY: inside the mapping just made, which is readable and writable.
            unsafe { p.add(off).write_volatile(1) };
        }
        mib += CHUNK >> 20;
        if mib % 256 == 0 {
            println!("memfrag: hog: {mib} MiB, free {} kB", mem_free_kb());
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = std::env::args().nth(1);
    if first.as_deref() == Some("noop") {
        return;
    }
    if first.as_deref() == Some("unmap") {
        unmap_churn(std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(20));
        return;
    }
    if first.as_deref() == Some("hog") {
        hog();
        return;
    }
    if first.as_deref() == Some("malloc") {
        malloc_loop(std::env::args().nth(2).and_then(|a| a.parse().ok()).unwrap_or(6));
        return;
    }
    let leave_kb: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(96 * 1024);
    let pipes: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(1000);
    let tcp = args.next().as_deref() == Some("tcp");

    // Room for the pipes' descriptors.
    let limit = libc::rlimit { rlim_cur: (2 * pipes + 64) as libc::rlim_t, rlim_max: (2 * pipes + 64) as libc::rlim_t };
    // SAFETY: setrlimit reads the struct, which lives through the call.
    unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };

    let start_kb = mem_free_kb();
    let mut regions: Vec<*mut u8> = Vec::new();
    while mem_free_kb() > leave_kb + (CHUNK >> 10) {
        // SAFETY: an anonymous private mapping at an address the kernel chooses; nothing else refers to it.
        let p = unsafe {
            libc::mmap(core::ptr::null_mut(), CHUNK, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0)
        };
        if p == libc::MAP_FAILED {
            break;
        }
        let p = p.cast::<u8>();
        for off in (0..CHUNK).step_by(PAGE) {
            // SAFETY: inside the mapping just made, which is readable and writable.
            unsafe { p.add(off).write_volatile(1) };
        }
        regions.push(p);
    }
    let filled_kb = mem_free_kb();
    for &p in &regions {
        for off in (PAGE..CHUNK).step_by(2 * PAGE) {
            // SAFETY: one page of a mapping this program made and no longer uses.
            unsafe { libc::munmap(p.add(off).cast(), PAGE) };
        }
    }
    // Freed pages come back after a grace period.
    sleep(Duration::from_secs(3));
    let holed_kb = mem_free_kb();

    if tcp {
        tcp_connections(pipes, start_kb, filled_kb, holed_kb);
        return;
    }

    let buf = vec![0x5au8; PIPE_BYTES];
    let (mut ok, mut enomem, mut other) = (0usize, 0usize, 0usize);
    let mut open: Vec<[libc::c_int; 2]> = Vec::new();
    for _ in 0..pipes {
        let mut pair = [0 as libc::c_int; 2];
        // SAFETY: pipe2 writes two descriptors into the array.
        if unsafe { libc::pipe2(pair.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            match errno() {
                libc::ENOMEM => enomem += 1,
                libc::EMFILE | libc::ENFILE => break,
                _ => other += 1,
            }
            continue;
        }
        // SAFETY: writes from a buffer of that length to a descriptor this program owns.
        let n = unsafe { libc::write(pair[1], buf.as_ptr().cast(), PIPE_BYTES) };
        if n < 0 {
            match errno() {
                libc::ENOMEM => enomem += 1,
                _ => other += 1,
            }
        } else {
            ok += 1;
        }
        open.push(pair);
    }
    println!(
        "memfrag: pipes ok {ok}, ENOMEM {enomem}, other {other}, free {start_kb} kB at start, {filled_kb} filled, {holed_kb} with every second page freed, {} now",
        mem_free_kb()
    );
    for pair in open {
        // SAFETY: descriptors this program owns.
        unsafe {
            libc::close(pair[0]);
            libc::close(pair[1]);
        }
    }
}

/// Bytes sent each way on each connection.
const TCP_BYTES: usize = 16384;

/// The `tcp` mode: up to `count` connections to a listener of this program, 16 KiB each way, all kept open.
fn tcp_connections(count: usize, start_kb: usize, filled_kb: usize, holed_kb: usize) {
    use std::{
        io::{ErrorKind, Read, Write},
        net::{TcpListener, TcpStream},
    };

    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            println!("memfrag: tcp listen failed: {err}");
            return;
        }
    };
    let addr = listener.local_addr().expect("a bound listener has an address");
    let out = vec![0x5au8; TCP_BYTES];
    let mut back = vec![0u8; TCP_BYTES];
    let (mut ok, mut enomem, mut timeout, mut other) = (0usize, 0usize, 0usize, 0usize);
    let mut open = Vec::new();
    // Once memory is gone every try fails, each a SYN timeout at worst: stop after a few in a row.
    let mut failed_in_a_row = 0;
    for _ in 0..count {
        if failed_in_a_row >= 8 {
            break;
        }
        let client = match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(client) => client,
            Err(err) => {
                failed_in_a_row += 1;
                match err.raw_os_error() {
                    Some(libc::ENOMEM) => enomem += 1,
                    Some(libc::EMFILE | libc::ENFILE) => break,
                    _ if err.kind() == ErrorKind::TimedOut => timeout += 1,
                    _ => other += 1,
                }
                continue;
            }
        };
        let server = match listener.accept() {
            Ok((server, _)) => server,
            Err(_) => {
                other += 1;
                continue;
            }
        };
        let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = server.set_read_timeout(Some(Duration::from_secs(5)));
        let (mut client, mut server) = (client, server);
        let moved = client.write_all(&out).is_ok()
            && server.read_exact(&mut back).is_ok()
            && back == out
            && server.write_all(&out).is_ok()
            && client.read_exact(&mut back).is_ok()
            && back == out;
        if moved {
            ok += 1;
            failed_in_a_row = 0;
        } else {
            other += 1;
        }
        open.push((client, server));
    }
    println!(
        "memfrag: tcp ok {ok}, ENOMEM {enomem}, SYN timeouts {timeout}, other {other}, free {start_kb} kB at start, {filled_kb} filled, {holed_kb} with every second page freed, {} now",
        mem_free_kb()
    );
    drop(open);
}
