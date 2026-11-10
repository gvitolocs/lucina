//! A simulated Book Go battery bus for tests: the SBS gauge and the BQ25710 with its 175 s watchdog, on a clock the
//! test advances, and a battery that fills while the charger charges it.
//!
//! The values are what DEBIAN.md read on the laptop: the boot firmware's MaxChargeVoltage `0x2238`, ChargeOption0
//! `0xe70e`, ChargerStatus `0x8000` (AC present) or `0x8400` (fast charge), the gauge asking for 8860 mV and 6916 mA and
//! showing `0x00c0` and no current while not charging. ChargeCurrent goes to 0 once 175 s pass without a write of
//! ChargeCurrent or MaxChargeVoltage (SLUSD20B 9.3.10.1).

extern crate std;

use core::fmt;
use std::vec::Vec;

use crate::{Reading, Smbus, bq25710, sbs};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read(u16, u8),
    Write(u16, u8, u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimError {
    Nack,
    Timeout,
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SimError::Nack => "address not acknowledged",
            SimError::Timeout => "timeout",
        })
    }
}

#[derive(Clone, Debug)]
pub struct Charger {
    pub charge_current: u16,
    pub max_charge_voltage: u16,
    pub option0: u16,
    pub manufacturer_id: u16,
    pub device_id: u16,
    pub ac: bool,
    /// The last write that fed the watchdog.
    pub last_kick_ms: u64,
    /// When the watchdog last set ChargeCurrent to 0.
    pub expired_at_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct Board {
    pub now_ms: u64,
    pub gauge: Reading,
    pub charger: Charger,
    pub capacity_mode_bit: bool,
    pub full_mah: u16,
    pub design_mah: u16,
    pub cycles: u16,
    /// Charge in µAh.
    pub charge_uah: u64,
    /// The gauge's own current, when a test sets it (else what the charger pushes).
    pub current_override: Option<i16>,
    /// Every transaction, in order.
    pub log: Vec<Op>,
    /// This register fails to read (NACK).
    pub fail_reads_of: Option<(u16, u8)>,
    /// This register fails to write (NACK).
    pub fail_writes_of: Option<(u16, u8)>,
    /// The next this many reads NACK.
    pub nack_next: u32,
    /// Every transaction times out.
    pub offline: bool,
    /// The chip ignores writes of MaxChargeVoltage.
    pub ignore_voltage_writes: bool,
    /// ChargeCurrent reads as this, whatever was written.
    pub current_reads_as: Option<u16>,
    /// The ADC: ADCOption, and the results of 0x23, 0x25, 0x26.
    pub adc_option: u16,
    pub adc_results: [u16; 3],
    /// Reads of ADCOption that still show ADC_START after a start.
    pub adc_busy_reads: u32,
    adc_busy_left: u32,
    pub adc_never_done: bool,
    /// A start of a conversion also sets this charger register, to test the check across it.
    pub adc_changes_reg: Option<(u8, u16)>,
    pub option1: u16,
    pub iin_host: u16,
}

impl Board {
    /// The laptop at DEBIAN.md's 18:09 test: 30 % of 5320 mAh, adapter in, not charging.
    pub fn new() -> Board {
        Board {
            now_ms: 0,
            gauge: Reading {
                status: 0x00c0,
                temp_dk: 2732 + 245,
                battery_mv: 7877,
                req_ma: 6916,
                req_mv: 8860,
            },
            charger: Charger {
                charge_current: 0,
                max_charge_voltage: 0x2238,
                option0: 0xe70e,
                manufacturer_id: bq25710::TI,
                device_id: bq25710::BQ25710,
                ac: true,
                last_kick_ms: 0,
                expired_at_ms: None,
            },
            capacity_mode_bit: false,
            full_mah: 5000,
            design_mah: 5320,
            cycles: 12,
            charge_uah: 1_500_000,
            current_override: None,
            log: Vec::new(),
            fail_reads_of: None,
            fail_writes_of: None,
            nack_next: 0,
            offline: false,
            ignore_voltage_writes: false,
            current_reads_as: None,
            adc_option: 0x2000,
            adc_results: [0xb900, 0x0700, 0x5d5b],
            adc_busy_reads: 1,
            adc_busy_left: 0,
            adc_never_done: false,
            adc_changes_reg: None,
            option1: 0x0200,
            iin_host: 0x3c00,
        }
    }

