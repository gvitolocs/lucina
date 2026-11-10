//! The UVC descriptors of a raw configuration descriptor: the VideoControl interface's header, and per VideoStreaming
//! interface its formats, their frames and colour matching, and the alternate settings with their isochronous endpoint.
//!
//! After Linux `uvc_driver.c` (`uvc_parse_control`, `uvc_parse_streaming`, `uvc_parse_format`, `uvc_parse_frame`) and the
//! UVC 1.1 specification, sections 3.7-3.9 and the payload specifications for uncompressed and MJPEG video. As in Linux, a
//! format or frame descriptor too short for its fields makes the whole streaming interface unusable; an unknown
//! class-specific descriptor is skipped.

use alloc::vec::Vec;

/// `bDescriptorType`s.
const DT_INTERFACE: u8 = 0x04;
const DT_ENDPOINT: u8 = 0x05;
const DT_CS_INTERFACE: u8 = 0x24;
/// `bInterfaceClass` video, and its subclasses.
pub const CLASS_VIDEO: u8 = 0x0e;
pub const SUBCLASS_CONTROL: u8 = 0x01;
pub const SUBCLASS_STREAMING: u8 = 0x02;
/// VideoControl `bDescriptorSubtype`.
const VC_HEADER: u8 = 0x01;
/// VideoStreaming `bDescriptorSubtype`s.
const VS_INPUT_HEADER: u8 = 0x01;
const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;
const VS_FRAME_UNCOMPRESSED: u8 = 0x05;
const VS_FORMAT_MJPEG: u8 = 0x06;
const VS_FRAME_MJPEG: u8 = 0x07;
const VS_COLORFORMAT: u8 = 0x0d;

/// The GUID of an uncompressed format is its FourCC followed by this (the UVC "uncompressed payload" table 2-1).
const GUID_TAIL: [u8; 12] = [0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// A descriptor's `bLength` is below 2 or runs past the buffer.
    Truncated,
    /// A format or frame descriptor is too short for its fields, or a frame comes before any format.
    Malformed,
    /// No VideoControl or no VideoStreaming interface.
    NotVideo,
}

/// What `bFrameIntervalType` gives, in 100 ns units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intervals {
    /// Discrete intervals, as listed (the specification wants them in increasing order).
    Discrete(Vec<u32>),
    Continuous { min: u32, max: u32, step: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub index: u8,
    pub width: u16,
    pub height: u16,
    pub max_bit_rate: u32,
    /// `dwMaxVideoFrameBufferSize`.
    pub max_frame_size: u32,
    pub default_interval: u32,
    pub intervals: Intervals,
}

/// `VS_COLORFORMAT`: the codes of UVC 1.1 table 3-19. Without one, the specification's defaults hold: BT.709 primaries and
/// transfer, SMPTE 170M (BT.601) matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colour {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
}

impl Colour {
    pub const DEFAULT: Colour = Colour { primaries: 1, transfer: 1, matrix: 4 };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Format {
    pub index: u8,
    /// `YUY2`, `NV12`, `MJPG`, ... An uncompressed format with a GUID outside the FourCC family has 0xff and the GUID's first
    /// three bytes here.
    pub fourcc: [u8; 4],
    pub compressed: bool,
    /// Uncompressed only; 0 for MJPEG.
    pub bits_per_pixel: u8,
    pub default_frame: u8,
    pub frames: Vec<Frame>,
    pub colour: Colour,
}

impl Format {
    pub fn frame(&self, index: u8) -> Option<&Frame> {
        self.frames.iter().find(|f| f.index == index)
    }

    /// The FourCC as text, for logs.
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.fourcc).unwrap_or("????")
    }
}

