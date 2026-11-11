//! The loader's own boot-stage times, for a performance session to read from the kernel command line or `LOG.TXT` without mounting the ESP.
//!
//! LucinaUEFI reads the architected counter (milliseconds since the machine's reset) at four points and passes them on as one word,
//! `lucina.loader_ms=<entry>,<chosen>,<read>,<exit>`, joined to the kernel's arguments before ` -- ` like `lucinaos.board=`.
//! The word is added only whole: a command line with no room for it is left as it was.

use crate::uefi::merge_cmdline;
use core::fmt::{self, Write};

/// The counter, in milliseconds, at the loader's four stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LoaderTimes {
    /// The loader's entry point.
    pub entry: u64,
    /// After the configuration, the trial bookkeeping and the choice of what to start.
    pub chosen: u64,
    /// After the kernel and the initramfs are read.
    pub read: u64,
    /// The last point before the command line goes into the device tree, just before ExitBootServices.
    pub exit: u64,
}

/// A fixed-size text buffer: the longest word is four 20-digit numbers and the key, well under 128 bytes.
struct Buf {
    bytes: [u8; 128],
    len: usize,
}

impl Write for Buf {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        self.bytes
            .get_mut(self.len..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl Buf {
    fn new() -> Self {
        Buf {
            bytes: [0; 128],
            len: 0,
        }
    }

    fn str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// The longest command line a stale word is stripped from (the arm64 kernel's own limit is 2048).
const CLEAN_MAX: usize = 2048;

/// The key of the word, with its `=`.
const KEY: &str = "lucina.loader_ms=";

/// `base` without `lucina.loader_ms=` words among the kernel's arguments (before ` -- `), written into `out` only if there was one.
/// None when the stripped line does not fit `out`.
fn without_word<'o>(base: &'o str, out: &'o mut [u8]) -> Option<&'o str> {
    let kernel_end = if base.starts_with("-- ") || base == "--" {
        0
    } else {
        base.find(" -- ")
            .or(base.ends_with(" --").then(|| base.len() - 3))
            .unwrap_or(base.len())
    };
    let (kernel, rest) = base.split_at(kernel_end);
    if !kernel.split_whitespace().any(|w| w.starts_with(KEY)) {
        return Some(base);
    }
    let mut n = 0;
    let mut push = |s: &str| -> Option<()> {
        out.get_mut(n..n + s.len())?.copy_from_slice(s.as_bytes());
        n += s.len();
        Some(())
    };
    let mut first = true;
    for w in kernel.split_whitespace().filter(|w| !w.starts_with(KEY)) {
        if !first {
            push(" ")?;
        }
        first = false;
        push(w)?;
    }
    push(rest)?;
    core::str::from_utf8(&out[..n]).ok()
}

/// The command-line word: `lucina.loader_ms=<entry>,<chosen>,<read>,<exit>`.
struct Word<'a>(&'a LoaderTimes);

impl fmt::Display for Word<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0;
        write!(
            f,
            "lucina.loader_ms={},{},{},{}",
            t.entry, t.chosen, t.read, t.exit
        )
    }
}

/// The log line: `times: entry N ms, chosen N ms, read N ms, exit N ms`.
struct LogLine<'a>(&'a LoaderTimes);

impl fmt::Display for LogLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0;
        write!(
            f,
            "times: entry {} ms, chosen {} ms, read {} ms, exit {} ms",
            t.entry, t.chosen, t.read, t.exit
        )
    }
}

impl LoaderTimes {
    /// The command-line word, for formatting.
    pub fn word(&self) -> impl fmt::Display + '_ {
        Word(self)
    }

    /// The `LOG.TXT` line, for formatting.
    pub fn log_line(&self) -> impl fmt::Display + '_ {
        LogLine(self)
    }

    /// `base` with the word added to the kernel's arguments (before ` -- `, else at the end), written into `out`.
    /// None when the result does not fit `out`: the command line is never truncated.
    pub fn add_to_cmdline<'b>(&self, base: &str, out: &'b mut [u8]) -> Option<&'b str> {
        let mut word = Buf::new();
        write!(word, "{}", self.word()).ok()?;
        // A word from an earlier boot (a command line copied from a running system) is dropped, not doubled.
        let mut clean = [0u8; CLEAN_MAX];
        let base = without_word(base, &mut clean)?;
        merge_cmdline(base, word.str(), out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: LoaderTimes = LoaderTimes {
        entry: 1200,
        chosen: 1250,
        read: 1900,
        exit: 1950,
    };
    const WORD: &str = "lucina.loader_ms=1200,1250,1900,1950";

    #[test]
    fn word_and_log_line() {
        let mut b = Buf::new();
        write!(b, "{}", T.word()).unwrap();
        assert_eq!(b.str(), WORD);
        let mut b = Buf::new();
        write!(b, "{}", T.log_line()).unwrap();
        assert_eq!(
            b.str(),
            "times: entry 1200 ms, chosen 1250 ms, read 1900 ms, exit 1950 ms"
        );
    }

    #[test]
    fn largest_values_fit_the_buffer() {
        let m = u64::MAX;
        let t = LoaderTimes {
            entry: m,
            chosen: m,
            read: m,
            exit: m,
        };
        let mut out = [0u8; 256];
        assert!(t.add_to_cmdline("a", &mut out).is_some());
    }

    #[test]
    fn added_at_the_end_without_init_arguments() {
        let mut out = [0u8; 128];
        assert_eq!(
            T.add_to_cmdline("console=tty0 quiet", &mut out).unwrap(),
            format!("console=tty0 quiet {WORD}")
        );
        let mut out = [0u8; 128];
        assert_eq!(T.add_to_cmdline("", &mut out).unwrap(), WORD);
    }

    #[test]
    fn added_before_the_init_separator() {
        let mut out = [0u8; 128];
        assert_eq!(
            T.add_to_cmdline("quiet -- /init x", &mut out).unwrap(),
            format!("quiet {WORD} -- /init x")
        );
        let mut out = [0u8; 128];
        assert_eq!(
            T.add_to_cmdline("-- /init", &mut out).unwrap(),
            format!("{WORD} -- /init")
        );
    }

    #[test]
    fn after_an_existing_board_word() {
        let mut out = [0u8; 160];
        let got = T
            .add_to_cmdline("quiet lucinaos.board=galaxy-book-go -- /init", &mut out)
            .unwrap();
        assert_eq!(
            got,
            format!("quiet lucinaos.board=galaxy-book-go {WORD} -- /init")
        );
    }

    #[test]
    fn a_stale_word_is_replaced_not_doubled() {
        let mut out = [0u8; 160];
        let got = T
            .add_to_cmdline("quiet lucina.loader_ms=1,2,3,4 x=1 -- /init", &mut out)
            .unwrap();
        assert_eq!(got, format!("quiet x=1 {WORD} -- /init"));
        let mut out = [0u8; 160];
        assert_eq!(
            T.add_to_cmdline("lucina.loader_ms=9,9,9,9", &mut out)
                .unwrap(),
            WORD
        );
    }

    #[test]
    fn refused_at_the_length_limit_not_truncated() {
        let base = "quiet -- /init";
        let need = format!("quiet {WORD} -- /init").len();
        let mut out = vec![0u8; need - 1];
        assert_eq!(T.add_to_cmdline(base, &mut out), None);
        let mut out = vec![0u8; need];
        assert_eq!(T.add_to_cmdline(base, &mut out).unwrap().len(), need);
    }
}
