//! The device: probe (Linux `ax88179_bind` + `ax88179_reset`), start (link wait + Linux `ax88179_link_reset`),
//! link polling, send and receive.

use alloc::vec;
use alloc::vec::Vec;

use crate::frame::{MAX_TX_FRAME, RxCursor, TX_HEADER_LEN, TxError, encode_tx};
use crate::regs::*;
use crate::transport::{TransportError, UsbTransport, VendorRequest};
use crate::MTU;

/// Bulk IN buffer: the largest transfer any queue setting asks for (26 KiB).
pub const RX_BUF_LEN: usize = 1024 * 0x1a;
/// The PHY is polled every 50 ms while waiting for the link.
pub const POLL_MS: u32 = 50;
/// How long `start` waits for the link before resetting the chip once.
pub const LINK_TIMEOUT_MS: u32 = 5000;
/// Linux's HZ/10 = 100 ms: the SROM-read poll in `ax88179_check_eeprom`/`ax88179_led_setting` and the control
/// TX FIFO drain wait in `ax88179_link_reset` both use it.
const HZ_DIV10_MS: u32 = 100;
/// Linux `ax88179_link_reset` waits at most HZ/10 = 100 ms for the control TX FIFO to drain.
pub const TX_FIFO_WAIT_MS: u32 = HZ_DIV10_MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Transport(TransportError),
    /// A register access or bulk OUT transfer moved fewer bytes than asked.
    ShortTransfer {
        expected: usize,
        actual: usize,
    },
    Tx(TxError),
    /// The receive buffer passed to [`Ax88179::receive`] is shorter than the frame (the frame is dropped).
    BufferTooSmall {
        needed: usize,
    },
    /// NODE_ID holds no valid Ethernet address (all zero or multicast). Linux takes a random address then; this crate
    /// has no source of randomness, so the adapter is not brought up.
    InvalidMac([u8; 6]),
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbSpeed {
    Super,
    High,
    /// Full or low speed.
    Slow,
}

/// The negotiated Ethernet link and the settings written for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub speed: Speed,
    pub full_duplex: bool,
    pub usb: UsbSpeed,
    /// Value written to [`MEDIUM_STATUS_MODE`].
    pub medium: u16,
    /// Value written to [`RX_BULKIN_QCTRL`].
    pub bulkin: [u8; 5],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx_frames: u64,
    pub rx_frames: u64,
    pub rx_transfers: u64,
    /// Packets skipped for CRC, drop, runt or truncation.
    pub rx_frame_errors: u64,
    /// Transfers rejected as a whole (bad trailer).
    pub rx_batch_errors: u64,
}

pub struct Ax88179<T: UsbTransport> {
    usb: T,
    mac: [u8; 6],
    max_packet: u16,
    link: Option<Link>,
    rx_buf: Vec<u8>,
    rx_len: usize,
    rx_cursor: Option<RxCursor>,
    /// Bytes asked for per bulk IN transfer (Linux `rx_urb_size`).
    rx_transfer_len: usize,
    tx_buf: Vec<u8>,
    stats: Stats,
    /// The RX_CTL value `ax88179_reset` started reception with (Linux `ax179_data->rxctl`), rewritten by
    /// `ax88179_link_reset` after each TX FIFO drain wait.
    rxctl: u16,
    /// Whether the WoL-support read of [`MONITOR_MOD`] succeeded (Linux `ax179_data->wol_supported`).
    wol_supported: bool,
    /// Time without link since the last chip reset, as [`Ax88179::poll_link_timed`] counts it.
    no_link_ms: u32,
    /// Whether the one retry reset of [`Ax88179::poll_link_timed`] is used up (or no longer needed).
    retried: bool,
}

/// The device is dropped when the adapter goes away or its bring-up failed: as Linux's `usbnet_stop` and
/// `ax88179_unbind`, reception stops and the PHY is powered down. On an unplugged adapter the writes fail, and that is
/// ignored.
impl<T: UsbTransport> Drop for Ax88179<T> {
    fn drop(&mut self) {
        self.stop();
        self.unbind();
    }
}

