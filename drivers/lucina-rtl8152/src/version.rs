//! Chip versions: the identifying bits of PLA_TCR1 (U-Boot `r8152_versions`).

use core::fmt;

/// What a chip version needs from the driver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Fast Ethernet, USB 2. What this driver brings up.
    Rtl8152,
    /// Gigabit, USB 3, with firmware patches that Linux loads. Refused: [`crate::Error::UnsupportedChip`].
    Rtl8153,
    /// Gigabit, USB 3, newer init. Refused: [`crate::Error::UnsupportedChip`].
    Rtl8153b,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    V01,
    V02,
    V03,
    V04,
    V05,
    V06,
    V07,
    V08,
    V09,
}

impl Version {
    /// The version with these identifying bits (PLA_TCR1 & [`crate::regs::VERSION_MASK`]).
    pub fn from_tcr(tcr: u16) -> Option<Self> {
        Some(match tcr {
            0x4c00 => Version::V01,
            0x4c10 => Version::V02,
            0x5c00 => Version::V03,
            0x5c10 => Version::V04,
            0x5c20 => Version::V05,
            0x5c30 => Version::V06,
            0x4800 => Version::V07,
            0x6000 => Version::V08,
            0x6010 => Version::V09,
            _ => return None,
        })
    }

    pub fn family(self) -> Family {
        match self {
            Version::V01 | Version::V02 | Version::V07 => Family::Rtl8152,
            Version::V03 | Version::V04 | Version::V05 | Version::V06 => Family::Rtl8153,
            Version::V08 | Version::V09 => Family::Rtl8153b,
        }
    }

    fn number(self) -> u8 {
        match self {
            Version::V01 => 1,
            Version::V02 => 2,
            Version::V03 => 3,
            Version::V04 => 4,
            Version::V05 => 5,
            Version::V06 => 6,
            Version::V07 => 7,
            Version::V08 => 8,
            Version::V09 => 9,
        }
    }
}

/// `RTL8152 VER_02`
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let family = match self.family() {
            Family::Rtl8152 => "RTL8152",
            Family::Rtl8153 => "RTL8153",
            Family::Rtl8153b => "RTL8153B",
        };
        write!(f, "{} VER_{:02}", family, self.number())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn the_table_as_in_u_boot() {
        for (tcr, version, family) in [
            (0x4c00, Version::V01, Family::Rtl8152),
            (0x4c10, Version::V02, Family::Rtl8152),
            (0x4800, Version::V07, Family::Rtl8152),
            (0x5c00, Version::V03, Family::Rtl8153),
            (0x5c10, Version::V04, Family::Rtl8153),
            (0x5c20, Version::V05, Family::Rtl8153),
            (0x5c30, Version::V06, Family::Rtl8153),
            (0x6000, Version::V08, Family::Rtl8153b),
            (0x6010, Version::V09, Family::Rtl8153b),
        ] {
            assert_eq!(Version::from_tcr(tcr), Some(version));
            assert_eq!(version.family(), family);
        }
        assert_eq!(Version::from_tcr(0), None);
        assert_eq!(Version::from_tcr(0x7cf0), None);
    }

    #[test]
    fn its_name_in_the_log() {
        assert_eq!(Version::V02.to_string(), "RTL8152 VER_02");
        assert_eq!(Version::V09.to_string(), "RTL8153B VER_09");
    }
}
