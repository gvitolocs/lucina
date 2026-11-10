//! The controller and the card: bring-up, commands, reads, and writes inside one window.
//!
//! The controller's bring-up follows Linux `sdhci_esdhc_imx_hwinit` and `esdhc_reset`, the clock `esdhc_pltfm_set_clock`
//! with Amazon's divider. The card's follows `mmc_init_card`, without what only matters for writing (cache, erase groups,
//! power-off notification) or for speeds this board has no pads for: 8 bits at 52 MHz with a single data rate is what the
//! device tree says and what Amazon's kernel ends up with unless the controller says it has DDR.

use crate::{
    Error,
    card::{self, Card, Cid, EXT_CSD_SIZE, ext_csd},
    regs::*,
};

/// Register access, 32 bits at a time.
pub trait Regs {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

/// The driver's own DMA memory: [`AREA_SIZE`] bytes the controller sees at [`DmaArea::daddr`].
pub trait DmaArea {
    fn daddr(&self) -> u32;
    fn read(&self, offset: usize, buf: &mut [u8]);
    /// Called before the controller writes `len` bytes at `offset`, so that nothing dirty is evicted over them.
    fn sync_to_device(&self, _offset: usize, _len: usize) {}
    /// Called after the controller wrote `len` bytes at `offset` and before the CPU reads them.
    fn sync_from_device(&self, _offset: usize, _len: usize) {}
}

/// Time for timeouts and polling delays.
pub trait Clock {
    fn now_us(&self) -> u64;
    /// Waits about `us` microseconds (may yield to other threads).
    fn delay_us(&self, us: u64);
}

/// Holds EXT_CSD.
pub const AREA_SIZE: usize = 0x1000;
pub const SECTOR: usize = 512;
/// SDMA runs to at most the next multiple of this address, which is also the most Linux moves in one request here.
pub const BOUNDARY: usize = 512 * 1024;

const INIT_CLOCK_HZ: u32 = 400_000;
const LEGACY_CLOCK_HZ: u32 = 26_000_000;
const HS26_CLOCK_HZ: u32 = 26_000_000;
const HS52_CLOCK_HZ: u32 = 52_000_000;

const CMD_TIMEOUT_US: u64 = 1_000_000;
const DATA_TIMEOUT_US: u64 = 5_000_000;
const REG_TIMEOUT_US: u64 = 100_000;
const BUSY_TIMEOUT_US: u64 = 1_000_000;
/// The longest a CMD6 takes on any card EXT_CSD can describe; GENERIC_CMD6_TIME is in units of 10 ms up to 2.55 s.
const SWITCH_TIMEOUT_US: u64 = 3_000_000;
const CMD1_TIMEOUT_US: u64 = 1_000_000;
const CMD1_PERIOD_US: u64 = 4_000;
/// How long a card may program a write or empty its cache. Linux waits ten minutes; this is a kernel that can try again.
const PROGRAM_TIMEOUT_US: u64 = 30_000_000;
/// CMD6: `CACHE_CTRL` to 0, which also empties the cache.
const CACHE_OFF_ARG: u32 = (3 << 24) | ((ext_csd::CACHE_CTRL as u32) << 16) | 1;

/// The bytes of EXT_CSD that CMD6 may set, and to what. Both are lost at a reset and hold nothing about the card's data.
/// Every other byte is refused: the same byte can select a partition and say what the card boots from, others are one-time
/// fuses (`BOOT_WP`, `RST_N_FUNCTION`, the partition sizes).
pub const SWITCHABLE: [(u8, &[u8]); 2] = [
    (ext_csd::HS_TIMING as u8, &[0, 1]),
    (ext_csd::BUS_WIDTH as u8, &[0, 1, 2]),
];

/// Whether a command may reach the card. This is the only way there, and nothing that writes is on it.
pub fn allowed(cmd: u8, arg: u32) -> bool {
    match cmd {
        // Argument 0 only: others make the card pre-idle or stream its boot partition.
        0 => arg == 0,
        // OCR, CID, the address, select, EXT_CSD, stop, status, read.
        1 | 2 | 3 | 7 | 8 | 12 | 13 | 17 | 18 => true,
        6 => {
            let (access, index, value, set) = (arg >> 24, (arg >> 16) as u8, (arg >> 8) as u8, arg as u8);
            access == 3
                && set == 1
                && SWITCHABLE
                    .iter()
                    .any(|(byte, values)| *byte == index && values.contains(&value))
        }
        _ => false,
    }
}

/// The sectors a host may write: [`Host::allow_writes`]. Without one nothing is written, and with one nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteWindow {
    pub start: u64,
    pub end: u64,
}

