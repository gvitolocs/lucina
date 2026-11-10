//! The voice client against an in-process stand-in for `/v1/voice/ws`: a note turn, a command turn, a cancel, an old server, a
//! ping, and a connection that is refused or dropped. The server follows LucinaVoice protocol sections 2 and 10 and checks what
//! the client sends.
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lucina_voice_stream::link::LinkConfig;
use lucina_voice_stream::msg::Start;
use lucina_voice_stream::rules;
use lucina_voice_stream::session::Event;
use lucina_voice_stream::voice::{PcmSink, PcmSource, PressError, Voice, VoiceConfig};
use serde_json::{json, Value};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;
use tungstenite::{accept_hdr, Message};

const TOKEN: &str = "sec_testtesttesttesttesttesttesttesttesttesttes";

#[derive(Default, Clone)]
struct Behaviour {
    old_server: bool,
    drop_first: bool,
    refuse_401: bool,
    ping_first: bool,
}

#[derive(Default)]
struct Seen {
    connections: usize,
    auth: Vec<Option<String>>,
    client_header: bool,
    texts: Vec<Value>,
    bytes_before_end: usize,
    bytes_after_end: usize,
    pongs: usize,
}

struct Mock {
    port: u16,
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Mock {
    fn start(b: Behaviour) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (s2, stop2) = (seen.clone(), stop.clone());
        let handle = thread::spawn(move || {
            let mut n = 0;
            while !stop2.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((conn, _)) => {
                        n += 1;
                        s2.lock().unwrap().connections = n;
                        if b.drop_first && n == 1 {
                            drop(conn);
                            continue;
                        }
                        let (s3, stop3, b3) = (s2.clone(), stop2.clone(), b.clone());
                        thread::spawn(move || serve(conn, s3, stop3, b3));
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(10)),
                    Err(_) => break,
                }
            }
        });
        Mock { port, seen, stop, handle: Some(handle) }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}/v1/voice/ws", self.port)
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn send(ws: &mut tungstenite::WebSocket<TcpStream>, v: Value) {
    let _ = ws.send(Message::text(v.to_string()));
}

fn serve(conn: TcpStream, seen: Arc<Mutex<Seen>>, stop: Arc<AtomicBool>, b: Behaviour) {
    let _ = conn.set_nonblocking(false);
    let seen2 = seen.clone();
    let refuse = b.refuse_401;
    let callback = move |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let mut s = seen2.lock().unwrap();
        s.auth.push(req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_owned));
        s.client_header |= req.headers().contains_key("x-secretary-client");
        if refuse {
            let mut r = ErrorResponse::new(Some("no".into()));
            *r.status_mut() = StatusCode::UNAUTHORIZED;
            return Err(r);
        }
        Ok(resp)
    };
    let Ok(mut ws) = accept_hdr(conn, callback) else { return };
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(50)));
    let mut purpose = String::from("command");
    let mut ended = false;
    let mut pinged = false;
    while !stop.load(Ordering::Acquire) {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let v: Value = serde_json::from_str(t.as_str()).unwrap();
                seen.lock().unwrap().texts.push(v.clone());
                match v["type"].as_str().unwrap_or("") {
                    "start" => {
                        purpose = v["purpose"].as_str().unwrap_or("command").to_owned();
                        ended = false;
                        if b.ping_first && !pinged {
                            pinged = true;
                            let _ = ws.send(Message::Ping(b"x".to_vec().into()));
                            thread::sleep(Duration::from_millis(100));
                        }
                        if b.old_server {
                            send(&mut ws, json!({"type":"listening"}));
                        } else {
                            send(&mut ws, json!({"type":"listening","purpose":purpose,"codec":"pcm16","rate":16000,"turn":"turn1"}));
                        }
                    }
                    "end" => {
                        ended = true;
                        if purpose == "note" {
                            send(&mut ws, json!({"type":"transcript","text":"ricordami il latte"}));
                            send(&mut ws, json!({"type":"note","shareId":"sh1","userItemId":"itm_u1"}));
                            thread::sleep(Duration::from_millis(60));
                            send(&mut ws, json!({"type":"reply","shareId":"sh1","text":"Fatto.","itemId":"itm_l1"}));
                            send(&mut ws, json!({"type":"done","shareId":"sh1","ms":{"stt":80,"note":300,"reply":900,"total":900}}));
                        } else {
                            send(&mut ws, json!({"type":"transcript","text":"che ore sono"}));
                            send(&mut ws, json!({"type":"delta","text":"Sono "}));
                            send(&mut ws, json!({"type":"delta","text":"le tre."}));
                            send(&mut ws, json!({"type":"answer","text":"Sono le tre.","replaces":false}));
                            send(&mut ws, json!({"type":"audio","rate":24000}));
                            for i in 0..3i16 {
                                let frame: Vec<u8> = (0..480i16).flat_map(|k| (k * 10 + i).to_le_bytes()).collect();
                                let _ = ws.send(Message::binary(frame));
                            }
                            send(&mut ws, json!({"type":"reply","text":"Sono le tre."}));
                            send(&mut ws, json!({"type":"done","ms":{"total":700}}));
                        }
                    }
                    _ => {}
                }
            }
            Ok(Message::Binary(bytes)) => {
                let mut s = seen.lock().unwrap();
                if ended {
                    s.bytes_after_end += bytes.len();
                } else {
                    s.bytes_before_end += bytes.len();
                }
            }
            Ok(Message::Pong(_)) => seen.lock().unwrap().pongs += 1,
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
}

