//! The driver against a mock AX88179. The golden sequences are Linux 7.1 `drivers/net/usb/ax88179_178a.c`
//! (`ax88179_reset`, `ax88179_auto_detach`, `ax88179_get_mac_addr`, `ax88179_led_setting`, `ax88179_disable_eee`,
//! `ax88179_link_reset`, `ax88179_stop`, `ax88179_unbind`) read call by call, in the terms of the mock's op log
//! (`cmd value index size` per control transfer, plus every delay).

extern crate std;
use std::collections::VecDeque;
use std::vec::Vec;

use super::*;
use crate::frame::tests::{batch, frame};

/// The owner's adapter.
const OWNER_MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x00, 0x00, 0x02];

/// The LED mode the mock's EEPROM word 0x42 carries: the default set with `LED_VALID`.
const MOCK_LEDVALUE: u16 = 0xbdc1;
/// `ledact`/`ledlink` the mock's LED mode produces (PHY regs start at 0).
const MOCK_LEDACT: u16 = GMII_LED0_ACTIVE | GMII_LED2_ACTIVE;
const MOCK_LEDLINK: u16 = GMII_LED1_LINK_10
    | GMII_LED1_LINK_100
    | GMII_LED1_LINK_1000
    | GMII_LED2_LINK_10
    | GMII_LED2_LINK_100
    | GMII_LED2_LINK_1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    /// Control OUT: `bRequest`, `wValue`, `wIndex`, data.
    W(u8, u16, u16, Vec<u8>),
    /// Control IN: `bRequest`, `wValue`, `wIndex`, length.
    R(u8, u16, u16, usize),
    Delay(u32),
    BulkOut(Vec<u8>),
}

fn mac_w(reg: u8, data: &[u8]) -> Op {
    Op::W(ACCESS_MAC, reg as u16, data.len() as u16, data.to_vec())
}
fn mac_r(reg: u8, len: usize) -> Op {
    Op::R(ACCESS_MAC, reg as u16, len as u16, len)
}
fn phy_w(reg: u8, v: u16) -> Op {
    Op::W(ACCESS_PHY, 3, reg as u16, v.to_le_bytes().to_vec())
}
fn phy_r(reg: u8) -> Op {
    Op::R(ACCESS_PHY, 3, reg as u16, 2)
}
fn eeprom_r(word: u16) -> Op {
    Op::R(ACCESS_EEPROM, word, 1, 2)
}
fn tx_fifo_r() -> Op {
    Op::R(0x81, 0x8c, 0, 4)
}
fn delay(ms: u32) -> Op {
    Op::Delay(ms)
}

struct Mock {
    ops: Vec<Op>,
    mac_regs: [u8; 256],
    phy: [u16; 32],
    /// EEPROM words as raw bytes (word `w` at `2*w`).
    eeprom: [u8; 256],
    /// The 64 eFuse bytes.
    efuse: [u8; 64],
    /// Values returned by the 0x81/0x8c TX FIFO status reads, in order.
    tx_fifo: VecDeque<u32>,
    /// When the queue is empty: report the FIFO full forever instead of free.
    fifo_stuck: bool,
    bmsr_reads: u32,
    /// BMSR read number (1-based, counted since the mock was made) from which the link is up; `None` = never.
    link_from_read: Option<u32>,
    bulk_in: VecDeque<Vec<u8>>,
    bulk_in_lens: Vec<usize>,
    max_packet: u16,
    /// Make every control OUT report one byte fewer.
    short_writes: bool,
    fail_next: Option<TransportError>,
    /// Every call fails with `Disconnected`, as after an unplug.
    unplugged: bool,
}

impl Mock {
    fn new() -> Self {
        let mut mac_regs = [0u8; 256];
        mac_regs[NODE_ID as usize..NODE_ID as usize + 6].copy_from_slice(&OWNER_MAC);
        mac_regs[PHYSICAL_LINK_STATUS as usize] = USB_SS;
        mac_regs[GENERAL_STATUS as usize] = SECLD; // UA2: no GPIO write in ax88179_led_setting
        let mut phy = [0u16; 32];
        phy[MII_BMCR as usize] = 0x1140; // the PHY's default: autoneg on, full duplex, 1000
        phy[GMII_PHY_PHYSR as usize] = PHYSR_GIGA | PHYSR_FULL | PHYSR_LINK;
        let mut eeprom = [0u8; 256];
        eeprom[10] = 0xff; // words 0..4 are zero, so the checksum byte makes ax88179_check_eeprom pass
        eeprom[0x84..0x86].copy_from_slice(&MOCK_LEDVALUE.to_le_bytes()); // word 0x42: valid LED mode
        eeprom[0x86..0x88].copy_from_slice(&0xffffu16.to_le_bytes()); // word 0x43: no auto-detach
        eeprom[0x78..0x7a].copy_from_slice(&0xffffu16.to_le_bytes()); // word 0x3c: old LED mode 0xff
        Self {
            ops: Vec::new(),
            mac_regs,
            phy,
            eeprom,
            efuse: [0xff; 64], // invalid: ax88179_check_efuse refuses it
            tx_fifo: VecDeque::new(),
            fifo_stuck: false,
            bmsr_reads: 0,
            link_from_read: Some(1),
            bulk_in: VecDeque::new(),
            bulk_in_lens: Vec::new(),
            max_packet: 1024,
            short_writes: false,
            fail_next: None,
            unplugged: false,
        }
    }

    fn take_ops(&mut self) -> Vec<Op> {
        core::mem::take(&mut self.ops)
    }
}

