//! Ceres serial protocol v0 as pure code, shared by the firmware (`ceres/firmware`) and tested on the host.
//!
//! Ceres is LucinaOS for small embedded boards. v0 has one role, `dsi-power`: an ESP32 holds and releases the
//! Nintendo DSi's power button through a PC817 optocoupler on GPIO25 (high = button held).
//!
//! | piece | |
//! |---|---|
//! | [`LineReader`] | bytes from UART0 into lines: `\n` ends one, a `\r` before it is dropped, at most [`MAX_LINE`] bytes |
//! | [`parse`] | a line into a [`Command`], with the limits and defaults, or an [`Error`] |
//! | [`Reply`] | every reply line (`ok ...` / `err <reason>`), formatted through [`core::fmt::Display`] without the `\n` |
//! | [`Plan`] | what `press` and `cycle` do to the pin: (level, duration) steps, the pin low after the last one |
//!
//! The full protocol is in `ceres/README.md`; a host tool (ceres-ctl) depends on it, so the strings here are exact.

#![no_std]

use core::fmt;

/// Firmware version as the protocol shows it (`v0.1`).
pub const VERSION: &str = "0.1";
pub const BOARD: &str = "esp32";
pub const ROLE: &str = "dsi-power";
/// The GPIO driving the optocoupler's LED.
pub const PIN: u8 = 25;
/// Longest input line, without its `\r\n`.
pub const MAX_LINE: usize = 80;

/// `press <ms>`.
pub const PRESS_MS: Limit = Limit {
    min: 1,
    max: 15_000,
};
/// `cycle` first press, long enough to force the DSi off.
pub const CYCLE_OFF_MS: Limit = Limit {
    min: 3_000,
    max: 15_000,
};
/// `cycle` wait between the two presses.
pub const CYCLE_GAP_MS: Limit = Limit {
    min: 500,
    max: 10_000,
};
/// `cycle` second press, which switches the DSi on.
pub const CYCLE_ON_MS: Limit = Limit {
    min: 100,
    max: 2_000,
};
/// `cycle` with no numbers: off 6000, gap 2000, on 300.
pub const CYCLE_DEFAULT: Command = Command::Cycle {
    off_ms: 6_000,
    gap_ms: 2_000,
    on_ms: 300,
};

/// An inclusive range of milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub min: u32,
    pub max: u32,
}

impl Limit {
    pub const fn contains(self, ms: u32) -> bool {
        ms >= self.min && ms <= self.max
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Ver,
    Status,
    Press {
        ms: u32,
    },
    Cycle {
        off_ms: u32,
        gap_ms: u32,
        on_ms: u32,
    },
    Release,
    Reboot,
}

/// Why a command failed: the `<reason>` of `err <reason>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    UnknownCommand,
    BadArgument,
    /// A `press` or `cycle` is already running.
    Busy,
    /// The running `press` or `cycle` was ended by `release`.
    Released,
    /// The line had more than [`MAX_LINE`] bytes; it is dropped whole.
    LineTooLong,
}

impl Error {
    pub const fn reason(self) -> &'static str {
        match self {
            Error::UnknownCommand => "unknown command",
            Error::BadArgument => "bad argument",
            Error::Busy => "busy",
            Error::Released => "released",
            Error::LineTooLong => "line too long",
        }
    }
}

/// Parses one line (without its `\n`; a trailing `\r` is dropped too). Words are separated by spaces or tabs and
/// matched without regard to case; numbers are plain decimal. `Ok(None)` is a blank line, which gets no reply.
pub fn parse(line: &[u8]) -> Result<Option<Command>, Error> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut words = line
        .split(|&b| b == b' ' || b == b'\t')
        .filter(|w| !w.is_empty());
    let Some(word) = words.next() else {
        return Ok(None);
    };
    let mut args = [0u32; 3];
    let mut n = 0;
    for w in words {
        if n == args.len() {
            return Err(Error::BadArgument);
        }
        args[n] = number(w).ok_or(Error::BadArgument)?;
        n += 1;
    }
    let args = &args[..n];
    let is = |name: &str| word.eq_ignore_ascii_case(name.as_bytes());
    let command = if is("help") {
        Command::Help
    } else if is("ver") {
        Command::Ver
    } else if is("status") {
        Command::Status
    } else if is("press") {
        match *args {
            [ms] if PRESS_MS.contains(ms) => return Ok(Some(Command::Press { ms })),
            _ => return Err(Error::BadArgument),
        }
    } else if is("cycle") {
        return match *args {
            [] => Ok(Some(CYCLE_DEFAULT)),
            [off_ms, gap_ms, on_ms]
                if CYCLE_OFF_MS.contains(off_ms)
                    && CYCLE_GAP_MS.contains(gap_ms)
                    && CYCLE_ON_MS.contains(on_ms) =>
            {
                Ok(Some(Command::Cycle {
                    off_ms,
                    gap_ms,
                    on_ms,
                }))
            }
            _ => Err(Error::BadArgument),
        };
    } else if is("release") {
        Command::Release
    } else if is("reboot") {
        Command::Reboot
    } else {
        return Err(Error::UnknownCommand);
    };
    // The commands without numbers take none.
    if args.is_empty() {
        Ok(Some(command))
    } else {
        Err(Error::BadArgument)
    }
}

