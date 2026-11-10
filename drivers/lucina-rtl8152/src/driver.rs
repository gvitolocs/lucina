//! The device: bring-up (U-Boot `r8152_eth_probe`: `r8152b_init`, `rtl8152_up`, `rtl8152_set_speed`), link polling
//! (`rtl8152_enable` at every link up, as Linux `set_carrier` and OpenBSD `ure_miibus_statchg`; nothing at link down, as
//! OpenBSD), send and receive.

use alloc::vec::Vec;
use core::fmt;

use lucina_usbnet::format_mac;

use crate::frame::{RxCursor, TxError, append_tx, encode_tx};
use crate::ocp::Ocp;
use crate::regs::*;
use crate::transport::{TransportError, UsbTransport};
use crate::version::{Family, Version};

/// One bulk IN transfer: Linux's `rx_buf_sz` for the RTL8152. The chip aggregates frames up to about this size.
pub const RX_BUF_LEN: usize = 16 * 1024;
/// U-Boot `R8152_WAIT_TIMEOUT`, in polls about a millisecond apart.
pub const MAX_WAIT_MS: u32 = 2000;
/// Linux's wait for the PHY's reset bit to clear: this many polls...
const PHY_RESET_POLLS: u32 = 50;
/// ... this far apart.
const PHY_RESET_POLL_MS: u32 = 20;
/// U-Boot's settling time after ALDPS is turned off.
const ALDPS_OFF_MS: u32 = 20;
/// Linux reads the version three times: "retrying a read of the version is always safe".
const VERSION_READS: u32 = 3;
/// With the link up and nothing received for this long, the log says so, once per link: on any LAN there are
/// broadcasts, so it is the RX path that is broken and the test wants to know.
pub const QUIET_LOG_MS: u32 = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Transport(TransportError),
    /// A register access or bulk OUT transfer moved fewer bytes than asked.
    ShortTransfer {
        expected: usize,
        actual: usize,
    },
    /// A register access that is not 4-byte aligned (a driver bug).
    BadAccess {
        addr: u16,
    },
    Tx(TxError),
    /// PLA_TCR1 holds bits no known chip has.
    UnknownChip {
        tcr: u16,
    },
    /// A known chip of a family this driver does not bring up yet.
    UnsupportedChip(Version),
    /// Neither PLA_IDR nor PLA_BACKUP holds a usable address.
    NoValidMac,
}

impl From<TransportError> for Error {
    fn from(e: TransportError) -> Self {
        Error::Transport(e)
    }
}

impl From<TxError> for Error {
    fn from(e: TxError) -> Self {
        Error::Tx(e)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Mbps10,
    Mbps100,
    Mbps1000,
    /// The link bit is on, but no speed bit.
    Unknown,
}

/// The negotiated Ethernet link, from PLA_PHYSTATUS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub speed: Speed,
    pub full_duplex: bool,
    /// The register's value, for the log.
    pub status: u8,
}

impl Link {
    fn from_status(status: u8) -> Self {
        let speed = if status & STATUS_1000 != 0 {
            Speed::Mbps1000
        } else if status & STATUS_100 != 0 {
            Speed::Mbps100
        } else if status & STATUS_10 != 0 {
            Speed::Mbps10
        } else {
            Speed::Unknown
        };
        Self {
            speed,
            full_duplex: status & STATUS_FULL_DUP != 0,
            status,
        }
    }
}

/// `100/full`
impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let duplex = if self.full_duplex { "full" } else { "half" };
        match self.speed {
            Speed::Mbps10 => write!(f, "10/{}", duplex),
            Speed::Mbps100 => write!(f, "100/{}", duplex),
            Speed::Mbps1000 => write!(f, "1000/{}", duplex),
            Speed::Unknown => write!(f, "?/{} (status {:#04x})", duplex, self.status),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx_frames: u64,
    /// Bulk OUT transfers; `tx_frames` over this is how well they are filled.
    pub tx_transfers: u64,
    /// Frames from the network stack that were no frame (`crate::frame::TxError`).
    pub tx_refused: u64,
    pub rx_frames: u64,
    pub rx_transfers: u64,
    /// Things in received transfers that were no frame (`crate::frame::RxError`).
    pub rx_frame_errors: u64,
    /// Waits for a chip bit that gave up after [`MAX_WAIT_MS`] polls (U-Boot carries on; so does this driver).
    pub wait_timeouts: u32,
}

