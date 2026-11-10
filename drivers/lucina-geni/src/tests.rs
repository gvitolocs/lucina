//! Tests against a model of a GENI serial engine running I2C firmware: it executes M_CMD0 write and read commands
//! against simulated slaves, raises the TX watermark while a write wants data, delivers read data with RX_LAST, and
//! reports NACKs (address or data) as CMD_FAILURE + GP_IRQ_1 without CMD_DONE, as the hardware does.

extern crate std;

use std::{collections::VecDeque, vec, vec::Vec};

use super::*;

/// A register-addressed slave: a write sets the register pointer (first two bytes) and stores the rest; a read
/// returns bytes from the pointer on.
struct Slave {
    addr: u8,
    mem: Vec<u8>,
    ptr: usize,
    /// NACK any data byte at or after this index of a write.
    nack_data_at: Option<usize>,
    written: Vec<Vec<u8>>,
    /// Return this many bytes fewer than asked.
    short_by: usize,
}

impl Slave {
    fn new(addr: u8, mem: Vec<u8>) -> Self {
        Slave {
            addr,
            mem,
            ptr: 0,
            nack_data_at: None,
            written: Vec::new(),
            short_by: 0,
        }
    }
}

#[derive(Clone, Copy)]
enum Cmd {
    Write { addr: u8, len: usize },
    Read,
}

struct Model {
    regs: Vec<u32>,
    slaves: Vec<Slave>,
    cmd: Option<Cmd>,
    tx: Vec<u8>,
    rx: VecDeque<u32>,
    status: u32,
    tx_wm: bool,
    cmds: Vec<u32>,
    writes: Vec<(usize, u32)>,
    /// Commands never finish.
    stuck: bool,
    /// These error bits come with every command until a bus clear, which needs `bus_clear_works`.
    bus_fault: u32,
    bus_clear_works: bool,
    /// A bus clear finishes but changes nothing.
    fault_survives_clear: bool,
    ignore_cancel: bool,
    ignore_abort: bool,
    max_tx_words_per_round: usize,
    tx_words_this_round: usize,
    /// When set, a read command reports this status instead of RX_FIFO_LAST | CMD_DONE.
    read_status: Option<u32>,
}

const DEPTH: u32 = 16;

impl Model {
    fn new(slaves: Vec<Slave>) -> Self {
        let mut m = Model {
            regs: vec![0; 0x4000 / 4],
            slaves,
            cmd: None,
            tx: Vec::new(),
            rx: VecDeque::new(),
            status: 0,
            tx_wm: false,
            cmds: Vec::new(),
            writes: Vec::new(),
            stuck: false,
            bus_fault: 0,
            bus_clear_works: true,
            fault_survives_clear: false,
            ignore_cancel: false,
            ignore_abort: false,
            max_tx_words_per_round: 0,
            tx_words_this_round: 0,
            read_status: None,
        };
        m.regs[reg::FW_REVISION_RO / 4] = (u32::from(PROTOCOL_I2C) << 8) | 0x41;
        m.regs[reg::HW_PARAM_0 / 4] = (32 << 24) | (DEPTH << 16);
        m.regs[reg::HW_PARAM_1 / 4] = (32 << 24) | (DEPTH << 16);
        m
    }

    fn slave(&mut self, addr: u8) -> Option<&mut Slave> {
        self.slaves.iter_mut().find(|s| s.addr == addr)
    }

    fn nack(&mut self, data: bool) {
        self.status |= irq::CMD_FAILURE | irq::GP_IRQ_1;
        self.regs[reg::M_GP_LENGTH / 4] = u32::from(data);
        self.tx_wm = false;
    }

    fn start(&mut self, cmd0: u32) {
        self.cmds.push(cmd0);
        let op = cmd0 >> 27;
        let addr = ((cmd0 >> 9) & 0x7f) as u8;
        if op == OPCODE_BUS_CLEAR {
            if self.bus_clear_works {
                if !self.fault_survives_clear {
                    self.bus_fault = 0;
                }
                self.status |= irq::CMD_DONE;
            } else {
                self.status |= irq::CMD_FAILURE | irq::GP_IRQ_3;
            }
            return;
        }
        if self.bus_fault != 0 {
            self.cmd = Some(Cmd::Read);
            self.status |= irq::CMD_FAILURE | self.bus_fault;
            return;
        }
        if self.stuck {
            self.cmd = Some(Cmd::Read);
            return;
        }
        if self.slave(addr).is_none() {
            return self.nack(false);
        }
        match op {
            OPCODE_WRITE => {
                let len = self.regs[reg::I2C_TX_TRANS_LEN / 4] as usize;
                self.cmd = Some(Cmd::Write { addr, len });
                self.tx.clear();
            }
            OPCODE_READ => {
                let len = self.regs[reg::I2C_RX_TRANS_LEN / 4] as usize;
                let s = self.slave(addr).unwrap();
                let n = len - s.short_by.min(len);
                let data: Vec<u8> = (0..n)
                    .map(|i| s.mem.get(s.ptr + i).copied().unwrap_or(0xff))
                    .collect();
                s.ptr += n;
                for chunk in data.chunks(4) {
                    let mut w = [0u8; 4];
                    w[..chunk.len()].copy_from_slice(chunk);
                    self.rx.push_back(u32::from_le_bytes(w));
                }
                self.status |= self.read_status.unwrap_or(irq::RX_FIFO_LAST | irq::CMD_DONE);
                self.cmd = None;
            }
            _ => self.status |= irq::ILLEGAL_CMD,
        }
    }

