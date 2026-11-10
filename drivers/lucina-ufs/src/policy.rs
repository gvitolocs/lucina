//! Which logical unit may be written, and which commands may reach a unit.
//!
//! The owner's rule for the Galaxy Book Go: nothing but the main user LUN is ever written. Its other units hold the
//! boot chain (XBL on the boot LUNs), CDT/DDR training data, Qualcomm firmware (TZ, HYP, AOP...) and the modem image;
//! a stray write there can leave the laptop unbootable. So the decision is made from what the device itself reports
//! and errs towards read-only:
//!
//! * the unit must be the configured main LUN (0 by default; `None` makes every unit read-only);
//! * enabled, not a boot LU (bBootLunID 0), not write protected (bLUWriteProtect 0), normal memory type;
//! * and the largest enabled unit, so a device whose LUN 0 is not the big user area is never written by accident.
//!
//! Commands are allowlisted per role. Read-only units get the commands that only read (TEST UNIT READY, REQUEST
//! SENSE, INQUIRY, READ CAPACITY, READ, MODE SENSE, REPORT LUNS); the main unit also gets WRITE (10/16) and
//! SYNCHRONIZE CACHE. Everything else (FORMAT UNIT, UNMAP, WRITE SAME, WRITE BUFFER, MODE SELECT, SECURITY PROTOCOL
//! OUT, START STOP UNIT, ...) is refused for every unit, as is any well-known LUN.

use crate::{desc::UnitDesc, write_rule};
pub use crate::write_rule::ReadOnly;

/// What the driver lets a unit do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitRole {
    /// The main user LUN: read and write.
    Main,
    ReadOnly(ReadOnly),
}

impl UnitRole {
    pub fn writable(self) -> bool {
        self == UnitRole::Main
    }
}

/// The write policy: which LUN is the main user LUN that may be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WritePolicy {
    pub main_lun: Option<u8>,
}

impl Default for WritePolicy {
    fn default() -> Self {
        Self { main_lun: Some(0) }
    }
}

impl WritePolicy {
    /// Every unit read-only.
    pub const READ_ONLY: Self = Self { main_lun: None };

    /// The role of `unit` (LUN `lun`), given the capacity in bytes of the largest enabled unit.
    pub fn role(&self, lun: u8, unit: &UnitDesc, largest_bytes: u64) -> UnitRole {
        let capacity = unit
            .block_size()
            .map(|bs| unit.block_count.saturating_mul(bs as u64));
        match write_rule::may_write(
            self.main_lun,
            lun,
            unit.enabled,
            unit.boot_lun_id,
            unit.write_protect,
            unit.memory_type,
            capacity,
            largest_bytes,
        ) {
            Ok(()) => UnitRole::Main,
            Err(why) => UnitRole::ReadOnly(why),
        }
    }

    /// Whether a CDB may be sent to a unit with this role.
    pub fn allows(role: UnitRole, lun: u8, cdb: &[u8]) -> bool {
        let Some(&op) = cdb.first() else {
            return false;
        };
        write_rule::command_allowed(role.writable(), lun, op, cdb.get(1).copied())
    }
}
