//! A Qualcomm GENI serial engine (QUPv3; the `qcom,geni-i2c` nodes of SC7180) as an I2C master, in FIFO mode and
//! polled, following Linux's `i2c-qcom-geni.c` and `qcom-geni-se.c` (7.1.10; the cancel-then-abort error recovery
//! and the bus clear are ours, notes in `docs/research/gbgo-geni-i2c.md` sections 2.5 and 8).
//!
//! * [`Probe::read`] only reads what the firmware left: the protocol its serial engine firmware runs (I2C is 3; the
//!   kernel cannot load that firmware), whether the FIFO interface is enabled at all, the clock divider and bus
//!   timing, and the FIFO depth.
//! * [`I2c::new`] stops a command the firmware left running, prepares the engine as Linux's probe does
//!   (`geni_se_init`, FIFO packing for 8-bit words) and keeps the firmware's bus timing when it left one, else programs
//!   the speed from Linux's 19.2 MHz table. It never touches the engine's source clock: the firmware's boot menu already
//!   runs the keyboard on it. [`I2c::with_source`] is the same but for that fallback, which needs the source clock
//!   ([`gcc::RcgState`]) to really give 19.2 MHz, and refuses otherwise.
//! * [`I2c::transfer`] runs messages to one address with repeated starts between them (`STOP_STRETCH`), one GENI
//!   command each, polling `M_IRQ_STATUS`. On a NACK, bus error or lost arbitration it stops filling the FIFO,
//!   cancels the command and, if that does not finish, aborts it. [`I2c::transfer_recovering`] also tries one bus clear
//!   after a bus error or lost arbitration, for a bus with no other master.
//! * [`gcc`] reads the source clock and starts a stopped one; [`pad`] is a TLMM pad's control value.
//!
//! Interrupts stay off in the GIC: the engine's interrupt line is never mapped, only its status register is read.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

pub mod gcc;
pub mod pad;

/// Register access to a serial engine (the `reg` of its node, 0x4000 bytes), by byte offset.
pub trait Regs {
    fn read32(&mut self, offset: usize) -> u32;
    fn write32(&mut self, offset: usize, value: u32);
}

/// A clock for timeouts.
pub trait Clock {
    /// Microseconds since some fixed point.
    fn now_us(&mut self) -> u64;
}

/// What [`ticks_to_us`] assumes of a counter whose frequency register reads 0: the rate of SC7180's architected timer.
pub const FALLBACK_COUNTER_HZ: u64 = 19_200_000;

/// A free-running counter's ticks as microseconds. A frequency of 0 (`CNTFRQ_EL0` that the firmware never set) falls
/// back to [`FALLBACK_COUNTER_HZ`], as a huge rate of microseconds would expire every timeout at once.
pub fn ticks_to_us(ticks: u64, hz: u64) -> u64 {
    let hz = if hz == 0 { FALLBACK_COUNTER_HZ } else { hz };
    (u128::from(ticks) * 1_000_000 / u128::from(hz)) as u64
}

/// A clock for what must not need one: each look at it counts a microsecond. A register read takes some 100 ns to a
/// microsecond, so a timeout lasts between a tenth of what it says and what it says.
struct Polls(u64);

