//! Measures a network link between Juno and a Linux host, without SSH's cipher in the way.
//!
//! Juno runs `lucina-netperf serve --bind IP`, which picks the link by its address: buildhost's firewall takes no connections, so buildhost
//! is the client. `serve --bind 127.0.0.1` and a client on Juno measure the stack without a device. The client runs one of:
//!
//! ```text
//! lucina-netperf tcp  --to HOST --dir up|down --secs N [--bind IP[,IP...]] [--streams K]
//! lucina-netperf udp  --to HOST --dir up|down --secs N --mbit R [--bind IP]
//! lucina-netperf check --to HOST --dir up|down --secs N [--bind IP]
//! lucina-netperf rr   --to HOST --count N [--bind IP]
//! lucina-netperf cpu
//! lucina-netperf fresh-pages
//! lucina-netperf spin --secs N
//! ```
//!
//! `--bind` picks the link by its source address; several addresses are taken in turn, one per stream. Every result is
//! what the receiver counted. `report` runs on the Linux host and names what `/proc/lucina-prof` sampled.

mod prof;
mod wire;

use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    os::fd::FromRawFd,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use wire::{CHUNK, DATAGRAM, Dir, Hello, Pattern, UdpHello, udp_sequence};

const PORT: u16 = 5599;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        usage();
    };
    let options = Options::parse(&args[1..]);
    match command.as_str() {
        "serve" => serve(options.bind.first().copied()),
        "tcp" => tcp(&options),
        "check" => check(&options),
        "udp" => udp(&options),
        "rr" => rr(&options),
        "cpu" => cpu(),
        "fresh-pages" => fresh_pages(),
        "spin" => spin(options.secs),
        "report" => prof::report(&args[1..]),
        _ => usage(),
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: lucina-netperf serve [--bind IP]\n\
         \x20      lucina-netperf tcp --to HOST --dir up|down --secs N [--bind IP[,IP...]] [--streams K]\n\
         \x20      lucina-netperf udp --to HOST --dir up|down --secs N --mbit R [--bind IP]\n\
         \x20      lucina-netperf check --to HOST --dir up|down --secs N [--bind IP]\n\
         \x20      lucina-netperf rr --to HOST --count N [--bind IP]\n\
         \x20      lucina-netperf cpu\n\
         \x20      lucina-netperf fresh-pages\n\
         \x20      lucina-netperf spin --secs N\n\
         \x20      lucina-netperf report <samples> <nm -n output>"
    );
    std::process::exit(2);
}

#[derive(Default)]
struct Options {
    to: Option<Ipv4Addr>,
    dir: Option<Dir>,
    secs: u32,
    streams: usize,
    bind: Vec<Ipv4Addr>,
    mbit: u32,
    count: u32,
}

impl Options {
    fn parse(args: &[String]) -> Self {
        let mut options = Options {
            secs: 10,
            streams: 1,
            count: 1000,
            ..Options::default()
        };
        let mut args = args.iter();
        while let Some(name) = args.next() {
            let Some(value) = args.next() else {
                break;
            };
            match name.as_str() {
                "--to" => options.to = value.parse().ok(),
                "--dir" => options.dir = Dir::parse(value),
                "--secs" => options.secs = value.parse().unwrap_or(10),
                "--streams" => options.streams = value.parse().unwrap_or(1),
                "--bind" => options.bind = value.split(',').filter_map(|ip| ip.parse().ok()).collect(),
                "--mbit" => options.mbit = value.parse().unwrap_or(0),
                "--count" => options.count = value.parse().unwrap_or(1000),
                _ => {}
            }
        }
        options
    }

    fn to(&self) -> SocketAddr {
        let Some(to) = self.to else {
            usage();
        };
        SocketAddr::new(IpAddr::V4(to), PORT)
    }

    fn bind_for(&self, stream: usize) -> Option<Ipv4Addr> {
        (!self.bind.is_empty()).then(|| self.bind[stream % self.bind.len()])
    }
}

/// `connect` from a source address: `std` has no such call.
fn connect_from(source: Option<Ipv4Addr>, to: SocketAddr) -> std::io::Result<TcpStream> {
    let Some(source) = source else {
        return TcpStream::connect(to);
    };
    let IpAddr::V4(to_ip) = to.ip() else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let sockaddr = |ip: Ipv4Addr, port: u16| libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: port.to_be(),
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(ip.octets()),
        },
        sin_zero: [0; 8],
    };
    let len = size_of::<libc::sockaddr_in>() as libc::socklen_t;
    // SAFETY: The file descriptor is ours and the addresses live through the calls, which take their length.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let stream = TcpStream::from_raw_fd(fd);
        let local = sockaddr(source, 0);
        if libc::bind(fd, (&raw const local).cast(), len) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let remote = sockaddr(to_ip, to.port());
        if libc::connect(fd, (&raw const remote).cast(), len) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(stream)
    }
}

