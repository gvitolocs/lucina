//! The SCSI commands the driver sends (SPC-4, SBC-3, as UFS JESD220 section 11 subsets them) and the data it parses.

use crate::Error;
pub use crate::write_rule::{
    INQUIRY, MODE_SENSE_10, READ_10, READ_16, READ_CAPACITY_10, REPORT_LUNS, REQUEST_SENSE, SAI_READ_CAPACITY_16,
    SERVICE_ACTION_IN_16, SYNCHRONIZE_CACHE_10, TEST_UNIT_READY, WRITE_10, WRITE_16,
};

pub const START_STOP_UNIT: u8 = 0x1b;

/// Sense keys the driver acts on.
pub const SENSE_NOT_READY: u8 = 0x2;
pub const SENSE_UNIT_ATTENTION: u8 = 0x6;

pub fn test_unit_ready() -> [u8; 6] {
    [TEST_UNIT_READY, 0, 0, 0, 0, 0]
}

pub fn inquiry(len: u8) -> [u8; 6] {
    [INQUIRY, 0, 0, 0, len, 0]
}

pub fn request_sense(len: u8) -> [u8; 6] {
    [REQUEST_SENSE, 0, 0, 0, len, 0]
}

pub fn read_capacity_10() -> [u8; 10] {
    [READ_CAPACITY_10, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

pub fn read_capacity_16(len: u32) -> [u8; 16] {
    let mut c = [0u8; 16];
    c[0] = SERVICE_ACTION_IN_16;
    c[1] = SAI_READ_CAPACITY_16;
    c[10..14].copy_from_slice(&len.to_be_bytes());
    c
}

pub fn synchronize_cache_10() -> [u8; 10] {
    // LBA 0 and 0 blocks: the whole medium.
    [SYNCHRONIZE_CACHE_10, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// READ or WRITE of `blocks` logical blocks at `lba`: the 10-byte form when it fits, as Linux sd does, else the
/// 16-byte one. Returns the CDB and its length.
pub fn rw(write: bool, lba: u64, blocks: u32) -> ([u8; 16], usize) {
    let mut c = [0u8; 16];
    if lba <= u32::MAX as u64 && blocks <= u16::MAX as u32 {
        c[0] = if write { WRITE_10 } else { READ_10 };
        c[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        c[7..9].copy_from_slice(&(blocks as u16).to_be_bytes());
        (c, 10)
    } else {
        c[0] = if write { WRITE_16 } else { READ_16 };
        c[2..10].copy_from_slice(&lba.to_be_bytes());
        c[10..14].copy_from_slice(&blocks.to_be_bytes());
        (c, 16)
    }
}

/// Sense data: key, additional sense code and qualifier (fixed or descriptor format).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sense {
    pub key: u8,
    pub asc: u8,
    pub ascq: u8,
}

impl Sense {
    pub fn parse(d: &[u8]) -> Option<Self> {
        match d.first()? & 0x7f {
            0x70 | 0x71 if d.len() >= 14 => Some(Self {
                key: d[2] & 0xf,
                asc: d[12],
                ascq: d[13],
            }),
            0x70 | 0x71 if d.len() >= 3 => Some(Self {
                key: d[2] & 0xf,
                asc: 0,
                ascq: 0,
            }),
            0x72 | 0x73 if d.len() >= 4 => Some(Self {
                key: d[1] & 0xf,
                asc: d[2],
                ascq: d[3],
            }),
            _ => None,
        }
    }
}

/// Standard INQUIRY data: device type and the identification strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inquiry {
    pub device_type: u8,
    pub vendor: [u8; 8],
    pub product: [u8; 16],
    pub revision: [u8; 4],
}

impl Inquiry {
    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        if d.len() < 36 {
            return Err(Error::BadData("INQUIRY data"));
        }
        let mut i = Self {
            device_type: d[0] & 0x1f,
            vendor: [0; 8],
            product: [0; 16],
            revision: [0; 4],
        };
        i.vendor.copy_from_slice(&d[8..16]);
        i.product.copy_from_slice(&d[16..32]);
        i.revision.copy_from_slice(&d[32..36]);
        Ok(i)
    }
}

/// Number of logical blocks and their size, from READ CAPACITY (10) or (16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub blocks: u64,
    pub block_size: u32,
}

impl Capacity {
    pub fn parse_10(d: &[u8]) -> Result<Self, Error> {
        if d.len() < 8 {
            return Err(Error::BadData("READ CAPACITY (10) data"));
        }
        let last = u32::from_be_bytes([d[0], d[1], d[2], d[3]]);
        let block_size = u32::from_be_bytes([d[4], d[5], d[6], d[7]]);
        Ok(Self {
            blocks: last as u64 + 1,
            block_size,
        })
    }

    pub fn parse_16(d: &[u8]) -> Result<Self, Error> {
        if d.len() < 12 {
            return Err(Error::BadData("READ CAPACITY (16) data"));
        }
        let last = u64::from_be_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]);
        let block_size = u32::from_be_bytes([d[8], d[9], d[10], d[11]]);
        Ok(Self {
            blocks: last
                .checked_add(1)
                .ok_or(Error::BadData("READ CAPACITY (16) data"))?,
            block_size,
        })
    }
}
