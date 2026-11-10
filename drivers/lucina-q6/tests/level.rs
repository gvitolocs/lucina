//! The ceiling and the tone, which stand between a bug and the speakers.

use lucina_q6::level::{
    CEILING_DB, DEFAULT_DB, Gain, SILENCE_DB, TONE_FRAMES, TONE_HZ, TONE_PEAK, TONE_RATE, Tone,
};

fn db_to_q15(db: i32) -> f64 {
    32768.0 * 10f64.powf(f64::from(db) / 20.0)
}

#[test]
fn every_step_of_the_table_is_that_many_decibels() {
    for db in SILENCE_DB..=CEILING_DB {
        let want = db_to_q15(db).round();
        assert_eq!(f64::from(Gain::from_db(db).q15()), want, "{db} dB");
    }
}

#[test]
fn nothing_gets_past_the_ceiling() {
    let ceiling = db_to_q15(CEILING_DB).round() as u16;
    for db in [CEILING_DB, -11, -1, 0, 1, 12, 100, i32::MAX] {
        let gain = Gain::from_db(db);
        assert_eq!(gain.q15(), ceiling, "{db} dB");
        assert_eq!(gain.db(), CEILING_DB);
    }
    // Full scale in, -12 dB out, on both extremes.
    let mut pcm = [0u8; 8];
    pcm[0..2].copy_from_slice(&i16::MAX.to_le_bytes());
    pcm[2..4].copy_from_slice(&i16::MIN.to_le_bytes());
    pcm[4..6].copy_from_slice(&i16::MIN.to_le_bytes());
    pcm[6..8].copy_from_slice(&i16::MAX.to_le_bytes());
    Gain::from_db(100).apply(&mut pcm);
    for s in pcm.chunks_exact(2) {
        let s = i32::from(i16::from_le_bytes([s[0], s[1]]));
        assert!(s.abs() <= i32::from(ceiling), "{s}");
    }
}

#[test]
fn a_stream_starts_quiet_and_silence_is_silent() {
    assert_eq!(Gain::default().db(), DEFAULT_DB);
    assert_eq!(Gain::default().q15(), db_to_q15(-30).round() as u16);
    assert_eq!(Gain::from_db(i32::MIN).q15(), 0);
    let mut pcm = i16::MAX.to_le_bytes();
    Gain::from_db(i32::MIN).apply(&mut pcm);
    assert_eq!(i16::from_le_bytes(pcm), 0);
}

fn the_tone() -> Vec<i16> {
    let mut tone = Tone::new();
    let mut all = Vec::new();
    // In chunks that do not divide a period, with a spare byte, as the kernel may ask.
    let mut chunk = [0u8; 3843];
    while !tone.finished() {
        let n = tone.fill(&mut chunk);
        assert_eq!(n % 4, 0);
        all.extend(
            chunk[..n]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]])),
        );
    }
    all
}

#[test]
fn the_tone_is_minus_30_db_for_two_seconds_at_1_khz() {
    let samples = the_tone();
    assert_eq!(samples.len() as u32, TONE_FRAMES * 2);
    assert_eq!(TONE_FRAMES, 96_000);
    assert_eq!((TONE_RATE, TONE_HZ), (48_000, 1000));
    assert_eq!(TONE_PEAK, db_to_q15(-30).round() as i16);
    let peak = samples.iter().map(|s| i32::from(*s).abs()).max().unwrap();
    assert_eq!(peak, i32::from(TONE_PEAK));
    // Exactly -30 dB, and no louder anywhere.
    let dbfs = 20.0 * (f64::from(peak) / 32768.0).log10();
    assert!((dbfs + 30.0).abs() < 0.01, "{dbfs}");

    let left: Vec<i32> = samples.iter().step_by(2).map(|s| i32::from(*s)).collect();
    let right: Vec<i32> = samples
        .iter()
        .skip(1)
        .step_by(2)
        .map(|s| i32::from(*s))
        .collect();
    assert_eq!(left, right);
    // No DC to speak of (the ramps round towards zero), and a thousand positive-going zero crossings a second.
    let sum: i64 = left.iter().map(|s| i64::from(*s)).sum();
    assert!(sum.abs() < left.len() as i64 / 1000, "{sum}");
    let crossings = left.windows(2).filter(|w| w[0] <= 0 && w[1] > 0).count();
    assert!((1999..=2001).contains(&crossings), "{crossings}");
    // It starts and ends at zero: no click.
    assert_eq!(left[0], 0);
    assert_eq!(*left.last().unwrap(), 0);
    assert!(left[..24].iter().all(|s| s.abs() < 200));
    // A finished tone gives nothing more.
    let mut tone = Tone::new();
    let mut all = vec![0u8; TONE_FRAMES as usize * 4 + 100];
    assert_eq!(tone.fill(&mut all), TONE_FRAMES as usize * 4);
    assert_eq!(tone.fill(&mut all), 0);
}

