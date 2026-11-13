//! Juno's flight recorder, the shared part: the wire format between the program on the laptop and the collector on buildhost, the
//! scrubber that keeps secrets out of what is sent, the settings words, and the sequence rules.
//!
//! The collector (`lucina-flight-collector`) runs `ssh <laptop> lucina-flight` and reads the program's standard output;
//! `lucina-flight` follows `/dev/kmsg` and writes every record the moment it is produced. Nothing listens on either machine.
//! The stream is text, one frame per line:
//!
//! ```text
//! laptop    HELLO 1 boot=<32 hex> mode=on|trace|off uptime_ms=<n> wall_ms=<n> build=<free text>
//! collector RESUME <seq>                       the next kernel record it wants (0 = from the oldest), on the program's stdin
//! laptop    K <seq> <level> <usec> <text>      one kernel record, scrubbed
//!           G <from> <to>                      records from..to (to excluded) were lost before they could be sent
//!           H <uptime_ms>                      heartbeat; only with mode=trace
//!           Z <reason>                         a clean end (the program was asked to stop)
//! ```
//! Kernel text never holds a newline (the log escapes it), so a frame is one line. The design is in docs/design/juno-logging.md.
#![forbid(unsafe_code)]

pub mod rules;
pub mod scrub;

use rules::{skip_step, take_step};

/// The protocol version in the first line.
pub const VERSION: u32 = 1;

/// The settings word of `lucina.flight=` (kernel command line) and of `/data/lucina/flight`, `/etc/lucina/flight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Not running.
    Off,
    /// Kernel lines as they come; no timer of its own.
    On,
    /// As `On`, and a heartbeat every 0.5 s, which wakes the machine twice a second: for the reset hunt only.
    Trace,
}

impl Mode {
    pub fn parse(word: &str) -> Option<Mode> {
        match word.trim() {
            "off" | "0" => Some(Mode::Off),
            "on" | "1" => Some(Mode::On),
            "trace" => Some(Mode::Trace),
            _ => None,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::On => "on",
            Mode::Trace => "trace",
        }
    }
}

/// The value of `key=` in a kernel command line (the last one wins, as on Linux).
pub fn cmdline_value<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    let mut found = None;
    for word in cmdline.split_whitespace() {
        if let Some(rest) = word.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            found = Some(rest);
        }
    }
    found
}

/// One kernel record, as `/dev/kmsg` renders it: `<level>,<seq>,<usec>,<flags>[,...];text`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kmsg<'a> {
    pub level: u8,
    pub seq: u64,
    pub usec: u64,
    pub text: &'a str,
}

pub fn parse_kmsg(line: &str) -> Option<Kmsg<'_>> {
    let (head, text) = line.split_once(';')?;
    let mut f = head.split(',');
    let prio: u32 = f.next()?.parse().ok()?;
    let seq: u64 = f.next()?.parse().ok()?;
    let usec: u64 = f.next()?.parse().ok()?;
    Some(Kmsg {
        level: (prio & 7) as u8,
        seq,
        usec,
        text: text.trim_end_matches('\n'),
    })
}

/// What the laptop says first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub boot: String,
    pub mode: Mode,
    pub uptime_ms: u64,
    pub wall_ms: u64,
    pub build: String,
}

/// One line on the wire after the greeting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Kernel {
        seq: u64,
        level: u8,
        usec: u64,
        text: String,
    },
    Gap {
        from: u64,
        to: u64,
    },
    Heartbeat {
        uptime_ms: u64,
    },
    Clean {
        reason: String,
    },
}

pub fn encode_hello(h: &Hello) -> String {
    format!(
        "HELLO {VERSION} boot={} mode={} uptime_ms={} wall_ms={} build={}\n",
        h.boot,
        h.mode.word(),
        h.uptime_ms,
        h.wall_ms,
        h.build.replace(['\n', '\r'], " ")
    )
}

