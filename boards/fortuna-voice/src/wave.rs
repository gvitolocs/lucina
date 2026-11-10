//! A voice note's waveform strip, the one the iPhone sends with its notes (`meta.files[i].waveform`, Secretary's
//! `VoiceWaveform`): the loudness of every 50 ms of the note, on the scale −50 dB … 0 dB with a 1.6 power curve, then
//! at most 64 bytes, each the loudest of its slice of the note (0 silent … 255 full scale). The server keeps it with
//! the note, and every app draws its bars from it, the DSi included.
//!
//! Integer only: the mean square of each window is compared with the 50 thresholds of whole decibels (`THRESH`), and
//! the curve is a table (`CURVE`); a host test checks both against the floating-point formula.

/// Samples in 50 ms at `crate::RATE`.
pub const WINDOW: u32 = crate::RATE / 20;
/// The strip's length, at most (the server takes 1 to 64 bytes).
pub const BYTES: usize = 64;
/// Windows kept: one minute, the note's cap.
const MAX_WINDOWS: usize = 60 * 20;

/// `THRESH[k - 1]`: the mean square (of 16-bit samples) at `k - 50` dB full scale, for k in 1..=50.
const THRESH: [u64; 50] = [
    13518, 17018, 21424, 26971, 33955, 42746, 53815, 67749, 85290, 107374, 135176, 170177, 214240, 269712, 339547,
    427464, 538146, 677485, 852903, 1073742, 1351761, 1701766, 2142397, 2697118, 3395470, 4274643, 5381457, 6774853,
    8529034, 10737418, 13517609, 17017661, 21423966, 26971175, 33954698, 42746432, 53814569, 67748529, 85290345,
    107374182, 135176087, 170176611, 214239660, 269711752, 339546978, 427464319, 538145694, 677485290, 852903448,
    1073741824,
];
/// `CURVE[i]`: (i / 50)^1.6 × 255, rounded: the byte for a window `i` dB above −50 dB.
const CURVE: [u8; 51] = [
    0, 0, 1, 3, 4, 6, 9, 11, 14, 16, 19, 23, 26, 30, 33, 37, 41, 45, 50, 54, 59, 64, 69, 74, 79, 84, 90, 95, 101, 107,
    113, 119, 125, 131, 138, 144, 151, 158, 164, 171, 178, 186, 193, 200, 208, 215, 223, 231, 239, 247, 255,
];

/// Whole decibels above −50 dB full scale (0..=50) of a window's mean square.
fn level(mean_sq: u64) -> u8 {
    THRESH.iter().take_while(|t| mean_sq >= **t).count() as u8
}

/// Collects the levels while a note records.
pub struct Wave {
    sum: u64,
    n: u32,
    levels: [u8; MAX_WINDOWS],
    len: usize,
}

impl Default for Wave {
    fn default() -> Self {
        Self::new()
    }
}

impl Wave {
    pub const fn new() -> Wave {
        Wave { sum: 0, n: 0, levels: [0; MAX_WINDOWS], len: 0 }
    }

    pub fn push(&mut self, s: i16) {
        let v = s as i64;
        self.sum += (v * v) as u64;
        self.n += 1;
        if self.n == WINDOW {
            self.close();
        }
    }

    fn close(&mut self) {
        if self.n > 0 && self.len < MAX_WINDOWS {
            self.levels[self.len] = level(self.sum / self.n as u64);
            self.len += 1;
        }
        self.sum = 0;
        self.n = 0;
    }

    /// The strip: `BYTES` at most (fewer for a note under 3.2 s: one byte a window), and its length.
    pub fn finish(mut self) -> ([u8; BYTES], usize) {
        if self.n >= WINDOW / 2 {
            self.close();
        }
        let mut out = [0u8; BYTES];
        let n = self.len;
        if n <= BYTES {
            for (o, l) in out.iter_mut().zip(&self.levels[..n]) {
                *o = CURVE[*l as usize];
            }
            return (out, n);
        }
        for (b, o) in out.iter_mut().enumerate() {
            let (start, end) = crate::rules::bucket(b as u32, n as u32, BYTES as u32);
            let loudest = self.levels[start as usize..end as usize].iter().copied().max().unwrap_or(0);
            *o = CURVE[loudest as usize];
        }
        (out, BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tables_are_the_iphones_formula() {
        let full = 32768f64 * 32768f64;
        for k in 1..=50 {
            let want = (full * 10f64.powf((k as f64 - 50.0) / 10.0)).round() as u64;
            assert_eq!(THRESH[k - 1], want, "threshold {k}");
        }
        for i in 0..=50 {
            assert_eq!(CURVE[i], ((i as f64 / 50.0).powf(1.6) * 255.0).round() as u8, "curve {i}");
        }
    }

    #[test]
    fn levels_follow_the_decibels() {
        // full-scale square wave: 0 dB; silence: below −50 dB; −20 dB: 30
        assert_eq!(level(32767 * 32767), 49);
        assert_eq!(level(32768 * 32768), 50);
        assert_eq!(level(0), 0);
        let minus20 = (32768.0f64 * 32768.0 * 0.01) as u64;
        assert_eq!(level(minus20), 30);
        assert_eq!(level(minus20 - 1), 29);
    }

    fn note(windows: &[i16]) -> ([u8; BYTES], usize) {
        let mut w = Wave::new();
        for amp in windows {
            for i in 0..WINDOW {
                w.push(if i % 2 == 0 { *amp } else { -*amp });
            }
        }
        w.finish()
    }

    #[test]
    fn a_short_note_gives_one_byte_a_window() {
        let (b, n) = note(&[0, 3277, 32767]);
        assert_eq!(n, 3);
        // silence, −20 dB (level 30 → 113), full scale (level 49 → 247)
        assert_eq!(&b[..3], &[0, 113, 247]);
    }

    #[test]
    fn a_long_note_keeps_the_loudest_of_each_slice() {
        // 10 s: 200 windows, quiet, with one loud window in the middle
        let mut w = [100i16; 200];
        w[101] = 32767;
        let (b, n) = note(&w);
        assert_eq!(n, BYTES);
        let loud: Vec<usize> = (0..BYTES).filter(|i| b[*i] == 247).collect();
        assert_eq!(loud.len(), 1);
        // window 101 of 200 falls in byte 101 * 64 / 200 = 32
        assert_eq!(loud[0], 32);
        assert!(b.iter().enumerate().all(|(i, v)| i == 32 || *v == CURVE[level(100 * 100) as usize]));
    }

    #[test]
    fn the_cap_stops_collecting() {
        let mut w = Wave::new();
        for _ in 0..(MAX_WINDOWS as u32 + 50) * WINDOW {
            w.push(1000);
        }
        let (_, n) = w.finish();
        assert_eq!(n, BYTES);
    }

    #[test]
    fn buckets_cover_every_level_once() {
        for n in [65u32, 100, 200, 1199, 1200] {
            let mut next = 0;
            for b in 0..BYTES as u32 {
                let (s, e) = crate::rules::bucket(b, n, BYTES as u32);
                assert!(s < e && e <= n);
                assert!(s <= next, "a gap before byte {b} of {n}");
                next = e;
            }
            assert_eq!(next, n);
        }
    }
}
