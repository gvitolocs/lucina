//! The battery and the adapter as `/sys/class/power_supply` shows them, in Linux's units and spelling
//! (`drivers/power/supply/sbs-battery.c` and `power_supply_sysfs.c`, v6.12).

use core::fmt;

use crate::sbs;

/// Linux's name for this gauge on this laptop (DEBIAN.md: `sbs-4-000b`).
pub const BATTERY_NAME: &str = "sbs-4-000b";
pub const ADAPTER_NAME: &str = "bq25710-charger";

/// The attributes of the battery, for the kernel's sysfs table.
pub const BATTERY_ATTRS: [&str; 12] = [
    "type",
    "status",
    "present",
    "technology",
    "capacity",
    "voltage_now",
    "current_now",
    "temp",
    "charge_now",
    "charge_full",
    "charge_full_design",
    "cycle_count",
];
/// The attributes of the adapter.
pub const ADAPTER_ATTRS: [&str; 6] = ["type", "online", "voltage_now", "current_now", "power_now", "lucina_adc"];

/// The latest reading. `None`: not read, as a Linux property the driver could not read, whose file fails to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub battery_status: Option<u16>,
    pub capacity: Option<u16>,
    pub voltage_mv: Option<u16>,
    pub current_ma: Option<i16>,
    pub temp_dk: Option<u16>,
    pub cycle_count: Option<u16>,
    pub charge_now_mah: Option<u16>,
    pub charge_full_mah: Option<u16>,
    pub charge_full_design_mah: Option<u16>,
    /// The adapter is present, from the charger's AC_STAT.
    pub ac_online: Option<bool>,
    /// The PD controller's cable state (attached as sink, or detached), as soon as it reports it: before AC_STAT follows.
    pub cable: Option<bool>,
    /// How long the adapter has been in without a positive battery current, in ms (the status says Not charging after
    /// [`crate::rules::DRY_LIMIT_MS`]).
    pub dry_ms: Option<u64>,
    /// The charger's last ADC conversion and its monotonic time in ms, while the adapter is in.
    pub adc: Option<(u64, crate::adc::Adc)>,
}

/// Why an attribute could not be shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrError {
    NoData,
    UnknownAttr,
    Format,
}

impl From<fmt::Error> for AttrError {
    fn from(_: fmt::Error) -> Self {
        AttrError::Format
    }
}

impl Snapshot {
    pub const EMPTY: Snapshot = Snapshot {
        battery_status: None,
        capacity: None,
        voltage_mv: None,
        current_ma: None,
        temp_dk: None,
        cycle_count: None,
        charge_now_mah: None,
        charge_full_mah: None,
        charge_full_design_mah: None,
        ac_online: None,
        cable: None,
        dry_ms: None,
        adc: None,
    };

    /// Whether the adapter counts as in ([`crate::rules::cable_online`]): the cable as the PD controller sees it, else AC_STAT.
    pub fn online(&self) -> Option<bool> {
        crate::rules::cable_online(self.cable, self.ac_online)
    }

    /// The gauge answered.
    pub fn present(&self) -> bool {
        self.battery_status.is_some()
    }

