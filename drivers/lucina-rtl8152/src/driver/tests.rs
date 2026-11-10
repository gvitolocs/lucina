//! The driver against a mock RTL8152. The golden sequences follow Linux's `r8152b_init` / `rtl8152_up` in their
//! order, as the control transfers they produce (`wValue`, `wIndex`, data) plus every `mdelay`; a step is U-Boot's
//! only where `docs/research/rtl8152.md` says so, and the expected line says so.

extern crate std;
use std::collections::{BTreeMap, VecDeque};
use std::string::{String, ToString};
use std::vec::Vec;

use lucina_usbnet::{BackendError, EthernetBackend, Event, Pump};

use super::*;
use crate::frame::tests::{batch, frame};
use crate::transport::VendorRequest;

/// The factory address in PLA_BACKUP.
const MAC: [u8; 6] = [0x6c, 0x6e, 0x07, 0x68, 0x01, 0x02];
/// What PLA_IDR holds on a cold chip: Realtek's placeholder.
const PLACEHOLDER: [u8; 6] = [0x00, 0xe0, 0x4c, 0x00, 0x00, 0x01];

const PLA: u16 = 0x0100;
const USB: u16 = 0x0000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Op {
    /// Control OUT: `wValue`, `wIndex`, data.
    W(u16, u16, Vec<u8>),
    /// Control IN: `wValue`, `wIndex`, length.
    R(u16, u16, usize),
    Delay(u32),
    BulkOut(Vec<u8>),
}

/// A 4-byte control OUT, as `ocp_write_*` produce: `space | byte enables`.
fn w(space: u16, byte_en: u16, addr: u16, value: u32) -> Op {
    Op::W(addr, space | byte_en, value.to_le_bytes().to_vec())
}
/// A 4-byte control IN, as `ocp_read_*` produce.
fn r(space: u16, addr: u16) -> Op {
    Op::R(addr, space, 4)
}
fn delay(ms: u32) -> Op {
    Op::Delay(ms)
}
/// `ocp_write_word` to PLA_OCP_GPHY_BASE, when the PHY page changes.
fn phy_page(base: u16) -> Op {
    w(PLA, 0x33, 0xe86c, base as u32)
}

/// The registers of a chip fresh from power-on, as far as the driver looks. Dwords by aligned address; the PHY window
/// is stored by OCP address, so that `PLA_OCP_GPHY_BASE` matters.
struct Mock {
    ops: Vec<Op>,
    pla: BTreeMap<u16, u32>,
    usb: BTreeMap<u16, u32>,
    phy: BTreeMap<u32, u32>,
    /// Reads of BMCR that still show the reset bit after a reset was written.
    bmcr_reset_reads: u32,
    bmcr_reset_takes: u32,
    /// Reads of BIST_CTRL that still show the reset bit.
    nic_reset_reads: u32,
    nic_reset_takes: u32,
    bulk_in: VecDeque<Vec<u8>>,
    bulk_in_polls: usize,
    rx_current: Vec<u8>,
    max_packet: u16,
    short_writes: bool,
    short_reads: bool,
    /// `bulk_out_ready` says no.
    tx_full: bool,
    fail_next: Option<TransportError>,
    fail_writes_next: Option<TransportError>,
    fail_reads_left: u32,
    unplugged: bool,
    log: Vec<String>,
}

impl Mock {
    fn new() -> Self {
        let mut pla = BTreeMap::new();
        set_mac(&mut pla, 0xc000, PLACEHOLDER);
        set_mac(&mut pla, 0xd000, MAC);
        pla.insert(0xc010, 0x0000_000e); // PLA_RCR accepting, as U-Boot's and Linux's clearing expects
        pla.insert(0xe610, 0x4c10_0800); // PLA_TCR1 = VER_02, PLA_TCR0 has TCR0_TX_EMPTY
        // PLA_OOB_CTRL in the top byte: OOB, FIFOs empty, link list ready.
        pla.insert(0xe84c, 0xb200_0000);
        pla.insert(0xe8dc, 0x4000_0000); // PLA_SFF_STS_7 has MCU_BORW_EN
        let mut usb = BTreeMap::new();
        usb.insert(0xd404, 0x0090_0000); // USB_USB_CTRL has RX_AGG_DISABLE and RX_ZERO_EN
        usb.insert(0xd800, 0x0000_0100); // USB_UPS_CTRL has POWER_CUT
        let mut phy = BTreeMap::new();
        phy.insert(0xa400, 0x7949_1140); // BMCR autoneg, full, 1000 bits as reset default; BMSR above it
        phy.insert(0xa404, 0x0000_0000);
        phy.insert(0xa408, 0x0000_01e1); // ANAR
        Self {
            ops: Vec::new(),
            pla,
            usb,
            phy,
            bmcr_reset_reads: 0,
            bmcr_reset_takes: 0,
            nic_reset_reads: 0,
            nic_reset_takes: 0,
            bulk_in: VecDeque::new(),
            bulk_in_polls: 0,
            rx_current: Vec::new(),
            max_packet: 512,
            short_writes: false,
            short_reads: false,
            tx_full: false,
            fail_next: None,
            fail_writes_next: None,
            fail_reads_left: 0,
            unplugged: false,
            log: Vec::new(),
        }
    }

    fn take_ops(&mut self) -> Vec<Op> {
        core::mem::take(&mut self.ops)
    }

    fn pla_read(&self, addr: u16) -> u32 {
        self.pla.get(&(addr & !3)).copied().unwrap_or(0)
    }

    fn usb_read(&self, addr: u16) -> u32 {
        self.usb.get(&(addr & !3)).copied().unwrap_or(0)
    }

    /// What a word-wide PHY register at this OCP address holds.
    fn phy_word(&self, ocp: u32) -> u16 {
        let dword = self.phy.get(&(ocp & !3)).copied().unwrap_or(0);
        (dword >> ((ocp & 2) * 8)) as u16
    }

    fn set_status(&mut self, status: u8) {
        self.pla.insert(0xe908, status as u32);
    }

    fn set_version(&mut self, tcr: u16) {
        let low = self.pla_read(0xe610) & 0xffff;
        self.pla.insert(0xe610, ((tcr as u32) << 16) | low);
    }

    fn phy_key(&self, addr: u16) -> u32 {
        let base = self.pla_read(0xe86c) & 0xf000;
        base | (addr as u32 & 0x0ffc)
    }
}

fn set_mac(pla: &mut BTreeMap<u16, u32>, addr: u16, mac: [u8; 6]) {
    pla.insert(addr, u32::from_le_bytes([mac[0], mac[1], mac[2], mac[3]]));
    pla.insert(addr + 4, u16::from_le_bytes([mac[4], mac[5]]) as u32);
}

