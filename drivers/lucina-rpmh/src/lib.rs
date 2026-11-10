//! Votes for Qualcomm RPMh resources, enough to turn on a regulator: look its address up in the command database
//! (cmd-db), send one active-only write through the application processor's RSC, wait for the acknowledgement.
//!
//! Behaviour from Linux `drivers/soc/qcom/{cmd-db,rpmh-rsc}.c` and `drivers/regulator/qcom-rpmh-regulator.c`; the
//! register layouts and the polled completion were checked against Scarlet's `drivers/soc/qcom-{cmd-db,rpmh-rsc}`
//! (petitstrawberry/scarlet-project-chromebook, GPL-2.0-only, Copyright its authors), which runs on a real SC7180. The
//! code is written for this crate. See `docs/research/gbgo-sd.md`.
//!
//! Only the register layout of versions below 3 (the SC7180 has one) is implemented; [`Rsc::new`] refuses anything else. Sleep and wake sets
//! are not touched, so a vote holds while the CPU subsystem is awake, which is all LucinaOS does on this SoC today.

#![no_std]
#![forbid(unsafe_code)]

/// 32-bit register access to a window.
pub trait Regs {
    fn read32(&self, offset: usize) -> u32;
    fn write32(&self, offset: usize, value: u32);
}

/// Time for timeouts.
pub trait Clock {
    fn now_us(&self) -> u64;
    fn delay_us(&self, us: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The database's magic is wrong, or it ends before a field does.
    BadDatabase,
    NotInDatabase,
    /// `qcom,tcs-config` does not list active TCSes the way this driver reads it.
    BadConfig,
    UnsupportedVersion(u32),
    /// The firmware left the TCS in use.
    Busy,
    Timeout(&'static str),
    /// The transfer completed but the accelerator did not acknowledge the command.
    NotAcknowledged,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::BadDatabase => write!(f, "cmd-db is malformed"),
            Error::NotInDatabase => write!(f, "not in cmd-db"),
            Error::BadConfig => write!(f, "bad qcom,tcs-config"),
            Error::UnsupportedVersion(v) => write!(f, "RSC version {v:#x} not supported"),
            Error::Busy => write!(f, "active TCS busy"),
            Error::Timeout(w) => write!(f, "timed out on {w}"),
            Error::NotAcknowledged => write!(f, "command not acknowledged"),
        }
    }
}

const MAGIC: [u8; 4] = [0xdb, 0x30, 0x03, 0x0c];
const MAX_SLAVES: usize = 8;
const RSC_HEADER: usize = 16;
const ENTRY: usize = 24;
const HEADER: usize = 4 + 4 + MAX_SLAVES * RSC_HEADER + 4 + 4;

/// The address of `name` (up to 8 bytes, e.g. `ldoc9`) in a cmd-db image. `read` gives the image's byte at an offset,
/// `None` past its end.
pub fn cmd_db_address(read: impl Fn(usize) -> Option<u8>, name: &str) -> Result<u32, Error> {
    let u16_at = |o: usize| Some(u16::from_le_bytes([read(o)?, read(o + 1)?]) as usize);
    let u32_at = |o: usize| {
        Some(u32::from_le_bytes([
            read(o)?,
            read(o + 1)?,
            read(o + 2)?,
            read(o + 3)?,
        ]))
    };
    for (i, byte) in MAGIC.iter().enumerate() {
        if read(4 + i) != Some(*byte) {
            return Err(Error::BadDatabase);
        }
    }
    let query = name.as_bytes();
    if query.is_empty() || query.len() > 8 {
        return Err(Error::NotInDatabase);
    }
    for slave in 0..MAX_SLAVES {
        let at = 8 + slave * RSC_HEADER;
        let id = u16_at(at).ok_or(Error::BadDatabase)?;
        if id == 0 {
            break;
        }
        let entries = u16_at(at + 2).ok_or(Error::BadDatabase)?;
        let count = u16_at(at + 6).ok_or(Error::BadDatabase)?;
        for n in 0..count {
            let entry = HEADER + entries + n * ENTRY;
            let mut same = true;
            for j in 0..8 {
                let c = read(entry + j).ok_or(Error::BadDatabase)?;
                same &= c == query.get(j).copied().unwrap_or(0);
            }
            if same {
                return u32_at(entry + 16).ok_or(Error::BadDatabase);
            }
        }
    }
    Err(Error::NotInDatabase)
}