pub struct Rtl8152<T> {
    ocp: Ocp<T>,
    version: Option<Version>,
    mac: [u8; 6],
    link: Option<Link>,
    /// Where the driver is in the transport's current bulk IN transfer.
    rx_cursor: Option<RxCursor>,
    /// A frame that did not fit into the last transmit transfer: the first of the next one.
    tx_held: Option<Vec<u8>>,
    stats: Stats,
    /// Time with the link up and nothing received, for [`QUIET_LOG_MS`].
    quiet_ms: u32,
    quiet_logged: bool,
}

impl<T> fmt::Debug for Rtl8152<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rtl8152")
            .field("version", &self.version)
            .field("mac", &self.mac)
            .field("link", &self.link)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

fn valid_unicast(mac: &[u8; 6]) -> bool {
    mac[0] & 1 == 0 && mac.iter().any(|&b| b != 0)
}

impl<T: UsbTransport> Rtl8152<T> {
    /// Takes the transport. Nothing is sent to the chip yet.
    pub fn new(usb: T) -> Self {
        Self {
            ocp: Ocp::new(usb),
            version: None,
            mac: [0; 6],
            link: None,
            rx_cursor: None,
            tx_held: None,
            stats: Stats::default(),
            quiet_ms: 0,
            quiet_logged: false,
        }
    }

    /// U-Boot `r8152_eth_probe`: reads the chip version and the MAC address, brings the chip out of its power-on state
    /// and starts autonegotiation, logging each step. The link is not waited for: [`Rtl8152::poll_link`] enables the
    /// MAC when it comes up.
    ///
    /// On an error the transport can be taken back with [`Rtl8152::into_transport`], for instance to try the
    /// adapter's CDC-ECM configuration instead.
    pub fn bring_up(&mut self) -> Result<(), Error> {
        let version = self.read_version()?;
        self.version = Some(version);
        self.log(format_args!("chip {}", version));
        if version.family() != Family::Rtl8152 {
            return Err(Error::UnsupportedChip(version));
        }
        self.mac = self.read_mac(version)?;
        let mac = self.mac;
        self.log(format_args!("MAC {}", format_mac(mac)));
        self.r8152b_init(version)?;
        self.log(format_args!("init done"));
        self.r8152_up()?;
        self.log(format_args!(
            "out of OOB mode, no firmware patch (as Linux and OpenBSD)"
        ));
        self.set_speed_auto_100_full()?;
        self.log(format_args!("autonegotiating, advertising 10/100"));
        Ok(())
    }

    /// One read of PLA_PHYSTATUS. When the link comes up, enables the MAC: Linux and OpenBSD both do that at every
    /// link up, "the media settings are getting reset" by a link change. Nothing is written at link down, as in
    /// OpenBSD: Linux's NIC reset there comes after it has cancelled its bulk transfers, which crab-usb cannot do.
    /// Call it about once a second with the time since the previous call.
    pub fn poll_link(&mut self, elapsed_ms: u32) -> Result<Option<Link>, Error> {
        let status = self.ocp.read_byte(Mcu::Pla, PLA_PHYSTATUS)?;
        let up = status & STATUS_LINK != 0;
        if up && self.link.is_none() {
            self.rtl8152_enable()?;
            let link = Link::from_status(status);
            self.link = Some(link);
            self.quiet_ms = 0;
            self.quiet_logged = false;
            self.log(format_args!("link up {}", link));
        } else if !up && self.link.is_some() {
            self.link = None;
            self.rx_cursor = None;
            self.log(format_args!("link down"));
        } else if up {
            self.note_quiet(elapsed_ms);
        }
        Ok(self.link)
    }

    fn note_quiet(&mut self, elapsed_ms: u32) {
        if self.stats.rx_transfers > 0 || self.quiet_logged {
            return;
        }
        self.quiet_ms = self.quiet_ms.saturating_add(elapsed_ms);
        if self.quiet_ms >= QUIET_LOG_MS {
            self.quiet_logged = true;
            let (seconds, sent) = (self.quiet_ms / 1000, self.stats.tx_frames);
            self.log(format_args!(
                "link up for {} s and no bulk IN transfer completed, {} frames sent",
                seconds, sent
            ));
        }
    }