fn mbytes_per_sec(bytes: u64, seconds: f64) -> f64 {
    bytes as f64 / 1e6 / seconds.max(1e-9)
}

// The far end.

fn serve(listen: Option<Ipv4Addr>) {
    let listen = listen.unwrap_or(Ipv4Addr::UNSPECIFIED);
    let tcp = TcpListener::bind((listen, PORT)).expect("listen");
    let udp = UdpSocket::bind((listen, PORT)).expect("bind");
    thread::spawn(move || serve_udp(udp));
    eprintln!("lucina-netperf: serving TCP and UDP on port {PORT}");
    for stream in tcp.incoming().flatten() {
        thread::spawn(move || {
            if let Err(error) = serve_tcp(stream) {
                eprintln!("lucina-netperf: {error}");
            }
        });
    }
}

fn serve_tcp(mut stream: TcpStream) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;
    let hello = Hello::read(&mut stream)?;
    let mut chunk = vec![0u8; CHUNK];
    match hello {
        Hello::Tcp(Dir::Up, _) => {
            let start = Instant::now();
            let mut total = 0u64;
            loop {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
            }
            stream.write_all(&total.to_le_bytes())?;
            eprintln!(
                "{peer}: received {} MB, {:.2} MB/s",
                total / 1_000_000,
                mbytes_per_sec(total, start.elapsed().as_secs_f64())
            );
        }
        Hello::Tcp(Dir::Down, secs) => {
            let end = Instant::now() + Duration::from_secs(u64::from(secs));
            while Instant::now() < end {
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
        Hello::Check(Dir::Up, _) => {
            let (mut pattern, mut total, mut wrong) = (Pattern::default(), 0u64, 0u64);
            loop {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                wrong += pattern.wrong_bytes(&chunk[..n]);
                total += n as u64;
            }
            stream.write_all(&total.to_le_bytes())?;
            stream.write_all(&wrong.to_le_bytes())?;
        }
        Hello::Check(Dir::Down, secs) => {
            let mut pattern = Pattern::default();
            let end = Instant::now() + Duration::from_secs(u64::from(secs));
            while Instant::now() < end {
                pattern.fill(&mut chunk);
                if stream.write_all(&chunk).is_err() {
                    break;
                }
            }
        }
        Hello::Echo => {
            let mut byte = [0u8; 1];
            stream.set_nodelay(true)?;
            while stream.read_exact(&mut byte).is_ok() {
                stream.write_all(&byte)?;
            }
        }
    }
    Ok(())
}

fn serve_udp(socket: UdpSocket) {
    let mut buf = vec![0u8; 2048];
    let mut receiving: Option<Receiving> = None;
    loop {
        let Ok((n, peer)) = socket.recv_from(&mut buf) else {
            continue;
        };
        match UdpHello::parse(&buf[..n]) {
            Some(UdpHello::SendToMe { secs, mbit }) => {
                let socket = socket.try_clone().expect("clone");
                thread::spawn(move || udp_blast(&socket, peer, secs, mbit));
            }
            Some(UdpHello::Report) => {
                let (frames, bytes) = receiving.take().map_or((0, 0), |r| (r.frames, r.bytes));
                let mut reply = [0u8; 16];
                reply[..8].copy_from_slice(&frames.to_le_bytes());
                reply[8..].copy_from_slice(&bytes.to_le_bytes());
                let _ = socket.send_to(&reply, peer);
            }
            None => {
                let r = receiving.get_or_insert(Receiving { frames: 0, bytes: 0 });
                r.frames += 1;
                r.bytes += n as u64;
            }
        }
    }
}

struct Receiving {
    frames: u64,
    bytes: u64,
}

/// Sends datagrams at `mbit`, paced in slices of a millisecond.
fn udp_blast(socket: &UdpSocket, to: SocketAddr, secs: u32, mbit: u32) -> u64 {
    let mut datagram = [0u8; DATAGRAM];
    let per_ms = f64::from(mbit) * 1e6 / 8.0 / DATAGRAM as f64 / 1000.0;
    let start = Instant::now();
    let total = Duration::from_secs(u64::from(secs));
    let mut sent = 0u64;
    while start.elapsed() < total {
        let due = (start.elapsed().as_secs_f64() * 1000.0 * per_ms) as u64;
        if sent >= due {
            thread::sleep(Duration::from_micros(200));
            continue;
        }
        datagram[..8].copy_from_slice(&udp_sequence(sent));
        if socket.send_to(&datagram, to).is_ok() {
            sent += 1;
        }
    }
    sent
}

// Juno's end.

fn tcp(options: &Options) {
    let dir = options.dir.unwrap_or(Dir::Down);
    let received = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let mut workers = Vec::new();
    for stream in 0..options.streams {
        let source = options.bind_for(stream);
        let to = options.to();
        let secs = options.secs;
        let received = received.clone();
        workers.push(thread::spawn(move || {
            let result = tcp_stream(source, to, dir, secs, &received);
            (source, result)
        }));
    }
    let meter = {
        let received = received.clone();
        let stop = stop.clone();
        thread::spawn(move || {
            let mut last = 0;
            let mut second = 0;
            while !stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
                let now = received.load(Ordering::Relaxed);
                second += 1;
                println!("  {second:3} s  {:7.2} MB/s", (now - last) as f64 / 1e6);
                last = now;
            }
        })
    };
    let mut total = 0;
    let mut failed = 0;
    for worker in workers {
        let (source, result) = worker.join().expect("worker");
        match result {
            Ok((bytes, seconds)) => {
                total += bytes;
                println!(
                    "stream from {}: {} bytes in {seconds:.2} s, {:.2} MB/s",
                    source.map_or("any".to_string(), |ip| ip.to_string()),
                    bytes,
                    mbytes_per_sec(bytes, seconds)
                );
            }
            Err(error) => {
                failed += 1;
                println!("stream failed: {error}");
            }
        }
    }
    let seconds = start.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    let _ = meter.join();
    println!(
        "RESULT tcp {} streams {} failed {failed}: {total} bytes in {seconds:.2} s, {:.2} MB/s, {:.1} Mbit/s",
        dir.name(),
        options.streams,
        mbytes_per_sec(total, seconds),
        mbytes_per_sec(total, seconds) * 8.0
    );
}