impl UsbTransport for Mock {
    fn control_out(&mut self, req: VendorRequest, data: &[u8]) -> Result<usize, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        self.ops
            .push(Op::W(req.request, req.value, req.index, data.to_vec()));
        match req.request {
            ACCESS_MAC => {
                assert_eq!(
                    req.index as usize,
                    data.len(),
                    "MAC access: wIndex is the length"
                );
                let r = req.value as usize;
                self.mac_regs[r..r + data.len()].copy_from_slice(data);
                // An SROM read command loads the addressed EEPROM word into the data registers.
                if r == SROM_CMD as usize && data[0] & EEP_RD != 0 {
                    let a = self.mac_regs[SROM_ADDR as usize] as usize * 2;
                    self.mac_regs[SROM_DATA_LOW as usize..SROM_DATA_LOW as usize + 2]
                        .copy_from_slice(&self.eeprom[a..a + 2]);
                    self.mac_regs[SROM_CMD as usize] = 0; // not busy
                }
            }
            ACCESS_PHY => {
                assert_eq!(req.value, PHY_ID);
                let v = u16::from_le_bytes([data[0], data[1]]);
                self.phy[req.index as usize] = v;
            }
            r => panic!("unexpected request {r:#x}"),
        }
        Ok(if self.short_writes {
            data.len() - 1
        } else {
            data.len()
        })
    }

    fn control_in(&mut self, req: VendorRequest, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        self.ops
            .push(Op::R(req.request, req.value, req.index, buf.len()));
        match req.request {
            ACCESS_MAC => {
                assert_eq!(req.index as usize, buf.len());
                let r = req.value as usize;
                buf.copy_from_slice(&self.mac_regs[r..r + buf.len()]);
            }
            ACCESS_PHY => {
                assert_eq!(req.value, PHY_ID);
                let v = match req.index as u8 {
                    MII_BMSR => {
                        self.bmsr_reads += 1;
                        let up = self.link_from_read.is_some_and(|n| self.bmsr_reads >= n);
                        0x7949 | if up { BMSR_LSTATUS } else { 0 }
                    }
                    r => self.phy[r as usize],
                };
                buf.copy_from_slice(&v.to_le_bytes());
            }
            ACCESS_EEPROM => {
                assert_eq!(req.index, 1);
                let w = req.value as usize * 2;
                buf.copy_from_slice(&self.eeprom[w..w + 2]);
            }
            ACCESS_EFUS => {
                assert_eq!(req.value, 0);
                assert_eq!(buf.len(), 64);
                buf.copy_from_slice(&self.efuse);
            }
            0x81 => {
                assert_eq!((req.value, req.index), (0x8c, 0));
                let v = self.tx_fifo.pop_front().unwrap_or(if self.fifo_stuck {
                    0x4000_0000
                } else {
                    0
                });
                buf.copy_from_slice(&v.to_le_bytes());
            }
            r => panic!("unexpected request {r:#x}"),
        }
        Ok(buf.len())
    }

    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        self.ops.push(Op::BulkOut(data.to_vec()));
        Ok(data.len())
    }

    fn bulk_in(&mut self, buf: &mut [u8]) -> Result<Option<usize>, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        self.bulk_in_lens.push(buf.len());
        Ok(self.bulk_in.pop_front().map(|t| {
            buf[..t.len()].copy_from_slice(&t);
            t.len()
        }))
    }

    fn bulk_out_max_packet(&self) -> u16 {
        self.max_packet
    }

    fn delay_ms(&mut self, ms: u32) {
        self.ops.push(Op::Delay(ms));
    }
}

/// Linux `ax88179_reset` against the default mock (UA2, valid EEPROM with a valid LED word, no auto-detach, PHY
/// FIFO free, BMCR with autoneg enabled).
fn linux_reset() -> Vec<Op> {
    let mut v = std::vec![
        // Power up the Ethernet PHY, msleep(500)
        mac_w(PHYPWR_RSTCTL, &[0x00, 0x00]),
        mac_w(PHYPWR_RSTCTL, &[0x20, 0x00]),
        delay(500),
        // CLK_SELECT = ACS | BCS, msleep(200)
        mac_w(CLK_SELECT, &[CLK_SELECT_ACS | CLK_SELECT_BCS]),
        delay(200),
        // ax88179_auto_detach: EEPROM word 0x43 is 0xffff -> nothing
        eeprom_r(0x43),
        // ax88179_get_mac_addr: read NODE_ID, write it back
        mac_r(NODE_ID, 6),
        mac_w(NODE_ID, &OWNER_MAC),
        // RX bulk configuration, pause watermarks, checksum offload
        mac_w(RX_BULKIN_QCTRL, &BULKIN_SUPER_GIGA),
        mac_w(PAUSE_WATERLVL_LOW, &[0x34]),
        mac_w(PAUSE_WATERLVL_HIGH, &[0x52]),
        mac_w(RXCOE_CTL, &[COE_ALL]),
        mac_w(TXCOE_CTL, &[COE_ALL]),
        // RX_CTL start, MONITOR_MOD, default medium (giga, no jumbo)
        mac_w(RX_CTL, &RX_CTL_RUN.to_le_bytes()),
        mac_w(MONITOR_MOD, &[0x64]),
        mac_w(MEDIUM_STATUS_MODE, &MEDIUM_DEFAULT.to_le_bytes()),
        // WoL check: read MONITOR_MOD back
        mac_r(MONITOR_MOD, 1),
    ];
    // ax88179_led_setting: UA2 (SECLD set) -> no GPIO write; valid EEPROM -> word 0x42
    v.push(mac_r(GENERAL_STATUS, 1));
    for i in 0..6u8 {
        v.push(mac_w(SROM_ADDR, &[i]));
        v.push(mac_w(SROM_CMD, &[EEP_RD]));
        v.push(mac_r(SROM_CMD, 1));
        v.push(mac_r(SROM_DATA_LOW, 2));
    }
    v.push(mac_w(SROM_ADDR, &[0x42]));
    v.push(mac_w(SROM_CMD, &[EEP_RD]));
    v.push(mac_r(SROM_CMD, 1));
    v.push(mac_r(SROM_DATA_HIGH, 1));
    v.push(mac_r(SROM_DATA_LOW, 1));
    v.extend(led_writes());
    // ax88179_disable_eee
    v.extend([
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE3),
        phy_w(MII_PHYADDR, 0x3246),
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE0),
    ]);
    // EEE advertisement cleared: MMD indirect write of 0 to MDIO_AN_EEE_ADV
    v.extend([
        phy_w(MII_MMD_CTRL, MDIO_MMD_AN),
        phy_w(MII_MMD_DATA, MDIO_AN_EEE_ADV),
        phy_w(MII_MMD_CTRL, MDIO_MMD_AN | MII_MMD_CTRL_NOINCR),
        phy_w(MII_MMD_DATA, 0),
    ]);
    // mii_nway_restart: BMCR has ANENABLE -> BMCR | ANRESTART
    v.extend([phy_r(MII_BMCR), phy_w(MII_BMCR, 0x1140 | BMCR_ANRESTART)]);
    v
}