/// One second of 440 Hz at 48 kHz, then the end of the source.
struct Tone {
    left: usize,
    t: usize,
}

impl PcmSource for Tone {
    fn open(&mut self) -> io::Result<()> {
        self.left = 48_000;
        self.t = 0;
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() / 2).min(self.left);
        for i in 0..n {
            let s = (8000.0 * (2.0 * std::f64::consts::PI * 440.0 * (self.t + i) as f64 / 48000.0).sin()) as i16;
            buf[2 * i..2 * i + 2].copy_from_slice(&s.to_le_bytes());
        }
        self.t += n;
        self.left -= n;
        Ok(n * 2)
    }
    fn close(&mut self) {}
}

/// A source that never ends by itself (until closed): the owner talks on.
struct Endless;

impl PcmSource for Endless {
    fn open(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_millis(40));
        buf.fill(0);
        Ok(buf.len())
    }
    fn close(&mut self) {}
}

#[derive(Default)]
struct Speaker {
    bytes: Vec<u8>,
    opened: u32,
    closed: u32,
    volume: Option<i32>,
}

struct Capture(Arc<Mutex<Speaker>>);

impl PcmSink for Capture {
    fn open(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().opened += 1;
        Ok(())
    }
    fn close(&mut self) {
        self.0.lock().unwrap().closed += 1;
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Takes at most 1000 bytes a call, to see the retry loop.
        let n = bytes.len().min(1000);
        self.0.lock().unwrap().bytes.extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) {}
    fn set_volume(&mut self, db: i32) {
        self.0.lock().unwrap().volume = Some(db);
    }
}

struct Rig {
    voice: Voice,
    events: Arc<Mutex<Vec<Event>>>,
    speaker: Arc<Mutex<Speaker>>,
}

