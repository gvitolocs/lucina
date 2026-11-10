//! The ceiling on what reaches the speakers, and the test tone.
//!
//! This path has no speaker protection: the amplifier is a plain I2S part, the calibration Windows loads into the DSP is not
//! sent, and nothing else limits it. So every sample from a program passes through [`Gain`], which cannot be made louder than
//! [`CEILING_DB`], and each stream starts at [`DEFAULT_DB`]. [`Tone`] takes no arguments at all: its level is a constant here,
//! and it does not go through [`Gain`], so no volume setting changes it.

pub use crate::rules::{CEILING_DB, SILENCE_DB};

/// What a stream starts at.
pub const DEFAULT_DB: i32 = -30;

/// `round(32768 * 10^(dB/20))` for each whole decibel from [`CEILING_DB`] down to [`SILENCE_DB`]: `no_std` has no `powf`.
const GAIN_Q15: [u16; 79] = [
    8231, 7336, 6538, 5827, 5193, 4629, 4125, 3677, 3277, 2920, 2603, 2320, 2068, 1843, 1642, 1464,
    1305, 1163, 1036, 924, 823, 734, 654, 583, 519, 463, 413, 368, 328, 292, 260, 232, 207, 184,
    164, 146, 130, 116, 104, 92, 82, 73, 65, 58, 52, 46, 41, 37, 33, 29, 26, 23, 21, 18, 16, 15,
    13, 12, 10, 9, 8, 7, 7, 6, 5, 5, 4, 4, 3, 3, 3, 2, 2, 2, 2, 1, 1, 1, 1,
];

use crate::rules;

/// A volume in Q15 that is never above the ceiling: the only way to make one is [`Gain::from_db`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gain {
    db: i32,
    q15: u16,
}

impl Gain {
    /// Asking for more than [`CEILING_DB`] gets [`CEILING_DB`]; below [`SILENCE_DB`] is silence.
    pub fn from_db(db: i32) -> Gain {
        match rules::gain_index(db) {
            None => Gain {
                db: SILENCE_DB - 1,
                q15: 0,
            },
            Some(index) => Gain {
                db: db.min(CEILING_DB),
                q15: rules::capped_q15(GAIN_Q15[index]),
            },
        }
    }

    pub fn db(&self) -> i32 {
        self.db
    }

    pub fn q15(&self) -> u16 {
        self.q15
    }

    /// Scales S16LE samples in place. A trailing odd byte is left alone: the caller only passes whole frames.
    pub fn apply(&self, pcm: &mut [u8]) {
        for sample in pcm.chunks_exact_mut(2) {
            let scaled = rules::scale(
                i16::from_le_bytes([sample[0], sample[1]]),
                rules::capped_q15(self.q15),
            );
            sample.copy_from_slice(&scaled.to_le_bytes());
        }
    }
}

impl Default for Gain {
    fn default() -> Gain {
        Gain::from_db(DEFAULT_DB)
    }
}

/// `round(32768 * 10^(-30/20))`: the tone's peak, as a constant nothing else feeds.
pub const TONE_PEAK: i16 = rules::TONE_PEAK as i16;
pub const TONE_RATE: u32 = 48_000;
pub const TONE_HZ: u32 = 1_000;
pub const TONE_FRAMES: u32 = 2 * TONE_RATE;
/// Up and down without a click, 5 ms.
const RAMP_FRAMES: u32 = rules::TONE_RAMP_FRAMES as u32;
/// One period of 1 kHz at 48 kHz, at [`TONE_PEAK`]: `round(1036 * sin(2 pi i / 48))`.
const TONE_TABLE: [i16; 48] = [
    0, 135, 268, 396, 518, 631, 733, 822, 897, 957, 1001, 1027, 1036, 1027, 1001, 957, 897, 822,
    733, 631, 518, 396, 268, 135, 0, -135, -268, -396, -518, -631, -733, -822, -897, -957, -1001,
    -1027, -1036, -1027, -1001, -957, -897, -822, -733, -631, -518, -396, -268, -135,
];

/// The test tone: 1 kHz, -30 dB, two seconds, S16LE stereo at 48 kHz. There is nothing to configure.
#[derive(Clone, Debug, Default)]
pub struct Tone {
    frame: u32,
}

impl Tone {
    pub fn new() -> Tone {
        Tone::default()
    }

    pub fn finished(&self) -> bool {
        self.frame >= TONE_FRAMES
    }

    /// Fills `pcm` with the next whole frames and says how many bytes were written.
    pub fn fill(&mut self, pcm: &mut [u8]) -> usize {
        let mut written = 0;
        for out in pcm.chunks_exact_mut(4) {
            if self.finished() {
                break;
            }
            let ramp = self
                .frame
                .min(TONE_FRAMES - 1 - self.frame)
                .min(RAMP_FRAMES);
            let s = rules::tone_sample(TONE_TABLE[(self.frame % 48) as usize], ramp);
            out[0..2].copy_from_slice(&s.to_le_bytes());
            out[2..4].copy_from_slice(&s.to_le_bytes());
            self.frame += 1;
            written += 4;
        }
        written
    }
}

/// CRC-32 (the zlib one) continued from `crc`, 0 at the start: lets a test compare what reached the device with what it expected.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = crc_table();
    let mut c = !crc;
    for byte in data {
        c = TABLE[((c ^ u32::from(*byte)) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}