/// One stream of data the receiver checks byte by byte. A wrong byte got past the Ethernet, TCP and Wi-Fi checks, or was changed after them.
fn check(options: &Options) {
    let dir = options.dir.unwrap_or(Dir::Down);
    let mut stream = connect_from(options.bind_for(0), options.to()).expect("connect");
    Hello::Check(dir, options.secs).write(&mut stream).expect("hello");
    let start = Instant::now();
    let mut chunk = vec![0u8; CHUNK];
    let mut pattern = Pattern::default();
    let (total, wrong) = match dir {
        Dir::Down => {
            let (mut total, mut wrong) = (0u64, 0u64);
            loop {
                let n = stream.read(&mut chunk).expect("read");
                if n == 0 {
                    break;
                }
                wrong += pattern.wrong_bytes(&chunk[..n]);
                total += n as u64;
            }
            (total, wrong)
        }
        Dir::Up => {
            let end = start + Duration::from_secs(u64::from(options.secs));
            while Instant::now() < end {
                pattern.fill(&mut chunk);
                stream.write_all(&chunk).expect("write");
            }
            stream.shutdown(std::net::Shutdown::Write).expect("shutdown");
            let mut reply = [0u8; 16];
            stream.read_exact(&mut reply).expect("reply");
            let field = |at: usize| {
                let mut word = [0u8; 8];
                word.copy_from_slice(&reply[at..at + 8]);
                u64::from_le_bytes(word)
            };
            (field(0), field(8))
        }
    };
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "RESULT check {}: {total} bytes, {wrong} wrong, {seconds:.2} s, {:.2} MB/s",
        dir.name(),
        mbytes_per_sec(total, seconds)
    );
}

/// One stream: returns what the receiver counted, and the time it took.
fn tcp_stream(
    source: Option<Ipv4Addr>,
    to: SocketAddr,
    dir: Dir,
    secs: u32,
    progress: &AtomicU64,
) -> std::io::Result<(u64, f64)> {
    let mut stream = connect_from(source, to)?;
    Hello::Tcp(dir, secs).write(&mut stream)?;
    let start = Instant::now();
    let mut chunk = vec![0u8; CHUNK];
    match dir {
        Dir::Down => {
            let mut total = 0u64;
            loop {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                progress.fetch_add(n as u64, Ordering::Relaxed);
            }
            Ok((total, start.elapsed().as_secs_f64()))
        }
        Dir::Up => {
            let end = start + Duration::from_secs(u64::from(secs));
            while Instant::now() < end {
                stream.write_all(&chunk)?;
                progress.fetch_add(CHUNK as u64, Ordering::Relaxed);
            }
            stream.shutdown(std::net::Shutdown::Write)?;
            let mut total = [0u8; 8];
            stream.read_exact(&mut total)?;
            Ok((u64::from_le_bytes(total), start.elapsed().as_secs_f64()))
        }
    }
}

