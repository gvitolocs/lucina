//! The Samsung Galaxy Book Go's charge logic, shared by the kernel (`device::qcom_charge`, patch 0600) and the Linux
//! path's daemon (`userspace/crates/lucina-chargerd`).
//!
//! The charger is a TI BQ25710 (SMBus `0x09`, ManufacturerID `0x0040`, DeviceID `0x0089`) next to the battery's Smart
//! Battery gauge (`0x0b`), both on the SC7180's `i2c@890000`. Its watchdog sets ChargeCurrent (`0x14`) to 0 unless the
//! host writes ChargeCurrent or MaxChargeVoltage within 175 s (SLUSD20B 9.3.10.1). The host relays the gauge's own
//! requests to it every [`PERIOD`].
//!
//! The source of truth is xbill9's `gbgo-charge` (linux-arm-adventures `34f76f5`, `galaxy-book-go/scripts/gbgo-charge`
//! and DEBIAN.md "Charging"):
//!
//! * voltage: min(gauge request, [`VMAX_MV`]), rounded down to the register's 8 mV step; a request outside
//!   [`V_REQ_MIN_MV`]..=[`V_REQ_MAX_MV`] stops.
//! * current: min(gauge request, cap), rounded down to 64 mA; a request of 0 or `0xffff` stops.
//! * stop (ChargeCurrent = 0) when the gauge reports FULLY_CHARGED or a temperature outside 0..=45 °C.
//!
//! Added, all on the safe side (recorded in `docs/research/integration-decisions.md`): the temperature window is
//! exact (the original truncated, so 45.9 °C and -0.9 °C passed), and the gauge's own over-charge, terminate-charge
//! and over-temperature alarms and a battery voltage no 2S pack can have also stop. [`control::Controller`] adds the
//! rest: chip IDs on every pass, the watchdog must be armed, and read-backs.
//!
//! [`decide`] is pure, [`control`] is bus-generic and tested against a simulated gauge and charger (`src/sim.rs`),
//! [`supply`] is the `/sys/class/power_supply` view, [`geni`] puts it all on a `lucina-geni` I2C master.

#![no_std]
#![forbid(unsafe_code)]

pub mod adc;
pub mod control;
pub mod geni;
pub mod pdic;
pub mod pdic_rules;
pub mod rules;
#[cfg(test)]
mod sim;
pub mod supply;
#[cfg(test)]
mod tests;

use core::fmt;
use core::time::Duration;

pub use rules::{
    Action, BATTERY_MAX_MV, BATTERY_MIN_MV, Band, Blocked, DEFAULT_CAP_MA, DESIGN_MAX_MA, Decision, Facts, Plan, Reading, Stop,
    TEMP_MAX_DK, TEMP_MIN_DK, V_REQ_MAX_MV, V_REQ_MIN_MV, VMAX_MV, decide, next_band, plan,
};

/// Keep-alive period: one pass, so one ChargeCurrent write, per minute.
pub const PERIOD: Duration = Duration::from_secs(60);
/// The temperature window in whole degrees, as the Linux path names it. The rule itself is in 0.1 K: [`TEMP_MIN_DK`], [`TEMP_MAX_DK`].
pub const T_MIN_C: i32 = 0;
pub const T_MAX_C: i32 = 45;

const _: () = assert!(PERIOD.as_secs() < bq25710::WATCHDOG.as_secs());

/// The Smart Battery gauge (SBS 1.1 word registers).
pub mod sbs {
    pub const ADDR: u16 = 0x0b;
    /// bit 15: capacities are in 10 mWh, not mAh
    pub const BATTERY_MODE: u8 = 0x03;
    /// 0.1 K
    pub const TEMPERATURE: u8 = 0x08;
    /// mV
    pub const VOLTAGE: u8 = 0x09;
    /// mA, signed, positive while charging
    pub const CURRENT: u8 = 0x0a;
    /// %
    pub const RELATIVE_STATE_OF_CHARGE: u8 = 0x0d;
    pub const REMAINING_CAPACITY: u8 = 0x0f;
    pub const FULL_CHARGE_CAPACITY: u8 = 0x10;
    /// mA the battery asks for
    pub const CHARGING_CURRENT: u8 = 0x14;
    /// mV the battery asks for
    pub const CHARGING_VOLTAGE: u8 = 0x15;
    pub const BATTERY_STATUS: u8 = 0x16;
    pub const CYCLE_COUNT: u8 = 0x17;
    pub const DESIGN_CAPACITY: u8 = 0x18;
    /// BatteryMode bit: capacities in 10 mWh
    pub const CAPACITY_MODE: u16 = 0x8000;
    pub use crate::rules::{CHARGE_ALARMS, FULLY_CHARGED};
    /// BatteryStatus bit
    pub const DISCHARGING: u16 = 0x40;
    /// mV
    pub const DESIGN_VOLTAGE: u8 = 0x19;
    pub const SPECIFICATION_INFO: u8 = 0x1a;
    pub const MANUFACTURE_DATE: u8 = 0x1b;
    /// Block reads: a length byte, then text.
    pub const MANUFACTURER_NAME: u8 = 0x20;
    pub const DEVICE_NAME: u8 = 0x21;
    pub const DEVICE_CHEMISTRY: u8 = 0x22;
}

