//! The host controller: probe, bring-up, commands and block transfers.
//!
//! Bring-up follows Linux `sdhci_init` / `sdhci_set_power` / `sdhci_set_clock` and `mmc_sd_init_card`. The Qualcomm
//! parts follow `sdhci_msm_probe`, with one change: Linux services the block's power requests in an interrupt thread,
//! this driver polls them, including while a reset is in progress, where a request left unanswered would otherwise keep
//! the reset from finishing. That is the behaviour Scarlet saw as "the SDCC5 reset-all path does not clear" with its
//! interrupt masked; if a reset still does not finish, the driver resets the command and data lines only, as Scarlet does.

use crate::{
    Error,
    card::{self, Card, Cid, Csd, Long, Scr},
    msm,
    regs::{self, *},
};

/// Register access. Offsets are the standard ones; 8- and 16-bit accesses are real ones of that width.
pub trait Regs {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
    fn read16(&self, offset: usize) -> u16 {
        (self.read32(offset & !3) >> ((offset & 3) * 8)) as u16
    }
    fn read8(&self, offset: usize) -> u8 {
        (self.read32(offset & !3) >> ((offset & 3) * 8)) as u8
    }
    fn write16(&self, offset: usize, value: u16);
    fn write8(&self, offset: usize, value: u8);
}

/// The driver's own DMA memory: [`AREA_SIZE`] bytes the controller sees at [`DmaArea::daddr`], page aligned.
pub trait DmaArea {
    fn daddr(&self) -> u64;
    fn read(&self, offset: usize, buf: &mut [u8]);
    fn write(&self, offset: usize, buf: &[u8]);
    /// Called after the CPU wrote `len` bytes at `offset` and before the controller may read them.
    fn sync_to_device(&self, _offset: usize, _len: usize) {}
    /// Called after the controller wrote `len` bytes at `offset` and before the CPU reads them.
    fn sync_from_device(&self, _offset: usize, _len: usize) {}
}

/// Time for timeouts and polling delays.
pub trait Clock {
    fn now_us(&self) -> u64;
    /// Waits about `us` microseconds (may yield to other threads).
    fn delay_us(&self, us: u64);
    /// Sleeps until the controller interrupts (the line is `INT_STATUS & SIGNAL_ENABLE`; Linux `sdhci_irq`), for at most
    /// `timeout_us`. `false`: there is no interrupt to sleep on here (no line, or a caller that may not sleep), and the
    /// caller polls.
    fn wait_interrupt(&self, _timeout_us: u64) -> bool {
        false
    }
}

/// Layout of the DMA area: the ADMA2 table in the first page, small data transfers (SCR, CMD6) in the second.
pub const AREA_SIZE: usize = 0x2000;
const TABLE_OFFSET: usize = 0;
const TABLE_SIZE: usize = 0x1000;
const SMALL_OFFSET: usize = 0x1000;

/// What one ADMA2 line moves at most. Scarlet's value; the descriptor could take 64 KiB.
const SEGMENT_MAX: usize = 32 * 1024;
pub const SECTOR: usize = 512;
/// Most segments a command can carry when each is split at [`SEGMENT_MAX`]: the table holds at least 256 lines
/// (64-bit lines are 12 bytes), and this leaves room for the splits of a request of [`MAX_TRANSFER`] bytes.
pub const MAX_SEGMENTS: usize = 128;
/// Bytes per command, which keeps the table within its page whatever the segments look like.
pub const MAX_TRANSFER: usize = 512 * 1024;

const CMD_TIMEOUT_US: u64 = 1_000_000;
const DATA_TIMEOUT_US: u64 = 5_000_000;
const REG_TIMEOUT_US: u64 = 100_000;
const CARD_BUSY_TIMEOUT_US: u64 = 5_000_000;
const INIT_CLOCK_HZ: u32 = 400_000;
const DEFAULT_CLOCK_HZ: u32 = 25_000_000;
const HS_CLOCK_HZ: u32 = 50_000_000;

/// One piece of memory for a transfer, at a device address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSegment {
    pub daddr: u64,
    pub len: usize,
}

