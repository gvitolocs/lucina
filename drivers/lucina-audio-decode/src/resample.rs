//! Rate conversion by a polyphase windowed-sinc filter: `to / from` in lowest terms gives the number of phases, and each output sample
//! is 24 input samples through one phase. 44.1 kHz to 48 kHz is 160 phases. With no float in the per-sample path a slow CPU pays
//! only 24 multiplications per sample per channel.

/// Taps per phase. At 44.1 kHz the transition band ends at about 22 kHz with this and a Kaiser window, enough for a laptop's speakers.
const TAPS: usize = 24;
/// The most phases kept. An odd pair of rates with a bigger numerator is rounded to this, which detunes it by a few cents at worst: no real file has such a rate.
const MAX_PHASES: u32 = 1024;
const KAISER_BETA: f64 = 8.0;
const COEFF_SHIFT: u32 = 15;

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The zeroth modified Bessel function, by its series.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..32 {
        term *= q / f64::from(k * k);
        sum += term;
    }
    sum
}

pub struct Resampler {
    phases: u32,
    step: u32,
    /// `TAPS` coefficients for each phase, in Q15, each phase summing to exactly 1.
    table: Vec<i16>,
    /// The last `TAPS` frames of input, then what has come since, interleaved.
    history: Vec<i16>,
    /// Which phase next, and how many frames into `history`.
    phase: u32,
    frame: usize,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        let g = gcd(from, to).max(1);
        let (mut phases, mut step) = (to / g, from / g);
        if phases > MAX_PHASES {
            let scale = f64::from(MAX_PHASES) / f64::from(phases);
            step = ((f64::from(step) * scale).round() as u32).max(1);
            phases = MAX_PHASES;
        }
        // Going down, the cut-off follows the output's Nyquist frequency.
        let cutoff = (f64::from(to) / f64::from(from)).min(1.0) * 0.97;
        let mut table = Vec::with_capacity(phases as usize * TAPS);
        let centre = (TAPS as f64 - 1.0) / 2.0;
        let denominator = bessel_i0(KAISER_BETA);
        for p in 0..phases {
            let frac = f64::from(p) / f64::from(phases);
            let raw: Vec<f64> = (0..TAPS)
                .map(|n| {
                    let t = n as f64 - centre - frac;
                    let sinc = if t.abs() < 1e-9 {
                        1.0
                    } else {
                        (std::f64::consts::PI * cutoff * t).sin() / (std::f64::consts::PI * cutoff * t)
                    };
                    let r = (t / (centre + 1.0)).clamp(-1.0, 1.0);
                    sinc * bessel_i0(KAISER_BETA * (1.0 - r * r).sqrt()) / denominator
                })
                .collect();
            let sum: f64 = raw.iter().sum();
            let mut fixed: Vec<i16> = raw
                .iter()
                .map(|c| (c / sum * f64::from(1u32 << COEFF_SHIFT)).round() as i16)
                .collect();
            // Exactly unity, so that DC goes through unchanged: the rounding error goes on the largest tap.
            let total: i32 = fixed.iter().map(|c| i32::from(*c)).sum();
            let largest = (0..TAPS).max_by_key(|i| fixed[*i].unsigned_abs()).unwrap_or(0);
            fixed[largest] = (i32::from(fixed[largest]) + (1 << COEFF_SHIFT) - total) as i16;
            table.extend_from_slice(&fixed);
        }
        Resampler {
            phases,
            step,
            table,
            history: vec![0; TAPS * 2],
            phase: 0,
            frame: 0,
        }
    }

    pub fn reset(&mut self) {
        self.history.clear();
        self.history.resize(TAPS * 2, 0);
        self.phase = 0;
        self.frame = 0;
    }

    /// Interleaved stereo in, S16LE stereo appended to `out`.
    pub fn process(&mut self, stereo: &[i16], out: &mut Vec<u8>) {
        self.history.extend_from_slice(stereo);
        let frames = self.history.len() / 2;
        while self.frame + TAPS <= frames {
            let coeffs = &self.table[self.phase as usize * TAPS..(self.phase as usize + 1) * TAPS];
            let window = &self.history[self.frame * 2..(self.frame + TAPS) * 2];
            let (mut left, mut right) = (0i32, 0i32);
            for (c, frame) in coeffs.iter().zip(window.chunks_exact(2)) {
                left += i32::from(*c) * i32::from(frame[0]);
                right += i32::from(*c) * i32::from(frame[1]);
            }
            let round = 1 << (COEFF_SHIFT - 1);
            let clamp = |v: i32| ((v + round) >> COEFF_SHIFT).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            out.extend_from_slice(&clamp(left).to_le_bytes());
            out.extend_from_slice(&clamp(right).to_le_bytes());
            self.phase += self.step;
            self.frame += (self.phase / self.phases) as usize;
            self.phase %= self.phases;
        }
        // Keep what the next call still needs.
        let keep_from = self.frame.min(frames);
        self.history.drain(..keep_from * 2);
        self.frame -= keep_from;
    }

    /// The filter's tail at the end of a file.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        let zeros = vec![0i16; TAPS * 2];
        self.process(&zeros, out);
    }
}