fn merge(old: u32, new: u32, enables: u16) -> u32 {
    let mut out = old.to_le_bytes();
    let new = new.to_le_bytes();
    for (i, byte) in out.iter_mut().enumerate() {
        if enables & (1 << i) != 0 {
            *byte = new[i];
        }
    }
    u32::from_le_bytes(out)
}

impl UsbTransport for Mock {
    fn control_out(&mut self, req: VendorRequest, data: &[u8]) -> Result<usize, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if let Some(e) = self.fail_next.take().or(self.fail_writes_next.take()) {
            return Err(e);
        }
        assert_eq!(req.request, 0x05);
        assert_eq!(req.value % 4, 0, "register accesses are dword aligned");
        assert_eq!(data.len() % 4, 0);
        let enables = req.index & 0x00ff;
        assert_eq!(
            enables >> 4,
            enables & 0x0f,
            "the chip gets both nibbles the same"
        );
        self.ops.push(Op::W(req.value, req.index, data.to_vec()));
        let space = req.index & 0xff00;
        for (i, chunk) in data.chunks(4).enumerate() {
            let addr = req.value + 4 * i as u16;
            let new = u32::from_le_bytes(chunk.try_into().unwrap());
            if space == PLA && addr & 0xf000 == 0xb000 {
                let key = self.phy_key(addr);
                let old = self.phy.get(&key).copied().unwrap_or(0);
                let merged = merge(old, new, enables);
                self.phy.insert(key, merged);
                if key == 0xa400 && merged & (BMCR_RESET as u32) != 0 {
                    self.bmcr_reset_reads = self.bmcr_reset_takes;
                }
            } else if space == PLA {
                let old = self.pla_read(addr);
                let merged = merge(old, new, enables);
                self.pla.insert(addr, merged);
                if addr == 0xe810 && merged & BIST_CTRL_SW_RESET != 0 {
                    self.nic_reset_reads = self.nic_reset_takes;
                }
            } else {
                let old = self.usb_read(addr);
                self.usb.insert(addr, merge(old, new, enables));
            }
        }
        if self.short_writes {
            return Ok(data.len() - 1);
        }
        Ok(data.len())
    }

    fn control_in(&mut self, req: VendorRequest, buf: &mut [u8]) -> Result<usize, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if self.fail_reads_left > 0 {
            self.fail_reads_left -= 1;
            return Err(TransportError::Other);
        }
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        assert_eq!(req.request, 0x05);
        assert_eq!(req.value % 4, 0, "register accesses are dword aligned");
        assert!(buf.len().is_multiple_of(4) && buf.len() <= 64);
        self.ops.push(Op::R(req.value, req.index, buf.len()));
        let space = req.index & 0xff00;
        for (i, chunk) in buf.chunks_mut(4).enumerate() {
            let addr = req.value + 4 * i as u16;
            let value = if space == PLA && addr & 0xf000 == 0xb000 {
                let key = self.phy_key(addr);
                let mut value = self.phy.get(&key).copied().unwrap_or(0);
                if key == 0xa400 && value & (BMCR_RESET as u32) != 0 {
                    if self.bmcr_reset_reads == 0 {
                        value &= !(BMCR_RESET as u32);
                        self.phy.insert(key, value);
                    } else {
                        self.bmcr_reset_reads -= 1;
                    }
                }
                value
            } else if space == PLA {
                let mut value = self.pla_read(addr);
                if addr == 0xe810 && value & BIST_CTRL_SW_RESET != 0 {
                    if self.nic_reset_reads == 0 {
                        value &= !BIST_CTRL_SW_RESET;
                        self.pla.insert(addr, value);
                    } else {
                        self.nic_reset_reads -= 1;
                    }
                }
                value
            } else {
                self.usb_read(addr)
            };
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        if self.short_reads {
            return Ok(buf.len() - 1);
        }
        Ok(buf.len())
    }

    fn bulk_out_ready(&mut self) -> Result<bool, TransportError> {
        Ok(!self.tx_full)
    }

    fn bulk_out(&mut self, data: Vec<u8>) -> Result<(), TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        self.ops.push(Op::BulkOut(data));
        Ok(())
    }

    fn bulk_in(&mut self) -> Result<Option<usize>, TransportError> {
        if self.unplugged {
            return Err(TransportError::Disconnected);
        }
        if let Some(e) = self.fail_next.take() {
            return Err(e);
        }
        self.bulk_in_polls += 1;
        match self.bulk_in.pop_front() {
            Some(data) => {
                self.rx_current = data;
                Ok(Some(self.rx_current.len()))
            }
            None => {
                // The buffer went back to the pipe: reading it now would be a driver bug.
                self.rx_current = std::vec![0xde; 64];
                Ok(None)
            }
        }
    }

    fn rx_data(&self) -> &[u8] {
        &self.rx_current
    }

    fn bulk_max_packet(&self) -> u16 {
        self.max_packet
    }

    fn delay_ms(&mut self, ms: u32) {
        self.ops.push(Op::Delay(ms));
    }

    fn log(&mut self, message: fmt::Arguments<'_>) {
        self.log.push(message.to_string());
    }
}

fn brought_up(mock: Mock) -> Rtl8152<Mock> {
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    dev.transport().take_ops();
    dev
}

fn linked_up() -> Rtl8152<Mock> {
    let mut dev = brought_up(Mock::new());
    dev.transport().set_status(0x0b);
    dev.poll_link(0).unwrap().unwrap();
    dev.transport().take_ops();
    dev
}

/// `r8152b_init` for VER_02, from a chip in the mock's state, with the PHY page unset.
fn golden_init() -> Vec<Op> {
    std::vec![
        // r8152_mdio_test_and_clr_bit(MII_BMCR, BMCR_PDOWN), first, as Linux's r8152b_init has it; BMCR has no PDOWN
        phy_page(0xa000),
        r(PLA, 0xb400),
        // r8152b_disable_aldps
        phy_page(0x2000),
        w(PLA, 0x33, 0xb010, 0x0310),
        delay(20),
        // r8152_power_cut_en(false): USB_UPS_CTRL, USB_PM_CTRL_STATUS
        r(USB, 0xd800),
        w(USB, 0x33, 0xd800, 0x0000),
        r(USB, 0xd430),
        w(USB, 0xcc, 0xd430, 0x0000_0000),
        // PLA_PHY_PWR |= TX_10M_IDLE_EN | PFM_PWM_SWITCH
        r(PLA, 0xe84c),
        w(PLA, 0x33, 0xe84c, 0x00c0),
        // PLA_MAC_PWR_CTRL
        r(PLA, 0xe0c0),
        w(PLA, 0xff, 0xe0c0, 0x0701_4f07),
        // PLA_GPHY_INTR_IMR
        w(PLA, 0xcc, 0xe020, 0x000f_0000),
        // r8152b_enable_fc: ANAR |= pause bits
        phy_page(0xa000),
        r(PLA, 0xb408),
        w(PLA, 0x33, 0xb408, 0x0de1),
        // rtl_tally_reset
        r(PLA, 0xe800),
        w(PLA, 0x33, 0xe800, 0x0001),
        // RX aggregation on: USB_USB_CTRL &= ~(RX_AGG_DISABLE | RX_ZERO_EN)
        r(USB, 0xd404),
        w(USB, 0xcc, 0xd404, 0x0000_0000),
    ]
}