impl Clock for Polls {
    fn now_us(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
}

mod reg {
    pub const FORCE_DEFAULT: usize = 0x20;
    pub const OUTPUT_CTRL: usize = 0x24;
    pub const CGC_CTRL: usize = 0x28;
    pub const STATUS: usize = 0x40;
    pub const SER_M_CLK_CFG: usize = 0x48;
    pub const IF_DISABLE_RO: usize = 0x64;
    pub const FW_REVISION_RO: usize = 0x68;
    pub const CLK_SEL: usize = 0x7c;
    pub const BYTE_GRAN: usize = 0x254;
    pub const DMA_MODE_EN: usize = 0x258;
    pub const TX_PACKING_CFG0: usize = 0x260;
    pub const TX_PACKING_CFG1: usize = 0x264;
    pub const I2C_TX_TRANS_LEN: usize = 0x26c;
    pub const I2C_RX_TRANS_LEN: usize = 0x270;
    pub const I2C_SCL_COUNTERS: usize = 0x278;
    pub const RX_PACKING_CFG0: usize = 0x284;
    pub const RX_PACKING_CFG1: usize = 0x288;
    pub const M_CMD0: usize = 0x600;
    pub const M_CMD_CTRL: usize = 0x604;
    pub const M_IRQ_STATUS: usize = 0x610;
    pub const M_IRQ_EN: usize = 0x614;
    pub const M_IRQ_CLEAR: usize = 0x618;
    pub const S_IRQ_EN: usize = 0x644;
    pub const S_IRQ_CLEAR: usize = 0x648;
    pub const TX_FIFO: usize = 0x700;
    pub const RX_FIFO: usize = 0x780;
    pub const RX_FIFO_STATUS: usize = 0x804;
    pub const TX_WATERMARK: usize = 0x80c;
    pub const RX_WATERMARK: usize = 0x810;
    pub const RX_RFR_WATERMARK: usize = 0x814;
    pub const M_GP_LENGTH: usize = 0x910;
    pub const DMA_TX_IRQ_CLR: usize = 0xc44;
    pub const DMA_RX_IRQ_CLR: usize = 0xd44;
    pub const GSI_EVENT_EN: usize = 0xe18;
    pub const IRQ_EN: usize = 0xe1c;
    pub const HW_PARAM_0: usize = 0xe24;
    pub const HW_PARAM_1: usize = 0xe28;
    pub const DMA_GENERAL_CFG: usize = 0xe30;
}

/// `M_IRQ_STATUS` bits.
pub mod irq {
    pub const CMD_DONE: u32 = 1 << 0;
    pub const CMD_OVERRUN: u32 = 1 << 1;
    pub const ILLEGAL_CMD: u32 = 1 << 2;
    pub const CMD_FAILURE: u32 = 1 << 3;
    pub const CMD_CANCEL: u32 = 1 << 4;
    pub const CMD_ABORT: u32 = 1 << 5;
    /// I2C: unknown GP interrupt.
    pub const GP_IRQ_0: u32 = 1 << 9;
    /// I2C: the slave did not acknowledge.
    pub const GP_IRQ_1: u32 = 1 << 10;
    /// I2C: bus protocol error.
    pub const GP_IRQ_3: u32 = 1 << 12;
    /// I2C: arbitration lost.
    pub const GP_IRQ_4: u32 = 1 << 13;
    pub const RX_FIFO_WATERMARK: u32 = 1 << 26;
    pub const RX_FIFO_LAST: u32 = 1 << 27;
    pub const TX_FIFO_WATERMARK: u32 = 1 << 30;
    /// Linux's `SE_I2C_ERR`.
    pub const I2C_ERR: u32 =
        CMD_OVERRUN | ILLEGAL_CMD | CMD_FAILURE | GP_IRQ_1 | GP_IRQ_3 | GP_IRQ_4;
    /// The error bits `geni_i2c_irq`'s error branch tests: `SE_I2C_ERR` plus `M_CMD_ABORT` and `M_GP_IRQ_0`.
    pub const IRQ_ERR: u32 = I2C_ERR | CMD_ABORT | GP_IRQ_0;
    /// Linux's `M_COMMON_GENI_M_IRQ_EN`.
    pub const COMMON_EN: u32 =
        0x7e | (1 << 22) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 28) | (1 << 29);
    /// What `geni_se_select_fifo_mode` adds for a non-UART engine.
    pub const FIFO_EN: u32 = CMD_DONE | TX_FIFO_WATERMARK | RX_FIFO_WATERMARK | RX_FIFO_LAST;
}

