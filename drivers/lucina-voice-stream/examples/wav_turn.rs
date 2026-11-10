//! The host check: the same client code the Juno chat runs, on Linux, with a WAV file for the microphone and a WAV file for the
//! speakers.
//!
//! ```text
//! cargo run -p lucina-voice-stream --features net --example wav_turn -- \
//!     --url wss://getsecretary.tech/v1/voice/ws --token-file ~/.secretary-token \
//!     --purpose note --in speech.wav --out reply.wav
//! ```
//!
//! `--in` is a 16-bit mono PCM WAV (any rate; it is resampled to the microphone's 48 kHz). `--out` receives the reply's speech as a
//! 48 kHz stereo WAV. `--purpose` is `note` or `command`. `--pace` feeds the file at real time, as a microphone would. The token is
//! read from the file named by `--token-file` (one line) and goes only in the upgrade request. Events are printed one per line as
//! their kind, never their text; add `--show-text` to see the texts too.
use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use lucina_voice_stream::link::LinkConfig;
use lucina_voice_stream::msg::Start;
use lucina_voice_stream::session::Event;
use lucina_voice_stream::voice::{PcmSink, PcmSource, Voice, VoiceConfig};

struct Args {
    url: String,
    token_file: String,
    input: String,
    out: String,
    note: bool,
    pace: bool,
    show_text: bool,
}

fn args() -> Result<Args, String> {
    let mut a = Args { url: String::new(), token_file: String::new(), input: String::new(), out: String::new(), note: true, pace: false, show_text: false };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut v = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match k.as_str() {
            "--url" => a.url = v("--url")?,
            "--token-file" => a.token_file = v("--token-file")?,
            "--in" => a.input = v("--in")?,
            "--out" => a.out = v("--out")?,
            "--purpose" => {
                a.note = match v("--purpose")?.as_str() {
                    "note" => true,
                    "command" => false,
                    other => return Err(format!("--purpose is note or command, not {other}")),
                }
            }
            "--pace" => a.pace = true,
            "--show-text" => a.show_text = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if a.url.is_empty() || a.input.is_empty() || a.out.is_empty() {
        return Err("usage: wav_turn --url <ws[s]://...> [--token-file F] --in in.wav --out out.wav [--purpose note|command] [--pace] [--show-text]".into());
    }
    Ok(a)
}

/// The data of a 16-bit mono PCM WAV, and its rate.
fn read_wav(path: &str) -> io::Result<(Vec<i16>, u32)> {
    let b = std::fs::read(path)?;
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_owned());
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("not a WAV"));
    }
    let (mut at, mut rate, mut ok) = (12, 0u32, false);
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            let tag = u16::from_le_bytes([b[body], b[body + 1]]);
            let ch = u16::from_le_bytes([b[body + 2], b[body + 3]]);
            rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
            let bits = u16::from_le_bytes([b[body + 14], b[body + 15]]);
            ok = tag == 1 && ch == 1 && bits == 16;
        } else if id == b"data" {
            if !ok {
                return Err(bad("need 16-bit mono PCM"));
            }
            let end = (body + len).min(b.len());
            let s = b[body..end].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            return Ok((s, rate));
        }
        at = body + len + (len & 1);
    }
    Err(bad("no data chunk"))
}

/// Nearest-neighbour to 48 kHz: this is only a test signal, and the decimator's own filter is what is being exercised.
fn to_48k(s: &[i16], rate: u32) -> Vec<i16> {
    let n = (s.len() as u64 * 48_000 / rate.max(1) as u64) as usize;
    (0..n).map(|i| s[(i as u64 * rate as u64 / 48_000) as usize]).collect()
}

fn write_wav(path: &str, pcm: &[u8]) -> io::Result<()> {
    let mut o = Vec::with_capacity(44 + pcm.len());
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&2u16.to_le_bytes());
    o.extend_from_slice(&48_000u32.to_le_bytes());
    o.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
    o.extend_from_slice(&4u16.to_le_bytes());
    o.extend_from_slice(&16u16.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    o.extend_from_slice(pcm);
    std::fs::write(path, o)
}

struct WavSource {
    pcm: Vec<u8>,
    at: usize,
    pace: bool,
    started: Option<Instant>,
}

