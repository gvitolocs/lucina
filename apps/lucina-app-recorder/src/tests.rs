//! The upload against a server that behaves as `web/src/api/memos.ts` says, including one that loses answers, and the recording against a fake
//! microphone.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use lucina_audio_decode::device::Mic;
use serde_json::json;

use crate::app;
use crate::capture::{self, Live};
use crate::upload::{self, Memos, MemoState, Meta, UploadError};
use crate::wav;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lucina-app-recorder-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn recording(dir: &std::path::Path, data_bytes: usize) -> (PathBuf, Meta) {
    let path = dir.join("r.wav");
    let mut w = wav::WavWriter::create(&path).unwrap();
    let samples: Vec<u8> = (0..data_bytes).map(|i| (i % 251) as u8).collect();
    w.write(&samples).unwrap();
    w.finish().unwrap();
    let meta = app::new_meta(data_bytes as u64, "UTC", String::from("2026-11-02T10:00:00Z"), String::from("c1"));
    upload::save_meta(&path, &meta);
    (path, meta)
}

/// `lucina-memo` as the web client sees it.
#[derive(Default)]
struct Server {
    received: Vec<u8>,
    id: Option<String>,
    status: String,
    creates: u32,
    client_ids: Vec<String>,
    completed: Option<String>,
    /// Puts that arrive and are stored, but whose answer is lost.
    lose_answer_of_put: VecDeque<u32>,
    puts: u32,
    offline_after_puts: Option<u32>,
    wrong_offsets: u32,
}

impl Server {
    fn new() -> Server {
        Server { status: String::from("uploading"), ..Server::default() }
    }

    fn memo(&self) -> MemoState {
        MemoState {
            id: self.id.clone().unwrap_or_default(),
            received_bytes: self.received.len() as u64,
            status: self.status.clone(),
        }
    }
}

impl Memos for Server {
    fn create(&mut self, meta: &Meta) -> Result<MemoState, UploadError> {
        self.creates += 1;
        self.client_ids.push(meta.client_id.clone());
        self.id.get_or_insert_with(|| String::from("m1"));
        Ok(self.memo())
    }
    fn detail(&mut self, _id: &str) -> Result<MemoState, UploadError> {
        Ok(self.memo())
    }
    fn put(&mut self, _id: &str, offset: u64, chunk: &[u8]) -> Result<u64, UploadError> {
        if self.offline_after_puts.is_some_and(|n| self.puts >= n) {
            return Err(UploadError::Offline(String::from("no network")));
        }
        self.puts += 1;
        // A wrong offset: 409 with the byte count, which the client treats as an answer.
        if offset != self.received.len() as u64 {
            self.wrong_offsets += 1;
            return Ok(self.received.len() as u64);
        }
        self.received.extend_from_slice(chunk);
        if self.lose_answer_of_put.front() == Some(&self.puts) {
            self.lose_answer_of_put.pop_front();
            return Err(UploadError::Offline(String::from("the answer was lost")));
        }
        Ok(self.received.len() as u64)
    }
    fn complete(&mut self, _id: &str, sha256: &str) -> Result<MemoState, UploadError> {
        self.completed = Some(sha256.to_owned());
        self.status = String::from("queued");
        Ok(self.memo())
    }
}

const THREE_CHUNKS: usize = 3 * upload::CHUNK as usize + 1000;

#[test]
fn a_recording_reaches_the_server_whole_and_the_hash_is_of_the_file() {
    let dir = temp("whole");
    let (path, mut meta) = recording(&dir, THREE_CHUNKS);
    let mut server = Server::new();
    let mut seen = Vec::new();
    upload::send(&mut server, &path, &mut meta, &mut |n| seen.push(n)).unwrap();
    assert_eq!(server.received, std::fs::read(&path).unwrap());
    assert_eq!(server.completed.as_deref(), Some(upload::sha256_file(&path).unwrap().as_str()));
    assert!(meta.done);
    assert_eq!(seen.last(), Some(&meta.size_bytes));
    assert_eq!(upload::load_meta(&path).unwrap().server_id.as_deref(), Some("m1"));
    // Asked again, nothing more happens.
    let puts = server.puts;
    upload::send(&mut server, &path, &mut meta, &mut |_| {}).unwrap();
    assert_eq!(server.puts, puts);
}

