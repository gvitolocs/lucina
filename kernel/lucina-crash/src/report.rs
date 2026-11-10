//! What a crash report says. The same words go to the screen, the serial port and the saved log.

use core::fmt::{self, Write};

/// What ended the system.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Kind {
    /// A kernel panic, a failed assertion or an unhandled exception in the kernel.
    Panic = 1,
    /// A CPU stopped scheduling: its watchdog thread did not run for too long.
    SoftLockup = 2,
    /// The hardware watchdog was about to reset the machine.
    WatchdogBark = 3,
    /// The program that promised to pet `/dev/watchdog` stopped doing so.
    UserWatchdog = 4,
    /// Nothing was recorded: the machine reset by itself (a hardware watchdog bite, a firmware reset, a lost power rail).
    UnexplainedReset = 5,
    /// A program asked for the reset and said why.
    RequestedReset = 6,
    /// The modem DSP failed (its watchdog fired or it raised its fatal bit) and said why; the boot went on (patch 1057).
    Modem = 7,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Panic => "panic",
            Kind::SoftLockup => "soft lockup",
            Kind::WatchdogBark => "hardware watchdog",
            Kind::UserWatchdog => "userspace watchdog",
            Kind::UnexplainedReset => "unexplained reset",
            Kind::RequestedReset => "requested reset",
            Kind::Modem => "modem",
        }
    }

    /// What the crash screen's headline calls it.
    pub fn headline_name(self) -> &'static str {
        match self {
            Kind::Panic => "Kernel panic",
            Kind::SoftLockup => "Soft lockup",
            Kind::WatchdogBark => "Hardware watchdog",
            Kind::UserWatchdog => "Userspace watchdog",
            Kind::UnexplainedReset => "Unexplained reset",
            Kind::RequestedReset => "Requested reset",
            Kind::Modem => "Modem failure",
        }
    }

    pub fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            1 => Kind::Panic,
            2 => Kind::SoftLockup,
            3 => Kind::WatchdogBark,
            4 => Kind::UserWatchdog,
            5 => Kind::UnexplainedReset,
            6 => Kind::RequestedReset,
            7 => Kind::Modem,
            _ => return None,
        })
    }
}

/// The registers of the code that crashed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Regs {
    /// `x0` to `x30`.
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    /// `ESR_EL1` and `FAR_EL1`, if an exception brought the kernel here.
    pub fault: Option<(u64, u64)>,
}

/// The facts of one crash.
#[derive(Clone, Copy, Debug)]
pub struct Facts<'a> {
    /// `LucinaJuno`, as the kernel build is named; the title takes off the `Lucina`.
    pub kernel_name: &'a str,
    pub kind: Kind,
    pub reason: &'a str,
    pub cpu: usize,
    /// The thread that ran, if one did.
    pub tid: Option<u32>,
    pub uptime_ms: u64,
    /// How many boots this machine has counted, if the saved log knows.
    pub boot: Option<u32>,
    /// What tells this kernel from others: release and build banner.
    pub build: &'a str,
    pub regs: Option<&'a Regs>,
    /// Return addresses, innermost first.
    pub backtrace: &'a [u64],
}

