//! The waveform file: what the panel's maker measured, and what the controller reads while it scans.
//!
//! The controller walks the file's data by itself. The CPU only needs the header: which modes the file has, its pixel
//! depth, and its temperature ranges. The data is handed over as it is, from [`Waveform::data_offset`] on.

use crate::Error;

const HEADER_LEN: usize = 48;
const MODE_VERSION: usize = 16;
const LUTS: usize = 36;
const TRC: usize = 38;

/// The logical modes, in Lab126's numbering, which is what user space asks for.
pub const LOGICAL_MODES: usize = 12;
pub const MODE_INIT: u32 = 0;
pub const MODE_DU: u32 = 1;
pub const MODE_GC16: u32 = 2;
pub const MODE_GC16_FAST: u32 = 3;
pub const MODE_A2: u32 = 4;
pub const MODE_GL16: u32 = 5;
pub const MODE_GL16_FAST: u32 = 6;
pub const MODE_DU4: u32 = 7;
pub const MODE_REAGL: u32 = 8;
pub const MODE_REAGLD: u32 = 9;
pub const MODE_GL4: u32 = 10;
pub const MODE_GL16_INV: u32 = 11;

/// The file's own mode numbers.
const FILE_INIT: u8 = 0;
const FILE_DU: u8 = 1;
const FILE_GC16: u8 = 2;
const FILE_GL16: u8 = 3;
const FILE_A2: u8 = 6;
const FILE_DU4: u8 = 7;

/// The file's mode for each logical mode, for mode version 25, this panel's. REAGL runs as GL16, as real REAGL needs the
/// processing in Amazon's closed module.
pub const MODES_V25: [u8; LOGICAL_MODES] = [
    FILE_INIT, FILE_DU, FILE_GC16, FILE_GC16, FILE_A2, FILE_GL16, FILE_GL16, FILE_DU4, FILE_GL16,
    FILE_GL16, FILE_DU4, FILE_GC16,
];

/// The temperature ranges a file can have.
const MAX_RANGES: usize = 32;

/// What the header says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Waveform {
    pub mode_version: u8,
    /// Whether pixels have 5 bits in the working buffer (else 4).
    pub five_bit: bool,
    /// Where the controller's data starts in the file.
    pub data_offset: usize,
    ranges: usize,
    bounds: [u8; MAX_RANGES],
}

impl Waveform {
    /// Reads a waveform file. Only mode version 25 is known here: another panel means other modes and another VCOM, and
    /// this driver must not guess those.
    pub fn parse(file: &[u8]) -> Result<Self, Error> {
        let trc = *file.get(TRC).ok_or(Error::BadWaveform)? as usize;
        let ranges = trc + 1;
        let data_offset = HEADER_LEN + ranges + 1;
        if ranges > MAX_RANGES || file.len() <= data_offset {
            return Err(Error::BadWaveform);
        }
        let mode_version = file[MODE_VERSION];
        if mode_version != 25 {
            return Err(Error::BadWaveform);
        }
        let mut bounds = [0; MAX_RANGES];
        bounds[..ranges].copy_from_slice(&file[HEADER_LEN..HEADER_LEN + ranges]);
        if bounds[..ranges].windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::BadWaveform);
        }
        Ok(Self {
            mode_version,
            five_bit: file[LUTS] & 0xc == 0x4,
            data_offset,
            ranges,
            bounds,
        })
    }

    /// This panel's header, for running without the panel or its file.
    pub fn assumed() -> Self {
        let mut bounds = [0; MAX_RANGES];
        bounds[..14].copy_from_slice(&[0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 38, 43]);
        Self {
            mode_version: 25,
            five_bit: true,
            data_offset: 63,
            ranges: 14,
            bounds,
        }
    }

    /// How many temperature ranges the file has.
    pub fn ranges(&self) -> usize {
        self.ranges
    }

    /// The range that holds this temperature: the last one that starts at or below it, or the first.
    pub fn temp_index(&self, temp_c: i32) -> u32 {
        let starts = &self.bounds[..self.ranges];
        starts
            .iter()
            .rposition(|&start| temp_c >= i32::from(start))
            .unwrap_or(0) as u32
    }

    /// The file's mode for a logical one.
    pub fn file_mode(&self, logical: u32) -> Option<u8> {
        MODES_V25.get(logical as usize).copied()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;

    /// A file with this panel's header: mode version 25, 5 bits, 14 ranges.
    pub(crate) fn file() -> Vec<u8> {
        let mut file = vec![0u8; 200];
        file[MODE_VERSION] = 25;
        file[LUTS] = 0x04;
        file[TRC] = 13;
        file[HEADER_LEN..HEADER_LEN + 14]
            .copy_from_slice(&[0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 38, 43]);
        file[HEADER_LEN + 14] = 48;
        file
    }

    #[test]
    fn the_data_starts_after_the_header_the_ranges_and_one_byte() {
        let waveform = Waveform::parse(&file()).unwrap();
        assert_eq!(waveform.data_offset, 63);
        assert_eq!(waveform.ranges(), 14);
        assert!(waveform.five_bit);
    }

    #[test]
    fn temperatures_fall_in_their_ranges_and_outside_ones_clamp() {
        let waveform = Waveform::parse(&file()).unwrap();
        assert_eq!(waveform.temp_index(-10), 0);
        assert_eq!(waveform.temp_index(0), 0);
        assert_eq!(waveform.temp_index(2), 0);
        assert_eq!(waveform.temp_index(3), 1);
        assert_eq!(waveform.temp_index(20), 6);
        assert_eq!(waveform.temp_index(25), 8);
        assert_eq!(waveform.temp_index(42), 12);
        assert_eq!(waveform.temp_index(43), 13);
        assert_eq!(waveform.temp_index(90), 13);
    }

    #[test]
    fn a_file_for_something_else_is_refused() {
        assert_eq!(Waveform::parse(&[]), Err(Error::BadWaveform));
        assert_eq!(Waveform::parse(&file()[..40]), Err(Error::BadWaveform));
        let mut other = file();
        other[MODE_VERSION] = 24;
        assert_eq!(Waveform::parse(&other), Err(Error::BadWaveform));
        let mut scrambled = file();
        scrambled[HEADER_LEN + 2] = 1;
        assert_eq!(Waveform::parse(&scrambled), Err(Error::BadWaveform));
    }

    #[test]
    fn the_modes_are_those_of_version_25() {
        let waveform = Waveform::parse(&file()).unwrap();
        assert_eq!(waveform.file_mode(MODE_INIT), Some(0));
        assert_eq!(waveform.file_mode(MODE_DU), Some(1));
        assert_eq!(waveform.file_mode(MODE_GC16), Some(2));
        assert_eq!(waveform.file_mode(MODE_A2), Some(6));
        assert_eq!(waveform.file_mode(MODE_GL16), Some(3));
        assert_eq!(waveform.file_mode(MODE_REAGL), Some(3));
        assert_eq!(waveform.file_mode(MODE_DU4), Some(7));
        assert_eq!(waveform.file_mode(12), None);
    }
}