#[test]
fn a_lost_network_goes_on_from_the_servers_count_even_after_a_restart() {
    let dir = temp("lost");
    let (path, mut meta) = recording(&dir, THREE_CHUNKS);
    let mut server = Server::new();
    server.offline_after_puts = Some(2);
    assert!(matches!(upload::send(&mut server, &path, &mut meta, &mut |_| {}), Err(UploadError::Offline(_))));
    assert_eq!(server.received.len() as u64, 2 * upload::CHUNK);
    // The program ends and starts again: only what is on the disk is left.
    let mut meta = upload::load_meta(&path).unwrap();
    assert_eq!(meta.error.as_deref(), Some("no network"));
    server.offline_after_puts = None;
    upload::send(&mut server, &path, &mut meta, &mut |_| {}).unwrap();
    assert_eq!(server.received, std::fs::read(&path).unwrap());
    assert_eq!(server.creates, 1, "a memo, once");
    assert_eq!(meta.error, None);
}

#[test]
fn a_chunk_stored_with_its_answer_lost_is_not_sent_twice() {
    let dir = temp("answer");
    let (path, mut meta) = recording(&dir, THREE_CHUNKS);
    let mut server = Server::new();
    server.lose_answer_of_put.push_back(2);
    assert!(upload::send(&mut server, &path, &mut meta, &mut |_| {}).is_err());
    upload::send(&mut server, &path, &mut meta, &mut |_| {}).unwrap();
    assert_eq!(server.received, std::fs::read(&path).unwrap());
    assert_eq!(server.wrong_offsets, 0);
}

#[test]
fn a_create_whose_answer_was_lost_is_the_same_memo() {
    let dir = temp("create");
    let (path, mut meta) = recording(&dir, 1000);
    let mut server = Server::new();
    server.received = std::fs::read(&path).unwrap();
    // The server already has it all and has moved on: no chunk, and nothing to complete.
    server.status = String::from("transcribing");
    upload::send(&mut server, &path, &mut meta, &mut |_| {}).unwrap();
    assert_eq!(server.puts, 0);
    assert_eq!(server.completed, None);
    assert!(meta.done);
    assert_eq!(server.client_ids, vec![String::from("c1")]);
}

#[test]
fn a_server_that_does_not_move_forward_stops_it() {
    struct Stuck;
    impl Memos for Stuck {
        fn create(&mut self, _: &Meta) -> Result<MemoState, UploadError> {
            Ok(MemoState { id: "m".into(), received_bytes: 0, status: "uploading".into() })
        }
        fn detail(&mut self, _: &str) -> Result<MemoState, UploadError> {
            unreachable!()
        }
        fn put(&mut self, _: &str, _: u64, _: &[u8]) -> Result<u64, UploadError> {
            Ok(0)
        }
        fn complete(&mut self, _: &str, _: &str) -> Result<MemoState, UploadError> {
            panic!("not with nothing sent")
        }
    }
    let dir = temp("stuck");
    let (path, mut meta) = recording(&dir, 1000);
    assert!(matches!(upload::send(&mut Stuck, &path, &mut meta, &mut |_| {}), Err(UploadError::Refused(_))));
    assert!(!meta.done);
}

#[test]
fn the_requests_and_answers_are_the_web_clients() {
    let meta = app::new_meta(96_000, "Europe/Copenhagen", String::from("2026-11-02T10:00:00Z"), String::from("c1"));
    assert_eq!(meta.duration_sec, 1.0);
    assert_eq!(
        upload::create_body(&meta),
        json!({
            "clientId": "c1", "recordedAt": "2026-11-02T10:00:00Z", "timezone": "Europe/Copenhagen",
            "durationSec": 1.0, "mime": "audio/wav", "sizeBytes": 96_044, "source": "juno",
        })
    );
    let state = upload::memo_from(&json!({"memo": {"id": "m", "clientId": "c1", "receivedBytes": 5, "status": "uploading", "title": "x"}})).unwrap();
    assert_eq!(state, MemoState { id: "m".into(), received_bytes: 5, status: "uploading".into() });
    assert!(upload::memo_from(&json!({"oops": 1})).is_err());
}

struct FakeMic {
    blocks: VecDeque<io::Result<Vec<u8>>>,
    live: Arc<Live>,
    closed: bool,
}

impl Mic for FakeMic {
    fn open(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.blocks.pop_front() {
            Some(Ok(block)) => {
                buf[..block.len()].copy_from_slice(&block);
                Ok(block.len())
            }
            Some(Err(err)) => Err(err),
            None => {
                self.live.stop.store(true, Ordering::Relaxed);
                Ok(2)
            }
        }
    }
    fn close(&mut self) {
        self.closed = true;
    }
}

