//! Reads Qualcomm PMIC registers through the SPMI arbiter, version 5 (SC7180), and the PMIC's real-time clock; and writes the few
//! registers Juno needs: the backlight's LPG channel, and the interrupt and alarm registers of the power key and the RTC alarm.
//!
//! Behaviour from Linux: `drivers/spmi/spmi-pmic-arb.c` for the arbiter and `drivers/rtc/rtc-pm8xxx.c` for the clock. Only the
//! observer channels are used, which read: the one register written there is the channel's command register in the arbiter,
//! which starts a read.
//!
//! Two functions can write to the PMIC: [`backlight::write`], which refuses every address outside the backlight's LPG channel, and
//! [`irq::write`], which refuses everything [`allow::allowed`] does not name (Verus-proved: the PON's and the alarm's interrupt
//! registers and the alarm's own). The PMIC's reset settings, regulators and charger cannot be reached from here.
#![no_std]
#![allow(unexpected_cfgs)]
#![forbid(unsafe_code)]

pub mod alarm;
pub mod allow;
pub mod irq;

/// The arbiter's registers, by their blocks in the device tree (`reg-names`): "core" and the observer channels of this
/// execution environment.
pub trait Regs {
    fn core_read(&mut self, offset: usize) -> u32;
    /// Reads from the "cnfg" block, which holds the APID owner table.
    fn cnfg_read(&mut self, offset: usize) -> u32;
    /// Reads from the block of the observer channels.
    fn observer_read(&mut self, offset: usize) -> u32;
    /// Writes to the block of the observer channels: only a read command goes this way.
    fn observer_write(&mut self, offset: usize, value: u32);
    /// Waits a microsecond.
    fn delay_us(&mut self);
}

const VERSION: usize = 0x0;
const FEATURES: usize = 0x4;
const FEATURES_APID_MASK: u32 = 0x7ff;
const APID_MAP: usize = 0x900;
/// PPIDs in the map: bits 8 to 19.
const PPID_SHIFT: u32 = 8;
const PPID_MASK: u32 = 0xfff;
/// The APID owner table in the "cnfg" block (`apid_owner` for v5): one word per APID.
const APID_OWNER: usize = 0x700;
/// `SPMI_OWNERSHIP_PERIPH2OWNER`: the owning EE in the low 3 bits.
const OWNER_MASK: u32 = 0x7;

const CHANNEL_CMD: usize = 0x00;
const CHANNEL_STATUS: usize = 0x08;
const CHANNEL_RDATA0: usize = 0x18;
const CHANNEL_RDATA1: usize = 0x1c;

const OP_EXT_READL: u32 = 1;
/// How long a read may take, as in Linux.
const TIMEOUT_US: u32 = 1000;

const STATUS_DONE: u32 = 1 << 0;
const STATUS_FAILED: u32 = 1 << 1;
const STATUS_DENIED: u32 = 1 << 2;
const STATUS_DROPPED: u32 = 1 << 3;

/// What went wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    /// This arbiter is not version 5; the value is its version register.
    Version(u32),
    /// No channel serves this peripheral.
    NoChannel {
        ppid: u16,
    },
    Timeout,
    Failed,
    Denied,
    Dropped,
    /// A read is 1 to 8 bytes, within one peripheral.
    BadLength,
    /// Only the backlight's channel can be written.
    NotAllowed,
    /// The APID that serves this peripheral is not owned by our execution environment, so it cannot be written.
    NotOwner {
        ppid: u16,
    },
}

/// Where a peripheral's observer channel is, from the base of the block, for an execution environment.
pub fn channel_offset(ee: u32, apid: u32) -> usize {
    (0x10000 * ee + 0x80 * apid) as usize
}

/// The arbiter version, the number of channels, for the log.
pub fn identify<R: Regs>(regs: &mut R) -> (u32, u32) {
    (
        regs.core_read(VERSION),
        regs.core_read(FEATURES) & FEATURES_APID_MASK,
    )
}

