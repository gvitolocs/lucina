//! Media Source Extensions byte streams: what a page hands a `SourceBuffer` with `appendBuffer`, cut at any byte, read into tracks and
//! coded frames. ISO BMFF (w3.org/TR/mse-byte-stream-format-isobmff) and WebM (w3.org/TR/mse-byte-stream-format-webm); where the specs
//! leave room this does what Chromium's `mp4_stream_parser.cc` and `webm_stream_parser.cc` do, with one exception: an ISO BMFF `mdat`
//! outside a media segment is skipped, as the spec says, where Chromium refuses it.
//!
//! The boxes and elements are read with the file demuxers' helpers (`mp4.rs`, `mkv.rs`); the arithmetic a page decides is in
//! [`crate::mse_rules`], proved with Verus. What is held is one box or element at most: a `moov`, a `moof`, an `mdat` until it is whole,
//! a WebM `Tracks` or block. Anything else is streamed through.

use crate::demux::{Codec, annexb_from_avcc, annexb_from_hvcc};
use crate::mkv::{
    ID_BLOCK, ID_BLOCK_GROUP, ID_CLUSTER, ID_CODEC_ID, ID_CODEC_PRIVATE, ID_CUES, ID_DURATION, ID_INFO, ID_PIXEL_HEIGHT, ID_PIXEL_WIDTH,
    ID_REFERENCE_BLOCK, ID_SEGMENT, ID_SIMPLE_BLOCK, ID_TIMECODE, ID_TIMECODE_SCALE, ID_TRACK_ENTRY, ID_TRACK_NUMBER, ID_TRACK_TYPE,
    ID_TRACKS, ID_VIDEO, codec_of, elements, float, read_id, read_size, uint,
};
use crate::mp4::{MAX_FRAME, MAX_META, MAX_SAMPLES, be32, be64, boxes, find, parse_stsd, parse_trak};
use crate::mse_rules::{self, BoxSize, MAX_PENDING};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    IsoBmff,
    WebM,
}