/// The PHY LED register writes and the MAC LEDCTRL write `ax88179_led_setting` ends with for `MOCK_LEDVALUE`.
fn led_writes() -> Vec<Op> {
    std::vec![
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_EXT),
        phy_w(GMII_PHYPAGE, 0x2c),
        phy_r(GMII_LED_ACT),
        phy_r(GMII_LED_LINK),
        phy_w(GMII_LED_ACT, MOCK_LEDACT),
        phy_w(GMII_LED_LINK, MOCK_LEDLINK),
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE0),
        mac_w(LEDCTRL, &[0x00]),
    ]
}

/// The link_reset prologue with a free TX FIFO: RX_CTL = 0, RX_CTL = rxctl, one 0x81/0x8c read.
fn fifo_drain_free() -> Vec<Op> {
    std::vec![
        mac_w(RX_CTL, &RX_CTL_STOP.to_le_bytes()),
        mac_w(RX_CTL, &RX_CTL_RUN.to_le_bytes()),
        tx_fifo_r(),
    ]
}

fn probed(mock: Mock) -> Ax88179<Mock> {
    let mut dev = Ax88179::probe(mock).unwrap();
    dev.transport().take_ops();
    dev
}

#[test]
fn probe_is_linux_reset_in_order() {
    let mut dev = Ax88179::probe(Mock::new()).unwrap();
    assert_eq!(dev.transport().ops, linux_reset());
    assert_eq!(dev.mac_addr(), OWNER_MAC);
    assert_eq!(dev.link(), None);
    assert_eq!(dev.stats(), Stats::default());
}

#[test]
fn register_values_match_linux() {
    assert_eq!(RX_CTL_RUN, 0x03aa);
    assert_eq!(RX_CTL_STOP, 0x0000);
    assert_eq!(MEDIUM_DEFAULT, 0x0133); // RECEIVE_EN | TXFLOW | RXFLOW | FULL_DUPLEX | GIGAMODE, no JUMBO_EN
    assert_eq!(COE_ALL, 0x67);
    assert_eq!(rx_transfer_len(&BULKIN_SUPER_GIGA), 20 * 1024);
    assert_eq!(rx_transfer_len(&BULKIN_FAST_100), RX_BUF_LEN);
}

#[test]
fn auto_detach_sets_ulr_and_at_when_eeprom_word_0x43_allows() {
    let mut mock = Mock::new();
    mock.eeprom[0x86..0x88].copy_from_slice(&0x0100u16.to_le_bytes()); // word 0x43, bit 0x0100 set
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    let at = ops.iter().position(|o| *o == eeprom_r(0x43)).unwrap();
    // Linux ax88179_auto_detach: CLK_SELECT |= ULR, PHYPWR_RSTCTL |= AT
    assert_eq!(
        ops[at + 1..at + 5],
        [
            mac_r(CLK_SELECT, 1),
            mac_w(CLK_SELECT, &[CLK_SELECT_ACS | CLK_SELECT_BCS | CLK_SELECT_ULR]),
            mac_r(PHYPWR_RSTCTL, 2),
            mac_w(PHYPWR_RSTCTL, &(PHYPWR_RSTCTL_IPRL | PHYPWR_RSTCTL_AT).to_le_bytes()),
        ]
    );
}

#[test]
fn auto_detach_does_nothing_when_bit_0x0100_is_clear() {
    let mut mock = Mock::new();
    mock.eeprom[0x86..0x88].copy_from_slice(&0x0200u16.to_le_bytes()); // word 0x43 without bit 0x0100
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    let at = ops.iter().position(|o| *o == eeprom_r(0x43)).unwrap();
    // Straight on to ax88179_get_mac_addr.
    assert_eq!(ops[at + 1], mac_r(NODE_ID, 6));
}

#[test]
fn led_setting_reads_efuse_when_the_eeprom_is_invalid() {
    let mut mock = Mock::new();
    mock.eeprom[0] = 0xff; // ax88179_check_eeprom refuses at word 0
    // A valid eFuse carrying MOCK_LEDVALUE in bytes 51/52.
    let mut efuse = [0u8; 64];
    efuse[0] = 1;
    efuse[51] = (MOCK_LEDVALUE >> 8) as u8;
    efuse[52] = MOCK_LEDVALUE as u8;
    for fix in 0..=255u16 {
        efuse[63] = fix as u8;
        let mut csum = efuse.iter().fold(0u16, |a, &b| a + b as u16);
        while csum > 255 {
            csum = (csum & 0xff) + (csum >> 8);
        }
        if csum == 0xff {
            break;
        }
    }
    mock.efuse = efuse;
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    // The eFuse read happened, the old-EEPROM conversion did not, and the LED writes are the usual ones.
    assert!(ops.contains(&Op::R(ACCESS_EFUS, 0, 64, 64)));
    assert!(!ops.contains(&eeprom_r(0x3c)));
    let at = ops.iter().position(|o| *o == Op::R(ACCESS_EFUS, 0, 64, 64)).unwrap();
    assert_eq!(ops[at + 1..at + 1 + led_writes().len()], led_writes()[..]);
}

#[test]
fn led_setting_converts_the_old_eeprom_led_when_both_sources_are_invalid() {
    let mut mock = Mock::new();
    mock.eeprom[0] = 0xff; // EEPROM invalid
    // efuse[0] is already 0xff: eFuse invalid too
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    // ax88179_convert_old_led reads EEPROM word 0x3c (0xffff -> the default set) and the LED writes follow.
    let at = ops.iter().position(|o| *o == eeprom_r(0x3c)).unwrap();
    assert_eq!(ops[at + 1..at + 1 + led_writes().len()], led_writes()[..]);
}

