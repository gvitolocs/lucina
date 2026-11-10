//! The battery and charger as the MAX77696 reports them, for `/sys/class/power_supply`: what Linux's `max77696-battery` and
//! `max77696-charger` show, under the same names and in the same units.
//!
//! This only reads: [`Bus`] has no way to write. Linux also loads a model into the gauge and sets up the charger, and those
//! stay in the PMIC across a kexec. Doing that here is for when this kernel can start the device by itself.

use core::fmt;

use crate::Error;

/// The gauge's page on the bus, of 16-bit registers, low byte first. The charger's are on the PMIC's own.
pub const GAUGE_ADDR: u8 = 0x34;

/// `shunt-resistor-micro-ohms` in the board's device tree.
const RSNS_UOHM: i64 = 10_000;

// The gauge.
const FG_STATUS: u8 = 0x00;
const FG_REMCAPREP: u8 = 0x05;
const FG_SOCREP: u8 = 0x06;
const FG_TEMP: u8 = 0x08;
const FG_VCELL: u8 = 0x09;
const FG_CURRENT: u8 = 0x0a;
const FG_AVGCURRENT: u8 = 0x0b;
const FG_FULLCAP: u8 = 0x10;
const FG_TTE: u8 = 0x11;
const FG_CYCLES: u8 = 0x17;
const FG_DESIGNCAP: u8 = 0x18;
const FG_AVGVCELL: u8 = 0x19;
const FG_QH: u8 = 0x4d;
const FG_QL: u8 = 0x4e;

const FG_STATUS_POR: u16 = 1 << 1;
const FG_STATUS_BST: u16 = 1 << 3;

/// The gauge's cycle counter starts here, and counts in hundredths of a cycle.
const CYCLES_OFFSET: u16 = 0x60;
const CYCLES_PER_CYCLE: u16 = 100;
const TTE_INVALID: u16 = 0xffff;
const CRIT_CAPACITY: u32 = 4;
const CRIT_VOLTAGE_UV: u32 = 3_460_000;
const LOW_CAPACITY: u32 = 10;

// The charger.
const CHG_INT_OK: u8 = 0x0a;
const CHG_DTLS_01: u8 = 0x0c;
const CHG_INT_CHGINA: u8 = 1 << 6;

const DTLS_PREQUAL: u8 = 0x0;
const DTLS_FAST_CC: u8 = 0x1;
const DTLS_FAST_CV: u8 = 0x2;
const DTLS_TOP_OFF: u8 = 0x3;
const DTLS_DONE: u8 = 0x4;
const DTLS_HIGH_TEMP: u8 = 0x5;
const DTLS_TIMER_FAULT: u8 = 0x6;
const DTLS_THM_SUSPEND: u8 = 0x7;
const DTLS_OFF: u8 = 0x8;

/// Reading the chip. There is no writing.
pub trait Bus {
    fn gauge(&mut self, reg: u8) -> Result<u16, Error>;
    fn charger(&mut self, reg: u8) -> Result<u8, Error>;
}

#[derive(Debug, PartialEq, Eq)]
pub enum AttrError {
    /// The chip did not answer.
    Bus,
    /// The chip has no such value now.
    NoData,
    /// Not one of the attributes.
    Unknown,
    Format,
}

impl From<Error> for AttrError {
    fn from(_: Error) -> Self {
        AttrError::Bus
    }
}

impl From<fmt::Error> for AttrError {
    fn from(_: fmt::Error) -> Self {
        AttrError::Format
    }
}

pub const BATTERY_NAME: &str = "max77696-battery";
pub const CHARGER_NAME: &str = "max77696-charger";

fn capacity<B: Bus>(bus: &mut B) -> Result<u32, Error> {
    Ok(((bus.gauge(FG_SOCREP)? >> 8) as u32).min(100))
}

fn microvolts(raw: u16) -> u32 {
    (raw as u32) * 625 / 8
}

fn microamps(raw: u16) -> i64 {
    (raw as i16 as i64) * 1_562_500 / RSNS_UOHM
}

fn microamp_hours(raw: u16) -> i64 {
    (raw as i64) * 5_000_000 / RSNS_UOHM
}