/// `rtl8152_up` right after [`golden_init`].
fn golden_up() -> Vec<Op> {
    std::vec![
        // r8152b_disable_aldps
        phy_page(0x2000),
        w(PLA, 0x33, 0xb010, 0x0310),
        delay(20),
        // r8152b_exit_oob: PLA_RCR &= ~RCR_ACPT_ALL
        r(PLA, 0xc010),
        w(PLA, 0xff, 0xc010, 0x0000_0000),
        // rxdy_gated_en(true)
        r(PLA, 0xe858),
        w(PLA, 0xcc, 0xe858, 0x0008_0000),
        // Linux r8153_teredo_off, OpenBSD ure_disable_teredo
        r(PLA, 0xc0bc),
        w(PLA, 0x33, 0xc0bc, 0x0000),
        w(PLA, 0x33, 0xe428, 0x0010),
        w(PLA, 0x33, 0xd2e8, 0x0000),
        w(PLA, 0xff, 0xd2cc, 0x0000_0000),
        // PLA_CRWECR, PLA_CR
        w(PLA, 0x11, 0xe81c, 0x00),
        w(PLA, 0x88, 0xe810, 0x00),
        // PLA_OOB_CTRL &= ~NOW_IS_OOB
        r(PLA, 0xe84c),
        w(PLA, 0x88, 0xe84c, 0x3200_0000),
        // PLA_SFF_STS_7 &= ~MCU_BORW_EN
        r(PLA, 0xe8dc),
        w(PLA, 0xcc, 0xe8dc, 0x0000_0000),
        // rtl8152_reinit_ll
        r(PLA, 0xe84c),
        r(PLA, 0xe8dc),
        w(PLA, 0xcc, 0xe8dc, 0x8000_0000),
        r(PLA, 0xe84c),
        // rtl8152_nic_reset
        r(PLA, 0xe810),
        w(PLA, 0xff, 0xe810, 0x1000_0000),
        r(PLA, 0xe810),
        // FIFO thresholds at high speed
        w(PLA, 0xff, 0xc0a0, 0x0008_0002),
        w(PLA, 0xff, 0xc0a4, 0x0000_0038),
        w(PLA, 0xff, 0xc0a8, 0x0000_0048),
        // PLA_TXFIFO_CTRL: Linux's TXFIFO_THR_NORMAL2 (U-Boot 0x0040_0008)
        w(PLA, 0xff, 0xe618, 0x0100_0008),
        // USB_TX_AGG, USB_RX_BUF_TH, USB_TX_DMA
        w(USB, 0x44, 0xd408, 0x0003_0000),
        w(USB, 0xff, 0xd40c, 0x7a12_0180),
        w(USB, 0xff, 0xd434, 0x0000_0101),
        // PLA_RMS
        w(PLA, 0xcc, 0xc014, 0x05f2_0000),
        // PLA_TCR0 |= TCR0_AUTO_FIFO
        r(PLA, 0xe610),
        w(PLA, 0x33, 0xe610, 0x0880),
        // r8152b_enable_aldps: the page is already 0x2000 from the init
        w(PLA, 0x33, 0xb010, 0x8310),
    ]
}

/// `rtl8152_set_speed(AUTONEG_ENABLE, SPEED_100, DUPLEX_FULL)` after [`golden_up`], and Linux's wait for the reset.
fn golden_set_speed() -> Vec<Op> {
    std::vec![
        phy_page(0xa000),
        r(PLA, 0xb408),
        w(PLA, 0x33, 0xb408, 0x0de1),
        // BMCR_ANENABLE | BMCR_ANRESTART | BMCR_RESET
        w(PLA, 0x33, 0xb400, 0x9200),
        // Linux's poll of BMCR_RESET
        delay(20),
        r(PLA, 0xb400),
    ]
}

/// `rtl8152_enable` from the state after bring-up.
fn golden_enable() -> Vec<Op> {
    std::vec![
        // U-Boot r8152_write_hwaddr, OpenBSD ure_ifmedia_init: config writes on, PLA_IDR with six bytes enabled
        w(PLA, 0x11, 0xe81c, 0xc0),
        Op::W(0xc000, PLA | 0xff, MAC[..4].to_vec()),
        Op::W(0xc004, PLA | 0x33, std::vec![MAC[4], MAC[5], 0, 0]),
        w(PLA, 0x11, 0xe81c, 0x00),
        // rtl_set_eee_plus
        r(PLA, 0xe080),
        w(PLA, 0x33, 0xe080, 0x0000),
        // r8152b_reset_packet_filter
        r(PLA, 0xc0b4),
        w(PLA, 0x33, 0xc0b4, 0x0000),
        w(PLA, 0x33, 0xc0b4, 0x0001),
        // PLA_CR |= RE | TE
        r(PLA, 0xe810),
        w(PLA, 0x88, 0xe810, 0x0c00_0000),
        // rxdy_gated_en(false)
        r(PLA, 0xe858),
        w(PLA, 0xcc, 0xe858, 0x0000_0000),
        // rtl8152_set_rx_mode: PLA_MAR all ones in two dwords, PLA_RCR |= APM | AM | AB
        w(PLA, 0xff, 0xcd00, 0xffff_ffff),
        w(PLA, 0xff, 0xcd04, 0xffff_ffff),
        r(PLA, 0xc010),
        w(PLA, 0xff, 0xc010, 0x0000_000e),
    ]
}

/// The order is Linux's `r8152b_init` / `rtl8152_up` (the PDOWN clear first in the init), with U-Boot's steps only
/// where `docs/research/rtl8152.md` says so.
#[test]
fn bring_up_follows_linux_order() {
    let mut dev = Rtl8152::new(Mock::new());
    dev.bring_up().unwrap();
    let mut expected = std::vec![
        // r8152b_get_version: PLA_TCR1 inside the dword at PLA_TCR0
        r(PLA, 0xe610),
        // PLA_BACKUP, as Linux and OpenBSD on VER_02 (U-Boot r8152_read_mac reads PLA_IDR)
        Op::R(0xd000, PLA, 8),
    ];
    expected.extend(golden_init());
    expected.extend(golden_up());
    expected.extend(golden_set_speed());
    let ops = dev.transport().take_ops();
    for (i, (got, want)) in ops.iter().zip(expected.iter()).enumerate() {
        assert_eq!(got, want, "step {}", i);
    }
    assert_eq!(ops.len(), expected.len());
    assert_eq!(dev.mac_addr(), MAC);
    assert_eq!(dev.version(), Some(Version::V02));
    assert_eq!(dev.link(), None);
}

