//! What an SD card reports (SD Physical Layer Simplified Specification; decoding as in Linux `mmc/core/sd.c`).

/// A 136-bit response without its CRC byte, most significant word first: `raw[0]` holds bits 127-96.
pub type Long = [u32; 4];

/// Bits `start..start + len` of a long response (`UNSTUFF_BITS`).
pub fn bits(raw: &Long, start: usize, len: usize) -> u32 {
    let mask = if len < 32 {
        (1u32 << len) - 1
    } else {
        u32::MAX
    };
    let off = 3 - start / 32;
    let shift = start % 32;
    let mut value = raw[off] >> shift;
    if len + shift > 32 {
        value |= raw[off - 1] << (32 - shift);
    }
    value & mask
}

pub const OCR_BUSY_DONE: u32 = 1 << 31;
pub const OCR_CCS: u32 = 1 << 30;
/// 2.7-3.6 V.
pub const OCR_VOLTAGES: u32 = 0x00ff_8000;
/// The argument of ACMD41's Host Capacity Support bit.
pub const OCR_HCS: u32 = 1 << 30;

/// R1 status bits that mean a command failed (Linux `CMD_ERRORS`).
pub const R1_ERRORS: u32 = 0xe538_0000;
pub const R1_READY_FOR_DATA: u32 = 1 << 8;
pub const R1_STATE_SHIFT: u32 = 9;
pub const R1_STATE_TRAN: u32 = 4;

/// The fields of the CSD the driver uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csd {
    /// Capacity in 512-byte sectors.
    pub sectors: u64,
    /// Command class 10 (switch function) is supported.
    pub class10: bool,
    pub write_protected: bool,
}

impl Csd {
    pub fn parse(raw: &Long) -> Option<Self> {
        let sectors = match bits(raw, 126, 2) {
            0 => {
                let c_size = bits(raw, 62, 12) as u64;
                let mult = bits(raw, 47, 3) as u64;
                let read_bl_len = bits(raw, 80, 4) as u64;
                if !(9..=11).contains(&read_bl_len) {
                    return None;
                }
                (c_size + 1) << (mult + 2 + read_bl_len - 9)
            }
            1 => (bits(raw, 48, 22) as u64 + 1) * 1024,
            // Version 3.0 (SDUC) and reserved values.
            _ => return None,
        };
        Some(Self {
            sectors,
            class10: bits(raw, 84, 12) & (1 << 10) != 0,
            write_protected: bits(raw, 12, 2) != 0,
        })
    }
}

/// The product name and serial from the CID, for the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cid {
    pub manufacturer: u8,
    pub name: [u8; 5],
    pub serial: u32,
}

impl Cid {
    pub fn parse(raw: &Long) -> Self {
        Self {
            manufacturer: bits(raw, 120, 8) as u8,
            name: [
                bits(raw, 96, 8) as u8,
                bits(raw, 88, 8) as u8,
                bits(raw, 80, 8) as u8,
                bits(raw, 72, 8) as u8,
                bits(raw, 64, 8) as u8,
            ],
            serial: bits(raw, 24, 32),
        }
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name).unwrap_or("?").trim_end()
    }
}

/// The fields of the 8-byte SCR.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scr {
    pub spec: u8,
    pub bus_4bit: bool,
}

impl Scr {
    pub fn parse(raw: &[u8; 8]) -> Self {
        Self {
            spec: raw[0] & 0xf,
            bus_4bit: raw[1] & 0x4 != 0,
        }
    }
}

/// CMD6 arguments: query or set function group 1 (access mode), function 1 (High Speed).
pub const SWITCH_CHECK_HS: u32 = 0x00ff_fff1;
pub const SWITCH_SET_HS: u32 = 0x80ff_fff1;
pub const SWITCH_STATUS_LEN: usize = 64;

/// Whether a CMD6 query says the card has High Speed.
pub fn switch_supports_hs(status: &[u8; SWITCH_STATUS_LEN]) -> bool {
    status[13] & 0x02 != 0
}

/// Whether a CMD6 set selected High Speed.
pub fn switch_selected_hs(status: &[u8; SWITCH_STATUS_LEN]) -> bool {
    status[16] & 0x0f == 1
}

/// A card that finished initialisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    pub rca: u16,
    /// Block addressed (SDHC, SDXC) rather than byte addressed.
    pub high_capacity: bool,
    pub sectors: u64,
    pub cid: Cid,
    pub csd: Csd,
    pub scr: Option<Scr>,
    pub bus_4bit: bool,
    pub high_speed: bool,
    pub clock_hz: u32,
    /// Writes are off for this card: the host is read-only, its switch says so, or its CSD does.
    pub read_only: bool,
}
