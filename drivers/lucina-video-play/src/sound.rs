//! A video's sound, through the same engine as the music player: it decodes on a thread of its own, owns the device while it plays, and
//! says where the speakers have got to. The file is opened a second time for this rather than sharing the picture's demuxer: symphonia
//! reads the containers for itself, and a seek in one never waits for the other.
//!
//! A file with no sound, or a sound this cannot decode, or no sound device: the reason goes to the log and the wall clock takes over.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::thread::JoinHandle;
use std::time::Duration;

use lucina_audio_decode::device::Device;
use lucina_audio_decode::engine::{Cmd, Engine, Report};
use lucina_audio_decode::{DecodeError, Decoder};
use lucina_video::AudioOutput;

/// Nearer than the music player's, because a picture follows it.
const REPORT_EVERY: Duration = Duration::from_millis(20);
/// As the music player starts. The kernel's ceiling is the limit on it.
const VOLUME_DB: i32 = -24;
const NOT_PLAYING: i64 = -1;

/// Opens the sound from the start of the file. Called once for each start and seek.
pub type Open = Arc<dyn Fn() -> Result<Decoder, DecodeError> + Send + Sync>;

struct Running {
    commands: Sender<Cmd>,
    thread: JoinHandle<()>,
}

pub struct Sound {
    open: Option<Open>,
    running: Option<Running>,
    /// Milliseconds at the speakers, or [`NOT_PLAYING`].
    position_ms: Arc<AtomicI64>,
    paused: bool,
    /// Where to begin once a pause ends: a seek or a start while paused.
    start_after_pause: Option<u64>,
}

impl Sound {
    pub fn none() -> Sound {
        Sound { open: None, running: None, position_ms: Arc::new(AtomicI64::new(NOT_PLAYING)), paused: false, start_after_pause: None }
    }

    pub fn from(open: Open) -> Sound {
        let mut sound = Sound::none();
        sound.open = Some(open);
        sound
    }

    /// Played frames per the device, not what was written to it: a program that fell behind hears the truth.
    fn commands(&mut self) -> Option<&Sender<Cmd>> {
        if self.running.is_none() {
            self.open.as_ref()?;
            let (commands, received) = channel();
            let position = self.position_ms.clone();
            let thread = std::thread::Builder::new()
                .name(String::from("video-sound"))
                .spawn(move || {
                    let report = move |r: Report| match r {
                        Report::Position { ms, .. } => position.store(ms as i64, Ordering::Relaxed),
                        Report::Paused { .. } => {}
                        Report::Ended => position.store(NOT_PLAYING, Ordering::Relaxed),
                        Report::Failed(why) => {
                            eprintln!("lucina-app-video: no sound: {why}");
                            position.store(NOT_PLAYING, Ordering::Relaxed);
                        }
                    };
                    Engine::new(Device::new(), VOLUME_DB, report).reporting_every(REPORT_EVERY).run(received);
                })
                .ok()?;
            self.running = Some(Running { commands, thread });
        }
        self.running.as_ref().map(|r| &r.commands)
    }

    fn send(&mut self, cmd: Cmd) {
        if let Some(commands) = self.commands() {
            let _ = commands.send(cmd);
        }
    }

    fn play_from(&mut self, ms: u64) {
        let Some(open) = self.open.clone() else {
            return;
        };
        self.position_ms.store(NOT_PLAYING, Ordering::Relaxed);
        self.send(Cmd::Open { open: Box::new(move || open()), start_ms: ms });
    }
}

impl Drop for Sound {
    /// Waits for the device to be let go, so that the next file's sound can have it.
    fn drop(&mut self) {
        if let Some(running) = self.running.take() {
            drop(running.commands);
            let _ = running.thread.join();
        }
    }
}

impl AudioOutput for Sound {
    fn position_us(&self) -> Option<i64> {
        let ms = self.position_ms.load(Ordering::Relaxed);
        (ms != NOT_PLAYING).then_some(ms * 1000)
    }

    fn start(&mut self, pts_us: i64) {
        let ms = pts_us.max(0) as u64 / 1000;
        if self.paused {
            self.start_after_pause = Some(ms);
        } else {
            self.play_from(ms);
        }
    }

    fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if paused {
            self.send(Cmd::Pause);
        } else if let Some(ms) = self.start_after_pause.take() {
            self.play_from(ms);
        } else {
            self.send(Cmd::Resume);
        }
    }

    fn seek(&mut self, _pts_us: i64) {
        self.position_ms.store(NOT_PLAYING, Ordering::Relaxed);
        self.start_after_pause = None;
        self.send(Cmd::Stop);
    }
}
