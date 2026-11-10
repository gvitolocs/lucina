//! Whole files through the decoder, with files made here, and the resampler against what a filter must do.

use std::f64::consts::PI;
use std::path::PathBuf;

use lucina_audio_decode::{Decoder, Resampler};

fn sine(rate: u32, hz: f64, seconds: f64, amplitude: f64) -> Vec<i16> {
    let n = (f64::from(rate) * seconds) as usize;
    (0..n)
        .flat_map(|i| {
            let s = (2.0 * PI * hz * i as f64 / f64::from(rate)).sin() * amplitude;
            [s as i16, s as i16]
        })
        .collect()
}

fn to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

/// The strength of `hz` in a channel of interleaved samples, by correlation, relative to full scale.
fn level(samples: &[i16], rate: u32, hz: f64) -> f64 {
    let left: Vec<f64> = samples.iter().step_by(2).map(|s| f64::from(*s) / 32768.0).collect();
    let (mut re, mut im) = (0.0, 0.0);
    for (i, s) in left.iter().enumerate() {
        let w = 2.0 * PI * hz * i as f64 / f64::from(rate);
        re += s * w.cos();
        im += s * w.sin();
    }
    2.0 * (re * re + im * im).sqrt() / left.len() as f64
}

#[test]
fn a_tone_keeps_its_pitch_and_level_going_from_44100_to_48000() {
    let input = sine(44_100, 1000.0, 2.0, 10000.0);
    let mut resampler = Resampler::new(44_100, 48_000);
    let mut out = Vec::new();
    // In uneven blocks, as a decoder gives them.
    for block in input.chunks(2 * 1152 + 2) {
        resampler.process(block, &mut out);
    }
    resampler.flush(&mut out);
    let samples = to_samples(&out);
    let frames = samples.len() / 2;
    assert!((96_000..=96_030).contains(&frames), "{frames}");
    let steady = &samples[9600..samples.len() - 9600];
    let at_1k = level(steady, 48_000, 1000.0);
    let want = 10000.0 / 32768.0;
    assert!((at_1k - want).abs() / want < 0.01, "{at_1k} against {want}");
    assert!(level(steady, 48_000, 1100.0) < want / 300.0);
    assert!(level(steady, 48_000, 44_100.0 - 1000.0 - 24_000.0) < want / 300.0, "an image");
}

#[test]
fn dc_and_full_scale_survive_without_wrapping() {
    let input = vec![i16::MAX; 96_000];
    let mut resampler = Resampler::new(44_100, 48_000);
    let mut out = Vec::new();
    resampler.process(&input, &mut out);
    let samples = to_samples(&out);
    // Past the filter's start, DC comes through at exactly full scale, since each phase sums to one.
    assert!(samples[200..].iter().all(|s| *s == i16::MAX));
    // A square wave rings past full scale in the filter, and must clip, not wrap.
    let square: Vec<i16> = (0..48_000).map(|i| if i % 200 < 100 { i16::MAX } else { i16::MIN }).collect();
    let mut resampler = Resampler::new(44_100, 48_000);
    let mut out = Vec::new();
    resampler.process(&square, &mut out);
    let samples = to_samples(&out);
    let jumps = samples.windows(4).filter(|w| (i32::from(w[0]) - i32::from(w[2])).abs() > 60_000).count();
    assert!(jumps < samples.len() / 10, "{jumps}");
}

#[test]
fn other_rates_come_out_at_the_right_length() {
    for from in [8_000, 16_000, 22_050, 32_000, 88_200, 96_000, 11_025] {
        let input = sine(from, 300.0, 1.0, 8000.0);
        let mut resampler = Resampler::new(from, 48_000);
        let mut out = Vec::new();
        resampler.process(&input, &mut out);
        resampler.flush(&mut out);
        let frames = out.len() / 4;
        assert!((47_800..=48_300).contains(&frames), "{from} Hz gave {frames}");
        let samples = to_samples(&out);
        let at = level(&samples[4800..samples.len() - 4800], 48_000, 300.0);
        let want = 8000.0 / 32768.0;
        assert!((at - want).abs() / want < 0.02, "{from} Hz: {at} against {want}");
    }
}