#[test]
fn bring_up_logs_each_step_for_the_laptop_test() {
    let mut dev = Rtl8152::new(Mock::new());
    dev.bring_up().unwrap();
    assert_eq!(
        dev.transport().log,
        std::vec![
            "chip RTL8152 VER_02",
            "MAC 6c:6e:07:68:01:02",
            "init done",
            "out of OOB mode, no firmware patch (as Linux and OpenBSD)",
            "autonegotiating, advertising 10/100",
        ]
    );
}

#[test]
fn every_rtl8152_version_is_brought_up_without_a_firmware_patch() {
    for tcr in [0x4c00, 0x4c10, 0x4800] {
        let mut mock = Mock::new();
        mock.set_version(tcr);
        let mut dev = Rtl8152::new(mock);
        dev.bring_up().unwrap();
        for op in dev.transport().take_ops() {
            if let Op::W(addr, _, _) = op {
                // U-Boot's r8152b_firmware: the patch at 0xf800, its breakpoints at 0xfc26.., the PHY code at 0xb09a.
                assert!(addr < 0xf800, "{:#x} is a firmware patch register", addr);
                assert!(addr != 0xb098 && addr != 0xb09a && addr != 0xb092);
            }
        }
    }
}

#[test]
fn only_ver_01_gets_the_led_setting() {
    for (tcr, led) in [(0x4c00, true), (0x4c10, false), (0x4800, false)] {
        let mut mock = Mock::new();
        mock.set_version(tcr);
        mock.pla.insert(0xdd90, 0x0700_0000); // PLA_LED_FEATURE in the upper half
        let mut dev = Rtl8152::new(mock);
        dev.bring_up().unwrap();
        let ops = dev.transport().take_ops();
        assert_eq!(
            ops.contains(&w(PLA, 0xcc, 0xdd90, 0)),
            led,
            "tcr {:#x}",
            tcr
        );
    }
}

#[test]
fn the_version_ignores_the_other_tcr_bits() {
    let mut mock = Mock::new();
    mock.set_version(0x4c10 | 0x8303); // bits outside VERSION_MASK
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    assert_eq!(dev.version(), Some(Version::V02));
}

#[test]
fn rtl8153_is_refused_for_now_and_the_transport_comes_back() {
    let mut mock = Mock::new();
    mock.set_version(0x5c20);
    let mut dev = Rtl8152::new(mock);
    assert_eq!(dev.bring_up(), Err(Error::UnsupportedChip(Version::V05)));
    let mock = dev.into_transport();
    // Refused before anything was written.
    assert!(mock.ops.iter().all(|op| matches!(op, Op::R(..))));
    assert_eq!(mock.log[0], "chip RTL8153 VER_05");
}

#[test]
fn an_unknown_chip_is_refused() {
    let mut mock = Mock::new();
    mock.set_version(0x7000);
    let mut dev = Rtl8152::new(mock);
    assert_eq!(dev.bring_up(), Err(Error::UnknownChip { tcr: 0x7000 }));
    assert!(dev.transport().ops.iter().all(|op| matches!(op, Op::R(..))));
}

#[test]
fn the_version_read_is_retried_as_in_linux() {
    let mut dev = Rtl8152::new(Mock::new());
    dev.transport().fail_reads_left = 2;
    assert_eq!(dev.read_version(), Ok(Version::V02));

    let mut dev = Rtl8152::new(Mock::new());
    dev.transport().fail_reads_left = 3;
    assert_eq!(
        dev.read_version(),
        Err(Error::Transport(TransportError::Other))
    );

    let mut dev = Rtl8152::new(Mock::new());
    dev.transport().unplugged = true;
    assert_eq!(
        dev.read_version(),
        Err(Error::Transport(TransportError::Disconnected))
    );
}

#[test]
fn the_factory_address_comes_from_the_backup_register() {
    let mut dev = brought_up(Mock::new());
    assert_eq!(dev.mac_addr(), MAC);
    // Not in PLA_IDR yet; the first link up puts it there.
    assert_eq!(dev.transport().pla_read(0xc000) as u8, PLACEHOLDER[0]);
    dev.transport().set_status(0x0b);
    dev.poll_link(0).unwrap();
    let mock = dev.transport();
    assert_eq!(mock.pla_read(0xc000).to_le_bytes(), MAC[..4]);
    assert_eq!(
        mock.pla_read(0xc004) as u16,
        u16::from_le_bytes([MAC[4], MAC[5]])
    );
}

#[test]
fn ver_01_reads_pla_idr_first() {
    let mut mock = Mock::new();
    mock.set_version(0x4c00);
    set_mac(&mut mock.pla, 0xc000, MAC);
    set_mac(&mut mock.pla, 0xd000, PLACEHOLDER);
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    assert_eq!(dev.mac_addr(), MAC);
    assert_eq!(dev.transport().ops[1], Op::R(0xc000, PLA, 8));
}

#[test]
fn an_unusable_address_falls_back_to_the_other_register() {
    for bad in [[0u8; 6], [0xff; 6], [0x01, 0, 0, 0, 0, 1]] {
        let mut mock = Mock::new();
        set_mac(&mut mock.pla, 0xd000, bad);
        set_mac(&mut mock.pla, 0xc000, MAC);
        let mut dev = Rtl8152::new(mock);
        dev.bring_up().unwrap();
        assert_eq!(dev.mac_addr(), MAC);
    }
}

#[test]
fn no_usable_address_anywhere_is_an_error() {
    let mut mock = Mock::new();
    set_mac(&mut mock.pla, 0xc000, [0; 6]);
    set_mac(&mut mock.pla, 0xd000, [0; 6]);
    let mut dev = Rtl8152::new(mock);
    assert_eq!(dev.bring_up(), Err(Error::NoValidMac));
    let ops = dev.transport().take_ops();
    assert!(
        ops.iter().all(|op| matches!(op, Op::R(..))),
        "nothing written"
    );
}

#[test]
fn a_powered_down_phy_is_woken_with_only_that_bit_changed() {
    let mut mock = Mock::new();
    mock.phy.insert(0xa400, 0x7949_1940); // BMCR with BMCR_PDOWN
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    let ops = dev.transport().take_ops();
    let read = ops.iter().position(|op| *op == r(PLA, 0xb400)).unwrap();
    assert_eq!(ops[read + 1], w(PLA, 0x33, 0xb400, 0x1140));
}