pub fn decode_hello(line: &str) -> Option<Hello> {
    let rest = line.trim_end().strip_prefix("HELLO ")?;
    let (version, rest) = rest.split_once(' ')?;
    if version.parse::<u32>().ok()? != VERSION {
        return None;
    }
    let (boot, rest) = rest.strip_prefix("boot=")?.split_once(' ')?;
    let (mode, rest) = rest.strip_prefix("mode=")?.split_once(' ')?;
    let (uptime, rest) = rest.strip_prefix("uptime_ms=")?.split_once(' ')?;
    let (wall, rest) = rest.strip_prefix("wall_ms=")?.split_once(' ')?;
    let build = rest.strip_prefix("build=")?;
    if boot.is_empty() || !boot.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(Hello {
        boot: boot.to_string(),
        mode: Mode::parse(mode)?,
        uptime_ms: uptime.parse().ok()?,
        wall_ms: wall.parse().ok()?,
        build: build.to_string(),
    })
}

pub fn encode(frame: &Frame) -> String {
    match frame {
        Frame::Kernel {
            seq,
            level,
            usec,
            text,
        } => format!(
            "K {seq} {level} {usec} {}\n",
            text.replace(['\n', '\r'], " ")
        ),
        Frame::Gap { from, to } => format!("G {from} {to}\n"),
        Frame::Heartbeat { uptime_ms } => format!("H {uptime_ms}\n"),
        Frame::Clean { reason } => format!("Z {}\n", reason.replace(['\n', '\r'], " ")),
    }
}

pub fn decode(line: &str) -> Option<Frame> {
    let line = line.trim_end_matches(['\n', '\r']);
    let (tag, rest) = line.split_once(' ')?;
    match tag {
        "K" => {
            let mut p = rest.splitn(4, ' ');
            Some(Frame::Kernel {
                seq: p.next()?.parse().ok()?,
                level: p.next()?.parse().ok()?,
                usec: p.next()?.parse().ok()?,
                text: p.next().unwrap_or("").to_string(),
            })
        }
        "G" => {
            let (a, b) = rest.split_once(' ')?;
            Some(Frame::Gap {
                from: a.parse().ok()?,
                to: b.parse().ok()?,
            })
        }
        "H" => Some(Frame::Heartbeat {
            uptime_ms: rest.parse().ok()?,
        }),
        "Z" => Some(Frame::Clean {
            reason: rest.to_string(),
        }),
        _ => None,
    }
}

/// Follows a numbered log and says what each next record means. Both ends use it: the daemon to report records it never
/// got from the ring, the collector to see holes the daemon did not know about.
#[derive(Clone, Debug)]
pub struct Cursor {
    expected: u64,
}

pub use rules::Taken;

impl Cursor {
    pub fn new(expected: u64) -> Self {
        Cursor { expected }
    }

    pub fn expected(&self) -> u64 {
        self.expected
    }

    pub fn take(&mut self, seq: u64) -> Taken {
        let (expected, taken) = take_step(self.expected, seq);
        self.expected = expected;
        taken
    }

    /// Records `from..to` are known lost (the other end said so): the next one due is `to`, if that is further on.
    pub fn skip_to(&mut self, to: u64) {
        self.expected = skip_step(self.expected, to);
    }
}