impl Facts<'_> {
    /// The screen's title, e.g. `Lucina Juno crashed`.
    pub fn title(&self) -> Title<'_> {
        Title(
            self.kernel_name
                .strip_prefix("Lucina")
                .filter(|name| !name.is_empty() && *name != "OS"),
        )
    }

    /// The crash screen's headline: what crashed and why, in one line, e.g. `Kernel panic: heap allocation of 2 MiB failed`.
    pub fn write_headline(&self, w: &mut dyn Write) -> fmt::Result {
        write!(w, "{}: ", self.kind.headline_name())?;
        write_short_reason(w, self.reason)
    }

    /// The crash screen's footer: the boot, the uptime, what happens next (`status`, from [`After::write_short_status`])
    /// and where the full record is. `saved`: the crash store took it.
    pub fn write_footer(&self, w: &mut dyn Write, status: &str, saved: bool) -> fmt::Result {
        if let Some(boot) = self.boot {
            write!(w, "boot {boot} | ")?;
        }
        write!(
            w,
            "uptime {}.{} s | {status} | full record: ",
            self.uptime_ms / 1000,
            self.uptime_ms % 1000 / 100
        )?;
        if saved {
            write!(w, "lucina-crash list")
        } else {
            write!(w, "the serial console")
        }
    }

    /// One line: the kind, the CPU, the thread, the time and the boot.
    pub fn write_where(&self, w: &mut dyn Write) -> fmt::Result {
        write!(w, "{} on CPU {}", self.kind.name(), self.cpu)?;
        if let Some(tid) = self.tid {
            write!(w, ", thread {tid}")?;
        }
        write!(
            w,
            ", uptime {}.{:03} s",
            self.uptime_ms / 1000,
            self.uptime_ms % 1000
        )?;
        if let Some(boot) = self.boot {
            write!(w, ", boot {boot}")?;
        }
        writeln!(w)
    }

    /// The registers, each row fitting 90 columns.
    pub fn write_regs(&self, w: &mut dyn Write) -> fmt::Result {
        let Some(regs) = self.regs else {
            return writeln!(w, "(no registers)");
        };
        writeln!(
            w,
            "pc {:016x}  lr {:016x}  sp {:016x}  pstate {:08x}",
            regs.pc, regs.x[30], regs.sp, regs.pstate
        )?;
        if let Some((esr, far)) = regs.fault {
            write!(w, "esr {esr:08x}  far {far:016x}  ")?;
            write_esr(w, esr)?;
            writeln!(w)?;
        }
        for (row, values) in regs.x[..28].chunks(4).enumerate() {
            for (i, value) in values.iter().enumerate() {
                let sep = if i == 3 { "" } else { "  " };
                write!(w, "x{:02} {value:016x}{sep}", row * 4 + i)?;
            }
            writeln!(w)?;
        }
        writeln!(w, "x28 {:016x}  x29 {:016x}", regs.x[28], regs.x[29])
    }

    /// The return addresses, as many on a row as fit `columns`.
    pub fn write_backtrace(&self, w: &mut dyn Write, columns: usize) -> fmt::Result {
        if self.backtrace.is_empty() {
            return writeln!(w, "(no backtrace)");
        }
        const CELL: usize = 17;
        let per_row = (columns / CELL).max(1);
        for row in self.backtrace.chunks(per_row) {
            for pc in row {
                write!(w, "{pc:016x} ")?;
            }
            writeln!(w)?;
        }
        Ok(())
    }

    /// The whole report without the kernel log, as the saved record and the serial port have it.
    pub fn write_report(&self, w: &mut dyn Write) -> fmt::Result {
        writeln!(w, "{}", self.title())?;
        writeln!(w, "reason: {}", self.reason)?;
        write!(w, "where: ")?;
        self.write_where(w)?;
        writeln!(w, "build: {}", self.build)?;
        writeln!(w, "registers:")?;
        self.write_regs(w)?;
        writeln!(w, "backtrace (scripts/juno-symbolize.sh names them):")?;
        self.write_backtrace(w, 17)
    }
}

/// The reason as the headline has it: its first line and first sentence, and a failed heap allocation as its size
/// (`Heap allocation error, layout = Layout { size: 0x40000, ...` is `heap allocation of 256 KiB failed`).
fn write_short_reason(w: &mut dyn Write, reason: &str) -> fmt::Result {
    if let Some(rest) = reason.strip_prefix("Heap allocation error")
        && let Some(size) = layout_size(rest)
    {
        write!(w, "heap allocation of ")?;
        write_size(w, size)?;
        return write!(w, " failed");
    }
    let line = reason.lines().next().unwrap_or("").trim();
    // The first sentence: a sentence ends at ". " (a path or a number has no space after its dot).
    let sentence = line.find(". ").map_or(line, |end| &line[..end]);
    if sentence.is_empty() {
        write!(w, "(no reason given)")
    } else {
        write!(w, "{sentence}")
    }
}

