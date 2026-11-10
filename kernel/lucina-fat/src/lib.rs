//! FAT12, FAT16 and FAT32 with VFAT long names, for the LucinaOS kernel.
//!
//! The behaviour is Linux's (`fs/fat`) and Microsoft's FAT specification: names, short-name generation,
//! timestamps, FSInfo and the dirty bit follow them. The crate is `no_std`, has no `unsafe` code and reaches the
//! device through the small [`BlockIo`] trait, so that the whole file system runs in host tests against images made
//! by `mkfs.fat` and checked by `fsck.fat`.
//!
//! [`Volume`] is the mounted file system; [`Node`] is the in-memory state of one file or directory that the caller
//! keeps and passes in.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod bpb;
mod dirent;
mod error;
mod io;
mod name;
mod time;
mod vol;

pub use bpb::{FatType, Geometry};
pub use dirent::{ATTR_ARCH, ATTR_DIR, ATTR_HIDDEN, ATTR_RO, ATTR_SYSTEM, ATTR_VOLUME, DirEntry};
pub use error::{Error, Result};
pub use io::{BlockIo, CACHE_BLOCK};
pub use time::Time;
pub use vol::{Clock, DirId, MountOptions, Node, Pos, StatFs, Volume};
