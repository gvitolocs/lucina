//! Coded frames, as Media Source Extensions hands them over with the codec's configuration from an init segment, to the same output as
//! [`crate::Decoder`]: interleaved S16LE stereo at 48 kHz. The decoders and the samples-to-bytes path are the ones [`crate::Decoder`] uses,
//! so a stream of packets decodes exactly as the same packets in a file.

use symphonia::core::audio::Channels;
use symphonia::core::audio::sample::SampleFormat;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_PCM_S16LE, CODEC_ID_VORBIS};
use symphonia::core::codecs::audio::{AudioCodecId, AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::packet::Packet;
use symphonia::core::units::{Duration, Timestamp};

use crate::{Codec, DecodeError, OUTPUT_RATE, Resampler, push_samples};

/// What the container said the sound is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioConfig {
    /// AAC: the AudioSpecificConfig (MP4 esds DecoderSpecificInfo, or Matroska A_AAC CodecPrivate).
    Aac { asc: Vec<u8> },
    /// MP3 (MP4 object type 0x6B/0x69, Matroska A_MPEG/L3). No configuration.
    Mp3,
    /// Vorbis: Matroska A_VORBIS CodecPrivate as stored (Xiph lacing of the three headers).
    Vorbis { codec_private: Vec<u8> },
    /// FLAC: the STREAMINFO block body (34 bytes), from MP4 dfLa or Matroska A_FLAC CodecPrivate ("fLaC" + metadata blocks).
    Flac { streaminfo: Vec<u8> },
    /// Raw little-endian PCM, 16-bit signed.
    PcmS16Le,
    /// Opus: the OpusHead bytes (MP4 `dOps` rebuilt as an OpusHead by the MSE parser, or WebM CodecPrivate).
    Opus { head: Vec<u8> },
}

/// An opus1 decoder at 48 kHz from the OpusHead, with the channel count and the pre-skip it states. Only mapping family 0 or 1 with at
/// most two channels is supported.
pub(crate) fn opus_decoder_from_head(head: &[u8]) -> Result<(opus1::OpusDecoder, usize, usize), DecodeError> {
    let bad = |why: &str| DecodeError::Unsupported(format!("opus head: {why}"));
    if head.len() < 19 || &head[0..8] != b"OpusHead" {
        return Err(bad("not an OpusHead"));
    }
    // Ogg and WebM say version 1, the MP4 dOps box says 0.
    if !matches!(head[8], 0 | 1) {
        return Err(bad("unsupported version"));
    }
    let channels = usize::from(head[9]);
    if channels < 1 || channels > 2 {
        return Err(bad("only one or two channels"));
    }
    if !matches!(head[18], 0 | 1) {
        return Err(bad("unsupported channel mapping family"));
    }
    // The OpusHead's pre-skip is little-endian; the MP4 dOps box (version 0) stores it big-endian.
    let pre_skip = if head[8] == 0 {
        u16::from_be_bytes([head[10], head[11]])
    } else {
        u16::from_le_bytes([head[10], head[11]])
    };
    let pre_skip = usize::from(pre_skip);
    let decoder = opus1::OpusDecoder::new(
        opus1::SampleRate::Hz48000,
        if channels == 1 { opus1::Channels::Mono } else { opus1::Channels::Stereo },
    )
    .map_err(|err| DecodeError::Unsupported(format!("opus: {err}")))?;
    Ok((decoder, channels, pre_skip))
}

/// Decodes coded frames one by one, as an MSE SourceBuffer receives them.
pub struct PacketDecoder {
    codec: Codec,
    resampler: Option<Resampler>,
    rate: u32,
    channels: usize,
    /// Interleaved stereo at the current rate, before resampling.
    stereo: Vec<i16>,
    samples: Vec<i16>,
    skipped_packets: u64,
}

impl PacketDecoder {
    /// `rate` and `channels` as the container says (they may be 0 when unknown; the codec's own then wins).
    pub fn new(config: &AudioConfig, rate: u32, channels: u16) -> Result<PacketDecoder, DecodeError> {
        let rate = if rate == 0 { OUTPUT_RATE } else { rate };
        let channels = usize::from(channels.max(1));
        let mut opus_channels: Option<usize> = None;
        let codec = match config {
            AudioConfig::Aac { asc } => Codec::Aac {
                decoder: Box::new(
                    syom::Decoder::from_asc(asc, syom::DecodeOptions::audio())
                        .map_err(|err| DecodeError::Unsupported(err.to_string()))?,
                ),
                asc: asc.clone(),
            },
            AudioConfig::Mp3 => Codec::Symphonia(make_decoder(params(CODEC_ID_MP3, rate, channels, None))?),
            AudioConfig::Vorbis { codec_private } => Codec::Symphonia(make_decoder(params(
                CODEC_ID_VORBIS,
                rate,
                channels,
                Some(vorbis_extra_data_from_codec_private(codec_private)?.into_boxed_slice()),
            ))?),
            AudioConfig::Flac { streaminfo } => Codec::Symphonia(make_decoder(params(
                CODEC_ID_FLAC,
                rate,
                channels,
                Some(streaminfo.clone().into_boxed_slice()),
            ))?),
            AudioConfig::PcmS16Le => {
                let mut p = params(CODEC_ID_PCM_S16LE, rate, channels, None);
                p.with_sample_format(SampleFormat::S16).with_bits_per_sample(16);
                Codec::Symphonia(make_decoder(p)?)
            }
            AudioConfig::Opus { head } => {
                let (decoder, head_channels, pre_skip) = opus_decoder_from_head(head)?;
                opus_channels = Some(head_channels);
                Codec::Opus { decoder, pre_skip, skip_left: pre_skip }
            }
        };
        // Opus always comes out at 48 kHz.
        let rate = if matches!(codec, Codec::Opus { .. }) { OUTPUT_RATE } else { rate };
        let channels = opus_channels.unwrap_or(channels);
        let resampler = (rate != OUTPUT_RATE).then(|| Resampler::new(rate, OUTPUT_RATE));
        Ok(PacketDecoder {
            codec,
            resampler,
            rate,
            channels,
            stereo: Vec::new(),
            samples: Vec::new(),
            skipped_packets: 0,
        })
    }