/// The coulomb counter, QH over QL: what `charge_now` holds is filtered and moves in steps of 500 uAh, this moves with the
/// current itself, so it is what a drain is measured from. One bit of QH is one of `charge_now`.
fn charge_counter<B: Bus>(bus: &mut B) -> Result<i64, Error> {
    let mut high = bus.gauge(FG_QH)?;
    loop {
        let low = bus.gauge(FG_QL)?;
        let again = bus.gauge(FG_QH)?;
        if again == high {
            return Ok((((high as u32) << 16 | low as u32) as i32 as i64) * 5_000_000 / RSNS_UOHM / 65_536);
        }
        high = again;
    }
}

fn charger_status<B: Bus>(bus: &mut B) -> Result<&'static str, Error> {
    if bus.charger(CHG_INT_OK)? & CHG_INT_CHGINA == 0 {
        return Ok("Discharging");
    }
    Ok(match bus.charger(CHG_DTLS_01)? & 0xf {
        DTLS_PREQUAL | DTLS_FAST_CC | DTLS_FAST_CV | DTLS_TOP_OFF | DTLS_HIGH_TEMP => "Charging",
        DTLS_DONE => "Full",
        DTLS_TIMER_FAULT | DTLS_THM_SUSPEND | DTLS_OFF => "Not charging",
        _ => "Unknown",
    })
}

fn charge_type<B: Bus>(bus: &mut B) -> Result<&'static str, Error> {
    if bus.charger(CHG_INT_OK)? & CHG_INT_CHGINA == 0 {
        return Ok("N/A");
    }
    Ok(match bus.charger(CHG_DTLS_01)? & 0xf {
        DTLS_PREQUAL | DTLS_TOP_OFF => "Trickle",
        DTLS_FAST_CC | DTLS_FAST_CV | DTLS_HIGH_TEMP => "Fast",
        DTLS_DONE | DTLS_TIMER_FAULT | DTLS_THM_SUSPEND | DTLS_OFF => "N/A",
        _ => "Unknown",
    })
}

/// An attribute of the battery's directory, without its line end.
pub fn battery_attr<B: Bus>(bus: &mut B, name: &str, w: &mut dyn fmt::Write) -> Result<(), AttrError> {
    match name {
        "type" => write!(w, "Battery")?,
        "status" => {
            let status = charger_status(bus)?;
            let status = if status == "Charging" && capacity(bus)? >= 100 {
                "Full"
            } else {
                status
            };
            write!(w, "{status}")?;
        }
        "present" => write!(w, "{}", (bus.gauge(FG_STATUS)? & FG_STATUS_BST == 0) as u8)?,
        "health" => {
            let por = bus.gauge(FG_STATUS)? & FG_STATUS_POR != 0;
            write!(w, "{}", if por { "Calibration required" } else { "Good" })?;
        }
        "cycle_count" => {
            let raw = bus.gauge(FG_CYCLES)?.max(CYCLES_OFFSET);
            write!(w, "{}", (raw - CYCLES_OFFSET) / CYCLES_PER_CYCLE)?;
        }
        "voltage_now" => write!(w, "{}", microvolts(bus.gauge(FG_VCELL)?))?,
        "voltage_avg" => write!(w, "{}", microvolts(bus.gauge(FG_AVGVCELL)?))?,
        "current_now" => write!(w, "{}", microamps(bus.gauge(FG_CURRENT)?))?,
        "current_avg" => write!(w, "{}", microamps(bus.gauge(FG_AVGCURRENT)?))?,
        "charge_full_design" => write!(w, "{}", microamp_hours(bus.gauge(FG_DESIGNCAP)?))?,
        "charge_full" => write!(w, "{}", microamp_hours(bus.gauge(FG_FULLCAP)?))?,
        "charge_now" => write!(w, "{}", microamp_hours(bus.gauge(FG_REMCAPREP)?))?,
        "charge_counter" => write!(w, "{}", charge_counter(bus)?)?,
        "capacity" => write!(w, "{}", capacity(bus)?)?,
        "capacity_level" => {
            let (capacity, uv) = (capacity(bus)?, microvolts(bus.gauge(FG_AVGVCELL)?));
            let level = if capacity <= CRIT_CAPACITY || uv <= CRIT_VOLTAGE_UV {
                "Critical"
            } else if capacity <= LOW_CAPACITY {
                "Low"
            } else if capacity >= 100 {
                "Full"
            } else {
                "Normal"
            };
            write!(w, "{level}")?;
        }
        "temp" => write!(w, "{}", (bus.gauge(FG_TEMP)? as i16 as i32) * 10 / 256)?,
        "time_to_empty_avg" => {
            let raw = bus.gauge(FG_TTE)?;
            if raw == TTE_INVALID {
                return Err(AttrError::NoData);
            }
            write!(w, "{}", (raw as u32) * 5625 / 1000)?;
        }
        _ => return Err(AttrError::Unknown),
    }
    Ok(())
}