/// U-Boot's pending update of its `idme` block: on every boot it takes this from the user area, applies it and clears it.
/// It is how a kernel it started says which kernel comes after it. Its layout is `lucina-init`'s `idme.rs`.
pub const UPDATE_LBA: u64 = 0x3f000 / SECTOR as u64;
pub const UPDATE_BYTES: usize = 5 * 1024;
const UPDATE_MAGIC_AT: usize = 0x1f0;
const UPDATE_MAGIC: &[u8; 16] = b"abcdefghhgfedcba";
const UPDATE_BOOTMODE_AT: usize = 0x1000;
const UPDATE_BOOTMODE_LEN: usize = 16;

/// The window of the pending update.
pub const UPDATE_WINDOW: WriteWindow = WriteWindow {
    start: UPDATE_LBA,
    end: UPDATE_LBA + (UPDATE_BYTES / SECTOR) as u64,
};

/// Whether this is a whole update that sends the next boot to the `main` slot, the only one a kernel may ask for.
pub fn update_goes_home(block: &[u8]) -> bool {
    if block.len() != UPDATE_BYTES || &block[UPDATE_MAGIC_AT..UPDATE_MAGIC_AT + UPDATE_MAGIC.len()] != UPDATE_MAGIC {
        return false;
    }
    let mode = &block[UPDATE_BOOTMODE_AT..UPDATE_BOOTMODE_AT + UPDATE_BOOTMODE_LEN];
    mode.starts_with(b"main") && mode[4..].iter().all(|byte| *byte == 0)
}

/// The boot mode of a whole pending update, up to its first zero, or `None` if this is not one.
pub fn update_bootmode(block: &[u8]) -> Option<&[u8]> {
    if block.len() != UPDATE_BYTES || &block[UPDATE_MAGIC_AT..UPDATE_MAGIC_AT + UPDATE_MAGIC.len()] != UPDATE_MAGIC {
        return None;
    }
    let field = &block[UPDATE_BOOTMODE_AT..UPDATE_BOOTMODE_AT + UPDATE_BOOTMODE_LEN];
    Some(&field[..field.iter().position(|byte| *byte == 0).unwrap_or(UPDATE_BOOTMODE_LEN)])
}

/// Gives a whole pending update another boot mode, changing nothing else; `false` if it is not one or the mode is too long.
pub fn set_update_bootmode(block: &mut [u8], mode: &[u8]) -> bool {
    if update_bootmode(block).is_none() || mode.is_empty() || mode.len() >= UPDATE_BOOTMODE_LEN {
        return false;
    }
    let field = &mut block[UPDATE_BOOTMODE_AT..UPDATE_BOOTMODE_AT + UPDATE_BOOTMODE_LEN];
    field.fill(0);
    field[..mode.len()].copy_from_slice(mode);
    true
}

impl WriteWindow {
    pub fn holds(&self, lba: u64, sectors: u64) -> bool {
        sectors > 0 && lba >= self.start && lba.checked_add(sectors).is_some_and(|end| end <= self.end)
    }
}

/// The window of partition `number` (1 to 4) in a master boot record, if it is a FAT volume, the only thing written to.
pub fn fat_partition(sector0: &[u8; SECTOR], number: usize) -> Option<WriteWindow> {
    if !(1..=4).contains(&number) || sector0[510..512] != [0x55, 0xaa] {
        return None;
    }
    let entry = &sector0[446 + (number - 1) * 16..][..16];
    let word = |at: usize| u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]]) as u64;
    let (start, sectors) = (word(8), word(12));
    (matches!(entry[4], 0x0b | 0x0c) && start > 0 && sectors > 0).then_some(WriteWindow {
        start,
        end: start + sectors,
    })
}

/// One piece of memory for a transfer, at a device address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataSegment {
    pub daddr: u32,
    pub len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// The controller's clock, from the clock controller: its capabilities do not tell.
    pub base_hz: u32,
    /// The fastest clock for the card.
    pub max_hz: u32,
}