/// Which controller this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Plain SDHCI.
    Generic,
    /// Qualcomm SDCC v5 with its core clock at this rate (its capabilities do not tell).
    QcomV5 { core_hz: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub variant: Variant,
    /// Whether writes are allowed at all.
    pub writable: bool,
}

/// What the registers say before the driver does anything. Reading this writes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub version: u16,
    pub caps: u32,
    pub present: u32,
    pub host_control: u8,
    pub power: u8,
    pub clock: u16,
    pub host_control2: u16,
    pub int_status: u32,
    /// Qualcomm only.
    pub mci_version: u32,
    pub vendor_spec: u32,
    pub pwrctl_status: u32,
    pub pwrctl_mask: u32,
    pub pwrctl_ctl: u32,
}

impl Probe {
    pub fn read(regs: &impl Regs, variant: Variant) -> Self {
        let mut p = Self {
            version: regs.read16(HOST_VERSION),
            caps: regs.read32(CAPABILITIES),
            present: regs.read32(PRESENT_STATE),
            host_control: regs.read8(HOST_CONTROL),
            power: regs.read8(POWER_CONTROL),
            clock: regs.read16(CLOCK_CONTROL),
            host_control2: regs.read16(HOST_CONTROL2),
            int_status: regs.read32(INT_STATUS),
            ..Self::default()
        };
        if let Variant::QcomV5 { .. } = variant {
            p.mci_version = regs.read32(msm::CORE_MCI_VERSION);
            p.vendor_spec = regs.read32(msm::CORE_VENDOR_SPEC);
            p.pwrctl_status = regs.read32(msm::CORE_PWRCTL_STATUS);
            p.pwrctl_mask = regs.read32(msm::CORE_PWRCTL_MASK);
            p.pwrctl_ctl = regs.read32(msm::CORE_PWRCTL_CTL);
        }
        p
    }

    /// The specification version field: 0 = 1.00, 1 = 2.00, 2 = 3.00, 3 and up = 4.x.
    pub fn spec(&self) -> u8 {
        self.version as u8
    }
}

/// A command's response format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rsp {
    None,
    /// R1, R6, R7.
    Short,
    /// R1b.
    ShortBusy,
    /// R2.
    Long,
    /// R3: no CRC, no index.
    Ocr,
}

impl Rsp {
    fn flags(self) -> u16 {
        match self {
            Rsp::None => CMD_RESP_NONE,
            Rsp::Short => CMD_RESP_SHORT | CMD_CRC | CMD_INDEX,
            Rsp::ShortBusy => CMD_RESP_SHORT_BUSY | CMD_CRC | CMD_INDEX,
            Rsp::Long => CMD_RESP_LONG | CMD_CRC,
            Rsp::Ocr => CMD_RESP_SHORT,
        }
    }
}

/// A data phase: `blocks` of `block_size`, the ADMA2 table already written.
#[derive(Clone, Copy)]
struct Xfer {
    block_size: u16,
    blocks: u16,
    read: bool,
}

/// How the bring-up went at the controller level, for the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StartNotes {
    /// RESET_ALL did not finish, so only the command and data lines were reset.
    pub reset_all_stuck: bool,
    /// Power requests the block raised and the driver answered.
    pub pwr_requests: u32,
    pub adma64: bool,
    pub write_switch_on: bool,
}

pub struct Host<R: Regs, D: DmaArea, C: Clock> {
    regs: R,
    dma: D,
    clock: C,
    config: Config,
    spec: u8,
    base_hz: u32,
    adma64: bool,
    notes: StartNotes,
    card: Option<Card>,
    /// A data-line reset did not finish: the ADMA engine may still use the last table and the buffers it names. Nothing
    /// more is sent, and the caller keeps every buffer it gave the last transfer.
    wedged: bool,
}

impl<R: Regs, D: DmaArea, C: Clock> Host<R, D, C> {
    /// Resets the controller, powers the bus and initialises the card. On a Qualcomm controller the clocks, the pins and
    /// the rails have to be up already ([`crate::soc`]; the kernel side sets the rails).
    pub fn start(regs: R, dma: D, clock: C, config: Config) -> Result<Self, Error> {
        let mut host = Self {
            regs,
            dma,
            clock,
            config,
            spec: 0,
            base_hz: 0,
            adma64: false,
            wedged: false,
            notes: StartNotes::default(),
            card: None,
        };
        host.start_controller()?;
        host.init_card()?;
        Ok(host)
    }

