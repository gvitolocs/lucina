//! lucina-app-desk: a RustDesk viewer on Juno. It views and drives a remote desktop through a self-hosted hbbs and hbbr.
//!
//! Licence: AGPL-3.0-only, through `lucinadesk-core`, which copies RustDesk's protocol files. Kept to this one program;
//! served to the owner only, by the decision recorded in `docs/research/juno-desk.md`.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod host;
mod keymap;
mod link;
mod screen;
mod video;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lucina_app::app::FullScreen;
use lucina_app::keys;
use lucina_app::{App, Sender, TextEdit, Ui};

use link::{Live, Target};

const CONFIG: &str = "/data/lucina/desk/client";
/// A session the form hands to the loop.
type Pending = Arc<Live>;

enum Msg {
    Connected(Pending),
    Failed(String),
}

struct Desk {
    tx: Option<Sender<Msg>>,
    server: TextEdit,
    key: TextEdit,
    id: TextEdit,
    password: TextEdit,
    connecting: bool,
    status: Option<String>,
    pending: Option<Pending>,
    ended: Arc<std::sync::Mutex<Option<String>>>,
    config: PathBuf,
    host_on: bool,
    reach_file: PathBuf,
    web_on: bool,
    web_file: PathBuf,
}

/// What is kept between runs. The password never is.
#[derive(Debug, Default, PartialEq, Eq)]
struct Saved {
    server: String,
    key: String,
    id: String,
}

impl Saved {
    fn parse(text: &str) -> Saved {
        let mut s = Saved::default();
        for line in text.lines() {
            match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
                Some(("server", v)) => s.server = v.to_owned(),
                Some(("key", v)) => s.key = v.to_owned(),
                Some(("id", v)) => s.id = v.to_owned(),
                _ => {}
            }
        }
        s
    }

    fn text(&self) -> String {
        format!("server = {}\nkey = {}\nid = {}\n", self.server, self.key, self.id)
    }

    fn load(path: &Path) -> Saved {
        let mut s = std::fs::read_to_string(path).map(|t| Saved::parse(&t)).unwrap_or_default();
        // A test sets these; a person types them once.
        for (name, field) in [("LUCINA_DESK_SERVER", &mut s.server), ("LUCINA_DESK_KEY", &mut s.key), ("LUCINA_DESK_ID", &mut s.id)] {
            if let Ok(v) = std::env::var(name) {
                *field = v;
            }
        }
        s
    }

    fn save(&self, path: &Path) {
        let Some(dir) = path.parent() else { return };
        // No /data (a boot from the stick, QEMU): nothing is kept, and that is fine.
        if std::fs::create_dir_all(dir).is_ok() {
            let _ = std::fs::write(path, self.text());
        }
    }
}

/// `host` or `host:port` of the rendezvous server into the two WebSocket addresses: hbbs on the port, hbbr on the next.
fn urls(server: &str) -> Result<(String, String), String> {
    let s = server.trim().trim_start_matches("ws://").trim_end_matches('/');
    if s.is_empty() {
        return Err("Type the address of the RustDesk server.".into());
    }
    let (host, port) = match s.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("{p} is not a port"))?),
        None => (s, 21128),
    };
    let next = port.checked_add(1).ok_or("that port is too high")?;
    Ok((format!("ws://{host}:{port}"), format!("ws://{host}:{next}")))
}

impl Desk {
    fn new(tx: Option<Sender<Msg>>, config: PathBuf) -> Desk {
        let saved = Saved::load(&config);
        let mut d = Desk {
            tx,
            server: TextEdit::new().placeholder("Server, as host or host:port"),
            key: TextEdit::new().placeholder("Server key"),
            id: TextEdit::new().placeholder("ID of the desktop"),
            password: TextEdit::new().placeholder("Password"),
            connecting: false,
            status: None,
            pending: None,
            ended: Arc::new(std::sync::Mutex::new(None)),
            config,
            host_on: host::control::is_on(host::control::Host::Direct),
            reach_file: PathBuf::from(host::direct::REACH_FILE),
            web_on: host::control::is_on(host::control::Host::Web),
            web_file: PathBuf::from(host::web::reach_file()),
        };
        d.password.password = true;
        d.server.set(&saved.server);
        d.key.set(&saved.key);
        d.id.set(&saved.id);
        // A test logs in by itself. Only ever through the environment, so never in a shipped tile.
        if let Ok(pw) = std::env::var("LUCINA_DESK_TEST_PASSWORD") {
            d.password.set(&pw);
            d.connect();
        }
        d
    }

