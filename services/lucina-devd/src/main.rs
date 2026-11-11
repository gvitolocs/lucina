//! `lucina-devd`: started by `boot/remote/remote.sh`, again if it ends. Takes no arguments; `LUCINA_SECRETARY_URL` names another
//! server, as for every program on the machine's sign-in.
// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::sync::atomic::AtomicBool;

use lucina_account::system;
use lucina_devd::audit::Audit;
use lucina_devd::poll::{Pace, Service};
use lucina_devd::tools::Machine;

fn main() {
    if std::env::args().len() > 1 {
        eprintln!("usage: lucina-devd");
        std::process::exit(2);
    }
    // A write to a socket the shell has closed is an error to handle, not a reason to end.
    // SAFETY: setting a signal's disposition to ignore.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let root = std::path::Path::new("/");
    let service = Service::new(Machine::system(), Audit::under(root), Pace::default());
    eprintln!(
        "lucina-devd: started; audit in {}",
        service.audit.path().display()
    );
    static NEVER: AtomicBool = AtomicBool::new(false);
    let events = service.machine.events_socket.clone();
    std::thread::spawn(move || lucina_devd::events::forward(&events, &system::client, &NEVER));
    service.run(&system::client, &system::report_rejected, &NEVER);
}
