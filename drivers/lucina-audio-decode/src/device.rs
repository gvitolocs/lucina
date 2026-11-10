//! The sound device as a program sees it: samples into `/dev/lucina-audio`, everything else through `/dev/lucina-audio-ctl`. [`Sink`]
//! lets a test stand in for it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;

const PCM: &str = "/dev/lucina-audio";
const CTL: &str = "/dev/lucina-audio-ctl";
const O_NONBLOCK: i32 = 0o4000;

pub trait Sink: Send {
    /// Takes the device. Fails if another program has it. The kernel starts a program at -30 dB, so set the volume after this.
    fn open(&mut self) -> io::Result<()>;
    /// Lets it go: what is queued plays out unless [`Self::flush`] went first.
    fn close(&mut self);
    /// As many bytes as fit, without blocking; `WouldBlock` when none do.
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize>;
    /// Drops what is queued.
    fn flush(&mut self);
    /// The kernel clamps it to its ceiling.
    fn set_volume(&mut self, db: i32);
    /// Frames played since the stream started. Goes back to 0 when a new one starts.
    fn played_frames(&mut self) -> u64;
}

/// The real one.
#[derive(Default)]
pub struct Device {
    pcm: Option<File>,
}

impl Device {
    pub fn new() -> Device {
        Device::default()
    }

    fn command(line: &str) {
        if let Ok(mut ctl) = OpenOptions::new().write(true).open(CTL) {
            let _ = ctl.write_all(line.as_bytes());
        }
    }
}

/// A value from the status text.
pub fn status_field(status: &str, name: &str) -> Option<String> {
    status.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(' ')).map(str::to_owned)
}

impl Sink for Device {
    fn open(&mut self) -> io::Result<()> {
        if self.pcm.is_none() {
            self.pcm = Some(OpenOptions::new().write(true).custom_flags(O_NONBLOCK).open(PCM)?);
        }
        Ok(())
    }

    fn close(&mut self) {
        self.pcm = None;
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.pcm.as_mut() {
            Some(pcm) => pcm.write(bytes),
            None => Err(io::Error::from(io::ErrorKind::NotConnected)),
        }
    }

    fn flush(&mut self) {
        Device::command("flush");
    }

    /// `LUCINA_AUDIO_TEST_DB` can only lower it: a test on the laptop that reads the counters and need not be heard.
    fn set_volume(&mut self, db: i32) {
        let limit = std::env::var("LUCINA_AUDIO_TEST_DB").ok().and_then(|v| v.parse().ok()).unwrap_or(db);
        Device::command(&format!("volume {}", db.min(limit)));
    }

    fn played_frames(&mut self) -> u64 {
        fs::read_to_string(CTL)
            .ok()
            .and_then(|s| status_field(&s, "played_frames"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }
}

const MIC: &str = "/dev/lucina-audio-in";

/// The microphone: 16-bit mono at 48 kHz, and a read blocks until there is some.
pub trait Mic: Send {
    fn open(&mut self) -> io::Result<()>;
    /// Some samples, as bytes: whole samples, at most `buf.len()`. 0 only if it has gone away.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn close(&mut self);
}

#[derive(Default)]
pub struct Microphone {
    file: Option<File>,
}

impl Mic for Microphone {
    fn open(&mut self) -> io::Result<()> {
        self.file = Some(File::open(MIC)?);
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::io::Read;
        match self.file.as_mut() {
            Some(file) => file.read(buf),
            None => Err(io::Error::from(io::ErrorKind::NotConnected)),
        }
    }

    fn close(&mut self) {
        self.file = None;
    }
}