/// Linux's `S_COMMON_GENI_S_IRQ_EN` (the secondary sequencer, unused by an I2C master but enabled by Linux too).
const S_COMMON_EN: u32 = 0x3e | 0x3e00 | (1 << 24) | (1 << 25);
/// `GENI_FW_REVISION_RO` protocol of an I2C engine.
pub const PROTOCOL_I2C: u8 = 3;
const OPCODE_WRITE: u32 = 1;
const OPCODE_READ: u32 = 2;
/// `I2C_BUS_CLEAR`. Linux defines it and never sends it; its recovery goes no further than cancel and abort.
/// Ours: `docs/research/gbgo-geni-i2c.md` section 8 — what the command really does is untested anywhere we can read.
const OPCODE_BUS_CLEAR: u32 = 6;
const STOP_STRETCH: u32 = 1 << 2;
/// Ours, not 7.1.10's: 7.1.10 aborts only after the 1 s message timeout (`geni_i2c_abort_xfer`); the cancel before it
/// comes from Linux master, which this tree does not have (`docs/research/gbgo-geni-i2c.md` section 2.5).
const CMD_CTRL_ABORT: u32 = 1 << 1;
const CMD_CTRL_CANCEL: u32 = 1 << 2;
/// `geni_se_config_packing(se, 8, 4, true, true, true)`: 8-bit words, 4 per FIFO word, little endian.
const PACKING_CFG0: u32 = 0x0007_f8fe;
const PACKING_CFG1: u32 = 0x000f_fefe;
/// Linux's per-message and abort timeouts (`XFER_TIMEOUT`, `ABORT_TIMEOUT`: one second each).
const XFER_TIMEOUT_US: u64 = 1_000_000;
const ABORT_TIMEOUT_US: u64 = 1_000_000;
/// How long a failed command gets to report `CMD_DONE` on its own before it is cancelled. Ours, no source
/// (`docs/research/gbgo-geni-i2c.md` sections 2.5 and 8).
const SETTLE_US: u64 = 10_000;
/// A bus clear is nine SCL pulses; a slave holding SCL low is all that makes it take this long. Ours, no source
/// (same sections).
const BUS_CLEAR_TIMEOUT_US: u64 = 100_000;
/// Polls of `SE_GENI_STATUS` for a stopped command to leave it. Ours, no source (same sections).
const IDLE_POLLS: u32 = 1000;
/// `TRANS_LEN` fields are 24 bits.
const MAX_LEN: usize = 0xff_ffff;

/// What the firmware left in a serial engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The protocol of the loaded serial engine firmware ([`PROTOCOL_I2C`] is 3; 255 or 0: none loaded).
    pub protocol: u8,
    pub fw_version: u8,
    /// `GENI_IF_DISABLE_RO.FIFO_IF_DISABLE`: the engine can only be driven through GPI DMA.
    pub fifo_disabled: bool,
    pub dma_mode: bool,
    pub m_clk_cfg: u32,
    pub clk_sel: u32,
    pub scl_counters: u32,
    /// FIFO depths in 32-bit words, and the FIFO width in bits.
    pub tx_depth: usize,
    pub rx_depth: usize,
    pub fifo_width: u32,
    /// `SE_GENI_STATUS.M_GENI_CMD_ACTIVE`: a command is running.
    pub busy: bool,
}

impl Probe {
    /// Reads the engine's state. `qup_hw_version` is the wrapper's `QUP_HW_VER_REG` (wrapper + 4), which decides
    /// how wide the FIFO depth fields are.
    pub fn read(r: &mut impl Regs, qup_hw_version: u32) -> Self {
        let (major, minor) = (qup_hw_version >> 28, (qup_hw_version >> 16) & 0xfff);
        let depth_mask = if major > 3 || (major == 3 && minor >= 10) {
            0xff
        } else {
            0x3f
        };
        let fw = r.read32(reg::FW_REVISION_RO);
        let hw0 = r.read32(reg::HW_PARAM_0);
        let hw1 = r.read32(reg::HW_PARAM_1);
        Probe {
            protocol: (fw >> 8) as u8,
            fw_version: fw as u8,
            fifo_disabled: r.read32(reg::IF_DISABLE_RO) & 1 != 0,
            dma_mode: r.read32(reg::DMA_MODE_EN) & 1 != 0,
            m_clk_cfg: r.read32(reg::SER_M_CLK_CFG),
            clk_sel: r.read32(reg::CLK_SEL) & 7,
            scl_counters: r.read32(reg::I2C_SCL_COUNTERS),
            tx_depth: ((hw0 >> 16) & depth_mask) as usize,
            rx_depth: ((hw1 >> 16) & depth_mask) as usize,
            fifo_width: (hw0 >> 24) & 0x3f,
            busy: r.read32(reg::STATUS) & 1 != 0,
        }
    }

