//! The PM6150's power-on (PON) registers, as patch 0841 logs them raw at every boot:
//! `power: PON reasons 08: 03 00 00 00 00 00 00 00 c0: c1 00 00 00 80 02 00 80 00 08 80 00 00 00 00 00`.
//! They survive what RAM does not, so they say how this boot started and how the one before ended even after a power loss.
//!
//! Meaning from Qualcomm's downstream `drivers/input/misc/qpnp-power-on.c` (GPL-2.0, behaviour only; read from the
//! LineageOS sm6150 tree, a copy in buildhost `~/scratch/juno-evlog/ref/`): this PMIC's PON is GEN2, so PON_REASON1 is 0xC0,
//! WARM_RESET_REASON1 0xC2, POFF_REASON1 0xC5, OFF_REASON 0xC7 (which sequence ended the last power-off: bit 7 POFF, bit 6
//! FAULT, bit 5 S3 reset), FAULT_REASON1/2 0xC8/0xC9 and S3_RESET_REASON 0xCA. The driver picks one sequence (POFF before
//! FAULT before S3) and names its lowest set bit (`qpnp_pon_read_gen2_pon_off_reason`, `ffs`); a warm boot is a non-zero
//! WARM_RESET_REASON1 (`_qpnp_pon_is_warm_reset`). The 0x08 block is GEN1's layout and is kept but not decoded.
//!
//! Shared by lucina-evlog (the event log) and the kernel's crash record (the unexplained reset's `pmic:` line), so that
//! both decode alike. no_std with alloc.

#![no_std]

extern crate alloc;

use alloc::{format, string::String, vec::Vec};

pub mod rules;

/// The bytes of one line: 0x08..0x0F and 0xC0..0xCF, `None` where the read failed (`--`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PonRegs {
    pub gen1: [Option<u8>; 8],
    pub gen2: [Option<u8>; 16],
}

fn read_block<const N: usize>(text: &str, label: &str) -> Option<[Option<u8>; N]> {
    let at = text.find(label)? + label.len();
    let mut out = [None; N];
    let mut words = text[at..].split_ascii_whitespace();
    for slot in out.iter_mut() {
        let w = words.next()?;
        *slot = match w {
            "--" => None,
            _ if w.len() == 2 => Some(u8::from_str_radix(w, 16).ok()?),
            _ => return None,
        };
    }
    Some(out)
}

/// Finds `power: PON reasons` anywhere in a log line (console or /dev/kmsg form).
pub fn parse_line(line: &str) -> Option<PonRegs> {
    let at = line.find("power: PON reasons")?;
    let rest = &line[at..];
    let gen2 = read_block::<16>(rest, " c0:")?;
    let gen1 = read_block::<8>(rest, " 08:").unwrap_or([None; 8]);
    Some(PonRegs { gen1, gen2 })
}

/// The registers that matter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pon {
    /// 0xC0, PON_REASON1: what started this boot.
    pub on_reason: u8,
    /// 0xC2, WARM_RESET_REASON1: non-zero for a warm reset.
    pub warm_reset: u8,
    /// 0xC5, POFF_REASON1.
    pub poff_reason: u8,
    /// 0xC7, OFF_REASON: which sequence ended the last power-off.
    pub off_seq: u8,
    /// 0xC8 | 0xC9 << 8, FAULT_REASON1/2.
    pub fault: u16,
    /// 0xCA, S3_RESET_REASON.
    pub s3_reset: u8,
}

impl PonRegs {
    pub fn pon(&self) -> Option<Pon> {
        let r = |a: usize| self.gen2[a - 0xc0];
        Some(Pon {
            on_reason: r(0xc0)?,
            warm_reset: r(0xc2)?,
            poff_reason: r(0xc5)?,
            off_seq: r(0xc7)?,
            fault: u16::from(r(0xc8)?) | (u16::from(r(0xc9)?) << 8),
            s3_reset: r(0xca)?,
        })
    }
}

/// PON_REASON1's bits, 0 to 7 (`qpnp_pon_reason[]`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnBit {
    HardReset,
    Smpl,
    Rtc,
    DcCharger,
    UsbCharger,
    Pon1,
    Cable,
    PowerKey,
}

const ON_BITS: [OnBit; 8] = [
    OnBit::HardReset,
    OnBit::Smpl,
    OnBit::Rtc,
    OnBit::DcCharger,
    OnBit::UsbCharger,
    OnBit::Pon1,
    OnBit::Cable,
    OnBit::PowerKey,
];

