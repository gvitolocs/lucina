//! The two sample-rate steps around the socket: the microphone's 48 kHz mono to the 16 kHz mono the server takes, and the
//! server's 24 kHz mono speech to the 48 kHz stereo the speakers play. The index arithmetic is in [`crate::rules`] (proved);
//! this file only moves bytes in and out of it.
use crate::rules;

/// One frame sent up: 40 ms of 16 kHz mono s16le (the protocol allows 8 KiB; 20 to 100 ms is the advice).
pub const FRAME_BYTES: usize = 1280;

/// 48 kHz mono s16le in, 16 kHz mono out, with a low-pass in front. State carries over between calls, so any cut of the
/// input gives the same output as one long call.
pub struct Down48to16 {
    carry: Vec<i16>,
    phase: usize,
    odd: Option<u8>,
    scratch: Vec<i16>,
}

impl Default for Down48to16 {
    fn default() -> Self {
        Self::new()
    }
}

impl Down48to16 {
    pub fn new() -> Self {
        Down48to16 { carry: vec![0; rules::HIST], phase: 0, odd: None, scratch: Vec::new() }
    }

    /// Samples as they are, appended to `out` after the low-pass and the 3:1 decimation.
    pub fn push(&mut self, samples: &[i16], out: &mut Vec<i16>) {
        self.scratch.clear();
        self.scratch.extend_from_slice(&self.carry);
        self.scratch.extend_from_slice(samples);
        self.phase = rules::decimate(&self.scratch, self.phase, out);
        let from = self.scratch.len() - rules::HIST;
        self.carry.copy_from_slice(&self.scratch[from..]);
    }

    /// Microphone bytes (s16le), which may end in half a sample; that byte waits for the next call.
    pub fn push_bytes(&mut self, bytes: &[u8], out: &mut Vec<i16>) {
        let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
        let mut rest = bytes;
        if let Some(lo) = self.odd.take() {
            if let Some((&hi, tail)) = rest.split_first() {
                samples.push(i16::from_le_bytes([lo, hi]));
                rest = tail;
            } else {
                self.odd = Some(lo);
                return;
            }
        }
        let (whole, left) = rules::split_samples(rest.len());
        for i in 0..whole {
            samples.push(i16::from_le_bytes([rest[2 * i], rest[2 * i + 1]]));
        }
        if left == 1 {
            self.odd = Some(rest[rest.len() - 1]);
        }
        self.push(&samples, out);
    }
}

/// Cuts a stream of samples into frames of [`FRAME_BYTES`] bytes.
#[derive(Default)]
pub struct Framer {
    buf: Vec<u8>,
}

impl Framer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds samples; every complete frame goes to `emit`.
    pub fn push(&mut self, samples: &[i16], mut emit: impl FnMut(Vec<u8>)) {
        for s in samples {
            self.buf.extend_from_slice(&s.to_le_bytes());
        }
        while self.buf.len() >= FRAME_BYTES {
            let rest = self.buf.split_off(FRAME_BYTES);
            emit(std::mem::replace(&mut self.buf, rest));
        }
    }

    /// What is left, shorter than a frame (empty when nothing is).
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

/// The server's 24 kHz mono s16le speech to the 48 kHz stereo s16le that `/dev/lucina-audio` plays. A sample split between two
/// frames waits for its second byte.
pub struct Up24to48 {
    prev: i16,
    odd: Option<u8>,
}

impl Default for Up24to48 {
    fn default() -> Self {
        Self::new()
    }
}

impl Up24to48 {
    pub fn new() -> Self {
        Up24to48 { prev: 0, odd: None }
    }