/// What the registers say before the driver does anything. Reading this writes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe {
    pub present: u32,
    pub prot: u32,
    pub sys: u32,
    pub int_status: u32,
    pub caps: u32,
    pub watermarks: u32,
    pub mix: u32,
    pub vendor: u32,
}

impl Probe {
    pub fn read(regs: &impl Regs) -> Self {
        Self {
            present: regs.read32(PRES_STATE),
            prot: regs.read32(PROT_CTRL),
            sys: regs.read32(SYS_CTRL),
            int_status: regs.read32(INT_STATUS),
            caps: regs.read32(HOST_CTRL_CAP),
            watermarks: regs.read32(WTMK_LVL),
            mix: regs.read32(MIX_CTRL),
            vendor: regs.read32(VEND_SPEC),
        }
    }
}

/// A command's response format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rsp {
    None,
    /// R1.
    Short,
    /// R1b.
    ShortBusy,
    /// R2.
    Long,
    /// R3: no CRC, no index.
    Ocr,
}

impl Rsp {
    fn bits(self) -> u32 {
        match self {
            Rsp::None => 0,
            Rsp::Short => CMD_RSP_SHORT | CMD_CRC_CHECK | CMD_INDEX_CHECK,
            Rsp::ShortBusy => CMD_RSP_SHORT_BUSY | CMD_CRC_CHECK | CMD_INDEX_CHECK,
            Rsp::Long => CMD_RSP_LONG | CMD_CRC_CHECK,
            Rsp::Ocr => CMD_RSP_SHORT,
        }
    }
}

/// A command's data.
#[derive(Clone, Copy)]
struct Data {
    daddr: u32,
    block_size: u32,
    blocks: u32,
    write: bool,
}

/// What can be moved with one command from `segments`, from `at` (segment, bytes into it): contiguous, whole sectors, up
/// to the next [`BOUNDARY`]. Returns the address, the length and where the next one starts.
pub(crate) fn next_chunk(
    segments: &[DataSegment],
    at: (usize, usize),
) -> Result<(u32, usize, (usize, usize)), Error> {
    let (mut index, offset) = at;
    let first = segments.get(index).ok_or(Error::BadBuffer)?;
    // A segment of nothing would never be passed.
    let start = first.daddr as usize + offset;
    if !start.is_multiple_of(4) || first.len <= offset {
        return Err(Error::BadBuffer);
    }
    let limit = BOUNDARY - start % BOUNDARY;
    let mut len = first.len - offset;
    while len < limit {
        match segments.get(index + 1) {
            Some(next) if next.daddr as usize == start + len && next.len > 0 => {
                len += next.len;
                index += 1;
            }
            _ => break,
        }
    }
    let chunk = len.min(limit);
    let chunk = chunk - chunk % SECTOR;
    if chunk == 0 {
        return Err(Error::BadBuffer);
    }
    let mut rest = offset + chunk;
    let mut next = at.0;
    while next < segments.len() && rest >= segments[next].len {
        rest -= segments[next].len;
        next += 1;
    }
    Ok((start as u32, chunk, (next, rest)))
}

pub struct Host<R: Regs, D: DmaArea, C: Clock> {
    regs: R,
    dma: D,
    clock: C,
    config: Config,
    card: Option<Card>,
    write_window: Option<WriteWindow>,
}

impl<R: Regs, D: DmaArea, C: Clock> Host<R, D, C> {
    /// Resets the controller and initialises the card. The controller's clock has to be on already: with it off, any access
    /// to these registers hangs the bus.
    pub fn start(regs: R, dma: D, clock: C, config: Config) -> Result<Self, Error> {
        let mut host = Self {
            regs,
            dma,
            clock,
            config,
            card: None,
            write_window: None,
        };
        host.start_controller()?;
        host.init_card()?;
        Ok(host)
    }

    pub fn card(&self) -> &Card {
        self.card.as_ref().expect("a host exists only with a card")
    }

    /// Whether a command may reach the card: [`allowed`], and what the window lets through.
    fn may_send(&self, cmd: u8, arg: u32, data: Option<&Data>) -> bool {
        if allowed(cmd, arg) {
            return data.is_none_or(|d| !d.write);
        }
        let Some(window) = self.write_window else {
            return false;
        };
        match (cmd, data) {
            (24 | 25, Some(d)) => {
                d.write && (cmd == 25 || d.blocks == 1) && window.holds(arg as u64, d.blocks as u64)
            }
            (6, None) => arg == CACHE_OFF_ARG,
            _ => false,
        }
    }

