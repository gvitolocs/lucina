// SPDX-License-Identifier: MPL-2.0

//! The wake interrupts' register work (`irq`, `alarm`, `allow`) against a model that records every access in order.

extern crate std;
use std::{vec, vec::Vec};

use crate::{
    Error, alarm,
    allow::{self, ALARM_PPID, PON_PPID},
    backlight::WriteChannel,
    irq::{self, Arbiter, Periph, Trigger},
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Access {
    Read(u16, usize),
    Write(u16, Vec<u8>),
    ApidRead(u32, usize),
    ApidWrite(u32, usize, u32),
    Acc(u32),
}

/// A PMIC's registers and an arbiter's, recording what is done to them.
struct Model {
    pmic: Vec<u8>,
    apid: Vec<(u32, usize, u32)>,
    acc: Vec<u32>,
    log: Vec<Access>,
}

impl Model {
    fn new() -> Self {
        Self { pmic: vec![0; 0x10000], apid: Vec::new(), acc: vec![0; 16], log: Vec::new() }
    }

    fn writes(&self) -> Vec<Access> {
        self.log.iter().filter(|a| !matches!(a, Access::Read(..) | Access::ApidRead(..) | Access::Acc(_))).cloned().collect()
    }
}

impl Periph for Model {
    fn read(&mut self, addr: u16, out: &mut [u8]) -> Result<(), Error> {
        self.log.push(Access::Read(addr, out.len()));
        let a = usize::from(addr);
        out.copy_from_slice(&self.pmic[a..a + out.len()]);
        Ok(())
    }

    fn write(&mut self, addr: u16, data: &[u8]) -> Result<(), Error> {
        self.log.push(Access::Write(addr, data.to_vec()));
        let a = usize::from(addr);
        self.pmic[a..a + data.len()].copy_from_slice(data);
        Ok(())
    }
}

impl Arbiter for Model {
    fn apid_read(&mut self, apid: u32, offset: usize) -> u32 {
        self.log.push(Access::ApidRead(apid, offset));
        self.apid.iter().rev().find(|(a, o, _)| *a == apid && *o == offset).map_or(0, |(_, _, v)| *v)
    }

    fn apid_write(&mut self, apid: u32, offset: usize, value: u32) {
        self.log.push(Access::ApidWrite(apid, offset, value));
        self.apid.push((apid, offset, value));
    }

    fn owner_acc_status(&mut self, word: u32) -> u32 {
        self.log.push(Access::Acc(word));
        self.acc[word as usize]
    }
}

const PON: u16 = 0x0800;
const ALARM: u16 = 0x6100;

#[test]
fn the_power_key_is_set_up_as_linux_requests_it() {
    // `request_irq` of `<0 8 0 IRQ_TYPE_EDGE_BOTH>`: activate, set_type, then unmask.
    let mut m = Model::new();
    m.pmic[0x811..0x814].copy_from_slice(&[0x40, 0x00, 0x80]);
    irq::activate(&mut m, PON, 0).unwrap();
    irq::set_type(&mut m, PON, 0, Trigger::EdgeBoth).unwrap();
    irq::unmask(&mut m, &mut Model::new(), 3, PON, 0).unwrap();
    assert_eq!(
        m.log,
        [
            Access::Write(0x816, vec![0x01]),
            Access::Write(0x814, vec![0x01]),
            Access::Read(0x811, 3),
            // Only bit 0 changes; the other bits the firmware left stay.
            Access::Write(0x811, vec![0x41, 0x01, 0x81]),
            Access::Read(0x815, 1),
            Access::Write(0x814, vec![0x01, 0x01]),
        ]
    );
}

#[test]
fn unmask_turns_acc_on_first_and_skips_an_enabled_interrupt() {
    let mut p = Model::new();
    let mut a = Model::new();
    p.pmic[0x6115] = 0x02;
    irq::unmask(&mut p, &mut a, 9, ALARM, 1).unwrap();
    assert_eq!(a.log, [Access::ApidWrite(9, irq::ACC_ENABLE, 1)]);
    assert_eq!(p.log, [Access::Read(0x6115, 1)]);
}

#[test]
fn the_alarm_interrupt_is_rising_edge() {
    assert_eq!(irq::type_bytes([0, 0, 0], 1, alarm::TRIGGER), [0x02, 0x02, 0x00]);
    assert_eq!(irq::type_bytes([0xff, 0xff, 0xff], 1, Trigger::LevelHigh), [0xfd, 0xff, 0xfd]);
    assert_eq!(irq::type_bytes([0xff, 0xff, 0xff], 1, Trigger::LevelLow), [0xfd, 0xfd, 0xff]);
    assert_eq!(irq::type_bytes([0, 0, 0], 0, Trigger::EdgeFalling), [0x01, 0x00, 0x01]);
}

#[test]
fn ack_clears_the_arbiter_then_the_latch_and_reads_nothing() {
    let mut p = Model::new();
    let mut a = Model::new();
    irq::ack(&mut p, &mut a, 3, PON, 0).unwrap();
    assert_eq!(a.log, [Access::ApidWrite(3, irq::IRQ_CLEAR, 1)]);
    assert_eq!(p.log, [Access::Write(0x814, vec![0x01])]);
    irq::mask(&mut p, PON, 0).unwrap();
    assert_eq!(p.log.last(), Some(&Access::Write(0x816, vec![0x01])));
}

#[test]
fn pending_reads_the_words_and_skips_what_is_not_enabled() {
    let mut a = Model::new();
    // APIDs 3 and 40 have bits; 40's ACC_ENABLE is off.
    a.acc[0] = 1 << 3;
    a.acc[1] = 1 << 8;
    a.apid.push((3, irq::ACC_ENABLE, 1));
    a.apid.push((3, irq::IRQ_STATUS, 0x1));
    a.apid.push((40, irq::IRQ_STATUS, 0x4));
    let mut seen = Vec::new();
    let calls = irq::pending(&mut a, 0, 63, &[3, 40], |apid, status| seen.push((apid, status)));
    assert_eq!(calls, 1);
    assert_eq!(seen, [(3, 1)]);
    // Nothing cleared by the scan itself.
    assert!(a.writes().is_empty());
}

#[test]
fn pending_falls_back_to_the_owned_apids_when_no_word_has_a_bit() {
    let mut a = Model::new();
    a.apid.push((97, irq::ACC_ENABLE, 1));
    a.apid.push((97, irq::IRQ_STATUS, 0x2));
    let mut seen = Vec::new();
    assert_eq!(irq::pending(&mut a, 3, 97, &[3, 97], |apid, status| seen.push((apid, status))), 1);
    assert_eq!(seen, [(97, 2)]);
    // The words read were those of the range: 3 >> 5 to 97 >> 5.
    let words: Vec<u32> = a.log.iter().filter_map(|x| if let Access::Acc(w) = x { Some(*w) } else { None }).collect();
    assert_eq!(words, [0, 1, 2, 3]);
}

#[test]
fn pending_ignores_bits_outside_the_range() {
    let mut a = Model::new();
    a.acc[0] = 1 << 1;
    a.apid.push((1, irq::ACC_ENABLE, 1));
    a.apid.push((1, irq::IRQ_STATUS, 1));
    assert_eq!(irq::pending(&mut a, 3, 31, &[], |_, _| panic!("APID 1 is out of range")), 0);
}

#[test]
fn an_unknown_status_is_cleared_in_the_arbiter_only() {
    let mut a = Model::new();
    irq::clear_unknown(&mut a, 12, 0x5);
    assert_eq!(a.log, [Access::ApidWrite(12, irq::IRQ_CLEAR, 0x5)]);
}

#[test]
fn the_alarm_is_set_off_first_then_written_then_on() {
    let mut p = Model::new();
    p.pmic[0x6146] = 0x80;
    alarm::set(&mut p, ALARM, 0x0102_0304).unwrap();
    assert_eq!(
        p.writes(),
        [Access::Write(0x6146, vec![0x00]), Access::Write(0x6140, vec![0x04, 0x03, 0x02, 0x01]), Access::Write(0x6146, vec![0x80])]
    );
    assert_eq!(alarm::armed(&mut p, ALARM).unwrap(), Some(0x0102_0304));
}

#[test]
fn update_bits_writes_nothing_that_is_already_so() {
    // The alarm already off: `set` only writes the value and turns it on.
    let mut p = Model::new();
    alarm::set(&mut p, ALARM, 7).unwrap();
    assert_eq!(p.writes(), [Access::Write(0x6140, vec![7, 0, 0, 0]), Access::Write(0x6146, vec![0x80])]);
}

#[test]
fn disabling_the_alarm_turns_it_off_and_clears_the_value() {
    let mut p = Model::new();
    p.pmic[0x6146] = 0x81;
    alarm::disable(&mut p, ALARM).unwrap();
    assert_eq!(p.writes(), [Access::Write(0x6146, vec![0x01]), Access::Write(0x6140, vec![0, 0, 0, 0])]);
    assert_eq!(alarm::armed(&mut p, ALARM).unwrap(), None);
}

#[test]
fn the_alarm_interrupt_turns_it_off_and_updates_alarm_clear_to_0() {
    let mut p = Model::new();
    p.pmic[0x6146] = 0x80;
    p.pmic[0x6148] = 0x01;
    alarm::on_irq(&mut p, ALARM).unwrap();
    assert_eq!(p.writes(), [Access::Write(0x6146, vec![0x00]), Access::Write(0x6148, vec![0x00])]);
    // Nothing to clear: only the alarm goes off.
    let mut p = Model::new();
    p.pmic[0x6146] = 0x80;
    alarm::on_irq(&mut p, ALARM).unwrap();
    assert_eq!(p.writes(), [Access::Write(0x6146, vec![0x00])]);
}

#[test]
fn the_allowlist_names_exactly_the_interrupt_and_alarm_registers() {
    for reg in 0x11..=0x16 {
        assert!(allow::allowed(PON_PPID, PON | reg, 1), "PON {:#x}", reg);
        assert!(allow::allowed(ALARM_PPID, ALARM | reg, 1), "alarm {:#x}", reg);
    }
    for reg in [0x40, 0x41, 0x42, 0x43, 0x46, 0x48] {
        assert!(allow::allowed(ALARM_PPID, ALARM | reg, 1), "alarm {:#x}", reg);
        assert!(!allow::allowed(PON_PPID, PON | reg, 1), "PON {:#x}", reg);
    }
    // The PON's reset and pull settings, the status registers, and anything in the RTC itself.
    for reg in [0x10, 0x17, 0x18, 0x40, 0x41, 0x42, 0x43, 0x5a, 0x5b, 0x70, 0x71] {
        assert!(!allow::allowed(PON_PPID, PON | reg, 1), "PON {:#x}", reg);
    }
    for addr in [0x6046, 0x6040, 0x6048, 0x6111 - 0x100] {
        assert!(!allow::allowed(0x060, addr, 1), "{:#x}", addr);
    }
    // Ranges: inside, straddling, too long, empty; another SID; an address of another peripheral.
    assert!(allow::allowed(PON_PPID, 0x811, 3));
    assert!(allow::allowed(ALARM_PPID, 0x6140, 4));
    assert!(!allow::allowed(PON_PPID, 0x815, 3));
    assert!(!allow::allowed(ALARM_PPID, 0x613f, 2));
    assert!(!allow::allowed(ALARM_PPID, 0x6140, 5));
    assert!(!allow::allowed(PON_PPID, 0x811, 0));
    assert!(!allow::allowed(0x108, 0x811, 1));
    assert!(!allow::allowed(PON_PPID, 0x6111, 1));
}

struct Channel {
    regs: Vec<(usize, u32)>,
}

impl WriteChannel for Channel {
    fn read(&mut self, offset: usize) -> u32 {
        // STATUS: done at once.
        if offset == 0x08 { 1 } else { 0 }
    }

    fn write(&mut self, offset: usize, value: u32) {
        self.regs.push((offset, value));
    }

    fn delay_us(&mut self) {}
}

#[test]
fn write_sends_the_bytes_and_v5s_command() {
    let mut c = Channel { regs: Vec::new() };
    irq::write(&mut c, PON_PPID, 0x814, &[0x01, 0x01]).unwrap();
    // WDATA0 little endian, then CMD: EXT_WRITEL (0) << 27 | 0x14 << 4 | (2 - 1).
    assert_eq!(c.regs, [(0x10, 0x0101), (0x00, 0x14 << 4 | 1)]);
}

#[test]
fn write_refuses_before_touching_the_channel() {
    let mut c = Channel { regs: Vec::new() };
    assert_eq!(irq::write(&mut c, PON_PPID, 0x870, &[0x02]), Err(Error::NotAllowed));
    assert_eq!(irq::write(&mut c, 0x10f, 0x0f11, &[0x01]), Err(Error::NotAllowed));
    assert!(c.regs.is_empty());
}

#[test]
fn the_irq_owner_flag_names_the_ee() {
    assert_eq!(irq::irq_ee(1 << 24 | 0x008 << 8, 0), Some(0));
    assert_eq!(irq::irq_ee(0x008 << 8, 0), None);
    assert_eq!(irq::owner_acc_status_offset(0, 3), 12);
    assert_eq!(irq::owner_acc_status_offset(2, 1), 0x20004);
}