#[test]
fn led_setting_converts_the_old_eeprom_led_when_word_0x42_is_0xffff() {
    let mut mock = Mock::new();
    mock.eeprom[0x84..0x86].copy_from_slice(&0xffffu16.to_le_bytes()); // word 0x42: no LED_VALID
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    assert!(ops.contains(&eeprom_r(0x3c)));
    let at = ops.iter().position(|o| *o == eeprom_r(0x3c)).unwrap();
    assert_eq!(ops[at + 1..at + 1 + led_writes().len()], led_writes()[..]);
}

#[test]
fn led_setting_enables_the_gpios_on_a_ua1_chip() {
    let mut mock = Mock::new();
    mock.mac_regs[GENERAL_STATUS as usize] = 0; // SECLD clear: UA1
    let mut dev = Ax88179::probe(mock).unwrap();
    let ops = dev.transport().take_ops();
    let at = ops.iter().position(|o| *o == mac_r(GENERAL_STATUS, 1)).unwrap();
    assert_eq!(
        ops[at + 1],
        mac_w(
            GPIO_CTRL,
            &[GPIO_CTRL_GPIO3EN | GPIO_CTRL_GPIO2EN | GPIO_CTRL_GPIO1EN]
        )
    );
}

#[test]
fn eee_disable_and_advertise_clear_are_the_probe_tail() {
    let mut dev = Ax88179::probe(Mock::new()).unwrap();
    let ops = dev.transport().take_ops();
    let mut want = std::vec![
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE3),
        phy_w(MII_PHYADDR, 0x3246),
        phy_w(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE0),
        phy_w(MII_MMD_CTRL, MDIO_MMD_AN),
        phy_w(MII_MMD_DATA, MDIO_AN_EEE_ADV),
        phy_w(MII_MMD_CTRL, MDIO_MMD_AN | MII_MMD_CTRL_NOINCR),
        phy_w(MII_MMD_DATA, 0),
        phy_r(MII_BMCR),
        phy_w(MII_BMCR, 0x1140 | BMCR_ANRESTART),
    ];
    let n = ops.len();
    assert_eq!(ops[n - want.len()..], want[..]);
    want.clear();
}

#[test]
fn start_gigabit_superspeed() {
    let mut mock = Mock::new();
    mock.link_from_read = Some(3);
    let mut dev = probed(mock);
    let link = dev.start().unwrap().unwrap();
    let mut want = std::vec![phy_r(MII_BMSR), delay(50), phy_r(MII_BMSR), delay(50), phy_r(MII_BMSR)];
    want.extend(fifo_drain_free());
    want.extend([
        mac_r(PHYSICAL_LINK_STATUS, 1),
        phy_r(GMII_PHY_PHYSR),
        mac_w(RX_BULKIN_QCTRL, &BULKIN_SUPER_GIGA),
        // RECEIVE_EN | TXFLOW | RXFLOW | GIGAMODE | EN_125MHZ | FULL_DUPLEX, no JUMBO_EN
        mac_w(MEDIUM_STATUS_MODE, &0x013b_u16.to_le_bytes()),
    ]);
    assert_eq!(dev.transport().take_ops(), want);
    assert_eq!(
        link,
        Link {
            speed: Speed::Mbps1000,
            full_duplex: true,
            usb: UsbSpeed::Super,
            medium: 0x013b,
            bulkin: BULKIN_SUPER_GIGA,
        }
    );
    assert_eq!(dev.rx_transfer_len(), 20 * 1024);
}

#[test]
fn start_picks_queue_and_medium_per_link() {
    // (USB status, PHYSR) -> (speed, bulk-in queue, medium mode), as Linux ax88179_link_reset branches.
    let cases: [(u8, u16, Speed, [u8; 5], u16); 9] = [
        (USB_SS, PHYSR_GIGA | PHYSR_FULL, Speed::Mbps1000, BULKIN_SUPER_GIGA, 0x013b),
        (USB_HS, PHYSR_GIGA | PHYSR_FULL, Speed::Mbps1000, BULKIN_HIGH_GIGA, 0x013b),
        (0, PHYSR_GIGA | PHYSR_FULL, Speed::Mbps1000, BULKIN_SLOW, 0x013b),
        (USB_SS, PHYSR_GIGA, Speed::Mbps1000, BULKIN_SUPER_GIGA, 0x0139),
        (USB_SS, PHYSR_100 | PHYSR_FULL, Speed::Mbps100, BULKIN_FAST_100, 0x0332),
        (USB_HS, PHYSR_100, Speed::Mbps100, BULKIN_FAST_100, 0x0330),
        (0, PHYSR_100 | PHYSR_FULL, Speed::Mbps100, BULKIN_SLOW, 0x0332),
        (USB_SS, PHYSR_FULL, Speed::Mbps10, BULKIN_SLOW, 0x0132),
        (USB_HS, 0, Speed::Mbps10, BULKIN_SLOW, 0x0130),
    ];
    for (usb, physr, speed, bulkin, medium) in cases {
        let mut mock = Mock::new();
        mock.mac_regs[PHYSICAL_LINK_STATUS as usize] = usb;
        mock.phy[GMII_PHY_PHYSR as usize] = physr | PHYSR_LINK;
        let mut dev = probed(mock);
        let link = dev.start().unwrap().unwrap();
        let ops = dev.transport().take_ops();
        let tail = &ops[ops.len() - 2..];
        assert_eq!(
            tail,
            [
                mac_w(RX_BULKIN_QCTRL, &bulkin),
                mac_w(MEDIUM_STATUS_MODE, &medium.to_le_bytes())
            ],
            "usb {usb:#x} physr {physr:#x}"
        );
        assert_eq!(
            (link.speed, link.bulkin, link.medium),
            (speed, bulkin, medium)
        );
        assert_eq!(dev.rx_transfer_len(), 1024 * (bulkin[3] as usize + 2));
        assert!(dev.rx_transfer_len() <= RX_BUF_LEN);
    }
}

