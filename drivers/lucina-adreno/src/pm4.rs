// SPDX-License-Identifier: GPL-2.0-only
//! PM4 packet headers (`adreno_pm4.xml.h`, `a6xx_gpu.h` of Linux).

pub const CP_WAIT_MEM_WRITES: u8 = 0x12;
pub const CP_WAIT_FOR_ME: u8 = 0x13;
pub const CP_WAIT_FOR_IDLE: u8 = 0x26;
pub const CP_BLIT: u8 = 0x2c;
pub const CP_MEM_WRITE: u8 = 0x3d;
pub const CP_INDIRECT_BUFFER: u8 = 0x3f;
pub const CP_EVENT_WRITE: u8 = 0x46;
pub const CP_ME_INIT: u8 = 0x48;
pub const CP_SET_MARKER: u8 = 0x65;
pub const CP_SET_SECURE_MODE: u8 = 0x66;
pub const CP_NOP: u8 = 0x10;
pub const CP_WAIT_REG_MEM: u8 = 0x3c;
pub const CP_REG_TO_MEM: u8 = 0x3e;
pub const CP_SMMU_TABLE_UPDATE: u8 = 0x53;
pub const CP_SET_PROTECTED_MODE: u8 = 0x5f;

/// `vgt_event_type` of `adreno_pm4.xml` for the A6xx.
pub const EVENT_CACHE_FLUSH_TS: u32 = 4;
pub const EVENT_PC_CCU_INVALIDATE_DEPTH: u32 = 24;
pub const EVENT_PC_CCU_INVALIDATE_COLOR: u32 = 25;
pub const EVENT_CACHE_INVALIDATE: u32 = 49;

/// `PM4_PARITY`: odd parity of a value.
pub const fn parity(value: u32) -> u32 {
    let mut v = value;
    v ^= v >> 16;
    v ^= v >> 8;
    v ^= v >> 4;
    v &= 0xf;
    (!0x6996u32 >> v) & 1
}

/// A type 4 packet: `count` registers from `reg`.
pub const fn pkt4(reg: u32, count: u32) -> u32 {
    (4 << 28) | count | (parity(count) << 7) | ((reg & 0x3ffff) << 8) | (parity(reg) << 27)
}

/// A type 7 packet: `count` words for `opcode`.
pub const fn pkt7(opcode: u8, count: u32) -> u32 {
    let op = opcode as u32;
    (7 << 28) | count | (parity(count) << 15) | ((op & 0x7f) << 16) | (parity(op) << 23)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Worked by hand from Linux's `PKT7` and `PKT4`: both parity bits of `CP_ME_INIT`, and the fields of a type 4 header.
    #[test]
    fn headers_match_linux() {
        assert_eq!(pkt7(CP_ME_INIT, 8), 0x70c8_0008);
        assert_eq!(parity(0), 1);
        assert_eq!(parity(1), 0);
        assert_eq!(pkt4(0x885, 1) & 0x7f, 1);
        assert_eq!((pkt4(0x885, 1) >> 8) & 0x3ffff, 0x885);
    }
}
