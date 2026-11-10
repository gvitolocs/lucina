//! IMA-ADPCM in Microsoft's WAV layout (format tag 0x0011), mono: 256-byte blocks of 505 samples. Each block starts
//! with its first sample (i16 LE, also the block's predictor), the step index and a zero byte; then 504 samples in
//! 252 bytes, the low nibble first. This is what ffmpeg's `adpcm_ima_wav` and Windows' imaadp32 read.

use crate::rules::{clamp_index, next_index, next_predictor};

pub const STEP_TABLE: [i16; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143,
    157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552,
    1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442, 11487,
    12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

pub const BLOCK_ALIGN: usize = 256;
pub const SAMPLES_PER_BLOCK: usize = 505;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoder {
    predictor: i32,
    index: i32,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder::new()
    }
}

impl Encoder {
    pub const fn new() -> Encoder {
        Encoder { predictor: 0, index: 0 }
    }

    /// One sample to a 4-bit code: the code whose reconstruction (`rules::reconstruct`, ffmpeg's) lands nearest the
    /// sample; the predictor follows that reconstruction, so encoder and decoder never drift.
    pub fn encode_sample(&mut self, s: i16) -> u8 {
        let step = STEP_TABLE[clamp_index(self.index) as usize] as i32;
        let diff = s as i32 - self.predictor;
        let sign: u8 = if diff < 0 { 8 } else { 0 };
        let mag = diff.unsigned_abs() as i32;
        // (2d + 1) * step / 8 is nearest mag near d = 4 * mag / step; the codes on either side are tried too.
        let d = (4 * mag / step.max(1)).clamp(0, 7);
        let mut best = d as u8;
        let mut best_err = i32::MAX;
        for c in [d - 1, d, d + 1] {
            if (0..=7).contains(&c) {
                let p = next_predictor(self.predictor, step, sign | c as u8);
                let err = (p - s as i32).abs();
                if err < best_err {
                    best_err = err;
                    best = c as u8;
                }
            }
        }
        let code = sign | best;
        self.predictor = next_predictor(self.predictor, step, code);
        self.index = next_index(clamp_index(self.index), code);
        code
    }

    /// One block from 1..=505 samples; a short block is padded with its last sample. Returns the bytes written (256).
    pub fn encode_block(&mut self, pcm: &[i16], out: &mut [u8; BLOCK_ALIGN]) -> usize {
        let n = pcm.len().clamp(1, SAMPLES_PER_BLOCK);
        let first = pcm.first().copied().unwrap_or(0);
        let last = pcm.get(n - 1).copied().unwrap_or(first);
        self.predictor = first as i32;
        out[0..2].copy_from_slice(&first.to_le_bytes());
        out[2] = clamp_index(self.index) as u8;
        out[3] = 0;
        for i in 0..(SAMPLES_PER_BLOCK - 1) / 2 {
            let a = pcm.get(1 + 2 * i).copied().filter(|_| 1 + 2 * i < n).unwrap_or(last);
            let b = pcm.get(2 + 2 * i).copied().filter(|_| 2 + 2 * i < n).unwrap_or(last);
            let lo = self.encode_sample(a);
            let hi = self.encode_sample(b);
            out[4 + i] = lo | (hi << 4);
        }
        BLOCK_ALIGN
    }
}

/// The decoder (tests, the simulator's fake server).
#[derive(Default)]
pub struct Decoder;

impl Decoder {
    pub fn decode_block(block: &[u8; BLOCK_ALIGN], out: &mut [i16; SAMPLES_PER_BLOCK]) {
        let mut pred = i16::from_le_bytes([block[0], block[1]]) as i32;
        let mut index = clamp_index(block[2] as i32);
        out[0] = pred as i16;
        let mut k = 1;
        for &byte in &block[4..] {
            for code in [byte & 15, byte >> 4] {
                let step = STEP_TABLE[index as usize] as i32;
                pred = next_predictor(pred, step, code);
                index = next_index(index, code);
                out[k] = pred as i16;
                k += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, amp: f64, hz: f64) -> Vec<i16> {
        (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / crate::RATE as f64).sin()) as i16).collect()
    }

    fn roundtrip(pcm: &[i16]) -> Vec<i16> {
        let mut e = Encoder::new();
        let mut out = Vec::new();
        for chunk in pcm.chunks(SAMPLES_PER_BLOCK) {
            let mut b = [0u8; BLOCK_ALIGN];
            e.encode_block(chunk, &mut b);
            assert!(b[2] <= 88);
            assert_eq!(i16::from_le_bytes([b[0], b[1]]), chunk[0]);
            let mut d = [0i16; SAMPLES_PER_BLOCK];
            Decoder::decode_block(&b, &mut d);
            out.extend_from_slice(&d[..chunk.len()]);
        }
        out
    }

    fn snr_db(a: &[i16], b: &[i16]) -> f64 {
        let sig: f64 = a.iter().map(|&x| (x as f64).powi(2)).sum();
        let err: f64 = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum();
        10.0 * (sig / err.max(1.0)).log10()
    }

    #[test]
    fn a_sine_survives() {
        let pcm = sine(SAMPLES_PER_BLOCK * 10, 12000.0, 440.0);
        let back = roundtrip(&pcm);
        assert!(snr_db(&pcm, &back) > 25.0, "snr {}", snr_db(&pcm, &back));
    }

    #[test]
    fn silence_stays_silent() {
        let back = roundtrip(&vec![0i16; 1010]);
        assert!(back.iter().all(|&x| x.abs() <= 8), "{:?}", &back[..20]);
    }

    #[test]
    fn decoder_matches_the_encoders_own_prediction() {
        let pcm = sine(SAMPLES_PER_BLOCK, 9000.0, 300.0);
        let mut e = Encoder::new();
        let mut b = [0u8; BLOCK_ALIGN];
        let mut shadow = Encoder::new();
        e.encode_block(&pcm, &mut b);
        let mut d = [0i16; SAMPLES_PER_BLOCK];
        Decoder::decode_block(&b, &mut d);
        shadow.predictor = pcm[0] as i32;
        for (i, &s) in pcm.iter().enumerate().skip(1) {
            shadow.encode_sample(s);
            assert_eq!(d[i] as i32, shadow.predictor, "sample {i}");
        }
    }

    #[test]
    fn extremes_keep_the_index_in_range() {
        let mut seed = 12345u32;
        let mut noise = Vec::new();
        for i in 0..SAMPLES_PER_BLOCK * 6 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            noise.push(if i % 1000 < 500 { if i % 2 == 0 { 32767 } else { -32768 } } else { (seed >> 16) as i16 });
        }
        let back = roundtrip(&noise);
        assert_eq!(back.len(), noise.len());
    }

    #[test]
    fn a_short_block_is_padded_with_its_last_sample() {
        let mut e = Encoder::new();
        let mut b = [0u8; BLOCK_ALIGN];
        assert_eq!(e.encode_block(&[1000, 1000, 1000], &mut b), 256);
        let mut d = [0i16; SAMPLES_PER_BLOCK];
        Decoder::decode_block(&b, &mut d);
        assert!(d[400..].iter().all(|&x| (x - 1000).abs() < 64));
    }
}