    /// `sbs-battery`'s status: the BatteryStatus bits, then `sbs_status_correct`: no current and not full is "Not
    /// charging" (the discharging bit is also set on an adapter while not charging; DEBIAN.md saw `0x00c0`), and a
    /// current while full is charging or discharging.
    pub fn status_text(&self) -> Option<&'static str> {
        let status = self.battery_status?;
        let full = status & sbs::FULLY_CHARGED != 0;
        // With the charger's AC_STAT known the status follows the cable at once, as on Windows (`rules::shown`).
        if let Some(online) = self.online() {
            return Some(match crate::rules::shown(online, full, self.current_ma.unwrap_or(0), self.dry_ms.unwrap_or(0)) {
                crate::rules::Shown::Charging => "Charging",
                crate::rules::Shown::NotCharging => "Not charging",
                crate::rules::Shown::Discharging => "Discharging",
                crate::rules::Shown::Full => "Full",
            });
        }
        let text = if full {
            "Full"
        } else if status & sbs::DISCHARGING != 0 {
            "Discharging"
        } else {
            "Charging"
        };
        Some(match self.current_ma {
            Some(0) if !full => "Not charging",
            Some(current) if full && current > 0 => "Charging",
            Some(current) if full && current < 0 => "Discharging",
            _ => text,
        })
    }

    /// Temperature in 0.1 °C. `sbs-battery` takes 2731 from the 0.1 K reading; the stop rule keeps the original's 2732.
    pub fn temp_tenths_c(&self) -> Option<i32> {
        self.temp_dk.map(|t| i32::from(t) - 2731)
    }

    /// What the battery's attribute file holds, without the newline.
    pub fn write_battery_attr(&self, name: &str, w: &mut dyn fmt::Write) -> Result<(), AttrError> {
        fn number(w: &mut dyn fmt::Write, value: Option<i64>) -> Result<(), AttrError> {
            let value = value.ok_or(AttrError::NoData)?;
            write!(w, "{value}")?;
            Ok(())
        }
        let micro = |v: Option<u16>| v.map(|v| i64::from(v) * 1000);
        match name {
            "type" => w.write_str("Battery")?,
            "technology" => w.write_str("Li-ion")?,
            "present" => write!(w, "{}", u8::from(self.present()))?,
            "status" => w.write_str(self.status_text().ok_or(AttrError::NoData)?)?,
            // "sbs spec says that this can be >100 %", min(ret, 100) as in sbs-battery.
            "capacity" => number(w, self.capacity.map(|c| i64::from(c.min(100))))?,
            "voltage_now" => number(w, micro(self.voltage_mv))?,
            "current_now" => number(w, self.current_ma.map(|c| i64::from(c) * 1000))?,
            "temp" => number(w, self.temp_tenths_c().map(i64::from))?,
            "charge_now" => number(w, micro(self.charge_now_mah))?,
            "charge_full" => number(w, micro(self.charge_full_mah))?,
            "charge_full_design" => number(w, micro(self.charge_full_design_mah))?,
            "cycle_count" => number(w, self.cycle_count.map(i64::from))?,
            _ => return Err(AttrError::UnknownAttr),
        }
        Ok(())
    }

    /// What the adapter's attribute file holds, without the newline.
    pub fn write_adapter_attr(&self, name: &str, w: &mut dyn fmt::Write) -> Result<(), AttrError> {
        match name {
            "type" => w.write_str("Mains")?,
            "online" => write!(w, "{}", u8::from(self.online().ok_or(AttrError::NoData)?))?,
            // Linux's units for a supply's input: µV, µA, µW.
            "voltage_now" => {
                let mv = self.adc.and_then(|(_, a)| a.vbus_mv).ok_or(AttrError::NoData)?;
                write!(w, "{}", u64::from(mv) * 1000)?
            }
            "current_now" => {
                let (_, a) = self.adc.ok_or(AttrError::NoData)?;
                write!(w, "{}", u64::from(a.iin_ma) * 1000)?
            }
            "power_now" => {
                let mw = self.adc.and_then(|(_, a)| a.power_mw()).ok_or(AttrError::NoData)?;
                write!(w, "{}", u64::from(mw) * 1000)?
            }
            // One line for a program: the conversion's time in ms of /proc/uptime's clock, then mV, mA, mW, mV, mV, and PSYS mV or `-`.
            "lucina_adc" => {
                let (t, a) = self.adc.ok_or(AttrError::NoData)?;
                write!(
                    w,
                    "t_ms {t} vbus_mv {} iin_ma {} power_mw {} vsys_mv {} vbat_mv {} psys_mv {}",
                    a.vbus_mv.unwrap_or(0),
                    a.iin_ma,
                    a.power_mw().unwrap_or(0),
                    a.vsys_mv,
                    a.vbat_mv,
                    Dash(a.psys_mv),
                )?
            }
            _ => return Err(AttrError::UnknownAttr),
        }
        Ok(())
    }
}

/// A number or `-`.
struct Dash(Option<u16>);

impl fmt::Display for Dash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(v) => write!(f, "{v}"),
            None => f.write_str("-"),
        }
    }
}