    /// Whether the firmware left a bus timing: the serial clock enabled with a divider, and SCL counters.
    pub fn has_timing(&self) -> bool {
        self.m_clk_cfg & 1 != 0 && self.m_clk_cfg >> 4 != 0 && self.scl_counters != 0
    }

    /// The bus frequency the firmware's timing gives on a serial clock of `source_hz`.
    pub fn scl_hz(&self, source_hz: u32) -> Option<u32> {
        let div = (self.m_clk_cfg >> 4) & 0xfff;
        let cycle = self.scl_counters & 0x3ff;
        source_hz.checked_div(div.checked_mul(cycle)?)
    }
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "protocol {} (firmware {:#x}), FIFO {}{}, {}x{}-bit words TX / {} RX, M_CLK_CFG {:#x}, CLK_SEL {}, \
             SCL_COUNTERS {:#x}{}",
            self.protocol,
            self.fw_version,
            if self.fifo_disabled {
                "disabled (GPI DMA only)"
            } else {
                "enabled"
            },
            if self.dma_mode { ", DMA mode" } else { "" },
            self.tx_depth,
            self.fifo_width,
            self.rx_depth,
            self.m_clk_cfg,
            self.clk_sel,
            self.scl_counters,
            if self.busy {
                ", a command is running"
            } else {
                ""
            }
        )
    }
}

/// Bus speeds, from Linux's table for a 19.2 MHz serial clock (`geni_i2c_clk_map_19p2mhz`, 7.1.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Standard,
    Fast,
    FastPlus,
}

impl Speed {
    /// `GENI_SER_M_CLK_CFG` and `SE_I2C_SCL_COUNTERS`.
    fn registers(self) -> (u32, u32) {
        match self {
            // div 7, high 10, low 12, cycle 26: 100 kHz.
            Speed::Standard => (0x71, (10 << 20) | (12 << 10) | 26),
            // div 2, high 5, low 11, cycle 22: 400 kHz.
            Speed::Fast => (0x21, (5 << 20) | (11 << 10) | 22),
            // div 1, high 2, low 8, cycle 18: 1 MHz.
            Speed::FastPlus => (0x11, (2 << 20) | (8 << 10) | 18),
        }
    }
}

/// Where the bus timing came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Firmware,
    Programmed(Speed),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The engine's firmware runs another protocol (or none: the kernel cannot load it).
    NotI2c(u8),
    /// The FIFO interface is disabled: only GPI DMA could drive this engine.
    FifoDisabled,
    /// The hardware reports no FIFO.
    NoFifo,
    /// The firmware left no bus timing and the serial clock is not known to give 19.2 MHz, the only rate the timing
    /// table is for. `hz`: what it runs at, if that can be known.
    SourceClock {
        hz: Option<u32>,
    },
    /// A message is empty or longer than the 24-bit length fields.
    BadLength,
    /// The slave did not acknowledge: its address if `data` is false, else a data byte.
    Nack {
        data: bool,
    },
    BusError,
    ArbitrationLost,
    /// The engine refused or overran the command (`ILLEGAL_CMD`, `CMD_OVERRUN`, `CMD_FAILURE` alone).
    CommandFailed(u32),
    /// The command finished with fewer bytes than asked for.
    ShortRead {
        got: usize,
    },
    /// No `CMD_DONE` within a second; the command was cancelled or aborted.
    Timeout,
    /// The engine did not finish a cancel or an abort either: it needs a reset.
    Stuck,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Error::NotI2c(p) => write!(f, "serial engine runs protocol {p}, not I2C"),
            Error::FifoDisabled => f.write_str("FIFO interface disabled (GPI DMA only)"),
            Error::NoFifo => f.write_str("no FIFO"),
            Error::SourceClock { hz: Some(hz) } => {
                write!(
                    f,
                    "no firmware bus timing and a {hz} Hz serial clock: no timing guessed"
                )
            }
            Error::SourceClock { hz: None } => {
                f.write_str("no firmware bus timing and an unknown serial clock: no timing guessed")
            }
            Error::BadLength => f.write_str("empty or too long message"),
            Error::Nack { data: false } => f.write_str("address not acknowledged"),
            Error::Nack { data: true } => f.write_str("data not acknowledged"),
            Error::BusError => f.write_str("bus protocol error"),
            Error::ArbitrationLost => f.write_str("arbitration lost"),
            Error::CommandFailed(s) => write!(f, "command failed (M_IRQ_STATUS {s:#x})"),
            Error::ShortRead { got } => write!(f, "read ended after {got} bytes"),
            Error::Timeout => f.write_str("timeout"),
            Error::Stuck => f.write_str("cancel and abort did not finish"),
        }
    }
}

