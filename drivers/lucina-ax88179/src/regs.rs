//! AX88179 register map: the values Linux 7.1 `drivers/net/usb/ax88179_178a.c` uses. Only facts (addresses, bit
//! positions, the bulk-in table) are taken; the names follow Linux's.

/// Vendor request: MAC register access. `wValue` = register, `wIndex` = length, data = little-endian bytes.
pub const ACCESS_MAC: u8 = 0x01;
/// Vendor request: PHY (MII) register access. `wValue` = PHY address, `wIndex` = MII register, 2 data bytes.
pub const ACCESS_PHY: u8 = 0x02;
/// Vendor request: EEPROM word access. `wValue` = word address, `wIndex` = 1, 2 data bytes (Linux `AX_ACCESS_EEPROM`).
pub const ACCESS_EEPROM: u8 = 0x04;
/// Vendor request: eFuse read. `wValue` = 0, `wIndex` = length (Linux `AX_ACCESS_EFUS`).
pub const ACCESS_EFUS: u8 = 0x05;

/// MII address of the internal gigabit PHY.
pub const PHY_ID: u16 = 0x03;

// ---- MAC registers (ACCESS_MAC) ----

/// 1 byte: which USB speed the chip is attached at.
pub const PHYSICAL_LINK_STATUS: u8 = 0x02;
pub const USB_SS: u8 = 1 << 2;
pub const USB_HS: u8 = 1 << 1;

/// 1 byte: general status; bit 2 (`SECLD`) tells UA2 from UA1 (Linux `GENERAL_STATUS`).
pub const GENERAL_STATUS: u8 = 0x03;
pub const SECLD: u8 = 1 << 2;

/// EEPROM (SROM) access registers (Linux `AX_SROM_*`), used by `ax88179_check_eeprom` / `ax88179_led_setting`.
pub const SROM_ADDR: u8 = 0x07;
pub const SROM_DATA_LOW: u8 = 0x08;
pub const SROM_DATA_HIGH: u8 = 0x09;
pub const SROM_CMD: u8 = 0x0a;
pub const EEP_RD: u8 = 0x04;
pub const EEP_BUSY: u8 = 0x10;

/// 2 bytes: receive control.
pub const RX_CTL: u8 = 0x0b;
pub const RX_CTL_DROPCRCERR: u16 = 1 << 8;
pub const RX_CTL_IPE: u16 = 1 << 9;
pub const RX_CTL_START: u16 = 1 << 7;
pub const RX_CTL_AP: u16 = 1 << 5;
pub const RX_CTL_AM: u16 = 1 << 4;
pub const RX_CTL_AB: u16 = 1 << 3;
pub const RX_CTL_AMALL: u16 = 1 << 1;
pub const RX_CTL_PRO: u16 = 1 << 0;
/// Linux `AX_RX_CTL_STOP`: reception off.
pub const RX_CTL_STOP: u16 = 0x0000;
/// What Linux `ax88179_reset` writes to start reception: 0x03aa.
pub const RX_CTL_RUN: u16 =
    RX_CTL_DROPCRCERR | RX_CTL_IPE | RX_CTL_START | RX_CTL_AP | RX_CTL_AMALL | RX_CTL_AB;

/// 6 bytes: the station (MAC) address.
pub const NODE_ID: u8 = 0x10;

/// 2 bytes: medium status/mode.
pub const MEDIUM_STATUS_MODE: u8 = 0x22;
pub const MEDIUM_GIGAMODE: u16 = 1 << 0;
pub const MEDIUM_FULL_DUPLEX: u16 = 1 << 1;
pub const MEDIUM_EN_125MHZ: u16 = 1 << 3;
pub const MEDIUM_RXFLOW_CTRLEN: u16 = 1 << 4;
pub const MEDIUM_TXFLOW_CTRLEN: u16 = 1 << 5;
pub const MEDIUM_RECEIVE_EN: u16 = 1 << 8;
pub const MEDIUM_PS: u16 = 1 << 9;
pub const MEDIUM_JUMBO_EN: u16 = 0x8040;
/// The probe-time default Linux `ax88179_reset` writes before the link is known (gigabit full duplex, no jumbo):
/// 0x0133.
pub const MEDIUM_DEFAULT: u16 = MEDIUM_RECEIVE_EN
    | MEDIUM_TXFLOW_CTRLEN
    | MEDIUM_RXFLOW_CTRLEN
    | MEDIUM_FULL_DUPLEX
    | MEDIUM_GIGAMODE;