#[test]
fn jumbo_en_is_never_set_at_mtu_1500() {
    assert_eq!(MTU, 1500);
    assert_eq!(MEDIUM_DEFAULT & MEDIUM_JUMBO_EN, 0);
    let mut dev = probed(Mock::new());
    let link = dev.start().unwrap().unwrap();
    assert_eq!(link.medium & MEDIUM_JUMBO_EN, 0);
    assert_eq!(link.medium, 0x013b);
}

#[test]
fn tx_fifo_busy_then_free_repeats_the_drain_wait() {
    let mut mock = Mock::new();
    mock.tx_fifo = VecDeque::from([0x4000_0000, 0]);
    let mut dev = probed(mock);
    let link = dev.start().unwrap().unwrap();
    let ops = dev.transport().take_ops();
    let mut want = std::vec![phy_r(MII_BMSR)];
    want.extend(fifo_drain_free());
    want.extend([delay(1)]);
    want.extend(fifo_drain_free());
    want.extend([
        mac_r(PHYSICAL_LINK_STATUS, 1),
        phy_r(GMII_PHY_PHYSR),
        mac_w(RX_BULKIN_QCTRL, &BULKIN_SUPER_GIGA),
        mac_w(MEDIUM_STATUS_MODE, &0x013b_u16.to_le_bytes()),
    ]);
    assert_eq!(ops, want);
    assert_eq!(link.speed, Speed::Mbps1000);
}

#[test]
fn tx_fifo_busy_past_100_ms_configures_nothing() {
    let mut mock = Mock::new();
    mock.fifo_stuck = true;
    let mut dev = probed(mock);
    assert_eq!(dev.start().unwrap(), None);
    let ops = dev.transport().take_ops();
    // 100 ms in 1 ms steps: 101 drain attempts (the last one hits the deadline), 100 delays, no medium write.
    assert_eq!(ops.iter().filter(|o| **o == tx_fifo_r()).count(), 101);
    assert_eq!(ops.iter().filter(|o| **o == delay(1)).count(), 100);
    assert!(!ops.contains(&mac_w(MEDIUM_STATUS_MODE, &0x013b_u16.to_le_bytes())));
    assert_eq!(dev.link(), None);
}

#[test]
fn start_without_cable_resets_once_then_reports_no_link() {
    let mut mock = Mock::new();
    mock.link_from_read = None;
    let mut dev = probed(mock);
    assert_eq!(dev.start().unwrap(), None);
    let ops = dev.transport().take_ops();
    let wait: Vec<Op> = (0..100)
        .flat_map(|_| [phy_r(MII_BMSR), delay(50)])
        .collect();
    let mut want = wait.clone();
    want.extend(linux_reset());
    want.extend_from_slice(&wait);
    assert_eq!(ops, want);
    assert_eq!(dev.link(), None);
}

#[test]
fn start_link_after_the_retry_reset() {
    let mut mock = Mock::new();
    mock.link_from_read = Some(101);
    let mut dev = probed(mock);
    let link = dev.start().unwrap().unwrap();
    assert_eq!(link.speed, Speed::Mbps1000);
    let ops = dev.transport().take_ops();
    assert_eq!(ops.iter().filter(|o| **o == phy_r(MII_BMSR)).count(), 101);
    assert_eq!(
        ops.iter()
            .filter(|o| **o == mac_w(CLK_SELECT, &[CLK_SELECT_ACS | CLK_SELECT_BCS]))
            .count(),
        1
    );
}

#[test]
fn start_with_bmsr_up_but_physr_down_writes_nothing_more() {
    let mut mock = Mock::new();
    mock.phy[GMII_PHY_PHYSR as usize] = PHYSR_GIGA;
    let mut dev = probed(mock);
    assert_eq!(dev.start().unwrap(), None);
    let mut want = std::vec![phy_r(MII_BMSR)];
    want.extend(fifo_drain_free());
    want.extend([mac_r(PHYSICAL_LINK_STATUS, 1), phy_r(GMII_PHY_PHYSR)]);
    assert_eq!(dev.transport().take_ops(), want);
}

#[test]
fn poll_link_follows_cable_changes() {
    let mut dev = probed(Mock::new());
    dev.start().unwrap().unwrap();
    dev.transport().take_ops();

    // Still up: one BMSR read, nothing written.
    assert!(dev.poll_link().unwrap().is_some());
    assert_eq!(dev.transport().take_ops(), [phy_r(MII_BMSR)]);

    // Unplugged.
    dev.transport().link_from_read = None;
    assert_eq!(dev.poll_link().unwrap(), None);
    assert_eq!(dev.transport().take_ops(), [phy_r(MII_BMSR)]);
    assert_eq!(dev.poll_link().unwrap(), None);
    dev.transport().take_ops();

    // Plugged into a 100 Mbit/s switch.
    dev.transport().link_from_read = Some(0);
    dev.transport().phy[GMII_PHY_PHYSR as usize] = PHYSR_100 | PHYSR_FULL | PHYSR_LINK;
    let link = dev.poll_link().unwrap().unwrap();
    assert_eq!(link.speed, Speed::Mbps100);
    let mut want = std::vec![phy_r(MII_BMSR)];
    want.extend(fifo_drain_free());
    want.extend([
        mac_r(PHYSICAL_LINK_STATUS, 1),
        phy_r(GMII_PHY_PHYSR),
        mac_w(RX_BULKIN_QCTRL, &BULKIN_FAST_100),
        mac_w(MEDIUM_STATUS_MODE, &0x0332_u16.to_le_bytes()),
    ]);
    assert_eq!(dev.transport().take_ops(), want);
    assert_eq!(dev.rx_transfer_len(), RX_BUF_LEN);
}

