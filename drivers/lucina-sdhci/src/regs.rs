//! SDHCI registers and bits (SD Host Controller Simplified Specification; names as in Linux `sdhci.h`).

pub const BLOCK_SIZE: usize = 0x04;
pub const BLOCK_COUNT: usize = 0x06;
pub const ARGUMENT: usize = 0x08;
pub const TRANSFER_MODE: usize = 0x0c;
pub const COMMAND: usize = 0x0e;
pub const RESPONSE: usize = 0x10;
pub const PRESENT_STATE: usize = 0x24;
pub const HOST_CONTROL: usize = 0x28;
pub const POWER_CONTROL: usize = 0x29;
pub const CLOCK_CONTROL: usize = 0x2c;
pub const TIMEOUT_CONTROL: usize = 0x2e;
pub const SOFTWARE_RESET: usize = 0x2f;
pub const INT_STATUS: usize = 0x30;
pub const INT_ENABLE: usize = 0x34;
pub const SIGNAL_ENABLE: usize = 0x38;
pub const HOST_CONTROL2: usize = 0x3e;
pub const CAPABILITIES: usize = 0x40;
pub const ADMA_ERROR: usize = 0x54;
pub const ADMA_ADDRESS: usize = 0x58;
pub const ADMA_ADDRESS_HI: usize = 0x5c;
pub const HOST_VERSION: usize = 0xfe;

pub const TRNS_DMA: u16 = 1 << 0;
pub const TRNS_BLK_CNT_EN: u16 = 1 << 1;
pub const TRNS_AUTO_CMD12: u16 = 1 << 2;
pub const TRNS_READ: u16 = 1 << 4;
pub const TRNS_MULTI: u16 = 1 << 5;

pub const CMD_RESP_NONE: u16 = 0;
pub const CMD_RESP_LONG: u16 = 1;
pub const CMD_RESP_SHORT: u16 = 2;
pub const CMD_RESP_SHORT_BUSY: u16 = 3;
pub const CMD_CRC: u16 = 1 << 3;
pub const CMD_INDEX: u16 = 1 << 4;
pub const CMD_DATA: u16 = 1 << 5;

pub const PS_CMD_INHIBIT: u32 = 1 << 0;
pub const PS_DATA_INHIBIT: u32 = 1 << 1;
pub const PS_CARD_INSERTED: u32 = 1 << 16;
/// Set while the card's write-protect switch allows writing.
pub const PS_WRITE_ENABLED: u32 = 1 << 19;

pub const CTRL_4BIT: u8 = 1 << 1;
pub const CTRL_HISPD: u8 = 1 << 2;
pub const CTRL_DMA_MASK: u8 = 3 << 3;
pub const CTRL_ADMA32: u8 = 2 << 3;
pub const CTRL_ADMA64: u8 = 3 << 3;

pub const POWER_ON: u8 = 1 << 0;
pub const POWER_180: u8 = 5 << 1;
pub const POWER_300: u8 = 6 << 1;
pub const POWER_330: u8 = 7 << 1;

pub const CLOCK_INT_EN: u16 = 1 << 0;
pub const CLOCK_INT_STABLE: u16 = 1 << 1;
pub const CLOCK_CARD_EN: u16 = 1 << 2;

pub const RESET_ALL: u8 = 1 << 0;
pub const RESET_CMD: u8 = 1 << 1;
pub const RESET_DATA: u8 = 1 << 2;

pub const INT_RESPONSE: u32 = 1 << 0;
pub const INT_DATA_END: u32 = 1 << 1;
pub const INT_ERROR: u32 = 1 << 15;
pub const INT_ERROR_MASK: u32 = 0xffff_0000;
/// Everything the driver polls: the normal statuses and all errors. Nothing is signalled to the CPU.
pub const INT_ENABLE_ALL: u32 = 0xffff_00ff & !(1 << 6) & !(1 << 7) & !(1 << 8);

pub const CAN_ADMA2: u32 = 1 << 19;
pub const CAN_VDD_330: u32 = 1 << 24;
pub const CAN_VDD_300: u32 = 1 << 25;
pub const CAN_64BIT: u32 = 1 << 28;
pub const CAN_8BIT: u32 = 1 << 18;

pub const ADMA_ATTR_VALID: u16 = 1 << 0;
pub const ADMA_ATTR_END: u16 = 1 << 1;
pub const ADMA_ATTR_TRAN: u16 = 2 << 4;

/// The value of the Clock Control divider fields for `target_hz`, and the card clock that gives.
///
/// Version 3.00 hosts divide by 2N with a 10-bit N (N = 0: no division), earlier ones by a power of two in 8 bits
/// (Linux `sdhci_calc_clk`).
pub fn clock_divider(base_hz: u32, target_hz: u32, spec_version: u8) -> (u16, u32) {
    if target_hz == 0 || base_hz == 0 {
        return (0, 0);
    }
    if base_hz <= target_hz {
        return (0, base_hz);
    }
    let divisor = if spec_version >= 2 {
        let mut n = base_hz.div_ceil(2 * target_hz);
        if n > 1023 {
            n = 1023;
        }
        n
    } else {
        let mut d = 1u32;
        while d < 128 && base_hz / (2 * d) > target_hz {
            d *= 2;
        }
        d
    };
    let field = ((divisor & 0xff) << 8) | ((divisor >> 8) & 3) << 6;
    (field as u16, base_hz / (2 * divisor))
}