impl OnBit {
    pub fn name(self) -> &'static str {
        match self {
            OnBit::HardReset => "hard reset",
            OnBit::Smpl => "SMPL",
            OnBit::Rtc => "RTC alarm",
            OnBit::DcCharger => "DC charger",
            OnBit::UsbCharger => "USB charger",
            OnBit::Pon1 => "PON1",
            OnBit::Cable => "cable",
            OnBit::PowerKey => "power key",
        }
    }

    pub fn plain(self) -> &'static str {
        match self {
            OnBit::HardReset => "a hard reset",
            OnBit::Smpl => "power was lost for a moment (SMPL)",
            OnBit::Rtc => "the real-time clock's alarm",
            OnBit::DcCharger => "a DC charger was plugged in",
            OnBit::UsbCharger => "a USB charger was plugged in",
            OnBit::Pon1 => "the secondary PMIC",
            OnBit::Cable => "external power (the charger) was present",
            OnBit::PowerKey => "the power key was pressed",
        }
    }
}

/// Why the last power-off happened (`qpnp_poff_reason[]`, GEN2 part).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffReason {
    // POFF sequence, index 0..7.
    Software,
    PsHold,
    PmicWatchdog,
    KeypadReset1,
    KeypadReset2,
    PowerKeyAndResetLine,
    ResetLine,
    PowerKeyLongHold,
    // FAULT sequence, index 16..31.
    GpFault(u8),
    MbgFault,
    Ovlo,
    Uvlo,
    AvddRb,
    FaultN,
    PbsWatchdog,
    PbsNack,
    RestartPon,
    OverTemperature,
    // S3 reset sequence, index 32..39.
    S3FaultN,
    S3PbsWatchdog,
    S3PbsNack,
    S3PowerKeyOrResetLine,
    /// A bit the driver's table calls "N/A", or past it: its index in that table.
    Unknown(u16),
}

impl OffReason {
    /// The table's entry for an index, as the C driver numbers them.
    pub fn from_index(i: u16) -> OffReason {
        use OffReason::*;
        match i {
            0 => Software,
            1 => PsHold,
            2 => PmicWatchdog,
            3 => KeypadReset1,
            4 => KeypadReset2,
            5 => PowerKeyAndResetLine,
            6 => ResetLine,
            7 => PowerKeyLongHold,
            16..=19 => GpFault((i - 16) as u8),
            20 => MbgFault,
            21 => Ovlo,
            22 => Uvlo,
            23 => AvddRb,
            27 => FaultN,
            28 => PbsWatchdog,
            29 => PbsNack,
            30 => RestartPon,
            31 => OverTemperature,
            36 => S3FaultN,
            37 => S3PbsWatchdog,
            38 => S3PbsNack,
            39 => S3PowerKeyOrResetLine,
            other => Unknown(other),
        }
    }

    pub fn name(&self) -> String {
        use OffReason::*;
        match *self {
            Software => "SOFT".into(),
            PsHold => "PS_HOLD".into(),
            PmicWatchdog => "PMIC watchdog".into(),
            KeypadReset1 => "keypad reset 1".into(),
            KeypadReset2 => "keypad reset 2".into(),
            PowerKeyAndResetLine => "power key and reset line".into(),
            ResetLine => "RESIN_N".into(),
            PowerKeyLongHold => "KPDPWR_N long hold".into(),
            GpFault(n) => format!("GP_FAULT{n}"),
            MbgFault => "MBG fault".into(),
            Ovlo => "OVLO".into(),
            Uvlo => "UVLO".into(),
            AvddRb => "AVDD_RB".into(),
            FaultN => "FAULT_N".into(),
            PbsWatchdog => "PBS watchdog".into(),
            PbsNack => "PBS NACK".into(),
            RestartPon => "restart PON".into(),
            OverTemperature => "OTST3".into(),
            S3FaultN => "S3 reset, FAULT_N".into(),
            S3PbsWatchdog => "S3 reset, PBS watchdog".into(),
            S3PbsNack => "S3 reset, PBS NACK".into(),
            S3PowerKeyOrResetLine => "S3 reset, power key or reset line".into(),
            Unknown(i) => format!("bit {i}"),
        }
    }

