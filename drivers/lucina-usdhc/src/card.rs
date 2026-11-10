//! What an eMMC reports (decoding as in Linux `mmc/core/mmc.c`).

/// What CMD1 asks for: the card's answer to 2.7-3.6 V, as Linux, with sector addressing.
pub const OCR_WINDOW: u32 = 0x0030_0000;
pub const OCR_SECTOR_MODE: u32 = 1 << 30;
pub const OCR_READY: u32 = 1 << 31;

/// R1 status bits that mean a command failed: range, address, length, protection, lock, CRC, illegal command, ECC, card
/// error and CMD6's own.
pub const R1_ERRORS: u32 = 0xfdf8_0080;
pub const R1_STATE_SHIFT: u32 = 9;
pub const R1_STATE_TRAN: u32 = 4;
pub const R1_READY_FOR_DATA: u32 = 1 << 8;

pub const RCA: u16 = 1;

pub const EXT_CSD_SIZE: usize = 512;

pub mod ext_csd {
    pub const CACHE_CTRL: usize = 33;
    pub const RST_N_FUNCTION: usize = 162;
    pub const PARTITION_SUPPORT: usize = 160;
    pub const RPMB_SIZE_MULT: usize = 168;
    pub const PARTITION_CONFIG: usize = 179;
    pub const BUS_WIDTH: usize = 183;
    pub const HS_TIMING: usize = 185;
    pub const REV: usize = 192;
    pub const STRUCTURE: usize = 194;
    pub const CARD_TYPE: usize = 196;
    pub const SEC_COUNT: usize = 212;
    pub const SEC_FEATURE_SUPPORT: usize = 231;
    pub const BOOT_SIZE_MULT: usize = 226;
    pub const HC_ERASE_GRP_SIZE: usize = 224;
    pub const HC_WP_GRP_SIZE: usize = 221;
}

/// Bytes that only the card can change, so a read over a wider bus must give the same ones (`mmc_compare_ext_csds`).
const READ_ONLY_BYTES: [usize; 11] = [
    ext_csd::PARTITION_SUPPORT,
    ext_csd::REV,
    ext_csd::STRUCTURE,
    ext_csd::CARD_TYPE,
    ext_csd::SEC_COUNT,
    ext_csd::SEC_COUNT + 1,
    ext_csd::SEC_COUNT + 2,
    ext_csd::SEC_COUNT + 3,
    ext_csd::HC_WP_GRP_SIZE,
    ext_csd::HC_ERASE_GRP_SIZE,
    ext_csd::SEC_FEATURE_SUPPORT,
];

/// Whether two reads of EXT_CSD agree on what cannot have changed.
pub fn same_read_only_bytes(a: &[u8; EXT_CSD_SIZE], b: &[u8; EXT_CSD_SIZE]) -> bool {
    READ_ONLY_BYTES.iter().all(|&i| a[i] == b[i])
}

/// The product name and serial from the CID, for the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cid {
    pub manufacturer: u8,
    pub name: [u8; 6],
    pub serial: u32,
}

impl Cid {
    /// From the response registers, which hold bits 127-8 of it with the CRC byte dropped: `raw[3]` has bits 127-104 in its
    /// low 24 bits.
    pub fn parse(raw: &[u32; 4]) -> Self {
        let byte = |bit: usize| {
            // A byte whose lowest bit is `bit` of the 128.
            let at = bit - 8;
            ((raw[at / 32] >> (at % 32)) | if at % 32 > 24 { raw[at / 32 + 1] << (32 - at % 32) } else { 0 }) as u8
        };
        Self {
            manufacturer: byte(120),
            name: [byte(96), byte(88), byte(80), byte(72), byte(64), byte(56)],
            serial: u32::from_be_bytes([byte(40), byte(32), byte(24), byte(16)]),
        }
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name).unwrap_or("?").trim_end()
    }
}

/// A card that finished initialisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    pub cid: Cid,
    pub sectors: u64,
    pub ext_csd_rev: u8,
    pub card_type: u8,
    pub rst_n_function: u8,
    pub partition_config: u8,
    /// Size in KiB of each boot partition, and of RPMB.
    pub boot_kib: u32,
    pub rpmb_kib: u32,
    pub bus_bits: u8,
    pub high_speed: bool,
    pub clock_hz: u32,
    /// The card keeps what it is given in a cache before the flash. CMD0 leaves this as the kernel before set it.
    pub cache_on: bool,
}

impl Card {
    pub(crate) fn sectors_of(ext: &[u8; EXT_CSD_SIZE]) -> u64 {
        u32::from_le_bytes([
            ext[ext_csd::SEC_COUNT],
            ext[ext_csd::SEC_COUNT + 1],
            ext[ext_csd::SEC_COUNT + 2],
            ext[ext_csd::SEC_COUNT + 3],
        ]) as u64
    }
}