    fn push_tx(&mut self, word: u32) {
        let Some(Cmd::Write { addr, len }) = self.cmd else {
            panic!("TX FIFO written without a write command");
        };
        self.tx_words_this_round += 1;
        self.max_tx_words_per_round = self.max_tx_words_per_round.max(self.tx_words_this_round);
        assert!(
            self.tx_words_this_round < DEPTH as usize,
            "TX FIFO overfilled"
        );
        let take = (len - self.tx.len()).min(4);
        self.tx.extend_from_slice(&word.to_le_bytes()[..take]);
        if self.tx.len() == len {
            let data = core::mem::take(&mut self.tx);
            let s = self.slave(addr).unwrap();
            if let Some(at) = s.nack_data_at.filter(|&at| at < data.len()) {
                let _ = at;
                self.cmd = None;
                return self.nack(true);
            }
            if data.len() >= 2 {
                s.ptr = usize::from(u16::from_le_bytes([data[0], data[1]]));
            }
            s.written.push(data);
            self.cmd = None;
            self.status |= irq::CMD_DONE;
        }
    }
}

impl Regs for Model {
    fn read32(&mut self, offset: usize) -> u32 {
        match offset {
            reg::M_IRQ_STATUS => {
                let wants = matches!(self.cmd, Some(Cmd::Write { .. })) && self.tx_wm;
                if wants {
                    self.tx_words_this_round = 0;
                }
                self.status | if wants { irq::TX_FIFO_WATERMARK } else { 0 }
            }
            reg::RX_FIFO_STATUS => self.rx.len() as u32,
            reg::STATUS => u32::from(self.cmd.is_some()) | self.regs[reg::STATUS / 4],

            reg::RX_FIFO => self.rx.pop_front().expect("RX FIFO read while empty"),
            _ => self.regs[offset / 4],
        }
    }

    fn write32(&mut self, offset: usize, value: u32) {
        self.writes.push((offset, value));
        match offset {
            reg::M_CMD0 => self.start(value),
            reg::TX_WATERMARK => self.tx_wm = value != 0,
            reg::TX_FIFO => self.push_tx(value),
            reg::M_IRQ_CLEAR => self.status &= !value,
            reg::M_CMD_CTRL if value == CMD_CTRL_CANCEL => {
                if !self.ignore_cancel {
                    self.cmd = None;
                    self.status |= irq::CMD_CANCEL;
                }
            }
            reg::M_CMD_CTRL if value == CMD_CTRL_ABORT => {
                if !self.ignore_abort {
                    self.cmd = None;
                    self.status |= irq::CMD_ABORT;
                }
            }
            _ => self.regs[offset / 4] = value,
        }
    }
}

struct Ticks(u64);

impl Clock for Ticks {
    fn now_us(&mut self) -> u64 {
        self.0 += 100;
        self.0
    }
}

/// A HID-over-I2C keyboard's descriptor at register 0x20.
fn keyboard() -> Slave {
    let mut mem = vec![0u8; 0x100];
    let desc: [u8; 30] = [
        0x1e, 0x00, 0x00, 0x01, 0x40, 0x00, 0x21, 0x00, 0x24, 0x00, 0x10, 0x00, 0x25, 0x00, 0x00,
        0x00, 0x22, 0x00, 0x23, 0x00, 0xf2, 0x0c, 0x28, 0x90, 0x00, 0x01, 0, 0, 0, 0,
    ];
    mem[0x20..0x20 + 30].copy_from_slice(&desc);
    Slave::new(0x05, mem)
}

fn ready(m: &mut Model) -> I2c {
    let probe = Probe::read(m, 0x2000_0000);
    I2c::new(m, &probe, Speed::Fast).unwrap()
}

