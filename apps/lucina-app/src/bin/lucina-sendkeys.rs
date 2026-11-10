//! lucina-sendkeys: press keys, and move the pointer, from a command line, for tests on a machine
//! nobody is sitting at. It writes input events to an evdev node (patch 0406, as Linux allows), and
//! every program reading that node gets them exactly as if the device had sent them: the real path
//! from the kernel's input core to the toolkit's event loop.
//!
//! ```text
//! lucina-sendkeys [--dev /dev/input/eventN] [--gap ms] <action>...
//!   enter, esc, tab, up, down, left, right, f1 .. f3, pgup, pgdn, home, end, backspace, space
//!   a key by evdev code: key:30
//!   text:hello        types on the US layout
//!   name*3            repeats
//!   shift+tab, ctrl+a  with a modifier held
//!   rel:DX,DY         moves the pointer
//!   click             the left button down and up
//!   wheel:N           wheel notches, positive up
//!   sleep:MS          waits
//! ```

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::fs::OpenOptions;
use std::io::Write;
use std::time::Duration;

use lucina_app::input::{EV_KEY, EV_REL, EV_SYN, REL_WHEEL, REL_X, REL_Y};
use lucina_app::keys;

fn usage() -> ! {
    eprintln!("usage: lucina-sendkeys [--dev /dev/input/eventN] [--gap ms] <action>...");
    std::process::exit(2);
}

fn named(s: &str) -> Option<u16> {
    Some(match s {
        "enter" => keys::KEY_ENTER,
        "esc" => keys::KEY_ESC,
        "tab" => keys::KEY_TAB,
        "up" => keys::KEY_UP,
        "down" => keys::KEY_DOWN,
        "left" => keys::KEY_LEFT,
        "right" => keys::KEY_RIGHT,
        "f1" => keys::KEY_F1,
        "f2" => keys::KEY_F2,
        "f3" => keys::KEY_F3,
        "pgup" => keys::KEY_PAGEUP,
        "pgdn" => keys::KEY_PAGEDOWN,
        "home" => keys::KEY_HOME,
        "end" => keys::KEY_END,
        "backspace" => keys::KEY_BACKSPACE,
        "delete" => keys::KEY_DELETE,
        "space" => keys::KEY_SPACE,
        "shift" => keys::KEY_LEFTSHIFT,
        "ctrl" => keys::KEY_LEFTCTRL,
        "alt" => keys::KEY_LEFTALT,
        _ => {
            if let Some(n) = s.strip_prefix("key:") {
                return n.parse().ok();
            }
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            return keys::code_for(c).map(|(code, _)| code);
        }
    })
}

struct Out {
    file: std::fs::File,
    gap: Duration,
}

impl Out {
    fn event(&mut self, type_: u16, code: u16, value: i32) {
        let ev = libc::input_event {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };
        // SAFETY: input_event is plain integers, and the slice covers exactly it.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&ev as *const libc::input_event).cast::<u8>(),
                std::mem::size_of::<libc::input_event>(),
            )
        };
        if let Err(e) = self.file.write_all(bytes) {
            eprintln!("lucina-sendkeys: {e}");
            std::process::exit(1);
        }
    }

    fn sync(&mut self) {
        self.event(EV_SYN, 0, 0);
    }

    fn down(&mut self, code: u16, value: i32) {
        self.event(EV_KEY, code, value);
        self.sync();
    }

    fn tap(&mut self, code: u16) {
        self.down(code, 1);
        self.down(code, 0);
        std::thread::sleep(self.gap);
    }
}

fn action(out: &mut Out, a: &str) {
    if let Some((name, n)) = a.rsplit_once('*')
        && let Ok(n) = n.parse::<u32>()
    {
        for _ in 0..n {
            action(out, name);
        }
    } else if let Some(t) = a.strip_prefix("text:") {
        for c in t.chars() {
            let Some((code, shift)) = keys::code_for(c) else {
                eprintln!("lucina-sendkeys: cannot type {c:?}");
                std::process::exit(2);
            };
            if shift {
                out.down(keys::KEY_LEFTSHIFT, 1);
            }
            out.tap(code);
            if shift {
                out.down(keys::KEY_LEFTSHIFT, 0);
            }
        }
    } else if let Some(ms) = a.strip_prefix("sleep:") {
        std::thread::sleep(Duration::from_millis(
            ms.parse().unwrap_or_else(|_| usage()),
        ));
    } else if let Some(xy) = a.strip_prefix("rel:") {
        let (x, y) = xy.split_once(',').unwrap_or_else(|| usage());
        out.event(EV_REL, REL_X, x.parse().unwrap_or_else(|_| usage()));
        out.event(EV_REL, REL_Y, y.parse().unwrap_or_else(|_| usage()));
        out.sync();
        std::thread::sleep(out.gap);
    } else if let Some(n) = a.strip_prefix("wheel:") {
        out.event(EV_REL, REL_WHEEL, n.parse().unwrap_or_else(|_| usage()));
        out.sync();
        std::thread::sleep(out.gap);
    } else if a == "click" {
        out.tap(keys::BTN_LEFT);
    } else if let Some((m, k)) = a.split_once('+')
        && m != "key"
    {
        let (Some(m), Some(k)) = (named(m), named(k)) else {
            usage();
        };
        out.down(m, 1);
        out.tap(k);
        out.down(m, 0);
    } else {
        match named(a) {
            Some(code) => out.tap(code),
            None => usage(),
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut dev = None;
    let mut gap = Duration::from_millis(60);
    while let Some(a) = args.peek() {
        match a.as_str() {
            "--dev" => {
                args.next();
                dev = Some(args.next().unwrap_or_else(|| usage()));
            }
            "--gap" => {
                args.next();
                let ms = args.next().unwrap_or_else(|| usage());
                gap = Duration::from_millis(ms.parse().unwrap_or_else(|_| usage()));
            }
            _ => break,
        }
    }
    // Any device will do: its readers, the shell and the app on screen, take a key from any of them.
    let dev = dev.unwrap_or_else(|| {
        let mut nodes: Vec<String> = std::fs::read_dir("/dev/input")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("event"))
            .collect();
        nodes.sort();
        match nodes.first() {
            Some(n) => format!("/dev/input/{n}"),
            None => {
                eprintln!("lucina-sendkeys: no input device");
                std::process::exit(1);
            }
        }
    });
    let file = OpenOptions::new()
        .write(true)
        .open(&dev)
        .unwrap_or_else(|e| {
            eprintln!("lucina-sendkeys: {dev}: {e}");
            std::process::exit(1);
        });
    let mut out = Out { file, gap };
    let actions: Vec<String> = args.collect();
    if actions.is_empty() {
        usage();
    }
    for a in &actions {
        action(&mut out, a);
    }
}
