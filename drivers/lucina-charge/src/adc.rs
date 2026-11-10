//! The BQ25710's own ADC: adapter voltage, input current, system and battery voltage, so that power can be measured on the
//! charger, where the battery gauge reads 0.
//!
//! The original is Chromium EC's `bq25710_adc_start` (driver/charger/bq25710.c): set the channels and ADC_START in
//! ADCOption, poll every 2 ms for ADC_START to clear, at most 10 times, then read the result registers. Scaling from TI's
//! SLUSD20B, 9.6.9 to 9.6.12.
//!
//! Differences, on purpose (`docs/research/juno-charge.md`):
//! * Chromium writes the channels and ADC_START alone. This reads ADCOption first and changes only ADC_CONV (one-shot),
//!   ADC_START and the channel bits, keeping ADC_FULLSCALE and the reserved bits as the firmware left them; it reads the
//!   register back after the conversion.
//! * Chromium clears EN_LWPWR in ChargeOption0 around the conversion. That is a charge register, so this never writes it.
//!   The datasheet's low power mode only applies "with battery only", so with the adapter in the ADC works; without it
//!   nothing is written and there is no reading.
//! * PSYS needs EN_PSYS (ChargeOption1 bit 12), which the firmware leaves clear: it is read only when the firmware set it.
//!   IBAT likewise needs EN_IBAT, so the charge and discharge currents are not read at all.
//! * ChargeOption0, ChargeCurrent, MaxChargeVoltage, IIN_HOST and ChargeOption1 are read before and after: a conversion
//!   that changes any of them turns the ADC off until restart. Only ChargeCurrent going to 0 is allowed, which the chip
//!   does by itself when its watchdog runs out or the adapter goes.
//!
//! ADCOption is not a watchdog write: only MaxChargeVoltage and ChargeCurrent reset the watchdog (SLUSD20B 9.3.10.1), so
//! conversions keep nothing charging.
//!
//! Verus proves [`start_value`] in this very file (`verification/juno-charge/rules.rs`).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[cfg(not(verus_keep_ghost))]
use crate::{Smbus, bq25710};

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_OPTION: u8 = 0x35;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_VBUS_PSYS: u8 = 0x23;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_IIN_CMPIN: u8 = 0x25;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_VSYS_VBAT: u8 = 0x26;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CHARGE_OPTION_1: u8 = 0x30;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IIN_HOST: u8 = 0x3f;

/// ADCOption: 1 is continuous (every second), 0 one-shot.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_CONV: u16 = 1 << 15;
/// ADCOption: starts a conversion, and clears itself when a one-shot conversion is done.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ADC_START: u16 = 1 << 14;
/// ADCOption: the input range (bit 13) and reserved bits 12:8, never changed here.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const KEEP: u16 = 0x3f00;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_VBUS: u16 = 1 << 6;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_PSYS: u16 = 1 << 5;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_IIN: u16 = 1 << 4;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_VSYS: u16 = 1 << 1;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_VBAT: u16 = 1 << 0;
/// ChargeOption0 bit 15: low power mode, "with battery only".
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const EN_LWPWR: u16 = 1 << 15;
/// ChargeOption1: PSYS sensing on.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const OPT1_EN_PSYS: u16 = 1 << 12;
/// ChargeOption1: the input sense resistor is 20 mΩ, not 10.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const OPT1_RSNS_RAC: u16 = 1 << 11;
/// Polls of ADC_START, 2 ms apart. There is no completion interrupt: SLUSD20B has only ADC_START clearing itself, and Chromium
/// polls it. Chromium's rule is to give up at twice the expected time: 10 polls for SLUSD20B's typical 10 ms, after it leaves
/// low power mode. Its own note: "If low power mode isn't exited first, then the conversion time jumps to ~60 msec". This never
/// leaves it (a charge register), so twice 60 ms by the same rule: 60 polls. [`Conversion::polls`] says how many it took.
pub const POLLS: u32 = 60;
pub const POLL_MS: u64 = 2;