/// From a MIME type's essence: video/mp4, audio/mp4 -> IsoBmff; video/webm, audio/webm -> WebM; others None.
pub fn format_of(mime: &str) -> Option<Format> {
    let essence = mime.split(';').next()?.trim().to_ascii_lowercase();
    match essence.as_str() {
        "video/mp4" | "audio/mp4" => Some(Format::IsoBmff),
        "video/webm" | "audio/webm" => Some(Format::WebM),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Mp3,
    Opus,
    Vorbis,
    Flac,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackKind {
    /// `config`: avcC / hvcC (or Matroska CodecPrivate) as for `demux::Track::config`.
    Video { codec: Codec, width: u32, height: u32, config: Vec<u8> },
    /// `config`: AAC AudioSpecificConfig (from esds DecoderSpecificInfo, or Matroska A_AAC CodecPrivate); Vorbis: Matroska
    /// CodecPrivate as stored; Opus: OpusHead (dOps body rebuilt as an OpusHead, or Matroska CodecPrivate); FLAC: the 34-byte
    /// STREAMINFO body; MP3: empty.
    Audio { codec: AudioCodec, config: Vec<u8>, rate: u32, channels: u16 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct TrackInfo {
    pub id: u64,
    pub kind: TrackKind,
    /// The track's codec string for SourceBuffer track lists, e.g. "avc1.64001f", "mp4a.40.2", "vp09", "opus".
    pub codec_name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Init {
    pub tracks: Vec<TrackInfo>,
    pub duration_us: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub track: u64,
    pub pts_us: i64,
    pub dts_us: i64,
    pub duration_us: i64,
    /// A random access point (sync sample / key frame).
    pub key: bool,
    /// For H.264 and HEVC with a config: Annex B, parameter sets before key frames (`annexb_from_avcc` / `annexb_from_hvcc`),
    /// exactly as `mp4.rs`/`mkv.rs` hand packets to the decoders. Other codecs as stored.
    pub data: Vec<u8>,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Init(Init),
    Frames(Vec<Frame>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Bytes that are not a stream of this format, or contradict what came before.
    Bad(&'static str),
    /// A size past what this machine holds for a page.
    TooLarge(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Bad(what) => write!(f, "damaged media stream: {what}"),
            ParseError::TooLarge(what) => write!(f, "media stream too large: {what}"),
        }
    }
}

impl std::error::Error for ParseError {}

use ParseError::{Bad, TooLarge};

/// What one look at the bytes held decided.
enum Step {
    /// Nothing more until this many bytes are held, counted from where the look began.
    Need(usize),
    /// These many bytes were read.
    Took(usize),
    /// A header of `head` bytes was read; the `body` bytes after it are dropped as they come.
    Skip { head: usize, body: u64 },
}

/// Bytes into a SourceBuffer's tracks and frames.
pub struct Parser {
    buf: Vec<u8>,
    /// Where `buf[0]` is in the stream since the parser was made or reset.
    at: u64,
    /// Bytes still to drop of a box or element streamed through. `buf` is empty while this is not 0.
    skip: u64,
    /// `buf` is not looked at again until it holds this many bytes.
    need: usize,
    format: State,
}

enum State {
    Iso(Iso),
    WebM(Box<WebM>),
}

impl Parser {
    pub fn new(format: Format) -> Parser {
        let format = match format {
            Format::IsoBmff => State::Iso(Iso::default()),
            Format::WebM => State::WebM(Box::default()),
        };
        Parser { buf: Vec::new(), at: 0, skip: 0, need: 0, format }
    }

    /// Takes the next bytes; emits what became complete, in order. Bytes of an element or box not yet complete are kept.
    /// After an `Err` the parser must not be used again until `reset` (MSE: the append error algorithm).
    pub fn append(&mut self, bytes: &[u8], out: &mut Vec<Event>) -> Result<(), ParseError> {
        // A megabyte at a time, so that what is held is what is incomplete, not the whole of a large append.
        for chunk in bytes.chunks(1 << 20) {
            self.feed(chunk, out)?;
        }
        if let State::WebM(w) = &mut self.format {
            w.flush(out);
        }
        Ok(())
    }

    /// MSE `abort()` / a parse error: drops a partial media segment and whatever is pending; keeps what the last init segment
    /// said (track ids, timescales, configs), so media segments can follow without a new init segment.
    pub fn reset(&mut self) {
        self.buf = Vec::new();
        self.at = 0;
        self.skip = 0;
        self.need = 0;
        match &mut self.format {
            State::Iso(i) => i.fragment = None,
            State::WebM(w) => w.reset(),
        }
    }

    /// Bytes held and not yet parsed (for SourceBuffer's quota).
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    fn feed(&mut self, mut chunk: &[u8], out: &mut Vec<Event>) -> Result<(), ParseError> {
        if self.skip > 0 {
            let n = self.skip.min(chunk.len() as u64);
            self.skip -= n;
            self.at = self.at.saturating_add(n);
            chunk = &chunk[n as usize..];
        }
        if chunk.is_empty() {
            return Ok(());
        }
        if !mse_rules::may_grow(self.buf.len() as u64, chunk.len() as u64) {
            return Err(TooLarge("more bytes held for a media stream than a page may have"));
        }
        self.buf.extend_from_slice(chunk);
        if self.buf.len() < self.need {
            return Ok(());
        }
        let mut used = 0usize;
        let result = loop {
            if self.skip > 0 {
                let n = self.skip.min((self.buf.len() - used) as u64);
                self.skip -= n;
                used += n as usize;
                if self.skip > 0 {
                    self.need = 0;
                    break Ok(());
                }
            }
            if let State::WebM(w) = &mut self.format {
                w.reached(self.at.saturating_add(used as u64));
            }
            if used == self.buf.len() {
                self.need = 0;
                break Ok(());
            }
            let at = self.at.saturating_add(used as u64);
            let rest = &self.buf[used..];
            let step = match &mut self.format {
                State::Iso(i) => i.step(rest, at, out),
                State::WebM(w) => w.step(rest, at, out),
            };
            match step {
                Ok(Step::Need(n)) => {
                    self.need = n;
                    break Ok(());
                }
                Ok(Step::Took(n)) => used += n,
                Ok(Step::Skip { head, body }) => {
                    used += head;
                    self.skip = body;
                }
                Err(e) => break Err(e),
            }
        };
        self.buf.drain(..used);
        self.at = self.at.saturating_add(used as u64);
        result
    }
}

fn to_us(units: i128, timescale: u32) -> i64 {
    (units * 1_000_000 / timescale.max(1) as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn decoder_data(codec: Codec, config: &[u8], key: bool, data: &[u8]) -> Vec<u8> {
    match codec {
        Codec::H264 if !config.is_empty() => annexb_from_avcc(data, config, key),
        Codec::Hevc if !config.is_empty() => annexb_from_hvcc(data, config, key),
        _ => data.to_vec(),
    }
}

// ISO BMFF.

struct IsoTrack {
    id: u32,
    timescale: u32,
    /// The edit list's start, in the track's timescale.
    elst: i64,
    audio: bool,
    codec: Codec,
    config: Vec<u8>,
    /// `trex` defaults.
    duration: u32,
    size: u32,
    flags: u32,
    /// Where the next fragment's decode time starts when it has no `tfdt`.
    next_dts: i64,
}

#[derive(Default)]
struct Iso {
    tracks: Vec<IsoTrack>,
    /// Tracks of the init segment that are not sound or pictures: their fragments are passed over.
    ignored: Vec<u32>,
    have_init: bool,
    /// A `moof` read, waiting for its `mdat`.
    fragment: Option<Fragment>,
}

struct Fragment {
    samples: Vec<Sample>,
    /// Each track's decode time after the fragment, kept when its frames go out.
    next_dts: Vec<i64>,
}

struct Sample {
    offset: u64,
    dts: i64,
    track: u32,
    size: u32,
    duration: u32,
    cts: i32,
    key: bool,
}

const SKIPPED_BOXES: [&[u8; 4]; 9] = [b"ftyp", b"styp", b"sidx", b"emsg", b"prft", b"free", b"skip", b"uuid", b"mfra"];

impl Iso {
    fn step(&mut self, b: &[u8], at: u64, out: &mut Vec<Event>) -> Result<Step, ParseError> {
        let Some(small) = be32(b, 0) else { return Ok(Step::Need(8)) };
        let kind: [u8; 4] = match b.get(4..8) {
            Some(k) => k.try_into().expect("four bytes"),
            None => return Ok(Step::Need(8)),
        };
        let large = if small == 1 { be64(b, 8) } else { None };
        let limit = match &kind {
            b"moov" | b"moof" => MAX_META,
            b"mdat" if self.fragment.is_some() => MAX_PENDING,
            _ => u64::MAX,
        };
        let (head, size) = match mse_rules::box_size(small, large, b.len() as u64, limit) {
            BoxSize::NeedMore => return Ok(Step::Need(16)),
            BoxSize::Bad => return Err(Bad("an MP4 box whose size cannot be one")),
            BoxSize::TooLarge => {
                return Err(TooLarge(if &kind == b"mdat" { "an MP4 mdat larger than a page may append" } else { "an MP4 index larger than any real one" }));
            }
            BoxSize::Box { head, size } => (head as usize, size),
        };
        let whole = size as usize;
        match &kind {
            b"moov" => {
                if self.fragment.is_some() {
                    return Err(Bad("an MP4 moof not followed by its mdat"));
                }
                if b.len() < whole {
                    return Ok(Step::Need(whole));
                }
                out.push(Event::Init(self.parse_moov(&b[head..whole])?));
                Ok(Step::Took(whole))
            }
            b"moof" => {
                if self.fragment.is_some() {
                    return Err(Bad("an MP4 moof not followed by its mdat"));
                }
                if !self.have_init {
                    return Err(Bad("an MP4 media segment before any initialization segment"));
                }
                if b.len() < whole {
                    return Ok(Step::Need(whole));
                }
                self.fragment = Some(self.parse_moof(&b[head..whole], at)?);
                Ok(Step::Took(whole))
            }
            b"mdat" if self.fragment.is_some() => {
                if b.len() < whole {
                    return Ok(Step::Need(whole));
                }
                let start = at.checked_add(head as u64).ok_or(Bad("an MP4 stream longer than any"))?;
                let end = mse_rules::end_of(at, 0, size).ok_or(Bad("an MP4 stream longer than any"))?;
                let frames = self.frames(&b[..whole], at, start, end)?;
                if !frames.is_empty() {
                    out.push(Event::Frames(frames));
                }
                Ok(Step::Took(whole))
            }
            // An mdat outside a media segment (a plain file's), and boxes a media segment does not need.
            k => {
                if self.fragment.is_some() && !SKIPPED_BOXES.contains(&k) {
                    return Err(Bad("an MP4 moof not followed by its mdat"));
                }
                Ok(Step::Skip { head, body: size - head as u64 })
            }
        }
    }

    fn parse_moov(&mut self, moov: &[u8]) -> Result<Init, ParseError> {
        let mvhd = find(moov, b"mvhd");
        let (movie_scale, movie_duration) = match mvhd {
            Some(m) if m.first() == Some(&1) => (be32(m, 20), be64(m, 24)),
            Some(m) => (be32(m, 12), be32(m, 16).map(|d| if d == u32::MAX { u64::MAX } else { d as u64 })),
            None => (None, None),
        };
        let mvex = find(moov, b"mvex");
        let mehd = mvex.and_then(|m| find(m, b"mehd")).and_then(|m| if m.first()? == &1 { be64(m, 4) } else { be32(m, 4).map(u64::from) });
        let units = mehd.or(movie_duration.filter(|d| *d != 0 && *d != u64::MAX));
        let duration_us = match (units, movie_scale) {
            (Some(u), Some(s)) if s > 0 => Some(to_us(u as i128, s)),
            _ => None,
        };
        let mut tracks: Vec<IsoTrack> = Vec::new();
        let mut infos = Vec::new();
        let mut ignored = Vec::new();
        for (kind, trak) in boxes(moov) {
            if &kind != b"trak" {
                continue;
            }
            let handler = find(trak, b"mdia").and_then(|m| find(m, b"hdlr")).and_then(|h| h.get(8..12));
            if !matches!(handler, Some(b"vide" | b"soun")) {
                if let Some(id) = find(trak, b"tkhd").and_then(|t| if t.first()? == &1 { be32(t, 20) } else { be32(t, 12) }) {
                    ignored.push(id);
                }
                continue;
            }
            let t = parse_trak(trak).ok_or(Bad("an MP4 track the initialization segment does not describe"))?;
            if t.timescale == 0 {
                return Err(Bad("an MP4 track with no timescale"));
            }
            if tracks.iter().any(|x| x.id == t.track_id) {
                return Err(Bad("two MP4 tracks with one id"));
            }
            let trex = mvex.and_then(|m| boxes(m).find(|(k, v)| k == b"trex" && be32(v, 4) == Some(t.track_id)).map(|(_, v)| v));
            let (kind, codec, config, codec_name) = if &t.handler == b"vide" {
                let (codec, width, height, config) = parse_stsd(t.stsd).ok_or(Bad("an MP4 video track's description"))?;
                let name = video_name(t.stsd, &config);
                (TrackKind::Video { codec, width, height, config: config.clone() }, codec, config, name)
            } else {
                let (codec, config, rate, channels, name) = parse_audio(t.stsd).ok_or(Bad("an MP4 sound track's description"))?;
                (TrackKind::Audio { codec, config, rate, channels }, Codec::Other, Vec::new(), name)
            };
            // A new init segment for the same track goes on from where its fragments were.
            let next_dts = self.tracks.iter().find(|x| x.id == t.track_id).map_or(0, |x| x.next_dts);
            tracks.push(IsoTrack {
                id: t.track_id,
                timescale: t.timescale,
                elst: t.elst_start.unwrap_or(0).max(0),
                audio: &t.handler == b"soun",
                codec,
                config,
                duration: trex.and_then(|v| be32(v, 12)).unwrap_or(0),
                size: trex.and_then(|v| be32(v, 16)).unwrap_or(0),
                flags: trex.and_then(|v| be32(v, 20)).unwrap_or(0),
                next_dts,
            });
            infos.push(TrackInfo { id: t.track_id as u64, kind, codec_name });
        }
        self.tracks = tracks;
        self.ignored = ignored;
        self.have_init = true;
        Ok(Init { tracks: infos, duration_us })
    }

    /// The samples of a `moof` starting at stream position `moof_start`, timed as `mp4.rs::parse_moof` times them.
    fn parse_moof(&self, moof: &[u8], moof_start: u64) -> Result<Fragment, ParseError> {
        const CUT: ParseError = Bad("an MP4 fragment index cut short");
        let mut samples: Vec<Sample> = Vec::new();
        let mut next_dts: Vec<i64> = self.tracks.iter().map(|t| t.next_dts).collect();
        for (kind, traf) in boxes(moof) {
            if &kind != b"traf" {
                continue;
            }
            let tfhd = find(traf, b"tfhd").ok_or(Bad("an MP4 track fragment with no header"))?;
            let flags = be32(tfhd, 0).ok_or(CUT)? & 0xff_ffff;
            let id = be32(tfhd, 4).ok_or(CUT)?;
            let Some(ti) = self.tracks.iter().position(|t| t.id == id) else {
                if self.ignored.contains(&id) {
                    continue;
                }
                return Err(Bad("an MP4 fragment of a track the initialization segment does not have"));
            };
            let track = &self.tracks[ti];
            let mut at = 8;
            // Explicit, or the moof's start: `default-base-is-moof` (0x020000) says the same as neither flag does here.
            let mut base = moof_start;
            if flags & 1 != 0 {
                base = be64(tfhd, at).ok_or(CUT)?;
                at += 8;
            }
            if flags & 2 != 0 {
                at += 4;
            }
            let mut d_duration = track.duration;
            if flags & 8 != 0 {
                d_duration = be32(tfhd, at).ok_or(CUT)?;
                at += 4;
            }
            let mut d_size = track.size;
            if flags & 0x10 != 0 {
                d_size = be32(tfhd, at).ok_or(CUT)?;
                at += 4;
            }
            let mut d_flags = track.flags;
            if flags & 0x20 != 0 {
                d_flags = be32(tfhd, at).ok_or(CUT)?;
            }
            if let Some(tfdt) = find(traf, b"tfdt") {
                let t = if tfdt.first() == Some(&1) { be64(tfdt, 4).map(|v| v as i64) } else { be32(tfdt, 4).map(i64::from) };
                next_dts[ti] = t.ok_or(CUT)?;
            }
            let mut offset = base;
            for (k, trun) in boxes(traf) {
                if &k != b"trun" {
                    continue;
                }
                let tf = be32(trun, 0).ok_or(CUT)? & 0xff_ffff;
                let count = be32(trun, 4).ok_or(CUT)?;
                if !mse_rules::samples_ok(samples.len() as u64, count as u64, MAX_SAMPLES as u64) {
                    return Err(TooLarge("an MP4 fragment with more samples than any real one"));
                }
                let mut p = 8;
                if tf & 1 != 0 {
                    let d = be32(trun, p).ok_or(CUT)? as i32;
                    offset = mse_rules::data_offset(base, d).ok_or(Bad("an MP4 sample before the stream's start"))?;
                    p += 4;
                }
                let mut first_flags = None;
                if tf & 4 != 0 {
                    first_flags = Some(be32(trun, p).ok_or(CUT)?);
                    p += 4;
                }
                // Every sample's fields are there before any is taken, so a count larger than the box is refused, not read.
                let per_sample = (tf & 0xf00).count_ones() as u64 * 4;
                if (p as u64).saturating_add(per_sample.saturating_mul(count as u64)) > trun.len() as u64 {
                    return Err(CUT);
                }
                samples.reserve((count as usize).min(1 << 16));
                for i in 0..count {
                    let (mut dur, mut size, mut fl, mut cts) = (d_duration, d_size, d_flags, 0i32);
                    if tf & 0x100 != 0 {
                        dur = be32(trun, p).ok_or(CUT)?;
                        p += 4;
                    }
                    if tf & 0x200 != 0 {
                        size = be32(trun, p).ok_or(CUT)?;
                        p += 4;
                    }
                    if tf & 0x400 != 0 {
                        fl = be32(trun, p).ok_or(CUT)?;
                        p += 4;
                    } else if i == 0
                        && let Some(f) = first_flags
                    {
                        fl = f;
                    }
                    if tf & 0x800 != 0 {
                        // Version 1 offsets are signed; version 0 ones are read as signed too, as mp4.rs reads them.
                        cts = be32(trun, p).ok_or(CUT)? as i32;
                        p += 4;
                    }
                    if !mse_rules::size_ok(size as u64, MAX_FRAME) {
                        return Err(TooLarge("an MP4 frame larger than any real one"));
                    }
                    let key = track.audio || fl & 0x0001_0000 == 0;
                    samples.push(Sample { offset, dts: next_dts[ti], track: ti as u32, size, duration: dur, cts, key });
                    offset = mse_rules::next_offset(offset, size).ok_or(Bad("an MP4 sample past any stream's end"))?;
                    next_dts[ti] = next_dts[ti].checked_add(dur as i64).ok_or(Bad("an MP4 decode time past any"))?;
                }
            }
        }
        Ok(Fragment { samples, next_dts })
    }

    /// The pending fragment's frames out of its `mdat`, held in `b` from stream position `at`, its body `[start, end)`.
    fn frames(&mut self, b: &[u8], at: u64, start: u64, end: u64) -> Result<Vec<Frame>, ParseError> {
        let fragment = self.fragment.take().expect("a fragment waits for this mdat");
        let mut frames = Vec::with_capacity(fragment.samples.len());
        let mut tracks_of = Vec::with_capacity(fragment.samples.len());
        for s in &fragment.samples {
            let (from, to) = mse_rules::sample_in(s.offset, s.size as u64, start, end, at, b.len() as u64)
                .ok_or(Bad("an MP4 sample outside the mdat after its moof"))?;
            let t = &self.tracks[s.track as usize];
            let dts = s.dts as i128 - t.elst as i128;
            frames.push(Frame {
                track: t.id as u64,
                pts_us: to_us(dts + s.cts as i128, t.timescale),
                dts_us: to_us(dts, t.timescale),
                duration_us: to_us(s.duration as i128, t.timescale),
                key: s.key,
                data: decoder_data(t.codec, &t.config, s.key, &b[from as usize..to as usize]),
            });
            tracks_of.push(s.track);
        }
        // In decode order within each track; the tracks stay interleaved as the samples were.
        for ti in 0..self.tracks.len() as u32 {
            let at: Vec<usize> = (0..frames.len()).filter(|&i| tracks_of[i] == ti).collect();
            if at.windows(2).all(|w| frames[w[0]].dts_us <= frames[w[1]].dts_us) {
                continue;
            }
            let mut mine: Vec<Frame> = at.iter().map(|&i| std::mem::replace(&mut frames[i], empty_frame())).collect();
            mine.sort_by_key(|f| f.dts_us);
            for (i, f) in at.into_iter().zip(mine) {
                frames[i] = f;
            }
        }
        for (t, d) in self.tracks.iter_mut().zip(fragment.next_dts) {
            t.next_dts = d;
        }
        Ok(frames)
    }
}

fn empty_frame() -> Frame {
    Frame { track: 0, pts_us: 0, dts_us: 0, duration_us: 0, key: false, data: Vec::new() }
}

fn fourcc(stsd: &[u8]) -> Option<[u8; 4]> {
    boxes(stsd.get(8..)?).next().map(|(k, _)| k)
}

fn video_name(stsd: &[u8], config: &[u8]) -> String {
    let Some(kind) = fourcc(stsd) else { return String::new() };
    let name = String::from_utf8_lossy(&kind).trim().to_owned();
    match (&kind, config.get(1..4)) {
        (b"avc1" | b"avc3", Some(p)) => format!("{name}.{:02x}{:02x}{:02x}", p[0], p[1], p[2]),
        _ => name,
    }
}

/// A sound sample entry: the codec, its config, rate, channels and codec string.
fn parse_audio(stsd: &[u8]) -> Option<(AudioCodec, Vec<u8>, u32, u16, String)> {
    let (kind, body) = boxes(stsd.get(8..)?).next()?;
    let version = u16::from_be_bytes(body.get(8..10)?.try_into().ok()?);
    let mut channels = u16::from_be_bytes(body.get(16..18)?.try_into().ok()?);
    let mut rate = be32(body, 24)? >> 16;
    // QuickTime's sound description versions 1 and 2 are longer.
    let children_at = match version {
        1 => 44,
        2 => {
            rate = f64::from_bits(be64(body, 32)?) as u32;
            channels = be32(body, 40)? as u16;
            64
        }
        _ => 28,
    };
    let children = body.get(children_at..).unwrap_or(&[]);
    Some(match &kind {
        b"mp4a" => {
            let (oti, dsi) = parse_esds(find(children, b"esds")?)?;
            match oti {
                0x40 | 0x66 | 0x67 => {
                    let aot = aac_object_type(&dsi);
                    let name = aot.map_or("mp4a.40".to_owned(), |a| format!("mp4a.40.{a}"));
                    (AudioCodec::Aac, dsi, rate, channels, name)
                }
                0x69 | 0x6B => (AudioCodec::Mp3, Vec::new(), rate, channels, format!("mp4a.{oti:02x}")),
                _ => {
                    let name = format!("mp4a.{oti:02x}");
                    (AudioCodec::Other(name.clone()), dsi, rate, channels, name)
                }
            }
        }
        b"Opus" => (AudioCodec::Opus, opus_head(find(children, b"dOps")?)?, rate, channels, "opus".to_owned()),
        b"fLaC" => {
            // A full box, then metadata blocks: the first is STREAMINFO (type 0), 34 bytes.
            let dfla = find(children, b"dfLa")?;
            let block = dfla.get(4..)?;
            if block.first()? & 0x7f != 0 || (be32(block, 0)? & 0xff_ffff) < 34 {
                return None;
            }
            (AudioCodec::Flac, block.get(4..38)?.to_vec(), rate, channels, "flac".to_owned())
        }
        b".mp3" => (AudioCodec::Mp3, Vec::new(), rate, channels, "mp3".to_owned()),
        other => {
            let name = String::from_utf8_lossy(other).trim().to_owned();
            (AudioCodec::Other(name.clone()), Vec::new(), rate, channels, name)
        }
    })
}

/// An MPEG-4 descriptor: `(tag, body, after)`.
fn descriptor(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *b.first()?;
    let mut size = 0usize;
    let mut at = 1;
    loop {
        let x = *b.get(at)?;
        at += 1;
        size = size << 7 | (x & 0x7f) as usize;
        if x & 0x80 == 0 {
            break;
        }
        if at > 4 {
            return None;
        }
    }
    let body = b.get(at..at.checked_add(size)?)?;
    Some((tag, body, &b[at + size..]))
}

/// `esds`: the DecoderConfigDescriptor's objectTypeIndication and the DecoderSpecificInfo's bytes.
fn parse_esds(esds: &[u8]) -> Option<(u8, Vec<u8>)> {
    let (tag, es, _) = descriptor(esds.get(4..)?)?;
    if tag != 3 {
        return None;
    }
    let flags = *es.get(2)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + *es.get(at)? as usize;
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let (tag, dcd, _) = descriptor(es.get(at..)?)?;
    if tag != 4 {
        return None;
    }
    let oti = *dcd.first()?;
    let dsi = dcd.get(13..).and_then(descriptor).filter(|(t, _, _)| *t == 5).map(|(_, d, _)| d.to_vec()).unwrap_or_default();
    Some((oti, dsi))
}

/// The audioObjectType an AudioSpecificConfig starts with.
fn aac_object_type(asc: &[u8]) -> Option<u8> {
    let first = asc.first()? >> 3;
    if first != 31 {
        return Some(first);
    }
    let next = ((asc.first()? & 7) << 3) | (asc.get(1)? >> 5);
    Some(32 + next)
}

/// A `dOps` body (big-endian) as the OpusHead (little-endian) a decoder takes.
fn opus_head(dops: &[u8]) -> Option<Vec<u8>> {
    let channels = *dops.get(1)?;
    let pre_skip = u16::from_be_bytes(dops.get(2..4)?.try_into().ok()?);
    let rate = be32(dops, 4)?;
    let gain = i16::from_be_bytes(dops.get(8..10)?.try_into().ok()?);
    let family = *dops.get(10)?;
    let mut head = b"OpusHead".to_vec();
    head.extend_from_slice(&[1, channels]);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&rate.to_le_bytes());
    head.extend_from_slice(&gain.to_le_bytes());
    head.push(family);
    if family != 0 {
        head.extend_from_slice(dops.get(11..13 + channels as usize)?);
    }
    Some(head)
}

// WebM.

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_DOC_TYPE: u32 = 0x4282;
const ID_SEEK_HEAD: u32 = 0x114D_9B74;
const ID_TAGS: u32 = 0x1254_C367;
const ID_CHAPTERS: u32 = 0x1043_A770;
const ID_ATTACHMENTS: u32 = 0x1941_A469;
const ID_DEFAULT_DURATION: u32 = 0x23_E383;
const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;
const ID_BLOCK_DURATION: u32 = 0x9B;

/// An element that cannot be inside a cluster: one begins where an unknown-size cluster ends.
fn ends_cluster(id: u32) -> bool {
    matches!(id, ID_EBML | ID_SEGMENT | ID_SEEK_HEAD | ID_INFO | ID_TRACKS | ID_CUES | ID_CLUSTER | ID_TAGS | ID_CHAPTERS | ID_ATTACHMENTS)
}

struct WebmTrack {
    number: u64,
    audio: bool,
    codec: Codec,
    config: Vec<u8>,
    default_duration_ns: Option<u64>,
    /// A frame whose duration is the gap to the next of this track, waiting for it.
    held: Option<Frame>,
    last_gap: Option<i64>,
}

struct Cluster {
    end: Option<u64>,
    time: Option<i64>,
}

struct WebM {
    header_seen: bool,
    /// Nanoseconds per unit of the stream's times.
    scale: u64,
    duration: Option<f64>,
    info_seen: bool,
    tracks: Option<Vec<TrackInfo>>,
    webm_tracks: Vec<WebmTrack>,
    ignored: Vec<u64>,
    init_sent: bool,
    segment_end: Option<u64>,
    cluster: Option<Cluster>,
    /// Frames completed in this append.
    frames: Vec<Frame>,
}

impl Default for WebM {
    fn default() -> WebM {
        WebM {
            header_seen: false,
            scale: 1_000_000,
            duration: None,
            info_seen: false,
            tracks: None,
            webm_tracks: Vec::new(),
            ignored: Vec::new(),
            init_sent: false,
            segment_end: None,
            cluster: None,
            frames: Vec::new(),
        }
    }
}

impl WebM {
    fn reset(&mut self) {
        self.cluster = None;
        self.segment_end = None;
        self.frames.clear();
        for t in &mut self.webm_tracks {
            t.held = None;
        }
    }

    fn flush(&mut self, out: &mut Vec<Event>) {
        if !self.frames.is_empty() {
            out.push(Event::Frames(std::mem::take(&mut self.frames)));
        }
    }

    fn send_init(&mut self, out: &mut Vec<Event>) {
        let Some(tracks) = &self.tracks else { return };
        let duration_us = self.duration.map(|d| (d * self.scale as f64 / 1000.0) as i64);
        let init = Init { tracks: tracks.clone(), duration_us };
        self.flush(out);
        out.push(Event::Init(init));
        self.init_sent = true;
    }

    /// The frames waiting for a next one get the gap before them.
    fn end_cluster(&mut self) {
        self.cluster = None;
        for t in &mut self.webm_tracks {
            if let Some(mut f) = t.held.take() {
                f.duration_us = t.last_gap.unwrap_or(0);
                self.frames.push(f);
            }
        }
    }

    /// The reading stands at `at`: a known-size cluster or segment ending there ends now, not when the next bytes come.
    fn reached(&mut self, at: u64) {
        if self.cluster.as_ref().and_then(|c| c.end).is_some_and(|end| at >= end) {
            self.end_cluster();
        }
        if self.segment_end.is_some_and(|end| at >= end) {
            self.end_cluster();
            self.segment_end = None;
        }
    }

    fn step(&mut self, b: &[u8], at: u64, out: &mut Vec<Event>) -> Result<Step, ParseError> {
        let mut s = b;
        let id = match read_id(&mut s) {
            Err(_) => return Err(Bad("a WebM element id")),
            Ok(None) => return Ok(Step::Need(b.len() + 1)),
            Ok(Some(id)) => id,
        };
        let size = match read_size(&mut s) {
            Err(_) => return Err(Bad("a WebM element size")),
            Ok(None) => return Ok(Step::Need(b.len() + 1)),
            Ok(Some(size)) => size,
        };
        let head = b.len() - s.len();
        let known = |limit: u64, what: &'static str| -> Result<usize, ParseError> {
            let size = size.ok_or(Bad("a WebM element of unknown size where its size must be known"))?;
            if !mse_rules::size_ok(size, limit) {
                return Err(TooLarge(what));
            }
            Ok(head + size as usize)
        };
        if self.cluster.is_some() {
            match id {
                ID_TIMECODE => {
                    let whole = known(8, "a WebM cluster time")?;
                    if b.len() < whole {
                        return Ok(Step::Need(whole));
                    }
                    let time = i64::try_from(uint(&b[head..whole])).map_err(|_| Bad("a WebM cluster time"))?;
                    self.cluster.as_mut().expect("in a cluster").time = Some(time);
                    return Ok(Step::Took(whole));
                }
                ID_SIMPLE_BLOCK | ID_BLOCK_GROUP => {
                    let whole = known(MAX_FRAME, "a WebM block larger than any real one")?;
                    if b.len() < whole {
                        return Ok(Step::Need(whole));
                    }
                    let body = &b[head..whole];
                    if id == ID_SIMPLE_BLOCK {
                        self.block(body, None, None)?;
                    } else {
                        let has_reference = elements(body).any(|(i, _)| i == ID_REFERENCE_BLOCK);
                        let duration = elements(body).find(|(i, _)| *i == ID_BLOCK_DURATION).map(|(_, v)| uint(v));
                        if let Some((_, block)) = elements(body).find(|(i, _)| *i == ID_BLOCK) {
                            self.block(block, Some(!has_reference), duration)?;
                        }
                    }
                    return Ok(Step::Took(whole));
                }
                id if ends_cluster(id) => {
                    self.end_cluster();
                    return Ok(Step::Took(0));
                }
                _ => {
                    let size = size.ok_or(Bad("a WebM element of unknown size inside a cluster"))?;
                    return Ok(Step::Skip { head, body: size });
                }
            }
        }
        match id {
            ID_EBML => {
                let whole = known(MAX_META, "a WebM header larger than any real one")?;
                if b.len() < whole {
                    return Ok(Step::Need(whole));
                }
                let doc_type = elements(&b[head..whole]).find(|(i, _)| *i == ID_DOC_TYPE).map(|(_, v)| v);
                let doc_type = doc_type.map_or("matroska".into(), |v| String::from_utf8_lossy(v).trim_end_matches('\0').to_owned());
                if doc_type != "webm" && doc_type != "matroska" {
                    return Err(Bad("an EBML stream that is neither WebM nor Matroska"));
                }
                // A new stream: its segment says its own scale and tracks.
                self.header_seen = true;
                self.info_seen = false;
                self.scale = 1_000_000;
                self.duration = None;
                self.segment_end = None;
                Ok(Step::Took(whole))
            }
            ID_SEGMENT => {
                if !self.header_seen {
                    return Err(Bad("a WebM segment before its EBML header"));
                }
                // Stepped into, not held.
                self.segment_end = match size {
                    Some(size) => Some(mse_rules::end_of(at, head as u64, size).ok_or(Bad("a WebM segment longer than any"))?),
                    None => None,
                };
                Ok(Step::Took(head))
            }
            ID_INFO | ID_TRACKS => {
                if !self.header_seen {
                    return Err(Bad("a WebM stream that does not start with an EBML header"));
                }
                let whole = known(MAX_META, "a WebM header element larger than any real one")?;
                if b.len() < whole {
                    return Ok(Step::Need(whole));
                }
                let body = &b[head..whole];
                if id == ID_INFO {
                    for (i, v) in elements(body) {
                        match i {
                            ID_TIMECODE_SCALE => self.scale = uint(v).max(1),
                            ID_DURATION => self.duration = Some(float(v)),
                            _ => {}
                        }
                    }
                    self.info_seen = true;
                    if self.tracks.is_some() && !self.init_sent {
                        self.send_init(out);
                    }
                } else {
                    self.parse_tracks(body)?;
                    self.init_sent = false;
                    if self.info_seen {
                        self.send_init(out);
                    }
                }
                Ok(Step::Took(whole))
            }
            ID_CLUSTER => {
                if self.tracks.is_none() {
                    return Err(Bad("a WebM cluster before its tracks"));
                }
                if !self.init_sent {
                    self.send_init(out);
                }
                let end = match size {
                    Some(size) => Some(mse_rules::end_of(at, head as u64, size).ok_or(Bad("a WebM cluster longer than any"))?),
                    None => None,
                };
                self.cluster = Some(Cluster { end, time: None });
                Ok(Step::Took(head))
            }
            // SeekHead, Cues, Tags, Chapters, Attachments, Void, CRC-32 and whatever else: passed over.
            _ => {
                let size = size.ok_or(Bad("a WebM element of unknown size where its size must be known"))?;
                Ok(Step::Skip { head, body: size })
            }
        }
    }

    fn parse_tracks(&mut self, b: &[u8]) -> Result<(), ParseError> {
        let mut infos = Vec::new();
        let mut tracks = Vec::new();
        let mut ignored = Vec::new();
        for (id, entry) in elements(b) {
            if id != ID_TRACK_ENTRY {
                continue;
            }
            let (mut number, mut kind, mut codec_id, mut config) = (0, 0, String::new(), Vec::new());
            let (mut width, mut height, mut rate, mut channels, mut default_duration) = (0, 0, 8000.0, 1, None);
            for (i, v) in elements(entry) {
                match i {
                    ID_TRACK_NUMBER => number = uint(v),
                    ID_TRACK_TYPE => kind = uint(v),
                    ID_CODEC_ID => codec_id = String::from_utf8_lossy(v).trim_end_matches('\0').to_owned(),
                    ID_CODEC_PRIVATE => config = v.to_vec(),
                    ID_DEFAULT_DURATION => default_duration = Some(uint(v)).filter(|d| *d > 0),
                    ID_VIDEO => {
                        for (j, x) in elements(v) {
                            match j {
                                ID_PIXEL_WIDTH => width = uint(x) as u32,
                                ID_PIXEL_HEIGHT => height = uint(x) as u32,
                                _ => {}
                            }
                        }
                    }
                    ID_AUDIO => {
                        for (j, x) in elements(v) {
                            match j {
                                ID_SAMPLING_FREQUENCY => rate = float(x),
                                ID_CHANNELS => channels = uint(x),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            if number == 0 {
                return Err(Bad("a WebM track with no number"));
            }
            if tracks.iter().any(|t: &WebmTrack| t.number == number) || ignored.contains(&number) {
                return Err(Bad("two WebM tracks with one number"));
            }
            let (info, codec) = match kind {
                1 => {
                    let codec = codec_of(&codec_id);
                    let name = match codec_id.as_str() {
                        "V_VP8" => "vp8".to_owned(),
                        "V_VP9" => "vp9".to_owned(),
                        "V_AV1" => "av1".to_owned(),
                        "V_MPEG4/ISO/AVC" => "avc1".to_owned(),
                        "V_MPEGH/ISO/HEVC" => "hev1".to_owned(),
                        other => other.trim_start_matches("V_").to_ascii_lowercase(),
                    };
                    let kind = TrackKind::Video { codec, width, height, config: config.clone() };
                    (TrackInfo { id: number, kind, codec_name: name }, codec)
                }
                2 => {
                    let (codec, cfg, name) = match codec_id.as_str() {
                        "A_OPUS" => (AudioCodec::Opus, config.clone(), "opus".to_owned()),
                        "A_VORBIS" => (AudioCodec::Vorbis, config.clone(), "vorbis".to_owned()),
                        "A_FLAC" => (AudioCodec::Flac, flac_streaminfo(&config), "flac".to_owned()),
                        "A_MPEG/L3" => (AudioCodec::Mp3, Vec::new(), "mp3".to_owned()),
                        id if id.starts_with("A_AAC") => (AudioCodec::Aac, config.clone(), "aac".to_owned()),
                        other => {
                            let name = other.trim_start_matches("A_").to_ascii_lowercase();
                            (AudioCodec::Other(name.clone()), config.clone(), name)
                        }
                    };
                    let kind = TrackKind::Audio { codec, config: cfg, rate: rate as u32, channels: channels.min(u16::MAX as u64) as u16 };
                    (TrackInfo { id: number, kind, codec_name: name }, Codec::Other)
                }
                _ => {
                    ignored.push(number);
                    continue;
                }
            };
            tracks.push(WebmTrack { number, audio: kind == 2, codec, config, default_duration_ns: default_duration, held: None, last_gap: None });
            infos.push(info);
        }
        self.tracks = Some(infos);
        self.webm_tracks = tracks;
        self.ignored = ignored;
        Ok(())
    }

    /// A Block or SimpleBlock's body. `group_key`: a BlockGroup's "no ReferenceBlock"; `None` for a SimpleBlock.
    fn block(&mut self, b: &[u8], group_key: Option<bool>, block_duration: Option<u64>) -> Result<(), ParseError> {
        const CUT: ParseError = Bad("a WebM block cut short");
        let mut s = b;
        let Ok(Some(Some(number))) = read_size(&mut s) else { return Err(Bad("a WebM block's track number")) };
        let Some(ti) = self.webm_tracks.iter().position(|t| t.number == number) else {
            if self.ignored.contains(&number) {
                return Ok(());
            }
            return Err(Bad("a WebM block of a track the initialization segment does not have"));
        };
        let time = self.cluster.as_ref().and_then(|c| c.time).ok_or(Bad("a WebM block before its cluster's time"))?;
        let rel = i16::from_be_bytes(s.get(..2).ok_or(CUT)?.try_into().expect("two bytes"));
        let flags = *s.get(2).ok_or(CUT)?;
        let laces = laces(&s[3..], flags)?;
        let scale = self.scale as i128;
        let track = &self.webm_tracks[ti];
        let key = track.audio || group_key.unwrap_or(flags & 0x80 != 0);
        let default_ns = track.default_duration_ns.map(|d| d as i128);
        let n = laces.len() as i128;
        // BlockDuration, else DefaultDuration, else the gap to the next frame.
        let duration_us = match (block_duration, default_ns) {
            (Some(d), _) => Some((d as i128).saturating_mul(scale) / n / 1000),
            (None, Some(d)) => Some(d / 1000),
            (None, None) => None,
        };
        // A page names the scale: up to 2^64 ns a unit, which times an i64 time leaves i128.
        let start_ns = (time as i128 + rel as i128).saturating_mul(scale);
        let (codec, config) = (track.codec, track.config.clone());
        for (i, data) in laces.into_iter().enumerate() {
            let pts_ns = start_ns.saturating_add(i as i128 * default_ns.unwrap_or(0));
            let pts_us = (pts_ns / 1000).clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            let frame = Frame {
                track: number,
                pts_us,
                dts_us: pts_us,
                duration_us: duration_us.unwrap_or(0).clamp(0, i64::MAX as i128) as i64,
                key,
                data: decoder_data(codec, &config, key, data),
            };
            let t = &mut self.webm_tracks[ti];
            if let Some(mut h) = t.held.take() {
                let gap = pts_us.saturating_sub(h.pts_us);
                if gap >= 0 {
                    t.last_gap = Some(gap);
                }
                h.duration_us = t.last_gap.unwrap_or(0);
                self.frames.push(h);
            }
            if duration_us.is_some() {
                self.frames.push(frame);
            } else {
                t.held = Some(frame);
            }
        }
        Ok(())
    }
}

/// A block's frames after its header: none, Xiph, fixed or EBML lacing (flags 0x06).
fn laces(s: &[u8], flags: u8) -> Result<Vec<&[u8]>, ParseError> {
    const LACING: ParseError = Bad("a WebM block's lacing");
    if flags & 0x06 == 0 {
        return Ok(vec![s]);
    }
    let n = *s.first().ok_or(LACING)? as usize + 1;
    let mut s = &s[1..];
    let mut sizes = Vec::with_capacity(n);
    match flags & 0x06 {
        0x02 => {
            for _ in 0..n - 1 {
                let mut size = 0usize;
                loop {
                    let x = *s.first().ok_or(LACING)?;
                    s = &s[1..];
                    size += x as usize;
                    if x != 255 {
                        break;
                    }
                }
                sizes.push(size);
            }
        }
        0x04 => {
            if !s.len().is_multiple_of(n) {
                return Err(LACING);
            }
            sizes = vec![s.len() / n; n - 1];
        }
        _ => {
            let mut prev = 0i64;
            for i in 0..n - 1 {
                let before = s.len();
                let Ok(Some(Some(v))) = read_size(&mut s) else { return Err(LACING) };
                let len = (before - s.len()) as u32;
                let size = if i == 0 {
                    i64::try_from(v).map_err(|_| LACING)?
                } else {
                    // A signed difference: the value less half its range.
                    prev + (v as i64 - ((1i64 << (7 * len - 1)) - 1))
                };
                if size < 0 {
                    return Err(LACING);
                }
                sizes.push(size as usize);
                prev = size;
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    for size in sizes {
        if size > s.len() {
            return Err(LACING);
        }
        out.push(&s[..size]);
        s = &s[size..];
    }
    out.push(s);
    Ok(out)
}

/// Matroska's A_FLAC CodecPrivate is "fLaC" and the metadata blocks; the decoder is given STREAMINFO's 34 bytes.
fn flac_streaminfo(private: &[u8]) -> Vec<u8> {
    match private.strip_prefix(b"fLaC") {
        Some(rest) if rest.first().is_some_and(|t| t & 0x7f == 0) => rest.get(4..38).map(<[u8]>::to_vec).unwrap_or_default(),
        Some(_) => Vec::new(),
        None if private.len() == 34 => private.to_vec(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_lacings() {
        let data: Vec<u8> = (0..16).collect();
        assert_eq!(laces(&data, 0).unwrap(), vec![&data[..]]);
        // Xiph: three frames of 5, 7 and the rest.
        let mut xiph = vec![2, 5, 7];
        xiph.extend(&data);
        assert_eq!(laces(&xiph, 0x02).unwrap(), vec![&data[..5], &data[5..12], &data[12..]]);
        // Xiph sizes over 255 are a run of 255s.
        let mut long = vec![1, 255, 45];
        long.extend(std::iter::repeat_n(7u8, 301));
        assert_eq!(laces(&long, 0x02).unwrap().iter().map(|l| l.len()).collect::<Vec<_>>(), [300, 1]);
        // Fixed: equal parts, or refused.
        let mut fixed = vec![3];
        fixed.extend(&data);
        assert_eq!(laces(&fixed, 0x04).unwrap(), vec![&data[..4], &data[4..8], &data[8..12], &data[12..]]);
        assert!(laces(&[2, 1, 2, 3, 4], 0x04).is_err());
        // EBML: 5, then 7 as the signed difference +2 (0xC1 is 65 = 2 + 63), then the rest.
        let mut ebml = vec![2, 0x85, 0xC1];
        ebml.extend(&data);
        assert_eq!(laces(&ebml, 0x06).unwrap(), vec![&data[..5], &data[5..12], &data[12..]]);
        // A difference below zero, or sizes past the block: refused.
        assert!(laces(&[2, 0x85, 0x80, 0, 0, 0, 0, 0, 0], 0x06).is_err());
        assert!(laces(&[1, 0x90, 1, 2], 0x06).is_err());
        assert!(laces(&[1, 200, 1, 2], 0x02).is_err());
        assert!(laces(&[], 0x02).is_err());
    }

    #[test]
    fn dops_becomes_an_opus_head() {
        // Version 0, 2 channels, pre-skip 312, 48000 Hz, gain 0, family 0.
        let dops = [0, 2, 0x01, 0x38, 0, 0, 0xbb, 0x80, 0, 0, 0];
        let head = opus_head(&dops).unwrap();
        assert_eq!(&head[..8], b"OpusHead");
        assert_eq!(&head[8..], [1, 2, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
        // Family 1 carries the stream counts and the mapping.
        let dops = [0, 2, 0, 0, 0, 0, 0xbb, 0x80, 0, 0, 1, 1, 1, 0, 1];
        assert_eq!(opus_head(&dops).unwrap()[19..], [1, 1, 0, 1]);
        assert!(opus_head(&dops[..12]).is_none());
    }

    #[test]
    fn esds_and_the_aac_object_type() {
        // ES_Descriptor (3) > DecoderConfigDescriptor (4, AAC 0x40) > DecoderSpecificInfo (5): AAC-LC, 48 kHz, mono.
        let esds = [
            0, 0, 0, 0, 3, 25, 0, 1, 0, 4, 17, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5, 2, 0x11, 0x88, 6, 1, 2,
        ];
        assert_eq!(parse_esds(&esds), Some((0x40, vec![0x11, 0x88])));
        assert_eq!(aac_object_type(&[0x11, 0x88]), Some(2));
        // Object type 31 escapes to 32 + the next 6 bits: 0xF8 0x20 is 32 + 1.
        assert_eq!(aac_object_type(&[0xF8, 0x20]), Some(33));
        // Descriptor sizes in the 4-byte form; and nonsense refused.
        assert_eq!(descriptor(&[5, 0x80, 0x80, 0x80, 2, 9, 9]).map(|d| d.1), Some(&[9, 9][..]));
        assert!(descriptor(&[5, 0x80, 0x80, 0x80, 0x80, 2]).is_none());
        assert!(parse_esds(&[0, 0, 0, 0, 4, 1, 0]).is_none());
    }

    #[test]
    fn flac_streaminfo_from_matroska() {
        let mut private = b"fLaC".to_vec();
        private.extend([0x80, 0, 0, 34]);
        private.extend(1..=34u8);
        assert_eq!(flac_streaminfo(&private), (1..=34u8).collect::<Vec<_>>());
        assert!(flac_streaminfo(b"fLaC").is_empty());
    }
}