#[test]
fn poll_link_timed_without_cable_resets_once_after_5_s_like_start() {
    let mut mock = Mock::new();
    mock.link_from_read = None;
    let mut dev = probed(mock);
    dev.transport().take_ops();
    // Right after probe, then once a second: one BMSR read each, until 5 s have passed without link.
    assert_eq!(dev.poll_link_timed(0).unwrap(), None);
    for _ in 0..4 {
        assert_eq!(dev.poll_link_timed(1000).unwrap(), None);
    }
    assert_eq!(dev.transport().take_ops(), std::vec![phy_r(MII_BMSR); 5]);
    // The fifth second: the retry reset.
    assert_eq!(dev.poll_link_timed(1000).unwrap(), None);
    let mut want = std::vec![phy_r(MII_BMSR)];
    want.extend(linux_reset());
    assert_eq!(dev.transport().take_ops(), want);
    // Never again.
    for _ in 0..20 {
        assert_eq!(dev.poll_link_timed(1000).unwrap(), None);
    }
    assert_eq!(dev.transport().take_ops(), std::vec![phy_r(MII_BMSR); 20]);
}

#[test]
fn poll_link_timed_configures_the_link_as_start_does() {
    let mut mock = Mock::new();
    mock.link_from_read = Some(3);
    let mut dev = probed(mock);
    dev.transport().take_ops();
    assert_eq!(dev.poll_link_timed(0).unwrap(), None);
    assert_eq!(dev.poll_link_timed(1000).unwrap(), None);
    let link = dev.poll_link_timed(1000).unwrap().unwrap();
    assert_eq!(link.speed, Speed::Mbps1000);
    assert_eq!(link.medium, 0x013b);
    assert_eq!(link.bulkin, BULKIN_SUPER_GIGA);
    let ops = dev.transport().take_ops();
    let mut want = std::vec![phy_r(MII_BMSR)];
    want.extend(fifo_drain_free());
    want.extend([
        mac_r(PHYSICAL_LINK_STATUS, 1),
        phy_r(GMII_PHY_PHYSR),
        mac_w(RX_BULKIN_QCTRL, &BULKIN_SUPER_GIGA),
        mac_w(MEDIUM_STATUS_MODE, &0x013b_u16.to_le_bytes()),
    ]);
    assert_eq!(ops[2..], want[..]);
}

#[test]
fn poll_link_timed_does_not_reset_after_a_link_was_seen() {
    let mut dev = probed(Mock::new());
    assert!(dev.poll_link_timed(0).unwrap().is_some());
    dev.transport().link_from_read = None;
    dev.transport().take_ops();
    for _ in 0..10 {
        assert_eq!(dev.poll_link_timed(1000).unwrap(), None);
    }
    assert_eq!(dev.transport().take_ops(), std::vec![phy_r(MII_BMSR); 10]);
}

#[test]
fn send_frames_header_and_padding() {
    let mut dev = probed(Mock::new());
    let f = frame(60, 1);
    dev.send(&f).unwrap();
    let mut want = std::vec![60, 0, 0, 0, 0, 0, 0, 0];
    want.extend_from_slice(&f);
    assert_eq!(dev.transport().take_ops(), [Op::BulkOut(want)]);

    // 1016 + 8 = 1024 = SuperSpeed max packet: Linux ax88179_tx_fixup sets the padding flag.
    let f = frame(1016, 2);
    dev.send(&f).unwrap();
    let ops = dev.transport().take_ops();
    let Op::BulkOut(sent) = &ops[0] else { panic!() };
    assert_eq!(sent.len(), 1024);
    assert_eq!(sent[..8], [0xf8, 0x03, 0, 0, 0x00, 0x80, 0x00, 0x80]);
    assert_eq!(&sent[8..], &f[..]);

    assert_eq!(dev.send(&[0; 10]), Err(Error::Tx(TxError::TooShort(10))));
    assert_eq!(dev.stats().tx_frames, 2);
}

#[test]
fn send_uses_the_endpoints_max_packet() {
    let mut mock = Mock::new();
    mock.max_packet = 512;
    let mut dev = probed(mock);
    dev.send(&frame(504, 0)).unwrap();
    let ops = dev.transport().take_ops();
    let Op::BulkOut(sent) = &ops[0] else { panic!() };
    assert_eq!(sent[4..8], [0x00, 0x80, 0x00, 0x80]);
}

#[test]
fn receive_drains_batches_in_order() {
    let mut dev = probed(Mock::new());
    dev.start().unwrap();
    let (a, b, c, d) = (frame(60, 1), frame(1514, 2), frame(98, 3), frame(342, 4));
    dev.transport()
        .bulk_in
        .push_back(batch(&[(&a, 0), (&b, RXHDR_CRC_ERR), (&c, 0)], true));
    dev.transport().bulk_in.push_back(batch(&[(&d, 0)], false));

    let mut out = [0u8; 1600];
    let mut got = Vec::new();
    while let Some(n) = dev.receive(&mut out).unwrap() {
        got.push(out[..n].to_vec());
    }
    assert_eq!(got, [a, c, d]);
    let s = dev.stats();
    assert_eq!(
        (s.rx_frames, s.rx_frame_errors, s.rx_transfers, s.rx_batch_errors),
        (3, 1, 2, 0)
    );
    // Every transfer was asked for at the link's size: 20 KiB for gigabit at SuperSpeed.
    assert!(dev.transport().bulk_in_lens.iter().all(|&l| l == 20 * 1024));
    assert!(!dev.has_buffered_frames());
}

#[test]
fn receive_leaves_frames_buffered_between_calls() {
    let mut dev = probed(Mock::new());
    let (a, b) = (frame(60, 1), frame(60, 2));
    dev.transport()
        .bulk_in
        .push_back(batch(&[(&a, 0), (&b, 0)], false));
    let mut out = [0u8; 1600];
    assert_eq!(dev.receive(&mut out).unwrap(), Some(60));
    assert!(dev.has_buffered_frames());
    assert_eq!(dev.receive(&mut out).unwrap(), Some(60));
    assert_eq!(out[..60], b[..]);
    assert_eq!(dev.receive(&mut out).unwrap(), None);
    // One transfer taken; the last call asked the transport once more and got nothing.
    assert_eq!(dev.transport().bulk_in_lens.len(), 2);
}

