// SPDX-License-Identifier: GPL-2.0-only
//! The rules of a shareable pixel-buffer object, `lucina-buf` (`docs/design/juno-zero-copy-frames.md`, sections 3 and 13), as pure
//! functions over integers. The kernel device (`/dev/lucina-surface`) is a thin shell around them.
//!
//! - [`rules::step`]: one operation on one buffer and the byte budget. A buffer lives while any descriptor, mapping or GPU slot
//!   holds it or a GPU batch still names it; it is freed (or, after a GPU failure, quarantined) the moment the last one goes;
//!   a purgeable buffer nobody maps or runs may give its bytes back and comes back, contents lost, when asked to.
//! - [`rules::charge`] and [`rules::release`]: the byte budget, which never goes over its limit nor below zero.
//! - [`rules::pitch`], [`rules::buffer_bytes`], [`rules::surface_size`]: row pitch (a multiple of 64) and the page-aligned size.
//! - [`rules::rows_ok`]: every sync rectangle lies inside the buffer.
//!
//! Verus checks `rules.rs` as it is compiled (`verification/juno-surface/rules.rs`; `verification/juno-surface/mutants.sh` plants
//! bugs that it must refuse).
#![cfg_attr(not(test), no_std)]

pub mod rules;
#[cfg(test)]
mod tests;

pub use rules::{
    Budget, Buf, MAX_DIM, MAX_PITCH, NoRoom, Op, Phase, Refusal, Underflow, buffer_bytes, charge,
    new, pitch, release, rows_ok, step, surface_size,
};