fn udp(options: &Options) {
    let dir = options.dir.unwrap_or(Dir::Down);
    let source = options.bind_for(0).unwrap_or(Ipv4Addr::UNSPECIFIED);
    let socket = UdpSocket::bind((source, 0)).expect("bind");
    socket.connect(options.to()).expect("connect");
    let mbit = options.mbit.max(1);
    match dir {
        Dir::Down => {
            socket
                .send(&UdpHello::SendToMe {
                    secs: options.secs,
                    mbit,
                }
                .encode())
                .expect("send");
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("timeout");
            let mut buf = vec![0u8; 2048];
            let (mut frames, mut bytes, mut highest) = (0u64, 0u64, 0u64);
            let mut first = None;
            let mut last = Instant::now();
            while let Ok(n) = socket.recv(&mut buf) {
                last = Instant::now();
                first.get_or_insert(last);
                frames += 1;
                bytes += n as u64;
                if n >= 8 {
                    let mut sequence = [0u8; 8];
                    sequence.copy_from_slice(&buf[..8]);
                    highest = highest.max(u64::from_le_bytes(sequence));
                }
            }
            let seconds = first.map_or(0.0, |first| (last - first).as_secs_f64());
            println!(
                "RESULT udp down asked {mbit} Mbit/s: received {frames} of about {} datagrams, {:.2} MB/s, {:.1} Mbit/s, {:.1}% lost",
                highest + 1,
                mbytes_per_sec(bytes, seconds),
                mbytes_per_sec(bytes, seconds) * 8.0,
                100.0 * (1.0 - frames as f64 / (highest + 1).max(1) as f64)
            );
        }
        Dir::Up => {
            let per_ms = f64::from(mbit) * 1e6 / 8.0 / DATAGRAM as f64 / 1000.0;
            let datagram = [0x55u8; DATAGRAM];
            let start = Instant::now();
            let total = Duration::from_secs(u64::from(options.secs));
            let (mut sent, mut refused) = (0u64, 0u64);
            while start.elapsed() < total {
                let due = (start.elapsed().as_secs_f64() * 1000.0 * per_ms) as u64;
                if sent >= due {
                    thread::yield_now();
                    continue;
                }
                if socket.send(&datagram).is_ok() {
                    sent += 1;
                } else {
                    refused += 1;
                }
            }
            let seconds = start.elapsed().as_secs_f64();
            thread::sleep(Duration::from_secs(1));
            socket.send(&UdpHello::Report.encode()).expect("send");
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("timeout");
            let mut reply = [0u8; 16];
            let n = socket.recv(&mut reply).unwrap_or(0);
            if n != 16 {
                println!("RESULT udp up: no report from the far end");
                return;
            }
            let mut field = [0u8; 8];
            field.copy_from_slice(&reply[..8]);
            let frames = u64::from_le_bytes(field);
            field.copy_from_slice(&reply[8..]);
            let bytes = u64::from_le_bytes(field);
            println!(
                "RESULT udp up asked {mbit} Mbit/s: sent {sent} (refused {refused}), arrived {frames}, {:.2} MB/s, {:.1} Mbit/s",
                mbytes_per_sec(bytes, seconds),
                mbytes_per_sec(bytes, seconds) * 8.0
            );
        }
    }
}