fn rig(url: String, endless: bool) -> Rig {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e2 = events.clone();
    let speaker = Arc::new(Mutex::new(Speaker::default()));
    let voice = Voice::start(VoiceConfig {
        link: LinkConfig { url, bearer: Some(TOKEN.into()) },
        source: Box::new(move || if endless { Box::new(Endless) as Box<dyn PcmSource> } else { Box::new(Tone { left: 0, t: 0 }) }),
        sink: Box::new(Capture(speaker.clone())),
        volume_db: -18,
        on_event: Arc::new(move |e| e2.lock().unwrap().push(e)),
        log_timings: false,
    });
    Rig { voice, events, speaker }
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if ok() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

impl Rig {
    fn has(&self, f: impl Fn(&Event) -> bool) -> bool {
        self.events.lock().unwrap().iter().any(f)
    }
    fn wait_event(&self, what: &str, f: impl Fn(&Event) -> bool) {
        wait_until(what, || self.has(&f));
    }
    fn names(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| !matches!(e, Event::Timing { .. }))
            .map(|e| format!("{e:?}").split(['(', ' ', '{']).next().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn a_voice_note_streams_up_and_is_answered_in_the_background() {
    let mock = Mock::start(Behaviour { ping_first: true, ..Default::default() });
    let r = rig(mock.url(), false);
    r.wait_event("connected", |e| *e == Event::Connected);
    assert!(r.voice.connected());
    r.voice.press(Start::note()).unwrap();
    assert_eq!(r.voice.press(Start::command()), Err(PressError::Busy), "one turn at a time");
    r.wait_event("note stored", |e| matches!(e, Event::NoteStored { share_id, user_item_id } if share_id == "sh1" && user_item_id.as_deref() == Some("itm_u1")));
    assert_eq!(r.voice.phase(), rules::IDLE, "the device is free after `note`");
    r.wait_event("note reply", |e| matches!(e, Event::NoteReply { text, item_id, .. } if text == "Fatto." && item_id.as_deref() == Some("itm_l1")));
    r.wait_event("note done", |e| matches!(e, Event::NoteDone { share_id } if share_id == "sh1"));
    assert_eq!(r.voice.pending_notes(), 0);
    assert!(r.has(|e| matches!(e, Event::Transcript(t) if t == "ricordami il latte")));
    assert!(r.has(|e| matches!(e, Event::Timing { what: "end to transcript", .. })));
    assert!(r.has(|e| matches!(e, Event::Timing { what: "end to note", .. })));

    let seen = mock.seen.lock().unwrap();
    let start = &seen.texts[0];
    assert_eq!(
        *start,
        json!({"type":"start","mode":"ptt","purpose":"note","codec":"pcm16","rate":16000,"thread":"main","channel":"web","source":"juno-chat"})
    );
    assert_eq!(seen.texts[1], json!({"type":"end"}));
    assert_eq!(seen.texts.len(), 2);
    // One second at 48 kHz is exactly 16000 samples at 16 kHz, all before the `end`.
    assert_eq!(seen.bytes_before_end, 32_000);
    assert_eq!(seen.bytes_after_end, 0);
    assert_eq!(seen.auth, vec![Some(format!("Bearer {TOKEN}"))]);
    assert!(!seen.client_header, "X-Secretary-Client is not sent");
    assert_eq!(seen.pongs, 1, "a ping is answered");
    assert_eq!(seen.connections, 1);
    assert_eq!(r.names()[..2], ["Connected".to_owned(), "Listening".to_owned()]);
}

#[test]
fn a_voice_command_streams_text_and_plays_the_speech() {
    let mock = Mock::start(Behaviour::default());
    let r = rig(mock.url(), false);
    r.wait_event("connected", |e| *e == Event::Connected);
    r.voice.press(Start::command()).unwrap();
    r.wait_event("done", |e| *e == Event::Done);
    let names = r.names();
    assert_eq!(
        names,
        ["Connected", "Listening", "Transcript", "Delta", "Delta", "Answer", "Reply", "Done"].map(str::to_owned).to_vec(),
        "{names:?}"
    );
    assert!(r.has(|e| matches!(e, Event::Timing { what: "end to first text", .. })));
    assert!(r.has(|e| matches!(e, Event::Timing { what: "end to first audio", .. })));
    assert_eq!(r.voice.phase(), rules::IDLE);

    let seen = mock.seen.lock().unwrap();
    assert_eq!(
        seen.texts[0],
        json!({"type":"start","mode":"vad","purpose":"command","codec":"pcm16","rate":16000,"thread":"main","channel":"web","text":true,"speak":true})
    );
    assert_eq!(seen.bytes_before_end, 32_000);
    drop(seen);

    // 3 frames of 480 mono samples at 24 kHz become 480 * 4 samples of 2 bytes each, per frame.
    wait_until("speech played", || r.speaker.lock().unwrap().closed >= 1);
    let sp = r.speaker.lock().unwrap();
    assert_eq!(sp.bytes.len(), 3 * 480 * 4 * 2);
    assert_eq!(sp.volume, Some(-18), "the volume is set after the device opens");
    assert_eq!(sp.opened, 1);
    // First sample of the first frame is 0: after the zero start, midpoint 0, then 0; the second is 10: midpoint 5.
    let s = |i: usize| i16::from_le_bytes([sp.bytes[2 * i], sp.bytes[2 * i + 1]]);
    assert_eq!((s(0), s(1), s(2), s(3)), (0, 0, 0, 0));
    assert_eq!((s(4), s(5), s(6), s(7)), (5, 5, 10, 10));
}

#[test]
fn escape_cancels_and_nothing_is_stored() {
    let mock = Mock::start(Behaviour::default());
    let r = rig(mock.url(), true);
    r.wait_event("connected", |e| *e == Event::Connected);
    r.voice.press(Start::note()).unwrap();
    r.wait_event("listening", |e| *e == Event::Listening);
    thread::sleep(Duration::from_millis(200));
    r.voice.cancel();
    assert_eq!(r.voice.phase(), rules::IDLE);
    wait_until("the cancel reached the server", || mock.seen.lock().unwrap().texts.iter().any(|t| t["type"] == "cancel"));
    thread::sleep(Duration::from_millis(200));
    let seen = mock.seen.lock().unwrap();
    assert!(!seen.texts.iter().any(|t| t["type"] == "end"));
    assert!(seen.bytes_before_end > 0, "audio went up while he talked");
    drop(seen);
    assert!(!r.has(|e| matches!(e, Event::NoteStored { .. })));
    // The microphone lets go, and a new turn is accepted.
    wait_until("a new press is accepted", || r.voice.press(Start::command()).is_ok());
    r.voice.cancel();
}

#[test]
fn an_old_server_cannot_take_a_note() {
    let mock = Mock::start(Behaviour { old_server: true, ..Default::default() });
    let r = rig(mock.url(), true);
    r.wait_event("connected", |e| *e == Event::Connected);
    r.voice.press(Start::note()).unwrap();
    r.wait_event("old server", |e| *e == Event::OldServer);
    wait_until("cancel", || mock.seen.lock().unwrap().texts.iter().any(|t| t["type"] == "cancel"));
    assert_eq!(r.voice.phase(), rules::IDLE);
}

#[test]
fn a_refused_token_says_signed_out_and_a_dropped_connection_comes_back() {
    let mock = Mock::start(Behaviour { refuse_401: true, ..Default::default() });
    let r = rig(mock.url(), false);
    r.wait_event("signed out", |e| matches!(e, Event::Disconnected(why) if why == "signed out"));
    assert!(!r.voice.connected());
    assert_eq!(r.voice.press(Start::note()), Err(PressError::NotConnected));
    drop(r);
    drop(mock);

    let mock = Mock::start(Behaviour { drop_first: true, ..Default::default() });
    let r = rig(mock.url(), false);
    r.wait_event("down once", |e| matches!(e, Event::Disconnected(_)));
    // The first try fails; the second, after the 1 s backoff, succeeds.
    r.wait_event("up again", |e| *e == Event::Connected);
    assert!(mock.seen.lock().unwrap().connections >= 2);
    assert_eq!(r.names().iter().filter(|n| *n == "Disconnected").count(), 1, "one report per outage");
}

#[test]
fn closing_the_screen_closes_the_socket_and_the_threads() {
    let mock = Mock::start(Behaviour::default());
    {
        let r = rig(mock.url(), true);
        r.wait_event("connected", |e| *e == Event::Connected);
        r.voice.press(Start::command()).unwrap();
        r.wait_event("listening", |e| *e == Event::Listening);
    }
    wait_until("the server saw the cancel", || mock.seen.lock().unwrap().texts.iter().any(|t| t["type"] == "cancel"));
}

/// The host check (`examples/wav_turn.rs`) runs the same client on a WAV file against the mock, for both purposes.
#[test]
fn the_host_check_runs_a_note_and_a_command_from_wav_files() {
    let exe = std::env::current_exe().unwrap();
    let example = exe.parent().unwrap().parent().unwrap().join("examples").join("wav_turn");
    assert!(example.exists(), "cargo test builds the examples: {example:?}");
    let dir = std::env::temp_dir().join(format!("voice-stream-host-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 1.5 s of a 300 Hz tone, 16 kHz mono 16-bit.
    let pcm: Vec<u8> = (0..24_000)
        .flat_map(|i| ((6000.0 * (2.0 * std::f64::consts::PI * 300.0 * i as f64 / 16_000.0).sin()) as i16).to_le_bytes())
        .collect();
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&[1, 0, 1, 0]);
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&[2, 0, 16, 0]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    let input = dir.join("in.wav");
    let token = dir.join("token");
    std::fs::write(&input, wav).unwrap();
    std::fs::write(&token, format!("{TOKEN}\n")).unwrap();

    for purpose in ["note", "command"] {
        let mock = Mock::start(Behaviour::default());
        let out = dir.join(format!("{purpose}.wav"));
        let run = std::process::Command::new(&example)
            .args(["--url", &mock.url(), "--token-file", token.to_str().unwrap(), "--in", input.to_str().unwrap()])
            .args(["--out", out.to_str().unwrap(), "--purpose", purpose])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&run.stdout).to_string();
        assert!(run.status.success(), "{purpose}: {stdout} {}", String::from_utf8_lossy(&run.stderr));
        let seen = mock.seen.lock().unwrap();
        // 1.5 s at 16 kHz in, resampled to 48 kHz and decimated back: the same 24000 samples.
        assert_eq!(seen.bytes_before_end, 48_000, "{purpose}");
        assert_eq!(seen.auth, vec![Some(format!("Bearer {TOKEN}"))]);
        if purpose == "note" {
            assert!(stdout.contains("note stored") && stdout.contains("note done"), "{stdout}");
            assert!(!out.exists(), "a note has no speech");
        } else {
            assert!(stdout.contains("transcript") && stdout.contains("done"), "{stdout}");
            let wav = std::fs::read(&out).unwrap();
            assert_eq!(&wav[0..4], b"RIFF");
            assert_eq!(wav.len(), 44 + 3 * 480 * 4 * 2, "{stdout} {}", String::from_utf8_lossy(&run.stderr));
        }
        assert!(!stdout.contains(TOKEN) && !stdout.contains("che ore sono"), "no token or text in the output");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// No poll loop (owner rule, 2026-11-08): an open link that is idle sleeps in poll(2) and runs no pass at all.
#[test]
fn an_idle_open_link_makes_no_reads() {
    let mock = Mock::start(Behaviour::default());
    let r = rig(mock.url(), false);
    r.wait_event("connected", |e| *e == Event::Connected);
    thread::sleep(Duration::from_millis(300));
    let before = r.voice.link_passes();
    thread::sleep(Duration::from_secs(2));
    assert_eq!(r.voice.link_passes() - before, 0, "loop passes while idle");
    // And it still wakes at once for a turn.
    r.voice.press(Start::note()).unwrap();
    r.wait_event("note stored", |e| matches!(e, Event::NoteStored { .. }));
    assert!(r.voice.link_passes() > before);
}