    pub fn card(&self) -> &Card {
        self.card.as_ref().expect("a host exists only with a card")
    }

    pub fn notes(&self) -> &StartNotes {
        &self.notes
    }

    /// Whether a data-line reset failed: the controller may still be doing DMA with the last transfer's buffers, which the
    /// caller must then keep for good. A wedged host sends nothing more.
    pub fn wedged(&self) -> bool {
        self.wedged
    }

    fn qcom(&self) -> bool {
        matches!(self.config.variant, Variant::QcomV5 { .. })
    }

    fn start_controller(&mut self) -> Result<(), Error> {
        let version = self.regs.read16(HOST_VERSION);
        let caps = self.regs.read32(CAPABILITIES);
        if version == u16::MAX && caps == u32::MAX {
            return Err(Error::NoController);
        }
        self.spec = version as u8;

        let mut caps_for_power = caps;
        if self.qcom() {
            // Interrupt masked: this driver polls. Answer whatever is pending from the firmware's time, as
            // `sdhci_msm_probe` does before it enables the interrupt.
            self.regs.write32(msm::CORE_PWRCTL_MASK, 0);
            self.service_pwr();
            self.regs
                .write32(msm::CORE_VENDOR_SPEC, msm::CORE_VENDOR_SPEC_POR_VAL);
            let core = self.regs.read32(msm::CORE_MCI_VERSION);
            if msm::core_wants_capabilities(core) {
                caps_for_power |= CAN_VDD_300 | CAN_8BIT;
                self.regs
                    .write32(msm::CORE_VENDOR_SPEC_CAPABILITIES0, caps_for_power);
            }
        }

        self.reset_all()?;

        if self.qcom() {
            // Not used at 100 MHz or less (`sdhci_msm_set_uhs_signaling`, `msm_hc_select_default`).
            let spec3 = self.regs.read32(msm::CORE_VENDOR_SPEC3);
            self.regs
                .write32(msm::CORE_VENDOR_SPEC3, spec3 & !msm::CORE_PWRSAVE_DLL);
            let dll = self.regs.read32(msm::CORE_DLL_CONFIG);
            self.regs
                .write32(msm::CORE_DLL_CONFIG, dll | msm::CORE_DLL_RST);
            let dll = self.regs.read32(msm::CORE_DLL_CONFIG);
            self.regs
                .write32(msm::CORE_DLL_CONFIG, dll | msm::CORE_DLL_PDN);
        }

        self.base_hz = match self.config.variant {
            Variant::QcomV5 { core_hz } => core_hz,
            Variant::Generic => {
                let mask = if self.spec >= 2 { 0xff } else { 0x3f };
                ((caps >> 8) & mask) * 1_000_000
            }
        };
        if self.base_hz == 0 {
            return Err(Error::Unusable(
                "the controller does not state its base clock",
            ));
        }
        if caps & CAN_ADMA2 == 0 {
            return Err(Error::Unusable("the controller has no ADMA2"));
        }

        if !self.qcom() && self.regs.read32(PRESENT_STATE) & PS_CARD_INSERTED == 0 {
            return Err(Error::NoCard);
        }

        self.regs.write32(INT_ENABLE, INT_ENABLE_ALL);
        self.regs.write32(SIGNAL_ENABLE, 0);
        self.regs.write8(TIMEOUT_CONTROL, 0xe);

        self.adma64 = caps & CAN_64BIT != 0;
        self.notes.adma64 = self.adma64;
        let ctrl = self.regs.read8(HOST_CONTROL) & !(CTRL_DMA_MASK | CTRL_4BIT | CTRL_HISPD);
        self.regs.write8(
            HOST_CONTROL,
            ctrl | if self.adma64 {
                CTRL_ADMA64
            } else {
                CTRL_ADMA32
            },
        );
        // No preset values (`SDHCI_QUIRK2_PRESET_VALUE_BROKEN`) and no UHS mode for this driver's speeds.
        self.regs
            .write16(HOST_CONTROL2, self.regs.read16(HOST_CONTROL2) & !0x800f);

        self.power_on(caps_for_power)?;
        self.set_clock(INIT_CLOCK_HZ)?;
        Ok(())
    }

