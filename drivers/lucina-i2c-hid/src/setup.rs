// SPDX-License-Identifier: MPL-2.0

//! How often the kernel sets an I2C-HID device up before it gives up (patch 1191), as a plain rule. Verus proves this very file
//! (`verification/juno-input/rules.rs`, D-011).
//!
//! The original is Linux's `i2c_hid_parse` (v6.12 `i2c-hid-core.c`): `int tries = 3;` and `do { ... msleep(1000) ... } while
//! (tries-- > 0 && ret);`, four tries in all, a second apart. Linux repeats the reset that way; here the whole setup is (the
//! descriptor, the reset, the report descriptor), because a setup that failed used to end the device's thread, and after a
//! reload the keyboard sometimes never appeared.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Tries in all: Linux's `tries = 3` retries after the first.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SETUP_TRIES: u32 = 4;
/// Between tries: Linux's `msleep(1000)`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SETUP_RETRY_MS: u32 = 1000;

/// After `failed` setups that failed, how long to wait before the next one; none once the last has failed (or when none has).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == setup_retry_spec(failed),
))]
pub fn setup_retry_ms(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES { None } else { Some(SETUP_RETRY_MS) }
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn setup_retry_spec(failed: u32) -> Option<u32> {
    if failed == 0 || failed >= SETUP_TRIES { None } else { Some(SETUP_RETRY_MS) }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_tries_a_second_apart() {
        assert_eq!(setup_retry_ms(0), None);
        assert_eq!(setup_retry_ms(1), Some(1000));
        assert_eq!(setup_retry_ms(3), Some(1000));
        assert_eq!(setup_retry_ms(4), None);
        assert_eq!(setup_retry_ms(u32::MAX), None);
    }
}
