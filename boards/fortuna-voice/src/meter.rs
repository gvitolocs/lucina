//! The recording's level meter (12 bars on a log scale, falling one bar a frame) and its timer ("0:07").

/// Peak levels for bars 1..=12, about -42 dBFS to -2 dBFS.
pub const LEVELS: [u16; 12] = [260, 370, 520, 740, 1040, 1470, 2070, 2930, 4130, 5840, 8240, 11640];

#[derive(Default, Debug, Clone, Copy)]
pub struct Meter {
    peak: u16,
    shown: u8,
}

impl Meter {
    pub fn feed(&mut self, pcm: &[i16]) {
        for &s in pcm {
            let a = s.unsigned_abs().min(32767);
            if a > self.peak {
                self.peak = a;
            }
        }
    }

    /// Bars lit since the last call, 0..=12.
    pub fn level(&mut self) -> u8 {
        let mut bars = 0u8;
        while (bars as usize) < LEVELS.len() && self.peak >= LEVELS[bars as usize] {
            bars += 1;
        }
        self.peak = 0;
        self.shown = if bars >= self.shown { bars } else { self.shown - 1 };
        self.shown
    }
}

/// "m:ss" of `samples` at `rate`, up to "99:59".
pub fn timer_text(samples: u32, rate: u32, out: &mut [u8; 5]) -> usize {
    let s = (samples / rate.max(1)).min(99 * 60 + 59);
    let (m, s) = (s / 60, s % 60);
    let mut n = 0;
    if m >= 10 {
        out[n] = b'0' + (m / 10) as u8;
        n += 1;
    }
    out[n] = b'0' + (m % 10) as u8;
    out[n + 1] = b':';
    out[n + 2] = b'0' + (s / 10) as u8;
    out[n + 3] = b'0' + (s % 10) as u8;
    n + 4
}

pub fn cap_reached(samples: u32, rate: u32, cap_secs: u32) -> bool {
    crate::rules::cap_reached(samples, rate, cap_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_rise_at_once_and_fall_one_at_a_time() {
        let mut m = Meter::default();
        m.feed(&[0, -32768, 5]);
        assert_eq!(m.level(), 12);
        m.feed(&[100]);
        assert_eq!(m.level(), 11);
        assert_eq!(m.level(), 10);
        m.feed(&[1500]);
        assert_eq!(m.level(), 9);
        m.feed(&[3000]);
        assert_eq!(m.level(), 8);
    }

    #[test]
    fn timers() {
        let mut b = [0u8; 5];
        let n = timer_text(7 * 10_909, 10_909, &mut b);
        assert_eq!(&b[..n], b"0:07");
        let n = timer_text(600 * 10_909, 10_909, &mut b);
        assert_eq!(&b[..n], b"10:00");
        assert!(cap_reached(60 * 10_909, 10_909, 60));
        assert!(!cap_reached(60 * 10_909 - 1, 10_909, 60));
    }
}