/// The execution environment that may write through `apid`, from the owner table as `_pmic_arb_read_apid_map` reads it.
pub fn apid_owner<R: Regs>(regs: &mut R, apid: u32) -> u32 {
    regs.cnfg_read(APID_OWNER + 4 * apid as usize) & OWNER_MASK
}

/// Finds the channel of a peripheral, `ppid` being its SID and the high byte of its registers' addresses. This reads the map; the
/// caller keeps the result. `core_size` limits the map to what the device tree says "core" holds. When several APIDs map to the
/// same PPID, the one owned by `ee` is preferred, as `_pmic_arb_read_apid_map` does; the first match is kept when none is.
pub fn find_apid<R: Regs>(regs: &mut R, ppid: u16, ee: u32, core_size: usize) -> Result<u32, Error> {
    let version = regs.core_read(VERSION);
    if !(0x5000_0000..0x7000_0000).contains(&version) {
        return Err(Error::Version(version));
    }
    let count = regs.core_read(FEATURES) & FEATURES_APID_MASK;
    let mut found: Option<u32> = None;
    for apid in 0..count {
        let offset = APID_MAP + 4 * apid as usize;
        if offset >= core_size {
            break;
        }
        let entry = regs.core_read(offset);
        if entry != 0
            && (entry >> PPID_SHIFT) & PPID_MASK == u32::from(ppid)
            && (found.is_none() || apid_owner(regs, apid) == ee)
        {
            found = Some(apid);
        }
    }
    found.ok_or(Error::NoChannel { ppid })
}

/// Where a peripheral's write channel is, as `pmic_arb_offset_v5` does for a read/write channel: the APID chosen for the PPID
/// must be owned by `ee`, else the write is refused. Observer reads stay allowed on any APID.
pub fn write_channel<R: Regs>(regs: &mut R, ppid: u16, ee: u32, core_size: usize) -> Result<usize, Error> {
    let apid = find_apid(regs, ppid, ee, core_size)?;
    if apid_owner(regs, apid) != ee {
        return Err(Error::NotOwner { ppid });
    }
    Ok(backlight::channel_offset(apid))
}

/// Reads registers of one peripheral through its channel. `channel` is [`channel_offset`]'s value.
pub fn read<R: Regs>(regs: &mut R, channel: usize, addr: u16, out: &mut [u8]) -> Result<(), Error> {
    if out.is_empty() || out.len() > 8 || (usize::from(addr) & 0xff) + out.len() > 0x100 {
        return Err(Error::BadLength);
    }
    let byte_count = (out.len() - 1) as u32;
    let command = OP_EXT_READL << 27 | (u32::from(addr) & 0xff) << 4 | byte_count;
    regs.observer_write(channel + CHANNEL_CMD, command);
    let mut waited = 0;
    loop {
        let status = regs.observer_read(channel + CHANNEL_STATUS);
        if status & STATUS_DONE != 0 {
            if status & STATUS_FAILED != 0 {
                return Err(Error::Failed);
            }
            if status & STATUS_DENIED != 0 {
                return Err(Error::Denied);
            }
            if status & STATUS_DROPPED != 0 {
                return Err(Error::Dropped);
            }
            break;
        }
        if waited == TIMEOUT_US {
            return Err(Error::Timeout);
        }
        waited += 1;
        regs.delay_us();
    }
    let (low, high) = out.split_at_mut(out.len().min(4));
    low.copy_from_slice(&regs.observer_read(channel + CHANNEL_RDATA0).to_le_bytes()[..low.len()]);
    if !high.is_empty() {
        let word = regs.observer_read(channel + CHANNEL_RDATA1).to_le_bytes();
        high.copy_from_slice(&word[..high.len()]);
    }
    Ok(())
}

/// The real-time clock's registers on `qcom,pm8941-rtc`: address of the device tree's "rtc" block plus these.
pub mod rtc {
    use super::{Error, Regs, read};