/// A plain decimal number that fits in a `u32` (leading zeros allowed, no sign).
fn number(word: &[u8]) -> Option<u32> {
    if word.is_empty() {
        return None;
    }
    word.iter().try_fold(0u32, |n, &b| {
        if b.is_ascii_digit() {
            n.checked_mul(10)?.checked_add(u32::from(b - b'0'))
        } else {
            None
        }
    })
}

/// A finished input line, as [`LineReader::push`] hands it out.
#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// The line without its `\r\n` (or `\n`).
    Text(&'a [u8]),
    /// More than [`MAX_LINE`] bytes came before the `\n`; reply [`Error::LineTooLong`].
    TooLong,
}

/// Collects UART bytes into lines. Only `\n` ends a line; the board echoes nothing.
pub struct LineReader {
    // One more than MAX_LINE, for the `\r` before the `\n`.
    buf: [u8; MAX_LINE + 1],
    len: usize,
    overflow: bool,
}

impl Default for LineReader {
    fn default() -> Self {
        Self::new()
    }
}

impl LineReader {
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_LINE + 1],
            len: 0,
            overflow: false,
        }
    }

    /// Takes one byte; returns the line when `b` is `\n`.
    pub fn push(&mut self, b: u8) -> Option<Line<'_>> {
        if b != b'\n' {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            } else {
                self.overflow = true;
            }
            return None;
        }
        let mut n = self.len;
        let overflow = self.overflow;
        self.len = 0;
        self.overflow = false;
        if n > 0 && self.buf[n - 1] == b'\r' {
            n -= 1;
        }
        if overflow || n > MAX_LINE {
            Some(Line::TooLong)
        } else {
            Some(Line::Text(&self.buf[..n]))
        }
    }
}

/// What the last `press`, `cycle` or `release` was.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Last {
    #[default]
    None,
    Press,
    Cycle,
    Release,
}

impl Last {
    pub const fn as_str(self) -> &'static str {
        match self {
            Last::None => "none",
            Last::Press => "press",
            Last::Cycle => "cycle",
            Last::Release => "release",
        }
    }
}

/// The fields of `status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub up_ms: u64,
    /// The pin is high (the button is held).
    pub pressed: bool,
    /// Times the pin went high since boot: one per `press`, two per `cycle`.
    pub presses: u32,
    pub last: Last,
}

/// One reply line. `Display` writes it without the line end; the firmware adds `\n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    /// The boot line, before any command.
    Ready,
    Help,
    Ver,
    Status(Status),
    Pressed {
        ms: u32,
    },
    Cycled {
        off_ms: u32,
        gap_ms: u32,
        on_ms: u32,
    },
    Released,
    Rebooting,
    Err(Error),
}

impl fmt::Display for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = format_args!("v{VERSION} board={BOARD} role={ROLE} pin={PIN}");
        match *self {
            Reply::Ready => write!(f, "ceres ready {id}"),
            Reply::Help => f.write_str("ok commands: help ver status press <ms> cycle [off_ms gap_ms on_ms] release reboot"),
            Reply::Ver => write!(f, "ok ceres {id}"),
            Reply::Status(s) => write!(
                f,
                "ok up_ms={} button={} presses={} last={}",
                s.up_ms,
                if s.pressed { "pressed" } else { "released" },
                s.presses,
                s.last.as_str()
            ),
            Reply::Pressed { ms } => write!(f, "ok pressed {ms}"),
            Reply::Cycled { off_ms, gap_ms, on_ms } => write!(f, "ok cycled off={off_ms} gap={gap_ms} on={on_ms}"),
            Reply::Released => f.write_str("ok released"),
            Reply::Rebooting => f.write_str("ok rebooting"),
            Reply::Err(e) => write!(f, "err {}", e.reason()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Low,
    High,
}

/// Hold the pin at `level` for `ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub level: Level,
    pub ms: u32,
}

/// What `press` or `cycle` does to the pin. After the last step the pin goes low, then the command replies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    steps: [Step; 3],
    len: usize,
    /// The reply once every step has run.
    pub done: Reply,
    pub last: Last,
}

impl Plan {
    /// The plan of a `press` or `cycle`; `None` for the other commands.
    pub fn of(command: Command) -> Option<Plan> {
        let high = |ms| Step {
            level: Level::High,
            ms,
        };
        let unused = Step {
            level: Level::Low,
            ms: 0,
        };
        match command {
            Command::Press { ms } => Some(Plan {
                steps: [high(ms), unused, unused],
                len: 1,
                done: Reply::Pressed { ms },
                last: Last::Press,
            }),
            Command::Cycle {
                off_ms,
                gap_ms,
                on_ms,
            } => Some(Plan {
                steps: [
                    high(off_ms),
                    Step {
                        level: Level::Low,
                        ms: gap_ms,
                    },
                    high(on_ms),
                ],
                len: 3,
                done: Reply::Cycled {
                    off_ms,
                    gap_ms,
                    on_ms,
                },
                last: Last::Cycle,
            }),
            _ => None,
        }
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps[..self.len]
    }

    pub fn total_ms(&self) -> u32 {
        self.steps().iter().map(|s| s.ms).sum()
    }
}
