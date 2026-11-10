//! The MSE byte stream parsers against the file demuxers, on streams FFmpeg makes (FFmpeg is the yardstick only: nothing links it).
//! A test without FFmpeg says so and passes.

use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use lucina_video::mse::{AudioCodec, Event, Format, Frame, Init, ParseError, Parser, TrackKind};
use lucina_video::{Codec, Packet};

fn have_ffmpeg() -> bool {
    let ok = Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipped: ffmpeg is not installed");
    }
    ok
}

/// A stream FFmpeg makes from three seconds of test pictures and a tone.
fn make(name: &str, args: &[&str]) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("lucina-mse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    static N: AtomicUsize = AtomicUsize::new(0);
    let path: PathBuf = dir.join(format!("{}-{name}", N.fetch_add(1, Ordering::Relaxed)));
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=24", "-f", "lavfi", "-i"])
        .args(["sine=frequency=440:sample_rate=48000", "-t", "3"])
        .args(args)
        .arg(&path);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "ffmpeg: {}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

/// How many packets FFmpeg reads from a stream of the file.
fn ffprobe_packets(bytes: &[u8], name: &str, stream: &str) -> usize {
    let path = std::env::temp_dir().join(format!("lucina-mse-probe-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-select_streams", stream, "-count_packets", "-show_entries", "stream=nb_read_packets", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

/// The summed durations FFmpeg reads for a stream of the file, in microseconds.
fn ffprobe_duration_us(bytes: &[u8], name: &str, stream: &str) -> i64 {
    let path = std::env::temp_dir().join(format!("lucina-mse-probe-d-{}-{name}", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "quiet", "-select_streams", stream, "-show_entries", "packet=duration_time", "-of", "csv=p=0"])
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let s: f64 = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| l.trim().parse::<f64>().ok()).sum();
    (s * 1e6).round() as i64
}

fn feed(format: Format, pieces: &[&[u8]]) -> Vec<Event> {
    let mut p = Parser::new(format);
    let mut out = Vec::new();
    for piece in pieces {
        p.append(piece, &mut out).unwrap();
    }
    out
}

fn feed_in(format: Format, bytes: &[u8], n: usize) -> Vec<Event> {
    feed(format, &bytes.chunks(n).collect::<Vec<_>>())
}

fn init_of(events: &[Event]) -> &Init {
    events.iter().find_map(|e| if let Event::Init(i) = e { Some(i) } else { None }).expect("an init segment")
}

fn frames_of(events: &[Event]) -> Vec<Frame> {
    events.iter().filter_map(|e| if let Event::Frames(f) = e { Some(f.clone()) } else { None }).flatten().collect()
}

fn packets(bytes: &[u8]) -> Vec<Packet> {
    let mut d = lucina_video::open(Cursor::new(bytes.to_vec())).unwrap();
    std::iter::from_fn(|| d.next_packet().unwrap()).collect()
}

fn video_id(init: &Init) -> u64 {
    init.tracks.iter().find(|t| matches!(t.kind, TrackKind::Video { .. })).unwrap().id
}

fn audio_id(init: &Init) -> u64 {
    init.tracks.iter().find(|t| matches!(t.kind, TrackKind::Audio { .. })).unwrap().id
}

/// The video frames are the demuxer's packets, in the same (decode) order, byte for byte.
fn assert_video_is_demuxed(frames: &[Frame], id: u64, file: &[u8]) {
    let video: Vec<&Frame> = frames.iter().filter(|f| f.track == id).collect();
    let packets = packets(file);
    assert_eq!(video.len(), packets.len());
    for (i, (f, p)) in video.iter().zip(&packets).enumerate() {
        assert_eq!((f.pts_us, f.key), (p.pts_us, p.key), "frame {i}");
        assert!(f.data == p.data, "frame {i}'s bytes");
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

/// The top-level boxes: `(kind, start, end)`.
fn top_boxes(b: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= b.len() {
        let size = be32(b, at) as usize;
        out.push((b[at + 4..at + 8].try_into().unwrap(), at, at + size));
        at += size;
    }
    out
}

/// The init segment (everything before the first moof), then each moof with what follows it up to the next.
fn iso_segments(b: &[u8]) -> Vec<&[u8]> {
    let starts: Vec<usize> = top_boxes(b).iter().filter(|(k, _, _)| k == b"moof").map(|(_, s, _)| *s).collect();
    let mut cuts = vec![0];
    cuts.extend(&starts);
    cuts.push(b.len());
    cuts.windows(2).map(|w| &b[w[0]..w[1]]).collect()
}

fn fragmented_mp4() -> Vec<u8> {
    make(
        "frag.mp4",
        &["-c:v", "libx264", "-g", "12", "-bf", "2", "-c:a", "aac", "-movflags", "frag_keyframe+empty_moov+default_base_moof"],
    )
}

#[test]
fn fragmented_mp4_in_any_pieces_is_what_the_demuxer_reads() {
    if !have_ffmpeg() {
        return;
    }
    let file = fragmented_mp4();
    let segments = iso_segments(&file);
    assert!(segments.len() > 3, "an init segment and media segments");
    let whole = feed(Format::IsoBmff, &[&file]);
    assert_eq!(feed_in(Format::IsoBmff, &file, 1), whole);
    assert_eq!(feed_in(Format::IsoBmff, &file, 4096), whole);
    assert_eq!(feed(Format::IsoBmff, &segments), whole);

    let init = init_of(&whole);
    assert_eq!(init.tracks.len(), 2);
    let (vid, aid) = (video_id(init), audio_id(init));
    let v = init.tracks.iter().find(|t| t.id == vid).unwrap();
    assert!(v.codec_name.starts_with("avc1.64"), "{}", v.codec_name);
    assert!(matches!(&v.kind, TrackKind::Video { codec: Codec::H264, width: 320, height: 240, config } if !config.is_empty()));
    let a = init.tracks.iter().find(|t| t.id == aid).unwrap();
    assert_eq!(a.codec_name, "mp4a.40.2");
    assert!(matches!(&a.kind, TrackKind::Audio { codec: AudioCodec::Aac, rate: 48000, channels: 1, config } if config.len() >= 2));

    // One Frames event per media segment.
    assert_eq!(whole.iter().filter(|e| matches!(e, Event::Frames(_))).count(), segments.len() - 1);
    let frames = frames_of(&whole);
    assert_video_is_demuxed(&frames, vid, &file);
    let video: Vec<&Frame> = frames.iter().filter(|f| f.track == vid).collect();
    assert!(video.windows(2).all(|w| w[0].dts_us <= w[1].dts_us), "video in decode order");
    assert!(video.windows(2).any(|w| w[1].pts_us < w[0].pts_us), "B-frames: display order differs");
    assert!(video.iter().all(|f| f.dts_us <= f.pts_us));

    let audio: Vec<&Frame> = frames.iter().filter(|f| f.track == aid).collect();
    assert_eq!(audio.len(), ffprobe_packets(&file, "frag.mp4", "a:0"));
    assert!(audio.iter().all(|f| f.key));
    // FFmpeg's AAC encoder primes and pads, and no edit list trims that in this file: ffprobe reads 148000 samples, 3.083 s, not 3.
    let total: i64 = audio.iter().map(|f| f.duration_us).sum();
    let theirs = ffprobe_duration_us(&file, "frag.mp4", "a:0");
    assert!((total - theirs).abs() <= 1_000, "audio lasts {total} us, FFmpeg says {theirs}");
    assert!((total - 3_000_000).abs() <= 100_000, "audio lasts {total} us");
}

#[test]
fn a_plain_mp4_gives_its_tracks_and_its_mdat_is_skipped() {
    if !have_ffmpeg() {
        return;
    }
    // Not fragmented: the moov has sample tables, which MSE does not use, and the mdat has no moof. The spec says an mdat outside a
    // media segment is skipped (Chromium refuses it); this follows the spec.
    let file = make("plain.mp4", &["-c:v", "libx264", "-g", "12", "-bf", "2", "-c:a", "aac", "-movflags", "+faststart"]);
    for events in [feed(Format::IsoBmff, &[&file]), feed_in(Format::IsoBmff, &file, 1), feed_in(Format::IsoBmff, &file, 4096)] {
        assert_eq!(events.len(), 1);
        let init = init_of(&events);
        assert_eq!(init.tracks.len(), 2);
        assert!(init.duration_us.is_some_and(|d| (d - 3_000_000).abs() < 100_000), "{:?}", init.duration_us);
    }
    let mut p = Parser::new(Format::IsoBmff);
    let mut out = Vec::new();
    p.append(&file, &mut out).unwrap();
    assert_eq!(p.pending(), 0);
}

#[test]
fn reset_in_a_media_segment_drops_only_that() {
    if !have_ffmpeg() {
        return;
    }
    let file = fragmented_mp4();
    let segments = iso_segments(&file);
    let mut p = Parser::new(Format::IsoBmff);
    let mut out = Vec::new();
    p.append(segments[0], &mut out).unwrap();
    p.append(segments[1], &mut out).unwrap();
    p.append(&segments[2][..segments[2].len() / 2], &mut out).unwrap();
    assert!(p.pending() > 0);
    p.reset();
    assert_eq!(p.pending(), 0);
    out.clear();
    p.append(segments[3], &mut out).unwrap();

    let reference = feed(Format::IsoBmff, &segments);
    let Event::Frames(expected) = reference.iter().filter(|e| matches!(e, Event::Frames(_))).nth(2).unwrap() else { unreachable!() };
    assert_eq!(out, vec![Event::Frames(expected.clone())]);
}

#[test]
fn a_trun_pointing_past_its_mdat_is_refused() {
    if !have_ffmpeg() {
        return;
    }
    let file = fragmented_mp4();
    let segments = iso_segments(&file);
    let mut bad = segments[1].to_vec();
    // The first trun's data offset (flags 0x1 set by FFmpeg): past the mdat.
    let at = bad.windows(4).position(|w| w == b"trun").unwrap() + 4;
    assert!(be32(&bad, at) & 1 != 0);
    bad[at + 8..at + 12].copy_from_slice(&0x7fff_0000u32.to_be_bytes());
    let mut p = Parser::new(Format::IsoBmff);
    let mut out = Vec::new();
    p.append(segments[0], &mut out).unwrap();
    assert!(matches!(p.append(&bad, &mut out), Err(ParseError::Bad(_))));
    // After the append error algorithm, the next good segment is read.
    p.reset();
    out.clear();
    p.append(segments[2], &mut out).unwrap();
    assert!(matches!(&out[..], [Event::Frames(f)] if !f.is_empty()));
}

/// An EBML variable-length number: `(value or None for unknown, length)`.
fn vint(b: &[u8]) -> (Option<u64>, usize) {
    let len = b[0].leading_zeros() as usize + 1;
    let mask = (0xffu32 >> len) as u8;
    let mut v = (b[0] & mask) as u64;
    let mut ones = v == mask as u64;
    for x in &b[1..len] {
        ones &= *x == 0xff;
        v = v << 8 | *x as u64;
    }
    (if ones { None } else { Some(v) }, len)
}

/// The same stream with its segment and every cluster of unknown size, as a live encoder that cannot seek back writes them.
fn unknown_sizes(b: &[u8]) -> Vec<u8> {
    const UNKNOWN: [u8; 8] = [0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
    let mut out = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let id_len = b[at].leading_zeros() as usize + 1;
        let id = &b[at..at + id_len];
        let (size, size_len) = vint(&b[at + id_len..]);
        let body = at + id_len + size_len;
        out.extend_from_slice(id);
        if id == [0x18, 0x53, 0x80, 0x67] || id == [0x1f, 0x43, 0xb6, 0x75] {
            // Stepped into: the children follow.
            out.extend_from_slice(&UNKNOWN);
            at = body;
        } else {
            out.extend_from_slice(&b[at + id_len..body + size.unwrap() as usize]);
            at = body + size.unwrap() as usize;
        }
    }
    out
}

fn check_webm(file: &[u8], name: &str, all_audio: bool) {
    let whole = feed(Format::WebM, &[file]);
    let frames = frames_of(&whole);
    assert_eq!(frames_of(&feed_in(Format::WebM, file, 1)), frames);
    assert_eq!(frames_of(&feed_in(Format::WebM, file, 4096)), frames);
    assert!(matches!(whole[0], Event::Init(_)));
    assert_eq!(whole.iter().filter(|e| matches!(e, Event::Init(_))).count(), 1);

    let init = init_of(&whole);
    let (vid, aid) = (video_id(init), audio_id(init));
    let v = init.tracks.iter().find(|t| t.id == vid).unwrap();
    assert_eq!(v.codec_name, "vp9");
    assert!(matches!(v.kind, TrackKind::Video { codec: Codec::Vp9, width: 320, height: 240, .. }));
    let a = init.tracks.iter().find(|t| t.id == aid).unwrap();
    assert_eq!(a.codec_name, "opus");
    assert!(matches!(&a.kind, TrackKind::Audio { codec: AudioCodec::Opus, rate: 48000, config, .. } if config.starts_with(b"OpusHead")));

    assert_video_is_demuxed(&frames, vid, file);
    let video: Vec<&Frame> = frames.iter().filter(|f| f.track == vid).collect();
    assert!(video.iter().all(|f| f.dts_us == f.pts_us && f.duration_us == 41_666));
    let audio: Vec<&Frame> = frames.iter().filter(|f| f.track == aid).collect();
    let expected = ffprobe_packets(file, name, "a:0");
    if all_audio {
        assert_eq!(audio.len(), expected);
    } else {
        // The last Opus block of an unknown-size cluster has no duration of its own: it waits for whatever ends the cluster.
        assert_eq!(audio.len(), expected - 1);
    }
    assert!(audio.iter().all(|f| f.key));
    // Opus blocks carry no duration: each is the gap to the next. The first is 21 ms (FFmpeg rounds the pre-skip into it).
    let odd: Vec<(i64, i64)> = audio.iter().skip(1).filter(|f| f.duration_us != 20_000).map(|f| (f.pts_us, f.duration_us)).collect();
    assert!(odd.is_empty(), "{odd:?}");
    let total: i64 = audio.iter().map(|f| f.duration_us).sum();
    let theirs = ffprobe_duration_us(file, name, "a:0");
    assert!((total - theirs).abs() <= 30_000, "audio lasts {total} us, FFmpeg says {theirs}");
}

#[test]
fn webm_in_any_pieces_is_what_the_demuxer_reads() {
    if !have_ffmpeg() {
        return;
    }
    let file = make("live.webm", &["-c:v", "libvpx-vp9", "-cpu-used", "8", "-g", "12", "-c:a", "libopus", "-f", "webm"]);
    check_webm(&file, "live.webm", true);
}

#[test]
fn webm_with_unknown_sizes_is_what_the_demuxer_reads() {
    if !have_ffmpeg() {
        return;
    }
    // FFmpeg's `-live 1` leaves the segment's size unknown; its clusters it still sizes, so they are made unknown here.
    let live = make(
        "live1.webm",
        &["-c:v", "libvpx-vp9", "-cpu-used", "8", "-g", "12", "-c:a", "libopus", "-f", "webm", "-live", "1", "-cluster_size_limit", "20000"],
    );
    let file = unknown_sizes(&live);
    assert!(file.windows(12).any(|w| w == [0x1f, 0x43, 0xb6, 0x75, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]));
    check_webm(&file, "unknown.webm", false);
    // A Cluster after the last one ends it: then every frame is out.
    let mut more = file.clone();
    more.extend_from_slice(&[0x1f, 0x43, 0xb6, 0x75, 0x80]);
    let frames = frames_of(&feed(Format::WebM, &[&more]));
    let aid = audio_id(init_of(&feed(Format::WebM, &[&file])));
    assert_eq!(frames.iter().filter(|f| f.track == aid).count(), ffprobe_packets(&file, "unknown.webm", "a:0"));
}

#[test]
fn webm_reset_in_a_cluster_drops_only_that() {
    if !have_ffmpeg() {
        return;
    }
    let file = make("reset.webm", &["-c:v", "libvpx-vp9", "-cpu-used", "8", "-g", "12", "-an", "-f", "webm"]);
    let clusters: Vec<usize> = file.windows(4).enumerate().filter(|(_, w)| *w == [0x1f, 0x43, 0xb6, 0x75]).map(|(i, _)| i).collect();
    assert!(clusters.len() >= 3);
    let (c1, c2, c3) = (clusters[0], clusters[1], clusters[2]);
    let mut p = Parser::new(Format::WebM);
    let mut out = Vec::new();
    p.append(&file[..c1 + (c2 - c1) / 2], &mut out).unwrap();
    p.reset();
    out.clear();
    p.append(&file[c2..c3], &mut out).unwrap();
    let reference = frames_of(&feed(Format::WebM, &[&file[..c3]]));
    let second: Vec<Frame> = reference.iter().filter(|f| f.pts_us >= 500_000).cloned().collect();
    assert_eq!(frames_of(&out), second);
}

#[test]
fn mime_types() {
    use lucina_video::mse::format_of;
    assert_eq!(format_of("video/mp4; codecs=\"avc1.64001f\""), Some(Format::IsoBmff));
    assert_eq!(format_of("Audio/MP4"), Some(Format::IsoBmff));
    assert_eq!(format_of("video/webm;codecs=vp9"), Some(Format::WebM));
    assert_eq!(format_of("audio/webm"), Some(Format::WebM));
    assert_eq!(format_of("video/mp2t"), None);
}

// Hostile pages: bytes made by hand, no FFmpeg.

/// An EBML element: the id's bytes, the size in one or two bytes, the body.
fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    match body.len() {
        n if n < 127 => out.push(0x80 | n as u8),
        n => out.extend_from_slice(&[0x40 | (n >> 8) as u8, n as u8]),
    }
    out.extend_from_slice(body);
    out
}

const SEGMENT_UNKNOWN: [u8; 12] = [0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
const CLUSTER_UNKNOWN: [u8; 12] = [0x1f, 0x43, 0xb6, 0x75, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];

fn ebml_header(doc_type: &[u8]) -> Vec<u8> {
    el(&[0x1a, 0x45, 0xdf, 0xa3], &el(&[0x42, 0x82], doc_type))
}

/// A WebM init segment: VP9 as track 1, Opus as track 2, a millisecond scale.
fn webm_init() -> Vec<u8> {
    let video = [el(&[0xd7], &[1]), el(&[0x83], &[1]), el(&[0x86], b"V_VP9"), el(&[0xe0], &[el(&[0xb0], &[64]), el(&[0xba], &[48])].concat())];
    let audio = [el(&[0xd7], &[2]), el(&[0x83], &[2]), el(&[0x86], b"A_OPUS"), el(&[0x63, 0xa2], b"OpusHead\x01\x02\0\0\x80\xbb\0\0\0\0\0")];
    let tracks = [el(&[0xae], &video.concat()), el(&[0xae], &audio.concat())].concat();
    let info = el(&[0x2a, 0xd7, 0xb1], &[0x0f, 0x42, 0x40]);
    [ebml_header(b"webm"), SEGMENT_UNKNOWN.to_vec(), el(&[0x15, 0x49, 0xa9, 0x66], &info), el(&[0x16, 0x54, 0xae, 0x6b], &tracks)].concat()
}

/// A SimpleBlock of `track` at `time` ms into its cluster.
fn simple_block(track: u8, time: i16, key: bool, data: &[u8]) -> Vec<u8> {
    let mut body = vec![0x80 | track];
    body.extend_from_slice(&time.to_be_bytes());
    body.push(if key { 0x80 } else { 0 });
    body.extend_from_slice(data);
    el(&[0xa3], &body)
}

fn one_append(format: Format, bytes: &[u8]) -> (Result<(), ParseError>, Vec<Event>, usize) {
    let mut p = Parser::new(format);
    let mut out = Vec::new();
    let r = p.append(bytes, &mut out);
    (r, out, p.pending())
}

#[test]
fn a_box_claiming_a_terabyte_is_refused_before_anything_is_held() {
    // Only the header is appended: refused at once, nothing kept for the rest.
    let mut large = vec![0, 0, 0, 1];
    large.extend_from_slice(b"moov");
    large.extend_from_slice(&(1u64 << 40).to_be_bytes());
    let (r, out, held) = one_append(Format::IsoBmff, &large);
    assert!(matches!(r, Err(ParseError::TooLarge(_))), "{r:?}");
    assert!(out.is_empty() && held <= 16);
    // A 32-bit size over the 64 MiB a moov or moof may have.
    for kind in [b"moov", b"moof"] {
        let mut small = 0xffff_fff0u32.to_be_bytes().to_vec();
        small.extend_from_slice(kind);
        small.extend_from_slice(&[0; 8]);
        let (r, _, held) = one_append(Format::IsoBmff, &small);
        assert!(matches!(r, Err(ParseError::TooLarge(_))), "{r:?}");
        assert!(held <= 16);
    }
    // A WebM Tracks of 2^40 bytes.
    let mut webm = [ebml_header(b"webm"), SEGMENT_UNKNOWN.to_vec()].concat();
    webm.extend_from_slice(&[0x16, 0x54, 0xae, 0x6b, 0x01, 0, 0x01, 0, 0, 0, 0, 0]);
    let (r, _, held) = one_append(Format::WebM, &webm);
    assert!(matches!(r, Err(ParseError::TooLarge(_))), "{r:?}");
    assert!(held <= 16);
    // A box that may be streamed through is, whatever it claims.
    let mut free = vec![0, 0, 0, 1];
    free.extend_from_slice(b"free");
    free.extend_from_slice(&(1u64 << 40).to_be_bytes());
    let (r, _, held) = one_append(Format::IsoBmff, &free);
    assert_eq!((r, held), (Ok(()), 0));
}

#[test]
fn a_media_segment_before_any_init_segment_is_bad() {
    let mut moof = 8u32.to_be_bytes().to_vec();
    moof.extend_from_slice(b"moof");
    assert!(matches!(one_append(Format::IsoBmff, &moof).0, Err(ParseError::Bad(_))));
    // The ftyp and styp a page may send first change nothing.
    let mut styp = 12u32.to_be_bytes().to_vec();
    styp.extend_from_slice(b"stypiso6");
    styp.extend_from_slice(&moof);
    assert!(matches!(one_append(Format::IsoBmff, &styp).0, Err(ParseError::Bad(_))));
}

#[test]
fn a_webm_cluster_before_its_tracks_is_bad() {
    let stream = [ebml_header(b"webm"), SEGMENT_UNKNOWN.to_vec(), CLUSTER_UNKNOWN.to_vec()].concat();
    assert!(matches!(one_append(Format::WebM, &stream).0, Err(ParseError::Bad(_))));
    // Nor a cluster with no header at all, nor a DocType that is not WebM or Matroska.
    assert!(matches!(one_append(Format::WebM, &CLUSTER_UNKNOWN).0, Err(ParseError::Bad(_))));
    assert!(matches!(one_append(Format::WebM, &ebml_header(b"mkvx")).0, Err(ParseError::Bad(_))));
}

#[test]
fn a_webm_stream_made_by_hand() {
    let mut stream = webm_init();
    stream.extend_from_slice(&CLUSTER_UNKNOWN);
    stream.extend(el(&[0xe7], &[0x03, 0xe8])); // 1000 ms
    stream.extend(simple_block(1, 0, true, b"key"));
    stream.extend(simple_block(2, 0, false, b"a0"));
    stream.extend(simple_block(1, 40, false, b"p1"));
    stream.extend(simple_block(2, 20, false, b"a1"));
    // The next cluster ends this one.
    stream.extend_from_slice(&CLUSTER_UNKNOWN);
    for events in [feed(Format::WebM, &[&stream]), feed_in(Format::WebM, &stream, 1)] {
        let init = init_of(&events);
        assert_eq!(init.tracks.iter().map(|t| t.codec_name.as_str()).collect::<Vec<_>>(), ["vp9", "opus"]);
        let frames = frames_of(&events);
        let video: Vec<(i64, bool, &[u8])> = frames.iter().filter(|f| f.track == 1).map(|f| (f.pts_us, f.key, &f.data[..])).collect();
        assert_eq!(video, [(1_000_000, true, &b"key"[..]), (1_040_000, false, b"p1")]);
        // Audio is all key frames; the gap is each one's duration, and the last of a cluster takes the one before.
        let audio: Vec<(i64, i64, bool)> = frames.iter().filter(|f| f.track == 2).map(|f| (f.pts_us, f.duration_us, f.key)).collect();
        assert_eq!(audio, [(1_000_000, 20_000, true), (1_020_000, 20_000, true)]);
    }
}

/// A fixed-seed generator: the same garbage on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Something that looks like the start of a box or element, with a size that is sometimes sane and sometimes not.
fn plausible(rng: &mut Lcg, format: Format) -> Vec<u8> {
    let size = match rng.below(4) {
        0 => rng.below(64) as u32,
        1 => rng.below(4096) as u32,
        2 => 1,
        _ => rng.next() as u32,
    };
    match format {
        Format::IsoBmff => {
            let kinds: [&[u8; 4]; 9] = [b"ftyp", b"moov", b"moof", b"mdat", b"free", b"trak", b"traf", b"trun", b"styp"];
            let mut out = size.to_be_bytes().to_vec();
            out.extend_from_slice(kinds[rng.below(kinds.len() as u64) as usize]);
            if size == 1 {
                let (hi, lo) = (rng.next(), rng.next());
                out.extend_from_slice(&(hi << 20 | lo).to_be_bytes());
            }
            out
        }
        Format::WebM => match rng.below(6) {
            0 => webm_init(),
            1 => CLUSTER_UNKNOWN.to_vec(),
            2 => {
                let (track, time, key, n) = (1 + rng.below(3) as u8, rng.next() as i16, rng.below(2) == 0, rng.below(40) as usize);
                simple_block(track, time, key, &rng.bytes(n))
            }
            3 => {
                let n = 1 + rng.below(9) as usize;
                el(&[0xe7], &rng.bytes(n))
            }
            4 => {
                // A block with lacing and nonsense lace sizes.
                let mut b = vec![0x82, 0, 0, 0x02 << rng.below(3)];
                let n = rng.below(30) as usize;
                b.extend(rng.bytes(n));
                el(&[0xa3], &b)
            }
            _ => {
                let ids: [&[u8]; 5] = [&[0x1a, 0x45, 0xdf, 0xa3], &[0x18, 0x53, 0x80, 0x67], &[0x16, 0x54, 0xae, 0x6b], &[0x1f, 0x43, 0xb6, 0x75], &[0xa0]];
                let mut out = ids[rng.below(ids.len() as u64) as usize].to_vec();
                out.push(0x08);
                out.extend_from_slice(&size.to_be_bytes());
                out
            }
        },
    }
}

#[test]
fn garbage_never_panics_nor_holds_too_much() {
    let max = lucina_video::mse_rules::MAX_PENDING as usize;
    for format in [Format::IsoBmff, Format::WebM] {
        let mut rng = Lcg(0x5eed_1234);
        let mut p = Parser::new(format);
        let (mut errors, mut events) = (0, 0);
        for round in 0..2000 {
            let mut bytes = Vec::new();
            // A page starting again every ten appends, so that the garbage meets the parser near a beginning, not deep in a skip.
            if round % 10 == 0 {
                p.reset();
                p = Parser::new(format);
                if format == Format::WebM {
                    bytes.extend(webm_init());
                    bytes.extend_from_slice(&CLUSTER_UNKNOWN);
                    bytes.extend(el(&[0xe7], &[0]));
                }
            }
            if round % 3 == 0 {
                bytes.extend(plausible(&mut rng, format));
            }
            let n = rng.below(300) as usize;
            bytes.extend(rng.bytes(n));
            if round % 5 == 0 {
                bytes.extend(plausible(&mut rng, format));
            }
            let mut out = Vec::new();
            if p.append(&bytes, &mut out).is_err() {
                errors += 1;
                p.reset();
            }
            events += out.len();
            assert!(p.pending() <= max);
            // Now and then a page aborts for no reason, or starts again with an init segment of its own.
            if rng.below(50) == 0 {
                p.reset();
            }
        }
        // Most garbage reads as an unknown MP4 box, which is streamed through; WebM refuses more.
        assert!(errors > 10, "{format:?}: garbage was refused only {errors} times");
        eprintln!("{format:?}: {errors} appends refused, {events} events");
    }
}
