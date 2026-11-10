//! Fortuna's game library: the pure part under the Games and Files screens.
//!
//! - [`names`]: what a file is ([`Kind`]), which [`Runner`] opens a game and what it needs, the search fold and
//!   [`hit`] for highlighting a row, [`order`], and how sizes, dates and play times read ([`Size`], [`Day`], [`Ago`]).
//!   Re-exported at the root.
//! - [`platform`]: which system a game file is for, and the filter pills.
//! - [`index`]: every game on the card in RAM, sorted by title, with as-you-type [`Search`] that narrows instead of
//!   rescanning and follows [`hit`]'s rules; play times and "Recently played".
//! - [`cache`]: the index as a checked file on the card, so the next boot has it at once.
//! - [`banner`]: the icon, palette and title inside a `.nds`/`.dsi` file. The icon is already DS OBJ tiles, so the
//!   kernel shows it as a sprite with no conversion.
//! - [`rules`]: the matching and banner-bounds rules, checked by Verus in the shipped file
//!   (scripts/verify-shipped.sh, mutants in verification/fortuna-library).
//!
//! `no_std` with `alloc`, no I/O: the kernel walks the card and reads files; the host runs the tests.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod banner;
pub mod cache;
pub mod index;
pub mod names;
pub mod platform;
pub mod rules;

pub use index::{Library, Search};
pub use names::*;
pub use platform::{Platform, PlatformMask};
