//! User space of the kernel's `/dev/lucina-camera0` (the UVC webcam), for apps such as `lucina-app-camera`.
//!
//! * [`read_info`] / [`parse_info`] read `/proc/lucina-camera`: what the camera offers (formats, sizes, rates, colour
//!   matrix) and what it is doing, including who is capturing.
//! * [`choose_preview`] picks the preview stream from those offers.
//! * [`Camera`] is the device itself: `start` a stream, then `next_frame` for header + frame, `stop` (or just drop it:
//!   closing the file stops the stream).
//! * [`affinity`] pins a thread to the fastest CPUs (the Gold cores).
//! * [`convert`] turns frames into `0x00RRGGBB` pixels, [`photo`] saves a JPEG.
//!
//! The wire protocol itself is [`lucina_uvc::wire`], shared with the kernel.

pub mod affinity;
pub mod convert;
pub mod photo;
#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;

use lucina_uvc::wire::{Command, HEADER_LEN};

pub use lucina_uvc::wire::{Header, StreamRequest};

/// The camera device.
pub const DEVICE: &str = "/dev/lucina-camera0";
/// The camera's description, as text.
pub const PROC: &str = "/proc/lucina-camera";

/// The read buffer before any `start`: big enough for a 1080p YUY2 frame plus slack.
const INITIAL_BUF: usize = 4 * 1024 * 1024;
/// The read buffer never grows past this on `EINVAL`.
const MAX_BUF: usize = 64 * 1024 * 1024;

/// One size a format offers, with the frame rates it can run at (highest first, as `/proc` lists them).
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub fourcc: [u8; 4],
    pub width: u16,
    pub height: u16,
    pub fps: Vec<f32>,
}

/// Which Y'CbCr matrix the camera's uncompressed frames use (UVC's `bMatrixCoefficients`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
}

/// What `/proc/lucina-camera` says. Unknown lines are ignored; nothing here panics on odd input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    /// The `camera:` line, `None` when it says `camera: none` (no camera attached).
    pub name: Option<String>,
    /// Every `WxH` of every `format` line, in order.
    pub offers: Vec<Offer>,
    /// One entry per `format` line: its FOURCC and colour matrix (1 is BT.709, anything else BT.601, UVC's default).
    pub matrix_by_fourcc: Vec<([u8; 4], Matrix)>,
    /// The `state:` line.
    pub state: String,
    /// The `capturing:` line: pid and name of the capturing program, `None` when `capturing: no`.
    pub capturing: Option<(u32, String)>,
}

/// Parses the text of `/proc/lucina-camera`. Never panics; lines it does not know are skipped.
pub fn parse_info(text: &str) -> Info {
    let mut info = Info::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "camera" {
            info.name = if value == "none" {
                None
            } else {
                Some(value.to_string())
            };
        } else if key == "state" {
            info.state = value.to_string();
        } else if key == "capturing" {
            info.capturing = parse_capturing(value);
        } else if let Some(rest) = key.strip_prefix("format ") {
            // `key` is `format <index> <FOURCC>` (the line's first colon is the one after the FOURCC).
            parse_format_line(rest, value, line, &mut info);
        }
    }
    info
}

/// `yes, pid 412 (lucina-app-came)` -> `Some((412, "lucina-app-came"))`; `no` -> `None`.
fn parse_capturing(value: &str) -> Option<(u32, String)> {
    let rest = value
        .strip_prefix("yes")?
        .trim_start()
        .strip_prefix(',')?
        .trim_start();
    let rest = rest.strip_prefix("pid")?.trim_start();
    let (pid, name) = rest.split_once('(')?;
    let pid = pid.trim().parse::<u32>().ok()?;
    let name = name.trim_end().strip_suffix(')')?;
    Some((pid, name.to_string()))
}

/// `head` is `<index> <FOURCC>` (the part of the key after `format `), `frames` the value with one `WxH@rates` per size
/// plus the `(colour p/t/m)` triple, `line` the whole line: one [`Offer`] per `WxH@rates`, and the format's matrix.
fn parse_format_line(head: &str, frames: &str, line: &str, info: &mut Info) {
    let mut head = head.split_ascii_whitespace();
    let _index = head.next();
    let Some(fourcc) = head.next() else {
        return;
    };
    let Ok(fourcc) = <[u8; 4]>::try_from(fourcc.as_bytes()) else {
        return;
    };
    for token in frames.split_ascii_whitespace() {
        let Some((size, rates)) = token.split_once('@') else {
            continue;
        };
        let Some((w, h)) = size.split_once('x') else {
            continue;
        };
        let (Ok(w), Ok(h)) = (w.parse::<u16>(), h.parse::<u16>()) else {
            continue;
        };
        let mut fps = Vec::new();
        for rate in rates.split('/') {
            // A rate written `a-b` is a continuous range; both ends are rates.
            if let Some((hi, lo)) = rate.split_once('-') {
                if let (Ok(hi), Ok(lo)) = (hi.parse::<f32>(), lo.parse::<f32>()) {
                    fps.push(hi);
                    fps.push(lo);
                }
            } else if let Ok(r) = rate.parse::<f32>() {
                fps.push(r);
            }
        }
        info.offers.push(Offer {
            fourcc,
            width: w,
            height: h,
            fps,
        });
    }
    // The colour triple sits in `(colour p/t/m)` at the end of the line; matrix 1 is BT.709, else BT.601.
    if let Some(at) = line.find("(colour") {
        let triple = line[at + "(colour".len()..].trim().trim_end_matches(')');
        let mut parts = triple.split('/');
        if let (Some(_p), Some(_t), Some(m)) = (parts.next(), parts.next(), parts.next())
            && let Ok(m) = m.trim().parse::<u8>()
        {
            let matrix = if m == 1 { Matrix::Bt709 } else { Matrix::Bt601 };
            info.matrix_by_fourcc.push((fourcc, matrix));
        }
    }
}