#[test]
fn probe_reads_the_firmware_state() {
    let mut m = Model::new(vec![]);
    m.regs[reg::SER_M_CLK_CFG / 4] = 0x21;
    m.regs[reg::I2C_SCL_COUNTERS / 4] = 0x0050_3018;
    m.regs[reg::IF_DISABLE_RO / 4] = 0;
    let p = Probe::read(&mut m, 0x2000_0000);
    assert_eq!(
        (
            p.protocol,
            p.fw_version,
            p.tx_depth,
            p.rx_depth,
            p.fifo_width
        ),
        (3, 0x41, 16, 16, 32)
    );
    assert!(p.has_timing() && !p.fifo_disabled && !p.busy);
    assert!(m.writes.is_empty(), "probe writes nothing");
    // QUP 3.10 and later have 8-bit depth fields.
    m.regs[reg::HW_PARAM_0 / 4] = 0x80 << 16;
    assert_eq!(Probe::read(&mut m, 0x2000_0000).tx_depth, 0);
    assert_eq!(Probe::read(&mut m, 0x300a_0000).tx_depth, 0x80);
}

#[test]
fn new_refuses_other_protocols_and_gpi_only_engines() {
    let mut m = Model::new(vec![]);
    m.regs[reg::FW_REVISION_RO / 4] = 2 << 8;
    let p = Probe::read(&mut m, 0);
    assert_eq!(
        I2c::new(&mut m, &p, Speed::Fast).err(),
        Some(Error::NotI2c(2))
    );
    let mut m = Model::new(vec![]);
    m.regs[reg::IF_DISABLE_RO / 4] = 1;
    let p = Probe::read(&mut m, 0);
    assert_eq!(
        I2c::new(&mut m, &p, Speed::Fast).err(),
        Some(Error::FifoDisabled)
    );
}

#[test]
fn firmware_timing_is_kept_else_400k_is_programmed() {
    let mut m = Model::new(vec![]);
    m.regs[reg::SER_M_CLK_CFG / 4] = 0x71;
    m.regs[reg::I2C_SCL_COUNTERS / 4] = 0x00a0_2c1a;
    let i2c = ready(&mut m);
    assert_eq!(i2c.timing(), Timing::Firmware);
    assert!(
        !m.writes
            .iter()
            .any(|w| w.0 == reg::SER_M_CLK_CFG || w.0 == reg::I2C_SCL_COUNTERS)
    );

    let mut m = Model::new(vec![]);
    let i2c = ready(&mut m);
    assert_eq!(i2c.timing(), Timing::Programmed(Speed::Fast));
    assert_eq!(m.regs[reg::SER_M_CLK_CFG / 4], 0x21);
    assert_eq!(m.regs[reg::I2C_SCL_COUNTERS / 4], 0x0050_2c16);
    assert_eq!(m.regs[reg::TX_PACKING_CFG0 / 4], 0x0007_f8fe);
    assert_eq!(m.regs[reg::RX_PACKING_CFG1 / 4], 0x000f_fefe);
    assert_eq!(m.regs[reg::RX_WATERMARK / 4], 15);
}

#[test]
fn the_fallback_timing_words_are_linux_7_1_10_s() {
    // `geni_i2c_clk_map_19p2mhz`: div, t_high, t_low, t_cycle packed as CLK_CFG = div << 4 | 1 and
    // SCL_COUNTERS = high << 20 | low << 10 | cycle.
    assert_eq!(Speed::Standard.registers(), (0x71, 0x00a0_301a));
    assert_eq!(Speed::Fast.registers(), (0x21, 0x0050_2c16));
    assert_eq!(Speed::FastPlus.registers(), (0x11, 0x0020_2012));
}

#[test]
fn the_rx_fifo_is_drained_only_on_a_watermark_or_the_last_word() {
    // CMD_DONE alone, with data still in the FIFO: not drained, as `geni_i2c_irq` drains only on
    // M_RX_FIFO_WATERMARK | M_RX_FIFO_LAST.
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    m.read_status = Some(irq::CMD_DONE);
    let mut buf = [0u8; 4];
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::ShortRead { got: 0 })
    );
    assert!(!m.rx.is_empty(), "CMD_DONE alone does not drain the RX FIFO");
}

#[test]
fn the_error_bits_are_the_ones_geni_i2c_irq_tests() {
    for extra in [irq::CMD_ABORT, irq::GP_IRQ_0] {
        let mut m = Model::new(vec![keyboard()]);
        let mut i2c = ready(&mut m);
        m.read_status = Some(extra);
        let mut buf = [0u8; 4];
        assert_eq!(
            i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
            Err(Error::CommandFailed(extra)),
            "bit {extra:#x} is an error, not a timeout"
        );
    }
}

#[test]
fn hid_descriptor_read_is_a_write_then_a_read_with_repeated_start() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    let mut desc = [0u8; 30];
    i2c.transfer(
        &mut m,
        &mut Ticks(0),
        0x05,
        &mut [Msg::Write(&[0x20, 0x00]), Msg::Read(&mut desc)],
    )
    .unwrap();
    assert_eq!(&desc[..4], &[0x1e, 0x00, 0x00, 0x01]);
    assert_eq!(&desc[20..24], &[0xf2, 0x0c, 0x28, 0x90]);
    // Write with STOP_STRETCH (repeated start follows), then a plain read; address 5 in bits 15:9.
    assert_eq!(m.cmds, [0x0800_0a04, 0x1000_0a00]);
}