/// Round trips of one byte over TCP. Juno has no ping that gets replies.
fn rr(options: &Options) {
    let mut stream = connect_from(options.bind_for(0), options.to()).expect("connect");
    stream.set_nodelay(true).expect("nodelay");
    Hello::Echo.write(&mut stream).expect("hello");
    let mut times = Vec::with_capacity(options.count as usize);
    let mut byte = [0u8; 1];
    for _ in 0..options.count {
        let start = Instant::now();
        stream.write_all(&byte).expect("write");
        stream.read_exact(&mut byte).expect("read");
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    println!(
        "RESULT rr {} round trips: min {:.2} ms, median {:.2} ms, p90 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        times.len(),
        at(0.0),
        at(0.5),
        at(0.9),
        at(0.99),
        at(1.0)
    );
}

/// UDP through pages no one has touched yet, as a fresh program's buffers are: two datagrams waiting on one socket, each read into a
/// fresh page, and one sent from a fresh page. The kernel once lost the first datagram to a fault in the copy, and sent a datagram twice.
fn fresh_pages() {
    const PAGE: usize = 4096;
    let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let to = rx.local_addr().expect("address");
    let tx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    tx.send_to(b"first", to).expect("send");
    tx.send_to(b"second", to).expect("send");
    thread::sleep(Duration::from_millis(200));
    rx.set_nonblocking(true).expect("nonblocking");
    // SAFETY: A private anonymous mapping of three pages, used only here and never unmapped, which is fine for a process that ends.
    let pages = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            3 * PAGE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_ne!(pages, libc::MAP_FAILED, "mmap");
    let page = |i: usize| {
        // SAFETY: Inside the mapping above, and each page is used for one thing at a time.
        unsafe { core::slice::from_raw_parts_mut((pages as *mut u8).add(i * PAGE), PAGE) }
    };
    let mut got = Vec::new();
    for i in 0..2 {
        let buf = page(i);
        match rx.recv(buf) {
            Ok(n) => got.push(String::from_utf8_lossy(&buf[..n]).into_owned()),
            Err(error) => got.push(format!("<{error}>")),
        }
    }
    println!("RESULT fresh-pages recv: {}", got.join(" "));

    // One datagram from a page that has never been read or written: the copy faults on its way in.
    let out = page(2);
    tx.send_to(&out[..100], to).expect("send");
    thread::sleep(Duration::from_millis(200));
    let mut buf = [0u8; 256];
    let mut arrived = Vec::new();
    while let Ok(n) = rx.recv(&mut buf) {
        arrived.push(n);
    }
    println!(
        "RESULT fresh-pages send: {} datagram(s) arrived, sizes {:?}",
        arrived.len(),
        arrived
    );
}

/// What this CPU can do, in user space on cached memory, for comparing with what the links ask of it.
fn cpu() {
    let spin = spin_rate();
    let source = vec![0xa5u8; 1 << 20];
    let mut destination = vec![0u8; 1 << 20];
    let copy = rate_of(
        || {
            std::hint::black_box(&mut destination).copy_from_slice(std::hint::black_box(&source));
        },
        source.len(),
    );
    let checksum = rate_of(
        || {
            std::hint::black_box(wire::internet_checksum(&source));
        },
        source.len(),
    );
    let frame = vec![0xa5u8; 1500];
    let mut frame_out = vec![0u8; 1500];
    let small_copy = rate_of(
        || {
            std::hint::black_box(&mut frame_out).copy_from_slice(std::hint::black_box(&frame));
        },
        frame.len(),
    );
    println!("RESULT cpu: about {:.0} MHz by a counting loop", spin / 1e6);
    println!(
        "RESULT cpu: copy {:.0} MB/s (1 MiB), {:.0} MB/s (1500 bytes), Internet checksum {:.0} MB/s",
        copy / 1e6,
        small_copy / 1e6,
        checksum / 1e6
    );
}

/// How much of the CPU a program can still get: counts for `secs`, a second at a time. Against the same run with nothing else going on,
/// the drop is what everything else took, including what a profile misses because interrupts were off.
fn spin(secs: u32) {
    for second in 1..=secs {
        println!("spin {second} s: {:.0} M per second", spin_rate() / 1e6);
    }
}

/// Increments per second of a loop that takes one increment per cycle on these cores.
fn spin_rate() -> f64 {
    let start = Instant::now();
    let mut counter = 0u64;
    while start.elapsed() < Duration::from_millis(1000) {
        for _ in 0..1_000_000u32 {
            // SAFETY: Adds to a register and touches nothing else.
            #[cfg(target_arch = "aarch64")]
            unsafe {
                core::arch::asm!("add {0}, {0}, #1", inout(reg) counter);
            }
            #[cfg(not(target_arch = "aarch64"))]
            {
                counter = std::hint::black_box(counter + 1);
            }
        }
    }
    counter as f64 / start.elapsed().as_secs_f64()
}

fn rate_of(mut work: impl FnMut(), bytes: usize) -> f64 {
    let start = Instant::now();
    let mut rounds = 0u64;
    while start.elapsed() < Duration::from_millis(300) {
        for _ in 0..64 {
            work();
        }
        rounds += 64;
    }
    rounds as f64 * bytes as f64 / start.elapsed().as_secs_f64()
}
