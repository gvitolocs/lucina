// SPDX-License-Identifier: MPL-2.0

//! The modem's last words, kept in the saved crash store (kernel patch 1057).
//!
//! When the modem DSP fails, Linux's `qcom_q6v5.c` hears it twice: the DSP's own watchdog interrupt (`q6v5_wdog_interrupt`,
//! SPI 266 on SC7180) and the fatal bit of its SMP2P state (`q6v5_fatal_interrupt`). Both read the DSP's crash reason, a
//! short text in SMEM item 421 (`MPSS_CRASH_REASON_SMEM`), and print it. On Juno nothing listened to the watchdog, and the
//! fatal bit's text only reached the kernel log, which a board reset loses. Now both write a record of kind `modem` in the
//! saved crash store at once, so that a reset that follows still has the modem's words.
//!
//! This file is what the interrupt handler may do: a read bounded by the item and the buffer, a scan bounded by both, a
//! byte map into printable ASCII, and a once-per-cause gate, so that a modem that fails again and again does not push the
//! kernel's own records out of the store's eight slots. No allocation, no loop longer than [`MAX_REASON`]. Verus proves
//! this very file (`verification/juno-modem-witness/rules.rs`, D-011).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The most of the crash reason that is kept, in bytes. The firmware's texts are a few dozen bytes ("err_qdi.c:456:EF:..."),
/// and the record's reason line holds this much.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_REASON: usize = 120;

/// The once-per-boot bit of the watchdog interrupt.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const WATCHDOG: u8 = 1;
/// The once-per-boot bit of the fatal SMP2P bit.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FATAL: u8 = 2;

/// How many bytes to read of an item of `item_len` bytes into a buffer of `cap`: no more than either.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <= cap,
        r as int <= item_len as int,
        r == cap || r as int == item_len as int,
))]
pub fn bounded_len(item_len: u32, cap: usize) -> usize {
    let item_len = item_len as usize;
    if item_len < cap { item_len } else { cap }
}

/// The length of the text at the start of `raw`: up to its first NUL, no more than `cap` bytes nor than `raw` holds.
#[cfg_attr(verus_keep_ghost, verus_spec(n =>
    ensures
        n <= cap,
        n <= raw.len(),
        forall|i: int| 0 <= i < n ==> raw[i] != 0u8,
        n < cap && n < raw.len() ==> raw[n as int] == 0u8,
))]
pub fn reason_len(raw: &[u8], cap: usize) -> usize {
    let limit = if raw.len() < cap { raw.len() } else { cap };
    let mut n = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= n <= limit,
            limit <= raw.len(),
            limit <= cap,
            limit == raw.len() || limit == cap,
            forall|i: int| 0 <= i < n ==> raw[i] != 0u8,
        decreases limit - n
    ))]
    while n < limit {
        if raw[n] == 0 {
            return n;
        }
        n += 1;
    }
    n
}

/// A byte of the reason as it is kept: itself if it is printable ASCII, else `?`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        0x20 <= r <= 0x7e,
        0x20 <= b <= 0x7e ==> r == b,
))]
pub fn printable(b: u8) -> u8 {
    if b >= 0x20 && b <= 0x7e { b } else { b'?' }
}

/// Whether a record for the cause with bit `bit` may be written, given the bits of the causes already written this boot.
/// The kernel takes `seen` from an atomic `fetch_or(bit)`, so of all the callers for one cause only the first sees it clear.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (seen & bit == 0),
))]
pub fn first_time(seen: u8, bit: u8) -> bool {
    seen & bit == 0
}
