//! A stand-in for the Secretary server on this machine, for showing the apps without the live one.
//! `lucina-secretary-fake [port] [--approve-after <s>] [--demo] [--revoked] [--juno-calls <file>] [--juno-results <file>]`.
//! `--revoked`: the device was revoked, so the token is refused. `--juno-calls`: Lucina's calls for `lucina-devd`, one JSON object a
//! line, handed out in order; `--juno-results`: where each result goes, one `{"id", "result"}` a line.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

fn main() {
    let mut port = 8931u16;
    let mut approve_after = None;
    let mut demo = false;
    let mut revoked = false;
    let mut juno_calls = None;
    let mut juno_results = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--approve-after" => approve_after = args.next().and_then(|s| s.parse().ok()),
            "--demo" => demo = true,
            "--revoked" => revoked = true,
            "--juno-calls" => juno_calls = args.next(),
            "--juno-results" => juno_results = args.next(),
            p => port = p.parse().expect("a port"),
        }
    }
    let fake = lucina_account::fake::Fake::start_on(port);
    fake.set_approve_after(approve_after);
    if demo {
        fake.fill_demo();
    }
    if revoked {
        fake.revoke();
    }
    if let Some(path) = juno_results {
        fake.set_juno_results_file(std::path::Path::new(&path));
    }
    if let Some(path) = juno_calls {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            fake.queue_juno_call(serde_json::from_str(line).unwrap_or_else(|e| panic!("{path}: {e}")));
        }
    }
    println!("{}", fake.base());
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