#[test]
fn long_writes_refill_the_fifo_on_each_watermark() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    let data: Vec<u8> = (0..130u8).collect();
    i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Write(&data)])
        .unwrap();
    assert_eq!(m.slaves[0].written.last().unwrap(), &data);
    assert_eq!(
        m.max_tx_words_per_round, 15,
        "at most depth - 1 words per watermark"
    );
    assert!(m.writes.contains(&(reg::TX_WATERMARK, 0)));
}

#[test]
fn nacks_are_told_apart_and_the_command_is_cancelled() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    let mut buf = [0u8; 2];
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x06, &mut [Msg::Read(&mut buf)]),
        Err(Error::Nack { data: false })
    );
    assert!(m.writes.contains(&(reg::M_CMD_CTRL, CMD_CTRL_CANCEL)));
    m.slaves[0].nack_data_at = Some(1);
    assert_eq!(
        i2c.transfer(
            &mut m,
            &mut Ticks(0),
            0x05,
            &mut [Msg::Write(&[0x22, 0x00, 0x00, 0x08])]
        ),
        Err(Error::Nack { data: true })
    );
    // The engine still works afterwards.
    m.slaves[0].nack_data_at = None;
    let mut desc = [0u8; 30];
    i2c.transfer(
        &mut m,
        &mut Ticks(0),
        0x05,
        &mut [Msg::Write(&[0x20, 0x00]), Msg::Read(&mut desc)],
    )
    .unwrap();
    assert_eq!(desc[0], 0x1e);
}

#[test]
fn a_command_that_never_finishes_is_cancelled_then_aborted() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    m.stuck = true;
    let mut buf = [0u8; 4];
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::Timeout)
    );
    m.ignore_cancel = true;
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::Timeout)
    );
    assert!(m.writes.contains(&(reg::M_CMD_CTRL, CMD_CTRL_ABORT)));
    m.ignore_abort = true;
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::Stuck)
    );
}

#[test]
fn short_reads_and_bad_lengths_are_errors() {
    let mut slave = keyboard();
    slave.short_by = 3;
    let mut m = Model::new(vec![slave]);
    let mut i2c = ready(&mut m);
    let mut buf = [0u8; 10];
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::ShortRead { got: 8 })
    );
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Write(&[])]),
        Err(Error::BadLength)
    );
}

#[test]
fn a_command_left_running_is_stopped_and_its_data_drained() {
    let mut m = Model::new(vec![keyboard()]);
    m.stuck = true;
    m.cmd = Some(Cmd::Read);
    m.rx.extend([1, 2, 3]);
    m.ignore_cancel = true; // Only the abort finishes it.
    m.status = irq::RX_FIFO_WATERMARK;
    let p = Probe::read(&mut m, 0x2000_0000);
    assert!(p.busy);
    I2c::new(&mut m, &p, Speed::Fast).unwrap();
    assert!(m.writes.contains(&(reg::M_CMD_CTRL, CMD_CTRL_CANCEL)));
    assert!(m.writes.contains(&(reg::M_CMD_CTRL, CMD_CTRL_ABORT)));
    assert!(m.rx.is_empty(), "the RX FIFO is drained");
    let stopped = m.writes.iter().position(|w| w.0 == reg::M_CMD_CTRL);
    let init = m.writes.iter().position(|w| w.0 == reg::FORCE_DEFAULT);
    assert!(stopped < init, "stopped before the engine is set up");
    assert_eq!(m.status, 0, "no status left from it");

    // The engine works afterwards.
    let mut m = Model::new(vec![keyboard()]);
    m.cmd = Some(Cmd::Read);
    let p = Probe::read(&mut m, 0x2000_0000);
    let mut i2c = I2c::new(&mut m, &p, Speed::Fast).unwrap();
    let mut desc = [0u8; 30];
    i2c.transfer(
        &mut m,
        &mut Ticks(0),
        0x05,
        &mut [Msg::Write(&[0x20, 0x00]), Msg::Read(&mut desc)],
    )
    .unwrap();
    assert_eq!(desc[0], 0x1e);
}

#[test]
fn an_engine_that_cannot_be_stopped_is_refused_untouched() {
    let mut m = Model::new(vec![]);
    m.cmd = Some(Cmd::Read);
    m.ignore_cancel = true;
    m.ignore_abort = true;
    // A cancel the firmware's own run left in the status does not count.
    m.status = irq::CMD_CANCEL | irq::CMD_ABORT;
    let p = Probe::read(&mut m, 0x2000_0000);
    assert_eq!(I2c::new(&mut m, &p, Speed::Fast).err(), Some(Error::Stuck));
    assert!(!m.writes.iter().any(|w| w.0 == reg::FORCE_DEFAULT));
}

