//! A file becomes 16-bit stereo at 48 kHz, which is all Juno's speakers play. [`Decoder`] gives it block by block and can seek.
//!
//! Mono is doubled; more than two channels are folded to the first two with the rest dropped (music on this laptop is stereo, and a
//! proper downmix needs the channel layout a file may not state). Other rates go through [`resample::Resampler`].

pub mod device;
pub mod engine;
pub mod packet;
pub mod resample;

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

pub use resample::Resampler;

pub const OUTPUT_RATE: u32 = 48_000;

#[derive(Debug)]
pub enum DecodeError {
    Open(std::io::Error),
    /// Not a file this can read, with what the library said.
    Unsupported(String),
    /// The stream broke, past the point of skipping a bad packet.
    Broken(String),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Open(err) => write!(f, "cannot open: {err}"),
            DecodeError::Unsupported(why) => write!(f, "not a format this plays: {why}"),
            DecodeError::Broken(why) => write!(f, "the file is damaged: {why}"),
        }
    }
}

/// symphonia wants its source `Sync`; only its one thread ever uses this.
struct Reader<R>(std::sync::Mutex<R>);

impl<R: std::io::Read> std::io::Read for Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.get_mut().map_err(|_| std::io::Error::other("a reader panicked"))?.read(buf)
    }
}

impl<R: std::io::Seek> std::io::Seek for Reader<R> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.0.get_mut().map_err(|_| std::io::Error::other("a reader panicked"))?.seek(pos)
    }
}

impl<R: std::io::Read + std::io::Seek + Send> MediaSource for Reader<R> {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// What decodes the packets symphonia's demuxer reads.
pub(crate) enum Codec {
    Symphonia(Box<dyn AudioDecoder>),
    /// AAC, LC and HE, by syom from the track's `AudioSpecificConfig`, kept to start again after a seek or a bad packet.
    Aac { decoder: Box<syom::Decoder>, asc: Vec<u8> },
    /// Opus, by opus1 at 48 kHz from the track's OpusHead. `skip_left` is the pre-skip still to drop, as RFC 7845 says.
    Opus { decoder: opus1::OpusDecoder, pre_skip: usize, skip_left: usize },
}

impl Codec {
    pub(crate) fn reset(&mut self) {
        match self {
            Codec::Symphonia(decoder) => decoder.reset(),
            Codec::Aac { decoder, asc } => {
                decoder.reset();
                let _ = decoder.set_asc(asc);
            }
            Codec::Opus { decoder, .. } => decoder.reset_state(),
        }
    }