/// One message of a transfer.
pub enum Msg<'a> {
    Write(&'a [u8]),
    Read(&'a mut [u8]),
}

/// A serial engine prepared as an I2C master.
pub struct I2c {
    tx_depth: usize,
    timing: Timing,
    bus_clears: u32,
}

impl I2c {
    /// Prepares the engine as Linux's probe and `geni_se_init` do, keeping the firmware's bus timing if it left
    /// one, else programming `speed` for a source clock of 19.2 MHz, on the caller's word: [`I2c::with_source`] checks
    /// it. A command the firmware left running is stopped first.
    pub fn new(r: &mut impl Regs, probe: &Probe, speed: Speed) -> Result<I2c, Error> {
        Self::check(probe)?;
        Self::prepare(r, probe, speed, 0)
    }

    /// [`I2c::new`], with what the GCC says about the engine's source clock. Without firmware timing it selects the
    /// perf level that gives 19.2 MHz, as Linux's `geni_i2c_clk_map_idx` does, or refuses to program a guess and leaves
    /// the engine untouched.
    pub fn with_source(
        r: &mut impl Regs,
        probe: &Probe,
        speed: Speed,
        source: Option<&gcc::RcgState>,
    ) -> Result<I2c, Error> {
        Self::check(probe)?;
        let clk_sel = if probe.has_timing() {
            0
        } else {
            let source = source.ok_or(Error::SourceClock { hz: None })?;
            source.clk_sel_for(gcc::TCXO_HZ).ok_or(Error::SourceClock {
                hz: source.rate_hz(probe.clk_sel),
            })?
        };
        Self::prepare(r, probe, speed, clk_sel)
    }

    fn check(probe: &Probe) -> Result<(), Error> {
        if probe.protocol != PROTOCOL_I2C {
            return Err(Error::NotI2c(probe.protocol));
        }
        if probe.fifo_disabled {
            return Err(Error::FifoDisabled);
        }
        if probe.tx_depth < 2 || probe.rx_depth == 0 {
            return Err(Error::NoFifo);
        }
        Ok(())
    }

    fn prepare(r: &mut impl Regs, probe: &Probe, speed: Speed, clk_sel: u32) -> Result<I2c, Error> {
        stop_left_running(r)?;
        // geni_se_irq_clear
        r.write32(reg::GSI_EVENT_EN, 0);
        for clear in [
            reg::M_IRQ_CLEAR,
            reg::S_IRQ_CLEAR,
            reg::DMA_TX_IRQ_CLR,
            reg::DMA_RX_IRQ_CLR,
        ] {
            r.write32(clear, u32::MAX);
        }
        r.write32(reg::IRQ_EN, u32::MAX);
        // geni_se_io_init
        let cgc = r.read32(reg::CGC_CTRL);
        r.write32(reg::CGC_CTRL, cgc | 0x7f);
        let dma_cfg = r.read32(reg::DMA_GENERAL_CFG);
        r.write32(reg::DMA_GENERAL_CFG, dma_cfg | 0xf);
        r.write32(reg::OUTPUT_CTRL, 0x7f);
        r.write32(reg::FORCE_DEFAULT, 1);
        // geni_se_io_set_mode
        let irq_en = r.read32(reg::IRQ_EN);
        r.write32(reg::IRQ_EN, irq_en | 0xf);
        let dma = r.read32(reg::DMA_MODE_EN);
        r.write32(reg::DMA_MODE_EN, dma & !1);
        r.write32(reg::GSI_EVENT_EN, 0);
        // Watermarks as the I2C probe passes them: RX at depth - 1, RFR at depth.
        r.write32(reg::RX_WATERMARK, (probe.tx_depth - 1) as u32);
        r.write32(reg::RX_RFR_WATERMARK, probe.tx_depth as u32);
        let m_en = r.read32(reg::M_IRQ_EN);
        r.write32(reg::M_IRQ_EN, m_en | irq::COMMON_EN);
        let s_en = r.read32(reg::S_IRQ_EN);
        r.write32(reg::S_IRQ_EN, s_en | S_COMMON_EN);
        // geni_se_config_packing(se, 8, 4, true, true, true)
        r.write32(reg::TX_PACKING_CFG0, PACKING_CFG0);
        r.write32(reg::TX_PACKING_CFG1, PACKING_CFG1);
        r.write32(reg::RX_PACKING_CFG0, PACKING_CFG0);
        r.write32(reg::RX_PACKING_CFG1, PACKING_CFG1);
        r.write32(reg::BYTE_GRAN, 0);
        let timing = if probe.has_timing() {
            Timing::Firmware
        } else {
            let (clk, scl) = speed.registers();
            r.write32(reg::CLK_SEL, clk_sel);
            r.write32(reg::SER_M_CLK_CFG, clk);
            r.write32(reg::I2C_SCL_COUNTERS, scl);
            Timing::Programmed(speed)
        };
        Ok(I2c {
            tx_depth: probe.tx_depth,
            timing,
            bus_clears: 0,
        })
    }

    pub fn timing(&self) -> Timing {
        self.timing
    }

    /// Runs `msgs` to the 7-bit address `addr`, with repeated starts between them and a stop after the last.
    /// Every message goes through the FIFO, whatever its size: Linux switches to SE DMA for messages of 32 bytes
    /// or more (`i2c_get_dma_safe_msg_buf(msg, 32)`); FIFO-only is ours (`docs/research/gbgo-geni-i2c.md`
    /// section 8), and the hardware does FIFO reads longer than the FIFO by watermark refills.
    pub fn transfer(
        &mut self,
        r: &mut impl Regs,
        clock: &mut impl Clock,
        addr: u8,
        msgs: &mut [Msg<'_>],
    ) -> Result<(), Error> {
        let n = msgs.len();
        for (i, msg) in msgs.iter_mut().enumerate() {
            let param =
                if i + 1 < n { STOP_STRETCH } else { 0 } | ((u32::from(addr) << 9) & 0xfe00);
            select_fifo_mode(r);
            match msg {
                Msg::Write(data) => self.write(r, clock, param, data)?,
                Msg::Read(buf) => self.read(r, clock, param, buf)?,
            }
        }
        Ok(())
    }

    /// [`I2c::transfer`], and after a bus error or lost arbitration one bus clear and one more try. Only for a bus with
    /// no other master: lost arbitration is then a slave holding SDA, not someone else's transfer to clock through.
    pub fn transfer_recovering(
        &mut self,
        r: &mut impl Regs,
        clock: &mut impl Clock,
        addr: u8,
        msgs: &mut [Msg<'_>],
    ) -> Result<(), Error> {
        match self.transfer(r, clock, addr, msgs) {
            Err(err @ (Error::BusError | Error::ArbitrationLost)) => {
                if self.clear_bus(r, clock) {
                    self.transfer(r, clock, addr, msgs)
                } else {
                    Err(err)
                }
            }
            other => other,
        }
    }

    /// Runs an I2C bus clear command. Returns whether it finished without an error.
    pub fn clear_bus(&mut self, r: &mut impl Regs, clock: &mut impl Clock) -> bool {
        self.bus_clears = self.bus_clears.saturating_add(1);
        select_fifo_mode(r);
        r.write32(reg::M_CMD0, OPCODE_BUS_CLEAR << 27);
        let deadline = clock.now_us() + BUS_CLEAR_TIMEOUT_US;
        loop {
            let status = r.read32(reg::M_IRQ_STATUS);
            if status & irq::IRQ_ERR != 0 {
                self.recover(r, clock, status);
                return false;
            }
            if status != 0 {
                r.write32(reg::M_IRQ_CLEAR, status);
            }
            if status & irq::CMD_DONE != 0 {
                return true;
            }
            if clock.now_us() > deadline {
                self.give_up(r, clock);
                return false;
            }
        }
    }

    /// How many bus clears this engine has run.
    pub fn bus_clears(&self) -> u32 {
        self.bus_clears
    }

    fn write(
        &mut self,
        r: &mut impl Regs,
        clock: &mut impl Clock,
        param: u32,
        data: &[u8],
    ) -> Result<(), Error> {
        if data.is_empty() || data.len() > MAX_LEN {
            return Err(Error::BadLength);
        }
        r.write32(reg::I2C_TX_TRANS_LEN, data.len() as u32);
        r.write32(reg::M_CMD0, (OPCODE_WRITE << 27) | param);
        r.write32(reg::TX_WATERMARK, 1);
        let deadline = clock.now_us() + XFER_TIMEOUT_US;
        let mut sent = 0;
        loop {
            let status = r.read32(reg::M_IRQ_STATUS);
            if status & irq::IRQ_ERR != 0 {
                return Err(self.recover(r, clock, status));
            }
            if status & irq::TX_FIFO_WATERMARK != 0 && sent < data.len() {
                for _ in 0..self.tx_depth - 1 {
                    let Some(chunk) = data.get(sent..).filter(|c| !c.is_empty()) else {
                        break;
                    };
                    let mut word = [0u8; 4];
                    let n = chunk.len().min(4);
                    word[..n].copy_from_slice(&chunk[..n]);
                    r.write32(reg::TX_FIFO, u32::from_le_bytes(word));
                    sent += n;
                }
                if sent >= data.len() {
                    r.write32(reg::TX_WATERMARK, 0);
                }
            }
            if status != 0 {
                r.write32(reg::M_IRQ_CLEAR, status);
            }
            if status & irq::CMD_DONE != 0 {
                return Ok(());
            }
            if clock.now_us() > deadline {
                return Err(self.give_up(r, clock));
            }
        }
    }

    fn read(
        &mut self,
        r: &mut impl Regs,
        clock: &mut impl Clock,
        param: u32,
        buf: &mut [u8],
    ) -> Result<(), Error> {
        if buf.is_empty() || buf.len() > MAX_LEN {
            return Err(Error::BadLength);
        }
        r.write32(reg::I2C_RX_TRANS_LEN, buf.len() as u32);
        r.write32(reg::M_CMD0, (OPCODE_READ << 27) | param);
        let deadline = clock.now_us() + XFER_TIMEOUT_US;
        let mut got = 0;
        loop {
            let status = r.read32(reg::M_IRQ_STATUS);
            if status & irq::IRQ_ERR != 0 {
                return Err(self.recover(r, clock, status));
            }
            // The RX FIFO is drained only on a watermark or the last word, as `geni_i2c_irq` does, not on
            // CMD_DONE alone.
            if status & (irq::RX_FIFO_WATERMARK | irq::RX_FIFO_LAST) != 0 {
                let words = r.read32(reg::RX_FIFO_STATUS) & 0x1ff_ffff;
                for _ in 0..words {
                    let word = r.read32(reg::RX_FIFO).to_le_bytes();
                    let n = (buf.len() - got).min(4);
                    buf[got..got + n].copy_from_slice(&word[..n]);
                    got += n;
                }
            }
            if status != 0 {
                r.write32(reg::M_IRQ_CLEAR, status);
            }
            if status & irq::CMD_DONE != 0 {
                return if got == buf.len() {
                    Ok(())
                } else {
                    Err(Error::ShortRead { got })
                };
            }
            if clock.now_us() > deadline {
                return Err(self.give_up(r, clock));
            }
        }
    }

    /// Classifies a failed command and stops it: no more TX data, a short wait for it to finish on its own, then
    /// cancel, then abort. The cancel-then-abort is ours, not 7.1.10's (`docs/research/gbgo-geni-i2c.md` sections 2.5
    /// and 8); Linux master's `geni_i2c_err` path is where the cancel comes from.
    fn recover(&mut self, r: &mut impl Regs, clock: &mut impl Clock, status: u32) -> Error {
        let err = if status & irq::GP_IRQ_1 != 0 {
            Error::Nack {
                data: r.read32(reg::M_GP_LENGTH) != 0,
            }
        } else if status & irq::GP_IRQ_3 != 0 {
            Error::BusError
        } else if status & irq::GP_IRQ_4 != 0 {
            Error::ArbitrationLost
        } else {
            Error::CommandFailed(status)
        };
        r.write32(reg::TX_WATERMARK, 0);
        let settled = wait_for(r, clock, irq::CMD_DONE, SETTLE_US) || stop(r, clock);
        drain(r);
        if settled { err } else { Error::Stuck }
    }

    /// A command that never finished: cancel or abort it.
    fn give_up(&mut self, r: &mut impl Regs, clock: &mut impl Clock) -> Error {
        r.write32(reg::TX_WATERMARK, 0);
        let stopped = stop(r, clock);
        drain(r);
        if stopped {
            Error::Timeout
        } else {
            Error::Stuck
        }
    }
}

/// Cancels the command running on the engine, and aborts it if that does not finish.
fn stop(r: &mut impl Regs, clock: &mut impl Clock) -> bool {
    r.write32(reg::M_CMD_CTRL, CMD_CTRL_CANCEL);
    if wait_for(r, clock, irq::CMD_CANCEL, ABORT_TIMEOUT_US) {
        return true;
    }
    r.write32(reg::M_CMD_CTRL, CMD_CTRL_ABORT);
    wait_for(r, clock, irq::CMD_ABORT, ABORT_TIMEOUT_US)
}

/// Stops a command the firmware left running (`SE_GENI_STATUS.M_GENI_CMD_ACTIVE`) and empties what it left. An idle
/// engine is not touched: cancelling nothing may never report back. No clock exists yet here, so timeouts count polls.
fn stop_left_running(r: &mut impl Regs) -> Result<(), Error> {
    if r.read32(reg::STATUS) & 1 == 0 {
        return Ok(());
    }
    // A cancel the firmware's own run left in the status must not pass for this one.
    r.write32(reg::M_IRQ_CLEAR, u32::MAX);
    r.write32(reg::TX_WATERMARK, 0);
    let stopped = stop(r, &mut Polls(0));
    drain(r);
    if stopped && (0..IDLE_POLLS).any(|_| r.read32(reg::STATUS) & 1 == 0) {
        Ok(())
    } else {
        Err(Error::Stuck)
    }
}

/// `geni_se_select_fifo_mode` for a non-UART engine.
fn select_fifo_mode(r: &mut impl Regs) {
    r.write32(reg::GSI_EVENT_EN, 0);
    for clear in [
        reg::M_IRQ_CLEAR,
        reg::S_IRQ_CLEAR,
        reg::DMA_TX_IRQ_CLR,
        reg::DMA_RX_IRQ_CLR,
    ] {
        r.write32(clear, u32::MAX);
    }
    r.write32(reg::IRQ_EN, u32::MAX);
    let en = r.read32(reg::M_IRQ_EN);
    r.write32(reg::M_IRQ_EN, en | irq::FIFO_EN);
    let dma = r.read32(reg::DMA_MODE_EN);
    r.write32(reg::DMA_MODE_EN, dma & !1);
}

/// Polls `M_IRQ_STATUS` for any bit of `mask`, clearing everything it sees.
fn wait_for(r: &mut impl Regs, clock: &mut impl Clock, mask: u32, timeout_us: u64) -> bool {
    let deadline = clock.now_us() + timeout_us;
    loop {
        let status = r.read32(reg::M_IRQ_STATUS);
        if status != 0 {
            r.write32(reg::M_IRQ_CLEAR, status);
        }
        if status & mask != 0 {
            return true;
        }
        if clock.now_us() > deadline {
            return false;
        }
    }
}

/// Discards what a stopped read left in the RX FIFO.
fn drain(r: &mut impl Regs) {
    let words = r.read32(reg::RX_FIFO_STATUS) & 0x1ff_ffff;
    for _ in 0..words {
        r.read32(reg::RX_FIFO);
    }
    r.write32(reg::M_IRQ_CLEAR, u32::MAX);
}

#[cfg(test)]
mod tests;