/// The `size` of a `Layout`'s debug text: `size: 0x40000` or `size: 262144`.
fn layout_size(text: &str) -> Option<u64> {
    let after = &text[text.find("size:")? + 5..];
    let digits = after.trim_start();
    let end = digits
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(digits.len());
    let number = &digits[..end];
    match number.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => number.parse().ok(),
    }
}

/// A size in bytes, KiB, MiB or GiB, exact or with one decimal.
fn write_size(w: &mut dyn Write, size: u64) -> fmt::Result {
    const UNITS: [(&str, u64); 3] = [("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)];
    for (unit, scale) in UNITS {
        if size >= scale {
            let tenths = size * 10 / scale;
            return if tenths.is_multiple_of(10) {
                write!(w, "{} {unit}", tenths / 10)
            } else {
                write!(w, "{}.{} {unit}", tenths / 10, tenths % 10)
            };
        }
    }
    write!(w, "{size} bytes")
}

/// See [`Facts::title`].
pub struct Title<'a>(Option<&'a str>);

impl fmt::Display for Title<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(board) => write!(f, "Lucina {board} crashed"),
            None => write!(f, "LucinaOS crashed"),
        }
    }
}

/// Says in words what an `ESR_EL1` value means, for the exceptions a kernel takes.
fn write_esr(w: &mut dyn Write, esr: u64) -> fmt::Result {
    let class = (esr >> 26) & 0x3f;
    let iss = esr & 0x1ff_ffff;
    let abort = |w: &mut dyn Write, what: &str| -> fmt::Result {
        write!(w, "{what}")?;
        if class == 0x25 || class == 0x24 {
            let wnr = if iss & (1 << 6) != 0 { "write" } else { "read" };
            write!(w, " {wnr}")?;
        }
        let dfsc = iss & 0x3f;
        match dfsc {
            0..=3 => write!(w, ", address size fault L{}", dfsc & 3),
            4..=7 => write!(w, ", translation fault L{}", dfsc & 3),
            8..=11 => write!(w, ", access flag fault L{}", dfsc & 3),
            12..=15 => write!(w, ", permission fault L{}", dfsc & 3),
            16 => write!(w, ", synchronous external abort"),
            33 => write!(w, ", alignment fault"),
            _ => write!(w, ", fault status {dfsc:#x}"),
        }
    };
    match class {
        0x00 => write!(w, "unknown instruction"),
        0x21 => abort(w, "instruction abort"),
        0x25 => abort(w, "data abort"),
        0x24 => abort(w, "user data abort"),
        0x20 => abort(w, "user instruction abort"),
        0x22 => write!(w, "misaligned pc"),
        0x26 => write!(w, "misaligned sp"),
        0x2f => write!(w, "SError"),
        0x3c => write!(w, "BRK"),
        _ => write!(w, "exception class {class:#x}"),
    }
}

/// How many crashes in a row end the restarts: a kernel that crashes at every boot would otherwise go on for ever, and
/// each round writes over an older report.
pub const MAX_CRASHES_IN_A_ROW: u32 = 3;

/// How long the screen stays after that many crashes in a row, unless `lucina.panic_loop=` says otherwise.
pub const LOOP_RESTART_SECONDS: u32 = 300;

/// How long a key press holds that screen.
pub const HELD_SECONDS: u32 = 3600;

/// How long the screen stays before the restart, unless `lucina.panic=` says otherwise.
pub const DEFAULT_RESTART_SECONDS: u32 = 30;

/// What follows the crash screen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum After {
    /// Show the screen for this long, then restart.
    Restart { seconds: u32 },
    /// `lucina.panic=0`: stay on the screen.
    StayAsked,
    /// Too many crashes in a row: stay on the screen for longer, then restart anyway.
    StayLoop { crashes: u32, seconds: u32 },
    /// A key was pressed on that screen: someone is reading it.
    Held { seconds: u32 },
    /// No screen to read and nothing saved: power off as stock Asterinas does, which also ends QEMU in a test.
    PowerOff,
}