    /// Starts again from the beginning of the stream: Opus drops its pre-skip again.
    pub(crate) fn reset_to_start(&mut self) {
        self.reset();
        if let Codec::Opus { pre_skip, skip_left, .. } = self {
            *skip_left = *pre_skip;
        }
    }
}

/// Interleaved `samples` with `channels` per frame become interleaved stereo at the current rate, resampled to [`OUTPUT_RATE`], and
/// appended to `out` as S16LE bytes. The one path every decoder's output goes through, so files and packets cannot drift apart.
pub(crate) fn push_samples(
    samples: &[i16],
    channels: usize,
    resampler: &mut Option<Resampler>,
    stereo: &mut Vec<i16>,
    out: &mut Vec<u8>,
) {
    stereo.clear();
    match channels {
        1 => {
            for s in samples {
                stereo.extend_from_slice(&[*s, *s]);
            }
        }
        n => {
            for frame in samples.chunks_exact(n) {
                stereo.extend_from_slice(&frame[..2]);
            }
        }
    }
    match resampler.as_mut() {
        Some(resampler) => resampler.process(stereo, out),
        None => {
            out.reserve(stereo.len() * 2);
            for s in stereo {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
    }
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    codec: Codec,
    track: u32,
    resampler: Option<Resampler>,
    rate: u32,
    channels: usize,
    duration_ms: Option<u64>,
    /// Interleaved stereo at the file's rate, before resampling.
    stereo: Vec<i16>,
    /// Packets skipped because they did not decode: a damaged stretch, not the end.
    pub skipped_packets: u64,
    samples: Vec<i16>,
}

impl Decoder {
    pub fn open(path: &Path) -> Result<Decoder, DecodeError> {
        let file = File::open(path).map_err(DecodeError::Open)?;
        let mut hint = Hint::new();
        if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(extension);
        }
        Decoder::from_source(Box::new(file), &hint)
    }

    /// From anything that can be read and sought, such as a file on the server through ranges.
    pub fn from_reader<R: std::io::Read + std::io::Seek + Send + 'static>(reader: R) -> Result<Decoder, DecodeError> {
        Decoder::from_source(Box::new(Reader(std::sync::Mutex::new(reader))), &Hint::new())
    }

    fn from_source(source: Box<dyn MediaSource>, hint: &Hint) -> Result<Decoder, DecodeError> {
        let stream = MediaSourceStream::new(source, Default::default());
        let format = symphonia::default::get_probe()
            .probe(hint, stream, FormatOptions::default(), MetadataOptions::default())
            .map_err(|err| DecodeError::Unsupported(err.to_string()))?;
        let track = format
            .default_track(TrackType::Audio)
            .ok_or_else(|| DecodeError::Unsupported(String::from("no audio in it")))?;
        let track_id = track.id;
        let Some(CodecParameters::Audio(params)) = track.codec_params.as_ref() else {
            return Err(DecodeError::Unsupported(String::from("no audio parameters")));
        };
        let mut opus_channels: Option<usize> = None;
        let codec = if params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS {
            let head = params
                .extra_data
                .as_deref()
                .ok_or_else(|| DecodeError::Unsupported(String::from("opus without an OpusHead")))?;
            let (decoder, channels, pre_skip) = packet::opus_decoder_from_head(head)?;
            opus_channels = Some(channels);
            Codec::Opus { decoder, pre_skip, skip_left: pre_skip }
        } else {
            match params.extra_data.as_deref() {
                Some(asc) if params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_AAC => Codec::Aac {
                    decoder: Box::new(
                        syom::Decoder::from_asc(asc, syom::DecodeOptions::audio())
                            .map_err(|err| DecodeError::Unsupported(err.to_string()))?,
                    ),
                    asc: asc.to_vec(),
                },
                _ => Codec::Symphonia(
                    symphonia::default::get_codecs()
                        .make_audio_decoder(params, &AudioDecoderOptions::default())
                        .map_err(|err| DecodeError::Unsupported(err.to_string()))?,
                ),
            }
        };
        // Opus always comes out at 48 kHz, whatever the container says.
        let rate = if matches!(codec, Codec::Opus { .. }) {
            OUTPUT_RATE
        } else {
            params.sample_rate.filter(|r| *r != 0).unwrap_or(OUTPUT_RATE)
        };
        let channels = opus_channels.unwrap_or_else(|| params.channels.as_ref().map_or(2, |c| c.count())).max(1);
        let duration_ms = match (track.num_frames, track.time_base) {
            (Some(frames), Some(base)) => base
                .calc_time(symphonia::core::units::Timestamp::from(frames as i64))
                .map(|t| (t.as_secs_f64() * 1000.0) as u64),
            _ => None,
        };
        let resampler = (rate != OUTPUT_RATE).then(|| Resampler::new(rate, OUTPUT_RATE));
        Ok(Decoder {
            format,
            codec,
            track: track_id,
            resampler,
            rate,
            channels,
            duration_ms,
            stereo: Vec::new(),
            skipped_packets: 0,
            samples: Vec::new(),
        })
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// The next block of interleaved S16LE stereo at 48 kHz, as bytes appended to `out`. `false` at the end of the file. A packet that
    /// does not decode is skipped and counted, as players do: a scratch is better than a stop.
    pub fn next_block(&mut self, out: &mut Vec<u8>) -> Result<bool, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.flush_resampler(out);
                    return Ok(false);
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.reset_codec();
                    continue;
                }
                Err(SymphoniaError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.flush_resampler(out);
                    return Ok(false);
                }
                Err(err) => return Err(DecodeError::Broken(err.to_string())),
            };
            if packet.track_id != self.track {
                continue;
            }
            match &mut self.codec {
                Codec::Symphonia(decoder) => match decoder.decode(&packet) {
                    Ok(decoded) => {
                        let channels = decoded.spec().channels().count().max(1);
                        self.channels = channels;
                        self.samples.resize(decoded.samples_interleaved(), 0);
                        decoded.copy_to_slice_interleaved(&mut self.samples);
                    }
                    Err(SymphoniaError::DecodeError(_)) => {
                        self.skipped_packets += 1;
                        continue;
                    }
                    Err(SymphoniaError::ResetRequired) => {
                        decoder.reset();
                        continue;
                    }
                    Err(err) => return Err(DecodeError::Broken(err.to_string())),
                },
                Codec::Aac { decoder, asc } => {
                    let (samples, channels) = (&mut self.samples, &mut self.channels);
                    let mut rate = 0;
                    samples.clear();
                    let decoded = decoder.decode_au(&packet.data, |frame| {
                        rate = frame.sample_rate;
                        *channels = frame.planar.len().max(1);
                        for i in 0..frame.samples {
                            for plane in frame.planar {
                                let v = plane.get(i).copied().unwrap_or(0.0);
                                samples.push((v * 32767.0).round().clamp(-32768.0, 32767.0) as i16);
                            }
                        }
                        Ok(())
                    });
                    if decoded.is_err() {
                        // A failed instance takes nothing more until it is reset.
                        self.skipped_packets += 1;
                        decoder.reset();
                        let _ = decoder.set_asc(asc);
                        continue;
                    }
                    if samples.is_empty() {
                        continue;
                    }
                    // HE-AAC plays at twice its core's rate, which the container may give: the frames say.
                    if rate != 0 && rate != self.rate {
                        self.rate = rate;
                        self.resampler = (rate != OUTPUT_RATE).then(|| Resampler::new(rate, OUTPUT_RATE));
                    }
                }
                Codec::Opus { decoder, skip_left, .. } => {
                    let channels = self.channels;
                    // 120 ms at 48 kHz is the longest frame Opus has.
                    self.samples.resize(5760 * channels, 0);
                    match decoder.decode_i16(&packet.data, 5760, &mut self.samples) {
                        Ok(frames) => {
                            self.samples.truncate(frames * channels);
                            // The pre-skip of the OpusHead, dropped once at the start and after a seek to 0.
                            if *skip_left > 0 {
                                let drop = (*skip_left).min(frames);
                                self.samples.drain(..drop * channels);
                                *skip_left -= drop;
                            }
                        }
                        Err(_) => {
                            self.skipped_packets += 1;
                            continue;
                        }
                    }
                }
            }
            push_samples(&self.samples, self.channels, &mut self.resampler, &mut self.stereo, out);
            return Ok(true);
        }
    }

    fn reset_codec(&mut self) {
        self.codec.reset();
    }

    fn flush_resampler(&mut self, out: &mut Vec<u8>) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.flush(out);
        }
    }

    /// Goes to about `ms` from the start. The next block starts at the packet found, which gives where it really landed.
    pub fn seek_ms(&mut self, ms: u64) -> Result<u64, DecodeError> {
        let seeked = self
            .format
            .seek(
                SeekMode::Coarse,
                SeekTo::Time {
                    time: Time::try_from_secs_f64(ms as f64 / 1000.0).unwrap_or(Time::ZERO),
                    track_id: Some(self.track),
                },
            )
            .map_err(|err| DecodeError::Broken(err.to_string()))?;
        // A seek to 0 starts the stream again, so the Opus pre-skip is dropped again; a seek into the middle must not drop samples.
        if ms == 0 {
            self.codec.reset_to_start();
        } else {
            self.reset_codec();
        }
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
        let landed = self
            .format
            .tracks()
            .iter()
            .find(|t| t.id == self.track)
            .and_then(|t| t.time_base)
            .and_then(|base| base.calc_time(seeked.actual_ts))
            .map_or(ms, |t| (t.as_secs_f64() * 1000.0) as u64);
        Ok(landed)
    }
}