    /// Moves the clock: the watchdog fires when it is due, and the battery takes what the charger gives it.
    pub fn advance(&mut self, ms: u64) {
        self.now_ms += ms;
        let c = &mut self.charger;
        if c.charge_current != 0 && self.now_ms >= c.last_kick_ms + 175_000 {
            let fired = c.last_kick_ms + 175_000;
            // What was charged up to the firing is close enough: this model is for schedules, not coulombs.
            c.charge_current = 0;
            c.expired_at_ms = Some(fired);
        }
        if c.charge_current != 0 && c.ac {
            self.gauge.status &= !sbs::DISCHARGING;
            self.charge_uah += u64::from(c.charge_current) * ms / 3600;
        } else {
            self.gauge.status |= sbs::DISCHARGING;
        }
        if self.charge_uah >= u64::from(self.full_mah) * 1000 {
            self.charge_uah = u64::from(self.full_mah) * 1000;
            self.gauge.status |= sbs::FULLY_CHARGED;
            self.gauge.req_ma = 0;
        }
    }

    pub fn charging(&self) -> bool {
        self.charger.charge_current != 0
    }

    pub fn writes(&self) -> Vec<Op> {
        self.log
            .iter()
            .filter(|op| matches!(op, Op::Write(..)))
            .copied()
            .collect()
    }

    fn word(&self, addr: u16, reg: u8) -> Option<u16> {
        let g = &self.gauge;
        let c = &self.charger;
        Some(match (addr, reg) {
            (sbs::ADDR, sbs::BATTERY_STATUS) => g.status,
            (sbs::ADDR, sbs::TEMPERATURE) => g.temp_dk,
            (sbs::ADDR, sbs::VOLTAGE) => g.battery_mv,
            (sbs::ADDR, sbs::CHARGING_CURRENT) => g.req_ma,
            (sbs::ADDR, sbs::CHARGING_VOLTAGE) => g.req_mv,
            (sbs::ADDR, sbs::CURRENT) => match self.current_override {
                Some(current) => current as u16,
                None if c.ac => c.charge_current,
                None => 0,
            },
            (sbs::ADDR, sbs::RELATIVE_STATE_OF_CHARGE) => {
                (self.charge_uah / (u64::from(self.full_mah) * 10)) as u16
            }
            (sbs::ADDR, sbs::BATTERY_MODE) => {
                if self.capacity_mode_bit {
                    sbs::CAPACITY_MODE
                } else {
                    0
                }
            }
            (sbs::ADDR, sbs::REMAINING_CAPACITY) => (self.charge_uah / 1000) as u16,
            (sbs::ADDR, sbs::FULL_CHARGE_CAPACITY) => self.full_mah,
            (sbs::ADDR, sbs::DESIGN_CAPACITY) => self.design_mah,
            (sbs::ADDR, sbs::CYCLE_COUNT) => self.cycles,
            (sbs::ADDR, sbs::DESIGN_VOLTAGE) => 7720,
            (sbs::ADDR, sbs::SPECIFICATION_INFO) => 0x0031,
            (sbs::ADDR, sbs::MANUFACTURE_DATE) => 0x5000,
            // What the firmware leaves, by Samsung's table.
            (bq25710::ADDR, 0x30) => self.option1,
            (bq25710::ADDR, 0x35) => self.adc_option,
            (bq25710::ADDR, 0x23) => self.adc_results[0],
            (bq25710::ADDR, 0x25) => self.adc_results[1],
            (bq25710::ADDR, 0x26) => self.adc_results[2],
            (bq25710::ADDR, 0x31) => 0x02b7,
            (bq25710::ADDR, 0x32) => 0x0031,
            (bq25710::ADDR, 0x33) => 0x126d,
            (bq25710::ADDR, 0x34) => 0x312c,
            (bq25710::ADDR, 0x3d) => 0x0500,
            (bq25710::ADDR, 0x3e) => 0x1800,
            (bq25710::ADDR, 0x3f) => self.iin_host,
            (bq25710::ADDR, 0x22) => 0x4100,
            (bq25710::ADDR, 0x21) => 0x0000,
            (bq25710::ADDR, bq25710::CHARGE_OPTION_0) => c.option0,
            (bq25710::ADDR, bq25710::CHARGE_CURRENT) => {
                self.current_reads_as.unwrap_or(c.charge_current)
            }
            (bq25710::ADDR, bq25710::MAX_CHARGE_VOLTAGE) => c.max_charge_voltage,
            (bq25710::ADDR, bq25710::CHARGER_STATUS) => {
                let ac = if c.ac { 0x8000 } else { 0 };
                if c.charge_current != 0 {
                    ac | 0x0400
                } else {
                    ac
                }
            }
            (bq25710::ADDR, bq25710::MANUFACTURER_ID) => c.manufacturer_id,
            (bq25710::ADDR, bq25710::DEVICE_ID) => c.device_id,
            _ => return None,
        })
    }
}

impl Smbus for Board {
    type Error = SimError;