#[test]
fn the_pdown_clear_is_the_first_mdio_write_of_the_bring_up() {
    let mut mock = Mock::new();
    mock.phy.insert(0xa400, 0x7949_1940); // BMCR with BMCR_PDOWN
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    let ops = dev.transport().take_ops();
    let read = ops.iter().position(|op| *op == r(PLA, 0xb400)).unwrap();
    let first_mdio_write = ops
        .iter()
        .position(|op| matches!(op, Op::W(addr, _, _) if (0xb400..=0xb40c).contains(addr)))
        .unwrap();
    assert_eq!(first_mdio_write, read + 1);
    assert_eq!(ops[read + 1], w(PLA, 0x33, 0xb400, 0x1140));
}

#[test]
fn full_speed_gets_the_slower_rx_fifo_thresholds() {
    let mut mock = Mock::new();
    mock.max_packet = 64;
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    let ops = dev.transport().take_ops();
    assert!(ops.contains(&w(PLA, 0xff, 0xc0a4, 0x60)));
    assert!(ops.contains(&w(PLA, 0xff, 0xc0a8, 0x78)));
    assert!(!ops.contains(&w(PLA, 0xff, 0xc0a4, 0x38)));
}

#[test]
fn a_chip_bit_that_never_comes_is_counted_and_bring_up_goes_on() {
    let mut mock = Mock::new();
    mock.pla.insert(0xe84c, 0x8000_0000); // the link list never gets ready
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    assert_eq!(dev.stats().wait_timeouts, 2);
    assert!(
        dev.transport()
            .log
            .iter()
            .any(|line| line.starts_with("timeout waiting for register 0xe84c"))
    );
    // Bounded: U-Boot's `while (--timeout)` with a 1 ms delay per poll.
    let delays = dev
        .transport()
        .ops
        .iter()
        .filter(|op| **op == Op::Delay(1))
        .count();
    assert_eq!(delays, 2 * (MAX_WAIT_MS as usize - 1));
}

#[test]
fn a_nic_reset_that_takes_a_while_is_waited_for() {
    let mut mock = Mock::new();
    mock.nic_reset_takes = 5;
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    assert_eq!(dev.stats().wait_timeouts, 0);
    let polls = dev
        .transport()
        .ops
        .iter()
        .filter(|op| **op == r(PLA, 0xe810))
        .count();
    assert_eq!(polls, 1 + 6); // the read before the reset, five with the bit, one without
}

#[test]
fn a_phy_reset_that_does_not_end_is_counted_after_a_second() {
    let mut mock = Mock::new();
    mock.bmcr_reset_takes = 1000;
    let mut dev = Rtl8152::new(mock);
    dev.bring_up().unwrap();
    assert_eq!(dev.stats().wait_timeouts, 1);
    let delays = dev
        .transport()
        .ops
        .iter()
        .filter(|op| **op == Op::Delay(20))
        .count();
    assert_eq!(delays, 50 + 2); // the polls and the two ALDPS settling times
}

#[test]
fn transport_errors_stop_the_bring_up() {
    let mut dev = Rtl8152::new(Mock::new());
    dev.transport().unplugged = true;
    assert_eq!(
        dev.bring_up(),
        Err(Error::Transport(TransportError::Disconnected))
    );

    let mut mock = Mock::new();
    mock.short_writes = true;
    let mut dev = Rtl8152::new(mock);
    assert_eq!(
        dev.bring_up(),
        Err(Error::ShortTransfer {
            expected: 4,
            actual: 3
        })
    );

    let mut mock = Mock::new();
    mock.short_reads = true;
    let mut dev = Rtl8152::new(mock);
    assert_eq!(
        dev.bring_up(),
        Err(Error::ShortTransfer {
            expected: 4,
            actual: 3
        })
    );
}

#[test]
fn no_link_after_bring_up_means_only_the_status_read() {
    let mut dev = brought_up(Mock::new());
    assert_eq!(dev.poll_link(0), Ok(None));
    assert_eq!(dev.transport().take_ops(), std::vec![r(PLA, 0xe908)]);
}

#[test]
fn the_link_coming_up_enables_the_mac_once() {
    let mut dev = brought_up(Mock::new());
    dev.transport().set_status(0x0b);
    let link = dev.poll_link(1000).unwrap().unwrap();
    assert_eq!(link.speed, Speed::Mbps100);
    assert!(link.full_duplex);
    assert_eq!(link.to_string(), "100/full");
    let mut expected = std::vec![r(PLA, 0xe908)];
    expected.extend(golden_enable());
    assert_eq!(dev.transport().take_ops(), expected);

    assert_eq!(dev.poll_link(1000), Ok(Some(link)));
    assert_eq!(dev.transport().take_ops(), std::vec![r(PLA, 0xe908)]);
    assert_eq!(dev.transport().log.last().unwrap(), "link up 100/full");
}

#[test]
fn link_text_for_each_status() {
    for (status, speed, text) in [
        (0x0b, Speed::Mbps100, "100/full"),
        (0x0a, Speed::Mbps100, "100/half"),
        (0x07, Speed::Mbps10, "10/full"),
        (0x06, Speed::Mbps10, "10/half"),
        (0x13, Speed::Mbps1000, "1000/full"),
        (0x03, Speed::Unknown, "?/full (status 0x03)"),
    ] {
        let link = Link::from_status(status);
        assert_eq!(link.speed, speed);
        assert_eq!(link.to_string(), text);
    }
}

#[test]
fn the_cable_going_out_writes_nothing_as_in_openbsd() {
    let mut dev = linked_up();
    dev.transport().set_status(0x00);
    assert_eq!(dev.poll_link(1000), Ok(None));
    assert_eq!(dev.transport().take_ops(), std::vec![r(PLA, 0xe908)]);
    assert_eq!(dev.transport().log.last().unwrap(), "link down");
    // Still none while it stays out.
    assert_eq!(dev.poll_link(1000), Ok(None));
    assert_eq!(dev.transport().take_ops(), std::vec![r(PLA, 0xe908)]);
}

#[test]
fn the_cable_back_in_enables_it_again() {
    let mut dev = linked_up();
    dev.transport().set_status(0x00);
    dev.poll_link(1000).unwrap();
    dev.transport().take_ops();
    dev.transport().set_status(0x0b);
    assert!(dev.poll_link(1000).unwrap().is_some());
    let mut expected = std::vec![r(PLA, 0xe908)];
    expected.extend(golden_enable());
    assert_eq!(dev.transport().take_ops(), expected);
}