#[test]
fn an_idle_engine_gets_no_cancel_even_if_the_probe_saw_it_busy() {
    let mut m = Model::new(vec![]);
    m.cmd = Some(Cmd::Read);
    let p = Probe::read(&mut m, 0x2000_0000);
    m.cmd = None;
    m.writes.clear();
    I2c::new(&mut m, &p, Speed::Fast).unwrap();
    assert!(!m.writes.iter().any(|w| w.0 == reg::M_CMD_CTRL));
}

#[test]
fn bus_errors_get_one_bus_clear_and_one_more_try() {
    for fault in [irq::GP_IRQ_3, irq::GP_IRQ_4] {
        let mut m = Model::new(vec![keyboard()]);
        let mut i2c = ready(&mut m);
        m.bus_fault = fault;
        let mut desc = [0u8; 30];
        i2c.transfer_recovering(
            &mut m,
            &mut Ticks(0),
            0x05,
            &mut [Msg::Write(&[0x20, 0x00]), Msg::Read(&mut desc)],
        )
        .unwrap();
        assert_eq!(desc[0], 0x1e);
        assert_eq!(i2c.bus_clears(), 1);
        assert_eq!(m.cmds.iter().filter(|c| **c == 6 << 27).count(), 1);
    }
}

#[test]
fn a_bus_that_stays_bad_gets_only_one_clear_and_keeps_its_error() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    let mut buf = [0u8; 2];
    m.bus_fault = irq::GP_IRQ_4;
    m.bus_clear_works = false;
    assert_eq!(
        i2c.transfer_recovering(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::ArbitrationLost)
    );
    assert_eq!(m.cmds.iter().filter(|c| **c == 6 << 27).count(), 1);
    // A clear that goes through but does not help: the second try's error stands, no second clear.
    m.bus_clear_works = true;
    m.fault_survives_clear = true;
    m.cmds.clear();
    assert_eq!(
        i2c.transfer_recovering(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::ArbitrationLost)
    );
    assert_eq!(m.cmds.len(), 3, "the read, the clear, the read again");
    assert_eq!(m.cmds[1], 6 << 27);
}

#[test]
fn nacks_and_plain_transfers_clear_nothing() {
    let mut m = Model::new(vec![keyboard()]);
    let mut i2c = ready(&mut m);
    let mut buf = [0u8; 2];
    assert_eq!(
        i2c.transfer_recovering(&mut m, &mut Ticks(0), 0x06, &mut [Msg::Read(&mut buf)]),
        Err(Error::Nack { data: false })
    );
    // transfer() stays as it was for a bus shared with another master.
    m.bus_fault = irq::GP_IRQ_4;
    assert_eq!(
        i2c.transfer(&mut m, &mut Ticks(0), 0x05, &mut [Msg::Read(&mut buf)]),
        Err(Error::ArbitrationLost)
    );
    assert!(!m.cmds.iter().any(|c| *c >> 27 == OPCODE_BUS_CLEAR));
    assert_eq!(i2c.bus_clears(), 0);
}

/// A GCC whose S4 branch runs only once voted on with a started root, and whose `UPDATE` bit clears unless told not to.
struct Gcc {
    regs: Vec<u32>,
    update_sticks: bool,
    needs_root: bool,
    writes: Vec<(usize, u32)>,
}

const S4_CBCR: usize = 0x184d4;
const S4_RCG: usize = 0x184d8;
const VOTE: usize = 0x52008;
const S4_VOTE: u32 = 1 << 26;

impl Gcc {
    fn new() -> Self {
        Gcc {
            regs: vec![0; 0x60000 / 4],
            update_sticks: false,
            needs_root: true,
            writes: Vec::new(),
        }
    }

    fn set(&mut self, offset: usize, value: u32) {
        self.regs[offset / 4] = value;
    }

    fn get(&self, offset: usize) -> u32 {
        self.regs[offset / 4]
    }

    fn branch_on(&self) -> bool {
        self.get(VOTE) & S4_VOTE != 0 && (!self.needs_root || self.get(S4_RCG) & 2 != 0)
    }
}

impl Regs for Gcc {
    fn read32(&mut self, offset: usize) -> u32 {
        if offset == S4_CBCR {
            return if self.branch_on() { 0 } else { 1 << 31 };
        }
        self.regs[offset / 4]
    }

    fn write32(&mut self, offset: usize, value: u32) {
        self.writes.push((offset, value));
        let value = if offset == S4_RCG && !self.update_sticks {
            value & !1
        } else {
            value
        };
        self.regs[offset / 4] = value;
    }
}

