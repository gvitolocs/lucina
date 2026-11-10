//! UFS descriptors (JESD220 section 14.1; offsets as Linux `enum device_desc_param`, `geometry_desc_param`,
//! `unit_desc_param`). Multi-byte fields are big endian.

use crate::Error;
pub use crate::write_rule::{MEMORY_TYPE_NORMAL, WLUN_FLAG};

/// Descriptor IDNs.
pub const IDN_DEVICE: u8 = 0x00;
pub const IDN_CONFIGURATION: u8 = 0x01;
pub const IDN_UNIT: u8 = 0x02;
pub const IDN_STRING: u8 = 0x05;
pub const IDN_GEOMETRY: u8 = 0x07;

/// Attribute IDNs.
pub const ATTR_CURRENT_POWER_MODE: u8 = 0x02;

/// Flag IDNs.
pub const FLAG_DEVICE_INIT: u8 = 0x01;

/// Well-known logical units (UPIU LUN field; bit 7 marks a W-LUN).
pub const WLUN_REPORT_LUNS: u8 = 0x81;
pub const WLUN_BOOT: u8 = 0xb0;
pub const WLUN_RPMB: u8 = 0xc4;
pub const WLUN_UFS_DEVICE: u8 = 0xd0;

fn be16(d: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([d[at], d[at + 1]])
}

fn be64(d: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[at..at + 8]);
    u64::from_be_bytes(b)
}

/// Checks bLength and bDescriptorIDN, and that the fields read are present.
fn check<'a>(d: &'a [u8], idn: u8, min: usize, what: &'static str) -> Result<&'a [u8], Error> {
    if d.len() < 2 || d[1] != idn {
        return Err(Error::BadData(what));
    }
    let len = (d[0] as usize).min(d.len());
    if len < min {
        return Err(Error::BadData(what));
    }
    Ok(&d[..len])
}

/// The device descriptor fields the driver uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDesc {
    /// Number of enabled logical units.
    pub num_lu: u8,
    pub num_wlu: u8,
    pub boot_enable: u8,
    /// UFS version, e.g. 0x0210 for 2.1, 0x0310 for 3.1.
    pub spec_version: u16,
    pub manufacturer_id: u16,
    /// String descriptor indexes.
    pub manufacturer_name: u8,
    pub product_name: u8,
    /// Offset of the unit descriptors in the configuration descriptor (unused here, recorded for the report).
    pub ud0_base_offset: u8,
}

impl DeviceDesc {
    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        let d = check(d, IDN_DEVICE, 0x1b, "device descriptor")?;
        Ok(Self {
            num_lu: d[0x06],
            num_wlu: d[0x07],
            boot_enable: d[0x08],
            spec_version: be16(d, 0x10),
            manufacturer_name: d[0x14],
            product_name: d[0x15],
            manufacturer_id: be16(d, 0x18),
            ud0_base_offset: d[0x1a],
        })
    }
}

/// The geometry descriptor fields the driver uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryDesc {
    /// Raw capacity in 512-byte units.
    pub total_raw_capacity: u64,
    /// Maximum number of logical units: 8 or 32.
    pub max_lu: u8,
}

impl GeometryDesc {
    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        let d = check(d, IDN_GEOMETRY, 0x0d, "geometry descriptor")?;
        // bMaxNumberLU: 0 = 8 units, 1 = 32 units.
        let max_lu = if d[0x0c] == 1 { 32 } else { 8 };
        Ok(Self {
            total_raw_capacity: be64(d, 0x04),
            max_lu,
        })
    }
}

/// bMemoryType "normal memory".
///
/// The unit descriptor of one logical unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnitDesc {
    pub unit_index: u8,
    /// bLUEnable: 1 enabled, 2 enabled for (H)PB, 0 disabled.
    pub enabled: u8,
    /// bBootLunID: 0 not bootable, 1 boot LU A, 2 boot LU B.
    pub boot_lun_id: u8,
    /// bLUWriteProtect: 0 none, 1 power-on write protect, 2 permanent.
    pub write_protect: u8,
    pub memory_type: u8,
    /// bDataReliability: whether the device says data survive a power failure during a write.
    pub data_reliability: u8,
    /// Block size is 2^log2_block_size bytes (0x0c = 4096).
    pub log2_block_size: u8,
    pub block_count: u64,
}

impl UnitDesc {
    pub fn parse(d: &[u8]) -> Result<Self, Error> {
        let d = check(d, IDN_UNIT, 0x13, "unit descriptor")?;
        Ok(Self {
            unit_index: d[0x02],
            enabled: d[0x03],
            boot_lun_id: d[0x04],
            write_protect: d[0x05],
            memory_type: d[0x08],
            data_reliability: d[0x09],
            log2_block_size: d[0x0a],
            block_count: be64(d, 0x0b),
        })
    }

    /// Block size in bytes, or `None` for a value no driver could use (outside 512 B - 64 KiB).
    pub fn block_size(&self) -> Option<u32> {
        (9..=16)
            .contains(&self.log2_block_size)
            .then(|| 1u32 << self.log2_block_size)
    }
}