#[test]
fn a_failed_enable_is_tried_again_at_the_next_poll() {
    let mut dev = brought_up(Mock::new());
    dev.transport().set_status(0x0b);
    // The status read succeeds, the first write of the enable fails.
    dev.transport().fail_writes_next = Some(TransportError::Other);
    assert_eq!(
        dev.poll_link(1000),
        Err(Error::Transport(TransportError::Other))
    );
    assert_eq!(dev.link(), None);
    assert!(dev.poll_link(1000).unwrap().is_some());
}

#[test]
fn twenty_quiet_seconds_of_link_are_logged_once() {
    let mut dev = linked_up();
    for _ in 0..19 {
        dev.poll_link(1000).unwrap();
    }
    assert!(!dev.transport().log.iter().any(|l| l.contains("no bulk IN")));
    dev.poll_link(1000).unwrap();
    dev.poll_link(1000).unwrap();
    let quiet = dev
        .transport()
        .log
        .iter()
        .filter(|l| l.contains("no bulk IN"))
        .count();
    assert_eq!(quiet, 1);
    assert_eq!(
        dev.transport().log.last().unwrap(),
        "link up for 20 s and no bulk IN transfer completed, 0 frames sent"
    );
}

#[test]
fn any_received_transfer_keeps_the_quiet_log_away() {
    let mut dev = linked_up();
    dev.transport().bulk_in.push_back(Vec::new());
    let mut out = [0u8; 1518];
    assert_eq!(dev.receive(&mut out), Ok(None));
    for _ in 0..30 {
        dev.poll_link(1000).unwrap();
    }
    assert!(!dev.transport().log.iter().any(|l| l.contains("no bulk IN")));
}

#[test]
fn send_writes_one_transfer_with_the_descriptor() {
    let mut dev = linked_up();
    let f = frame(60, 1);
    dev.send(&f).unwrap();
    let mut expected = std::vec![60u8, 0, 0, 0xc0, 0, 0, 0, 0];
    expected.extend_from_slice(&f);
    assert_eq!(dev.transport().take_ops(), std::vec![Op::BulkOut(expected)]);
    assert_eq!(dev.stats().tx_frames, 1);
    assert_eq!(
        dev.transport().log.last().unwrap(),
        "first frame sent, 60 bytes"
    );
    dev.send(&f).unwrap();
    assert_eq!(dev.stats().tx_frames, 2);
    assert_eq!(
        dev.transport().log.last().unwrap(),
        "first frame sent, 60 bytes",
        "only the first one is logged"
    );
}

#[test]
fn send_frames_packs_a_transfer_and_keeps_what_did_not_fit_for_the_next() {
    let mut dev = linked_up();
    let mut queue: VecDeque<Vec<u8>> = (0..13).map(|i| frame(1514, i as u8)).collect();
    let mut next = || queue.pop_front();
    assert_eq!(dev.send_frames(&mut next, 64), Ok(10));
    assert_eq!(dev.send_frames(&mut next, 64), Ok(3));
    assert_eq!(dev.send_frames(&mut next, 64), Ok(0));
    let ops = dev.transport().take_ops();
    assert_eq!(ops.len(), 2, "two transfers: {}", ops.len());
    let Op::BulkOut(first) = &ops[0] else {
        panic!("{:?}", ops[0])
    };
    assert_eq!(first[..4], [0xea, 0x05, 0, 0xc0]);
    assert_eq!(first[8..1522], frame(1514, 0)[..]);
    assert_eq!(
        first[1524..1528],
        [0xea, 0x05, 0, 0xc0],
        "the next one on a 4-byte boundary"
    );
    let Op::BulkOut(second) = &ops[1] else {
        panic!("{:?}", ops[1])
    };
    assert_eq!(
        second[8..1522],
        frame(1514, 10)[..],
        "the frame that did not fit goes first"
    );
    assert_eq!(dev.stats().tx_frames, 13);
    assert_eq!(dev.stats().tx_transfers, 2);
}

#[test]
fn send_frames_takes_nothing_while_the_transport_is_full() {
    let mut dev = linked_up();
    dev.transport().tx_full = true;
    let mut queue: VecDeque<Vec<u8>> = VecDeque::from([frame(60, 1)]);
    let mut next = || queue.pop_front();
    assert_eq!(dev.send_frames(&mut next, 64), Ok(0));
    assert_eq!(queue.len(), 1, "it stays queued");
    assert_eq!(dev.transport().take_ops(), std::vec![]);
}

#[test]
fn send_frames_stops_at_the_limit_and_skips_what_is_no_frame() {
    let mut dev = linked_up();
    let mut queue: VecDeque<Vec<u8>> =
        VecDeque::from([frame(60, 1), std::vec![0; 3], frame(60, 2), frame(60, 3)]);
    let mut next = || queue.pop_front();
    assert_eq!(dev.send_frames(&mut next, 2), Ok(2));
    assert_eq!(dev.stats().tx_refused, 1);
    drop(next);
    assert_eq!(queue.len(), 1);
}

#[test]
fn send_keeps_no_zero_length_packet_even_on_a_packet_boundary() {
    let mut dev = linked_up();
    let f = frame(504, 1); // 504 + 8 = 512
    dev.send(&f).unwrap();
    let ops = dev.transport().take_ops();
    assert_eq!(ops.len(), 1);
    match &ops[0] {
        Op::BulkOut(data) => assert_eq!(data.len(), 512),
        other => panic!("{:?}", other),
    }
}

#[test]
fn send_refuses_what_is_no_frame() {
    let mut dev = linked_up();
    assert_eq!(dev.send(&[0; 13]), Err(Error::Tx(TxError::TooShort(13))));
    assert_eq!(dev.send(&[0; 1519]), Err(Error::Tx(TxError::TooLong(1519))));
    assert!(dev.transport().take_ops().is_empty());
}

#[test]
fn receive_hands_out_the_frames_of_a_transfer_one_by_one() {
    let mut dev = linked_up();
    let (a, b) = (frame(60, 1), frame(1514, 2));
    dev.transport().bulk_in.push_back(batch(&[&a, &b]));
    let mut out = [0u8; 1518];
    assert_eq!(dev.receive(&mut out), Ok(Some(60)));
    assert_eq!(&out[..60], &a[..]);
    assert_eq!(dev.receive(&mut out), Ok(Some(1514)));
    assert_eq!(&out[..1514], &b[..]);
    assert_eq!(dev.receive(&mut out), Ok(None));
    let s = dev.stats();
    assert_eq!((s.rx_frames, s.rx_transfers, s.rx_frame_errors), (2, 1, 0));
    assert!(
        dev.transport()
            .log
            .contains(&"first frame received, 60 bytes".to_string())
    );
}