    fn power_on(&mut self, caps: u32) -> Result<(), Error> {
        let voltage = if self.qcom() {
            // The slot's rail is 2.95 V; `sdhci_msm_set_regulator_caps` reports 3 V.
            POWER_300
        } else if caps & CAN_VDD_330 != 0 {
            POWER_330
        } else if caps & CAN_VDD_300 != 0 {
            POWER_300
        } else {
            return Err(Error::NoVoltage);
        };
        // One write (`SDHCI_QUIRK_SINGLE_POWER_WRITE`): on Qualcomm each write can raise a power request.
        self.regs.write8(POWER_CONTROL, voltage | POWER_ON);
        if self.qcom() {
            // The block raises BUS_ON a moment after the write; none comes if the bus was on already.
            for _ in 0..100 {
                if self.regs.read32(msm::CORE_PWRCTL_STATUS) & msm::PWRCTL_REQUESTS != 0 {
                    break;
                }
                self.clock.delay_us(100);
            }
            self.service_pwr();
        }
        // Supply ramp-up, before the card sees a clock.
        self.clock.delay_us(10_000);
        Ok(())
    }

    /// Answers the block's pending power requests (`sdhci_msm_handle_pwr_irq`), with the retries Linux has for a
    /// clear pulse lost to a reset in progress.
    fn service_pwr(&mut self) {
        if !self.qcom() {
            return;
        }
        let requests = self.regs.read32(msm::CORE_PWRCTL_STATUS) & msm::PWRCTL_REQUESTS;
        if requests == 0 {
            return;
        }
        self.regs.write32(msm::CORE_PWRCTL_CLEAR, requests);
        for _ in 0..10 {
            if self.regs.read32(msm::CORE_PWRCTL_STATUS) & requests == 0 {
                break;
            }
            self.regs.write32(msm::CORE_PWRCTL_CLEAR, requests);
            self.clock.delay_us(10);
        }
        self.regs
            .write32(msm::CORE_PWRCTL_CTL, msm::pwrctl_ack(requests));
        self.notes.pwr_requests |= requests;
    }

    fn reset_all(&mut self) -> Result<(), Error> {
        self.regs.write8(SOFTWARE_RESET, RESET_ALL);
        let deadline = self.clock.now_us() + REG_TIMEOUT_US;
        loop {
            self.service_pwr();
            if self.regs.read8(SOFTWARE_RESET) & RESET_ALL == 0 {
                self.service_pwr();
                return Ok(());
            }
            if self.clock.now_us() >= deadline {
                break;
            }
            self.clock.delay_us(10);
        }
        if !self.qcom() {
            return Err(Error::Timeout("the controller's reset"));
        }
        self.notes.reset_all_stuck = true;
        self.reset_lines(RESET_CMD | RESET_DATA)
    }

    fn reset_lines(&mut self, mask: u8) -> Result<(), Error> {
        self.regs.write8(SOFTWARE_RESET, mask);
        let deadline = self.clock.now_us() + REG_TIMEOUT_US;
        while self.regs.read8(SOFTWARE_RESET) & mask != 0 {
            if self.clock.now_us() >= deadline {
                if mask & RESET_DATA != 0 {
                    self.wedged = true;
                }
                return Err(Error::Timeout("a line reset"));
            }
            self.clock.delay_us(10);
        }
        Ok(())
    }