/// A DFS table as firmware programs one from `ftbl_gcc_qupv3_wrap0_s0_clk_src`: 7.3728, 14.7456, 19.2, 32, 48, 51.2,
/// 64 and 96 MHz.
fn dfs_table(g: &mut Gcc, base: usize) {
    const EVEN: u32 = 6 << 8;
    const GPLL6: u32 = 2 << 8;
    const MND: u32 = 2 << 12;
    let levels: [(u32, u32, u32); 8] = [
        (EVEN | MND | 1, 384, 15625),
        (EVEN | MND | 1, 768, 15625),
        (1, 0, 0),
        (EVEN | MND | 1, 8, 75),
        (EVEN | MND | 1, 4, 25),
        (GPLL6 | 14, 0, 0),
        (EVEN | MND | 1, 16, 75),
        (EVEN | MND | 1, 8, 25),
    ];
    for (l, &(cfg, m, n)) in levels.iter().enumerate() {
        g.set(base + 0x1c + 4 * l, cfg);
        g.set(base + 0x5c + 4 * l, m);
        // Hardware holds ~(N - M).
        g.set(base + 0x9c + 4 * l, !(n.wrapping_sub(m)));
    }
}

#[test]
fn a_stopped_root_starts_on_tcxo_and_then_the_vote_takes() {
    let mut g = Gcc::new();
    // The firmware left the field values of another rate and a bit outside what the configuration owns.
    g.set(S4_RCG + 4, (6 << 8) | (2 << 12) | 5 | (1 << 20) | (1 << 16));
    g.set(VOTE, S4_VOTE);
    assert_ne!(g.read32(S4_CBCR), 0, "a vote alone leaves it off");
    assert!(gcc::start_root(&mut g, S4_RCG));
    assert_eq!(
        g.get(S4_RCG + 4),
        1 | (1 << 16),
        "source 0, divider 1, MN off"
    );
    assert_eq!(g.read32(S4_CBCR), 0, "running once voted again");
    // CFG first, then the update that makes it take.
    let cfg = g.writes.iter().position(|w| w.0 == S4_RCG + 4);
    let cmd = g.writes.iter().position(|w| w.0 == S4_RCG);
    assert!(cfg < cmd);
    assert!(g.writes.contains(&(S4_RCG, 3)), "UPDATE and ROOT_EN");
    assert_eq!(
        gcc::RcgState::read(&mut g, S4_RCG).rate_hz(0),
        Some(gcc::TCXO_HZ)
    );
}

#[test]
fn a_root_whose_update_never_finishes_is_reported() {
    let mut g = Gcc::new();
    g.update_sticks = true;
    assert!(!gcc::start_root(&mut g, S4_RCG));
}

#[test]
fn a_dfs_root_keeps_its_configuration_when_started() {
    let mut g = Gcc::new();
    g.set(S4_RCG + 0x14, 1);
    g.set(S4_RCG + 4, 0x605);
    assert!(gcc::start_root(&mut g, S4_RCG));
    assert_eq!(g.get(S4_RCG + 4), 0x605);
    assert!(!g.writes.iter().any(|w| w.0 == S4_RCG + 4));
    assert!(g.writes.contains(&(S4_RCG, 3)));
}

#[test]
fn rates_are_read_as_clk_rcg2_computes_them() {
    let mut g = Gcc::new();
    let rcg = 0x18018;
    // CFG, DFS off: source 6 (GPLL0 even), divider 4 (field 7), no MN: 75 MHz.
    g.set(rcg + 4, (6 << 8) | 7);
    assert_eq!(
        gcc::RcgState::read(&mut g, rcg).rate_hz(5),
        Some(75_000_000)
    );
    // An unknown source.
    g.set(rcg + 4, 3 << 8);
    assert_eq!(gcc::RcgState::read(&mut g, rcg).rate_hz(0), None);
    // The whole table, by CLK_SEL.
    g.set(rcg + 0x14, 1 | (2 << 1));
    dfs_table(&mut g, rcg);
    let s = gcc::RcgState::read(&mut g, rcg);
    assert!(s.dfs);
    assert_eq!(s.dfs_level, 2);
    assert_eq!(
        s.levels.map(Option::unwrap),
        [
            7_372_800, 14_745_600, 19_200_000, 32_000_000, 48_000_000, 51_200_000, 64_000_000,
            96_000_000
        ]
    );
    assert_eq!(s.rate_hz(0), Some(7_372_800));
    assert_eq!(s.rate_hz(8), None);
    assert_eq!(s.clk_sel_for(gcc::TCXO_HZ), Some(2));
}

#[test]
fn without_firmware_timing_a_dfs_engine_gets_the_19_2_mhz_level() {
    let mut g = Gcc::new();
    g.set(0x18018 + 0x14, 1);
    dfs_table(&mut g, 0x18018);
    let source = gcc::RcgState::read(&mut g, 0x18018);
    let mut m = Model::new(vec![]);
    let p = Probe::read(&mut m, 0x2000_0000);
    let i2c = I2c::with_source(&mut m, &p, Speed::Fast, Some(&source)).unwrap();
    assert_eq!(i2c.timing(), Timing::Programmed(Speed::Fast));
    assert_eq!(m.regs[reg::CLK_SEL / 4], 2, "not level 0's 7.3728 MHz");
    assert_eq!(m.regs[reg::SER_M_CLK_CFG / 4], 0x21);
}