impl After {
    /// Decides from `lucina.panic=`, `lucina.panic_loop=`, whether a screen shows the report, and the crashes in a row if the saved log counts
    /// them.
    pub fn decide(
        panic_seconds: Option<u32>,
        loop_seconds: Option<u32>,
        has_screen: bool,
        crashes: Option<u32>,
    ) -> Self {
        let seconds = match panic_seconds {
            Some(0) => return After::StayAsked,
            Some(seconds) => seconds,
            None if has_screen || crashes.is_some() => DEFAULT_RESTART_SECONDS,
            None => return After::PowerOff,
        };
        match crashes {
            Some(crashes) if crashes >= MAX_CRASHES_IN_A_ROW => After::StayLoop {
                crashes,
                seconds: loop_seconds.unwrap_or(LOOP_RESTART_SECONDS),
            },
            _ => After::Restart { seconds },
        }
    }

    /// The line under the title. `seconds_left` is for a countdown.
    pub fn write_status(&self, w: &mut dyn Write, seconds_left: Option<u32>) -> fmt::Result {
        match *self {
            After::Restart { seconds } => write!(
                w,
                "Restarting in {} s. A kernel reloaded in RAM goes back to the one on the disk.",
                seconds_left.unwrap_or(seconds)
            ),
            After::StayAsked => {
                write!(
                    w,
                    "Staying here (lucina.panic=0). Hold the power button to restart."
                )
            }
            After::StayLoop { crashes, seconds } => write!(
                w,
                "Crashed {crashes} times in a row. Restarting anyway in {} s; press a key to keep this screen.",
                seconds_left.unwrap_or(seconds)
            ),
            After::Held { seconds } => write!(
                w,
                "A key holds this screen for {} s more. Hold the power button to restart sooner.",
                seconds_left.unwrap_or(seconds)
            ),
            After::PowerOff => write!(w, "Powering off."),
        }
    }
}

impl After {
    /// What happens next, as the crash screen's footer says it: short, with the countdown.
    pub fn write_short_status(&self, w: &mut dyn Write, seconds_left: Option<u32>) -> fmt::Result {
        match *self {
            After::Restart { seconds } => write!(w, "Restarting in {} s", seconds_left.unwrap_or(seconds)),
            After::StayAsked => write!(w, "Staying here (lucina.panic=0)"),
            After::StayLoop { crashes, seconds } => write!(
                w,
                "Crashed {crashes} times in a row. Restarting anyway in {} s; a key keeps this screen",
                seconds_left.unwrap_or(seconds)
            ),
            After::Held { seconds } => write!(w, "A key holds this screen for {} s", seconds_left.unwrap_or(seconds)),
            After::PowerOff => write!(w, "Powering off"),
        }
    }
}

/// Tells a key press from the keyboard's interrupt line. The line goes low when a report is waiting and nothing reads it
/// after a crash, so it stays low: the first press is all there is to see, and a line that was low from the start says
/// nothing.
#[derive(Default)]
pub struct KeyWatch {
    seen_up: bool,
}

impl KeyWatch {
    /// Takes the line's level and says whether a key has gone down.
    pub fn saw(&mut self, low: bool) -> bool {
        self.seen_up |= !low;
        low && self.seen_up
    }
}