    /// Appends the 4 samples (2 stereo frames) each input sample becomes, as bytes.
    pub fn push_bytes(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        let mut samples = Vec::with_capacity(bytes.len() / 2 + 1);
        let mut rest = bytes;
        if let Some(lo) = self.odd.take() {
            if let Some((&hi, tail)) = rest.split_first() {
                samples.push(i16::from_le_bytes([lo, hi]));
                rest = tail;
            } else {
                self.odd = Some(lo);
                return;
            }
        }
        let (whole, left) = rules::split_samples(rest.len());
        for i in 0..whole {
            samples.push(i16::from_le_bytes([rest[2 * i], rest[2 * i + 1]]));
        }
        if left == 1 {
            self.odd = Some(rest[rest.len() - 1]);
        }
        let mut wide = Vec::with_capacity(samples.len() * 4);
        self.prev = rules::expand(&samples, self.prev, &mut wide);
        out.reserve(wide.len() * 2);
        for s in wide {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: f64, hz: f64, n: usize, amp: f64) -> Vec<i16> {
        (0..n).map(|i| (amp * (2.0 * std::f64::consts::PI * hz * i as f64 / rate).sin()) as i16).collect()
    }

    fn rms(v: &[i16]) -> f64 {
        (v.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / v.len() as f64).sqrt()
    }

    #[test]
    fn three_to_one_and_any_cut_gives_the_same_samples() {
        let input = sine(48000.0, 1000.0, 4800, 10000.0);
        let mut whole = Vec::new();
        Down48to16::new().push(&input, &mut whole);
        assert_eq!(whole.len(), 1600);
        for cut in [1usize, 2, 3, 7, 100, 640, 1919] {
            let mut d = Down48to16::new();
            let mut got = Vec::new();
            for chunk in input.chunks(cut) {
                d.push(chunk, &mut got);
            }
            assert_eq!(got, whole, "cut {cut}");
        }
    }

    #[test]
    fn odd_byte_cuts_give_the_same_samples() {
        let input = sine(48000.0, 440.0, 3000, 8000.0);
        let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut whole = Vec::new();
        Down48to16::new().push(&input, &mut whole);
        let mut d = Down48to16::new();
        let mut got = Vec::new();
        for chunk in bytes.chunks(333) {
            d.push_bytes(chunk, &mut got);
        }
        assert_eq!(got, whole);
    }

    #[test]
    fn speech_passes_and_aliasing_tones_are_cut() {
        // 1 kHz is speech; 12 kHz would fold to 4 kHz at 16 kHz without the low-pass.
        let mut keep = Vec::new();
        Down48to16::new().push(&sine(48000.0, 1000.0, 48000, 10000.0), &mut keep);
        let mut fold = Vec::new();
        Down48to16::new().push(&sine(48000.0, 12000.0, 48000, 10000.0), &mut fold);
        let keep_rms = rms(&keep[100..]);
        let fold_rms = rms(&fold[100..]);
        assert!(keep_rms > 6500.0, "1 kHz kept: {keep_rms}");
        assert!(fold_rms < keep_rms / 30.0, "12 kHz cut: {fold_rms} vs {keep_rms}");
    }

    #[test]
    fn dc_keeps_its_level_and_loud_input_does_not_wrap() {
        let mut out = Vec::new();
        Down48to16::new().push(&vec![20000i16; 960], &mut out);
        assert!((out[100] as i32 - 20000).abs() <= 2, "dc {}", out[100]);
        let mut loud = Vec::new();
        Down48to16::new().push(&sine(48000.0, 700.0, 4800, 32767.0), &mut loud);
        assert!(loud.iter().all(|s| *s > -32768 || *s == -32768));
        assert!(rms(&loud[50..]) > 20000.0);
    }

    #[test]
    fn frames_are_forty_milliseconds() {
        let mut f = Framer::new();
        let mut got = Vec::new();
        f.push(&vec![1i16; 1000], |fr| got.push(fr));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), FRAME_BYTES);
        assert_eq!(f.finish().len(), 2000 - FRAME_BYTES);
        assert!(f.finish().is_empty());
    }

    #[test]
    fn speech_is_four_samples_per_sample_and_a_split_sample_waits() {
        let samples: Vec<i16> = vec![100, 300, -200, 32767, -32768];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut whole = Vec::new();
        Up24to48::new().push_bytes(&bytes, &mut whole);
        assert_eq!(whole.len(), samples.len() * 8);
        let mut cut = Vec::new();
        let mut u = Up24to48::new();
        u.push_bytes(&bytes[..3], &mut cut);
        u.push_bytes(&bytes[3..4], &mut cut);
        u.push_bytes(&bytes[4..], &mut cut);
        assert_eq!(cut, whole);
        let at = |i: usize| i16::from_le_bytes([whole[2 * i], whole[2 * i + 1]]);
        // First input sample 100 after a silent start: midpoint 50, then 100, left and right.
        assert_eq!((at(0), at(1), at(2), at(3)), (50, 50, 100, 100));
        // Second: midpoint of 100 and 300.
        assert_eq!((at(4), at(5), at(6), at(7)), (200, 200, 300, 300));
    }
}