/// 1 byte: monitor (wake-on-LAN) mode.
pub const MONITOR_MOD: u8 = 0x24;
pub const MONITOR_MODE_RWLC: u8 = 1 << 1;
pub const MONITOR_MODE_RWMP: u8 = 1 << 2;
pub const MONITOR_MODE_PMEPOL: u8 = 1 << 5;
pub const MONITOR_MODE_PMETYPE: u8 = 1 << 6;

/// 1 byte: GPIO control (Linux `AX_GPIO_CTRL`); `ax88179_led_setting` enables all three GPIOs on UA1 chips.
pub const GPIO_CTRL: u8 = 0x25;
pub const GPIO_CTRL_GPIO3EN: u8 = 0x80;
pub const GPIO_CTRL_GPIO2EN: u8 = 0x40;
pub const GPIO_CTRL_GPIO1EN: u8 = 0x20;

/// 2 bytes: PHY power and reset control.
pub const PHYPWR_RSTCTL: u8 = 0x26;
pub const PHYPWR_RSTCTL_BZ: u16 = 1 << 4;
pub const PHYPWR_RSTCTL_IPRL: u16 = 1 << 5;
/// Auto-detach (Linux `AX_PHYPWR_RSTCTL_AT`), set by `ax88179_auto_detach`.
pub const PHYPWR_RSTCTL_AT: u16 = 1 << 12;

/// 5 bytes: bulk-in queue control (`ctrl, timer_l, timer_h, size, ifg`).
pub const RX_BULKIN_QCTRL: u8 = 0x2e;

/// 1 byte: clock selection.
pub const CLK_SELECT: u8 = 0x33;
pub const CLK_SELECT_BCS: u8 = 1 << 0;
pub const CLK_SELECT_ACS: u8 = 1 << 1;
pub const CLK_SELECT_ULR: u8 = 1 << 3;

/// 1 byte: receive checksum offload.
pub const RXCOE_CTL: u8 = 0x34;
/// 1 byte: transmit checksum offload.
pub const TXCOE_CTL: u8 = 0x35;
pub const COE_IP: u8 = 1 << 0;
pub const COE_TCP: u8 = 1 << 1;
pub const COE_UDP: u8 = 1 << 2;
pub const COE_TCPV6: u8 = 1 << 5;
pub const COE_UDPV6: u8 = 1 << 6;
/// IPv4, TCP, UDP, TCPv6 and UDPv6 (0x67), for both directions.
pub const COE_ALL: u8 = COE_IP | COE_TCP | COE_UDP | COE_TCPV6 | COE_UDPV6;

/// 1 byte each: flow-control pause watermarks.
pub const PAUSE_WATERLVL_HIGH: u8 = 0x54;
pub const PAUSE_WATERLVL_LOW: u8 = 0x55;

/// 1 byte: LED full-duplex/USB3 mode (Linux `AX_LEDCTRL`), written at the end of `ax88179_led_setting`.
pub const LEDCTRL: u8 = 0x73;

// ---- PHY registers (ACCESS_PHY), IEEE 802.3 clause 22 plus the Realtek-style vendor status ----

pub const MII_BMCR: u8 = 0x00;
pub const MII_BMSR: u8 = 0x01;
/// MMD (clause 45) access registers, used by `ax88179_phy_mmd_indirect`.
pub const MII_MMD_CTRL: u8 = 0x0d;
pub const MII_MMD_DATA: u8 = 0x0e;
pub const MII_MMD_CTRL_NOINCR: u16 = 0x4000;
/// PHY address register inside the PHY (Linux `MII_PHYADDR`), written by `ax88179_disable_eee`.
pub const MII_PHYADDR: u8 = 0x19;

pub const BMCR_ANENABLE: u16 = 0x1000;
pub const BMCR_ANRESTART: u16 = 0x0200;

pub const BMSR_LSTATUS: u16 = 0x0004;

/// PHY-specific status: resolved speed, duplex and link.
pub const GMII_PHY_PHYSR: u8 = 0x11;
pub const PHYSR_SMASK: u16 = 0xc000;
pub const PHYSR_GIGA: u16 = 1 << 15;
pub const PHYSR_100: u16 = 1 << 14;
pub const PHYSR_FULL: u16 = 1 << 13;
pub const PHYSR_LINK: u16 = 1 << 10;

/// PHY page select (Linux `GMII_PHY_PAGE_SELECT`) and page register (`GMII_PHYPAGE`).
pub const GMII_PHY_PAGE_SELECT: u8 = 0x1f;
pub const GMII_PHYPAGE: u8 = 0x1e;
pub const GMII_PHY_PGSEL_EXT: u16 = 0x0007;
pub const GMII_PHY_PGSEL_PAGE0: u16 = 0x0000;
pub const GMII_PHY_PGSEL_PAGE3: u16 = 0x0003;