/// An attribute of the charger's directory, without its line end.
pub fn charger_attr<B: Bus>(bus: &mut B, name: &str, w: &mut dyn fmt::Write) -> Result<(), AttrError> {
    match name {
        "type" => write!(w, "USB")?,
        "online" => write!(w, "{}", (bus.charger(CHG_INT_OK)? & CHG_INT_CHGINA != 0) as u8)?,
        "status" => write!(w, "{}", charger_status(bus)?)?,
        "charge_type" => write!(w, "{}", charge_type(bus)?)?,
        _ => return Err(AttrError::Unknown),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{collections::HashMap, string::String};

    use super::*;

    #[derive(Default)]
    struct Chip {
        gauge: HashMap<u8, u16>,
        charger: HashMap<u8, u8>,
        dead: bool,
    }

    impl Bus for Chip {
        fn gauge(&mut self, reg: u8) -> Result<u16, Error> {
            if self.dead {
                return Err(Error::I2cTimeout);
            }
            Ok(*self.gauge.get(&reg).unwrap_or(&0))
        }
        fn charger(&mut self, reg: u8) -> Result<u8, Error> {
            if self.dead {
                return Err(Error::I2cTimeout);
            }
            Ok(*self.charger.get(&reg).unwrap_or(&0))
        }
    }

    fn bat(chip: &mut Chip, name: &str) -> String {
        let mut s = String::new();
        battery_attr(chip, name, &mut s).unwrap();
        s
    }

    fn chg(chip: &mut Chip, name: &str) -> String {
        let mut s = String::new();
        charger_attr(chip, name, &mut s).unwrap();
        s
    }

    #[test]
    fn units_are_linuxs() {
        let mut chip = Chip::default();
        // 3.9 V: 49920 LSB of 78.125 uV.
        chip.gauge.insert(FG_VCELL, 49920);
        assert_eq!(bat(&mut chip, "voltage_now"), "3900000");
        // 57.5 %, rounded down.
        chip.gauge.insert(FG_SOCREP, (57 << 8) | 128);
        assert_eq!(bat(&mut chip, "capacity"), "57");
        // 23.5 degrees, in tenths.
        chip.gauge.insert(FG_TEMP, (23 << 8) | 128);
        assert_eq!(bat(&mut chip, "temp"), "235");
        // -3 degrees: a negative number.
        chip.gauge.insert(FG_TEMP, (-3i16 << 8) as u16);
        assert_eq!(bat(&mut chip, "temp"), "-30");
        // 156.25 uA a bit at 10 mOhm, and drawing is negative.
        chip.gauge.insert(FG_CURRENT, 64);
        assert_eq!(bat(&mut chip, "current_now"), "10000");
        chip.gauge.insert(FG_CURRENT, (-64i16) as u16);
        assert_eq!(bat(&mut chip, "current_now"), "-10000");
        // 500 uAh a bit at 10 mOhm.
        chip.gauge.insert(FG_FULLCAP, 2000);
        assert_eq!(bat(&mut chip, "charge_full"), "1000000");
        // QH.QL: 2000 bits of 500 uAh and half of one more.
        chip.gauge.insert(FG_QH, 2000);
        chip.gauge.insert(FG_QL, 0x8000);
        assert_eq!(bat(&mut chip, "charge_counter"), "1000250");
        // The counter runs below zero when the battery has given more than it had at its start.
        chip.gauge.insert(FG_QH, 0xffff);
        chip.gauge.insert(FG_QL, 0);
        assert_eq!(bat(&mut chip, "charge_counter"), "-500");
        assert_eq!(bat(&mut chip, "type"), "Battery");
    }

    #[test]
    fn capacity_stops_at_100() {
        let mut chip = Chip::default();
        chip.gauge.insert(FG_SOCREP, 103 << 8);
        assert_eq!(bat(&mut chip, "capacity"), "100");
    }

    #[test]
    fn the_battery_is_there_unless_the_gauge_says_otherwise() {
        let mut chip = Chip::default();
        assert_eq!(bat(&mut chip, "present"), "1");
        chip.gauge.insert(FG_STATUS, FG_STATUS_BST);
        assert_eq!(bat(&mut chip, "present"), "0");
    }

    #[test]
    fn a_gauge_that_lost_its_model_says_so() {
        let mut chip = Chip::default();
        assert_eq!(bat(&mut chip, "health"), "Good");
        chip.gauge.insert(FG_STATUS, FG_STATUS_POR);
        assert_eq!(bat(&mut chip, "health"), "Calibration required");
    }

    #[test]
    fn cycles_start_at_the_offset() {
        let mut chip = Chip::default();
        assert_eq!(bat(&mut chip, "cycle_count"), "0");
        chip.gauge.insert(FG_CYCLES, CYCLES_OFFSET + 250);
        assert_eq!(bat(&mut chip, "cycle_count"), "2");
    }

    #[test]
    fn no_time_to_empty_is_no_data() {
        let mut chip = Chip::default();
        chip.gauge.insert(FG_TTE, TTE_INVALID);
        let mut s = String::new();
        assert_eq!(battery_attr(&mut chip, "time_to_empty_avg", &mut s), Err(AttrError::NoData));
        chip.gauge.insert(FG_TTE, 640);
        assert_eq!(bat(&mut chip, "time_to_empty_avg"), "3600");
    }

    #[test]
    fn levels() {
        let mut chip = Chip::default();
        chip.gauge.insert(FG_AVGVCELL, 49920);
        for (capacity, level) in [(4, "Critical"), (5, "Low"), (10, "Low"), (11, "Normal"), (100, "Full")] {
            chip.gauge.insert(FG_SOCREP, capacity << 8);
            assert_eq!(bat(&mut chip, "capacity_level"), level, "{capacity}");
        }
        // A low voltage is critical at any capacity.
        chip.gauge.insert(FG_SOCREP, 50 << 8);
        chip.gauge.insert(FG_AVGVCELL, 44288);
        assert_eq!(bat(&mut chip, "capacity_level"), "Critical");
    }

    #[test]
    fn the_status_comes_from_the_charger() {
        let mut chip = Chip::default();
        assert_eq!(bat(&mut chip, "status"), "Discharging");
        assert_eq!(chg(&mut chip, "online"), "0");
        assert_eq!(chg(&mut chip, "charge_type"), "N/A");

        chip.charger.insert(CHG_INT_OK, CHG_INT_CHGINA);
        assert_eq!(chg(&mut chip, "online"), "1");
        for (dtls, status, kind) in [
            (DTLS_PREQUAL, "Charging", "Trickle"),
            (DTLS_FAST_CC, "Charging", "Fast"),
            (DTLS_FAST_CV, "Charging", "Fast"),
            (DTLS_TOP_OFF, "Charging", "Trickle"),
            (DTLS_HIGH_TEMP, "Charging", "Fast"),
            (DTLS_DONE, "Full", "N/A"),
            (DTLS_TIMER_FAULT, "Not charging", "N/A"),
            (DTLS_THM_SUSPEND, "Not charging", "N/A"),
            (DTLS_OFF, "Not charging", "N/A"),
            (0xc, "Unknown", "Unknown"),
        ] {
            // The upper bits are the battery's details.
            chip.charger.insert(CHG_DTLS_01, 0x50 | dtls);
            assert_eq!(chg(&mut chip, "status"), status, "{dtls}");
            assert_eq!(bat(&mut chip, "status"), status, "{dtls}");
            assert_eq!(chg(&mut chip, "charge_type"), kind, "{dtls}");
        }
    }

    #[test]
    fn still_charging_at_100_is_full() {
        let mut chip = Chip::default();
        chip.charger.insert(CHG_INT_OK, CHG_INT_CHGINA);
        chip.charger.insert(CHG_DTLS_01, DTLS_TOP_OFF);
        chip.gauge.insert(FG_SOCREP, 100 << 8);
        assert_eq!(bat(&mut chip, "status"), "Full");
        assert_eq!(chg(&mut chip, "status"), "Charging");
    }

    #[test]
    fn a_silent_chip_and_a_wrong_name_are_errors() {
        let mut chip = Chip { dead: true, ..Chip::default() };
        let mut s = String::new();
        assert_eq!(battery_attr(&mut chip, "capacity", &mut s), Err(AttrError::Bus));
        assert_eq!(charger_attr(&mut chip, "online", &mut s), Err(AttrError::Bus));
        assert_eq!(battery_attr(&mut chip, "nonsense", &mut s), Err(AttrError::Unknown));
        assert_eq!(charger_attr(&mut chip, "nonsense", &mut s), Err(AttrError::Unknown));
    }
}