#[test]
fn receive_counts_a_bad_transfer_and_moves_on() {
    let mut dev = probed(Mock::new());
    let a = frame(60, 1);
    dev.transport().bulk_in.push_back(std::vec![1, 2]);
    dev.transport().bulk_in.push_back(batch(&[(&a, 0)], false));
    let mut out = [0u8; 1600];
    assert_eq!(dev.receive(&mut out).unwrap(), None);
    assert_eq!(dev.receive(&mut out).unwrap(), Some(60));
    assert_eq!(dev.stats().rx_batch_errors, 1);
}

#[test]
fn receive_into_a_short_buffer_drops_only_that_frame() {
    let mut dev = probed(Mock::new());
    let (a, b) = (frame(1514, 1), frame(60, 2));
    dev.transport()
        .bulk_in
        .push_back(batch(&[(&a, 0), (&b, 0)], false));
    let mut out = [0u8; 100];
    assert_eq!(
        dev.receive(&mut out),
        Err(Error::BufferTooSmall { needed: 1514 })
    );
    assert_eq!(dev.receive(&mut out).unwrap(), Some(60));
    assert_eq!(out[..60], b[..]);
}

#[test]
fn stop_clears_receive_en_and_drops_buffered_frames() {
    let mut dev = probed(Mock::new());
    dev.start().unwrap().unwrap();
    dev.transport().take_ops();
    let (a, b) = (frame(60, 1), frame(60, 2));
    dev.transport()
        .bulk_in
        .push_back(batch(&[(&a, 0), (&b, 0)], false));
    let mut out = [0u8; 1600];
    dev.receive(&mut out).unwrap();
    dev.stop();
    // Linux ax88179_stop: MEDIUM_STATUS_MODE with RECEIVE_EN cleared (0x013b -> 0x003b).
    assert_eq!(
        dev.transport().take_ops(),
        [
            mac_r(MEDIUM_STATUS_MODE, 2),
            mac_w(MEDIUM_STATUS_MODE, &0x003b_u16.to_le_bytes())
        ]
    );
    assert!(!dev.has_buffered_frames());
    assert_eq!(dev.receive(&mut out).unwrap(), None);
}

#[test]
fn unbind_writes_the_linux_teardown() {
    let mut dev = probed(Mock::new());
    dev.unbind();
    assert_eq!(
        dev.transport().take_ops(),
        [
            mac_w(RX_CTL, &RX_CTL_STOP.to_le_bytes()),
            mac_w(CLK_SELECT, &[0]),
            mac_w(PHYPWR_RSTCTL, &[0, 0]),
        ]
    );
}

#[test]
fn unbind_ignores_a_gone_device() {
    let mut dev = probed(Mock::new());
    dev.transport().unplugged = true;
    dev.unbind(); // must not panic
    dev.stop(); // must not panic either
}

#[test]
fn short_control_transfer_is_an_error() {
    let mut mock = Mock::new();
    mock.short_writes = true;
    assert_eq!(
        Ax88179::probe(mock).err(),
        Some(Error::ShortTransfer {
            expected: 2,
            actual: 1
        })
    );
}

#[test]
fn transport_errors_propagate() {
    let mut mock = Mock::new();
    mock.fail_next = Some(TransportError::Disconnected);
    assert_eq!(
        Ax88179::probe(mock).err(),
        Some(Error::Transport(TransportError::Disconnected))
    );

    let mut dev = probed(Mock::new());
    dev.transport().fail_next = Some(TransportError::Stall);
    assert_eq!(
        dev.poll_link(),
        Err(Error::Transport(TransportError::Stall))
    );
}

#[test]
fn works_through_a_mutable_reference() {
    let mut mock = Mock::new();
    let dev = Ax88179::probe(&mut mock).unwrap();
    assert_eq!(dev.mac_addr(), OWNER_MAC);
    drop(dev);
    assert!(mock.ops.contains(&mac_r(NODE_ID, 6)));
    assert!(mock.ops.contains(&phy_w(MII_BMCR, 0x1140 | BMCR_ANRESTART)));
    // Dropping the device runs Linux ax88179_stop and ax88179_unbind: the PHY is powered down last.
    assert_eq!(
        &mock.ops[mock.ops.len() - 3..],
        &[mac_w(RX_CTL, &RX_CTL_STOP.to_le_bytes()), mac_w(CLK_SELECT, &[0]), mac_w(PHYPWR_RSTCTL, &[0, 0])]
    );
}

#[test]
fn debug_shows_mac_and_link() {
    let dev = probed(Mock::new());
    let s = std::format!("{dev:?}");
    assert!(s.contains("mac: [2, 0, 94, 0, 0, 2]"), "{s}");
    assert!(s.contains("link: None"), "{s}");
}

// ---- through the kernel's frame pump (lucina-usbnet), as the USB network glue drives it ----

mod pump {
    use std::collections::VecDeque;
    use std::string::String;

    use lucina_usbnet::{EthernetBackend, Event, Pump};

    use super::*;

    struct Glue {
        dev: Ax88179<Mock>,
        pump: Pump,
        tx: VecDeque<Vec<u8>>,
        rx: Vec<Vec<u8>>,
    }

    impl Glue {
        /// What `attach` does: probe, then the first pass right away.
        fn attach(mock: Mock) -> Self {
            let dev = Ax88179::probe(mock).unwrap();
            let pump = Pump::new(&dev);
            Self {
                dev,
                pump,
                tx: VecDeque::new(),
                rx: Vec::new(),
            }
        }

        fn pass(&mut self, now: u64) -> lucina_usbnet::Pass {
            let Self { dev, pump, tx, rx } = self;
            pump.pass(dev, now, &mut || tx.pop_front(), &mut |f| {
                rx.push(f.to_vec());
                true
            })
        }
    }

    #[test]
    fn attach_with_cable_logs_the_test_plans_link_line() {
        let mut g = Glue::attach(Mock::new());
        assert_eq!(
            lucina_usbnet::format_mac(g.dev.mac_addr()),
            "02:00:5e:00:00:02"
        );
        let p = g.pass(0);
        assert_eq!(
            p.events,
            [Event::LinkUp(String::from(
                "1000/full, usb Super, medium 0x013b, bulkin 07 4f 00 12 ff"
            ))]
        );
        assert!(g.pump.link_up());
    }