#[test]
fn receive_goes_on_to_the_next_transfer() {
    let mut dev = linked_up();
    let (a, b) = (frame(60, 1), frame(61, 2));
    dev.transport().bulk_in.push_back(Vec::new()); // a zero-length transfer carries nothing
    dev.transport().bulk_in.push_back(batch(&[&a]));
    dev.transport().bulk_in.push_back(batch(&[&b]));
    let mut out = [0u8; 1518];
    assert_eq!(dev.receive(&mut out), Ok(Some(60)));
    assert_eq!(dev.receive(&mut out), Ok(Some(61)));
    assert_eq!(dev.stats().rx_transfers, 3);
}

#[test]
fn a_malformed_transfer_is_counted_and_the_next_one_works() {
    let mut dev = linked_up();
    let mut bad = std::vec![0u8; 100];
    bad[..4].copy_from_slice(&0x7fffu32.to_le_bytes());
    dev.transport().bulk_in.push_back(bad);
    let good = frame(60, 1);
    dev.transport().bulk_in.push_back(batch(&[&good]));
    let mut out = [0u8; 1518];
    assert_eq!(dev.receive(&mut out), Ok(Some(60)));
    assert_eq!(dev.stats().rx_frame_errors, 1);
    assert_eq!(dev.stats().rx_frames, 1);
}

#[test]
fn a_frame_larger_than_the_buffer_is_counted_not_cut() {
    let mut dev = linked_up();
    let (big, small) = (frame(100, 1), frame(60, 2));
    dev.transport().bulk_in.push_back(batch(&[&big, &small]));
    let mut out = [0u8; 80];
    assert_eq!(dev.receive(&mut out), Ok(Some(60)));
    assert_eq!(dev.stats().rx_frame_errors, 1);
}

#[test]
fn receive_passes_transport_errors_on() {
    let mut dev = linked_up();
    dev.transport().fail_next = Some(TransportError::Stall);
    let mut out = [0u8; 1518];
    assert_eq!(
        dev.receive(&mut out),
        Err(Error::Transport(TransportError::Stall))
    );
}

#[test]
fn the_link_going_down_forgets_buffered_frames() {
    let mut dev = linked_up();
    dev.transport()
        .bulk_in
        .push_back(batch(&[&frame(60, 1), &frame(60, 2)]));
    let mut out = [0u8; 1518];
    assert_eq!(dev.receive(&mut out), Ok(Some(60)));
    dev.transport().set_status(0);
    dev.poll_link(1000).unwrap();
    dev.transport().set_status(0x0b);
    dev.poll_link(1000).unwrap();
    assert_eq!(dev.receive(&mut out), Ok(None));
}

// Through the pump, as the kernel's USB thread drives it.

struct Pumped {
    dev: Rtl8152<Mock>,
    pump: Pump,
    now: u64,
    tx: VecDeque<Vec<u8>>,
    rx: Vec<Vec<u8>>,
}

impl Pumped {
    fn new() -> Self {
        let dev = brought_up(Mock::new());
        let pump = Pump::new(&dev);
        Self {
            dev,
            pump,
            now: 0,
            tx: VecDeque::new(),
            rx: Vec::new(),
        }
    }

    fn pass(&mut self) -> lucina_usbnet::Pass {
        let tx = &mut self.tx;
        let rx = &mut self.rx;
        let pass = self.pump.pass(
            &mut self.dev,
            self.now,
            &mut || tx.pop_front(),
            &mut |frame| {
                rx.push(frame.to_vec());
                true
            },
        );
        self.now += 1000;
        pass
    }
}