/// LED control registers on the extended page (Linux `GMII_LED_ACT` / `GMII_LED_LINK`), written by
/// `ax88179_led_setting`.
pub const GMII_LED_ACT: u8 = 0x1a;
pub const GMII_LED_ACTIVE_MASK: u16 = 0xff8f;
pub const GMII_LED0_ACTIVE: u16 = 1 << 4;
pub const GMII_LED1_ACTIVE: u16 = 1 << 5;
pub const GMII_LED2_ACTIVE: u16 = 1 << 6;
pub const GMII_LED_LINK: u8 = 0x1c;
pub const GMII_LED_LINK_MASK: u16 = 0xf888;
pub const GMII_LED0_LINK_10: u16 = 1 << 0;
pub const GMII_LED0_LINK_100: u16 = 1 << 1;
pub const GMII_LED0_LINK_1000: u16 = 1 << 2;
pub const GMII_LED1_LINK_10: u16 = 1 << 4;
pub const GMII_LED1_LINK_100: u16 = 1 << 5;
pub const GMII_LED1_LINK_1000: u16 = 1 << 6;
pub const GMII_LED2_LINK_10: u16 = 1 << 8;
pub const GMII_LED2_LINK_100: u16 = 1 << 9;
pub const GMII_LED2_LINK_1000: u16 = 1 << 10;

/// LED mode bits of the 16-bit LED value read from EEPROM/eFuse (Linux `LED*`).
pub const LED0_ACTIVE: u16 = 1 << 0;
pub const LED0_LINK_10: u16 = 1 << 1;
pub const LED0_LINK_100: u16 = 1 << 2;
pub const LED0_LINK_1000: u16 = 1 << 3;
pub const LED0_FD: u16 = 1 << 4;
pub const LED0_USB3_MASK: u16 = 0x001f;
pub const LED1_ACTIVE: u16 = 1 << 5;
pub const LED1_LINK_10: u16 = 1 << 6;
pub const LED1_LINK_100: u16 = 1 << 7;
pub const LED1_LINK_1000: u16 = 1 << 8;
pub const LED1_FD: u16 = 1 << 9;
pub const LED1_USB3_MASK: u16 = 0x03e0;
pub const LED2_ACTIVE: u16 = 1 << 10;
pub const LED2_LINK_10: u16 = 1 << 11;
pub const LED2_LINK_100: u16 = 1 << 12;
pub const LED2_LINK_1000: u16 = 1 << 13;
pub const LED2_FD: u16 = 1 << 14;
pub const LED2_USB3_MASK: u16 = 0x7c00;
pub const LED_VALID: u16 = 1 << 15;

/// MMD device addresses (Linux `MDIO_MMD_AN`) and the EEE advertisement register (`MDIO_AN_EEE_ADV`), written by
/// `ax88179_ethtool_set_eee`.
pub const MDIO_MMD_AN: u16 = 7;
pub const MDIO_AN_EEE_ADV: u16 = 60;

// ---- Bulk-in queue settings ----

/// Linux's `AX88179_BULKIN_SIZE` table, written to [`RX_BULKIN_QCTRL`].
pub const BULKIN_SUPER_GIGA: [u8; 5] = [0x07, 0x4f, 0x00, 0x12, 0xff];
pub const BULKIN_HIGH_GIGA: [u8; 5] = [0x07, 0x20, 0x03, 0x16, 0xff];
pub const BULKIN_FAST_100: [u8; 5] = [0x07, 0xae, 0x07, 0x18, 0xff];
pub const BULKIN_SLOW: [u8; 5] = [0x07, 0xcc, 0x4c, 0x18, 0x08];

/// Bytes the host asks for per bulk-in transfer for a queue setting: `1024 * (size + 2)` (Linux `ax88179_reset` /
/// `ax88179_link_reset` set `rx_urb_size` this way).
pub const fn rx_transfer_len(qctrl: &[u8; 5]) -> usize {
    1024 * (qctrl[3] as usize + 2)
}

// ---- RX per-packet header bits ----

pub const RXHDR_DROP_ERR: u32 = 1 << 31;
pub const RXHDR_CRC_ERR: u32 = 1 << 29;
pub const RXHDR_L3CSUM_ERR: u32 = 1 << 1;
pub const RXHDR_L4CSUM_ERR: u32 = 1 << 0;

/// TX header word 2 flag Linux `ax88179_tx_fixup` sets when `frame + 8` is a multiple of the bulk-out max packet
/// size. (usbnet additionally appends a zero-length packet on such boundaries — `FLAG_SEND_ZLP` is not set for this
/// device — but the chip only needs this flag; our transport sends no ZLP.)
pub const TX_HDR2_PADDING: u32 = 0x8000_8000;