fn wav(path: &PathBuf, rate: u32, channels: u16, samples: &[i16]) {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&channels.to_le_bytes());
    f.extend_from_slice(&rate.to_le_bytes());
    f.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    f.extend_from_slice(&(channels * 2).to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(&data);
    std::fs::write(path, f).unwrap();
}

fn decode_all(path: &PathBuf) -> Vec<i16> {
    let mut decoder = Decoder::open(path).unwrap();
    let mut out = Vec::new();
    while decoder.next_block(&mut out).unwrap() {}
    to_samples(&out)
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lucina-audio-decode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn a_48k_stereo_wav_comes_out_bit_exact_and_mono_is_doubled() {
    let path = temp("stereo.wav");
    let input = sine(48_000, 440.0, 0.5, 12_000.0);
    wav(&path, 48_000, 2, &input);
    let decoder = Decoder::open(&path).unwrap();
    assert_eq!(decoder.rate(), 48_000);
    assert_eq!(decoder.duration_ms(), Some(500));
    assert_eq!(decode_all(&path), input);

    let path = temp("mono.wav");
    let mono: Vec<i16> = (0..4800).map(|i| i as i16).collect();
    wav(&path, 48_000, 1, &mono);
    let doubled: Vec<i16> = mono.iter().flat_map(|s| [*s, *s]).collect();
    assert_eq!(decode_all(&path), doubled);
}

#[test]
fn seeking_lands_where_it_says() {
    let path = temp("seek.wav");
    // A ramp, so that a sample says where it is.
    let input: Vec<i16> = (0..48_000 * 4).flat_map(|i| {
        let s = (i / 16) as i16;
        [s, s]
    }).collect();
    wav(&path, 48_000, 2, &input);
    let mut decoder = Decoder::open(&path).unwrap();
    let landed = decoder.seek_ms(2000).unwrap();
    assert!((1900..=2100).contains(&landed), "{landed}");
    let mut out = Vec::new();
    decoder.next_block(&mut out).unwrap();
    let first = i64::from(to_samples(&out)[0]) * 16;
    let at_ms = first * 1000 / 48_000;
    assert!((at_ms - landed as i64).abs() <= 2, "{at_ms} against {landed}");
}

#[test]
fn junk_and_missing_files_are_errors_not_panics() {
    let path = temp("junk.mp3");
    std::fs::write(&path, vec![0x55u8; 4096]).unwrap();
    assert!(Decoder::open(&path).is_err());
    assert!(Decoder::open(&temp("no-such-file.flac")).is_err());
    // Cut in the middle: it plays what is there.
    let path = temp("cut.wav");
    wav(&path, 48_000, 2, &sine(48_000, 440.0, 0.5, 12_000.0));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
    assert!(!decode_all(&path).is_empty());
}

// Most of the library's videos carry HE-AAC, which symphonia decodes only the core of ("aac too complex" on the laptop, 2026-11-03).
// The fixture is a 1 kHz tone at 0.305 of full scale, 2 s, 44.1 kHz stereo: syom's HE-AAC encoder, then put in MP4 by FFmpeg
// (`-c copy`), as symphonia reads FFmpeg's MP4 and not syom's. SBR doubles the core's rate, so the decoder must take the rate from what
// comes out, not from the container.
#[test]
fn he_aac_in_mp4_keeps_its_pitch_length_and_level() {
    let path = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/he-aac-1khz.m4a"));
    let out = decode_all(&path);
    let seconds = out.len() as f64 / 2.0 / 48_000.0;
    assert!((1.9..2.2).contains(&seconds), "{seconds} s");
    // Away from the start, where the codec's priming sits.
    let middle = &out[2 * 9600..2 * 86400];
    let at = level(middle, 48_000, 1000.0);
    assert!((0.2..0.4).contains(&at), "level {at} at 1 kHz, 0.305 went in");
    assert!(level(middle, 48_000, 918.75) < at / 10.0, "the pitch moved");
}
