//! Register addresses and bits, named as in U-Boot's `r8152.h` (facts of the chip's interface, the same in OpenBSD's
//! `if_urereg.h`). Only what the RTL8152
//! path uses; the RTL8153 registers come with that path.

/// `bRequest` of every register access.
pub const REQ_REGS: u8 = 0x05;

/// Which of the chip's two register spaces an access goes to (`wIndex` bits 15:8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mcu {
    /// The MAC ("packet layer").
    Pla,
    /// The USB side.
    Usb,
}

impl Mcu {
    pub const fn index(self) -> u16 {
        match self {
            Mcu::Pla => 0x0100,
            Mcu::Usb => 0x0000,
        }
    }
}

// Byte enables in `wIndex` bits 7:0.
pub const BYTE_EN_DWORD: u16 = 0xff;
pub const BYTE_EN_WORD: u16 = 0x33;
pub const BYTE_EN_BYTE: u16 = 0x11;
pub const BYTE_EN_SIX_BYTES: u16 = 0x3f;
pub const BYTE_EN_START_MASK: u16 = 0x0f;
pub const BYTE_EN_END_MASK: u16 = 0xf0;

// PLA registers.
pub const PLA_IDR: u16 = 0xc000;
pub const PLA_RCR: u16 = 0xc010;
pub const PLA_RMS: u16 = 0xc016;
pub const PLA_RXFIFO_CTRL0: u16 = 0xc0a0;
pub const PLA_RXFIFO_CTRL1: u16 = 0xc0a4;
pub const PLA_RXFIFO_CTRL2: u16 = 0xc0a8;
pub const PLA_FMC: u16 = 0xc0b4;
pub const PLA_TEREDO_CFG: u16 = 0xc0bc;
pub const PLA_TEREDO_TIMER: u16 = 0xd2cc;
pub const PLA_REALWOW_TIMER: u16 = 0xd2e8;
pub const PLA_WDT6_CTRL: u16 = 0xe428;
pub const PLA_MAR: u16 = 0xcd00;
pub const PLA_BACKUP: u16 = 0xd000;
pub const PLA_LED_FEATURE: u16 = 0xdd92;
pub const PLA_GPHY_INTR_IMR: u16 = 0xe022;
pub const PLA_EEEP_CR: u16 = 0xe080;
pub const PLA_MAC_PWR_CTRL: u16 = 0xe0c0;
pub const PLA_TCR0: u16 = 0xe610;
pub const PLA_TCR1: u16 = 0xe612;
pub const PLA_TXFIFO_CTRL: u16 = 0xe618;
pub const PLA_RSTTALLY: u16 = 0xe800;
pub const BIST_CTRL: u16 = 0xe810;
pub const PLA_CR: u16 = 0xe813;
pub const PLA_CRWECR: u16 = 0xe81c;
pub const PLA_PHY_PWR: u16 = 0xe84c;
pub const PLA_OOB_CTRL: u16 = 0xe84f;
pub const PLA_MISC_1: u16 = 0xe85a;
pub const PLA_OCP_GPHY_BASE: u16 = 0xe86c;
pub const PLA_SFF_STS_7: u16 = 0xe8de;
pub const PLA_PHYSTATUS: u16 = 0xe908;

// USB registers.
pub const USB_USB_CTRL: u16 = 0xd406;
pub const USB_TX_AGG: u16 = 0xd40a;
pub const USB_RX_BUF_TH: u16 = 0xd40c;
pub const USB_PM_CTRL_STATUS: u16 = 0xd432;
pub const USB_TX_DMA: u16 = 0xd434;
pub const USB_UPS_CTRL: u16 = 0xd800;

// PHY registers (OCP addresses, reached through PLA_OCP_GPHY_BASE and the window at 0xb000).
pub const OCP_ALDPS_CONFIG: u16 = 0x2010;
pub const OCP_BASE_MII: u16 = 0xa400;
pub const OCP_WINDOW: u16 = 0xb000;

// PLA_RCR.
pub const RCR_AAP: u32 = 0x0000_0001;
pub const RCR_APM: u32 = 0x0000_0002;
pub const RCR_AM: u32 = 0x0000_0004;
pub const RCR_AB: u32 = 0x0000_0008;
pub const RCR_ACPT_ALL: u32 = RCR_AAP | RCR_APM | RCR_AM | RCR_AB;

// FIFO thresholds.
pub const RXFIFO_THR1_NORMAL: u32 = 0x0008_0002;
pub const RXFIFO_THR2_FULL: u32 = 0x0000_0060;
pub const RXFIFO_THR2_HIGH: u32 = 0x0000_0038;
pub const RXFIFO_THR3_FULL: u32 = 0x0000_0078;
pub const RXFIFO_THR3_HIGH: u32 = 0x0000_0048;
/// Linux's value for the RTL8152 (U-Boot and OpenBSD have 0x0040_0008).
pub const TXFIFO_THR_NORMAL2: u32 = 0x0100_0008;

