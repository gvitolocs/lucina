// The integer rules under Fortuna's voice notes: IMA-ADPCM's step index and predictor, and the record's framing (how
// many 505-sample blocks a note takes, the WAV's sizes, the length cap). The microphone's ring is boards/fortuna-ring. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011).
// `adpcm.rs`, `ring.rs` and `wav.rs` call these functions and keep no copy of the rules.

/// The step index of IMA-ADPCM stays within its table (0..=88).
pub fn clamp_index(i: i32) -> i32 //@ret
//@ ensures 0 <= ret <= 88, (0 <= i <= 88) ==> ret == i, i < 0 ==> ret == 0, i > 88 ==> ret == 88,
{
    if i < 0 {
        0
    } else if i > 88 {
        88
    } else {
        i
    }
}

/// A predicted sample stays a 16-bit sample.
pub fn clamp_sample(v: i32) -> i32 //@ret
//@ ensures -32768 <= ret <= 32767, (-32768 <= v <= 32767) ==> ret == v, v < -32768 ==> ret == -32768, v > 32767 ==> ret == 32767,
{
    if v < -32768 {
        -32768
    } else if v > 32767 {
        32767
    } else {
        v
    }
}

/// The size of the change a code stands for (its low three bits; the sign bit is left to `next_predictor`):
/// `((2 * d + 1) * step) / 8`, the reconstruction of ffmpeg's `adpcm_ima_wav` decoder, which is what reads these
/// notes on the server. (Windows' imaadp32 sums shifted steps instead; the two differ by a few units.)
pub fn reconstruct(step: i32, code: u8) -> i32 //@ret
//@ requires 0 <= step <= 32767,
//@ ensures 0 <= ret <= 61438,
{
    //@ assert((code & 7u8) <= 7u8) by (bit_vector);
    let d = (code & 7) as i32;
    //@ assert(0 <= (2 * d + 1) * step <= 15 * 32767) by (nonlinear_arith) requires 0 <= d <= 7, 0 <= step <= 32767;
    ((2 * d + 1) * step) / 8
}

/// The next predictor: the change added or (sign bit 8) taken away, kept in 16 bits.
pub fn next_predictor(pred: i32, step: i32, code: u8) -> i32 //@ret
//@ requires -32768 <= pred <= 32767, 0 <= step <= 32767,
//@ ensures -32768 <= ret <= 32767,
{
    let d = reconstruct(step, code);
    if code & 8 != 0 {
        clamp_sample(pred - d)
    } else {
        clamp_sample(pred + d)
    }
}

/// The next step index: down one for a small code, up 2/4/6/8 for larger ones (IMA's index table), kept in 0..=88.
pub fn next_index(i: i32, code: u8) -> i32 //@ret
//@ requires 0 <= i <= 88,
//@ ensures 0 <= ret <= 88, (code & 7) < 4 ==> ret == if i > 0 { i - 1 } else { 0 },
{
    let delta = match code & 7 {
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 8,
        _ => -1,
    };
    clamp_index(i + delta)
}

/// Whether a WAV of `blocks` 256-byte blocks keeps its RIFF size (52 + data) within 32 bits.
pub fn wav_fits(blocks: u32) -> bool //@ret
//@ ensures ret == (blocks <= (0xffff_ffffu32 - 60) / 256),
{
    blocks <= (0xffff_ffffu32 - 60) / 256
}

/// Samples in one IMA-ADPCM block of the WAV (`adpcm::SAMPLES_PER_BLOCK`).
pub const BLOCK_SAMPLES: u32 = 505;

/// Blocks a note of `samples` takes: enough for every sample, and not one more (the last may be part padding).
pub fn blocks_for(samples: u32) -> u32 //@ret
//@ ensures ret * 505 >= samples, ret == 0 || (ret - 1) * 505 < samples,
{
    let full = samples / BLOCK_SAMPLES;
    //@ assert(samples as int == full as int * 505 + (samples % 505) as int && (samples % 505) < 505) by (nonlinear_arith) requires full == samples / 505;
    if samples % BLOCK_SAMPLES == 0 {
        full
    } else {
        full + 1
    }
}

/// The WAV's data size and RIFF size for `blocks` blocks of 256 bytes, or `None` past 32 bits.
pub fn wav_sizes(blocks: u32) -> Option<(u32, u32)> //@ret
//@ ensures match ret { Some((data, riff)) => data == blocks * 256 && riff == data + 52 && riff <= 0xffff_ffff, None => blocks > (0xffff_ffffu32 - 60) / 256 },
{
    if !wav_fits(blocks) {
        return None;
    }
    let data = blocks * 256;
    Some((data, data + 52))
}

/// Whether a note of `samples` at `rate` has reached `cap_secs`.
pub fn cap_reached(samples: u32, rate: u32, cap_secs: u32) -> bool //@ret
//@ ensures ret == (samples as int >= rate as int * cap_secs as int),
{
    //@ assert(0 <= rate as int * cap_secs as int <= 0xffff_ffff * 0xffff_ffff) by (nonlinear_arith);
    samples as u64 >= rate as u64 * cap_secs as u64
}

/// The levels one byte of a note's waveform strip covers: `[start, end)` of `n` levels shared out over `count`
/// bytes, each byte the loudest of its slice (the iPhone's `VoiceWaveform.resample`). Never empty, never past the end.
pub fn bucket(b: u32, n: u32, count: u32) -> (u32, u32) //@ret
//@ requires 0 < count, count < n, n <= 40_000, b < count,
//@ ensures ret.0 < ret.1, ret.1 <= n, ret.0 as int == (b as int * n as int) / (count as int),
{
    //@ assert((b as int) * (n as int) <= 40_000 * 40_000 && (b as int + 1) * (n as int) <= 40_000 * 40_000) by (nonlinear_arith) requires b < count, count < n, n <= 40_000;
    //@ assert((b as int) * (n as int) < (count as int) * (n as int) && (b as int + 1) * (n as int) <= (count as int) * (n as int)) by (nonlinear_arith) requires b < count, 0 < n;
    //@ assert((b as int) * (n as int) / (count as int) < n as int && (b as int + 1) * (n as int) / (count as int) <= n as int) by (nonlinear_arith) requires (b as int) * (n as int) < (count as int) * (n as int), (b as int + 1) * (n as int) <= (count as int) * (n as int), 0 < count;
    let start = b * n / count;
    let end = (b + 1) * n / count;
    if end > start {
        (start, end)
    } else {
        (start, start + 1)
    }
}