    pub fn plain(&self) -> String {
        use OffReason::*;
        match *self {
            Software => "software asked the PMIC to switch off".into(),
            PsHold => "the processor itself switched the power off or restarted (PS_HOLD dropped): a software restart, a firmware reset or a watchdog, not a lost power supply".into(),
            PmicWatchdog => "the PMIC's own watchdog expired".into(),
            KeypadReset1 | KeypadReset2 => "a key combination reset the machine".into(),
            PowerKeyAndResetLine => "the power key and the reset line together forced a reset".into(),
            ResetLine => "the reset line (volume-down on phones) forced a reset".into(),
            PowerKeyLongHold => "the power key was held down until the PMIC switched off".into(),
            GpFault(n) => format!("a general-purpose fault input ({n}) tripped"),
            MbgFault => "the PMIC's reference (MBG) faulted".into(),
            Ovlo => "the supply voltage went too high".into(),
            Uvlo => "the supply voltage fell too low (battery empty or a brownout)".into(),
            AvddRb => "the PMIC's analogue supply faulted".into(),
            FaultN => "an external fault line pulled the power".into(),
            PbsWatchdog => "the PMIC's sequencer (PBS) timed out".into(),
            PbsNack => "the PMIC's sequencer (PBS) refused a command".into(),
            RestartPon => "the PMIC restarted the power-on sequence".into(),
            OverTemperature => "the PMIC overheated".into(),
            S3FaultN => "a stage-3 reset from an external fault line".into(),
            S3PbsWatchdog => "a stage-3 reset after the PMIC's sequencer timed out".into(),
            S3PbsNack => "a stage-3 reset after the PMIC's sequencer refused a command".into(),
            S3PowerKeyOrResetLine => "a stage-3 reset by holding the power key or reset line".into(),
            Unknown(i) => format!("an undocumented reason (bit {i})"),
        }
    }
}

/// Every set bit of a reason register, lowest first, as `rules::lowest_bit` finds them.
fn bits(value: u16, offset: u16) -> Vec<OffReason> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(b) = rules::lowest_bit(value, from) {
        out.push(OffReason::from_index(offset + b));
        from = b + 1;
    }
    out
}

impl Pon {
    pub fn on_bits(&self) -> Vec<OnBit> {
        (0..8)
            .filter(|b| self.on_reason & (1 << b) != 0)
            .map(|b| ON_BITS[b])
            .collect()
    }

    pub fn started_by(&self) -> Option<OnBit> {
        self.on_bits().first().copied()
    }

    pub fn is_warm(&self) -> bool {
        self.warm_reset != 0
    }

    /// The chosen sequence's register value and its offset in the table.
    fn chosen(&self) -> Option<(u16, u16)> {
        rules::off_register(self.off_seq, self.poff_reason, self.fault, self.s3_reset)
    }

    /// The driver's one reason (`rules::off_index`, Verus-checked).
    pub fn off_reason(&self) -> Option<OffReason> {
        rules::off_index(self.off_seq, self.poff_reason, self.fault, self.s3_reset)
            .map(OffReason::from_index)
    }

    pub fn off_reasons(&self) -> Vec<OffReason> {
        self.chosen().map(|(v, o)| bits(v, o)).unwrap_or_default()
    }

    pub fn older_reasons(&self) -> Vec<OffReason> {
        let chosen = self.chosen().map(|(_, o)| o);
        let mut out = Vec::new();
        if chosen != Some(16) {
            out.extend(bits(self.fault, 16));
        }
        if chosen != Some(32) {
            out.extend(bits(u16::from(self.s3_reset), 32));
        }
        out
    }
}

impl Pon {
    /// The line the kernel adds to an unexplained reset's record (`pmic: ...`): how the last boot ended as the PMIC tells
    /// it, and how this one started, e.g. `last power-off by PS_HOLD (the processor itself switched the power off ...);
    /// this boot by hard reset, cold`.
    pub fn record_line(&self) -> String {
        let off = match self.off_reason() {
            Some(r) => format!("last power-off by {} ({})", r.name(), r.plain()),
            None => String::from("last power-off: the PMIC names no reason"),
        };
        let on = match self.started_by() {
            Some(b) => b.name(),
            None => "nothing the PMIC names",
        };
        format!(
            "{off}; this boot by {on}, {} [c0 {:02x} c2 {:02x} c5 {:02x} c7 {:02x} c8 {:02x} c9 {:02x} ca {:02x}]",
            if self.is_warm() { "warm" } else { "cold" },
            self.on_reason,
            self.warm_reset,
            self.poff_reason,
            self.off_seq,
            self.fault as u8,
            (self.fault >> 8) as u8,
            self.s3_reset,
        )
    }

