//! Juno's semantic events (docs/research/juno-ai-native.md, layer 3; stage D).
//!
//! Programs say what changed, in words and ids, so that Lucina follows the machine without watching the screen: an app finished
//! an action or opened a dialog, the shell put an app in front, the kernel saw a program exit or the network go down.
//!
//! * [`rules`]: the queue, order, filter and visibility rules, checked by Verus as shipped (D-011). `no_std`: the kernel's ring
//!   (`/dev/lucina-events`) uses the same file.
//! * With `std`: the wire format, [`emit`] for programs and the hub `lucina-eventd`, which owns the one queue.
#![cfg_attr(not(feature = "std"), no_std)]
// rules.rs is checked by Verus and is not linted here.
#![allow(clippy::manual_range_contains)]

extern crate alloc;

pub mod rules;

#[cfg(feature = "std")]
pub mod client;
#[cfg(feature = "std")]
pub mod hub;
#[cfg(feature = "std")]
pub mod wire;

#[cfg(feature = "std")]
pub use client::{emit, present, Emit, Reader};