/// An alternate setting of a VideoStreaming interface with its isochronous IN endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AltSetting {
    pub alt: u8,
    pub endpoint: u8,
    pub max_packet: u16,
    /// What one service interval moves: `wMaxPacketSize` bits 0-10 times (bits 11-12 plus one), Linux `uvc_endpoint_max_bpi`
    /// for high speed.
    pub bytes_per_interval: u32,
    pub interval: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Streaming {
    pub interface: u8,
    /// From the input header (`bEndpointAddress`).
    pub endpoint: u8,
    /// `bStillCaptureMethod` of the input header.
    pub still_method: u8,
    pub formats: Vec<Format>,
    /// Alternate settings above 0 that have an isochronous IN endpoint, in descriptor order.
    pub alts: Vec<AltSetting>,
    /// An alternate setting 0 with a bulk IN endpoint: the stream is bulk, not isochronous.
    pub bulk_endpoint: Option<(u8, u16)>,
}

impl Streaming {
    pub fn format(&self, index: u8) -> Option<&Format> {
        self.formats.iter().find(|f| f.index == index)
    }

    /// The largest bandwidth any alternate setting offers (Linux `stream->maxpsize`).
    pub fn max_bytes_per_interval(&self) -> u32 {
        self.alts.iter().map(|a| a.bytes_per_interval).max().unwrap_or(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Camera {
    pub control_interface: u8,
    /// `bcdUVC`.
    pub uvc_version: u16,
    pub clock_frequency: u32,
    pub streaming: Vec<Streaming>,
}

fn u16_at(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}

fn u32_at(d: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}

/// Splits a configuration descriptor into its descriptors.
pub fn descriptors(raw: &[u8]) -> Result<Vec<&[u8]>, Error> {
    let mut out = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        let len = rest[0] as usize;
        if len < 2 || len > rest.len() {
            return Err(Error::Truncated);
        }
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
    Ok(out)
}

/// Whether a configuration descriptor has a video interface (the kernel's driver match).
pub fn is_video(raw: &[u8]) -> bool {
    descriptors(raw).is_ok_and(|ds| ds.iter().any(|d| d[1] == DT_INTERFACE && d.len() >= 9 && d[5] == CLASS_VIDEO))
}

fn parse_frame(d: &[u8]) -> Result<Frame, Error> {
    // Linux uvc_parse_frame: at least 26 bytes, and 26 + 4 per interval (continuous: 3 values).
    if d.len() < 26 {
        return Err(Error::Malformed);
    }
    let kind = d[25];
    let count = if kind == 0 { 3 } else { kind as usize };
    if d.len() < 26 + 4 * count {
        return Err(Error::Malformed);
    }
    let values: Vec<u32> = (0..count).map(|i| u32_at(d, 26 + 4 * i)).collect();
    let intervals = if kind == 0 {
        Intervals::Continuous { min: values[0], max: values[1], step: values[2] }
    } else {
        Intervals::Discrete(values)
    };
    Ok(Frame {
        index: d[3],
        width: u16_at(d, 5),
        height: u16_at(d, 7),
        max_bit_rate: u32_at(d, 13),
        max_frame_size: u32_at(d, 17),
        default_interval: u32_at(d, 21),
        intervals,
    })
}

fn streaming_interface(camera: &mut Camera, number: u8) -> Option<&mut Streaming> {
    camera.streaming.iter_mut().find(|s| s.interface == number)
}

/// The UVC description of a configuration descriptor.
pub fn parse(raw: &[u8]) -> Result<Camera, Error> {
    let ds = descriptors(raw)?;
    let mut camera = Camera { control_interface: 0, uvc_version: 0, clock_frequency: 0, streaming: Vec::new() };
    let mut have_control = false;
    // The interface the descriptors are in: (number, alt, subclass), or not a video one.
    let mut current: Option<(u8, u8, u8)> = None;
    for d in ds {
        match d[1] {
            DT_INTERFACE => {
                if d.len() < 9 {
                    return Err(Error::Truncated);
                }
                current = (d[5] == CLASS_VIDEO).then_some((d[2], d[3], d[6]));
                match current {
                    Some((number, 0, SUBCLASS_STREAMING)) if streaming_interface(&mut camera, number).is_none() => {
                        camera.streaming.push(Streaming {
                            interface: number,
                            endpoint: 0,
                            still_method: 0,
                            formats: Vec::new(),
                            alts: Vec::new(),
                            bulk_endpoint: None,
                        });
                    }
                    Some((number, _, SUBCLASS_CONTROL)) if !have_control => {
                        camera.control_interface = number;
                        have_control = true;
                    }
                    _ => {}
                }
            }
            DT_CS_INTERFACE => match current {
                Some((_, _, SUBCLASS_CONTROL)) => {
                    if d.len() >= 3 && d[2] == VC_HEADER {
                        if d.len() < 12 {
                            return Err(Error::Malformed);
                        }
                        camera.uvc_version = u16_at(d, 3);
                        camera.clock_frequency = u32_at(d, 7);
                    }
                }
                Some((number, _, SUBCLASS_STREAMING)) => {
                    let Some(stream) = streaming_interface(&mut camera, number) else {
                        continue;
                    };
                    if d.len() < 3 {
                        continue;
                    }
                    match d[2] {
                        VS_INPUT_HEADER => {
                            if d.len() < 13 {
                                return Err(Error::Malformed);
                            }
                            stream.endpoint = d[6];
                            stream.still_method = d[9];
                        }
                        VS_FORMAT_UNCOMPRESSED => {
                            if d.len() < 27 {
                                return Err(Error::Malformed);
                            }
                            // A GUID of the FourCC family is its FourCC; any other keeps its first three bytes behind a
                            // leading 0xff, which no FourCC has.
                            let fourcc = if d[9..21] == GUID_TAIL { [d[5], d[6], d[7], d[8]] } else { [0xff, d[5], d[6], d[7]] };
                            stream.formats.push(Format {
                                index: d[3],
                                fourcc,
                                compressed: false,
                                bits_per_pixel: d[21],
                                default_frame: d[22],
                                frames: Vec::new(),
                                colour: Colour::DEFAULT,
                            });
                        }
                        VS_FORMAT_MJPEG => {
                            if d.len() < 11 {
                                return Err(Error::Malformed);
                            }
                            stream.formats.push(Format {
                                index: d[3],
                                fourcc: *b"MJPG",
                                compressed: true,
                                bits_per_pixel: 0,
                                default_frame: d[6],
                                frames: Vec::new(),
                                colour: Colour::DEFAULT,
                            });
                        }
                        VS_FRAME_UNCOMPRESSED | VS_FRAME_MJPEG => {
                            let compressed = d[2] == VS_FRAME_MJPEG;
                            let Some(format) = stream.formats.last_mut() else {
                                return Err(Error::Malformed);
                            };
                            if format.compressed != compressed {
                                return Err(Error::Malformed);
                            }
                            format.frames.push(parse_frame(d)?);
                        }
                        VS_COLORFORMAT => {
                            if d.len() < 6 {
                                return Err(Error::Malformed);
                            }
                            if let Some(format) = stream.formats.last_mut() {
                                format.colour = Colour { primaries: d[3], transfer: d[4], matrix: d[5] };
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            DT_ENDPOINT => {
                let Some((number, alt, SUBCLASS_STREAMING)) = current else {
                    continue;
                };
                if d.len() < 7 {
                    return Err(Error::Truncated);
                }
                let Some(stream) = streaming_interface(&mut camera, number) else {
                    continue;
                };
                let (address, attributes, max_packet, interval) = (d[2], d[3], u16_at(d, 4), d[6]);
                let input = address & 0x80 != 0;
                match attributes & 0x3 {
                    // Isochronous.
                    1 if input && alt != 0 => {
                        let size = u32::from(max_packet & 0x7ff);
                        let mult = u32::from((max_packet >> 11) & 0x3) + 1;
                        stream.alts.push(AltSetting { alt, endpoint: address, max_packet, bytes_per_interval: size * mult, interval });
                    }
                    // Bulk.
                    2 if input && alt == 0 => stream.bulk_endpoint = Some((address, max_packet)),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    if !have_control || camera.streaming.is_empty() {
        return Err(Error::NotVideo);
    }
    Ok(camera)
}
