//! Packets through `PacketDecoder` must give exactly what `Decoder` gives for the same packets in a file. Fixtures beyond the checked-in
//! AAC one are made with FFmpeg at test time; without FFmpeg those cases print a reason and pass.

use std::path::{Path, PathBuf};
use std::process::Command;

use lucina_audio_decode::packet::{AudioConfig, PacketDecoder};
use lucina_audio_decode::Decoder;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_AAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_VORBIS};
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

fn fixture() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/he-aac-1khz.m4a"))
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lucina-audio-decode-packet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn has_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Makes `name` from the AAC fixture with FFmpeg; `None` when FFmpeg is not there.
fn transcode(name: &str, args: &[&str]) -> Option<PathBuf> {
    if !has_ffmpeg() {
        return None;
    }
    let out = temp(name);
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(fixture())
        .args(args)
        .arg(&out)
        .status()
        .ok()?;
    status.success().then_some(out)
}

/// Reads the file the way an MSE parser would see it: the track's codec parameters and every packet's bytes.
fn read_track(path: &Path) -> (AudioConfig, u32, u16, Vec<Vec<u8>>) {
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
    let config = match params.codec {
        CODEC_ID_AAC => AudioConfig::Aac {
            asc: params.extra_data.as_deref().unwrap().to_vec(),
        },
        CODEC_ID_MP3 => AudioConfig::Mp3,
        CODEC_ID_VORBIS => AudioConfig::Vorbis {
            codec_private: xiph_lace(&params.extra_data.as_deref().unwrap()),
        },
        CODEC_ID_FLAC => AudioConfig::Flac {
            streaminfo: params.extra_data.as_deref().unwrap().to_vec(),
        },
        other => panic!("unexpected codec {other:?} in {}", path.display()),
    };
    let rate = params.sample_rate.unwrap_or(0);
    let channels = params.channels.as_ref().map_or(0, |c| c.count() as u16);
    let mut packets = Vec::new();
    while let Some(packet) = format.next_packet().unwrap() {
        if packet.track_id == track_id {
            packets.push(packet.data.into_vec());
        }
    }
    (config, rate, channels, packets)
}

/// Rebuilds the Matroska A_VORBIS CodecPrivate (Xiph lacing) from symphonia's extra data, which holds the headers back to back: the
/// identification header is 30 bytes, then the comment header (Ogg keeps it, Matroska drops it), then the setup header.
fn xiph_lace(extra_data: &[u8]) -> Vec<u8> {
    let ident_len = 30;
    let mut headers = vec![&extra_data[..ident_len]];
    let mut rest = &extra_data[ident_len..];
    if rest.first() == Some(&3) {
        let vendor_len = u32::from_le_bytes(rest[7..11].try_into().unwrap()) as usize;
        let mut pos = 11 + vendor_len;
        let count = u32::from_le_bytes(rest[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        for _ in 0..count {
            let len = u32::from_le_bytes(rest[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4 + len;
        }
        pos += 1; // framing bit
        headers.push(&rest[..pos]);
        rest = &rest[pos..];
    }
    assert_eq!(rest.first(), Some(&5), "vorbis setup header");
    headers.push(rest);

    let mut out = vec![(headers.len() - 1) as u8];
    for header in &headers[..headers.len() - 1] {
        let mut len = header.len();
        while len >= 255 {
            out.push(255);
            len -= 255;
        }
        out.push(len as u8);
    }
    for header in headers {
        out.extend_from_slice(header);
    }
    out
}

fn decode_file(path: &Path) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap();
    let mut decoder = Decoder::from_reader(file).unwrap();
    let mut out = Vec::new();
    while decoder.next_block(&mut out).unwrap() {}
    out
}

fn decode_packets(config: &AudioConfig, rate: u32, channels: u16, packets: &[Vec<u8>]) -> (Vec<u8>, u64) {
    let mut decoder = PacketDecoder::new(config, rate, channels).unwrap();
    let mut out = Vec::new();
    for packet in packets {
        decoder.decode(packet, &mut out).unwrap();
    }
    decoder.flush(&mut out);
    (out, decoder.skipped_packets())
}

fn check(path: &Path) {
    let (config, rate, channels, packets) = read_track(path);
    let from_file = decode_file(path);
    let (from_packets, skipped) = decode_packets(&config, rate, channels, &packets);
    assert_eq!(skipped, 0, "{} skipped packets", path.display());
    assert_eq!(from_packets, from_file, "{} differs between packets and file", path.display());

    // A garbage frame is skipped and counted, not an error.
    let mut decoder = PacketDecoder::new(&config, rate, channels).unwrap();
    let mut out = Vec::new();
    decoder.decode(&[0xA5; 64], &mut out).unwrap();
    assert_eq!(decoder.skipped_packets(), 1);

    // After a reset, the same packets give the same bytes.
    let mut decoder = PacketDecoder::new(&config, rate, channels).unwrap();
    let mut first = Vec::new();
    for packet in &packets {
        decoder.decode(packet, &mut first).unwrap();
    }
    decoder.flush(&mut first);
    decoder.reset();
    let mut second = Vec::new();
    for packet in &packets {
        decoder.decode(packet, &mut second).unwrap();
    }
    decoder.flush(&mut second);
    assert_eq!(first, second, "{} differs after reset", path.display());
}

#[test]
fn aac_packets_match_the_file() {
    check(&fixture());
}

#[test]
fn mp3_packets_match_the_file() {
    // No Xing/LAME tag, as in an MP4 track: the demuxer then sets no trim on the packets.
    match transcode("from-aac.mp3", &["-c:a", "libmp3lame", "-b:a", "128k", "-write_xing", "0"]) {
        Some(path) => check(&path),
        None => eprintln!("skipping mp3: no ffmpeg"),
    }
}

#[test]
fn vorbis_packets_match_the_file() {
    match transcode("from-aac-vorbis.webm", &["-c:a", "libvorbis", "-q:a", "4"]) {
        Some(path) => check(&path),
        None => eprintln!("skipping vorbis: no ffmpeg"),
    }
}

#[test]
fn flac_packets_match_the_file() {
    match transcode("from-aac-flac.mkv", &["-c:a", "flac"]) {
        Some(path) => check(&path),
        None => eprintln!("skipping flac: no ffmpeg"),
    }
}
