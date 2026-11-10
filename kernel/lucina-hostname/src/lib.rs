//! The host name a LucinaOS kernel gives itself: the initial UTS nodename (`uname -n`) and the name its DHCP client sends
//! (option 12). `lucina.hostname=<name>` in the kernel part of the command line (before ` -- `) sets it; without the word
//! it is `lucinaos`, as it was before (the Kindle and the POCO). The laptop's disk command line carries
//! `lucina.hostname=Juno001` (owner, 2026-11-08).
//!
//! A name that breaks the rules in [`rules`] (1 to 64 letters, digits and '-', no '-' at either end) is refused, and the
//! default stands: the kernel says so in its log. With the word given more than once the last one counts, as later words
//! do on Linux's command line.
#![cfg_attr(not(test), no_std)]

pub mod rules;
#[cfg(test)]
mod tests;

pub use rules::{MAX, valid};

/// The name without `lucina.hostname=`.
pub const DEFAULT: &str = "lucinaos";
/// The command-line word.
pub const WORD: &str = "lucina.hostname=";

/// What the command line says about the host name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice<'a> {
    /// No `lucina.hostname=` word: [`DEFAULT`].
    Default,
    /// A name that keeps the rules.
    Given(&'a str),
    /// A name that breaks them: [`DEFAULT`] stands.
    Refused(&'a str),
}

impl<'a> Choice<'a> {
    /// The name to use.
    pub fn name(self) -> &'a str {
        match self {
            Choice::Given(name) => name,
            Choice::Default | Choice::Refused(_) => DEFAULT,
        }
    }
}

/// Reads `lucina.hostname=` from the kernel part of a command line; the words after ` -- ` belong to init.
pub fn from_cmdline(cmdline: &str) -> Choice<'_> {
    let kernel = cmdline.split(" -- ").next().unwrap_or("");
    match kernel.split_ascii_whitespace().filter_map(|word| word.strip_prefix(WORD)).last() {
        None => Choice::Default,
        Some(name) if valid(name.as_bytes()) => Choice::Given(name),
        Some(name) => Choice::Refused(name),
    }
}