    /// Makes this laptop reachable from this network, with a new password. Until the next restart.
    fn host_turn_on(&mut self) {
        match host::control::turn_on(host::control::Host::Direct) {
            Ok(()) => {
                self.host_on = true;
                self.status = Some("This laptop can be reached until the next restart, or until you turn it off.".into());
                // The host writes what a client needs as soon as it listens.
                for _ in 0..40 {
                    if host::direct::read_reach(&self.reach_file).is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            Err(e) => self.status = Some(e),
        }
    }

    /// Lets a browser on this network see and drive this laptop, with a new password. Until the next restart.
    fn web_turn_on(&mut self) {
        match host::control::turn_on(host::control::Host::Web) {
            Ok(()) => {
                self.web_on = true;
                self.status = Some("A browser can reach this laptop until the next restart, or until you turn it off.".into());
                for _ in 0..40 {
                    if host::web::read_reach(&self.web_file).is_some() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            Err(e) => self.status = Some(e),
        }
    }

    fn connect(&mut self) {
        if self.connecting {
            return;
        }
        let (rendezvous, relay) = match urls(&self.server.text) {
            Ok(u) => u,
            Err(e) => {
                self.status = Some(e);
                return;
            }
        };
        if self.id.text.trim().is_empty() || self.key.text.trim().is_empty() || self.password.text.is_empty() {
            self.status = Some("The server key, the ID and the password are all needed.".into());
            return;
        }
        let saved = Saved { server: self.server.text.trim().into(), key: self.key.text.trim().into(), id: self.id.text.trim().into() };
        saved.save(&self.config);
        let target = Target {
            rendezvous,
            relay,
            key: saved.key,
            id: saved.id,
            password: self.password.text.clone(),
        };
        self.connecting = true;
        self.status = Some("Connecting...".into());
        if let Some(tx) = &self.tx {
            tx.spawn(move || match link::connect(&target, "Juno") {
                Ok(live) => Msg::Connected(live),
                Err(e) => Msg::Failed(e),
            });
        }
    }
}

impl App for Desk {
    type Msg = Msg;

    fn message(&mut self, m: Msg) {
        self.connecting = false;
        match m {
            Msg::Connected(live) => {
                self.status = None;
                self.pending = Some(live);
            }
            Msg::Failed(e) => {
                eprintln!("lucina-app-desk: {e}");
                self.status = Some(e);
            }
        }
    }

    fn take_screen(&mut self) -> Option<Box<dyn FnOnce(&mut FullScreen)>> {
        let live = self.pending.take()?;
        let ended = self.ended.clone();
        Some(Box::new(move |fs: &mut FullScreen| {
            let why = screen::run(&live, fs);
            eprintln!("lucina-app-desk: session over: {why}");
            if let Ok(mut e) = ended.lock() {
                *e = Some(why);
            }
        }))
    }

    fn view(&mut self, ui: &mut Ui) {
        if let Ok(mut e) = self.ended.lock() {
            if let Some(why) = e.take() {
                self.status = Some(format!("The session ended: {why}."));
                self.password.clear();
            }
        }
        ui.header("Remote desktop");
        ui.pad(14.0);
        let side = ((ui.area().w - ui.px(620.0)) / 2).max(0);
        ui.cut_left(side);
        ui.cut_right(side);
        let mut submit = false;
        ui.dim("RustDesk, through your own server.");
        let gap = ui.px(10.0);
        ui.cut_top(gap);
        ui.label("Server");
        submit |= ui.text_field("server", &mut self.server).submitted;
        ui.cut_top(gap);
        ui.label("Server key");
        submit |= ui.text_field("key", &mut self.key).submitted;
        ui.cut_top(gap);
        ui.label("ID");
        submit |= ui.text_field("id", &mut self.id).submitted;
        ui.cut_top(gap);
        ui.label("Password");
        submit |= ui.text_field("password", &mut self.password).submitted;
        ui.cut_top(gap);
        if ui.primary_button(if self.connecting { "Connecting" } else { "Connect" }) {
            submit = true;
        }
        ui.cut_top(gap);
        if let Some(s) = &self.status {
            ui.paragraph(s);
        }
        ui.cut_top(gap);
        ui.dim("In a session, F1 goes home and Ctrl+Alt+Shift+Q comes back here. The host cannot be sent F1.");
        ui.cut_top(gap);
        ui.separator();
        ui.cut_top(gap);
        // The two ways into this laptop side by side, across the screen's width, so that both fit while both are on.
        let below = ui.area();
        let margin = ui.px(40.0);
        let (x0, w) = (below.x - side + margin, below.w + 2 * side - 2 * margin);
        let col = (w - gap * 3) / 2;
        let left = lucina_app::Rect { x: x0, y: below.y, w: col, h: below.h };
        let right = lucina_app::Rect { x: x0 + col + gap * 3, y: below.y, w: col, h: below.h };
        ui.within(left, |ui| {
            ui.label("This laptop, for a RustDesk client");
            if self.host_on {
                // This laptop's address and password: for the owner's eyes, not Lucina's.
                ui.private("This laptop's address", |ui| ui.paragraph(&reach_text(host::direct::read_reach(&self.reach_file).as_ref())));
                ui.paragraph("Whoever connects sees and drives this screen, and nothing here shows that they are there. The picture and the keys cross the network unencrypted, so use this on a network you trust.");
                if ui.button_id("host-off", "Turn it off") {
                    host::control::turn_off(host::control::Host::Direct);
                    self.host_on = false;
                    self.status = Some("This laptop can no longer be reached.".into());
                }
            } else {
                ui.dim("Off. Turned on, a RustDesk client on this network can reach it with a new password, until the next restart.");
                if ui.button("Let another device reach this laptop") {
                    self.host_turn_on();
                }
            }
        });
        ui.within(right, |ui| {
            ui.label("This laptop, in a browser");
            if self.web_on {
                // The browser link carries the password: for the owner's eyes, not Lucina's.
                ui.private("This laptop's browser link", |ui| ui.paragraph(&web_text(host::web::read_reach(&self.web_file).as_ref())));
                ui.paragraph("Whoever opens it sees and drives this screen, unencrypted: use it on a network you trust. Browsers show the picture only on secure pages: open it as localhost through a forwarded port, or mark the address as secure.");
                if ui.button_id("web-off", "Turn it off") {
                    host::control::turn_off(host::control::Host::Web);
                    self.web_on = false;
                    self.status = Some("A browser can no longer reach this laptop.".into());
                }
            } else {
                ui.dim("Off. Turned on, a browser on this network sees and drives this screen, until the next restart.");
                if ui.button("Let a browser see this laptop") {
                    self.web_turn_on();
                }
            }
        });
        if submit {
            self.connect();
        }
        if ui.key(keys::KEY_ESC) {
            ui.quit();
        }
    }
}

/// The whole path without a screen, for a test against a real host, from a machine with no panel:
/// `--probe <seconds> <out.ppm>` with LUCINA_DESK_{SERVER,KEY,ID,TEST_PASSWORD} set. It types "echo probe" and Enter and moves the
/// mouse, as a person would, then writes what it last decoded.
fn probe(secs: u64, out: &str) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    let var = |n: &str| std::env::var(n).map_err(|_| format!("{n} is not set"));
    let (rendezvous, relay) = urls(&var("LUCINA_DESK_SERVER")?)?;
    let target = Target { rendezvous, relay, key: var("LUCINA_DESK_KEY")?, id: var("LUCINA_DESK_ID")?, password: var("LUCINA_DESK_TEST_PASSWORD")? };
    let t0 = Instant::now();
    let live = link::connect(&target, "Juno probe")?;
    println!("logged in after {} ms: {} on {}", t0.elapsed().as_millis(), live.hostname, live.platform);
    let host = keymap::Host::from_platform(&live.platform);
    std::thread::sleep(Duration::from_secs(2));
    live.send(|s| s.mouse(0, 300, 200));
    for code in [18u16, 46, 35, 24, 57, 25, 19, 24, 48, 18, 28] {
        // e c h o space p r o b e Enter, as evdev codes on the keyboard's positions.
        let Some(c) = keymap::host_code(host, code) else { continue };
        live.send(|s| s.key_map(c, true));
        live.send(|s| s.key_map(c, false));
        std::thread::sleep(Duration::from_millis(40));
    }
    let run = Instant::now();
    let f0 = live.counters.frames.load(Ordering::Relaxed);
    while run.elapsed() < Duration::from_secs(secs) {
        if let Some(why) = live.ended.lock().map_err(|_| "poisoned")?.clone() {
            return Err(why);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let frames = live.counters.frames.load(Ordering::Relaxed) - f0;
    println!("{:.1} fps decoded, {} bytes in all", frames as f64 / secs as f64, live.counters.bytes.load(Ordering::Relaxed));
    let c = live.canvas.lock().map_err(|_| "poisoned")?;
    let mut ppm = format!("P6\n{} {}\n255\n", c.w, c.h).into_bytes();
    for p in &c.px {
        ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(out, ppm).map_err(|e| e.to_string())?;
    println!("wrote {out}, {}x{}", c.w, c.h);
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--capture-check") {
        let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
        let dir = args.get(3).map(String::as_str).unwrap_or("/tmp");
        if let Err(e) = host::check::run(secs, dir) {
            eprintln!("lucina-app-desk: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).is_some_and(|a| a == "--probe") {
        let secs = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
        let out = args.get(3).map(String::as_str).unwrap_or("/tmp/desk-probe.ppm");
        if let Err(e) = probe(secs, out) {
            eprintln!("lucina-app-desk: {e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(mode) = args.get(1).filter(|a| a.starts_with("--host")) {
        if let Err(e) = host_command(mode, &args[2..]) {
            eprintln!("lucina-app-desk: {e}");
            std::process::exit(1);
        }
        return;
    }
    lucina_app::run("lucina-app-desk", |tx| Desk::new(Some(tx), PathBuf::from(CONFIG)));
}

/// `--host-setup <id> <server host:ws port> <udp host:port> <client's relay host:port> <server key>`, the password on stdin.
/// `--host [--test-pattern]` serves until SIGTERM. `--host --web [--only <ip>] [--port <n>] [--read-only] [--test-pattern]` serves a
/// browser instead. `--test-pattern` shows a moving picture instead of the screen, on a machine with none.
fn host_command(mode: &str, rest: &[String]) -> Result<(), String> {
    let path = std::env::var("LUCINA_DESK_HOST_CONFIG").unwrap_or_else(|_| host::config::PATH.to_owned());
    let path = Path::new(&path);
    match mode {
        "--host-setup" => {
            let [id, server, udp, client_relay, key] = rest else {
                return Err("--host-setup <id> <server> <udp> <client relay> <key>, with the password on stdin".into());
            };
            let mut password = String::new();
            std::io::stdin().read_line(&mut password).map_err(|e| e.to_string())?;
            let password = password.trim_end_matches(['\r', '\n']);
            host::config::setup(path, id, server, udp, client_relay, key, password)?;
            println!("stored in {}", path.display());
            Ok(())
        }
        "--host" => {
            let test_pattern = rest.iter().any(|a| a == "--test-pattern");
            let pattern = || host::capture::TestPattern { w: 1920, h: 1080, frame: 0, moving: true };
            if rest.iter().any(|a| a == "--web") {
                let (only, port) = direct_options(rest, host::web::PORT)?;
                let (screen, input): (Box<dyn lucina_desk_web::Screen>, Box<dyn lucina_desk_web::Input>) = if test_pattern {
                    (Box::new(pattern()), Box::new(host::inject::Injector::new(Box::new(host::inject::LogSink::default()))))
                } else {
                    (Box::new(host::capture::FbSource::open("/dev/fb0")?), Box::new(host::inject::Injector::open()?))
                };
                let read_only = rest.iter().any(|a| a == "--read-only");
                let file = host::web::reach_file();
                return host::web::run(screen, input, only, port, read_only, Path::new(&file));
            }
            let source: Box<dyn host::capture::Source> =
                if test_pattern { Box::new(pattern()) } else { Box::new(host::capture::FbSource::open("/dev/fb0")?) };
            if rest.iter().any(|a| a == "--direct") {
                let (only, port) = direct_options(rest, host::direct::PORT)?;
                let reach = std::env::var("LUCINA_DESK_REACH_FILE").unwrap_or_else(|_| host::direct::REACH_FILE.to_owned());
                return host::direct::run(source, only, port, Path::new(&reach));
            }
            host::run(host::config::HostConfig::load(path)?, source)
        }
        other => Err(format!("unknown mode {other}")),
    }
}

/// `--only <ip>`, one machine instead of the whole network; `--port <n>`.
fn direct_options(rest: &[String], default_port: u16) -> Result<(Option<std::net::IpAddr>, u16), String> {
    let (mut only, mut port) = (None, default_port);
    let mut args = rest.iter();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--only" => {
                let v = args.next().ok_or("--only <ip>")?;
                only = Some(v.parse().map_err(|_| format!("{v} is not an address"))?);
            }
            "--port" => {
                let v = args.next().ok_or("--port <n>")?;
                port = v.parse().map_err(|_| format!("{v} is not a port"))?;
            }
            _ => {}
        }
    }
    Ok((only, port))
}

/// What the app shows while the browser host runs.
fn web_text(reach: Option<&host::web::Reach>) -> String {
    match reach {
        Some(r) => {
            let bare: Vec<&str> = r.urls.iter().map(|u| u.split("/?").next().unwrap_or(u)).collect();
            format!(
                "Reachable from this network. In a browser, open {} with the password {}, or add /?token= and the password to the address.",
                bare.join(" or "),
                r.password
            )
        }
        None => "Starting, or it was started some other way. Open this page again to see the address.".into(),
    }
}

/// What the app shows while the host runs.
fn reach_text(reach: Option<&host::direct::Reach>) -> String {
    match reach {
        Some(r) => format!("Reachable from this network. In a RustDesk client, connect to {} with the password {}", r.addresses.join(" or "), r.password),
        None => "Starting, or it was started some other way. Open this page again to see how to connect.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_app::Harness;

    #[test]
    fn saved_keeps_three_fields_and_never_the_password() {
        let s = Saved { server: "h:1".into(), key: "k=".into(), id: "a".into() };
        assert_eq!(Saved::parse(&s.text()), s);
        assert!(!s.text().contains("password"));
    }

    #[test]
    fn addresses() {
        assert_eq!(urls("192.168.1.55").unwrap(), ("ws://192.168.1.55:21128".into(), "ws://192.168.1.55:21129".into()));
        assert_eq!(urls("ws://a:100/").unwrap(), ("ws://a:100".into(), "ws://a:101".into()));
        assert!(urls("").is_err());
        assert!(urls("a:b").is_err());
        assert!(urls("a:65535").is_err());
    }

    fn form() -> Harness<Desk> {
        Harness::new(Desk::new(None, PathBuf::from("/nonexistent/desk/client")), 1920, 1080)
    }

    #[test]
    fn the_host_needs_a_password_before_anything_starts() {
        // Off when the app opens, as nothing started it, and it asks for nothing but the button.
        let mut h = form();
        assert!(!h.app().host_on);
        h.core.paint();
    }

    #[test]
    fn the_app_tells_how_to_connect_and_only_when_it_knows() {
        let r = host::direct::Reach { password: "abcd-efgh".into(), addresses: vec!["192.168.1.47".into(), "192.168.1.63".into()] };
        let t = reach_text(Some(&r));
        assert!(t.contains("192.168.1.47 or 192.168.1.63"), "{t}");
        assert!(t.contains("abcd-efgh"), "{t}");
        assert!(!reach_text(None).contains("password"));
    }

    #[test]
    fn the_app_shows_the_browser_links_only_when_it_knows_them() {
        let r = host::web::Reach { password: "ab".repeat(16), urls: vec![format!("http://192.168.1.47:6901/?token={}", "ab".repeat(16))] };
        let t = web_text(Some(&r));
        assert!(t.contains("open http://192.168.1.47:6901 with the password abab"), "{t}");
        assert!(!web_text(None).contains("token"));
        // Off when the app opens: nothing started it.
        let mut h = form();
        assert!(!h.app().web_on);
        h.core.paint();
    }

    #[test]
    fn direct_options_narrow_it_and_refuse_nonsense() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(direct_options(&a(&["--direct"]), host::direct::PORT).unwrap(), (None, host::direct::PORT));
        assert_eq!(direct_options(&a(&["--web"]), host::web::PORT).unwrap(), (None, 6901));
        let (only, port) = direct_options(&a(&["--direct", "--only", "192.168.1.55", "--port", "9000"]), host::direct::PORT).unwrap();
        assert_eq!(only, Some("192.168.1.55".parse().unwrap()));
        assert_eq!(port, 9000);
        assert!(direct_options(&a(&["--only", "buildhost"]), 1).is_err());
        assert!(direct_options(&a(&["--only"]), 1).is_err());
        assert!(direct_options(&a(&["--port", "99999"]), 1).is_err());
    }

    #[test]
    fn missing_fields_say_so_and_nothing_starts() {
        let mut h = form();
        h.app().connect();
        assert_eq!(h.app().status.as_deref(), Some("Type the address of the RustDesk server."));
        h.app().server.set("a");
        h.app().connect();
        assert!(h.app().status.as_deref().unwrap().contains("are all needed"));
        assert!(!h.app().connecting);
    }

    #[test]
    fn the_form_says_when_it_failed_and_can_try_again() {
        let mut h = form();
        h.app().connecting = true;
        h.core.message(Msg::Failed("login refused: Wrong Password".into()));
        h.core.paint();
        assert!(!h.app().connecting);
        assert_eq!(h.app().status.as_deref(), Some("login refused: Wrong Password"));
    }

    #[test]
    fn lucina_reads_no_password_and_may_open_this_laptop() {
        use lucina_ax::accesskit::Role;
        let mut h = form();
        let s = h.tree();
        let field = s.find(Role::PasswordInput, "Password");
        let (x, y) = s.node(field.expect("the password field is a password input")).and_then(|n| n.bounds()).map(|b| (((b.x0 + b.x1) / 2.0) as i32, ((b.y0 + b.y1) / 2.0) as i32)).unwrap();
        h.click(x, y);
        h.type_str("hunter2-canary");
        assert_eq!(h.app().password.text, "hunter2-canary");
        let json = serde_json::to_string(&h.tree().reply(1)).unwrap();
        assert!(!json.contains("hunter2"), "{json}");
        assert!(!h.app().host_on);
        // Pairing may take her input (owner, 2026-11-04): both buttons are there for her, not refused.
        let s = h.tree();
        for name in ["Let another device reach this laptop", "Let a browser see this laptop"] {
            let id = s.find(Role::Button, name).expect(name);
            assert!(s.plan(&lucina_ax::tree::id_text(id), "click", None, &s.version).is_ok(), "{name}");
        }
    }

    #[test]
    fn a_finished_session_clears_the_password() {
        let mut h = form();
        h.app().password.set("secret");
        *h.app().ended.lock().unwrap() = Some("the host closed the connection".into());
        h.core.message(Msg::Failed(String::new()));
        h.core.paint();
        assert_eq!(h.app().password.text, "");
        assert!(h.app().status.as_deref().unwrap().contains("the host closed the connection"));
    }
}
