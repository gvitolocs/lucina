//! Voice in the chat: a voice note (the mic control) and a voice command (the talk control), streamed up Secretary's voice socket
//! while the owner talks (`lucina-voice-stream`, LucinaVoice protocol section 10). The note becomes a chat message the timeline
//! stream shows; a command's reply grows a pending Lucina bubble as she writes it and is spoken through the speakers.
//!
//! The screen keeps [`VoiceUi`], which the events from the voice thread change; it draws the owner's transcript and Lucina's
//! pending reply as two rows after the real ones until the timeline has the real items, and a status line above the composer.
//! Texts are never logged here.
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_account::Client;
use lucina_audio_decode::device::{Device, Mic, Microphone, Sink};
use lucina_voice_stream::link::LinkConfig;
use lucina_voice_stream::msg::{Purpose, Start};
use lucina_voice_stream::rules;
use lucina_voice_stream::session::Event;
use lucina_voice_stream::voice::{PcmSink, PcmSource, PressError, Voice, VoiceConfig};

/// "VA_DEC Volume" for speech, in dB: the Recorder's setting (juno-audio.md section 31), +30 dB over the MEMS microphone's level.
const SPEECH_GAIN_DB: i32 = 30;
/// The speakers' volume for Lucina's voice, in dB of the device's range (the Recorder plays back at -18).
const SPEECH_VOLUME_DB: i32 = -18;
/// How long the transient rows stay after the turn if the timeline never shows the real ones.
const LINGER: Duration = Duration::from_secs(10);

/// `/dev/lucina-audio-in` with the speech gain set first, as the Recorder does (best effort: an older kernel has no control).
struct MicSource(Microphone);

impl PcmSource for MicSource {
    fn open(&mut self) -> io::Result<()> {
        use std::io::Write;
        if let Ok(mut ctl) = std::fs::OpenOptions::new().write(true).open("/dev/lucina-audio-ctl") {
            let _ = ctl.write_all(format!("mic-gain {SPEECH_GAIN_DB}\n").as_bytes());
        }
        self.0.open()
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
    fn close(&mut self) {
        self.0.close();
    }
}

/// `/dev/lucina-audio`: 48 kHz stereo, written without blocking.
struct Speaker(Device);

impl PcmSink for Speaker {
    fn open(&mut self) -> io::Result<()> {
        self.0.open()
    }
    fn close(&mut self) {
        self.0.close();
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) {
        self.0.flush();
    }
    fn set_volume(&mut self, db: i32) {
        self.0.set_volume(db);
    }
}

/// The voice thread's events, waiting for the screen. The callback runs under the controller's lock: it only queues and wakes.
type Queue = Arc<Mutex<VecDeque<Event>>>;

pub struct VoiceUi {
    voice: Voice,
    queue: Queue,
    down: Option<String>,
    /// What the open or last turn was.
    kind: Option<Purpose>,
    transcript: Option<String>,
    /// Lucina's command reply, growing.
    reply: String,
    /// The last turn ended at this time (its transient rows linger a little).
    settled: Option<Instant>,
    /// The line above the composer.
    pub line: Option<String>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl VoiceUi {
    /// Opens the socket in the background (the chat window is showing) and keeps it.
    pub fn start(client: &Client, wake: Arc<dyn Fn() + Send + Sync>) -> Option<VoiceUi> {
        let token = client.bearer()?.expose().to_owned();
        let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
        let (q, w) = (queue.clone(), wake.clone());
        let voice = Voice::start(VoiceConfig {
            link: LinkConfig { url: client.ws_url("/v1/voice/ws"), bearer: Some(token) },
            source: Box::new(|| Box::new(MicSource(Microphone::default()))),
            sink: Box::new(Speaker(Device::new())),
            volume_db: SPEECH_VOLUME_DB,
            on_event: Arc::new(move |e| {
                if let Ok(mut q) = q.lock() {
                    q.push_back(e);
                }
                w();
            }),
            log_timings: true,
        });
        Some(VoiceUi { voice, queue, down: None, kind: None, transcript: None, reply: String::new(), settled: None, line: None, wake })
    }

    /// Whether the owner is recording (the control then says Stop).
    pub fn recording(&self) -> Option<Purpose> {
        match self.voice.phase() {
            rules::STARTING | rules::STREAMING => self.kind,
            _ => None,
        }
    }

    /// The server has the audio and is working on it.
    pub fn working(&self) -> bool {
        self.voice.phase() == rules::WAITING
    }

    /// A press of the mic (note) or talk (command) control, or its shortcut: starts, or ends what is recording.
    pub fn toggle(&mut self, purpose: Purpose) {
        match self.recording() {
            Some(p) if p == purpose => {
                self.voice.end();
                self.line = Some("Sending…".into());
            }
            Some(_) => self.line = Some("Finish the current voice message first.".into()),
            None if self.working() => self.line = Some("Lucina is still working on the last one.".into()),
            None => {
                let start = if purpose == Purpose::Note { Start::note() } else { Start::command() };
                match self.voice.press(start) {
                    Ok(()) => {
                        self.kind = Some(purpose);
                        self.transcript = None;
                        self.reply.clear();
                        self.settled = None;
                        self.line = Some("Listening… press again to send, Esc cancels.".into());
                    }
                    Err(PressError::NotConnected) => {
                        self.voice.retry_now();
                        self.line = Some(match &self.down {
                            Some(why) => format!("Voice is offline ({why}). Trying again."),
                            None => "Connecting to Lucina's voice…".into(),
                        });
                    }
                    Err(PressError::Busy) => self.line = Some("One moment: the microphone is still closing.".into()),
                }
            }
        }
    }

    /// Esc: drops what is being recorded or worked on.
    pub fn cancel(&mut self) -> bool {
        if self.recording().is_none() && !self.working() {
            return false;
        }
        self.voice.cancel();
        self.transcript = None;
        self.reply.clear();
        self.line = Some("Cancelled.".into());
        true
    }

    /// Takes the events the voice thread left. Returns whether the screen has something new to draw.
    pub fn pump(&mut self) -> bool {
        let events: Vec<Event> = match self.queue.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return false,
        };
        let changed = !events.is_empty();
        for e in events {
            self.apply(e);
        }
        if let Some(t) = self.settled {
            if t.elapsed() > LINGER && (self.transcript.is_some() || !self.reply.is_empty()) {
                self.transcript = None;
                self.reply.clear();
                return true;
            }
        }
        changed
    }