impl PcmSource for WavSource {
    fn open(&mut self) -> io::Result<()> {
        self.at = 0;
        self.started = Some(Instant::now());
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.pcm.len() - self.at) & !1;
        if self.pace {
            // The bytes up to `at + n` exist only after that much real time.
            let due = Duration::from_micros((self.at + n) as u64 * 1_000_000 / 96_000);
            if let Some(t) = self.started {
                if let Some(wait) = due.checked_sub(t.elapsed()) {
                    std::thread::sleep(wait);
                }
            }
        }
        buf[..n].copy_from_slice(&self.pcm[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }

    fn close(&mut self) {}
}

struct WavSink(Arc<Mutex<Vec<u8>>>);

impl PcmSink for WavSink {
    fn open(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn close(&mut self) {}
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) {}
    fn set_volume(&mut self, _db: i32) {}
}

fn main() {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let (samples, rate) = read_wav(&a.input).unwrap_or_else(|e| {
        eprintln!("{}: {e}", a.input);
        std::process::exit(2);
    });
    let pcm: Vec<u8> = to_48k(&samples, rate).iter().flat_map(|s| s.to_le_bytes()).collect();
    let bearer = if a.token_file.is_empty() {
        None
    } else {
        Some(std::fs::read_to_string(&a.token_file).unwrap_or_else(|e| {
            eprintln!("{}: {e}", a.token_file);
            std::process::exit(2);
        }).trim().to_owned())
    };
    let (tx, rx) = mpsc::channel::<Event>();
    let tx = Mutex::new(tx);
    let heard = Arc::new(Mutex::new(Vec::new()));
    let (pace, show_text) = (a.pace, a.show_text);
    let voice = Voice::start(VoiceConfig {
        link: LinkConfig { url: a.url.clone(), bearer },
        source: Box::new(move || Box::new(WavSource { pcm: pcm.clone(), at: 0, pace, started: None })),
        sink: Box::new(WavSink(heard.clone())),
        volume_db: 0,
        on_event: Arc::new(move |e| {
            let _ = tx.lock().unwrap().send(e);
        }),
        log_timings: true,
    });
    let begin = Instant::now();
    let mut pressed = false;
    let mut note_open: Option<String> = None;
    let code = loop {
        if begin.elapsed() > Duration::from_secs(300) {
            eprintln!("timed out");
            break 1;
        }
        if !pressed && voice.connected() {
            let start = if a.note { Start::note() } else { Start::command() };
            if let Err(e) = voice.press(start) {
                eprintln!("press: {e:?}");
                break 1;
            }
            pressed = true;
        }
        let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) else { continue };
        let text = |t: &str| if show_text { format!(" {t:?}") } else { String::new() };
        match ev {
            Event::Connected => println!("connected"),
            Event::Disconnected(why) => println!("disconnected: {why}"),
            Event::Listening => println!("listening"),
            Event::Endpoint => println!("endpoint"),
            Event::Transcript(t) => println!("transcript{}", text(&t)),
            Event::Delta(t) => println!("delta{}", text(&t)),
            Event::Answer { text: t, replaces } => println!("answer replaces={replaces}{}", text(&t)),
            Event::Reply(t) => println!("reply{}", text(&t)),
            Event::Done => {
                println!("done");
                break 0;
            }
            Event::NoteStored { share_id, .. } => {
                println!("note stored");
                note_open = Some(share_id);
            }
            Event::NoteReply { text: t, .. } => println!("note reply{}", text(&t)),
            Event::NoteDone { share_id } => {
                println!("note done");
                if note_open.as_deref() == Some(share_id.as_str()) {
                    break 0;
                }
            }
            Event::NoteDropped { .. } => println!("note dropped"),
            Event::NoteFailed { code, .. } => {
                println!("note failed: {code}");
                break 1;
            }
            Event::Failed { code, .. } => {
                println!("failed: {code}");
                break 1;
            }
            Event::OldServer => {
                println!("old server: no voice notes");
                break 1;
            }
            Event::Timing { .. } => {}
        }
    };
    // The player thread writes the speech a little after the events: wait until it has been quiet for 300 ms.
    let (mut last, mut quiet) = (usize::MAX, Instant::now());
    while quiet.elapsed() < Duration::from_millis(300) && begin.elapsed() < Duration::from_secs(300) {
        std::thread::sleep(Duration::from_millis(20));
        let n = heard.lock().unwrap().len();
        if n != last {
            last = n;
            quiet = Instant::now();
        }
    }
    drop(voice);
    let out = heard.lock().unwrap().clone();
    if !out.is_empty() {
        if let Err(e) = write_wav(&a.out, &out) {
            eprintln!("{}: {e}", a.out);
        }
    }
    std::process::exit(code);
}