    /// Decodes one coded frame and appends interleaved S16LE stereo at 48 kHz ([`OUTPUT_RATE`]) to `out`. A frame that does not decode is
    /// skipped and counted in `skipped_packets`, as [`crate::Decoder::next_block`] does; `Ok(())` then.
    pub fn decode(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), DecodeError> {
        let packet = Packet::new(0, Timestamp::from(0), Duration::ZERO, frame.to_vec());
        match &mut self.codec {
            Codec::Symphonia(decoder) => match decoder.decode(&packet) {
                Ok(decoded) => {
                    self.channels = decoded.spec().channels().count().max(1);
                    self.samples.resize(decoded.samples_interleaved(), 0);
                    decoded.copy_to_slice_interleaved(&mut self.samples);
                }
                // A frame that does not decode, truncated garbage included, is skipped and counted.
                Err(SymphoniaError::DecodeError(_)) => {
                    self.skipped_packets += 1;
                    return Ok(());
                }
                Err(SymphoniaError::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.skipped_packets += 1;
                    return Ok(());
                }
                Err(SymphoniaError::ResetRequired) => {
                    decoder.reset();
                    return Ok(());
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
                    return Ok(());
                }
                if samples.is_empty() {
                    return Ok(());
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
                match decoder.decode_i16(frame, 5760, &mut self.samples) {
                    Ok(frames) => {
                        self.samples.truncate(frames * channels);
                        if *skip_left > 0 {
                            let drop = (*skip_left).min(frames);
                            self.samples.drain(..drop * channels);
                            *skip_left -= drop;
                        }
                    }
                    Err(_) => {
                        self.skipped_packets += 1;
                        return Ok(());
                    }
                }
            }
        }
        push_samples(&self.samples, self.channels, &mut self.resampler, &mut self.stereo, out);
        Ok(())
    }

    /// After a seek or a gap: the codec and resampler start again (the configuration is kept).
    pub fn reset(&mut self) {
        self.codec.reset();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
    }

    /// A start from 0, as opposed to a seek into the middle: the Opus pre-skip is dropped again.
    pub fn reset_to_start(&mut self) {
        self.codec.reset_to_start();
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.reset();
        }
    }

    /// What the resampler still holds, at the end of the stream.
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        if let Some(resampler) = self.resampler.as_mut() {
            resampler.flush(out);
        }
    }

    pub fn skipped_packets(&self) -> u64 {
        self.skipped_packets
    }
}

fn params(
    codec: AudioCodecId,
    rate: u32,
    channels: usize,
    extra_data: Option<Box<[u8]>>,
) -> AudioCodecParameters {
    let mut p = AudioCodecParameters::new();
    p.for_codec(codec).with_sample_rate(rate).with_channels(Channels::Discrete(channels as u16));
    if let Some(extra_data) = extra_data {
        p.with_extra_data(extra_data);
    }
    p
}

fn make_decoder(params: AudioCodecParameters) -> Result<Box<dyn symphonia::core::codecs::audio::AudioDecoder>, DecodeError> {
    symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|err| DecodeError::Unsupported(err.to_string()))
}

/// The layout symphonia's Vorbis codec expects, built from the Matroska CodecPrivate exactly as symphonia-format-mkv does: the Xiph-laced
/// packets are split, and the identification and setup headers are concatenated.
fn vorbis_extra_data_from_codec_private(codec_private: &[u8]) -> Result<Vec<u8>, DecodeError> {
    const VORBIS_PACKET_TYPE_IDENTIFICATION: u8 = 1;
    const VORBIS_PACKET_TYPE_SETUP: u8 = 5;

    let bad = || DecodeError::Unsupported(String::from("vorbis CodecPrivate is not Xiph lacing"));
    let mut pos = 0usize;
    // 1 byte: number of packets minus one; then that many Xiph-coded lengths; the last packet is the remainder.
    let packet_count = *codec_private.first().ok_or_else(bad)? as usize;
    pos += 1;
    let mut packets: Vec<&[u8]> = Vec::new();
    for _ in 0..packet_count {
        let mut length = 0usize;
        loop {
            let b = *codec_private.get(pos).ok_or_else(bad)?;
            pos += 1;
            length += usize::from(b);
            if b != 255 {
                break;
            }
        }
        let end = pos.checked_add(length).ok_or_else(bad)?;
        packets.push(codec_private.get(pos..end).ok_or_else(bad)?);
        pos = end;
    }
    packets.push(codec_private.get(pos..).ok_or_else(bad)?);

    let mut ident_header = None;
    let mut setup_header = None;
    for packet in packets {
        match packet.first().copied() {
            Some(VORBIS_PACKET_TYPE_IDENTIFICATION) => ident_header = Some(packet),
            Some(VORBIS_PACKET_TYPE_SETUP) => setup_header = Some(packet),
            _ => {}
        }
    }
    let ident = ident_header.ok_or_else(|| DecodeError::Unsupported(String::from("vorbis CodecPrivate has no identification header")))?;
    let setup = setup_header.ok_or_else(|| DecodeError::Unsupported(String::from("vorbis CodecPrivate has no setup header")))?;
    Ok([ident, setup].concat())
}