/// Whether a kernel line is the kernel saying the machine is being turned off or restarted on purpose: the collector counts the
/// end of a stream after one of these as a clean end.
pub fn is_shutdown_line(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("power off requested")
        || t.contains("restart requested")
        || t.contains("reboot requested")
        || t.contains("system halted")
        || t.contains("poweroff:")
        || t.contains("reboot: restarting")
        || t.contains("reboot: power down")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::after;

    #[test]
    fn kmsg_lines_parse() {
        let k = parse_kmsg("6,41,12345,-;lucina: hello world\n").unwrap();
        assert_eq!(
            (k.level, k.seq, k.usec, k.text),
            (6, 41, 12345, "lucina: hello world")
        );
        let k = parse_kmsg("30,7,1,-,caller=T1;x;y").unwrap();
        assert_eq!((k.level, k.text), (6, "x;y"));
        assert!(parse_kmsg("no record here").is_none());
    }

    #[test]
    fn frames_round_trip() {
        let frames = [
            Frame::Kernel {
                seq: 9,
                level: 3,
                usec: 77,
                text: "a b  c".into(),
            },
            Frame::Kernel {
                seq: 0,
                level: 0,
                usec: 0,
                text: String::new(),
            },
            Frame::Gap { from: 4, to: 99 },
            Frame::Heartbeat { uptime_ms: 12 },
            Frame::Clean {
                reason: "SIGTERM".into(),
            },
        ];
        for f in frames {
            assert_eq!(decode(&encode(&f)).unwrap(), f);
        }
        assert!(decode("X 1").is_none());
        assert!(decode("K 1 2").is_none());
    }

    #[test]
    fn hello_round_trips_and_refuses_a_bad_boot() {
        let h = Hello {
            boot: "00ff".into(),
            mode: Mode::Trace,
            uptime_ms: 5,
            wall_ms: 6,
            build: "Linux version x y z".into(),
        };
        assert_eq!(decode_hello(&encode_hello(&h)).unwrap(), h);
        assert!(decode_hello("HELLO 1 boot=zz mode=on uptime_ms=1 wall_ms=1 build=x").is_none());
        assert!(decode_hello("HELLO 2 boot=aa mode=on uptime_ms=1 wall_ms=1 build=x").is_none());
    }

    #[test]
    fn cursor_reports_gaps_and_drops_duplicates() {
        let mut c = Cursor::new(0);
        assert_eq!(c.take(0), Taken::Keep { lost: None });
        assert_eq!(c.take(1), Taken::Keep { lost: None });
        assert_eq!(c.take(5), Taken::Keep { lost: Some((2, 5)) });
        assert_eq!(c.take(5), Taken::Duplicate);
        assert_eq!(c.take(3), Taken::Duplicate);
        assert_eq!(c.expected(), 6);
        c.skip_to(10);
        assert_eq!(c.expected(), 10);
        c.skip_to(2);
        assert_eq!(c.expected(), 10);
    }

    #[test]
    fn cursor_at_the_top_of_the_range() {
        let mut c = Cursor::new(u64::MAX);
        assert_eq!(c.take(u64::MAX), Taken::Keep { lost: None });
        assert_eq!(c.expected(), u64::MAX);
        assert_eq!(c.take(u64::MAX), Taken::Keep { lost: None });
        let mut c = Cursor::new(3);
        assert_eq!(
            c.take(u64::MAX),
            Taken::Keep {
                lost: Some((3, u64::MAX))
            }
        );
        assert_eq!(c.expected(), u64::MAX);
        assert_eq!(c.take(7), Taken::Duplicate);
        assert_eq!(c.expected(), u64::MAX);
    }

    #[test]
    fn take_step_cases() {
        assert_eq!(take_step(4, 4), (5, Taken::Keep { lost: None }));
        assert_eq!(take_step(4, 9), (10, Taken::Keep { lost: Some((4, 9)) }));
        assert_eq!(take_step(4, 3), (4, Taken::Duplicate));
        assert_eq!(take_step(0, 0), (1, Taken::Keep { lost: None }));
        assert_eq!(
            take_step(u64::MAX, u64::MAX),
            (u64::MAX, Taken::Keep { lost: None })
        );
        assert_eq!(
            take_step(0, u64::MAX),
            (
                u64::MAX,
                Taken::Keep {
                    lost: Some((0, u64::MAX))
                }
            )
        );
        assert_eq!(take_step(u64::MAX, 0), (u64::MAX, Taken::Duplicate));
    }

    #[test]
    fn skip_step_never_goes_back() {
        assert_eq!(skip_step(5, 10), 10);
        assert_eq!(skip_step(5, 5), 5);
        assert_eq!(skip_step(5, 2), 5);
        assert_eq!(skip_step(0, u64::MAX), u64::MAX);
        assert_eq!(skip_step(u64::MAX, 0), u64::MAX);
    }

    #[test]
    fn after_does_not_wrap() {
        assert_eq!(after(u64::MAX), u64::MAX);
        assert_eq!(after(4), 5);
    }

    #[test]
    fn mode_words() {
        assert_eq!(Mode::parse("trace"), Some(Mode::Trace));
        assert_eq!(Mode::parse(" off\n"), Some(Mode::Off));
        assert_eq!(Mode::parse("loud"), None);
        assert_eq!(
            cmdline_value(
                "a=1 lucina.flight=on b lucina.flight=trace",
                "lucina.flight"
            ),
            Some("trace")
        );
        assert_eq!(cmdline_value("lucina.flightx=1", "lucina.flight"), None);
    }

    #[test]
    fn shutdown_lines() {
        assert!(is_shutdown_line(
            "reboot: power off requested by /sbin/poweroff (pid 80)"
        ));
        assert!(!is_shutdown_line("usb 1-1: new device"));
    }
}