    /// Sends one Ethernet frame (no FCS) as one bulk transfer.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), Error> {
        let data = encode_tx(frame)?;
        self.ocp.usb.bulk_out(data)?;
        self.stats.tx_frames += 1;
        if self.stats.tx_frames == 1 {
            self.log(format_args!("first frame sent, {} bytes", frame.len()));
        }
        Ok(())
    }

    /// Packs frames from `next`, at most `limit`, into one bulk OUT transfer and hands it over. Returns how many went:
    /// 0 if the transport has no room for a transfer, which leaves them in `next`, or if `next` has none. Never waits.
    pub fn send_frames(
        &mut self,
        next: &mut dyn FnMut() -> Option<Vec<u8>>,
        limit: usize,
    ) -> Result<usize, Error> {
        if !self.ocp.usb.bulk_out_ready()? {
            return Ok(0);
        }
        let mut agg = Vec::new();
        let mut frames = 0;
        while frames < limit {
            let Some(frame) = self.tx_held.take().or_else(&mut *next) else {
                break;
            };
            match append_tx(&mut agg, &frame) {
                Ok(true) => frames += 1,
                Ok(false) => {
                    self.tx_held = Some(frame);
                    break;
                }
                Err(_) => self.stats.tx_refused += 1,
            }
        }
        if frames == 0 {
            return Ok(0);
        }
        let bytes = agg.len();
        self.ocp.usb.bulk_out(agg)?;
        self.stats.tx_transfers += 1;
        self.stats.tx_frames += frames as u64;
        if self.stats.tx_frames == frames as u64 {
            self.log(format_args!(
                "first transfer sent, {} frames in {} bytes",
                frames, bytes
            ));
        }
        Ok(frames)
    }

    /// Copies the next received frame into `out` and returns its length, or `None` when nothing is waiting. A bulk IN
    /// transfer is parsed where the transport keeps it: this copy is the only one in the driver.
    pub fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, Error> {
        loop {
            if let Some(n) = self.next_frame(out) {
                return Ok(Some(n));
            }
            if self.ocp.usb.bulk_in()?.is_none() {
                return Ok(None);
            }
            self.stats.rx_transfers += 1;
            self.rx_cursor = Some(RxCursor::new());
        }
    }

    pub fn mac_addr(&self) -> [u8; 6] {
        self.mac
    }

    pub fn version(&self) -> Option<Version> {
        self.version
    }

    pub fn link(&self) -> Option<Link> {
        self.link
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.ocp.usb
    }

    /// Gives the transport back.
    pub fn into_transport(self) -> T {
        self.ocp.usb
    }

    fn log(&mut self, message: fmt::Arguments<'_>) {
        self.ocp.usb.log(message);
    }

    fn next_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        let mut cursor = self.rx_cursor?;
        let buf = self.ocp.usb.rx_data();
        let mut delivered = None;
        while let Some(item) = cursor.next(buf) {
            match item {
                Ok(range) if range.len() <= out.len() => {
                    out[..range.len()].copy_from_slice(&buf[range.clone()]);
                    delivered = Some(range.len());
                    break;
                }
                Ok(_) | Err(_) => self.stats.rx_frame_errors += 1,
            }
        }
        self.rx_cursor = cursor.has_more(buf.len()).then_some(cursor);
        if let Some(n) = delivered {
            self.stats.rx_frames += 1;
            if self.stats.rx_frames == 1 {
                self.log(format_args!("first frame received, {} bytes", n));
            }
        }
        delivered
    }

    /// Linux and OpenBSD: PLA_IDR on VER_01, PLA_BACKUP on the others. On a cold chip PLA_IDR may hold a Realtek
    /// placeholder (m3OS saw that) and the real address is in the backup; [`Rtl8152::rtl8152_enable`] copies it to
    /// PLA_IDR, which is what the unicast filter compares with. The other register is the fallback.
    fn read_mac(&mut self, version: Version) -> Result<[u8; 6], Error> {
        let order = if version == Version::V01 {
            [PLA_IDR, PLA_BACKUP]
        } else {
            [PLA_BACKUP, PLA_IDR]
        };
        for addr in order {
            let mut raw = [0u8; 8];
            self.ocp.read(Mcu::Pla, addr, &mut raw)?;
            let mut mac = [0u8; 6];
            mac.copy_from_slice(&raw[..6]);
            if valid_unicast(&mac) {
                return Ok(mac);
            }
        }
        Err(Error::NoValidMac)
    }

    fn read_version(&mut self) -> Result<Version, Error> {
        let mut tcr = 0;
        for read in 1..=VERSION_READS {
            match self.ocp.read_word(Mcu::Pla, PLA_TCR1) {
                Ok(value) => {
                    tcr = value & VERSION_MASK;
                    break;
                }
                Err(Error::Transport(TransportError::Disconnected)) => {
                    return Err(TransportError::Disconnected.into());
                }
                Err(error) if read == VERSION_READS => return Err(error),
                Err(_) => {}
            }
        }
        Version::from_tcr(tcr).ok_or(Error::UnknownChip { tcr })
    }

    fn update_word(
        &mut self,
        mcu: Mcu,
        addr: u16,
        change: impl FnOnce(u16) -> u16,
    ) -> Result<(), Error> {
        let value = self.ocp.read_word(mcu, addr)?;
        self.ocp.write_word(mcu, addr, change(value))
    }

    fn update_dword(
        &mut self,
        mcu: Mcu,
        addr: u16,
        change: impl FnOnce(u32) -> u32,
    ) -> Result<(), Error> {
        let value = self.ocp.read_dword(mcu, addr)?;
        self.ocp.write_dword(mcu, addr, change(value))
    }

    fn update_byte(
        &mut self,
        mcu: Mcu,
        addr: u16,
        change: impl FnOnce(u8) -> u8,
    ) -> Result<(), Error> {
        let value = self.ocp.read_byte(mcu, addr)?;
        self.ocp.write_byte(mcu, addr, change(value))
    }

    /// `r8152_wait_for_bit`: polls a dword until all of `mask` is set (`set`) or clear. Giving up is not an error
    /// (U-Boot logs it and goes on), it is counted and logged.
    fn wait_for_bits(&mut self, addr: u16, mask: u32, set: bool) -> Result<(), Error> {
        for _ in 1..MAX_WAIT_MS {
            let value = self.ocp.read_dword(Mcu::Pla, addr)?;
            let value = if set { value } else { !value };
            if value & mask == mask {
                return Ok(());
            }
            self.ocp.usb.delay_ms(1);
        }
        self.stats.wait_timeouts += 1;
        self.log(format_args!(
            "timeout waiting for register {:#06x} mask {:#010x} {}",
            addr,
            mask,
            if set { "set" } else { "clear" }
        ));
        Ok(())
    }

    fn aldps_off(&mut self) -> Result<(), Error> {
        self.ocp
            .reg_write(OCP_ALDPS_CONFIG, ENPDNPS | LINKENA | DIS_SDSAVE)?;
        self.ocp.usb.delay_ms(ALDPS_OFF_MS);
        Ok(())
    }

    fn aldps_on(&mut self) -> Result<(), Error> {
        self.ocp
            .reg_write(OCP_ALDPS_CONFIG, ENPWRSAVE | ENPDNPS | LINKENA | DIS_SDSAVE)
    }

    /// `rxdy_gated_en`
    fn gate_rx(&mut self, gated: bool) -> Result<(), Error> {
        self.update_word(Mcu::Pla, PLA_MISC_1, |value| {
            if gated {
                value | RXDY_GATED_EN
            } else {
                value & !RXDY_GATED_EN
            }
        })
    }

    /// `rtl8152_nic_reset`
    fn nic_reset(&mut self) -> Result<(), Error> {
        self.update_dword(Mcu::Pla, BIST_CTRL, |value| value | BIST_CTRL_SW_RESET)?;
        self.wait_for_bits(BIST_CTRL, BIST_CTRL_SW_RESET, false)
    }

    /// `r8152b_init`
    fn r8152b_init(&mut self, version: Version) -> Result<(), Error> {
        // r8152_mdio_test_and_clr_bit(MII_BMCR, BMCR_PDOWN), first, as Linux's r8152b_init has it.
        let bmcr = self.ocp.mdio_read(MII_BMCR)?;
        if bmcr & BMCR_PDOWN != 0 {
            self.ocp.mdio_write(MII_BMCR, bmcr & !BMCR_PDOWN)?;
        }
        self.aldps_off()?;
        if version == Version::V01 {
            self.update_word(Mcu::Pla, PLA_LED_FEATURE, |value| value & !LED_MODE_MASK)?;
        }
        // r8152_power_cut_en(false)
        self.update_word(Mcu::Usb, USB_UPS_CTRL, |value| value & !POWER_CUT)?;
        self.update_word(Mcu::Usb, USB_PM_CTRL_STATUS, |value| {
            value & !RESUME_INDICATE
        })?;

        self.update_word(Mcu::Pla, PLA_PHY_PWR, |value| {
            value | TX_10M_IDLE_EN | PFM_PWM_SWITCH
        })?;
        self.update_dword(Mcu::Pla, PLA_MAC_PWR_CTRL, |value| {
            (value & !MCU_CLK_RATIO_MASK) | MCU_CLK_RATIO | D3_CLK_GATED_EN
        })?;
        self.ocp.write_word(
            Mcu::Pla,
            PLA_GPHY_INTR_IMR,
            GPHY_STS_MSK | SPEED_DOWN_MSK | SPDWN_RXDV_MSK | SPDWN_LINKCHG_MSK,
        )?;

        // r8152b_enable_fc
        let advertise = self.ocp.mdio_read(MII_ADVERTISE)?;
        self.ocp.mdio_write(
            MII_ADVERTISE,
            advertise | ADVERTISE_PAUSE_CAP | ADVERTISE_PAUSE_ASYM,
        )?;

        self.update_word(Mcu::Pla, PLA_RSTTALLY, |value| value | TALLY_RESET)?;

        // RX aggregation on, no zero-length packets.
        self.update_word(Mcu::Usb, USB_USB_CTRL, |value| {
            value & !(RX_AGG_DISABLE | RX_ZERO_EN)
        })
    }

    /// `rtl8152_up`
    fn r8152_up(&mut self) -> Result<(), Error> {
        self.aldps_off()?;
        self.r8152b_exit_oob()?;
        self.aldps_on()
    }

    /// `r8152b_exit_oob`, without the firmware patch in its `r8152b_hw_phy_cfg`. Teredo off (Linux and OpenBSD) and the
    /// TX FIFO threshold (Linux) are not U-Boot's.
    fn r8152b_exit_oob(&mut self) -> Result<(), Error> {
        self.update_dword(Mcu::Pla, PLA_RCR, |value| value & !RCR_ACPT_ALL)?;
        self.gate_rx(true)?;

        // Linux r8153_teredo_off, OpenBSD ure_disable_teredo
        self.update_word(Mcu::Pla, PLA_TEREDO_CFG, |value| {
            value & !(TEREDO_SEL | TEREDO_RS_EVENT_MASK | OOB_TEREDO_EN)
        })?;
        self.ocp
            .write_word(Mcu::Pla, PLA_WDT6_CTRL, WDT6_SET_MODE)?;
        self.ocp.write_word(Mcu::Pla, PLA_REALWOW_TIMER, 0)?;
        self.ocp.write_dword(Mcu::Pla, PLA_TEREDO_TIMER, 0)?;

        self.ocp.write_byte(Mcu::Pla, PLA_CRWECR, CRWECR_NORMAL)?;
        self.ocp.write_byte(Mcu::Pla, PLA_CR, 0)?;
        self.update_byte(Mcu::Pla, PLA_OOB_CTRL, |value| value & !NOW_IS_OOB)?;
        self.update_word(Mcu::Pla, PLA_SFF_STS_7, |value| value & !MCU_BORW_EN)?;

        // rtl8152_reinit_ll
        self.wait_for_bits(PLA_PHY_PWR, PLA_PHY_PWR_LLR, true)?;
        self.update_word(Mcu::Pla, PLA_SFF_STS_7, |value| value | RE_INIT_LL)?;
        self.wait_for_bits(PLA_PHY_PWR, PLA_PHY_PWR_LLR, true)?;

        self.nic_reset()?;

        self.ocp
            .write_dword(Mcu::Pla, PLA_RXFIFO_CTRL0, RXFIFO_THR1_NORMAL)?;
        let full_speed = self.ocp.usb.bulk_max_packet() < 512;
        let (thr2, thr3) = if full_speed {
            (RXFIFO_THR2_FULL, RXFIFO_THR3_FULL)
        } else {
            (RXFIFO_THR2_HIGH, RXFIFO_THR3_HIGH)
        };
        self.ocp.write_dword(Mcu::Pla, PLA_RXFIFO_CTRL1, thr2)?;
        self.ocp.write_dword(Mcu::Pla, PLA_RXFIFO_CTRL2, thr3)?;
        self.ocp
            .write_dword(Mcu::Pla, PLA_TXFIFO_CTRL, TXFIFO_THR_NORMAL2)?;

        self.ocp
            .write_byte(Mcu::Usb, USB_TX_AGG, TX_AGG_MAX_THRESHOLD)?;
        self.ocp.write_dword(Mcu::Usb, USB_RX_BUF_TH, RX_THR_HIGH)?;
        self.ocp
            .write_dword(Mcu::Usb, USB_TX_DMA, TEST_MODE_DISABLE | TX_SIZE_ADJUST1)?;

        self.ocp.write_word(Mcu::Pla, PLA_RMS, RMS)?;
        self.update_word(Mcu::Pla, PLA_TCR0, |value| value | TCR0_AUTO_FIFO)
    }

    /// `rtl8152_set_speed(AUTONEG_ENABLE, SPEED_100, DUPLEX_FULL)`, then Linux's wait for the PHY reset to finish.
    fn set_speed_auto_100_full(&mut self) -> Result<(), Error> {
        let all = ADVERTISE_10HALF | ADVERTISE_10FULL | ADVERTISE_100HALF | ADVERTISE_100FULL;
        let advertise = self.ocp.mdio_read(MII_ADVERTISE)?;
        self.ocp
            .mdio_write(MII_ADVERTISE, (advertise & !all) | all)?;
        self.ocp
            .mdio_write(MII_BMCR, BMCR_ANENABLE | BMCR_ANRESTART | BMCR_RESET)?;
        for _ in 0..PHY_RESET_POLLS {
            self.ocp.usb.delay_ms(PHY_RESET_POLL_MS);
            if self.ocp.mdio_read(MII_BMCR)? & BMCR_RESET == 0 {
                return Ok(());
            }
        }
        self.stats.wait_timeouts += 1;
        self.log(format_args!("timeout waiting for the PHY reset"));
        Ok(())
    }

    /// `rtl8152_enable`: the MAC address into PLA_IDR (OpenBSD `ure_ifmedia_init`, U-Boot `r8152_write_hwaddr`), then
    /// `rtl_set_eee_plus` and `rtl_enable`, whose `rtl8152_set_rx_mode` accepts unicast to this address, broadcast and
    /// every multicast (the network stack has no multicast filter to program).
    fn rtl8152_enable(&mut self) -> Result<(), Error> {
        let mut idr = [0u8; 8];
        idr[..6].copy_from_slice(&self.mac);
        self.ocp.write_byte(Mcu::Pla, PLA_CRWECR, CRWECR_CONFIG)?;
        self.ocp.write(Mcu::Pla, PLA_IDR, BYTE_EN_SIX_BYTES, &idr)?;
        self.ocp.write_byte(Mcu::Pla, PLA_CRWECR, CRWECR_NORMAL)?;

        self.update_word(Mcu::Pla, PLA_EEEP_CR, |value| value & !EEEP_CR_EEEP_TX)?;

        // r8152b_reset_packet_filter
        let fmc = self.ocp.read_word(Mcu::Pla, PLA_FMC)?;
        self.ocp
            .write_word(Mcu::Pla, PLA_FMC, fmc & !FMC_FCR_MCU_EN)?;
        self.ocp
            .write_word(Mcu::Pla, PLA_FMC, fmc | FMC_FCR_MCU_EN)?;

        self.update_byte(Mcu::Pla, PLA_CR, |value| value | PLA_CR_RE | PLA_CR_TE)?;
        self.gate_rx(false)?;

        self.ocp
            .write(Mcu::Pla, PLA_MAR, BYTE_EN_DWORD, &[0xff; 8])?;
        self.update_dword(Mcu::Pla, PLA_RCR, |value| value | RCR_APM | RCR_AM | RCR_AB)
    }
}

#[cfg(test)]
mod tests;