impl<T: UsbTransport> core::fmt::Debug for Ax88179<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Ax88179")
            .field("mac", &self.mac)
            .field("max_packet", &self.max_packet)
            .field("link", &self.link)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl<T: UsbTransport> Ax88179<T> {
    /// Resets the chip into its working configuration and reads its MAC address (Linux `ax88179_bind` calling
    /// `ax88179_reset`). Autonegotiation is restarted; the link is not waited for.
    pub fn probe(usb: T) -> Result<Self, Error> {
        let max_packet = usb.bulk_out_max_packet();
        let mut dev = Self {
            usb,
            mac: [0; 6],
            max_packet,
            link: None,
            rx_buf: vec![0; RX_BUF_LEN],
            rx_len: 0,
            rx_cursor: None,
            rx_transfer_len: rx_transfer_len(&BULKIN_SUPER_GIGA),
            tx_buf: vec![0; MAX_TX_FRAME + TX_HEADER_LEN],
            stats: Stats::default(),
            rxctl: RX_CTL_RUN,
            wol_supported: false,
            no_link_ms: 0,
            retried: false,
        };
        dev.reset()?;
        Ok(dev)
    }

    /// Waits up to 5 s for the link, resetting the chip once if it does not come up, then configures the MAC for
    /// the resolved link (Linux `ax88179_link_reset`). Returns the link, or `None` when no cable is up:
    /// [`Ax88179::poll_link`] picks the link up later.
    pub fn start(&mut self) -> Result<Option<Link>, Error> {
        if !self.wait_link()? {
            self.reset()?;
            if !self.wait_link()? {
                self.set_link(None);
                return Ok(None);
            }
        }
        let link = self.link_reset()?;
        self.set_link(link);
        Ok(link)
    }

    /// One BMSR read. On a change, configures the MAC for the new link (or forgets the old one). Call it every
    /// second or so from the network poll loop.
    pub fn poll_link(&mut self) -> Result<Option<Link>, Error> {
        let up = self.read_phy(MII_BMSR)? & BMSR_LSTATUS != 0;
        if up != self.link.is_some() {
            let link = if up { self.link_reset()? } else { None };
            self.set_link(link);
        }
        Ok(self.link)
    }

    /// [`Ax88179::start`] spread over calls, for a host stack that cannot wait 10 s for a cable: call it right
    /// after [`Ax88179::probe`] and then about once a second with the time since the previous call. Each call is
    /// one [`Ax88179::poll_link`]. If no link came up within [`LINK_TIMEOUT_MS`] of the probe, the chip is reset
    /// once more, as `start` does after its first wait; after that, or once a link was seen, it only polls.
    pub fn poll_link_timed(&mut self, elapsed_ms: u32) -> Result<Option<Link>, Error> {
        let link = self.poll_link()?;
        if link.is_some() {
            self.retried = true;
        } else if !self.retried {
            self.no_link_ms = self.no_link_ms.saturating_add(elapsed_ms);
            if self.no_link_ms >= LINK_TIMEOUT_MS {
                self.retried = true;
                self.reset()?;
            }
        }
        Ok(link)
    }

    /// Sends one Ethernet frame (no FCS) as one bulk transfer. The 8-byte header's padding flag follows Linux
    /// `ax88179_tx_fixup`; unlike Linux's usbnet (which appends a zero-length packet when the transfer ends on a
    /// max-packet boundary, `FLAG_SEND_ZLP` not being set) the transport sends no ZLP — the header flag is enough.
    pub fn send(&mut self, frame: &[u8]) -> Result<(), Error> {
        let len = encode_tx(frame, self.max_packet, &mut self.tx_buf)?;
        let sent = self.usb.bulk_out(&self.tx_buf[..len])?;
        if sent != len {
            return Err(Error::ShortTransfer {
                expected: len,
                actual: sent,
            });
        }
        self.stats.tx_frames += 1;
        Ok(())
    }

    /// Copies the next received frame into `out` and returns its length, or `None` when nothing is waiting. Takes
    /// at most one new bulk transfer from the transport per call.
    pub fn receive(&mut self, out: &mut [u8]) -> Result<Option<usize>, Error> {
        if let Some(n) = self.next_frame(out)? {
            return Ok(Some(n));
        }
        let Some(n) = self.usb.bulk_in(&mut self.rx_buf[..self.rx_transfer_len])? else {
            return Ok(None);
        };
        self.stats.rx_transfers += 1;
        self.rx_len = n.min(self.rx_transfer_len);
        match RxCursor::new(&self.rx_buf[..self.rx_len]) {
            Ok(c) => self.rx_cursor = Some(c),
            Err(_) => {
                self.stats.rx_batch_errors += 1;
                return Ok(None);
            }
        }
        self.next_frame(out)
    }

    /// Whether frames of an earlier transfer are still waiting (a new transfer may be ready even if not).
    pub fn has_buffered_frames(&self) -> bool {
        self.rx_cursor.is_some_and(|c| c.remaining() > 0)
    }

    /// Stops reception on the chip and drops buffered frames (Linux `ax88179_stop`). Errors from a gone device
    /// are ignored.
    pub fn stop(&mut self) {
        // Linux ax88179_stop: clear RECEIVE_EN in MEDIUM_STATUS_MODE.
        if let Ok(m) = self.read_mac::<2>(MEDIUM_STATUS_MODE) {
            let mode = u16::from_le_bytes(m) & !MEDIUM_RECEIVE_EN;
            let _ = self.write_mac16(MEDIUM_STATUS_MODE, mode);
        }
        self.rx_cursor = None;
        self.rx_len = 0;
    }

    /// Tears the chip down (Linux `ax88179_unbind`): stop reception, drop the clock selects, power the PHY down.
    /// Errors from a gone device are ignored.
    pub fn unbind(&mut self) {
        let _ = self.write_mac16(RX_CTL, RX_CTL_STOP);
        let _ = self.write_mac(CLK_SELECT, &[0]);
        let _ = self.write_mac16(PHYPWR_RSTCTL, 0);
    }

    pub fn mac_addr(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link(&self) -> Option<Link> {
        self.link
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Bytes requested per bulk IN transfer for the current link.
    pub fn rx_transfer_len(&self) -> usize {
        self.rx_transfer_len
    }

    pub fn transport(&mut self) -> &mut T {
        &mut self.usb
    }

    fn next_frame(&mut self, out: &mut [u8]) -> Result<Option<usize>, Error> {
        let Some(cursor) = self.rx_cursor.as_mut() else {
            return Ok(None);
        };
        let buf = &self.rx_buf[..self.rx_len];
        while let Some(r) = cursor.next(buf) {
            match r {
                Ok(frame) => {
                    let n = frame.data.len();
                    let Some(dst) = out.get_mut(..n) else {
                        self.stats.rx_frame_errors += 1;
                        return Err(Error::BufferTooSmall { needed: n });
                    };
                    dst.copy_from_slice(frame.data);
                    self.stats.rx_frames += 1;
                    return Ok(Some(n));
                }
                Err(_) => self.stats.rx_frame_errors += 1,
            }
        }
        self.rx_cursor = None;
        Ok(None)
    }

    fn set_link(&mut self, link: Option<Link>) {
        self.link = link;
        if let Some(l) = link {
            self.rx_transfer_len = rx_transfer_len(&l.bulkin);
        }
    }

    /// Linux `ax88179_reset`, register for register.
    fn reset(&mut self) -> Result<(), Error> {
        // Power up the Ethernet PHY. Linux ax88179_reset
        self.write_mac16(PHYPWR_RSTCTL, 0)?;
        self.write_mac16(PHYPWR_RSTCTL, PHYPWR_RSTCTL_IPRL)?;
        self.usb.delay_ms(500); // Linux ax88179_reset msleep(500)

        self.write_mac(CLK_SELECT, &[CLK_SELECT_ACS | CLK_SELECT_BCS])?;
        self.usb.delay_ms(200); // Linux ax88179_reset msleep(200)

        self.auto_detach()?; // Linux ax88179_reset -> ax88179_auto_detach
        self.get_mac_addr()?; // Linux ax88179_reset -> ax88179_get_mac_addr

        // RX bulk configuration. Linux ax88179_reset
        self.write_mac(RX_BULKIN_QCTRL, &BULKIN_SUPER_GIGA)?;
        self.rx_transfer_len = 1024 * 20;

        self.write_mac(PAUSE_WATERLVL_LOW, &[0x34])?;
        self.write_mac(PAUSE_WATERLVL_HIGH, &[0x52])?;

        // Enable checksum offload. Linux ax88179_reset
        self.write_mac(RXCOE_CTL, &[COE_ALL])?;
        self.write_mac(TXCOE_CTL, &[COE_ALL])?;

        // Configure RX control register => start operation. Linux ax88179_reset
        self.write_mac16(RX_CTL, RX_CTL_RUN)?;
        self.rxctl = RX_CTL_RUN;

        self.write_mac(
            MONITOR_MOD,
            &[MONITOR_MODE_PMETYPE | MONITOR_MODE_PMEPOL | MONITOR_MODE_RWMP],
        )?;

        // Configure default medium type => giga. Linux ax88179_reset
        self.write_mac16(MEDIUM_STATUS_MODE, MEDIUM_DEFAULT)?;

        // Check if WoL is supported: read MONITOR_MOD back. Linux ax88179_reset
        self.wol_supported = self.read_mac::<1>(MONITOR_MOD).is_ok();

        self.led_setting()?; // Linux ax88179_reset -> ax88179_led_setting

        self.disable_eee()?; // Linux ax88179_reset -> ax88179_disable_eee
        self.clear_eee_advertise()?; // Linux ax88179_reset -> ax88179_ethtool_set_eee with an empty advertised set

        // Restart autonegotiation. Linux ax88179_reset -> mii_nway_restart
        let bmcr = self.read_phy(MII_BMCR)?;
        if bmcr & BMCR_ANENABLE != 0 {
            self.write_phy(MII_BMCR, bmcr | BMCR_ANRESTART)?;
        }
        Ok(())
    }

    /// Linux `ax88179_auto_detach`: EEPROM word 0x43 decides whether the auto-detach bits are set.
    fn auto_detach(&mut self) -> Result<(), Error> {
        // Linux ax88179_auto_detach: ax88179_read_cmd(dev, AX_ACCESS_EEPROM, 0x43, 1, 2, &tmp16)
        let Ok(word) = self.read_eeprom_word(0x43) else {
            return Ok(());
        };
        if word == 0xFFFF || word & 0x0100 == 0 {
            return Ok(());
        }
        // Enable Auto Detach bit. Linux ax88179_auto_detach
        let mut clk = self.read_mac::<1>(CLK_SELECT)?[0];
        clk |= CLK_SELECT_ULR;
        self.write_mac(CLK_SELECT, &[clk])?;
        let rst = self.read_mac::<2>(PHYPWR_RSTCTL)?;
        self.write_mac16(PHYPWR_RSTCTL, u16::from_le_bytes(rst) | PHYPWR_RSTCTL_AT)?;
        Ok(())
    }

    /// Linux `ax88179_get_mac_addr`: read NODE_ID, and write it back when it is a valid Ethernet address.
    fn get_mac_addr(&mut self) -> Result<(), Error> {
        let mac = self.read_mac::<6>(NODE_ID)?;
        // Linux is_valid_ether_addr: not all zero, not multicast.
        if mac == [0; 6] || mac[0] & 1 != 0 {
            return Err(Error::InvalidMac(mac));
        }
        self.mac = mac;
        self.write_mac(NODE_ID, &mac)?;
        Ok(())
    }

    /// Linux `ax88179_led_setting` whole: GPIOs on UA1, LED mode from EEPROM, else eFuse, else the old-EEPROM
    /// conversion, then the PHY LED registers and the MAC LED full-duplex register.
    fn led_setting(&mut self) -> Result<(), Error> {
        // Check AX88179 version. UA1 or UA2. Linux ax88179_led_setting
        let mut value = self.read_mac::<1>(GENERAL_STATUS)?[0];
        if value & SECLD == 0 {
            // UA1
            value = GPIO_CTRL_GPIO3EN | GPIO_CTRL_GPIO2EN | GPIO_CTRL_GPIO1EN;
            self.write_mac(GPIO_CTRL, &[value])?;
        }

        let mut ledvalue: u16 = 0;
        if self.check_eeprom()? {
            // Linux ax88179_led_setting, EEPROM branch
            self.write_mac(SROM_ADDR, &[0x42])?;
            self.write_mac(SROM_CMD, &[EEP_RD])?;
            if !self.wait_srom()? {
                return Ok(()); // Linux returns -EINVAL here, skipping the LED writes
            }
            ledvalue = (self.read_mac::<1>(SROM_DATA_HIGH)?[0] as u16) << 8;
            ledvalue |= self.read_mac::<1>(SROM_DATA_LOW)?[0] as u16;
            // Load internal ROM for default setting. Linux ax88179_led_setting
            if ledvalue == 0xFFFF || ledvalue & LED_VALID == 0 {
                ledvalue = self.convert_old_led()?;
            }
        } else if self.check_efuse(&mut ledvalue)? {
            // Linux ax88179_led_setting, eFuse branch
            if ledvalue == 0xFFFF || ledvalue & LED_VALID == 0 {
                ledvalue = self.convert_old_led()?;
            }
        } else {
            ledvalue = self.convert_old_led()?;
        }

        // Linux ax88179_led_setting: the PHY LED registers on the extended page
        self.write_phy(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_EXT)?;
        self.write_phy(GMII_PHYPAGE, 0x2c)?;
        let mut ledact = self.read_phy(GMII_LED_ACT)? & GMII_LED_ACTIVE_MASK;
        let mut ledlink = self.read_phy(GMII_LED_LINK)? & GMII_LED_LINK_MASK;
        if ledvalue & LED0_ACTIVE != 0 {
            ledact |= GMII_LED0_ACTIVE;
        }
        if ledvalue & LED1_ACTIVE != 0 {
            ledact |= GMII_LED1_ACTIVE;
        }
        if ledvalue & LED2_ACTIVE != 0 {
            ledact |= GMII_LED2_ACTIVE;
        }
        if ledvalue & LED0_LINK_10 != 0 {
            ledlink |= GMII_LED0_LINK_10;
        }
        if ledvalue & LED1_LINK_10 != 0 {
            ledlink |= GMII_LED1_LINK_10;
        }
        if ledvalue & LED2_LINK_10 != 0 {
            ledlink |= GMII_LED2_LINK_10;
        }
        if ledvalue & LED0_LINK_100 != 0 {
            ledlink |= GMII_LED0_LINK_100;
        }
        if ledvalue & LED1_LINK_100 != 0 {
            ledlink |= GMII_LED1_LINK_100;
        }
        if ledvalue & LED2_LINK_100 != 0 {
            ledlink |= GMII_LED2_LINK_100;
        }
        if ledvalue & LED0_LINK_1000 != 0 {
            ledlink |= GMII_LED0_LINK_1000;
        }
        if ledvalue & LED1_LINK_1000 != 0 {
            ledlink |= GMII_LED1_LINK_1000;
        }
        if ledvalue & LED2_LINK_1000 != 0 {
            ledlink |= GMII_LED2_LINK_1000;
        }
        self.write_phy(GMII_LED_ACT, ledact)?;
        self.write_phy(GMII_LED_LINK, ledlink)?;
        self.write_phy(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE0)?;

        // LED full duplex setting. Linux ax88179_led_setting
        let mut ledfd = 0u8;
        if ledvalue & LED0_FD != 0 {
            ledfd |= 0x01;
        } else if ledvalue & LED0_USB3_MASK == 0 {
            ledfd |= 0x02;
        }
        if ledvalue & LED1_FD != 0 {
            ledfd |= 0x04;
        } else if ledvalue & LED1_USB3_MASK == 0 {
            ledfd |= 0x08;
        }
        if ledvalue & LED2_FD != 0 {
            ledfd |= 0x10;
        } else if ledvalue & LED2_USB3_MASK == 0 {
            ledfd |= 0x20;
        }
        self.write_mac(LEDCTRL, &[ledfd])?;
        Ok(())
    }

    /// Linux `ax88179_check_eeprom`: read the first six EEPROM words through the SROM registers and check the
    /// checksum. `Ok(true)` when the EEPROM is valid.
    fn check_eeprom(&mut self) -> Result<bool, Error> {
        let mut eeprom = [0u8; 20];
        for i in 0..6u8 {
            self.write_mac(SROM_ADDR, &[i])?;
            self.write_mac(SROM_CMD, &[EEP_RD])?;
            if !self.wait_srom()? {
                return Ok(false); // Linux returns -EINVAL
            }
            let word = self.read_mac::<2>(SROM_DATA_LOW)?;
            eeprom[i as usize * 2..i as usize * 2 + 2].copy_from_slice(&word);
            if i == 0 && eeprom[0] == 0xFF {
                return Ok(false); // Linux returns -EINVAL
            }
        }
        let mut csum = eeprom[6] as u16 + eeprom[7] as u16 + eeprom[8] as u16 + eeprom[9] as u16;
        csum = (csum >> 8) + (csum & 0xff);
        Ok(csum + eeprom[10] as u16 == 0xff)
    }

    /// Linux `ax88179_check_efuse`: read the 64 eFuse bytes and check the checksum; on success `ledmode` is the
    /// LED mode from bytes 51/52.
    fn check_efuse(&mut self, ledmode: &mut u16) -> Result<bool, Error> {
        let efuse = self.read_efuse()?;
        if efuse[0] == 0xFF {
            return Ok(false); // Linux returns -EINVAL
        }
        let mut csum = efuse.iter().fold(0u16, |a, &b| a + b as u16);
        while csum > 255 {
            csum = (csum & 0x00ff) + (csum >> 8);
        }
        if csum != 0xFF {
            return Ok(false); // Linux returns -EINVAL
        }
        *ledmode = (efuse[51] as u16) << 8 | efuse[52] as u16;
        Ok(true)
    }

    /// Linux `ax88179_convert_old_led`: the LED mode from old EEPROM word 0x3C, mapped to the new bit layout.
    fn convert_old_led(&mut self) -> Result<u16, Error> {
        // Linux ax88179_convert_old_led: ax88179_read_cmd(dev, AX_ACCESS_EEPROM, 0x3C, 1, 2, &led)
        let led = self.read_eeprom_word(0x3C)? >> 8;
        let ledvalue = match led {
            0xFE => LED0_ACTIVE | LED1_LINK_1000 | LED2_LINK_100 | LED_VALID,
            0xFD => LED0_ACTIVE | LED1_LINK_1000 | LED2_LINK_100 | LED2_LINK_10 | LED_VALID,
            0xFC => {
                LED0_ACTIVE | LED1_ACTIVE | LED1_LINK_1000 | LED2_ACTIVE | LED2_LINK_100
                    | LED2_LINK_10
                    | LED_VALID
            }
            // 0xFF and anything else: the default setting. Linux ax88179_convert_old_led
            _ => {
                LED0_ACTIVE | LED1_LINK_10 | LED1_LINK_100 | LED1_LINK_1000 | LED2_ACTIVE
                    | LED2_LINK_10
                    | LED2_LINK_100
                    | LED2_LINK_1000
                    | LED_VALID
            }
        };
        Ok(ledvalue)
    }

    /// Waits for the SROM read to finish, at most HZ/10 = 100 ms (Linux `ax88179_check_eeprom` /
    /// `ax88179_led_setting` poll `AX_SROM_CMD` until `EEP_BUSY` clears).
    fn wait_srom(&mut self) -> Result<bool, Error> {
        let mut waited = 0;
        loop {
            let cmd = self.read_mac::<1>(SROM_CMD)?[0];
            if cmd & EEP_BUSY == 0 {
                return Ok(true);
            }
            if waited >= HZ_DIV10_MS {
                return Ok(false);
            }
            self.usb.delay_ms(1);
            waited += 1;
        }
    }

    /// Linux `ax88179_disable_eee`: the PHY page-3 register that turns EEE off.
    fn disable_eee(&mut self) -> Result<(), Error> {
        self.write_phy(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE3)?;
        self.write_phy(MII_PHYADDR, 0x3246)?;
        self.write_phy(GMII_PHY_PAGE_SELECT, GMII_PHY_PGSEL_PAGE0)?;
        Ok(())
    }

    /// Linux `ax88179_reset` -> `ax88179_ethtool_set_eee` with an empty `advertised` set: clear the EEE
    /// advertisement through an MMD indirect write of 0 to `MDIO_AN_EEE_ADV` in `MDIO_MMD_AN`
    /// (`ax88179_phy_write_mmd_indirect`).
    fn clear_eee_advertise(&mut self) -> Result<(), Error> {
        self.write_phy(MII_MMD_CTRL, MDIO_MMD_AN)?;
        self.write_phy(MII_MMD_DATA, MDIO_AN_EEE_ADV)?;
        self.write_phy(MII_MMD_CTRL, MDIO_MMD_AN | MII_MMD_CTRL_NOINCR)?;
        self.write_phy(MII_MMD_DATA, 0)?;
        Ok(())
    }

    /// Link wait: BMSR every 50 ms for up to 5 s.
    fn wait_link(&mut self) -> Result<bool, Error> {
        let mut t = 0;
        loop {
            if self.read_phy(MII_BMSR)? & BMSR_LSTATUS != 0 {
                return Ok(true);
            }
            self.usb.delay_ms(POLL_MS);
            t += POLL_MS;
            if t >= LINK_TIMEOUT_MS {
                return Ok(false);
            }
        }
    }

    /// Linux `ax88179_link_reset`: pick the bulk-in queue and the medium mode for the resolved link. Writes
    /// nothing when the PHY reports no link or the control TX FIFO stays busy for 100 ms.
    fn link_reset(&mut self) -> Result<Option<Link>, Error> {
        // Linux ax88179_link_reset: while bit 30 (0x4000_0000) of the 0x81/0x8c read is set (the USB device control TX FIFO),
        // stop and restart reception, for at most HZ/10 = 100 ms.
        let mut waited = 0;
        loop {
            self.write_mac16(RX_CTL, 0)?;
            self.write_mac16(RX_CTL, self.rxctl)?;
            let fifo = self.read_tx_fifo_status()?;
            if fifo & 0x4000_0000 == 0 {
                break;
            }
            if waited >= TX_FIFO_WAIT_MS {
                return Ok(None); // Linux returns without configuring
            }
            self.usb.delay_ms(1);
            waited += 1;
        }

        let mut mode = MEDIUM_RECEIVE_EN | MEDIUM_TXFLOW_CTRLEN | MEDIUM_RXFLOW_CTRLEN;
        let link_sts = self.read_mac::<1>(PHYSICAL_LINK_STATUS)?[0];
        let physr = self.read_phy(GMII_PHY_PHYSR)?;
        if physr & PHYSR_LINK == 0 {
            return Ok(None);
        }
        let usb = if link_sts & USB_SS != 0 {
            UsbSpeed::Super
        } else if link_sts & USB_HS != 0 {
            UsbSpeed::High
        } else {
            UsbSpeed::Slow
        };
        let (speed, bulkin) = match physr & PHYSR_SMASK {
            PHYSR_GIGA => {
                mode |= MEDIUM_GIGAMODE | MEDIUM_EN_125MHZ;
                // Linux ax88179_link_reset: JUMBO_EN only above the standard MTU
                if MTU > 1500 {
                    mode |= MEDIUM_JUMBO_EN;
                }
                let q = match usb {
                    UsbSpeed::Super => BULKIN_SUPER_GIGA,
                    UsbSpeed::High => BULKIN_HIGH_GIGA,
                    UsbSpeed::Slow => BULKIN_SLOW,
                };
                (Speed::Mbps1000, q)
            }
            PHYSR_100 => {
                mode |= MEDIUM_PS;
                let q = if usb == UsbSpeed::Slow {
                    BULKIN_SLOW
                } else {
                    BULKIN_FAST_100
                };
                (Speed::Mbps100, q)
            }
            _ => (Speed::Mbps10, BULKIN_SLOW),
        };
        self.write_mac(RX_BULKIN_QCTRL, &bulkin)?;
        let full_duplex = physr & PHYSR_FULL != 0;
        if full_duplex {
            mode |= MEDIUM_FULL_DUPLEX;
        }
        self.write_mac16(MEDIUM_STATUS_MODE, mode)?;
        Ok(Some(Link {
            speed,
            full_duplex,
            usb,
            medium: mode,
            bulkin,
        }))
    }

    // ---- register access ----

    fn write_mac(&mut self, reg: u8, data: &[u8]) -> Result<(), Error> {
        let req = VendorRequest {
            request: ACCESS_MAC,
            value: reg as u16,
            index: data.len() as u16,
        };
        let n = self.usb.control_out(req, data)?;
        check_len(data.len(), n)
    }

    fn write_mac16(&mut self, reg: u8, v: u16) -> Result<(), Error> {
        self.write_mac(reg, &v.to_le_bytes())
    }

    fn read_mac<const N: usize>(&mut self, reg: u8) -> Result<[u8; N], Error> {
        let req = VendorRequest {
            request: ACCESS_MAC,
            value: reg as u16,
            index: N as u16,
        };
        let mut buf = [0u8; N];
        let n = self.usb.control_in(req, &mut buf)?;
        check_len(N, n)?;
        Ok(buf)
    }

    fn write_phy(&mut self, reg: u8, v: u16) -> Result<(), Error> {
        let req = VendorRequest {
            request: ACCESS_PHY,
            value: PHY_ID,
            index: reg as u16,
        };
        let n = self.usb.control_out(req, &v.to_le_bytes())?;
        check_len(2, n)
    }

    fn read_phy(&mut self, reg: u8) -> Result<u16, Error> {
        let req = VendorRequest {
            request: ACCESS_PHY,
            value: PHY_ID,
            index: reg as u16,
        };
        let mut buf = [0u8; 2];
        let n = self.usb.control_in(req, &mut buf)?;
        check_len(2, n)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// One EEPROM word: Linux `ax88179_read_cmd(dev, AX_ACCESS_EEPROM, word, 1, 2, ...)`.
    fn read_eeprom_word(&mut self, word: u16) -> Result<u16, Error> {
        let req = VendorRequest {
            request: ACCESS_EEPROM,
            value: word,
            index: 1,
        };
        let mut buf = [0u8; 2];
        let n = self.usb.control_in(req, &mut buf)?;
        check_len(2, n)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// The 64 eFuse bytes: Linux `ax88179_read_cmd(dev, AX_ACCESS_EFUS, 0, 64, 64, ...)`.
    fn read_efuse(&mut self) -> Result<[u8; 64], Error> {
        let req = VendorRequest {
            request: ACCESS_EFUS,
            value: 0,
            index: 64,
        };
        let mut buf = [0u8; 64];
        let n = self.usb.control_in(req, &mut buf)?;
        check_len(64, n)?;
        Ok(buf)
    }

    /// The USB device control TX FIFO status: Linux `ax88179_link_reset` reads request 0x81, value 0x8c, index 0,
    /// 4 bytes; Linux waits while bit 30 (0x4000_0000) is set.
    fn read_tx_fifo_status(&mut self) -> Result<u32, Error> {
        let req = VendorRequest {
            request: 0x81,
            value: 0x8c,
            index: 0,
        };
        let mut buf = [0u8; 4];
        let n = self.usb.control_in(req, &mut buf)?;
        check_len(4, n)?;
        Ok(u32::from_le_bytes(buf))
    }
}

fn check_len(expected: usize, actual: usize) -> Result<(), Error> {
    if expected == actual {
        Ok(())
    } else {
        Err(Error::ShortTransfer { expected, actual })
    }
}

#[cfg(test)]
mod tests;