#[test]
fn through_the_pump_link_frames_and_unplug() {
    let mut p = Pumped::new();
    assert_eq!(EthernetBackend::mac_addr(&p.dev), MAC);
    assert_eq!(p.dev.max_frame_len(), 1518);

    // No link yet: no bulk IN is touched, frames to send are dropped by the pump.
    p.tx.push_back(frame(60, 1));
    let pass = p.pass();
    assert!(pass.events.is_empty());
    assert_eq!(pass.sent, 0);
    assert_eq!(p.dev.transport().bulk_in_polls, 0);
    assert_eq!(p.pump.counters().tx_dropped, 1);

    // The link comes up: the text the test plan reads.
    p.dev.transport().set_status(0x0b);
    let pass = p.pass();
    assert_eq!(
        pass.events,
        std::vec![Event::LinkUp("RTL8152 VER_02, 100/full".to_string())]
    );

    // A frame each way.
    let (out, inc) = (frame(100, 2), frame(200, 3));
    p.tx.push_back(out.clone());
    p.dev.transport().bulk_in.push_back(batch(&[&inc]));
    let pass = p.pass();
    assert_eq!((pass.sent, pass.received), (1, 1));
    assert_eq!(p.rx, std::vec![inc]);
    let sent = p
        .dev
        .transport()
        .ops
        .iter()
        .rev()
        .find_map(|op| match op {
            Op::BulkOut(data) => Some(data.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(&sent[8..], &out[..]);

    // Cable out: the counters the test plan reads.
    p.dev.transport().set_status(0);
    let pass = p.pass();
    assert_eq!(
        pass.events,
        std::vec![Event::LinkDown(
            "tx_frames 1 tx_transfers 1 tx_refused 0 rx_frames 1 rx_transfers 1 rx_frame_errors 0 wait_timeouts 0".to_string()
        )]
    );

    // Unplugged: the pump gives the adapter up at once.
    p.dev.transport().unplugged = true;
    let pass = p.pass();
    assert!(pass.gone());
}

#[test]
fn backend_errors_tell_gone_from_failed() {
    let mut dev = linked_up();
    dev.transport().fail_next = Some(TransportError::Other);
    assert_eq!(
        EthernetBackend::send(&mut dev, &frame(60, 1)),
        Err(BackendError::Failed)
    );
    assert_eq!(
        EthernetBackend::send(&mut dev, &[0; 3]),
        Err(BackendError::Failed)
    );
    dev.transport().unplugged = true;
    assert_eq!(
        EthernetBackend::send(&mut dev, &frame(60, 1)),
        Err(BackendError::Gone)
    );
    let mut out = [0u8; 1518];
    assert_eq!(
        EthernetBackend::receive(&mut dev, &mut out),
        Err(BackendError::Gone)
    );
    assert_eq!(
        EthernetBackend::poll_link(&mut dev, 1000),
        Err(BackendError::Gone)
    );
}

#[test]
fn link_text_while_down() {
    let dev = brought_up(Mock::new());
    assert_eq!(dev.link_text(), "down");
}

// The register layer in the terms of U-Boot's functions.

fn ocp() -> Ocp<Mock> {
    Ocp::new(Mock::new())
}

#[test]
fn ocp_writes_shift_the_value_to_its_bytes() {
    let mut o = ocp();
    o.write_word(Mcu::Pla, 0xe022, 0x000f).unwrap();
    o.write_word(Mcu::Usb, 0xd800, 0x1234).unwrap();
    o.write_byte(Mcu::Pla, 0xe813, 0x0c).unwrap();
    o.write_byte(Mcu::Usb, 0xd40a, 0x03).unwrap();
    o.write_byte(Mcu::Pla, 0xe81c, 0xc0).unwrap();
    o.write_byte(Mcu::Pla, 0xe909, 0x55).unwrap();
    o.write_dword(Mcu::Usb, 0xd40c, 0x7a12_0180).unwrap();
    assert_eq!(
        o.usb.take_ops(),
        std::vec![
            Op::W(0xe020, 0x01cc, std::vec![0, 0, 0x0f, 0]),
            Op::W(0xd800, 0x0033, std::vec![0x34, 0x12, 0, 0]),
            Op::W(0xe810, 0x0188, std::vec![0, 0, 0, 0x0c]),
            Op::W(0xd408, 0x0044, std::vec![0, 0, 0x03, 0]),
            Op::W(0xe81c, 0x0111, std::vec![0xc0, 0, 0, 0]),
            Op::W(0xe908, 0x0122, std::vec![0, 0x55, 0, 0]),
            Op::W(0xd40c, 0x00ff, std::vec![0x80, 0x01, 0x12, 0x7a]),
        ]
    );
}

#[test]
fn ocp_reads_shift_the_value_back() {
    let mut o = ocp();
    o.usb.pla.insert(0xe610, 0x4c10_0880);
    assert_eq!(o.read_word(Mcu::Pla, 0xe610), Ok(0x0880));
    assert_eq!(o.read_word(Mcu::Pla, 0xe612), Ok(0x4c10));
    o.usb.pla.insert(0xe84c, 0xb200_00c0);
    assert_eq!(o.read_byte(Mcu::Pla, 0xe84f), Ok(0xb2));
    assert_eq!(o.read_byte(Mcu::Pla, 0xe84c), Ok(0xc0));
    o.usb.pla.insert(0xe908, 0x0000_000b);
    assert_eq!(o.read_byte(Mcu::Pla, 0xe908), Ok(0x0b));
    assert_eq!(
        o.usb.take_ops(),
        std::vec![
            r(PLA, 0xe610),
            r(PLA, 0xe610),
            r(PLA, 0xe84c),
            r(PLA, 0xe84c),
            r(PLA, 0xe908),
        ]
    );
}

#[test]
fn ocp_block_write_as_generic_ocp_write() {
    // Eight bytes: first and last dword, nothing between. The six-byte enable gives 0x33 on the last.
    let mut o = ocp();
    o.write(
        Mcu::Pla,
        0xc000,
        BYTE_EN_SIX_BYTES,
        &[1, 2, 3, 4, 5, 6, 0, 0],
    )
    .unwrap();
    assert_eq!(
        o.usb.take_ops(),
        std::vec![
            Op::W(0xc000, 0x01ff, std::vec![1, 2, 3, 4]),
            Op::W(0xc004, 0x0133, std::vec![5, 6, 0, 0]),
        ]
    );
    // Long writes: the first dword, the middle in bursts of 512, the last dword (RTL8153's firmware writes need it).
    let data: Vec<u8> = (0..1040u32).map(|i| i as u8).collect();
    o.write(Mcu::Usb, 0x1000, BYTE_EN_DWORD, &data).unwrap();
    let ops = o.usb.take_ops();
    let lengths: Vec<(u16, usize)> = ops
        .iter()
        .map(|op| match op {
            Op::W(addr, _, data) => (*addr, data.len()),
            other => panic!("{:?}", other),
        })
        .collect();
    assert_eq!(
        lengths,
        std::vec![
            (0x1000, 4),
            (0x1004, 512),
            (0x1204, 512),
            (0x1404, 8),
            (0x140c, 4)
        ]
    );
}

#[test]
fn ocp_block_read_splits_at_64_bytes() {
    let mut o = ocp();
    let mut buf = [0u8; 72];
    o.read(Mcu::Pla, 0xc000, &mut buf).unwrap();
    assert_eq!(
        o.usb.take_ops(),
        std::vec![Op::R(0xc000, PLA, 64), Op::R(0xc040, PLA, 8)]
    );
}

#[test]
fn ocp_refuses_what_u_boot_refuses() {
    let mut o = ocp();
    let mut four = [0u8; 4];
    assert_eq!(
        o.read(Mcu::Pla, 0xc002, &mut four),
        Err(Error::BadAccess { addr: 0xc002 })
    );
    assert_eq!(
        o.read(Mcu::Pla, 0xc000, &mut four[..3]),
        Err(Error::BadAccess { addr: 0xc000 })
    );
    assert_eq!(
        o.read(Mcu::Pla, 0xc000, &mut []),
        Err(Error::BadAccess { addr: 0xc000 })
    );
    assert_eq!(
        o.read(Mcu::Pla, 0xfffc, &mut four),
        Err(Error::BadAccess { addr: 0xfffc })
    );
    assert_eq!(
        o.write(Mcu::Pla, 0xc001, BYTE_EN_DWORD, &four),
        Err(Error::BadAccess { addr: 0xc001 })
    );
    assert_eq!(
        o.write(Mcu::Pla, 0xc000, BYTE_EN_DWORD, &[]),
        Err(Error::BadAccess { addr: 0xc000 })
    );
    assert!(o.usb.take_ops().is_empty());
}

#[test]
fn the_phy_page_is_written_only_when_it_changes() {
    let mut o = ocp();
    o.mdio_write(MII_ADVERTISE, 0x01e1).unwrap();
    o.mdio_read(MII_ADVERTISE).unwrap();
    o.reg_write(OCP_ALDPS_CONFIG, 0x0310).unwrap();
    assert_eq!(
        o.usb.take_ops(),
        std::vec![
            phy_page(0xa000),
            w(PLA, 0x33, 0xb408, 0x01e1),
            r(PLA, 0xb408),
            phy_page(0x2000),
            w(PLA, 0x33, 0xb010, 0x0310),
        ]
    );
    assert_eq!(o.usb.phy_word(0xa408), 0x01e1);
    assert_eq!(o.usb.phy_word(0x2010), 0x0310);
}

#[test]
fn a_phy_register_in_the_upper_half_of_its_dword() {
    let mut o = ocp();
    o.usb.phy.insert(0xa400, 0x796d_1140);
    assert_eq!(o.mdio_read(1), Ok(0x796d)); // BMSR
    assert_eq!(o.mdio_read(0), Ok(0x1140));
    o.mdio_write(1, 0x1234).unwrap();
    assert_eq!(o.usb.phy_word(0xa402), 0x1234);
    assert_eq!(o.usb.phy_word(0xa400), 0x1140);
}