    pub const READ: u16 = 0x48;
    pub const CONTROL: u16 = 0x46;
    /// In the control register: the counter runs.
    pub const ENABLE: u8 = 1 << 7;

    /// Reads the counter, in seconds, as Linux's `pm8xxx_rtc_read_raw`: if the low byte went back while reading, the upper
    /// bytes may belong to another second, and the read is done again.
    pub fn read_raw<R: Regs>(regs: &mut R, channel: usize, base: u16) -> Result<u32, Error> {
        let mut value = [0u8; 4];
        read(regs, channel, base + READ, &mut value)?;
        let mut low = [0u8; 1];
        read(regs, channel, base + READ, &mut low)?;
        if low[0] < value[0] {
            read(regs, channel, base + READ, &mut value)?;
        }
        Ok(u32::from_le_bytes(value))
    }

    /// Why a counter and offset are not believed. The counter can be zeroed by a hard power cycle, as it seems on the Galaxy
    /// Book Go, and the sum then looks like a date but lies in the past.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct TooEarly {
        pub unix: i64,
        pub floor: i64,
        pub floor_name: &'static str,
    }

    /// The time from a counter and an offset, if it is not before any of the floors: dates known to have passed, each with a name for
    /// the log.
    pub fn time_from(
        raw: u32,
        offset: i64,
        floors: &[(i64, &'static str)],
    ) -> Result<i64, TooEarly> {
        let unix = i64::from(raw).saturating_add(offset);
        match floors.iter().find(|(floor, _)| unix < *floor) {
            Some(&(floor, floor_name)) => Err(TooEarly {
                unix,
                floor,
                floor_name,
            }),
            None => Ok(unix),
        }
    }

    /// Whether the counter runs.
    pub fn is_enabled<R: Regs>(regs: &mut R, channel: usize, base: u16) -> Result<bool, Error> {
        let mut control = [0u8; 1];
        read(regs, channel, base + CONTROL, &mut control)?;
        Ok(control[0] & ENABLE != 0)
    }
}

/// The power-on block (`qcom,pm8998-pon`), behaviour from Linux's `pm8941-pwrkey.c`. Read only, like the rest of this crate:
/// the reset settings below are named so that they can be reported, and there is no way to write them from here.
pub mod pon {
    pub const REV2: u16 = 0x01;
    pub const SUBTYPE: u16 = 0x05;
    pub const RT_STS: u16 = 0x10;
    pub const KPDPWR_S1_TIMER: u16 = 0x40;
    pub const KPDPWR_S2_TIMER: u16 = 0x41;
    pub const KPDPWR_S2_CNTL: u16 = 0x42;
    pub const KPDPWR_S2_CNTL2: u16 = 0x43;
    /// In `RT_STS`: `PON_KPDPWR_N_SET`.
    const KPDPWR_N_SET: u8 = 1 << 0;

    /// Whether `RT_STS` says the power key is down.
    pub fn key_is_down(rt_sts: u8) -> bool {
        rt_sts & KPDPWR_N_SET != 0
    }
}

/// The write channel of the backlight's LPG channel on the Galaxy Book Go, behaviour from Linux's `pmic_arb_write_cmd`.
pub mod backlight {
    use super::{CHANNEL_CMD, Error};

    /// SID 5, `0xbc00`.
    const ONLY_PPID: u16 = 5 << 8 | 0xbc;
    const CHANNEL_WDATA0: usize = 0x10;
    const OP_EXT_WRITEL: u32 = 0;

    /// A channel's registers in the arbiter's "chnls" block: this channel's page, by offset.
    pub trait WriteChannel {
        fn read(&mut self, offset: usize) -> u32;
        fn write(&mut self, offset: usize, value: u32);
        fn delay_us(&mut self);
    }

    /// Where a peripheral's write channel is, from the base of the "chnls" block.
    pub fn channel_offset(apid: u32) -> usize {
        0x10000 * apid as usize
    }

    /// Writes one byte. `ppid` and `addr` are checked against the one peripheral this may touch.
    pub fn write<C: WriteChannel>(channel: &mut C, ppid: u16, addr: u16, value: u8) -> Result<(), Error> {
        if ppid != ONLY_PPID || addr >> 8 != ONLY_PPID & 0xff {
            return Err(Error::NotAllowed);
        }
        channel.write(CHANNEL_WDATA0, u32::from(value));
        channel.write(CHANNEL_CMD, OP_EXT_WRITEL << 27 | (u32::from(addr) & 0xff) << 4);
        crate::wait_done(channel)
    }
}

/// Waits for a write channel's command to finish, as `pmic_arb_wait_for_done`: DONE, then FAILED, DENIED or DROPPED as errors,
/// at most [`TIMEOUT_US`] microseconds.
pub(crate) fn wait_done<C: backlight::WriteChannel>(channel: &mut C) -> Result<(), Error> {
    let mut waited = 0;
    loop {
        let status = channel.read(CHANNEL_STATUS);
        if status & STATUS_DONE != 0 {
            if status & STATUS_FAILED != 0 {
                return Err(Error::Failed);
            }
            if status & STATUS_DENIED != 0 {
                return Err(Error::Denied);
            }
            if status & STATUS_DROPPED != 0 {
                return Err(Error::Dropped);
            }
            return Ok(());
        }
        if waited == TIMEOUT_US {
            return Err(Error::Timeout);
        }
        waited += 1;
        channel.delay_us();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{vec, vec::Vec};

    use super::*;

    const EE: u32 = 0;
    const RTC_PPID: u16 = 0x060;
    const RTC_APID: u32 = 7;

    /// An arbiter as the hardware behaves: a command on a channel reads the PMIC's registers into RDATA.
    struct Model {
        map: Vec<u32>,
        /// The APID owner table in the "cnfg" block.
        owners: Vec<u32>,
        pmic: Vec<u8>,
        status: u32,
        rdata: [u32; 2],
        delay_until_done: u32,
        delays: u32,
        commands: Vec<(usize, u32)>,
        /// A counter that moves on after this many reads of the counter's low byte.
        tick_after_reads: Option<u32>,
    }

    impl Model {
        fn new() -> Self {
            let mut map = vec![0u32; 32];
            map[3] = 0x020 << 8;
            map[RTC_APID as usize] = u32::from(RTC_PPID) << 8 | 1 << 24;
            Self {
                map,
                owners: vec![0; 32],
                pmic: vec![0; 0x10000],
                status: 0,
                rdata: [0; 2],
                delay_until_done: 0,
                delays: 0,
                commands: Vec::new(),
                tick_after_reads: None,
            }
        }

        fn set_counter(&mut self, base: usize, value: u32) {
            self.pmic[base + 0x48..base + 0x4c].copy_from_slice(&value.to_le_bytes());
        }
    }

    impl Regs for Model {
        fn core_read(&mut self, offset: usize) -> u32 {
            match offset {
                VERSION => 0x5000_0000,
                FEATURES => self.map.len() as u32 | 0xf000,
                _ if offset >= APID_MAP => self.map[(offset - APID_MAP) / 4],
                _ => panic!("read of core register {offset:#x}"),
            }
        }

        fn cnfg_read(&mut self, offset: usize) -> u32 {
            assert!(offset >= APID_OWNER, "read of cnfg register {offset:#x}");
            self.owners[(offset - APID_OWNER) / 4]
        }

        fn observer_read(&mut self, offset: usize) -> u32 {
            match offset & 0x7f {
                x if x == CHANNEL_STATUS => {
                    if self.delays < self.delay_until_done {
                        0
                    } else {
                        self.status
                    }
                }
                x if x == CHANNEL_RDATA0 => self.rdata[0],
                x if x == CHANNEL_RDATA1 => self.rdata[1],
                x => panic!("read of channel register {x:#x}"),
            }
        }

        fn observer_write(&mut self, offset: usize, value: u32) {
            assert_eq!(offset & 0x7f, CHANNEL_CMD, "only commands are written");
            assert_eq!(value >> 27, OP_EXT_READL, "only reads");
            self.commands.push((offset, value));
            let apid = ((offset - channel_offset(EE, 0)) / 0x80) as u32;
            let ppid = (self.map[apid as usize] >> PPID_SHIFT & PPID_MASK) as usize;
            let addr = (ppid & 0xff) << 8 | (value >> 4 & 0xff) as usize;
            let len = (value & 7) as usize + 1;
            let mut bytes = [0u8; 8];
            bytes[..len].copy_from_slice(&self.pmic[addr..addr + len]);
            self.rdata = [
                u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                u32::from_le_bytes(bytes[4..].try_into().unwrap()),
            ];
            self.status = STATUS_DONE;
            if let Some(reads) = &mut self.tick_after_reads {
                if *reads == 0 {
                    self.pmic[0x6048..0x604c].copy_from_slice(&0x0002_0000u32.to_le_bytes());
                    self.tick_after_reads = None;
                } else {
                    *reads -= 1;
                }
            }
        }

        fn delay_us(&mut self) {
            self.delays += 1;
        }
    }

    struct FakeChannel {
        writes: Vec<(usize, u32)>,
        status: u32,
    }

    impl backlight::WriteChannel for FakeChannel {
        fn read(&mut self, _offset: usize) -> u32 {
            self.status
        }

        fn write(&mut self, offset: usize, value: u32) {
            self.writes.push((offset, value));
        }

        fn delay_us(&mut self) {}
    }

    #[test]
    fn a_backlight_write_is_data_then_command() {
        let mut ch = FakeChannel {
            writes: Vec::new(),
            status: 1,
        };
        backlight::write(&mut ch, 0x5bc, 0xbc46, 0x80).unwrap();
        assert_eq!(ch.writes, [(0x10, 0x80), (0x00, 0x46 << 4)]);
    }

    #[test]
    fn nothing_but_the_backlight_can_be_written() {
        let mut ch = FakeChannel {
            writes: Vec::new(),
            status: 1,
        };
        // PON's reset settings, on both SIDs that could hold one; another LPG channel; a mismatch of channel and address.
        for (ppid, addr) in [(0x008, 0x0842), (0x408, 0x0842), (0x5b1, 0xb146), (0x5bc, 0x0842), (0x008, 0xbc46)] {
            assert_eq!(backlight::write(&mut ch, ppid, addr, 0), Err(Error::NotAllowed));
        }
        assert!(ch.writes.is_empty());
    }

    #[test]
    fn a_denied_or_silent_backlight_write_says_so() {
        let mut denied = FakeChannel {
            writes: Vec::new(),
            status: 1 | 1 << 2,
        };
        assert_eq!(backlight::write(&mut denied, 0x5bc, 0xbc46, 0), Err(Error::Denied));
        let mut silent = FakeChannel {
            writes: Vec::new(),
            status: 0,
        };
        assert_eq!(backlight::write(&mut silent, 0x5bc, 0xbc46, 0), Err(Error::Timeout));
    }

    #[test]
    fn the_power_key_is_bit_0_of_the_status() {
        assert!(pon::key_is_down(0x01));
        assert!(pon::key_is_down(0x83));
        assert!(!pon::key_is_down(0x80));
        assert!(!pon::key_is_down(0));
    }

    #[test]
    fn the_channel_is_found_in_the_map() {
        let mut m = Model::new();
        assert_eq!(find_apid(&mut m, RTC_PPID, EE, 0x1100), Ok(RTC_APID));
        assert_eq!(find_apid(&mut m, 0x20, EE, 0x1100), Ok(3));
        assert_eq!(
            find_apid(&mut m, 0x61, EE, 0x1100),
            Err(Error::NoChannel { ppid: 0x61 })
        );
    }

    #[test]
    fn the_map_stops_at_the_block() {
        let mut m = Model::new();
        assert_eq!(
            find_apid(&mut m, RTC_PPID, EE, APID_MAP + 4 * 3),
            Err(Error::NoChannel { ppid: RTC_PPID })
        );
    }

    #[test]
    fn of_two_apids_for_one_ppid_the_one_owned_by_our_ee_is_chosen() {
        let mut m = Model::new();
        // APID 7 (ours, EE 0) and APID 9 both serve the RTC's PPID; APID 9 belongs to EE 3.
        m.map[9] = u32::from(RTC_PPID) << 8 | 1 << 24;
        m.owners[9] = 3;
        assert_eq!(find_apid(&mut m, RTC_PPID, EE, 0x1100), Ok(RTC_APID));
        // From EE 3's point of view the later, owned APID wins.
        assert_eq!(find_apid(&mut m, RTC_PPID, 3, 0x1100), Ok(9));
        // An EE that owns neither gets the first mapping, as Linux does.
        assert_eq!(find_apid(&mut m, RTC_PPID, 5, 0x1100), Ok(RTC_APID));
    }

    #[test]
    fn a_write_channel_needs_an_apid_owned_by_our_ee() {
        let mut m = Model::new();
        assert_eq!(
            write_channel(&mut m, RTC_PPID, EE, 0x1100),
            Ok(backlight::channel_offset(RTC_APID))
        );
        // The only APID for the PPID belongs to another EE: the write is refused, the read is not.
        m.owners[RTC_APID as usize] = 3;
        assert_eq!(
            write_channel(&mut m, RTC_PPID, EE, 0x1100),
            Err(Error::NotOwner { ppid: RTC_PPID })
        );
        let mut out = [0u8; 4];
        assert!(read(&mut m, channel_offset(EE, RTC_APID), 0x6048, &mut out).is_ok());
    }

    struct OtherVersion;

    impl Regs for OtherVersion {
        fn core_read(&mut self, _offset: usize) -> u32 {
            0x7000_0000
        }
        fn cnfg_read(&mut self, _offset: usize) -> u32 {
            0
        }
        fn observer_read(&mut self, _offset: usize) -> u32 {
            0
        }
        fn observer_write(&mut self, _offset: usize, _value: u32) {}
        fn delay_us(&mut self) {}
    }

    #[test]
    fn only_version_5_is_read() {
        assert_eq!(
            find_apid(&mut OtherVersion, RTC_PPID, EE, 0x1100),
            Err(Error::Version(0x7000_0000))
        );
    }

    #[test]
    fn channels_are_where_linux_has_them() {
        assert_eq!(channel_offset(0, 7), 0x380);
        assert_eq!(channel_offset(1, 7), 0x10380);
    }

    #[test]
    fn the_command_is_linuxs() {
        let mut m = Model::new();
        let mut out = [0u8; 4];
        read(&mut m, channel_offset(EE, RTC_APID), 0x6048, &mut out).unwrap();
        // EXT_READL, address low byte, 3 for 4 bytes.
        assert_eq!(m.commands, [(0x380, 1 << 27 | 0x48 << 4 | 3)]);
    }

    #[test]
    fn long_reads_use_both_data_registers() {
        let mut m = Model::new();
        m.pmic[0x6048..0x6050].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut out = [0u8; 6];
        read(&mut m, channel_offset(EE, RTC_APID), 0x6048, &mut out).unwrap();
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn bad_lengths_are_refused() {
        let mut m = Model::new();
        let c = channel_offset(EE, RTC_APID);
        assert_eq!(read(&mut m, c, 0x6048, &mut []), Err(Error::BadLength));
        assert_eq!(read(&mut m, c, 0x6048, &mut [0; 9]), Err(Error::BadLength));
        assert_eq!(read(&mut m, c, 0x60fe, &mut [0; 4]), Err(Error::BadLength));
    }

    #[test]
    fn a_slow_read_is_waited_for() {
        let mut m = Model::new();
        m.delay_until_done = 500;
        read(&mut m, 0x380, 0x6048, &mut [0; 1]).unwrap();
        assert_eq!(m.delays, 500);
    }

    #[test]
    fn a_read_that_never_ends_times_out() {
        let mut m = Model::new();
        m.delay_until_done = u32::MAX;
        assert_eq!(
            read(&mut m, 0x380, 0x6048, &mut [0; 1]),
            Err(Error::Timeout)
        );
    }

    #[test]
    fn bad_status_is_reported() {
        struct Status(u32);
        impl Regs for Status {
            fn core_read(&mut self, _offset: usize) -> u32 {
                0
            }
            fn cnfg_read(&mut self, _offset: usize) -> u32 {
                0
            }
            fn observer_read(&mut self, _offset: usize) -> u32 {
                self.0
            }
            fn observer_write(&mut self, _offset: usize, _value: u32) {}
            fn delay_us(&mut self) {}
        }
        for (status, error) in [
            (STATUS_DONE | STATUS_FAILED, Error::Failed),
            (STATUS_DONE | STATUS_DENIED, Error::Denied),
            (STATUS_DONE | STATUS_DROPPED, Error::Dropped),
        ] {
            assert_eq!(
                read(&mut Status(status), 0, 0x6048, &mut [0; 1]),
                Err(error)
            );
        }
    }

    #[test]
    fn the_counter_is_read_little_endian() {
        let mut m = Model::new();
        m.set_counter(0x6000, 0x6b49_d280);
        assert_eq!(rtc::read_raw(&mut m, 0x380, 0x6000), Ok(0x6b49_d280));
    }

    #[test]
    fn a_carry_during_the_read_reads_again() {
        let mut m = Model::new();
        // 0x0001ffff, then the counter moves on before the low byte is read again.
        m.set_counter(0x6000, 0x0001_ffff);
        m.tick_after_reads = Some(0);
        assert_eq!(rtc::read_raw(&mut m, 0x380, 0x6000), Ok(0x0002_0000));
    }

    #[test]
    fn a_time_after_every_floor_is_believed() {
        let floors = [(1000, "a"), (2000, "b")];
        assert_eq!(rtc::time_from(500, 1600, &floors), Ok(2100));
        assert_eq!(rtc::time_from(0, 2000, &floors), Ok(2000));
        assert_eq!(rtc::time_from(5, 5, &[]), Ok(10));
    }

    #[test]
    fn a_zeroed_counter_falls_before_the_floor() {
        // Offset measured in the past, the counter back at a few seconds, a kernel built later.
        let floors = [(1000, "2026"), (1_793_600_000, "this kernel's build")];
        assert_eq!(
            rtc::time_from(12, 1_793_559_296, &floors),
            Err(rtc::TooEarly {
                unix: 1_793_559_308,
                floor: 1_793_600_000,
                floor_name: "this kernel's build",
            })
        );
    }

    #[test]
    fn extreme_offsets_do_not_overflow() {
        assert_eq!(rtc::time_from(u32::MAX, i64::MAX, &[]), Ok(i64::MAX));
        assert!(rtc::time_from(1, i64::MIN, &[(0, "x")]).is_err());
    }

    #[test]
    fn the_control_register_says_if_it_runs() {
        let mut m = Model::new();
        assert_eq!(rtc::is_enabled(&mut m, 0x380, 0x6000), Ok(false));
        m.pmic[0x6046] = 0x80;
        assert_eq!(rtc::is_enabled(&mut m, 0x380, 0x6000), Ok(true));
    }
}

#[cfg(test)]
mod wake_tests;