/// The TI BQ25710 charger (SLUSD20B).
pub mod bq25710 {
    use core::time::Duration;

    pub const ADDR: u16 = 0x09;
    /// bits 14:13 are the watchdog period
    pub const CHARGE_OPTION_0: u8 = 0x12;
    /// bits 12:6, 64 mA per step
    pub const CHARGE_CURRENT: u8 = 0x14;
    /// bits 14:3, 8 mV per step
    pub const MAX_CHARGE_VOLTAGE: u8 = 0x15;
    pub const CHARGER_STATUS: u8 = 0x20;
    /// bits 14:8, 50 mA per step: the input current limit (EmuEC writes it from the PD contract)
    pub const IIN_HOST: u8 = 0x3f;
    pub const MANUFACTURER_ID: u8 = 0xfe;
    pub const DEVICE_ID: u8 = 0xff;
    pub const TI: u16 = 0x0040;
    pub const BQ25710: u16 = 0x0089;
    /// ChargeOption0 bits 14:13 = 11b, the reset value
    pub const WATCHDOG: Duration = Duration::from_secs(175);
    pub const WATCHDOG_MASK: u16 = 0x6000;
    /// ChargerStatus bit 15: the adapter is present
    pub const AC_STAT: u16 = 0x8000;
    /// The largest ChargeCurrent the register holds (0x1fc0).
    pub const MAX_CURRENT_MA: u16 = 8128;

    pub use crate::rules::{current_reg, voltage_reg};

    /// The registers nothing here ever writes, read to see what the firmware left: Samsung's driver sets them, and Linux's reads them.
    pub const READ_ONLY: [(u8, &str); 10] = [
        (0x30, "ChargeOption1"),
        (0x31, "ChargeOption2"),
        (0x32, "ChargeOption3"),
        (0x33, "ProchotOption0"),
        (0x34, "ProchotOption1"),
        (0x3d, "InputVoltage"),
        (0x3e, "MinSystemVoltage"),
        (0x3f, "IIN_HOST"),
        (0x22, "IIN_DPM"),
        (0x21, "ProchotStatus"),
    ];
}

/// Word reads and writes on one SMBus adapter (SMBus read/write word data, little-endian, no PEC: what
/// `i2cget -f -y B addr reg w` does).
pub trait Smbus {
    type Error;

    fn read_word(&mut self, addr: u16, reg: u8) -> Result<u16, Self::Error>;
    fn write_word(&mut self, addr: u16, reg: u8, value: u16) -> Result<(), Self::Error>;

    /// An SMBus block read: the length byte goes first in `buf`. `None`: this bus has no such read. For a name in the log only.
    fn read_block(&mut self, _addr: u16, _reg: u8, _buf: &mut [u8]) -> Option<Result<(), Self::Error>> {
        None
    }

    /// Whether a read that failed like this may simply be tried again. A smart battery gauge NACKs now and then while
    /// it is busy (DEBIAN.md saw `-6` from `sbs-battery` in the first minute after boot).
    fn may_retry(_error: &Self::Error) -> bool {
        false
    }
}

impl Reading {
    /// Whole °C, truncated toward zero, for messages.
    pub fn temp_c(&self) -> i32 {
        (i32::from(self.temp_dk) - 2732) / 10
    }

    /// Reads the gauge through `read_word`, stopping at the first failure.
    pub fn read<E>(mut read_word: impl FnMut(u8) -> Result<u16, E>) -> Result<Reading, E> {
        Ok(Reading {
            status: read_word(sbs::BATTERY_STATUS)?,
            temp_dk: read_word(sbs::TEMPERATURE)?,
            battery_mv: read_word(sbs::VOLTAGE)?,
            req_ma: read_word(sbs::CHARGING_CURRENT)?,
            req_mv: read_word(sbs::CHARGING_VOLTAGE)?,
        })
    }
}

impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stop::Temperature(dk) => write!(f, "temperature {} C", (i32::from(*dk) - 2732) / 10),
            Stop::Alarm(status) => write!(f, "gauge alarm in status 0x{status:04x}"),
            Stop::FullyCharged => f.write_str("fully charged"),
            Stop::Current(ma) => write!(f, "gauge requests {ma} mA"),
            Stop::Voltage(mv) => write!(f, "gauge requests {mv} mV"),
            Stop::BatteryVoltage(mv) => write!(f, "battery reads {mv} mV"),
            Stop::Swelling(mv) => write!(f, "too hot or cold for {mv} mV"),
        }
    }
}

/// `lucina.charge=`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Reads and prints everything, including what it would write. Writes nothing to the charger.
    Report,
    /// Does the writes. The default.
    On,
    /// Leaves the bus alone.
    Off,
}

impl Mode {
    /// The mode from the kernel's arguments, up to `--`. Charging is on unless something says otherwise (the owner's
    /// go of 2026-11-05, decision dc-f1ce4611f316, after the 512 and 2048 mA runs on the laptop): any `off` wins, then
    /// a `report` or a value that is not understood, which only reads.
    pub fn from_args<'a>(args: impl Iterator<Item = &'a str>) -> Mode {
        let mut report = false;
        for value in args
            .take_while(|arg| *arg != "--")
            .filter_map(|arg| arg.strip_prefix("lucina.charge="))
        {
            match value {
                "off" => return Mode::Off,
                "on" => {}
                _ => report = true,
            }
        }
        if report { Mode::Report } else { Mode::On }
    }
}

/// What the kernel's arguments set. The supervised test needs all of these: a lower cap, an end, and a fast log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    pub mode: Mode,
    /// `lucina.charge.cap=<mA>`: lower than [`DEFAULT_CAP_MA`] only.
    pub cap_ma: u16,
    /// `lucina.charge.window=<seconds>`: no setpoint goes out after this long since the first one.
    pub window_ms: Option<u64>,
    /// `lucina.charge.trace=<ms>`: read this often, and print one line each time. Without it, every [`control::READ_PERIOD_MS`].
    pub trace_ms: Option<u64>,
    /// `lucina.charge.adc=<ms>|off`: how often the charger's ADC converts, while the adapter is in. Default every
    /// [`control::ADC_PERIOD_MS`]. It writes ADCOption, the one write `report` makes to the charger: it starts a conversion and
    /// changes no charging ([`adc`]).
    pub adc_ms: Option<u64>,
    /// The PD contract's input limit written to IIN_HOST as EmuEC does ([`control::Controller`]'s input limit): on by default,
    /// `lucina.charge.iin=off` (or `report`) only says the value.
    pub iin_on: bool,
}

impl Options {
    pub fn from_args<'a>(args: impl Iterator<Item = &'a str> + Clone) -> Options {
        let raw = |name: &str| {
            args.clone()
                .take_while(|arg| *arg != "--")
                .filter_map(|arg| arg.strip_prefix(name)?.strip_prefix('='))
                .last()
        };
        let value = |name: &str| raw(name).and_then(|v| v.parse::<u64>().ok());
        Options {
            mode: Mode::from_args(args.clone()),
            cap_ma: value("lucina.charge.cap").map_or(DEFAULT_CAP_MA, |ma| ma.min(u64::from(DEFAULT_CAP_MA)) as u16),
            window_ms: value("lucina.charge.window").map(|s| s.saturating_mul(1000)),
            trace_ms: value("lucina.charge.trace").map(|ms| ms.clamp(200, control::READ_PERIOD_MS)),
            adc_ms: if raw("lucina.charge.adc") == Some("off") {
                None
            } else {
                Some(value("lucina.charge.adc").map_or(control::ADC_PERIOD_MS, |ms| ms.clamp(200, 60_000)))
            },
            // On since the laptop run of 2026-11-08 (IIN_HOST 0x3c00 read back after an attach; Lucina dc-1e6753f3881f: default
            // on if it holds): `off` or `report` only says it.
            iin_on: !matches!(raw("lucina.charge.iin"), Some("off") | Some("report")),
        }
    }
}