    /// The registers again from a [`Pon::record_line`] (the bracket at its end), so that a record can be decoded when the
    /// boot that wrote it is gone from the log.
    pub fn from_record_line(line: &str) -> Option<Pon> {
        let inner = line[line.rfind('[')? + 1..].strip_suffix(']')?;
        let mut regs = [None::<u8>; 7];
        let names = ["c0", "c2", "c5", "c7", "c8", "c9", "ca"];
        let mut words = inner.split_ascii_whitespace();
        while let (Some(name), Some(value)) = (words.next(), words.next()) {
            let i = names.iter().position(|n| *n == name)?;
            regs[i] = Some(u8::from_str_radix(value, 16).ok()?);
        }
        Some(Pon {
            on_reason: regs[0]?,
            warm_reset: regs[1]?,
            poff_reason: regs[2]?,
            off_seq: regs[3]?,
            fault: u16::from(regs[4]?) | (u16::from(regs[5]?) << 8),
            s3_reset: regs[6]?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const RESET: &str = "[    0.149000] power: PON reasons 08: 03 00 00 00 00 00 00 00 c0: 41 00 00 00 80 02 00 80 00 08 80 00 00 00 00 00";
    const KEY: &str = "6,234,372000,-;power: PON reasons 08: 03 00 00 00 00 00 00 00 c0: c1 00 00 00 80 02 00 80 00 08 80 00 00 00 00 00";

    #[test]
    fn a_reset_with_the_charger_in() {
        let regs = parse_line(RESET).unwrap();
        assert_eq!(regs.gen1[0], Some(3));
        let p = regs.pon().unwrap();
        assert_eq!(p.on_bits(), vec![OnBit::HardReset, OnBit::Cable]);
        assert_eq!(p.started_by(), Some(OnBit::HardReset));
        assert!(!p.is_warm());
        assert_eq!(p.off_reason(), Some(OffReason::PsHold));
        assert_eq!(p.off_reasons(), vec![OffReason::PsHold]);
        assert_eq!(
            p.older_reasons(),
            vec![OffReason::FaultN, OffReason::S3PowerKeyOrResetLine]
        );
    }

    #[test]
    fn the_record_line() {
        let p = parse_line(RESET).unwrap().pon().unwrap();
        assert_eq!(
            p.record_line(),
            format!(
                "last power-off by PS_HOLD ({}); this boot by hard reset, cold [c0 41 c2 00 c5 02 c7 80 c8 00 c9 08 ca 80]",
                OffReason::PsHold.plain()
            )
        );
        assert_eq!(Pon::from_record_line(&p.record_line()), Some(p));
        let mut warm = p;
        warm.warm_reset = 1;
        warm.off_seq = 0;
        assert!(warm.record_line().starts_with("last power-off: the PMIC names no reason; this boot by hard reset, warm ["));
        assert_eq!(Pon::from_record_line(&warm.record_line()), Some(warm));
        assert_eq!(Pon::from_record_line("no bracket"), None);
        assert_eq!(Pon::from_record_line("x [c0 41 c2 00]"), None);
        assert_eq!(Pon::from_record_line("x [c0 zz c2 00 c5 02 c7 80 c8 00 c9 08 ca 80]"), None);
    }

    #[test]
    fn the_power_key_started_it() {
        let p = parse_line(KEY).unwrap().pon().unwrap();
        assert_eq!(
            p.on_bits(),
            vec![OnBit::HardReset, OnBit::Cable, OnBit::PowerKey]
        );
    }

    #[test]
    fn failed_reads_and_other_lines() {
        let l = RESET.replace("80 02 00 80", "80 -- 00 80");
        let regs = parse_line(&l).unwrap();
        assert_eq!(regs.gen2[5], None);
        assert!(regs.pon().is_none());
        assert!(parse_line("wifi: alive at 1 ms").is_none());
        assert!(parse_line("power: PON reasons 08: 03 c0: 41 00").is_none());
    }

    fn pon(off_seq: u8, poff: u8, fault: u16, s3: u8) -> Pon {
        Pon {
            on_reason: 1,
            warm_reset: 0,
            poff_reason: poff,
            off_seq,
            fault,
            s3_reset: s3,
        }
    }

    #[test]
    fn the_sequence_chooses_the_register() {
        assert_eq!(pon(0x40, 0, 0x0040, 0).off_reason(), Some(OffReason::Uvlo));
        assert_eq!(
            pon(0x20, 0, 0, 0x20).off_reason(),
            Some(OffReason::S3PbsWatchdog)
        );
        assert_eq!(pon(0, 2, 0, 0).off_reason(), None);
        assert_eq!(pon(0x80, 0, 0, 0).off_reason(), None);
        assert_eq!(
            pon(0x80, 0x80, 0, 0).off_reason(),
            Some(OffReason::PowerKeyLongHold)
        );
        assert_eq!(
            pon(0xc0, 0x02, 0x0040, 0).off_reason(),
            Some(OffReason::PsHold),
            "POFF wins over FAULT"
        );
        assert_eq!(
            pon(0x80, 0x06, 0, 0).off_reasons(),
            vec![OffReason::PsHold, OffReason::PmicWatchdog]
        );
        assert_eq!(
            pon(0x40, 0, 0x0100, 0).off_reason(),
            Some(OffReason::Unknown(24))
        );
    }

    #[test]
    fn every_reason_reads_in_words() {
        for i in 0..48 {
            let r = OffReason::from_index(i);
            assert!(!r.plain().is_empty() && !r.plain().contains("N/A"), "{i}");
            assert!(!r.name().is_empty());
        }
        for b in ON_BITS {
            assert!(!b.plain().is_empty());
        }
    }
}