#[test]
fn without_firmware_timing_a_clock_of_another_or_unknown_rate_is_refused_untouched() {
    let mut g = Gcc::new();
    g.set(0x18018 + 4, (6 << 8) | 7); // 75 MHz
    let other = gcc::RcgState::read(&mut g, 0x18018);
    g.set(0x18018 + 4, 3 << 8);
    let unknown = gcc::RcgState::read(&mut g, 0x18018);
    for (source, hz) in [
        (Some(&other), Some(75_000_000)),
        (Some(&unknown), None),
        (None, None),
    ] {
        let mut m = Model::new(vec![]);
        let p = Probe::read(&mut m, 0x2000_0000);
        assert_eq!(
            I2c::with_source(&mut m, &p, Speed::Fast, source).err(),
            Some(Error::SourceClock { hz })
        );
        assert!(m.writes.is_empty(), "nothing written");
    }
    // A DFS table with no 19.2 MHz level.
    let mut g = Gcc::new();
    g.set(0x18018 + 0x14, 1);
    dfs_table(&mut g, 0x18018);
    g.set(0x18018 + 0x1c + 8, (6 << 8) | 7);
    let no_tcxo = gcc::RcgState::read(&mut g, 0x18018);
    let mut m = Model::new(vec![]);
    let p = Probe::read(&mut m, 0x2000_0000);
    assert_eq!(
        I2c::with_source(&mut m, &p, Speed::Fast, Some(&no_tcxo)).err(),
        Some(Error::SourceClock {
            hz: Some(7_372_800)
        })
    );
}

#[test]
fn firmware_timing_needs_no_known_source() {
    let mut m = Model::new(vec![]);
    m.regs[reg::SER_M_CLK_CFG / 4] = 0x21;
    m.regs[reg::I2C_SCL_COUNTERS / 4] = 0x0050_3018;
    let p = Probe::read(&mut m, 0x2000_0000);
    let i2c = I2c::with_source(&mut m, &p, Speed::Fast, None).unwrap();
    assert_eq!(i2c.timing(), Timing::Firmware);
    assert_eq!(p.scl_hz(gcc::TCXO_HZ), Some(400_000));
    assert_eq!(p.scl_hz(gcc::TCXO_HZ / 2), Some(200_000));
    assert!(!m.writes.iter().any(|w| w.0 == reg::CLK_SEL));
}

#[test]
fn pad_states_are_the_device_trees() {
    use pad::{Bias, GpioInput};
    // hid-keyboard-default-state on gpio104, from a pad the firmware left muxed elsewhere with its driver on.
    let keyboard = GpioInput {
        bias: Some(Bias::PullUp),
        drive_ma: Some(2),
    };
    assert_eq!(keyboard.ctl(0), 0x3);
    assert_eq!(keyboard.ctl(0x1c4 | (1 << 9)), 0x3);
    assert_eq!(keyboard.ctl(0x3), 0x3, "already right");
    // Unrelated bits survive a full state, as `msm_pinmux_set_mux` / `msm_config_group_set` keep them.
    assert_eq!(keyboard.ctl(0x400 | 0x1c4 | (1 << 9)), 0x400 | 0x3);
    // hid-touchpad-default-state on gpio31: no pull.
    let touchpad = GpioInput {
        bias: Some(Bias::Disable),
        drive_ma: Some(2),
    };
    assert_eq!(touchpad.ctl(0x3), 0x0);
    // What the state does not say, and bits that are not its, stay.
    let unsaid = GpioInput::default();
    assert_eq!(unsaid.ctl(0x400 | 0xc0 | 3 | (2 << 2)), 0x400 | 0xc0 | 3);
    let down = GpioInput {
        bias: Some(Bias::PullDown),
        drive_ma: Some(16),
    };
    assert_eq!(down.ctl(0), 0x1c1);
    let odd = GpioInput {
        bias: None,
        drive_ma: Some(3),
    };
    assert_eq!(odd.ctl(0x40), 0x40);
}

#[test]
fn a_counter_frequency_of_zero_does_not_make_timeouts_expire_at_once() {
    assert_eq!(ticks_to_us(19_200_000, 19_200_000), 1_000_000);
    assert_eq!(ticks_to_us(1000, 1_000_000), 1000);
    assert_eq!(ticks_to_us(19_200_000, 0), 1_000_000);
    // No overflow on a counter that has run for centuries.
    assert_eq!(
        ticks_to_us(u64::MAX, u64::from(u32::MAX)),
        (u128::from(u64::MAX) * 1_000_000 / u128::from(u32::MAX)) as u64
    );
}