    fn set_bits(&self, offset: usize, clear: u32, set: u32) {
        self.regs.write32(offset, (self.regs.read32(offset) & !clear) | set);
    }

    fn wait_register(
        &self,
        offset: usize,
        mask: u32,
        want: u32,
        timeout: u64,
        what: &'static str,
    ) -> Result<(), Error> {
        let deadline = self.clock.now_us() + timeout;
        while self.regs.read32(offset) & mask != want {
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout(what));
            }
            self.clock.delay_us(2);
        }
        Ok(())
    }

    fn start_controller(&mut self) -> Result<(), Error> {
        // A reset during a transfer can hang the system (`esdhc_reset`). If it does not end, the reset is all that is left.
        let _ = self.wait_register(PRES_STATE, PS_DATA_INHIBIT, 0, REG_TIMEOUT_US, "the data lines");

        // What the ROM or the kernel before this one left, as `sdhci_esdhc_imx_probe` and `_hwinit`.
        self.regs.write32(MIX_CTRL, 0);
        self.regs.write32(AUTOCMD12_ERR, 0);
        self.regs.write32(TUNE_CTRL_STATUS, 0);
        self.regs.write32(DLL_CTRL, 0);
        self.set_bits(TUNING_CTRL, TUNING_STANDARD, 0);

        self.set_bits(SYS_CTRL, 0, SYS_RSTA);
        self.wait_register(SYS_CTRL, SYS_RSTA, 0, REG_TIMEOUT_US, "the controller's reset")?;
        // The reset clears these, and the controller must not be left so.
        self.set_bits(SYS_CTRL, 0, SYS_CLOCKS_ON);
        self.set_bits(MIX_CTRL, !MIX_TUNING, 0);

        self.regs.write32(WTMK_LVL, WTMK_VALUE);
        self.set_bits(
            PROT_CTRL,
            PROT_DTW_MASK | PROT_D3CD | PROT_EMODE_MASK | PROT_DMASEL_MASK,
            PROT_EMODE_LE | PROT_BURST_LEN_INCR,
        );
        self.set_bits(SYS_CTRL, SYS_DTOCV_MASK, DTOCV_VALUE << SYS_DTOCV_SHIFT);
        self.regs.write32(INT_STATUS_EN, INT_USED);
        self.regs.write32(INT_SIGNAL_EN, 0);
        self.regs.write32(INT_STATUS, u32::MAX);

        self.set_clock(INIT_CLOCK_HZ)?;
        Ok(())
    }

    /// `esdhc_pltfm_set_clock`.
    fn set_clock(&mut self, target_hz: u32) -> Result<u32, Error> {
        if self.config.base_hz == 0 {
            return Err(Error::Unusable("no clock for the controller"));
        }
        self.set_bits(VEND_SPEC, VEND_FORCE_CLOCK_ON, 0);
        // Linux only warns about the next two, so a slow one is no reason to stop.
        let _ = self.wait_register(PRES_STATE, PS_CLOCK_OFF, PS_CLOCK_OFF, 100, "the clock to stop");
        self.set_bits(SYS_CTRL, SYS_CLOCKS_ON | SYS_DIVIDERS, 0);
        let (fields, actual) = clock_fields(self.config.base_hz, target_hz);
        self.set_bits(SYS_CTRL, 0, SYS_CLOCKS_ON | fields);
        let _ = self.wait_register(PRES_STATE, PS_CLOCK_STABLE, PS_CLOCK_STABLE, 100, "the clock");
        self.set_bits(VEND_SPEC, 0, VEND_FORCE_CLOCK_ON);
        // Amazon's wait, and at 400 kHz well over the 74 clocks a card needs before its first command.
        self.clock.delay_us(1000);
        Ok(actual)
    }

    /// Resets the command and data lines, and puts back what that clears.
    fn reset_lines(&mut self) {
        let _ = self.wait_register(PRES_STATE, PS_DATA_INHIBIT, 0, REG_TIMEOUT_US, "the data lines");
        let prot = self.regs.read32(PROT_CTRL);
        self.set_bits(SYS_CTRL, 0, SYS_RSTC | SYS_RSTD);
        let _ = self.wait_register(SYS_CTRL, SYS_RSTC | SYS_RSTD, 0, REG_TIMEOUT_US, "a line reset");
        self.regs.write32(PROT_CTRL, prot);
        self.regs.write32(INT_STATUS, u32::MAX);
    }

    /// Sends a command and, with `data`, runs its data phase. Returns the response registers.
    fn command(
        &mut self,
        cmd: u8,
        arg: u32,
        rsp: Rsp,
        data: Option<Data>,
    ) -> Result<[u32; 4], Error> {
        if !self.may_send(cmd, arg, data.as_ref()) {
            return Err(Error::Refused { cmd, arg });
        }
        // CMD12 is what ends a data phase, so it cannot wait for the data lines.
        let inhibit = if cmd != 12 && (data.is_some() || rsp == Rsp::ShortBusy) {
            PS_CMD_INHIBIT | PS_DATA_INHIBIT
        } else {
            PS_CMD_INHIBIT
        };
        if let Err(e) = self.wait_register(PRES_STATE, inhibit, 0, CMD_TIMEOUT_US, "the lines to be free") {
            self.reset_lines();
            return Err(e);
        }
        self.regs.write32(INT_STATUS, u32::MAX);

        let mut xfer = ((cmd as u32) << CMD_INDEX_SHIFT) | rsp.bits();
        if cmd == 12 {
            xfer |= CMD_TYPE_ABORT;
        }
        let mut mix = self.regs.read32(MIX_CTRL) & !MIX_TRANSFER;
        if let Some(d) = data {
            xfer |= CMD_DATA;
            mix |= MIX_DMA | MIX_BLOCK_COUNT;
            if !d.write {
                mix |= MIX_READ;
            }
            if d.blocks > 1 {
                mix |= MIX_MULTI;
            }
            self.regs.write32(DS_ADDR, d.daddr);
            self.regs.write32(BLK_ATT, (d.blocks << 16) | d.block_size);
        }
        self.regs.write32(CMD_ARG, arg);
        self.regs.write32(MIX_CTRL, mix);
        self.regs.write32(CMD_XFR_TYP, xfer);

        let status = match self.wait_status(INT_CMD_DONE, CMD_TIMEOUT_US, "a command's response") {
            Ok(status) => status,
            Err(e) => {
                self.reset_lines();
                return Err(e);
            }
        };
        // A data error can come with the response. It stays set for the data phase below.
        if status & INT_CMD_ERRORS != 0 || status & INT_CMD_DONE == 0 {
            self.reset_lines();
            return Err(Error::Command { cmd, status });
        }
        self.regs.write32(INT_STATUS, INT_CMD_DONE);
        let response = [
            self.regs.read32(CMD_RSP0),
            self.regs.read32(CMD_RSP0 + 4),
            self.regs.read32(CMD_RSP0 + 8),
            self.regs.read32(CMD_RSP0 + 12),
        ];

        if data.is_some() {
            match self.wait_status(INT_DATA_DONE, DATA_TIMEOUT_US, "a data transfer") {
                Ok(status) if status & INT_ERRORS != 0 => {
                    self.reset_lines();
                    return Err(Error::Data { cmd, status });
                }
                Ok(_) => self.regs.write32(INT_STATUS, INT_DATA_DONE | INT_DMA),
                Err(e) => {
                    self.reset_lines();
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
            if status & (want | INT_ERRORS) != 0 {
                return Ok(status);
            }
            if status & INT_DMA != 0 {
                // DMA stopped at a boundary, and continues from the address it reached when told.
                self.regs.write32(INT_STATUS, INT_DMA);
                self.regs.write32(DS_ADDR, self.regs.read32(DS_ADDR));
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout(what));
            }
            // Spin for the fast commands, then let other threads run.
            spins += 1;
            self.clock.delay_us(if spins < 200 { 5 } else { 1000 });
        }
    }

    /// The controller here does not say when a card's busy ends (`SDHCI_QUIRK_NO_BUSY_IRQ`), and DAT0 does.
    fn wait_not_busy(&self, timeout: u64) -> Result<(), Error> {
        let deadline = self.clock.now_us() + timeout;
        let mut spins = 0u32;
        while self.regs.read32(PRES_STATE) & PS_DAT0 == 0 {
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout("the card to stop being busy"));
            }
            spins += 1;
            self.clock.delay_us(if spins < 200 { 5 } else { 1000 });
        }
        Ok(())
    }

    fn status(&mut self) -> Result<u32, Error> {
        Ok(self.command(13, (card::RCA as u32) << 16, Rsp::Short, None)?[0])
    }

    /// CMD6, and its result.
    fn switch(&mut self, index: usize, value: u8) -> Result<(), Error> {
        let arg = (3 << 24) | ((index as u32) << 16) | ((value as u32) << 8) | 1;
        self.command(6, arg, Rsp::ShortBusy, None)?;
        self.wait_not_busy(SWITCH_TIMEOUT_US)?;
        let status = self.status()?;
        if status & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd: 6, status });
        }
        Ok(())
    }

    /// CMD1 until the card is ready, as `mmc_send_op_cond`.
    fn op_cond(&mut self, arg: u32) -> Result<u32, Error> {
        let deadline = self.clock.now_us() + CMD1_TIMEOUT_US;
        loop {
            let ocr = self.command(1, arg, Rsp::Ocr, None)?[0];
            if ocr & card::OCR_READY != 0 {
                return Ok(ocr);
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Unusable("it stayed busy through CMD1"));
            }
            self.clock.delay_us(CMD1_PERIOD_US);
        }
    }

    fn read_ext_csd(&mut self, ext: &mut [u8; EXT_CSD_SIZE]) -> Result<(), Error> {
        self.dma.sync_to_device(0, EXT_CSD_SIZE);
        let r = self.command(
            8,
            0,
            Rsp::Short,
            Some(Data {
                daddr: self.dma.daddr(),
                block_size: EXT_CSD_SIZE as u32,
                blocks: 1,
                write: false,
            }),
        )?;
        if r[0] & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd: 8, status: r[0] });
        }
        self.dma.sync_from_device(0, EXT_CSD_SIZE);
        self.dma.read(0, ext);
        Ok(())
    }

    fn set_host_width(&self, dtw: u32) {
        self.set_bits(PROT_CTRL, PROT_DTW_MASK, dtw);
    }

    /// `mmc_init_card`.
    fn init_card(&mut self) -> Result<(), Error> {
        // The first answer to CMD1 says what the card takes. CMD0 again is what Linux does "for eMMC that are asleep".
        self.command(0, 0, Rsp::None, None)?;
        self.clock.delay_us(1000);
        let ocr = self.command(1, 0, Rsp::Ocr, None)?[0];
        if ocr & card::OCR_WINDOW == 0 {
            return Err(Error::Unusable("it does not take this voltage"));
        }
        self.command(0, 0, Rsp::None, None)?;
        self.clock.delay_us(1000);
        let ocr = self.op_cond((ocr & card::OCR_WINDOW) | card::OCR_SECTOR_MODE)?;
        if ocr & card::OCR_SECTOR_MODE == 0 {
            return Err(Error::Unusable("it is addressed by byte"));
        }

        let cid = Cid::parse(&self.command(2, 0, Rsp::Long, None)?);
        let rca_arg = (card::RCA as u32) << 16;
        self.command(3, rca_arg, Rsp::Short, None)?;
        self.command(7, rca_arg, Rsp::ShortBusy, None)?;
        self.wait_not_busy(BUSY_TIMEOUT_US)?;
        let status = self.status()?;
        if status & card::R1_ERRORS != 0 || (status >> card::R1_STATE_SHIFT) & 0xf != card::R1_STATE_TRAN {
            return Err(Error::CardStatus { cmd: 7, status });
        }

        // CMD0 has always been enough after a bootloader's 8 bits, but nothing says it must be: a card still on a wide bus
        // is told otherwise.
        let mut ext = [0u8; EXT_CSD_SIZE];
        if self.read_ext_csd(&mut ext).is_err() {
            self.switch(ext_csd::BUS_WIDTH, 0)?;
            self.switch(ext_csd::HS_TIMING, 0)?;
            self.read_ext_csd(&mut ext)?;
        }

        let partition = ext[ext_csd::PARTITION_CONFIG] & 7;
        if partition != 0 {
            return Err(Error::OtherPartition(partition));
        }
        let sectors = Card::sectors_of(&ext);
        if sectors == 0 {
            return Err(Error::Unusable("EXT_CSD gives it no size"));
        }

        // The clock first follows the card's speed, the bus width only after that: `mmc_select_timing`, then
        // `mmc_select_bus_width`. A failure from here on costs speed and not the card.
        let card_type = ext[ext_csd::CARD_TYPE];
        let hs_hz = if card_type & 2 != 0 {
            Some(HS52_CLOCK_HZ)
        } else if card_type & 1 != 0 {
            Some(HS26_CLOCK_HZ)
        } else {
            None
        };
        let (high_speed, target) = match hs_hz {
            Some(hz) if self.switch(ext_csd::HS_TIMING, 1).is_ok() => (true, hz),
            _ => (false, LEGACY_CLOCK_HZ),
        };
        let clock_hz = self.set_clock(target.min(self.config.max_hz))?;

        // No bus test on this controller in either kernel: a read over the new bus has to match the one over the old.
        let mut bus_bits = 1;
        let mut wide = [0u8; EXT_CSD_SIZE];
        for (value, dtw, bits) in [(2, PROT_DTW_8, 8), (1, PROT_DTW_4, 4)] {
            if self.switch(ext_csd::BUS_WIDTH, value).is_err() {
                continue;
            }
            self.set_host_width(dtw);
            if self.read_ext_csd(&mut wide).is_ok() && card::same_read_only_bytes(&ext, &wide) {
                bus_bits = bits;
                break;
            }
            self.set_host_width(0);
        }
        if bus_bits == 1 {
            self.switch(ext_csd::BUS_WIDTH, 0)?;
            self.read_ext_csd(&mut wide)?;
        }

        self.card = Some(Card {
            cid,
            sectors,
            ext_csd_rev: ext[ext_csd::REV],
            card_type,
            rst_n_function: ext[ext_csd::RST_N_FUNCTION],
            partition_config: ext[ext_csd::PARTITION_CONFIG],
            boot_kib: ext[ext_csd::BOOT_SIZE_MULT] as u32 * 128,
            rpmb_kib: ext[ext_csd::RPMB_SIZE_MULT] as u32 * 128,
            bus_bits,
            high_speed,
            clock_hz,
            cache_on: ext[ext_csd::CACHE_CTRL] & 1 != 0,
        });
        Ok(())
    }

    /// One command's read. Linux has CMD12 follow the data on this chip: it has no working Auto CMD23, and sets no Auto CMD12.
    fn read_chunk(&mut self, lba: u64, daddr: u32, len: usize) -> Result<(), Error> {
        let blocks = (len / SECTOR) as u32;
        let cmd = if blocks > 1 { 18 } else { 17 };
        let data = Data {
            daddr,
            block_size: SECTOR as u32,
            blocks,
            write: false,
        };
        let r = self.command(cmd, lba as u32, Rsp::Short, Some(data))?;
        if r[0] & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd, status: r[0] });
        }
        if blocks > 1 {
            self.command(12, 0, Rsp::ShortBusy, None)?;
            self.wait_not_busy(BUSY_TIMEOUT_US)?;
        }
        Ok(())
    }

    /// One command's write, ended like a read's, and then waited for: the card is programming until it says ready again.
    fn write_chunk(&mut self, lba: u64, daddr: u32, len: usize) -> Result<(), Error> {
        let blocks = (len / SECTOR) as u32;
        let cmd = if blocks > 1 { 25 } else { 24 };
        let data = Data {
            daddr,
            block_size: SECTOR as u32,
            blocks,
            write: true,
        };
        let r = self.command(cmd, lba as u32, Rsp::Short, Some(data))?;
        if r[0] & card::R1_ERRORS != 0 {
            return Err(Error::CardStatus { cmd, status: r[0] });
        }
        if blocks > 1 {
            self.command(12, 0, Rsp::ShortBusy, None)?;
        }
        self.wait_not_busy(PROGRAM_TIMEOUT_US)?;
        self.wait_programmed(cmd)
    }

    /// `card_busy_detect`.
    fn wait_programmed(&mut self, cmd: u8) -> Result<(), Error> {
        let deadline = self.clock.now_us() + PROGRAM_TIMEOUT_US;
        loop {
            let status = self.status()?;
            if status & card::R1_ERRORS != 0 {
                return Err(Error::CardStatus { cmd, status });
            }
            if status & card::R1_READY_FOR_DATA != 0 && (status >> card::R1_STATE_SHIFT) & 0xf == card::R1_STATE_TRAN {
                return Ok(());
            }
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout("the card to program a write"));
            }
            self.clock.delay_us(1000);
        }
    }

    /// Lets writes through to `window`, on a card with no cache in the way: what is written is on the flash when the write
    /// ends, so that no flush is needed and a lost one cannot lose anything.
    pub fn allow_writes(&mut self, window: WriteWindow) -> Result<(), Error> {
        if window.start >= window.end || window.end > self.card().sectors {
            return Err(Error::OutOfRange);
        }
        self.write_window = Some(window);
        if self.card().cache_on {
            let result = self
                .command(6, CACHE_OFF_ARG, Rsp::ShortBusy, None)
                .and_then(|_| self.wait_not_busy(PROGRAM_TIMEOUT_US))
                .and_then(|()| self.wait_programmed(6));
            if let Err(e) = result {
                self.write_window = None;
                return Err(e);
            }
            self.card.as_mut().expect("checked above").cache_on = false;
        }
        Ok(())
    }

    /// No writes from here on, until the next [`Self::allow_writes`].
    pub fn forbid_writes(&mut self) {
        self.write_window = None;
    }

    /// Reads one sector into the driver's own DMA memory, and from there into `buf`.
    pub fn read_sector(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<(), Error> {
        let daddr = self.dma.daddr();
        self.read(
            lba,
            &[DataSegment {
                daddr,
                len: SECTOR,
            }],
        )?;
        self.dma.sync_from_device(0, SECTOR);
        self.dma.read(0, buf);
        Ok(())
    }

    /// Writes whole 512-byte sectors at `lba` from the segments, which the caller has cleaned from the CPU's caches. A chunk
    /// that fails is tried once more, as the same data at the same place can do no harm.
    pub fn write(&mut self, mut lba: u64, segments: &[DataSegment]) -> Result<(), Error> {
        let bytes: usize = segments.iter().map(|s| s.len).sum();
        if bytes == 0 || !bytes.is_multiple_of(SECTOR) {
            return Err(Error::BadBuffer);
        }
        let sectors = (bytes / SECTOR) as u64;
        if !self.write_window.is_some_and(|w| w.holds(lba, sectors)) {
            return Err(Error::Refused { cmd: 25, arg: lba as u32 });
        }

        let mut at = (0, 0);
        while at.0 < segments.len() {
            let (daddr, len, next) = next_chunk(segments, at)?;
            if let Err(first) = self.write_chunk(lba, daddr, len) {
                self.recover();
                if self.write_chunk(lba, daddr, len).is_err() {
                    self.recover();
                    return Err(first);
                }
            }
            lba += (len / SECTOR) as u64;
            at = next;
        }
        Ok(())
    }

    /// After a failed read the card may still be sending. Stops it, and waits until it takes commands again.
    fn recover(&mut self) {
        let _ = self.command(12, 0, Rsp::ShortBusy, None);
        let _ = self.wait_not_busy(BUSY_TIMEOUT_US);
        let deadline = self.clock.now_us() + BUSY_TIMEOUT_US;
        while self.clock.now_us() < deadline {
            match self.status() {
                Ok(status) if (status >> card::R1_STATE_SHIFT) & 0xf == card::R1_STATE_TRAN => return,
                _ => self.clock.delay_us(1000),
            }
        }
    }

    /// Reads whole 512-byte sectors from `lba` into the segments, which must each be at a 4-byte boundary. The caller syncs
    /// them. A chunk that fails is tried once more.
    pub fn read(&mut self, mut lba: u64, segments: &[DataSegment]) -> Result<(), Error> {
        let bytes: usize = segments.iter().map(|s| s.len).sum();
        if bytes == 0 || !bytes.is_multiple_of(SECTOR) {
            return Err(Error::BadBuffer);
        }
        let end = lba
            .checked_add((bytes / SECTOR) as u64)
            .ok_or(Error::OutOfRange)?;
        if end > self.card().sectors {
            return Err(Error::OutOfRange);
        }

        let mut at = (0, 0);
        while at.0 < segments.len() {
            let (daddr, len, next) = next_chunk(segments, at)?;
            if let Err(first) = self.read_chunk(lba, daddr, len) {
                self.recover();
                if self.read_chunk(lba, daddr, len).is_err() {
                    self.recover();
                    return Err(first);
                }
            }
            lba += (len / SECTOR) as u64;
            at = next;
        }
        Ok(())
    }
}