#[test]
fn what_the_microphone_gave_is_in_a_wav_file_the_decoder_reads() {
    let dir = temp("mic");
    let path = dir.join("r.wav");
    let live = Arc::new(Live::default());
    let block: Vec<u8> = (0..4096).flat_map(|i: i16| i.to_le_bytes()).take(4096).collect();
    let mut mic = FakeMic { blocks: (0..50).map(|_| Ok(block.clone())).collect(), live: live.clone(), closed: false };
    let finished = capture::record(&mut mic, &path, live.clone());
    assert_eq!(finished.error, None);
    assert!(mic.closed);
    assert_eq!(finished.data_bytes, 50 * 4096 + 2);
    assert_eq!(live.peak.load(Ordering::Relaxed), 0);
    let mut decoder = lucina_audio_decode::Decoder::open(&path).unwrap();
    assert_eq!(decoder.rate(), 48_000);
    let mut out = Vec::new();
    while decoder.next_block(&mut out).unwrap() {}
    // Mono comes out doubled: each sample twice, in order.
    assert_eq!(out.len() as u64, finished.data_bytes * 2);
    assert_eq!(&out[..8], &[0, 0, 0, 0, 1, 0, 1, 0]);
}

#[test]
fn a_failing_microphone_keeps_what_it_gave() {
    let dir = temp("fail");
    let path = dir.join("r.wav");
    let live = Arc::new(Live::default());
    let mut mic = FakeMic {
        blocks: VecDeque::from([Ok(vec![1u8; 1000]), Err(io::Error::other("gone"))]),
        live: live.clone(),
        closed: false,
    };
    let finished = capture::record(&mut mic, &path, live);
    assert!(finished.error.unwrap().contains("gone"));
    assert_eq!(finished.data_bytes, 1000);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 1044);
}

#[test]
fn a_recording_cut_off_by_a_crash_is_repaired_and_listed() {
    let dir = temp("crash");
    let path = dir.join("cut.wav");
    let mut w = wav::WavWriter::create(&path).unwrap();
    w.write(&vec![7u8; 96_001]).unwrap();
    w.sync().unwrap();
    // No finish(): the sizes in the header are still 0, and there is no file beside it.
    std::mem::forget(w);
    let rows = app::list(&dir, "UTC");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].seconds, 1.0);
    assert_eq!(rows[0].size, 96_044);
    assert!(!rows[0].done);
    let mut decoder = lucina_audio_decode::Decoder::open(&path).unwrap();
    let mut out = Vec::new();
    while decoder.next_block(&mut out).unwrap() {}
    assert_eq!(out.len(), 192_000);
}

#[test]
fn choosing_a_recording_plays_it_choosing_it_again_stops_and_recording_stops_it() {
    use std::cell::RefCell;
    use std::rc::Rc;
    use lucina_audio_decode::engine::{Cmd, Report};
    use lucina_app::App;
    let sent: Rc<RefCell<Vec<String>>> = Rc::default();
    let log = sent.clone();
    let play: app::Play = Box::new(move |cmd: Cmd| log.borrow_mut().push(format!("{cmd:?}")));
    let mut r = app::Recorder::new(
        PathBuf::from("/tmp/none"),
        false,
        String::from("UTC"),
        Box::new(|_, _| {}),
        Box::new(|_| {}),
        play,
    );
    let row = |name: &str| app::Row {
        path: PathBuf::from(format!("/tmp/none/{name}.wav")),
        title: name.to_owned(),
        seconds: 2.0,
        size: 192_044,
        uploaded: 0,
        done: false,
        error: None,
    };
    r.rows = vec![row("a"), row("b")];
    r.choose(0);
    assert_eq!(r.playing, Some(PathBuf::from("/tmp/none/a.wav")));
    r.choose(1);
    assert_eq!(r.playing, Some(PathBuf::from("/tmp/none/b.wav")));
    r.choose(1);
    assert_eq!(r.playing, None);
    r.choose(0);
    r.message(app::Msg::Played(Report::Ended));
    assert_eq!(r.playing, None);
    r.choose(0);
    r.begin("c.wav");
    assert_eq!(r.playing, None);
    r.choose(1);
    assert_eq!(r.playing, None, "nothing plays while recording");
    let sent = sent.borrow();
    assert_eq!(sent.iter().filter(|c| c.starts_with("Play")).count(), 4);
    // Choosing the playing one again, the end of a recording, and the start of a new recording each let the device go.
    assert_eq!(sent.iter().filter(|c| *c == "Stop").count(), 3);
}
