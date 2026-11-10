//! uSDHC registers and bits (i.MX6SoloLite). Everything here is accessed 32 bits at a time: a byte written to what SDHCI
//! calls the timeout register, for one, also clears the bit that holds the eMMC's reset line.

pub const DS_ADDR: usize = 0x00;
/// BLKSIZE in bits 12:0, BLKCNT in bits 31:16.
pub const BLK_ATT: usize = 0x04;
pub const CMD_ARG: usize = 0x08;
pub const CMD_XFR_TYP: usize = 0x0c;
pub const CMD_RSP0: usize = 0x10;
pub const PRES_STATE: usize = 0x24;
pub const PROT_CTRL: usize = 0x28;
pub const SYS_CTRL: usize = 0x2c;
pub const INT_STATUS: usize = 0x30;
pub const INT_STATUS_EN: usize = 0x34;
pub const INT_SIGNAL_EN: usize = 0x38;
pub const AUTOCMD12_ERR: usize = 0x3c;
pub const HOST_CTRL_CAP: usize = 0x40;
pub const WTMK_LVL: usize = 0x44;
pub const MIX_CTRL: usize = 0x48;
pub const DLL_CTRL: usize = 0x60;
pub const TUNE_CTRL_STATUS: usize = 0x68;
pub const VEND_SPEC: usize = 0xc0;
pub const TUNING_CTRL: usize = 0xcc;

// CMD_XFR_TYP
pub const CMD_INDEX_SHIFT: u32 = 24;
pub const CMD_TYPE_ABORT: u32 = 3 << 22;
pub const CMD_DATA: u32 = 1 << 21;
pub const CMD_INDEX_CHECK: u32 = 1 << 20;
pub const CMD_CRC_CHECK: u32 = 1 << 19;
pub const CMD_RSP_LONG: u32 = 1 << 16;
pub const CMD_RSP_SHORT: u32 = 2 << 16;
pub const CMD_RSP_SHORT_BUSY: u32 = 3 << 16;

// PRES_STATE
pub const PS_CMD_INHIBIT: u32 = 1 << 0;
pub const PS_DATA_INHIBIT: u32 = 1 << 1;
pub const PS_CLOCK_STABLE: u32 = 1 << 3;
pub const PS_CLOCK_OFF: u32 = 1 << 7;
/// The level of DAT0, which the card holds low while it is busy.
pub const PS_DAT0: u32 = 1 << 24;

// PROT_CTRL
pub const PROT_DTW_MASK: u32 = 3 << 1;
pub const PROT_DTW_4: u32 = 1 << 1;
pub const PROT_DTW_8: u32 = 2 << 1;
pub const PROT_D3CD: u32 = 1 << 3;
pub const PROT_EMODE_MASK: u32 = 3 << 4;
pub const PROT_EMODE_LE: u32 = 2 << 4;
/// 00 is SDMA.
pub const PROT_DMASEL_MASK: u32 = 3 << 8;
pub const PROT_BURST_LEN_INCR: u32 = 1 << 27;

// SYS_CTRL
pub const SYS_CLOCKS_ON: u32 = 7;
pub const SYS_DVS_SHIFT: u32 = 4;
pub const SYS_SDCLKFS_SHIFT: u32 = 8;
pub const SYS_DIVIDERS: u32 = 0xfff0;
pub const SYS_DTOCV_MASK: u32 = 0xf << 16;
pub const SYS_DTOCV_SHIFT: u32 = 16;
/// Active low: clearing it pulls the eMMC's RST_n down. Nothing here does.
pub const SYS_IPP_RST_N: u32 = 1 << 23;
pub const SYS_RSTA: u32 = 1 << 24;
pub const SYS_RSTC: u32 = 1 << 25;
pub const SYS_RSTD: u32 = 1 << 26;

// INT_STATUS, INT_STATUS_EN
pub const INT_CMD_DONE: u32 = 1 << 0;
pub const INT_DATA_DONE: u32 = 1 << 1;
pub const INT_DMA: u32 = 1 << 3;
/// Command time-out, CRC, end bit and index; data time-out, CRC and end bit; Auto CMD12; DMA.
pub const INT_ERRORS: u32 = 0x117f_0000;
/// Those of the command alone.
pub const INT_CMD_ERRORS: u32 = 0x000f_0000;
pub const INT_USED: u32 = INT_CMD_DONE | INT_DATA_DONE | INT_DMA | INT_ERRORS;

// MIX_CTRL
pub const MIX_DMA: u32 = 1 << 0;
pub const MIX_BLOCK_COUNT: u32 = 1 << 1;
pub const MIX_READ: u32 = 1 << 4;
pub const MIX_MULTI: u32 = 1 << 5;
/// The bits a transfer sets. Linux's mask: DDR_EN and the nibble position are not among them.
pub const MIX_TRANSFER: u32 = 0xb7;
/// What a full reset leaves alone in Linux: the tuning bits.
pub const MIX_TUNING: u32 = 0x03c0_0000;

// VEND_SPEC
pub const VEND_FORCE_CLOCK_ON: u32 = 1 << 8;

// TUNING_CTRL
pub const TUNING_STANDARD: u32 = 1 << 24;

/// Linux's watermarks for this chip: 64 words each way, in bursts of 16.
pub const WTMK_VALUE: u32 = 0x1040_1040;

/// A data time-out of 2^27 card clocks.
pub const DTOCV_VALUE: u32 = 0xe;

/// The SDCLKFS and DVS fields for a clock of at most `target_hz`, and the clock that gives. Amazon's arithmetic for this
/// chip, where the prescaler starts at 2 (`sdhci-esdhc.h`).
pub fn clock_fields(base_hz: u32, target_hz: u32) -> (u32, u32) {
    let mut pre = 2u32;
    while base_hz / (16 * pre) > target_hz && pre < 256 {
        pre *= 2;
    }
    let mut div = 1u32;
    while base_hz / (div * pre) > target_hz && div < 16 {
        div += 1;
    }
    (
        ((pre >> 1) << SYS_SDCLKFS_SHIFT) | ((div - 1) << SYS_DVS_SHIFT),
        base_hz / (div * pre),
    )
}