    /// Queues events of a command turn as if the voice thread had sent them (tests).
    #[cfg(test)]
    pub fn inject(&mut self, events: Vec<Event>) {
        self.kind = Some(Purpose::Command);
        self.queue.lock().unwrap().extend(events);
    }

    fn settle(&mut self) {
        self.settled = Some(Instant::now());
        // The rows go when the timeline shows the real ones, or after a while: wake the screen to check.
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            std::thread::sleep(LINGER + Duration::from_millis(200));
            wake();
        });
    }

    fn apply(&mut self, e: Event) {
        match e {
            Event::Connected => self.down = None,
            Event::Disconnected(why) => self.down = Some(why),
            Event::Listening => self.line = Some("Listening… press again to send, Esc cancels.".into()),
            Event::Endpoint => self.line = Some("Thinking…".into()),
            Event::Transcript(t) => {
                self.transcript = Some(t);
                if self.kind == Some(Purpose::Command) {
                    self.line = Some("Lucina is answering…".into());
                }
            }
            Event::Delta(t) => self.reply.push_str(&t),
            Event::Answer { text, replaces } => {
                if replaces || self.reply.is_empty() {
                    self.reply = text;
                }
            }
            Event::Reply(t) => {
                if !t.is_empty() {
                    self.reply = t;
                }
            }
            Event::Done => {
                self.line = None;
                self.settle();
            }
            Event::NoteStored { .. } => {
                self.line = None;
                self.settle();
            }
            Event::NoteReply { .. } | Event::NoteDone { .. } | Event::NoteDropped { .. } => {}
            Event::NoteFailed { message, .. } => {
                self.line = Some(if message.is_empty() { "The voice note was not answered.".into() } else { message });
            }
            Event::Failed { code, message } => {
                self.transcript = None;
                self.reply.clear();
                self.line = Some(match code.as_str() {
                    "mic" | "closed" => message,
                    "note_failed" => "The voice note was not sent.".into(),
                    _ if message.is_empty() => "That did not work.".into(),
                    _ => message,
                });
            }
            Event::OldServer => {
                self.transcript = None;
                self.line = Some("This server cannot take voice notes yet.".into());
            }
            Event::Timing { .. } => {}
        }
    }

    /// The owner's words and Lucina's reply as far as they are known, to draw after the timeline's rows. Each stays only until
    /// the timeline has a row with the same text, so nothing shows twice.
    pub fn transient(&self, real_user: &[&str], real_lucina: &[&str]) -> (Option<String>, Option<String>) {
        let user = self.transcript.as_ref().filter(|t| !t.trim().is_empty() && !real_user.iter().any(|r| r.trim() == t.trim())).cloned();
        let lucina = if self.kind == Some(Purpose::Command) && !self.reply.is_empty() && !real_lucina.iter().any(|r| r.trim() == self.reply.trim()) {
            Some(self.reply.clone())
        } else {
            None
        };
        (user, lucina)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> VoiceUi {
        // A server nobody listens on: the socket never comes up, which is all these tests need.
        let client = Client::new("http://127.0.0.1:9", lucina_account::Token::parse(lucina_account::fake::TOKEN)).unwrap();
        VoiceUi::start(&client, Arc::new(|| {})).expect("signed in")
    }

    fn feed(v: &mut VoiceUi, events: Vec<Event>) {
        v.queue.lock().unwrap().extend(events);
        v.pump();
    }

    #[test]
    fn a_command_grows_a_pending_reply_that_goes_when_the_timeline_has_it() {
        let mut v = ui();
        v.kind = Some(Purpose::Command);
        feed(&mut v, vec![Event::Transcript("che ore sono".into()), Event::Delta("Sono ".into()), Event::Delta("le tre".into())]);
        assert_eq!(v.transient(&[], &[]), (Some("che ore sono".into()), Some("Sono le tre".into())));
        feed(&mut v, vec![Event::Answer { text: "Sono le tre.".into(), replaces: true }, Event::Reply("Sono le tre.".into()), Event::Done]);
        assert_eq!(v.transient(&[], &[]).1.as_deref(), Some("Sono le tre."));
        assert!(v.line.is_none());
        // The real rows arrive: nothing shows twice.
        assert_eq!(v.transient(&["che ore sono"], &["Sono le tre."]), (None, None));
    }

    #[test]
    fn a_note_shows_its_transcript_until_the_real_message_arrives() {
        let mut v = ui();
        v.kind = Some(Purpose::Note);
        feed(&mut v, vec![Event::Transcript("ricordami il latte".into()), Event::NoteStored { share_id: "s".into(), user_item_id: None }]);
        assert_eq!(v.transient(&[], &[]), (Some("ricordami il latte".into()), None), "a note has no pending Lucina bubble");
        assert_eq!(v.transient(&["ricordami il latte"], &[]), (None, None));
    }

    #[test]
    fn failures_say_so_and_drop_the_pending_rows() {
        let mut v = ui();
        v.kind = Some(Purpose::Command);
        feed(&mut v, vec![Event::Transcript("x".into()), Event::Delta("y".into())]);
        feed(&mut v, vec![Event::Failed { code: "lucina_unavailable".into(), message: "Lucina is offline.".into() }]);
        assert_eq!(v.transient(&[], &[]), (None, None));
        assert_eq!(v.line.as_deref(), Some("Lucina is offline."));
        feed(&mut v, vec![Event::OldServer]);
        assert!(v.line.as_deref().unwrap().contains("cannot take voice notes"));
    }

    #[test]
    fn offline_the_controls_say_why_and_do_not_record() {
        let mut v = ui();
        feed(&mut v, vec![Event::Disconnected("signed out".into())]);
        v.toggle(Purpose::Note);
        assert!(v.line.as_deref().unwrap().contains("signed out"));
        assert!(v.recording().is_none());
        assert!(!v.cancel(), "nothing to cancel");
    }
}
