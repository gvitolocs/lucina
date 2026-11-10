//! Opus through `Decoder` (files) and `PacketDecoder` (packets), against FFmpeg's libopus as the yardstick. Clips are made with FFmpeg at
//! test time; without FFmpeg the tests print a reason and pass.

use std::path::{Path, PathBuf};
use std::process::Command;

use lucina_audio_decode::packet::{AudioConfig, PacketDecoder};
use lucina_audio_decode::Decoder;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lucina-audio-decode-opus-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn ffmpeg(args: &[&str]) -> Option<PathBuf> {
    let out = args.last()?;
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(&args[..args.len() - 1])
        .arg(out)
        .status()
        .ok()?;
    status.success().then(|| PathBuf::from(out))
}

/// Ogg Opus, stereo, 96 kbit/s, music-like: a sine and a chirp mixed.
fn celt_clip() -> Option<PathBuf> {
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-f", "lavfi", "-i", "aevalsrc=sin(2*PI*(300+2000*t)*t):d=2",
        "-filter_complex", "amix=inputs=2:duration=first",
        "-ac", "2", "-c:a", "libopus", "-b:a", "96k",
        temp("celt.ogg").to_str()?,
    ])
}

/// WebM Opus, mono, 32 kbit/s, voip.
fn voip_clip() -> Option<PathBuf> {
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=300:duration=2",
        "-ac", "1", "-c:a", "libopus", "-b:a", "32k", "-application", "voip",
        temp("voip.webm").to_str()?,
    ])
}

/// MP4 Opus.
fn mp4_clip() -> Option<PathBuf> {
    ffmpeg(&[
        "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
        "-c:a", "libopus", "-b:a", "64k", "-f", "mp4",
        temp("opus.mp4").to_str()?,
    ])
}

fn to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

fn decode_file(path: &Path) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let mut decoder = Decoder::from_reader(file).unwrap();
    let mut out = Vec::new();
    while decoder.next_block(&mut out).unwrap() {}
    out
}

/// The clip's channel count, from its track.
fn clip_channels(path: &Path) -> usize {
    let file = std::fs::File::open(path).unwrap();
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let format = symphonia::default::get_probe()
        .probe(&Default::default(), stream, FormatOptions::default(), MetadataOptions::default())
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        panic!("no audio parameters");
    };
    params.channels.as_ref().map_or(2, |c| c.count())
}

/// libopus (FFmpeg's `-c:a libopus` decoder, not its native one) decoding the same clip to the output format. A mono clip keeps one
/// channel: `-ac 2` would upmix at -3 dB, while this crate doubles mono at full level.
fn reference(path: &Path, channels: usize) -> Vec<u8> {
    let out = temp(&format!("ref-{}.pcm", path.file_name().unwrap().to_str().unwrap()));
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-c:a", "libopus", "-i"])
        .arg(path)
        .args(["-f", "s16le", "-ac", &channels.to_string(), "-ar", "48000"])
        .arg(&out)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(out).unwrap()
}

fn snr_db(ours: &[i16], reference: &[i16]) -> f64 {
    snr_at_lag(ours, reference, 0)
}

/// SNR of the overlapping part when the reference is shifted by `lag` samples per channel (frames at 48 kHz); the reference is
/// always the signal.
fn snr_at_lag(ours: &[i16], reference: &[i16], lag: i32) -> f64 {
    let (a, b) = if lag >= 0 {
        let k = 2 * lag as usize;
        let n = ours.len().min(reference.len());
        (&ours[..n.saturating_sub(k)], &reference[k..n])
    } else {
        let k = 2 * (-lag) as usize;
        let n = ours.len().min(reference.len());
        (&ours[k..n], &reference[..n.saturating_sub(k)])
    };
    let n = a.len().min(b.len());
    let (mut signal, mut noise) = (0.0f64, 0.0f64);
    for i in 0..n {
        let r = f64::from(b[i]);
        let d = f64::from(a[i]) - r;
        signal += r * r;
        noise += d * d;
    }
    10.0 * (signal / noise.max(1.0)).log10()
}

/// The best lag in -2000..=2000 samples per channel and its SNR. A coarse pass every 16 samples, then every sample around the best.
fn best_lag(ours: &[i16], reference: &[i16]) -> (i32, f64) {
    let mut best = (0i32, snr_at_lag(ours, reference, 0));
    for lag in (-2000i32..=2000).step_by(16) {
        let snr = snr_at_lag(ours, reference, lag);
        if snr > best.1 {
            best = (lag, snr);
        }
    }
    for lag in (best.0 - 16).max(-2000)..=(best.0 + 16).min(2000) {
        let snr = snr_at_lag(ours, reference, lag);
        if snr > best.1 {
            best = (lag, snr);
        }
    }
    best
}