/// Reads and parses [`PROC`].
pub fn read_info() -> std::io::Result<Info> {
    Ok(parse_info(&std::fs::read_to_string(PROC)?))
}

/// The frame interval (100 ns units, as UVC counts them) for a rate in frames per second, rounded: 30.0 -> 333333.
pub fn interval_for_fps(fps: f32) -> u32 {
    if !fps.is_finite() || fps <= 0.0 {
        return 0;
    }
    (10_000_000.0 / fps).round() as u32
}

/// The preview stream: MJPG 1280x720 at its highest rate if offered; else the MJPG offer with the most pixels not
/// above 1280x720; else YUY2 640x480; else the first offer. Always the chosen offer's highest rate. `None` if the
/// camera offers nothing.
pub fn choose_preview(info: &Info) -> Option<StreamRequest> {
    fn pick(o: &Offer) -> StreamRequest {
        let fps = o.fps.iter().copied().fold(0.0f32, f32::max);
        StreamRequest {
            fourcc: o.fourcc,
            width: o.width,
            height: o.height,
            interval: interval_for_fps(if fps > 0.0 { fps } else { 30.0 }),
        }
    }
    let mjpg = || info.offers.iter().filter(|o| &o.fourcc == b"MJPG");
    if let Some(o) = mjpg().find(|o| o.width == 1280 && o.height == 720) {
        return Some(pick(o));
    }
    if let Some(o) = mjpg()
        .filter(|o| o.width <= 1280 && o.height <= 720)
        .max_by_key(|o| u32::from(o.width) * u32::from(o.height))
    {
        return Some(pick(o));
    }
    if let Some(o) = info
        .offers
        .iter()
        .find(|o| &o.fourcc == b"YUY2" && o.width == 640 && o.height == 480)
    {
        return Some(pick(o));
    }
    info.offers.first().map(pick)
}

/// One frame off the device: the header, then the frame's bytes (YUY2, or one JPEG for MJPG).
#[derive(Clone, Debug)]
pub struct Frame {
    pub header: Header,
    pub data: Vec<u8>,
}

/// Splits a read of `n` bytes into header and frame. A header that does not decode, or whose `bytes` does not match
/// `n - HEADER_LEN`, is an `InvalidData` error.
fn split_frame(buf: &[u8], n: usize) -> std::io::Result<Frame> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bad frame from the camera device",
        )
    };
    if n < HEADER_LEN {
        return Err(invalid());
    }
    let header = Header::decode(&buf[..HEADER_LEN]).ok_or_else(invalid)?;
    if header.bytes as usize != n - HEADER_LEN {
        return Err(invalid());
    }
    Ok(Frame {
        header,
        data: buf[HEADER_LEN..n].to_vec(),
    })
}

/// An open `/dev/lucina-camera0`. Dropping it closes the file, and the kernel stops the stream on close.
pub struct Camera {
    file: File,
    /// The read buffer: `HEADER_LEN + width*height*2 + 65536` of the last `start`, 4 MiB before any.
    buf: Vec<u8>,
}

impl Camera {
    /// Opens [`DEVICE`] for reading and writing. `EBUSY` when another program already has the camera.
    pub fn open() -> std::io::Result<Camera> {
        Camera::open_path(Path::new(DEVICE))
    }

    pub fn open_path(path: &Path) -> std::io::Result<Camera> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Camera {
            file,
            buf: vec![0; INITIAL_BUF],
        })
    }

    /// Starts a stream: one `start <FOURCC> <w> <h> <interval>` line in one write. Also sizes the read buffer for it.
    pub fn start(&mut self, r: StreamRequest) -> std::io::Result<()> {
        self.buf = vec![0; HEADER_LEN + r.width as usize * r.height as usize * 2 + 65536];
        self.write_command(Command::Start(r))
    }

    /// Stops the stream (`stop`); closing the file stops it too.
    pub fn stop(&mut self) -> std::io::Result<()> {
        self.write_command(Command::Stop)
    }

    fn write_command(&mut self, c: Command) -> std::io::Result<()> {
        let line = c.line();
        let n = self.file.write(line.as_bytes())?;
        if n != line.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "short write to the camera device",
            ));
        }
        Ok(())
    }

    /// Waits up to `timeout` (`None` = forever) for a frame with `poll`, then reads it. `Ok(None)` on timeout.
    /// A buffer too small for the header and the frame gets `EINVAL` from the read (that frame is skipped); the buffer
    /// is then doubled, up to 64 MiB, and the read retried.
    pub fn next_frame(&mut self, timeout: Option<Duration>) -> std::io::Result<Option<Frame>> {
        let ms = match timeout {
            None => -1,
            Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
        };
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is one valid pollfd that outlives the call.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            let e = std::io::Error::last_os_error();
            // A signal cut the wait short: no frame yet, as a timeout.
            if e.raw_os_error() == Some(libc::EINTR) {
                return Ok(None);
            }
            return Err(e);
        }
        if ready == 0 {
            return Ok(None);
        }
        loop {
            match self.file.read(&mut self.buf) {
                Ok(n) => return Ok(Some(split_frame(&self.buf, n)?)),
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                    if self.buf.len() >= MAX_BUF {
                        return Err(e);
                    }
                    self.buf.resize((self.buf.len() * 2).min(MAX_BUF), 0);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