/// Registers that do as they are told; `CBCR` follows a vote if asked to.
struct Branches {
    cbcr: u32,
    vote: u32,
    on_after_vote: Option<u32>,
    writes: u32,
}

impl Regs for Branches {
    fn read32(&mut self, offset: usize) -> u32 {
        if offset == 0 { self.cbcr } else { self.vote }
    }

    fn write32(&mut self, _offset: usize, value: u32) {
        self.writes += 1;
        self.vote = value;
        if let Some(cbcr) = self.on_after_vote {
            self.cbcr = cbcr;
        }
    }
}

const S_AHB: gcc::Branch = gcc::Branch {
    cbcr: 0,
    vote: 4,
    vote_bit: 21,
    halt: gcc::Halt::Voted,
    hwcg_bit: Some(1),
};

fn branch(cbcr: u32, vote: u32, on_after_vote: Option<u32>) -> Branches {
    Branches {
        cbcr,
        vote,
        on_after_vote,
        writes: 0,
    }
}

#[test]
fn a_branch_in_hardware_gated_mode_reading_off_is_not_refused() {
    // What the laptop's QUP 1 S-AHB could read: CLK_OFF with HWCG on, already voted. Nothing to do.
    let mut r = branch((1 << 31) | 2, 1 << 21, None);
    assert_eq!(
        gcc::enable_branch(&mut r, &S_AHB),
        (gcc::BranchState::NotCheckable, (1 << 31) | 2, 1 << 21)
    );
    assert_eq!(r.writes, 0);
    // Not voted yet: voted, as Linux always does, and still not looked at.
    let mut r = branch((1 << 31) | 2, 0, None);
    let (state, _, vote) = gcc::enable_branch(&mut r, &S_AHB);
    assert_eq!((state, vote), (gcc::BranchState::NotCheckable, 1 << 21));
    assert!(state.is_on());
}

#[test]
fn the_same_branch_with_hardware_gating_off_really_is_off() {
    let mut r = branch(1 << 31, 1 << 21, None);
    let (state, cbcr, vote) = gcc::enable_branch(&mut r, &S_AHB);
    assert_eq!(state, gcc::BranchState::StaysOff);
    assert!(!state.is_on());
    assert_eq!(
        (cbcr, vote),
        (1 << 31, 1 << 21),
        "the raw values are reported"
    );
}

#[test]
fn a_branch_without_hwcg_is_polled_whatever_its_bit_1_says() {
    let mut no_hwcg = S_AHB;
    no_hwcg.hwcg_bit = None;
    let mut r = branch((1 << 31) | 2, 0, None);
    assert_eq!(
        gcc::enable_branch(&mut r, &no_hwcg).0,
        gcc::BranchState::StaysOff
    );
    let mut r = branch(1 << 31, 0, Some(0));
    assert_eq!(
        gcc::enable_branch(&mut r, &no_hwcg).0,
        gcc::BranchState::VotedOn
    );
}

#[test]
fn clk_off_clear_or_the_noc_fsm_on_mean_running_and_the_vote_is_still_set() {
    for cbcr in [0, 2, (1 << 31) | (2 << 28)] {
        // Linux's `clk_enable_regmap` sets the vote bit even when the CBCR already reads running.
        let mut r = branch(cbcr, 0, None);
        assert_eq!(
            gcc::enable_branch(&mut r, &S_AHB),
            (gcc::BranchState::Running, cbcr, 1 << 21)
        );
        assert_eq!(r.writes, 1, "the vote bit was missing and is set");
        // Already voted: no write at all.
        let mut r = branch(cbcr, 1 << 21, None);
        assert_eq!(
            gcc::enable_branch(&mut r, &S_AHB).0,
            gcc::BranchState::Running
        );
        assert_eq!(r.writes, 0, "the vote bit is already set, nothing is rewritten");
    }
    // After a vote the FSM counts too.
    let mut no_hwcg = S_AHB;
    no_hwcg.hwcg_bit = None;
    let mut r = branch(1 << 31, 0, Some((1 << 31) | (2 << 28)));
    assert_eq!(
        gcc::enable_branch(&mut r, &no_hwcg).0,
        gcc::BranchState::VotedOn
    );
}

#[test]
fn halt_skip_does_not_look_at_the_branch() {
    let mut skip = S_AHB;
    skip.halt = gcc::Halt::Skip;
    skip.hwcg_bit = None;
    let mut r = branch(1 << 31, 0, None);
    assert_eq!(
        gcc::enable_branch(&mut r, &skip).0,
        gcc::BranchState::NotCheckable
    );
    assert_eq!(r.vote, 1 << 21);
}

#[test]
fn a_failed_read_never_looks_like_a_gated_or_running_clock() {
    let mut r = branch(u32::MAX, u32::MAX, None);
    assert_eq!(
        gcc::enable_branch(&mut r, &S_AHB).0,
        gcc::BranchState::StaysOff
    );
}
