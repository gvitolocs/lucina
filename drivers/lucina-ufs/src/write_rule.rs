//! The decisions of [`crate::policy`] as plain functions on numbers. Verus proves this very file, not a copy of it
//! (`verification/juno-disk/rules.rs`; the attributes are only seen by Verus, rustc ignores them). What each function promises is
//! in its `ensures`.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const TEST_UNIT_READY: u8 = 0x00;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const REQUEST_SENSE: u8 = 0x03;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const INQUIRY: u8 = 0x12;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const READ_CAPACITY_10: u8 = 0x25;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const READ_10: u8 = 0x28;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const WRITE_10: u8 = 0x2a;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SYNCHRONIZE_CACHE_10: u8 = 0x35;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MODE_SENSE_10: u8 = 0x5a;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const READ_16: u8 = 0x88;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const WRITE_16: u8 = 0x8a;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SERVICE_ACTION_IN_16: u8 = 0x9e;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const SAI_READ_CAPACITY_16: u8 = 0x10;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const REPORT_LUNS: u8 = 0xa0;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const WLUN_FLAG: u8 = 0x80;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MEMORY_TYPE_NORMAL: u8 = 0;

/// Why a unit is read-only.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOnly {
    /// Writes are disabled for every unit.
    WritesDisabled,
    /// Not the configured main LUN.
    NotMain,
    Disabled,
    BootLun,
    WriteProtected,
    NotNormalMemory,
    /// Another enabled unit is larger.
    NotLargest,
    /// Block size the driver does not handle.
    BadBlockSize,
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn changes_the_medium(op: u8) -> bool {
    op == WRITE_10 || op == WRITE_16 || op == SYNCHRONIZE_CACHE_10
}

pub open spec fn only_reads(op: u8) -> bool {
    ||| op == TEST_UNIT_READY
    ||| op == REQUEST_SENSE
    ||| op == INQUIRY
    ||| op == READ_CAPACITY_10
    ||| op == READ_10
    ||| op == READ_16
    ||| op == MODE_SENSE_10
    ||| op == REPORT_LUNS
    ||| op == SERVICE_ACTION_IN_16
}

} // verus!

/// Whether a unit may be written: only the configured LUN, enabled, not a boot LU, not write protected, normal memory, and as
/// large as the largest. `capacity` is its size in bytes, `None` if the driver does not handle its block size.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r is Ok ==> main == Some(lun),
        r is Ok ==> enabled != 0 && boot_lun_id == 0 && write_protect == 0 && memory_type == MEMORY_TYPE_NORMAL,
        r is Ok ==> (capacity matches Some(c) && c >= largest),
        main is None ==> r is Err,
))]
#[allow(clippy::too_many_arguments)]
pub fn may_write(
    main: Option<u8>,
    lun: u8,
    enabled: u8,
    boot_lun_id: u8,
    write_protect: u8,
    memory_type: u8,
    capacity: Option<u64>,
    largest: u64,
) -> Result<(), ReadOnly> {
    let Some(main) = main else {
        return Err(ReadOnly::WritesDisabled);
    };
    if lun != main {
        Err(ReadOnly::NotMain)
    } else if enabled == 0 {
        Err(ReadOnly::Disabled)
    } else if boot_lun_id != 0 {
        Err(ReadOnly::BootLun)
    } else if write_protect != 0 {
        Err(ReadOnly::WriteProtected)
    } else if memory_type != MEMORY_TYPE_NORMAL {
        Err(ReadOnly::NotNormalMemory)
    } else {
        match capacity {
            Some(c) => {
                if c < largest {
                    Err(ReadOnly::NotLargest)
                } else {
                    Ok(())
                }
            }
            None => Err(ReadOnly::BadBlockSize),
        }
    }
}

/// Whether a command may reach a unit. `byte1` is the CDB's second byte. No well-known LUN gets anything, a command that changes
/// the medium needs a writable unit, and a command on neither list is refused for every unit.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> lun & WLUN_FLAG == 0,
        r ==> changes_the_medium(op) || only_reads(op),
        r && changes_the_medium(op) ==> writable,
))]
pub fn command_allowed(writable: bool, lun: u8, op: u8, byte1: Option<u8>) -> bool {
    if lun & WLUN_FLAG != 0 {
        return false;
    }
    if op == WRITE_10 || op == WRITE_16 || op == SYNCHRONIZE_CACHE_10 {
        return writable;
    }
    if op == SERVICE_ACTION_IN_16 {
        return match byte1 {
            Some(sa) => sa & 0x1f == SAI_READ_CAPACITY_16,
            None => false,
        };
    }
    op == TEST_UNIT_READY
        || op == REQUEST_SENSE
        || op == INQUIRY
        || op == READ_CAPACITY_10
        || op == READ_10
        || op == READ_16
        || op == MODE_SENSE_10
        || op == REPORT_LUNS
}