/// Where the active TCSes start and how many there are, from `qcom,tcs-config`'s `<type count>` pairs. TCSes are laid
/// out in the order listed, control TCSes taking no numbers (`rpmh_probe_tcs_config`).
pub fn active_tcs(config: &[u32]) -> Result<(u32, u32), Error> {
    const WAKE_OR_SLEEP_MAX: u32 = 1;
    const ACTIVE: u32 = 2;
    const CONTROL: u32 = 3;
    if !config.len().is_multiple_of(2) {
        return Err(Error::BadConfig);
    }
    let mut next = 0;
    let mut found = None;
    for pair in config.chunks(2) {
        let (kind, count) = (pair[0], pair[1]);
        match kind {
            CONTROL => continue,
            ACTIVE => found = Some((next, count)),
            k if k <= WAKE_OR_SLEEP_MAX => {}
            _ => return Err(Error::BadConfig),
        }
        next += count;
    }
    match found {
        Some((start, count)) if count > 0 => Ok((start, count)),
        _ => Err(Error::BadConfig),
    }
}

const RSC_DRV_ID: usize = 0x0;
const TCS_STRIDE: usize = 672;
const IRQ_STATUS: usize = 0x04;
const IRQ_CLEAR: usize = 0x08;
const TCS_WAIT: usize = 0x10;
const TCS_CONTROL: usize = 0x14;
const TCS_STATUS: usize = 0x18;
const TCS_CMD_ENABLE: usize = 0x1c;
const CMD_MSGID: usize = 0x30;
const CMD_ADDR: usize = 0x34;
const CMD_DATA: usize = 0x38;
const CMD_STATUS: usize = 0x3c;
const AMC_ENABLE: u32 = 1 << 16;
const AMC_TRIGGER: u32 = 1 << 24;
/// Four-byte write, response required.
const MSGID_WRITE_RESP: u32 = 8 | (1 << 8) | (1 << 16);
const CMD_ISSUED: u32 = 1 << 8;
const CMD_COMPLETE: u32 = 1 << 16;
const TIMEOUT_US: u64 = 1_000_000;

/// The application processor's RSC: `drv` is its `drv-<n>` window, `tcs_offset` its `qcom,tcs-offset`.
pub struct Rsc<R: Regs, C: Clock> {
    drv: R,
    clock: C,
    tcs_offset: usize,
    tcs: u32,
}

impl<R: Regs, C: Clock> Rsc<R, C> {
    pub fn new(drv: R, clock: C, tcs_offset: usize, config: &[u32]) -> Result<Self, Error> {
        let (tcs, _) = active_tcs(config)?;
        let version = drv.read32(RSC_DRV_ID);
        let major = (version >> 16) & 0xff;
        if major >= 3 {
            return Err(Error::UnsupportedVersion(version));
        }
        Ok(Self {
            drv,
            clock,
            tcs_offset,
            tcs,
        })
    }

    fn reg(&self, offset: usize) -> usize {
        self.tcs_offset + self.tcs as usize * TCS_STRIDE + offset
    }

    fn irq_reg(&self, offset: usize) -> usize {
        self.tcs_offset + offset
    }

    fn wait(&self, what: &'static str, mut done: impl FnMut() -> bool) -> Result<(), Error> {
        let deadline = self.clock.now_us() + TIMEOUT_US;
        while !done() {
            if self.clock.now_us() >= deadline {
                return Err(Error::Timeout(what));
            }
            self.clock.delay_us(10);
        }
        Ok(())
    }

    fn write_seen(&self, offset: usize, value: u32) -> Result<(), Error> {
        self.drv.write32(offset, value);
        self.wait("a TCS register", || self.drv.read32(offset) == value)
    }

    /// Sends one active-only write of `data` to `address`, and waits until the accelerator has acknowledged it.
    pub fn write(&self, address: u32, data: u32) -> Result<(), Error> {
        let (control, status) = (self.reg(TCS_CONTROL), self.reg(TCS_STATUS));
        let (enable, wait) = (self.reg(TCS_CMD_ENABLE), self.reg(TCS_WAIT));
        let mask = 1u32 << self.tcs;
        // TCS_STATUS reads 0 while in use.
        if self.drv.read32(status) == 0 {
            return Err(Error::Busy);
        }
        self.write_seen(
            control,
            self.drv.read32(control) & !AMC_TRIGGER & !AMC_ENABLE,
        )?;
        self.write_seen(enable, 0)?;
        self.drv.write32(self.irq_reg(IRQ_CLEAR), mask);
        self.wait("a stale completion", || {
            self.drv.read32(self.irq_reg(IRQ_STATUS)) & mask == 0
        })?;

        self.drv.write32(self.reg(CMD_MSGID), MSGID_WRITE_RESP);
        self.drv.write32(self.reg(CMD_ADDR), address);
        self.drv.write32(self.reg(CMD_DATA), data);
        self.write_seen(enable, 1)?;
        self.write_seen(wait, 1)?;
        self.write_seen(control, AMC_ENABLE)?;
        self.drv.write32(control, AMC_ENABLE | AMC_TRIGGER);

        let done = self.wait("an RPMh vote", || {
            self.drv.read32(self.irq_reg(IRQ_STATUS)) & mask != 0
        });
        let command_status = self.drv.read32(self.reg(CMD_STATUS));
        self.write_seen(control, 0)?;
        self.write_seen(enable, 0)?;
        self.write_seen(wait, 0)?;
        self.drv.write32(self.irq_reg(IRQ_CLEAR), mask);
        done?;
        if command_status & (CMD_ISSUED | CMD_COMPLETE) != CMD_ISSUED | CMD_COMPLETE {
            return Err(Error::NotAcknowledged);
        }
        Ok(())
    }