/// The first line of a saved report's reason.
pub fn reason_of(report: &[u8]) -> &[u8] {
    const LABEL: &[u8] = b"reason: ";
    report
        .split(|&b| b == b'\n')
        .find_map(|line| line.strip_prefix(LABEL))
        .unwrap_or(b"(no reason given)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Buf;

    fn facts<'a>(regs: Option<&'a Regs>, backtrace: &'a [u64]) -> Facts<'a> {
        Facts {
            kernel_name: "LucinaJuno",
            kind: Kind::Panic,
            reason: "boom",
            cpu: 0,
            tid: Some(12),
            uptime_ms: 61_250,
            boot: Some(7),
            build: "0.19.0 juno-abc",
            regs,
            backtrace,
        }
    }

    fn text(f: impl FnOnce(&mut dyn Write) -> fmt::Result, out: &mut [u8]) -> &str {
        let mut buf = Buf::new(out);
        f(&mut buf).unwrap();
        let len = buf.len();
        core::str::from_utf8(&out[..len]).unwrap()
    }

    #[test]
    fn titles() {
        let mut f = facts(None, &[]);
        let mut out = [0u8; 64];
        assert_eq!(
            text(|w| write!(w, "{}", f.title()), &mut out),
            "Lucina Juno crashed"
        );
        f.kernel_name = "LucinaOS";
        assert_eq!(
            text(|w| write!(w, "{}", f.title()), &mut out),
            "LucinaOS crashed"
        );
    }

    #[test]
    fn report_has_every_fact() {
        let regs = Regs {
            pc: 0xffff_8000_0001_2345,
            fault: Some((0x9600_0045, 0xdead_0000)),
            ..Regs::default()
        };
        let backtrace = [0xffff_8000_0000_0001, 0xffff_8000_0000_0002];
        let f = facts(Some(&regs), &backtrace);
        let mut out = [0u8; 4096];
        let report = text(|w| f.write_report(w), &mut out);
        for needle in [
            "Lucina Juno crashed\n",
            "reason: boom\n",
            "where: panic on CPU 0, thread 12, uptime 61.250 s, boot 7\n",
            "build: 0.19.0 juno-abc\n",
            "pc ffff800000012345",
            "esr 96000045  far 00000000dead0000  data abort write, translation fault L1\n",
            "x24 0000000000000000",
            "x29 0000000000000000",
            "ffff800000000001 \nffff800000000002 \n",
        ] {
            assert!(report.contains(needle), "{needle:?} in {report}");
        }
    }

    #[test]
    fn what_follows_a_crash() {
        use After::*;
        assert_eq!(
            After::decide(Some(5), None, true, Some(0)),
            Restart { seconds: 5 }
        );
        assert_eq!(After::decide(Some(5), None, false, None), Restart { seconds: 5 });
        assert_eq!(After::decide(None, None, true, None), Restart { seconds: 30 });
        assert_eq!(After::decide(None, None, false, Some(1)), Restart { seconds: 30 });
        assert_eq!(After::decide(None, None, false, None), PowerOff);
        assert_eq!(After::decide(Some(0), None, true, Some(9)), StayAsked);
        assert_eq!(After::decide(Some(0), None, false, None), StayAsked);
        assert_eq!(
            After::decide(Some(5), None, true, Some(2)),
            Restart { seconds: 5 }
        );
        assert_eq!(
            After::decide(Some(5), None, true, Some(3)),
            StayLoop {
                crashes: 3,
                seconds: LOOP_RESTART_SECONDS
            }
        );
        assert_eq!(
            After::decide(None, Some(20), true, Some(4)),
            StayLoop {
                crashes: 4,
                seconds: 20
            }
        );
    }

    #[test]
    fn status_lines_say_it() {
        let mut out = [0u8; 128];
        let restart = After::Restart { seconds: 30 };
        assert!(
            text(|w| restart.write_status(w, Some(9)), &mut out).starts_with("Restarting in 9 s")
        );
        assert!(
            text(|w| restart.write_status(w, None), &mut out).starts_with("Restarting in 30 s")
        );
        let looped = After::StayLoop {
            crashes: 3,
            seconds: 300,
        };
        assert!(
            text(|w| looped.write_status(w, None), &mut out)
                .starts_with("Crashed 3 times in a row. Restarting anyway in 300 s")
        );
    }

    #[test]
    fn headlines_say_what_and_why() {
        let mut out = [0u8; 256];
        let mut f = facts(None, &[]);
        f.reason = "Heap allocation error, layout = Layout {\n    size: 0x200000,\n    align: 8 (1 << 3),\n}";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Kernel panic: heap allocation of 2 MiB failed");
        f.reason = "Heap allocation error, layout = Layout { size: 262144, align: 1 }";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Kernel panic: heap allocation of 256 KiB failed");
        f.reason = "Heap allocation error, layout = Layout { size: 0x1800, align: 8 }";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Kernel panic: heap allocation of 6 KiB failed");
        f.reason = "Heap allocation error, layout = Layout { size: 0x1880, align: 8 }";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Kernel panic: heap allocation of 6.1 KiB failed");
        f.reason = "Heap allocation error, layout = Layout { size: 24, align: 8 }";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Kernel panic: heap allocation of 24 bytes failed");
        f.reason = "called `Option::unwrap()` on a `None` value at kernel/core/src/syscall/arch/arm.rs:8";
        assert_eq!(
            text(|w| f.write_headline(w), &mut out),
            "Kernel panic: called `Option::unwrap()` on a `None` value at kernel/core/src/syscall/arch/arm.rs:8"
        );
        f.kind = Kind::SoftLockup;
        f.reason = "CPU 2 is stuck, perhaps with interrupts off: its watchdog thread did not run for 20 s. These registers are of CPU 3, which noticed";
        assert_eq!(
            text(|w| f.write_headline(w), &mut out),
            "Soft lockup: CPU 2 is stuck, perhaps with interrupts off: its watchdog thread did not run for 20 s"
        );
        f.reason = "";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Soft lockup: (no reason given)");
        f.reason = "Heap allocation error, layout = (cut";
        assert_eq!(text(|w| f.write_headline(w), &mut out), "Soft lockup: Heap allocation error, layout = (cut");
    }

    #[test]
    fn footers_say_boot_time_next_and_where() {
        let mut out = [0u8; 256];
        let f = facts(None, &[]);
        let mut status = [0u8; 128];
        let restart = After::Restart { seconds: 30 };
        let s = text(|w| restart.write_short_status(w, Some(9)), &mut status);
        assert_eq!(
            text(|w| f.write_footer(w, s, true), &mut out),
            "boot 7 | uptime 61.2 s | Restarting in 9 s | full record: lucina-crash list"
        );
        let mut unsaved = f;
        unsaved.boot = None;
        assert_eq!(
            text(|w| unsaved.write_footer(w, "Powering off", false), &mut out),
            "uptime 61.2 s | Powering off | full record: the serial console"
        );
        let looped = After::StayLoop { crashes: 3, seconds: 300 };
        assert!(text(|w| looped.write_short_status(w, None), &mut status).starts_with("Crashed 3 times in a row. Restarting anyway in 300 s"));
        assert_eq!(text(|w| After::StayAsked.write_short_status(w, None), &mut status), "Staying here (lucina.panic=0)");
        assert!(text(|w| After::Held { seconds: 3600 }.write_short_status(w, None), &mut status).starts_with("A key holds this screen"));
    }

    #[test]
    fn a_key_is_a_line_going_low() {
        let mut keys = KeyWatch::default();
        assert!(!keys.saw(true), "low from the start says nothing");
        assert!(!keys.saw(false));
        assert!(keys.saw(true));
        assert!(keys.saw(true));
    }

    #[test]
    fn reason_of_a_report() {
        assert_eq!(reason_of(b"Juno crashed\nreason: boom\nmore\n"), b"boom");
        assert_eq!(reason_of(b"junk"), b"(no reason given)");
    }

    #[test]
    fn registers_fit_ninety_columns() {
        let regs = Regs::default();
        let f = facts(Some(&regs), &[]);
        let mut out = [0u8; 4096];
        let text = text(|w| f.write_regs(w), &mut out);
        assert!(text.lines().all(|line| line.len() <= 90), "{text}");
    }

    #[test]
    fn kinds_round_trip() {
        for kind in [
            Kind::Panic,
            Kind::SoftLockup,
            Kind::WatchdogBark,
            Kind::UserWatchdog,
            Kind::UnexplainedReset,
            Kind::RequestedReset,
            Kind::Modem,
        ] {
            assert_eq!(Kind::from_u32(kind as u32), Some(kind));
        }
        assert_eq!(Kind::from_u32(0), None);
    }
}