    #[test]
    fn no_bulk_in_before_the_link_is_configured() {
        // Autonegotiation takes seconds: the passes before it must not queue a transfer of the default size,
        // which is too short for the 24 KiB queue of a high-speed link.
        let mut mock = Mock::new();
        mock.mac_regs[PHYSICAL_LINK_STATUS as usize] = USB_HS;
        mock.link_from_read = Some(2);
        let mut g = Glue::attach(mock);
        g.pass(0);
        assert!(!g.pump.link_up());
        assert!(g.dev.transport().bulk_in_lens.is_empty());
        g.pass(1000);
        assert!(g.pump.link_up());
        assert_eq!(g.dev.transport().bulk_in_lens, [24 * 1024]);
    }

    #[test]
    fn high_speed_100_and_10_mbit_link_lines() {
        let mut mock = Mock::new();
        mock.mac_regs[PHYSICAL_LINK_STATUS as usize] = USB_HS;
        let mut g = Glue::attach(mock);
        g.pass(0);
        assert_eq!(
            g.dev.link_text(),
            "1000/full, usb High, medium 0x013b, bulkin 07 20 03 16 ff"
        );
        let mut mock = Mock::new();
        mock.phy[GMII_PHY_PHYSR as usize] = PHYSR_100 | PHYSR_FULL | PHYSR_LINK;
        let mut g = Glue::attach(mock);
        g.pass(0);
        assert_eq!(
            g.dev.link_text(),
            "100/full, usb Super, medium 0x0332, bulkin 07 ae 07 18 ff"
        );
        let mut mock = Mock::new();
        mock.phy[GMII_PHY_PHYSR as usize] = PHYSR_LINK;
        let mut g = Glue::attach(mock);
        g.pass(0);
        assert_eq!(
            g.dev.link_text(),
            "10/half, usb Super, medium 0x0130, bulkin 07 cc 4c 18 08"
        );
    }

    #[test]
    fn frames_go_out_with_the_header_and_come_in_from_a_batch() {
        let mut g = Glue::attach(Mock::new());
        g.pass(0);
        g.dev.transport().take_ops();
        // A DHCP DISCOVER-sized frame out, a 1016-byte frame on the 1024-byte packet boundary, and a batch in.
        let (out1, out2) = (frame(342, 7), frame(1016, 8));
        g.tx.extend([out1.clone(), out2.clone()]);
        let (a, b) = (frame(342, 1), frame(1514, 2));
        g.dev
            .transport()
            .bulk_in
            .push_back(batch(&[(&a, 0), (&b, 0)], true));
        let p = g.pass(4);
        assert_eq!((p.sent, p.received), (2, 2));
        let mut want1 = std::vec![0x56, 0x01, 0, 0, 0, 0, 0, 0];
        want1.extend_from_slice(&out1);
        let mut want2 = std::vec![0xf8, 0x03, 0, 0, 0x00, 0x80, 0x00, 0x80];
        want2.extend_from_slice(&out2);
        assert_eq!(
            g.dev.transport().take_ops(),
            [Op::BulkOut(want1), Op::BulkOut(want2)]
        );
        assert_eq!(g.rx, [a, b]);
        assert!(
            g.dev
                .stats_text()
                .starts_with("tx_frames 2 rx_frames 2 rx_transfers 1 ")
        );
    }

    #[test]
    fn jumbo_frames_fit_the_pumps_buffer() {
        let mut g = Glue::attach(Mock::new());
        g.pass(0);
        // The longest the 13-bit RX length allows is 8189 bytes.
        let big = frame(8000, 3);
        g.dev
            .transport()
            .bulk_in
            .push_back(batch(&[(&big, 0)], false));
        assert_eq!(g.pass(4).received, 1);
        assert_eq!(g.rx, [big]);
    }

    #[test]
    fn no_cable_then_cable_in_after_seconds() {
        let mut mock = Mock::new();
        mock.link_from_read = None;
        let mut g = Glue::attach(mock);
        g.tx.push_back(frame(60, 1));
        assert!(g.pass(0).events.is_empty());
        assert_eq!(g.pump.counters().tx_dropped, 1);
        // Passes every 4 ms; link polls once a second; the retry reset after 5 s; cable in at 7 s.
        let mut up_at = None;
        for t in (4..10_000).step_by(4) {
            if t == 7000 {
                let reads = g.dev.transport().bmsr_reads;
                g.dev.transport().link_from_read = Some(reads + 1);
            }
            if g.pass(t)
                .events
                .iter()
                .any(|e| matches!(e, Event::LinkUp(_)))
            {
                up_at = Some(t);
                break;
            }
        }
        assert_eq!(up_at, Some(7000));
        // The probe's reset, plus exactly one retry reset.
        let resets = g
            .dev
            .transport()
            .take_ops()
            .iter()
            .filter(|op| **op == mac_w(CLK_SELECT, &[CLK_SELECT_ACS | CLK_SELECT_BCS]))
            .count();
        assert_eq!(resets, 2);
    }

    #[test]
    fn cable_out_is_link_down_with_the_stats() {
        let mut g = Glue::attach(Mock::new());
        g.pass(0);
        g.dev.transport().link_from_read = None;
        let p = g.pass(1000);
        assert_eq!(
            p.events,
            [Event::LinkDown(String::from(
                "tx_frames 0 rx_frames 0 rx_transfers 0 rx_frame_errors 0 rx_batch_errors 0"
            ))]
        );
    }

    #[test]
    fn unplug_is_gone() {
        let mut g = Glue::attach(Mock::new());
        g.pass(0);
        g.dev.transport().unplugged = true;
        assert!(g.pass(4).gone());
    }

    #[test]
    fn other_transfer_errors_are_not_gone() {
        let mut g = Glue::attach(Mock::new());
        g.pass(0);
        g.dev.transport().fail_next = Some(TransportError::Timeout);
        // The next link poll's BMSR read fails; the adapter stays.
        let p = g.pass(1000);
        assert!(!p.gone());
        assert_eq!(g.pump.counters().link_errors, 1);
        assert!(g.pump.link_up());
    }
}