#[test]
fn the_tones_checksum_is_what_the_qemu_test_expects() {
    let bytes: Vec<u8> = the_tone().iter().flat_map(|s| s.to_le_bytes()).collect();
    assert_eq!(lucina_q6::level::crc32_update(0, &bytes), TONE_CRC);
    // As the kernel feeds it, a period at a time.
    let crc = bytes.chunks(3840).fold(0, lucina_q6::level::crc32_update);
    assert_eq!(crc, TONE_CRC);
    assert_eq!(lucina_q6::level::crc32_update(0, b"123456789"), 0xcbf4_3926);
}

/// `tests/guest/audio.sh` checks the same number.
const TONE_CRC: u32 = 0x8789ce91;

// The laptop reset when the plain address went to the DSP (2026-11-03): `iommus` of `dais` is 0x1001 there.
#[test]
fn the_dsp_is_given_its_stream_in_the_address() {
    use lucina_q6::stream::dsp_address;
    assert_eq!(dsp_address(0xaad0_0000, 0x1001), Some(0x1_aad0_0000));
    assert_eq!(dsp_address(0xffff_f000, 0x1001), Some(0x1_ffff_f000));
    // Memory it cannot reach: the laptop's allocator gave 0x1_1fd1_0000 on one boot, which would read as stream 1 at 0x1fd1_0000.
    assert_eq!(dsp_address(0x1_1fd1_0000, 0x1001), None);
    // Only the low bits are the stream.
    assert_eq!(dsp_address(0x8000_0000, 0x1012), Some(0x2_8000_0000));
}

// `rules::scale` is written for Verus. What ships must still be what the C-like shift gave, for every sample at every volume.
#[test]
fn scale_is_the_arithmetic_shift_for_every_sample_at_every_volume() {
    use lucina_q6::level::{CEILING_DB, Gain, SILENCE_DB};
    use lucina_q6::rules::scale;
    for db in SILENCE_DB..=CEILING_DB {
        let q15 = Gain::from_db(db).q15();
        for s in i16::MIN..=i16::MAX {
            let shifted = (i32::from(s) * i32::from(q15)) >> 15;
            assert_eq!(i32::from(scale(s, q15)), shifted, "{s} at {db} dB");
        }
    }
}

#[test]
fn the_tone_sample_is_what_the_table_and_ramp_gave_before() {
    use lucina_q6::rules::tone_sample;
    for raw in -1036i16..=1036 {
        for ramp in 0u32..=240 {
            assert_eq!(
                i32::from(tone_sample(raw, ramp)),
                i32::from(raw) * ramp as i32 / 240
            );
        }
    }
    assert_eq!(tone_sample(i16::MAX, 240), 1036);
    assert_eq!(tone_sample(i16::MIN, 240), -1036);
}

#[test]
fn the_amplifier_gate_needs_a_running_stream_and_a_genuine_period() {
    use lucina_q6::rules::{gate_is_on, gate_off, gate_running, gate_write_done};
    let off = gate_off();
    assert!(!gate_is_on(off));
    assert!(!gate_is_on(gate_write_done(off, true)));
    let running = gate_running();
    assert!(!gate_is_on(running));
    assert!(!gate_is_on(gate_write_done(running, false)));
    let on = gate_write_done(running, true);
    assert!(gate_is_on(on));
    assert!(!gate_is_on(gate_off()));
}