    /// Sets a VRM regulator at `resource` (its cmd-db address) to `millivolts` and turns it on: voltage first, as
    /// `rpmh_regulator_vrm_set_voltage_sel` and `rpmh_regulator_enable` run. Registers per `qcom-rpmh-regulator.c`.
    pub fn regulator_on(&self, resource: u32, millivolts: u32) -> Result<(), Error> {
        const VRM_VOLTAGE: u32 = 0x0;
        const VRM_ENABLE: u32 = 0x4;
        self.write(resource + VRM_VOLTAGE, millivolts)?;
        self.write(resource + VRM_ENABLE, 1)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use core::cell::{Cell, RefCell};
    use std::{collections::BTreeMap, vec, vec::Vec};

    use super::*;

    const TCS_OFF: usize = 0xd00;

    #[derive(Default)]
    struct Model {
        regs: RefCell<BTreeMap<usize, u32>>,
        sent: RefCell<Vec<(u32, u32)>>,
        irq: Cell<u32>,
        ack: Cell<bool>,
        hang: Cell<bool>,
    }

    impl Regs for &Model {
        fn read32(&self, offset: usize) -> u32 {
            if offset == TCS_OFF + IRQ_STATUS {
                return self.irq.get();
            }
            if offset == RSC_DRV_ID {
                return 0x0002_0700;
            }
            if offset == TCS_OFF + TCS_STATUS {
                return *self.regs.borrow().get(&offset).unwrap_or(&1);
            }
            *self.regs.borrow().get(&offset).unwrap_or(&0)
        }
        fn write32(&self, offset: usize, value: u32) {
            if offset == TCS_OFF + IRQ_CLEAR {
                self.irq.set(self.irq.get() & !value);
                return;
            }
            self.regs.borrow_mut().insert(offset, value);
            if offset == TCS_OFF + TCS_CONTROL && value & AMC_TRIGGER != 0 && !self.hang.get() {
                let (addr, data) = {
                    let r = self.regs.borrow();
                    (r[&(TCS_OFF + CMD_ADDR)], r[&(TCS_OFF + CMD_DATA)])
                };
                self.sent.borrow_mut().push((addr, data));
                assert_eq!(self.regs.borrow()[&(TCS_OFF + TCS_CMD_ENABLE)], 1);
                self.irq.set(1);
                let status = if self.ack.get() {
                    CMD_ISSUED | CMD_COMPLETE
                } else {
                    CMD_ISSUED
                };
                self.regs.borrow_mut().insert(TCS_OFF + CMD_STATUS, status);
            }
        }
    }

    #[derive(Default, Clone)]
    struct TestClock(std::rc::Rc<Cell<u64>>);

    impl Clock for TestClock {
        fn now_us(&self) -> u64 {
            self.0.set(self.0.get() + 1);
            self.0.get()
        }
        fn delay_us(&self, us: u64) {
            self.0.set(self.0.get() + us);
        }
    }

    const CONFIG: [u32; 8] = [2, 2, 0, 3, 1, 3, 3, 1];

    fn rsc(m: &Model) -> Rsc<&Model, TestClock> {
        Rsc::new(m, TestClock::default(), TCS_OFF, &CONFIG).unwrap()
    }

    #[test]
    fn the_laptops_tcs_config_puts_active_first() {
        assert_eq!(active_tcs(&CONFIG), Ok((0, 2)));
        // In another order the sleep TCSes come first; control takes no numbers.
        assert_eq!(active_tcs(&[3, 1, 0, 3, 1, 3, 2, 2]), Ok((6, 2)));
        assert_eq!(active_tcs(&[0, 3, 1, 3]), Err(Error::BadConfig));
        assert_eq!(active_tcs(&[2, 0]), Err(Error::BadConfig));
        assert_eq!(active_tcs(&[9, 1]), Err(Error::BadConfig));
        assert_eq!(active_tcs(&[2]), Err(Error::BadConfig));
    }

    #[test]
    fn a_regulator_gets_its_voltage_before_its_enable() {
        let m = Model::default();
        m.ack.set(true);
        rsc(&m).regulator_on(0x4000, 2950).unwrap();
        assert_eq!(*m.sent.borrow(), vec![(0x4000, 2950), (0x4004, 1)]);
        // Left as it was found.
        assert_eq!((&m).read32(TCS_OFF + TCS_CONTROL), 0);
        assert_eq!((&m).read32(TCS_OFF + TCS_CMD_ENABLE), 0);
        assert_eq!(m.irq.get(), 0);
        assert_eq!((&m).read32(TCS_OFF + CMD_MSGID), MSGID_WRITE_RESP);
    }

    #[test]
    fn a_vote_fails_on_a_busy_tcs_a_hang_and_no_acknowledgement() {
        let m = Model::default();
        m.ack.set(true);
        (&m).write32(TCS_OFF + TCS_STATUS, 0);
        assert_eq!(rsc(&m).write(0x4000, 1), Err(Error::Busy));
        assert!(m.sent.borrow().is_empty());

        let m = Model::default();
        m.hang.set(true);
        assert_eq!(
            rsc(&m).write(0x4000, 1),
            Err(Error::Timeout("an RPMh vote"))
        );
        // Nothing is left armed.
        assert_eq!((&m).read32(TCS_OFF + TCS_CONTROL), 0);

        let m = Model::default();
        assert_eq!(rsc(&m).write(0x4000, 1), Err(Error::NotAcknowledged));
    }

    #[test]
    fn a_stale_completion_is_cleared_first() {
        let m = Model::default();
        m.ack.set(true);
        m.irq.set(1);
        rsc(&m).write(0x4000, 1).unwrap();
        assert_eq!(m.sent.borrow().len(), 1);
    }

    #[test]
    fn other_rsc_versions_are_refused() {
        struct V3;
        impl Regs for V3 {
            fn read32(&self, _: usize) -> u32 {
                0x0003_0000
            }
            fn write32(&self, _: usize, _: u32) {
                panic!("no write");
            }
        }
        assert!(matches!(
            Rsc::new(V3, TestClock::default(), TCS_OFF, &CONFIG),
            Err(Error::UnsupportedVersion(0x0003_0000))
        ));
    }

    fn image() -> Vec<u8> {
        let mut db = vec![0u8; 4096];
        db[4..8].copy_from_slice(&MAGIC);
        let put16 =
            |db: &mut Vec<u8>, at: usize, v: u16| db[at..at + 2].copy_from_slice(&v.to_le_bytes());
        // Two slaves: an ARC one with one entry, the VRM one with two.
        put16(&mut db, 8, 3);
        put16(&mut db, 8 + 2, 0);
        put16(&mut db, 8 + 6, 1);
        put16(&mut db, 24, 4);
        put16(&mut db, 24 + 2, ENTRY as u16);
        put16(&mut db, 24 + 6, 2);
        let entry = |db: &mut Vec<u8>, at: usize, name: &str, addr: u32| {
            db[HEADER + at..HEADER + at + name.len()].copy_from_slice(name.as_bytes());
            db[HEADER + at + 16..HEADER + at + 20].copy_from_slice(&addr.to_le_bytes());
        };
        entry(&mut db, 0, "cx.lvl", 0x3_0000);
        entry(&mut db, ENTRY, "ldoc6", 0x4_0600);
        entry(&mut db, 2 * ENTRY, "ldoc9", 0x4_0900);
        db
    }

    #[test]
    fn cmd_db_finds_entries_across_slaves() {
        let db = image();
        let read = |o: usize| db.get(o).copied();
        assert_eq!(cmd_db_address(read, "ldoc9"), Ok(0x4_0900));
        assert_eq!(cmd_db_address(read, "ldoc6"), Ok(0x4_0600));
        assert_eq!(cmd_db_address(read, "cx.lvl"), Ok(0x3_0000));
        assert_eq!(cmd_db_address(read, "ldoc"), Err(Error::NotInDatabase));
        assert_eq!(
            cmd_db_address(read, "ldoc9-too-long"),
            Err(Error::NotInDatabase)
        );
    }

    #[test]
    fn cmd_db_refuses_a_bad_image() {
        let mut db = image();
        db[4] = 0;
        assert_eq!(
            cmd_db_address(|o| db.get(o).copied(), "ldoc9"),
            Err(Error::BadDatabase)
        );
        let db = image();
        // Cut before the entries.
        assert_eq!(
            cmd_db_address(|o| db[..HEADER].get(o).copied(), "ldoc9"),
            Err(Error::BadDatabase)
        );
    }
}
