//! Step 1 check on the laptop: clock, randomness, DNS and TLS against the real roots.
use std::io::Read;
fn main() {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    println!("unix time {now}");
    let mut b = [0u8; 8];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).map(|_| println!("urandom ok")).unwrap_or_else(|e| println!("urandom: {e}"));
    let n = unsafe { getrandom(b.as_mut_ptr(), 8, 1) };
    println!("getrandom(GRND_NONBLOCK) -> {n}");
    let url = std::env::args().nth(1).unwrap_or("https://getsecretary.tech/v1/health".into());
    match std::net::ToSocketAddrs::to_socket_addrs(&(url.split("/").nth(2).unwrap(), 443)) {
        Ok(a) => println!("dns {:?}", a.collect::<Vec<_>>()),
        Err(e) => println!("dns error: {e}"),
    }
    let t = std::time::Instant::now();
    match ureq::get(&url).call() {
        Ok(mut r) => println!("{} in {:?}: {}", r.status(), t.elapsed(), r.body_mut().read_to_string().unwrap_or_default().chars().take(300).collect::<String>()),
        Err(e) => println!("error after {:?}: {e}", t.elapsed()),
    }
}
unsafe extern "C" { fn getrandom(buf: *mut u8, len: usize, flags: u32) -> isize; }
