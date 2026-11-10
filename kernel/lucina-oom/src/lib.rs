//! The out-of-memory killer's pure part, for the LucinaOS kernel (patch 1118). As Linux's `mm/oom_kill.c`: when a user page
//! fault finds no memory, the process with the highest score is sent SIGKILL and the fault is tried again, instead of the
//! kernel panicking (Juno, 2026-11-08 15:21: a browser at 1920x1080 used all 3.69 GB and a page-table node found no frame).
//!
//! - [`rules::points`]: resident pages plus `oom_score_adj` thousandths of the machine, never for -1000 (`oom_badness`).
//! - [`rules::choose`]: the highest score, the last of equal ones (`oom_evaluate_task`).
//! - [`rules::user_may_take`]: user page faults stop short of the last [`DEFAULT_RESERVE_PAGES`], which page-table nodes and
//!   the kernel's own allocations keep, so that the killer runs before anything in the kernel can fail.
//! - [`rules::adj_at_exec`] and [`is_protected`]: init and the programs that keep the machine reachable get -1000 at exec
//!   ([`PROTECTED`]); a program they start that inherited -1000 starts at 0, so that an SSH session or an app is not
//!   protected by its parent.
#![cfg_attr(not(test), no_std)]

pub mod rules;
#[cfg(test)]
mod tests;

pub use rules::{ADJ_MAX, ADJ_MIN, adj_at_exec, choose, points, user_may_take};

/// Frames a user page fault leaves free: 8 MiB of 4 KiB pages. Linux's `min_free_kbytes` on a 4 GB machine is about this.
pub const DEFAULT_RESERVE_PAGES: u64 = 2048;

/// The programs that keep the machine reachable and its record kept: never chosen (`oom_score_adj` -1000 at exec). init
/// (pid 1) is never chosen either, by the kernel. The owner's list (coordinator, 2026-11-08: init, devd, eventd, the shell,
/// sshd), plus lucina-watchd (it pets the hardware watchdog: killing it resets the machine) and lucina-evlog (it records the
/// kill).
pub const PROTECTED: [&str; 7] = [
    "init",
    "lucina-devd",
    "lucina-eventd",
    "lucina-shell",
    "lucina-sshd",
    "lucina-watchd",
    "lucina-evlog",
];

/// Whether the program at `path` (an executable's path, or its name) is one of [`PROTECTED`]: by the last path element.
pub fn is_protected(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    PROTECTED.contains(&name)
}

/// `lucina.oom.reserve=<MiB>` from the kernel part of a command line, as pages; [`DEFAULT_RESERVE_PAGES`] without it or for
/// a value that is not a number.
pub fn reserve_from_cmdline(cmdline: &str) -> u64 {
    // The words before ` -- ` are the kernel's; a line that starts with `-- ` has none.
    let kernel = if cmdline.starts_with("-- ") { "" } else { cmdline.split(" -- ").next().unwrap_or("") };
    kernel
        .split_ascii_whitespace()
        .filter_map(|w| w.strip_prefix("lucina.oom.reserve="))
        .last()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|mib| mib.saturating_mul(256))
        .unwrap_or(DEFAULT_RESERVE_PAGES)
}
