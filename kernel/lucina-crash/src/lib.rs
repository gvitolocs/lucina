//! What the LucinaOS kernel shows and keeps when it crashes (kernel patches 0680-0699).
//!
//! The kernel's part (`ostd/src/crash`) has the unsafe half: mapping the framebuffer, the registers, the reset. This crate
//! has the rest, so that it can be tested on the host: what the report says ([`report`]), how it looks on the screen
//! ([`screen`], with the backtrace's frames named and cut by [`frames`]), how it is saved across a reset ([`store`]) and the text helpers ([`text`]). Nothing here allocates or takes a lock: it all runs in the
//! panic handler, where neither can be trusted.
#![no_std]
#![forbid(unsafe_code)]

mod ecc;
pub mod frames;
pub mod mmio;
pub mod report;
pub mod screen;
pub mod store;
pub mod text;
pub mod witness;