/// What ADCOption becomes for a one-shot conversion of `channels`: one-shot, started, those channels, and the input range
/// and reserved bits as `old` had them.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r & 0x3f00 == old & 0x3f00, // KEEP
        r & 0x8000 == 0, // ADC_CONV
        r & 0x4000 == 0x4000, // ADC_START
        r & 0xff == channels & 0xff,
))]
pub fn start_value(old: u16, channels: u16) -> u16 {
    #[cfg(verus_keep_ghost)]
    proof! { start_value_bits(old, channels); }
    // KEEP, ADC_START, as literals for the solver; the assertion below ties them to the names.
    (old & 0x3f00) | 0x4000 | (channels & 0xff)
}

#[cfg(not(verus_keep_ghost))]
const _: () = assert!(KEEP == 0x3f00 && ADC_START == 0x4000 && ADC_CONV == 0x8000);

#[cfg(verus_keep_ghost)]
verus! {

/// The bits of [`start_value`], for the solver: `KEEP` is 0x3f00, `ADC_START` 0x4000, `ADC_CONV` 0x8000.
#[verifier::bit_vector]
proof fn start_value_bits(old: u16, channels: u16)
    ensures
        ((old & 0x3f00) | 0x4000 | (channels & 0xff)) & 0x3f00 == old & 0x3f00,
        ((old & 0x3f00) | 0x4000 | (channels & 0xff)) & 0x8000 == 0,
        ((old & 0x3f00) | 0x4000 | (channels & 0xff)) & 0x4000 == 0x4000,
        ((old & 0x3f00) | 0x4000 | (channels & 0xff)) & 0xff == channels & 0xff,
{
}

}

/// One conversion's results, in mV and mA.
#[cfg(not(verus_keep_ghost))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Adc {
    /// None below 3.2 V, which the ADC cannot measure (Chromium reports 0).
    pub vbus_mv: Option<u16>,
    pub iin_ma: u16,
    pub vsys_mv: u16,
    pub vbat_mv: u16,
    /// The PSYS pin's voltage, only when the firmware turned PSYS on. Watts need the board's PSYS resistor, not known.
    pub psys_mv: Option<u16>,
}

#[cfg(not(verus_keep_ghost))]
impl Adc {
    /// Input power, VBUS × IIN, in mW.
    pub fn power_mw(&self) -> Option<u32> {
        self.vbus_mv
            .map(|mv| u32::from(mv) * u32::from(self.iin_ma) / 1000)
    }

    /// The result registers, as SLUSD20B 9.6.9-9.6.12 scale them. `rac_20`: ChargeOption1's RSNS_RAC.
    pub fn decode(vbus_psys: u16, iin_cmpin: u16, vsys_vbat: u16, rac_20: bool, psys_on: bool) -> Adc {
        let hi = |w: u16| w >> 8;
        let lo = |w: u16| w & 0xff;
        let vbus = hi(vbus_psys);
        Adc {
            vbus_mv: (vbus != 0).then(|| vbus * 64 + 3200),
            iin_ma: hi(iin_cmpin) * if rac_20 { 25 } else { 50 },
            vsys_mv: hi(vsys_vbat) * 64 + 2880,
            vbat_mv: lo(vsys_vbat) * 64 + 2880,
            psys_mv: psys_on.then(|| lo(vbus_psys) * 12),
        }
    }
}

/// Why a conversion gave nothing.
#[cfg(not(verus_keep_ghost))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdcError<E> {
    Bus { write: bool, reg: u8, error: E },
    /// Battery only and low power mode: "ADC is not available". Nothing was written.
    LowPower,
    /// ADC_START did not clear.
    Timeout { option: u16 },
    /// ADCOption did not read back as written.
    ReadBack { wrote: u16, read: u16 },
    /// A charge register changed across the conversion. The ADC is never used again until restart.
    ChargeChanged { reg: u8, before: u16, after: u16 },
}