    fn read_word(&mut self, addr: u16, reg: u8) -> Result<u16, SimError> {
        self.log.push(Op::Read(addr, reg));
        if self.offline {
            return Err(SimError::Timeout);
        }
        if self.nack_next > 0 {
            self.nack_next -= 1;
            return Err(SimError::Nack);
        }
        if self.fail_reads_of == Some((addr, reg)) {
            return Err(SimError::Nack);
        }
        // A one-shot conversion is done after a few reads; ADC_START then clears.
        if (addr, reg) == (bq25710::ADDR, 0x35) && self.adc_option & 0x4000 != 0 && !self.adc_never_done {
            if self.adc_busy_left == 0 {
                self.adc_option &= !0x4000;
            } else {
                self.adc_busy_left -= 1;
            }
        }
        self.word(addr, reg).ok_or(SimError::Nack)
    }

    fn write_word(&mut self, addr: u16, reg: u8, value: u16) -> Result<(), SimError> {
        self.log.push(Op::Write(addr, reg, value));
        if self.offline {
            return Err(SimError::Timeout);
        }
        if self.fail_writes_of == Some((addr, reg)) {
            return Err(SimError::Nack);
        }
        // ADCOption: never a watchdog write (SLUSD20B 9.3.10.1).
        if (addr, reg) == (bq25710::ADDR, 0x35) {
            self.adc_option = value;
            if value & 0x4000 != 0 {
                self.adc_busy_left = self.adc_busy_reads;
                match self.adc_changes_reg {
                    Some((bq25710::CHARGE_CURRENT, v)) => self.charger.charge_current = v,
                    Some((bq25710::MAX_CHARGE_VOLTAGE, v)) => self.charger.max_charge_voltage = v,
                    Some((0x3f, v)) => self.iin_host = v,
                    _ => {}
                }
            }
            return Ok(());
        }
        let now = self.now_ms;
        let c = &mut self.charger;
        match (addr, reg) {
            (bq25710::ADDR, bq25710::CHARGE_CURRENT) => {
                c.charge_current = if c.ac { value & 0x1fc0 } else { 0 };
            }
            (bq25710::ADDR, bq25710::MAX_CHARGE_VOLTAGE) => {
                if !self.ignore_voltage_writes {
                    c.max_charge_voltage = value & 0x7ff8;
                }
            }
            _ => return Err(SimError::Nack),
        }
        c.last_kick_ms = now;
        Ok(())
    }

    fn may_retry(error: &SimError) -> bool {
        *error == SimError::Nack
    }
}
