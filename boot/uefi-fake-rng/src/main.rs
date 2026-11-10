//! A test program, never on a real disk: installs a fake `EFI_RNG_PROTOCOL` and starts the real LucinaUEFI, unchanged, as a child
//! image, so that what `scripts/qemu-rng-marker.sh` tests is the very bytes that ship. It stays resident as the child's parent, and
//! the child sees this program's volume and file path as its own.
//!
//! `FAKE_RNG_MODE` at build time says what `GetRNG` does: `ok`, `hang`, `fault`, `error`, `slow` (8 s, then ok), or `none` to
//! install nothing. Each call prints `fake-rng: called` first.
//! On any other target this is an empty program, for the merge gate's `cargo check`.
#![cfg_attr(target_os = "uefi", no_std, no_main)]

#[cfg(target_os = "uefi")]
mod fake;

#[cfg(not(target_os = "uefi"))]
fn main() {}
