//! The thread that plays: a file goes through the decoder and into the sound device, and it says how far it got.
//!
//! It never blocks on the device, so that a pause or a seek takes effect at once. The position comes from what the device has played,
//! not from what was written to it. A pause drops what is queued and lets the device go, so that the amplifier goes off; going on
//! means seeking back to where the sound stopped.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::device::Sink;
use crate::{DecodeError, Decoder};

const POLL: Duration = Duration::from_millis(15);
const REPORT_EVERY: Duration = Duration::from_millis(250);
const FRAME_BYTES: u64 = 4;
const FRAMES_PER_MS: u64 = 48;

/// Opens what to play. For a source that is not a local file; [`Cmd::Play`] is this for a path.
pub type Opener = Box<dyn FnOnce() -> Result<Decoder, DecodeError> + Send>;

pub enum Cmd {
    Play { path: PathBuf, start_ms: u64 },
    Open { open: Opener, start_ms: u64 },
    Pause,
    Resume,
    Seek(u64),
    Volume(i32),
    Stop,
}

impl std::fmt::Debug for Cmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cmd::Play { path, start_ms } => f.debug_struct("Play").field("path", path).field("start_ms", start_ms).finish(),
            Cmd::Open { start_ms, .. } => f.debug_struct("Open").field("start_ms", start_ms).finish(),
            Cmd::Pause => f.write_str("Pause"),
            Cmd::Resume => f.write_str("Resume"),
            Cmd::Seek(ms) => write!(f, "Seek({ms})"),
            Cmd::Volume(db) => write!(f, "Volume({db})"),
            Cmd::Stop => f.write_str("Stop"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Report {
    Position { ms: u64, duration_ms: Option<u64> },
    Paused { ms: u64 },
    Ended,
    Failed(String),
}

struct Track {
    decoder: Decoder,
    /// Decoded and not yet taken by the device.
    pending: Vec<u8>,
    at_end: bool,
    /// Where the current run started in the file, and in the device's count.
    start_ms: u64,
    base_played: u64,
    written_frames: u64,
}

pub struct Engine<S: Sink, F: Fn(Report)> {
    sink: S,
    report: F,
    track: Option<Track>,
    paused: bool,
    volume: i32,
    open: bool,
    last_report: Instant,
    report_every: Duration,
    skipped_reported: u64,
}

impl<S: Sink, F: Fn(Report)> Engine<S, F> {
    pub fn new(sink: S, volume: i32, report: F) -> Self {
        Engine {
            sink,
            report,
            track: None,
            paused: false,
            volume,
            open: false,
            last_report: Instant::now(),
            report_every: REPORT_EVERY,
            skipped_reported: 0,
        }
    }

    /// How often [`Report::Position`] comes. A picture following the sound wants it more often than a progress bar.
    pub fn reporting_every(mut self, every: Duration) -> Self {
        self.report_every = every;
        self
    }

    pub fn run(mut self, commands: Receiver<Cmd>) {
        loop {
            match commands.recv_timeout(if self.busy() { POLL } else { Duration::from_secs(3600) }) {
                Err(RecvTimeoutError::Disconnected) => {
                    self.stop();
                    return;
                }
                Ok(cmd) => self.command(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.pump();
        }
    }

    fn busy(&self) -> bool {
        self.track.is_some() && !self.paused
    }

    fn take_device(&mut self) -> bool {
        if self.open {
            return true;
        }
        match self.sink.open() {
            Ok(()) => {
                self.open = true;
                self.sink.set_volume(self.volume);
                true
            }
            Err(err) => {
                (self.report)(Report::Failed(format!("the sound device: {err}")));
                self.track = None;
                false
            }
        }
    }

    fn release_device(&mut self, flush: bool) {
        if self.open {
            if flush {
                self.sink.flush();
            }
            self.sink.close();
            self.open = false;
        }
    }

    fn stop(&mut self) {
        self.release_device(true);
        self.track = None;
    }

    pub fn command(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Play { path, start_ms } => self.command(Cmd::Open { open: Box::new(move || Decoder::open(&path)), start_ms }),
            Cmd::Open { open, start_ms } => {
                match open() {
                    Ok(mut decoder) => {
                        let start_ms = if start_ms > 0 { decoder.seek_ms(start_ms).unwrap_or(0) } else { 0 };
                        if self.open {
                            self.sink.flush();
                        }
                        self.paused = false;
                        self.skipped_reported = 0;
                        if !self.take_device() {
                            return;
                        }
                        let base_played = self.sink.played_frames();
                        self.track = Some(Track {
                            decoder,
                            pending: Vec::new(),
                            at_end: false,
                            start_ms,
                            base_played,
                            written_frames: 0,
                        });
                    }
                    Err(err) => (self.report)(Report::Failed(err.to_string())),
                }
            }
            Cmd::Pause => {
                if self.track.is_some() && !self.paused {
                    let ms = self.position();
                    self.release_device(true);
                    if let Some(track) = self.track.as_mut() {
                        track.start_ms = ms;
                    }
                    self.paused = true;
                    (self.report)(Report::Paused { ms });
                }
            }
            Cmd::Resume => {
                if self.track.is_some() && self.paused {
                    self.paused = false;
                    let ms = self.track.as_ref().map_or(0, |t| t.start_ms);
                    if self.take_device() {
                        self.seek(ms);
                    }
                }
            }
            Cmd::Seek(ms) => {
                if self.paused {
                    if let Some(track) = self.track.as_mut() {
                        track.start_ms = ms;
                    }
                } else {
                    self.sink.flush();
                    self.seek(ms);
                }
            }
            Cmd::Volume(db) => {
                self.volume = db;
                if self.open {
                    self.sink.set_volume(db);
                }
            }
            Cmd::Stop => self.stop(),
        }
    }

    fn seek(&mut self, ms: u64) {
        let played = self.sink.played_frames();
        let Some(track) = self.track.as_mut() else {
            return;
        };
        match track.decoder.seek_ms(ms) {
            Ok(landed) => {
                track.start_ms = landed;
                track.pending.clear();
                track.at_end = false;
                track.base_played = played;
                track.written_frames = 0;
            }
            Err(err) => (self.report)(Report::Failed(err.to_string())),
        }
    }

    fn played_in_run(&mut self) -> u64 {
        let played = self.sink.played_frames();
        let Some(track) = self.track.as_mut() else {
            return 0;
        };
        // A new stream started and the device's count began again.
        if played < track.base_played {
            track.base_played = 0;
        }
        played - track.base_played
    }

    fn position(&mut self) -> u64 {
        let frames = self.played_in_run();
        self.track.as_ref().map_or(0, |t| t.start_ms + frames / FRAMES_PER_MS)
    }

    /// Everything that can be done now without waiting.
    pub fn pump(&mut self) {
        if self.track.is_none() || self.paused {
            return;
        }
        let failed = loop {
            let Some(track) = self.track.as_mut() else {
                return;
            };
            if track.pending.is_empty() {
                if track.at_end {
                    break None;
                }
                match track.decoder.next_block(&mut track.pending) {
                    Ok(true) => {}
                    Ok(false) => {
                        track.at_end = true;
                        continue;
                    }
                    Err(err) => break Some(err.to_string()),
                }
            }
            match self.sink.write(&track.pending) {
                Ok(0) => break None,
                Ok(n) => {
                    track.written_frames += n as u64 / FRAME_BYTES;
                    track.pending.drain(..n);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break None,
                Err(err) => break Some(format!("the sound device: {err}")),
            }
        };
        if let Some(why) = failed {
            (self.report)(Report::Failed(why));
            self.stop();
            return;
        }
        let frames = self.played_in_run();
        let Some(track) = self.track.as_ref() else {
            return;
        };
        if track.at_end && track.pending.is_empty() && frames >= track.written_frames {
            self.track = None;
            (self.report)(Report::Ended);
            return;
        }
        if frames > 0 && self.last_report.elapsed() >= self.report_every {
            self.last_report = Instant::now();
            let ms = track.start_ms + frames / FRAMES_PER_MS;
            let duration_ms = track.decoder.duration_ms();
            (self.report)(Report::Position { ms, duration_ms });
        }
    }
}