pub const FMC_FCR_MCU_EN: u16 = 0x0001;
pub const EEEP_CR_EEEP_TX: u16 = 0x0002;
pub const TCR0_AUTO_FIFO: u16 = 0x0080;
/// PLA_TCR1: the bits that identify the chip.
pub const VERSION_MASK: u16 = 0x7cf0;
pub const TALLY_RESET: u16 = 0x0001;
pub const PLA_CR_RE: u8 = 0x08;
pub const PLA_CR_TE: u8 = 0x04;
pub const BIST_CTRL_SW_RESET: u32 = 0x10 << 24;
pub const CRWECR_NORMAL: u8 = 0x00;
pub const CRWECR_CONFIG: u8 = 0xc0;
pub const NOW_IS_OOB: u8 = 0x80;
pub const LINK_LIST_READY: u8 = 0x02;
/// PLA_PHY_PWR read as a dword: its top byte is PLA_OOB_CTRL.
pub const PLA_PHY_PWR_LLR: u32 = (LINK_LIST_READY as u32) << 24;
pub const TX_10M_IDLE_EN: u16 = 0x0080;
pub const PFM_PWM_SWITCH: u16 = 0x0040;
pub const RXDY_GATED_EN: u16 = 0x0008;
pub const RE_INIT_LL: u16 = 0x8000;
pub const MCU_BORW_EN: u16 = 0x4000;
pub const TEREDO_SEL: u16 = 0x8000;
pub const TEREDO_RS_EVENT_MASK: u16 = 0x00fe;
pub const OOB_TEREDO_EN: u16 = 0x0001;
pub const WDT6_SET_MODE: u16 = 0x0010;
pub const LED_MODE_MASK: u16 = 0x0700;
pub const D3_CLK_GATED_EN: u32 = 0x0000_4000;
pub const MCU_CLK_RATIO: u32 = 0x0701_0f07;
pub const MCU_CLK_RATIO_MASK: u32 = 0x0f0f_0f0f;
pub const GPHY_STS_MSK: u16 = 0x0001;
pub const SPEED_DOWN_MSK: u16 = 0x0002;
pub const SPDWN_RXDV_MSK: u16 = 0x0004;
pub const SPDWN_LINKCHG_MSK: u16 = 0x0008;
pub const TX_AGG_MAX_THRESHOLD: u8 = 0x03;
pub const RX_THR_HIGH: u32 = 0x7a12_0180;
pub const TEST_MODE_DISABLE: u32 = 0x0000_0001;
pub const TX_SIZE_ADJUST1: u32 = 0x0000_0100;
pub const POWER_CUT: u16 = 0x0100;
pub const RESUME_INDICATE: u16 = 0x0001;
pub const RX_AGG_DISABLE: u16 = 0x0010;
pub const RX_ZERO_EN: u16 = 0x0080;

// OCP_ALDPS_CONFIG.
pub const ENPWRSAVE: u16 = 0x8000;
pub const ENPDNPS: u16 = 0x0200;
pub const LINKENA: u16 = 0x0100;
pub const DIS_SDSAVE: u16 = 0x0010;

// PLA_PHYSTATUS.
pub const STATUS_1000: u8 = 0x10;
pub const STATUS_100: u8 = 0x08;
pub const STATUS_10: u8 = 0x04;
pub const STATUS_LINK: u8 = 0x02;
pub const STATUS_FULL_DUP: u8 = 0x01;

// Generic MII registers and bits.
pub const MII_BMCR: u16 = 0x00;
pub const MII_ADVERTISE: u16 = 0x04;
pub const BMCR_RESET: u16 = 0x8000;
pub const BMCR_ANENABLE: u16 = 0x1000;
pub const BMCR_PDOWN: u16 = 0x0800;
pub const BMCR_ANRESTART: u16 = 0x0200;
pub const ADVERTISE_10HALF: u16 = 0x0020;
pub const ADVERTISE_10FULL: u16 = 0x0040;
pub const ADVERTISE_100HALF: u16 = 0x0080;
pub const ADVERTISE_100FULL: u16 = 0x0100;
pub const ADVERTISE_PAUSE_CAP: u16 = 0x0400;
pub const ADVERTISE_PAUSE_ASYM: u16 = 0x0800;

/// Frame, VLAN tag and FCS (Linux `RTL8152_RMS`, OpenBSD; U-Boot leaves the VLAN tag out).
pub const RMS: u16 = 1514 + 4 + 4;