/// The same clip decoded packet by packet with opus1 directly, without any of this crate's code (pre-skip applied per RFC 7845
/// section 4.2), to tell an opus1 gap from a wiring gap.
fn opus1_direct(path: &Path) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(&Default::default(), stream, FormatOptions::default(), MetadataOptions::default())
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        panic!("no audio parameters");
    };
    let head = params.extra_data.as_deref().unwrap();
    let channels = usize::from(head[9]);
    // The same pre-skip rule as the decoder: little-endian in an OpusHead, big-endian in the MP4 dOps box (version 0).
    let pre_skip = usize::from(if head[8] == 0 {
        u16::from_be_bytes([head[10], head[11]])
    } else {
        u16::from_le_bytes([head[10], head[11]])
    });
    let mut decoder = opus1::OpusDecoder::new(
        opus1::SampleRate::Hz48000,
        if channels == 1 { opus1::Channels::Mono } else { opus1::Channels::Stereo },
    )
    .unwrap();
    let mut buf = vec![0i16; 5760 * channels];
    let mut out = Vec::new();
    let mut skip = pre_skip;
    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id != track_id {
            continue;
        }
        if let Ok(frames) = decoder.decode_i16(&packet.data, 5760, &mut buf) {
            let mut v = buf[..frames * channels].to_vec();
            if skip > 0 {
                let drop = skip.min(frames);
                v.drain(..drop * channels);
                skip -= drop;
            }
            for frame in v.chunks(channels) {
                out.extend_from_slice(&[frame[0], frame[0]]);
            }
        }
    }
    out.iter().flat_map(|s| s.to_le_bytes()).collect()
}

/// Same length within one frame (20 ms), and close to libopus. A mono clip is compared channel by channel: the reference keeps one
/// channel and our left channel must equal our right (mono is doubled at full level, as this crate documents).
fn check_against_ffmpeg(path: &Path, min_snr: f64) {
    let channels = clip_channels(path);
    let out = to_samples(&decode_file(path));
    let reference = to_samples(&reference(path, channels));
    let (ours, reference) = if channels == 1 {
        let left: Vec<i16> = out.iter().step_by(2).copied().collect();
        let right: Vec<i16> = out.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(left, right, "{}: mono must come out doubled equally", path.display());
        (left, reference)
    } else {
        (out, reference)
    };
    let frame = 20 * 48 * 2; // 20 ms at 48 kHz, in samples per channel
    assert!(
        (ours.len() as i64 - reference.len() as i64).abs() <= frame as i64,
        "{}: {} samples against libopus's {}",
        path.display(),
        ours.len(),
        reference.len()
    );
    let snr = snr_db(&ours, &reference);
    let (lag, lag_snr) = best_lag(&ours, &reference);
    let direct = to_samples(&opus1_direct(path));
    let direct: Vec<i16> = if channels == 1 { direct.iter().step_by(2).copied().collect() } else { direct };
    let direct_snr = snr_db(&direct, &reference);
    println!(
        "{}: SNR {snr:.2} dB at lag 0, best {lag_snr:.2} dB at lag {lag} samples/channel, opus1 alone {direct_snr:.2} dB, against libopus",
        path.display()
    );
    assert!(snr > min_snr, "{}: SNR {snr} dB", path.display());
}

/// The packets symphonia reads from the Ogg clip through `PacketDecoder` give the same bytes as `Decoder` on the file.
fn check_packets_match_file(path: &Path) {
    let file = std::fs::File::open(path).unwrap();
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(&Default::default(), stream, FormatOptions::default(), MetadataOptions::default())
        .unwrap();
    let track = format.default_track(TrackType::Audio).unwrap();
    let track_id = track.id;
    let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
        panic!("no audio parameters");
    };
    assert_eq!(params.codec, CODEC_ID_OPUS);
    let config = AudioConfig::Opus { head: params.extra_data.as_deref().unwrap().to_vec() };
    let rate = params.sample_rate.unwrap_or(0);
    let channels = params.channels.as_ref().map_or(0, |c| c.count() as u16);
    let mut packets = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id == track_id {
            packets.push(packet.data.into_vec());
        }
    }

    let mut decoder = PacketDecoder::new(&config, rate, channels).unwrap();
    let mut from_packets = Vec::new();
    for packet in &packets {
        decoder.decode(packet, &mut from_packets).unwrap();
    }
    decoder.flush(&mut from_packets);
    assert_eq!(decoder.skipped_packets(), 0);
    assert_eq!(from_packets, decode_file(path), "packets differ from the file");
}

#[test]
fn ogg_opus_stereo_matches_libopus_and_packets_match_the_file() {
    match celt_clip() {
        Some(path) => {
            check_against_ffmpeg(&path, 60.0);
            check_packets_match_file(&path);
        }
        None => eprintln!("skipping ogg opus: no ffmpeg"),
    }
}

#[test]
fn webm_opus_mono_voip_matches_libopus() {
    match voip_clip() {
        Some(path) => check_against_ffmpeg(&path, 60.0),
        None => eprintln!("skipping webm opus: no ffmpeg"),
    }
}

#[test]
fn mp4_opus_matches_libopus() {
    match mp4_clip() {
        Some(path) => check_against_ffmpeg(&path, 60.0),
        None => eprintln!("skipping mp4 opus: no ffmpeg"),
    }
}