/// What was done, besides the results.
#[cfg(not(verus_keep_ghost))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Conversion {
    pub adc: Adc,
    /// ADCOption before, and what was written (`None` when the firmware already converts continuously).
    pub option_before: u16,
    pub wrote: Option<u16>,
    /// Polls until ADC_START cleared, 2 ms apart (0 with nothing written).
    pub polls: u32,
}

/// The registers that must not change: ChargeOption0, ChargeCurrent, MaxChargeVoltage, IIN_HOST, ChargeOption1.
#[cfg(not(verus_keep_ghost))]
const WATCHED: [u8; 5] = [
    bq25710::CHARGE_OPTION_0,
    bq25710::CHARGE_CURRENT,
    bq25710::MAX_CHARGE_VOLTAGE,
    IIN_HOST,
    CHARGE_OPTION_1,
];

/// One conversion, as Chromium does it, with the differences above. `wait_ms` sleeps. Only for a chip whose IDs were
/// just read as the BQ25710's.
#[cfg(not(verus_keep_ghost))]
pub fn convert<B: Smbus>(bus: &mut B, wait_ms: &mut dyn FnMut(u64)) -> Result<Conversion, AdcError<B::Error>> {
    let c = bq25710::ADDR;
    let read = |bus: &mut B, reg| bus.read_word(c, reg).map_err(|error| AdcError::Bus { write: false, reg, error });
    let mut before = [0u16; WATCHED.len()];
    for (value, reg) in before.iter_mut().zip(WATCHED) {
        *value = read(bus, reg)?;
    }
    let (option0, option1) = (before[0], before[4]);
    let adapter = read(bus, bq25710::CHARGER_STATUS)? & bq25710::AC_STAT != 0;
    if !adapter && option0 & EN_LWPWR != 0 {
        return Err(AdcError::LowPower);
    }
    let psys_on = option1 & OPT1_EN_PSYS != 0;
    let channels = EN_VBUS | EN_IIN | EN_VSYS | EN_VBAT | if psys_on { EN_PSYS } else { 0 };

    let option_before = read(bus, ADC_OPTION)?;
    let mut polls = 0;
    // Continuous conversions, if the firmware set them, already fill the results: nothing to write.
    let wrote = if option_before & ADC_CONV != 0 && option_before & channels == channels {
        None
    } else {
        let value = start_value(option_before, channels);
        bus.write_word(c, ADC_OPTION, value)
            .map_err(|error| AdcError::Bus { write: true, reg: ADC_OPTION, error })?;
        let mut option = value;
        while polls < POLLS {
            wait_ms(POLL_MS);
            polls += 1;
            option = read(bus, ADC_OPTION)?;
            if option & ADC_START == 0 {
                break;
            }
        }
        if option & ADC_START != 0 {
            return Err(AdcError::Timeout { option });
        }
        if option != value & !ADC_START {
            return Err(AdcError::ReadBack { wrote: value, read: option });
        }
        Some(value)
    };

    for (&was, reg) in before.iter().zip(WATCHED) {
        let now = read(bus, reg)?;
        let allowed = reg == bq25710::CHARGE_CURRENT && now == 0;
        if now != was && !allowed {
            return Err(AdcError::ChargeChanged { reg, before: was, after: now });
        }
    }
    let vbus_psys = read(bus, ADC_VBUS_PSYS)?;
    let iin_cmpin = read(bus, ADC_IIN_CMPIN)?;
    let vsys_vbat = read(bus, ADC_VSYS_VBAT)?;
    Ok(Conversion {
        adc: Adc::decode(vbus_psys, iin_cmpin, vsys_vbat, option1 & OPT1_RSNS_RAC != 0, psys_on),
        option_before,
        wrote,
        polls,
    })
}
