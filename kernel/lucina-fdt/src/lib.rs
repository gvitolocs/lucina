//! Flattened device tree (devicetree specification v0.4, section 5).
//!
//! * The reader ([`Fdt`], [`Node`], [`Property`], ...) is `no_std`, allocation-free and `unsafe`-free. It validates
//!   the header and the whole structure block once in [`Fdt::new`]; every later read is bounds-checked as well, and
//!   malformed input yields an [`Error`], never a panic or an endless loop (each token consumes at least 4 bytes).
//! * With the default `std` feature, [`Tree`] parses a blob into an owned tree, edits it and serializes a valid
//!   version-17 DTB back (memory reservations preserved, property names deduplicated).
//!
//! Built for the kernel test that adds a `/reserved-memory` node with `no-map` children to the DTB QEMU generates for
//! `-machine virt`. The fixture `tests/data/qemu-virt-gicv3-8g.dtb` was produced with the pinned
//! `asterinas/kernel-dev` image: `qemu-system-aarch64 -machine virt,gic-version=3,dumpdtb=v.dtb -cpu cortex-a72 -m 8G`.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

mod error;
mod patch;
mod reader;

#[cfg(feature = "std")]
pub mod files;
#[cfg(feature = "std")]
pub mod report;
#[cfg(feature = "std")]
mod tree;

pub use error::Error;
pub use patch::{Edit, MAX_EDITS, patch};
pub use reader::{
    Children, Chosen, Fdt, Framebuffer, MemReservations, MemoryRegions, Node, Properties, Property,
    RegIter, Region, ReservedRegion, ReservedRegions, StringList, Token, Tokens,
};
#[cfg(feature = "std")]
pub use tree::{
    FbFormat, FramebufferSpec, NodeBuf, PropBuf, RegionSpec, Tree, cells, cells32, string,
};

/// FDT magic number.
pub const MAGIC: u32 = 0xd00d_feed;
/// Size of the version-17 header.
pub const HEADER_SIZE: usize = 40;
/// Deepest node nesting the editor parses or writes (the reader itself needs no stack).
pub const MAX_DEPTH: usize = 64;