    fn set_clock(&mut self, target_hz: u32) -> Result<u32, Error> {
        self.regs.write16(CLOCK_CONTROL, 0);
        let (field, actual) = regs::clock_divider(self.base_hz, target_hz, self.spec);
        self.regs.write16(CLOCK_CONTROL, field | CLOCK_INT_EN);
        let deadline = self.clock.now_us() + REG_TIMEOUT_US;
        while self.regs.read16(CLOCK_CONTROL) & CLOCK_INT_STABLE == 0 {
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout("the internal clock"));
            }
            self.clock.delay_us(10);
        }
        self.regs
            .write16(CLOCK_CONTROL, field | CLOCK_INT_EN | CLOCK_CARD_EN);
        // The card needs 74 clocks before its first command; at 400 kHz that is under 200 us.
        self.clock.delay_us(1000);
        Ok(actual)
    }

    fn write_table(&mut self, segments: &[DataSegment]) -> Result<(), Error> {
        let line = if self.adma64 { 12 } else { 8 };
        let mut lines = 0;
        let last_seg = segments.len().checked_sub(1).ok_or(Error::BadBuffer)?;
        for (i, seg) in segments.iter().enumerate() {
            if seg.len == 0 || !seg.daddr.is_multiple_of(4) {
                return Err(Error::BadBuffer);
            }
            let end = seg
                .daddr
                .checked_add(seg.len as u64)
                .ok_or(Error::BadBuffer)?;
            if !self.adma64 && end > 1 << 32 {
                return Err(Error::BadBuffer);
            }
            let mut at = 0;
            while at < seg.len {
                let n = (seg.len - at).min(SEGMENT_MAX);
                let last = i == last_seg && at + n == seg.len;
                let mut bytes = [0u8; 12];
                let attr = ADMA_ATTR_VALID | ADMA_ATTR_TRAN | if last { ADMA_ATTR_END } else { 0 };
                let addr = seg.daddr + at as u64;
                bytes[0..2].copy_from_slice(&attr.to_le_bytes());
                bytes[2..4].copy_from_slice(&(n as u16).to_le_bytes());
                bytes[4..8].copy_from_slice(&(addr as u32).to_le_bytes());
                bytes[8..12].copy_from_slice(&((addr >> 32) as u32).to_le_bytes());
                let off = lines * line;
                if off + line > TABLE_SIZE {
                    return Err(Error::BadBuffer);
                }
                self.dma.write(TABLE_OFFSET + off, &bytes[..line]);
                lines += 1;
                at += n;
            }
        }
        self.dma.sync_to_device(TABLE_OFFSET, lines * line);
        let table = self.dma.daddr() + TABLE_OFFSET as u64;
        self.regs.write32(ADMA_ADDRESS, table as u32);
        if self.adma64 {
            self.regs.write32(ADMA_ADDRESS_HI, (table >> 32) as u32);
        }
        Ok(())
    }

    /// Sends a command and, with `xfer`, runs its data phase. Returns the raw response registers.
    fn command(
        &mut self,
        cmd: u8,
        arg: u32,
        rsp: Rsp,
        xfer: Option<Xfer>,
    ) -> Result<[u32; 4], Error> {
        if self.wedged {
            return Err(Error::Wedged);
        }
        let inhibit = if xfer.is_some() || rsp == Rsp::ShortBusy {
            PS_CMD_INHIBIT | PS_DATA_INHIBIT
        } else {
            PS_CMD_INHIBIT
        };
        let deadline = self.clock.now_us() + CMD_TIMEOUT_US;
        while self.regs.read32(PRESENT_STATE) & inhibit != 0 {
            if self.clock.now_us() >= deadline {
                let _ = self.reset_lines(RESET_CMD | RESET_DATA);
                return Err(Error::Timeout("the command and data lines"));
            }
            self.clock.delay_us(10);
        }
        self.regs.write32(INT_STATUS, u32::MAX);

        let mut flags = rsp.flags() | ((cmd as u16) << 8);
        if let Some(x) = xfer {
            flags |= CMD_DATA;
            let mut mode = TRNS_DMA;
            if x.read {
                mode |= TRNS_READ;
            }
            if x.blocks > 1 {
                // Auto CMD12 for reads and writes (`SDHCI_QUIRK_MULTIBLOCK_READ_ACMD12`, `SDHCI_AUTO_CMD12`).
                mode |= TRNS_MULTI | TRNS_BLK_CNT_EN | TRNS_AUTO_CMD12;
            }
            self.regs.write16(BLOCK_SIZE, (7 << 12) | x.block_size);
            self.regs.write16(BLOCK_COUNT, x.blocks);
            self.regs.write32(ARGUMENT, arg);
            self.regs.write16(TRANSFER_MODE, mode);
        } else {
            self.regs.write32(ARGUMENT, arg);
            self.regs.write16(TRANSFER_MODE, 0);
        }
        self.regs.write16(COMMAND, flags);

        let status = self.wait_status(INT_RESPONSE, CMD_TIMEOUT_US, "a command's response");
        let status = match status {
            Ok(s) => s,
            Err(Error::Timeout(w)) => {
                let _ = self.reset_lines(RESET_CMD | RESET_DATA);
                return Err(Error::Timeout(w));
            }
            Err(e) => return Err(e),
        };
        if status & (INT_ERROR | INT_ERROR_MASK) != 0 {
            let _ = self.reset_lines(RESET_CMD | RESET_DATA);
            self.regs.write32(INT_STATUS, u32::MAX);
            return Err(Error::Command { cmd, status });
        }
        self.regs.write32(INT_STATUS, INT_RESPONSE);
        let response = [
            self.regs.read32(RESPONSE),
            self.regs.read32(RESPONSE + 4),
            self.regs.read32(RESPONSE + 8),
            self.regs.read32(RESPONSE + 12),
        ];

        if xfer.is_some() || rsp == Rsp::ShortBusy {
            let timeout = if xfer.is_some() {
                DATA_TIMEOUT_US
            } else {
                CMD_TIMEOUT_US
            };
            match self.wait_status(INT_DATA_END, timeout, "a data transfer") {
                Ok(s) if s & (INT_ERROR | INT_ERROR_MASK) != 0 => {
                    let adma = self.regs.read8(ADMA_ERROR);
                    let _ = self.reset_lines(RESET_CMD | RESET_DATA);
                    self.regs.write32(INT_STATUS, u32::MAX);
                    return Err(Error::Data {
                        cmd,
                        status: s,
                        adma,
                    });
                }
                Ok(_) => self.regs.write32(INT_STATUS, INT_DATA_END),
                Err(e) => {
                    let _ = self.reset_lines(RESET_CMD | RESET_DATA);
                    return Err(e);
                }
            }
        }
        Ok(response)
    }

    fn wait_status(&mut self, want: u32, timeout: u64, what: &'static str) -> Result<u32, Error> {
        let deadline = self.clock.now_us() + timeout;
        let mut spins = 0u32;
        loop {
            let status = self.regs.read32(INT_STATUS);
            if status & (want | INT_ERROR | INT_ERROR_MASK) != 0 {
                return Ok(status);
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout(what));
            }
            // Most responses are there at once: spin for the first 20 looks. After that, sleep until the controller
            // interrupts, or back off when there is no interrupt to sleep on.
            spins += 1;
            if spins <= 20 {
                self.clock.delay_us(5);
                continue;
            }
            let left = deadline.saturating_sub(self.clock.now_us());
            if !self.clock.wait_interrupt(left.max(1)) {
                self.clock.delay_us(if spins < 200 { 5 } else { 1000 });
            }
        }
    }

    fn app_command(
        &mut self,
        rca: u16,
        cmd: u8,
        arg: u32,
        rsp: Rsp,
        xfer: Option<Xfer>,
    ) -> Result<[u32; 4], Error> {
        self.command(55, (rca as u32) << 16, Rsp::Short, None)?;
        self.command(cmd, arg, rsp, xfer)
    }

    fn long(r: [u32; 4]) -> Long {
        // The registers hold bits 127-8 of the response with the CRC byte dropped (Linux `sdhci_read_rsp_136`).
        [
            (r[3] << 8) | (r[2] >> 24),
            (r[2] << 8) | (r[1] >> 24),
            (r[1] << 8) | (r[0] >> 24),
            r[0] << 8,
        ]
    }

    /// `mmc_sd_init_card`, at 3 V and without the UHS steps.
    fn init_card(&mut self) -> Result<(), Error> {
        self.command(0, 0, Rsp::None, None)?;
        self.clock.delay_us(2000);

        // CMD8 tells a version 2 card from an older one; older ones do not answer.
        let v2 = match self.command(8, 0x1aa, Rsp::Short, None) {
            Ok(r) if r[0] & 0xfff == 0x1aa => true,
            Ok(_) => return Err(Error::Unusable("CMD8 echoed another pattern")),
            Err(Error::Command { .. }) | Err(Error::Timeout(_)) => false,
            Err(e) => return Err(e),
        };

        let hcs = if v2 { card::OCR_HCS } else { 0 };
        let deadline = self.clock.now_us() + 1_000_000;
        let ocr = loop {
            let r = self.app_command(0, 41, hcs | card::OCR_VOLTAGES, Rsp::Ocr, None)?;
            if r[0] & card::OCR_BUSY_DONE != 0 {
                break r[0];
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Unusable("it stayed busy through ACMD41"));
            }
            self.clock.delay_us(10_000);
        };
        let high_capacity = ocr & card::OCR_CCS != 0;

        let cid = Cid::parse(&Self::long(self.command(2, 0, Rsp::Long, None)?));
        let rca = (self.command(3, 0, Rsp::Short, None)?[0] >> 16) as u16;
        let csd_raw = Self::long(self.command(9, (rca as u32) << 16, Rsp::Long, None)?);
        let csd =
            Csd::parse(&csd_raw).ok_or(Error::Unusable("its CSD is not one this driver reads"))?;
        self.command(7, (rca as u32) << 16, Rsp::ShortBusy, None)?;

        if !high_capacity {
            self.command(16, SECTOR as u32, Rsp::Short, None)?;
        }

        self.notes.write_switch_on = self.regs.read32(PRESENT_STATE) & PS_WRITE_ENABLED != 0;
        let read_only = !self.config.writable || !self.notes.write_switch_on || csd.write_protected;
        let mut info = Card {
            rca,
            high_capacity,
            sectors: csd.sectors,
            cid,
            csd,
            scr: None,
            bus_4bit: false,
            high_speed: false,
            clock_hz: self.set_clock(DEFAULT_CLOCK_HZ)?,
            read_only,
        };

        // From here on a failure costs speed, not the card (`mmc_sd_setup_card` treats a bad SCR as fatal; a slow
        // card is worth more on a machine with no other way to read it).
        if let Ok(scr) = self.read_scr(rca) {
            info.scr = Some(scr);
            if scr.bus_4bit && self.app_command(rca, 6, 2, Rsp::Short, None).is_ok() {
                let ctrl = self.regs.read8(HOST_CONTROL);
                self.regs.write8(HOST_CONTROL, ctrl | CTRL_4BIT);
                info.bus_4bit = true;
            }
            if scr.spec >= 1 && csd.class10 && self.switch_high_speed().unwrap_or(false) {
                let ctrl = self.regs.read8(HOST_CONTROL);
                self.regs.write8(HOST_CONTROL, ctrl | CTRL_HISPD);
                info.clock_hz = self.set_clock(HS_CLOCK_HZ)?;
                info.high_speed = true;
            }
        }
        self.card = Some(info);
        Ok(())
    }

    fn small_read(
        &mut self,
        cmd: u8,
        arg: u32,
        len: usize,
        app_rca: Option<u16>,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        let daddr = self.dma.daddr() + SMALL_OFFSET as u64;
        self.dma.sync_to_device(SMALL_OFFSET, len);
        self.write_table(&[DataSegment { daddr, len }])?;
        let xfer = Xfer {
            block_size: len as u16,
            blocks: 1,
            read: true,
        };
        let r = match app_rca {
            Some(rca) => self.app_command(rca, cmd, arg, Rsp::Short, Some(xfer))?,
            None => self.command(cmd, arg, Rsp::Short, Some(xfer))?,
        };
        if r[0] & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd, status: r[0] });
        }
        self.dma.sync_from_device(SMALL_OFFSET, len);
        self.dma.read(SMALL_OFFSET, &mut buf[..len]);
        Ok(())
    }

    fn read_scr(&mut self, rca: u16) -> Result<Scr, Error> {
        let mut raw = [0u8; 8];
        self.small_read(51, 0, 8, Some(rca), &mut raw)?;
        Ok(Scr::parse(&raw))
    }

    fn switch_high_speed(&mut self) -> Result<bool, Error> {
        let mut status = [0u8; card::SWITCH_STATUS_LEN];
        self.small_read(
            6,
            card::SWITCH_CHECK_HS,
            card::SWITCH_STATUS_LEN,
            None,
            &mut status,
        )?;
        if !card::switch_supports_hs(&status) {
            return Ok(false);
        }
        self.small_read(
            6,
            card::SWITCH_SET_HS,
            card::SWITCH_STATUS_LEN,
            None,
            &mut status,
        )?;
        // The card takes 8 clocks to switch.
        self.clock.delay_us(100);
        Ok(card::switch_selected_hs(&status))
    }

    fn transfer(
        &mut self,
        write: bool,
        lba: u64,
        sectors: u32,
        segments: &[DataSegment],
    ) -> Result<(), Error> {
        let card = *self.card();
        if write && card.read_only {
            return Err(Error::ReadOnly);
        }
        let bytes = sectors as usize * SECTOR;
        if sectors == 0 || bytes > MAX_TRANSFER || sectors > u16::MAX as u32 {
            return Err(Error::BadBuffer);
        }
        if segments.iter().map(|s| s.len).sum::<usize>() != bytes {
            return Err(Error::BadBuffer);
        }
        if lba
            .checked_add(sectors as u64)
            .is_none_or(|end| end > card.sectors)
        {
            return Err(Error::OutOfRange);
        }
        let arg = if card.high_capacity {
            lba
        } else {
            lba * SECTOR as u64
        };
        let arg = u32::try_from(arg).map_err(|_| Error::OutOfRange)?;

        self.write_table(segments)?;
        let cmd = match (write, sectors > 1) {
            (false, false) => 17,
            (false, true) => 18,
            (true, false) => 24,
            (true, true) => 25,
        };
        let r = self.command(
            cmd,
            arg,
            Rsp::Short,
            Some(Xfer {
                block_size: SECTOR as u16,
                blocks: sectors as u16,
                read: !write,
            }),
        )?;
        if r[0] & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd, status: r[0] });
        }
        if write {
            self.wait_programmed(card.rca)?;
        }
        Ok(())
    }

    /// Polls CMD13 until the card has programmed what it was sent: `mmc_blk_card_busy`.
    fn wait_programmed(&mut self, rca: u16) -> Result<(), Error> {
        let deadline = self.clock.now_us() + CARD_BUSY_TIMEOUT_US;
        loop {
            let status = self.command(13, (rca as u32) << 16, Rsp::Short, None)?[0];
            if status & card::R1_ERRORS != 0 {
                return Err(Error::CardStatus { cmd: 13, status });
            }
            if status & card::R1_READY_FOR_DATA != 0
                && (status >> card::R1_STATE_SHIFT) & 0xf == card::R1_STATE_TRAN
            {
                return Ok(());
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout("the card to program"));
            }
            self.clock.delay_us(100);
        }
    }

    /// Reads `sectors` 512-byte sectors from `lba` into the segments. The caller syncs them.
    pub fn read(&mut self, lba: u64, sectors: u32, segments: &[DataSegment]) -> Result<(), Error> {
        self.transfer(false, lba, sectors, segments)
    }

    /// Writes `sectors` 512-byte sectors at `lba`. Refused on a read-only host or card, before anything is sent.
    pub fn write(&mut self, lba: u64, sectors: u32, segments: &[DataSegment]) -> Result<(), Error> {
        self.transfer(true, lba, sectors, segments)
    }
}

/// Before a jump to another kernel: resets the command and data lines, which stops the ADMA engine, and masks the
/// interrupt signals. For a quiesce hook: it takes no host (a stopped CPU may hold its lock), and `now_us` must not
/// yield. Returns whether the reset was seen to finish within `REG_TIMEOUT_US`; if not, nothing proves the DMA ended.
pub fn quiesce(regs: &impl Regs, now_us: impl Fn() -> u64) -> bool {
    regs.write8(SOFTWARE_RESET, RESET_CMD | RESET_DATA);
    let deadline = now_us() + REG_TIMEOUT_US;
    let done = loop {
        if regs.read8(SOFTWARE_RESET) & (RESET_CMD | RESET_DATA) == 0 {
            break true;
        }
        if now_us() >= deadline {
            break false;
        }
        core::hint::spin_loop();
    };
    regs.write32(SIGNAL_ENABLE, 0);
    done
}
