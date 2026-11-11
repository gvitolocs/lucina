//! The engine against a device that takes samples as fast as they come, the screen through the toolkit's harness, and the server's
//! answers as `web/src/api/player.ts` types them.

use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use lucina_app::{App, Harness, keys};
use lucina_audio_decode::device::Sink;
use secretary_player::progress::Entry;
use secretary_player::queue::Item;
use serde_json::json;

use crate::app::{self, Job, Msg, Player, Tab};
use lucina_audio_decode::engine::{Cmd, Engine, Report};
use crate::library::{self, Server};

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lucina-app-player-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A ramp, so that a sample says where it came from.
fn wav(path: &std::path::Path, frames: u32) {
    let mut data = Vec::new();
    for i in 0..frames {
        let s = (i / 16) as i16;
        data.extend_from_slice(&s.to_le_bytes());
        data.extend_from_slice(&s.to_le_bytes());
    }
    let mut f = Vec::new();
    f.extend_from_slice(b"RIFF");
    f.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    f.extend_from_slice(b"WAVEfmt ");
    f.extend_from_slice(&16u32.to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&2u16.to_le_bytes());
    f.extend_from_slice(&48_000u32.to_le_bytes());
    f.extend_from_slice(&192_000u32.to_le_bytes());
    f.extend_from_slice(&4u16.to_le_bytes());
    f.extend_from_slice(&16u16.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&(data.len() as u32).to_le_bytes());
    f.extend_from_slice(&data);
    std::fs::write(path, f).unwrap();
}

#[derive(Default)]
struct DeviceState {
    open: bool,
    bytes: Vec<u8>,
    played: u64,
    volumes: Vec<i32>,
    flushes: u32,
    busy: bool,
    /// How much it takes at a time before saying it is full.
    room: usize,
}

#[derive(Clone, Default)]
struct FakeDevice(Arc<Mutex<DeviceState>>);

impl Sink for FakeDevice {
    fn open(&mut self) -> io::Result<()> {
        let mut d = self.0.lock().unwrap();
        if d.busy {
            return Err(io::Error::from(io::ErrorKind::ResourceBusy));
        }
        d.open = true;
        Ok(())
    }
    fn close(&mut self) {
        self.0.lock().unwrap().open = false;
    }
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut d = self.0.lock().unwrap();
        assert!(d.open, "a write to a closed device");
        if d.room == 0 {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let n = bytes.len().min(d.room);
        d.bytes.extend_from_slice(&bytes[..n]);
        d.room -= n;
        Ok(n)
    }
    fn flush(&mut self) {
        self.0.lock().unwrap().flushes += 1;
    }
    fn set_volume(&mut self, db: i32) {
        self.0.lock().unwrap().volumes.push(db);
    }
    fn played_frames(&mut self) -> u64 {
        self.0.lock().unwrap().played
    }
}

type Reports = Rc<RefCell<Vec<Report>>>;

fn engine(device: &FakeDevice) -> (Engine<FakeDevice, impl Fn(Report)>, Reports) {
    let reports: Reports = Rc::default();
    let sink = reports.clone();
    (Engine::new(device.clone(), -24, move |r| sink.borrow_mut().push(r)), reports)
}

fn set_room(device: &FakeDevice, room: usize) {
    device.0.lock().unwrap().room = room;
}

#[test]
fn a_file_reaches_the_device_in_order_and_ends_only_when_it_has_played() {
    let dir = temp("order");
    let path = dir.join("a.wav");
    wav(&path, 48_000);
    let device = FakeDevice::default();
    let (mut e, reports) = engine(&device);
    set_room(&device, 1 << 30);
    e.command(Cmd::Play { path, start_ms: 0 });
    e.pump();
    {
        let d = device.0.lock().unwrap();
        assert_eq!(d.bytes.len(), 192_000);
        assert_eq!(d.volumes, vec![-24], "the volume is set after the device is taken");
        for (i, frame) in d.bytes.chunks_exact(4).enumerate().step_by(997) {
            assert_eq!(i16::from_le_bytes([frame[0], frame[1]]), (i / 16) as i16);
        }
    }
    assert!(!reports.borrow().contains(&Report::Ended), "all written, none played");
    device.0.lock().unwrap().played = 47_000;
    e.pump();
    assert!(!reports.borrow().contains(&Report::Ended));
    device.0.lock().unwrap().played = 48_000;
    e.pump();
    assert!(reports.borrow().contains(&Report::Ended));
}

#[test]
fn a_full_device_makes_it_wait_without_losing_anything() {
    let dir = temp("full");
    let path = dir.join("a.wav");
    wav(&path, 48_000);
    let device = FakeDevice::default();
    let (mut e, _reports) = engine(&device);
    e.command(Cmd::Play { path, start_ms: 0 });
    let mut rounds = 0;
    loop {
        set_room(&device, 3001);
        e.pump();
        rounds += 1;
        if device.0.lock().unwrap().bytes.len() >= 192_000 || rounds > 1000 {
            break;
        }
    }
    let d = device.0.lock().unwrap();
    assert_eq!(d.bytes.len(), 192_000);
    assert!(rounds > 10);
}

#[test]
fn a_pause_lets_the_device_go_and_going_on_starts_where_the_sound_stopped() {
    let dir = temp("pause");
    let path = dir.join("a.wav");
    wav(&path, 48_000 * 4);
    let device = FakeDevice::default();
    let (mut e, reports) = engine(&device);
    set_room(&device, 96_000);
    e.command(Cmd::Play { path, start_ms: 0 });
    e.pump();
    device.0.lock().unwrap().played = 48_000;
    e.command(Cmd::Pause);
    assert!(reports.borrow().contains(&Report::Paused { ms: 1000 }));
    {
        let mut d = device.0.lock().unwrap();
        assert!(!d.open, "the amplifier goes off with the device");
        assert_eq!(d.flushes, 1);
        // The next stream counts from zero.
        d.played = 0;
        d.bytes.clear();
    }
    set_room(&device, 4096);
    e.command(Cmd::Resume);
    e.pump();
    let d = device.0.lock().unwrap();
    assert!(d.open);
    assert_eq!(d.volumes, vec![-24, -24], "the kernel forgets the volume with the program");
    let first = i16::from_le_bytes([d.bytes[0], d.bytes[1]]);
    assert!((i64::from(first) * 16 - 48_000).abs() < 4800, "{first}");
}

fn last_position(reports: &Reports) -> (u64, Option<u64>) {
    match reports.borrow().last() {
        Some(Report::Position { ms, duration_ms }) => (*ms, *duration_ms),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_position_counts_what_played_not_what_was_written_and_seeking_moves_it() {
    let dir = temp("seek");
    let path = dir.join("a.wav");
    wav(&path, 48_000 * 10);
    let device = FakeDevice::default();
    let (mut e, reports) = engine(&device);
    set_room(&device, 1 << 20);
    e.command(Cmd::Play { path, start_ms: 2000 });
    e.pump();
    device.0.lock().unwrap().played = 24_000;
    std::thread::sleep(std::time::Duration::from_millis(260));
    e.pump();
    // WAV seeks land on a packet, a little before where was asked.
    let (ms, duration) = last_position(&reports);
    assert!((2450..=2510).contains(&ms), "{ms}");
    assert_eq!(duration, Some(10000));
    e.command(Cmd::Seek(6000));
    assert_eq!(device.0.lock().unwrap().flushes, 1);
    device.0.lock().unwrap().played = 24_000 + 4_800;
    std::thread::sleep(std::time::Duration::from_millis(260));
    e.pump();
    let (ms, _) = last_position(&reports);
    assert!((6050..=6110).contains(&ms), "{ms}");
}

#[test]
fn a_file_it_cannot_read_or_a_busy_device_is_said_not_fatal() {
    let dir = temp("bad");
    let device = FakeDevice::default();
    let (mut e, reports) = engine(&device);
    let junk = dir.join("junk.mp3");
    std::fs::write(&junk, [0x55u8; 4096]).unwrap();
    e.command(Cmd::Play { path: junk, start_ms: 0 });
    assert!(matches!(reports.borrow().last(), Some(Report::Failed(_))));
    assert!(!device.0.lock().unwrap().open);

    device.0.lock().unwrap().busy = true;
    let path = dir.join("a.wav");
    wav(&path, 4800);
    e.command(Cmd::Play { path, start_ms: 0 });
    let last = format!("{:?}", reports.borrow().last());
    assert!(last.contains("sound device"), "{last}");
}

#[test]
fn after_a_stop_the_engine_still_plays() {
    let dir = temp("stop");
    let path = dir.join("a.wav");
    wav(&path, 4800);
    let device = FakeDevice::default();
    let reports: Arc<Mutex<Vec<String>>> = Arc::default();
    let sink = reports.clone();
    let e = Engine::new(device.clone(), -24, move |r| sink.lock().unwrap().push(format!("{r:?}")));
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || e.run(rx));
    set_room(&device, 1 << 20);
    tx.send(Cmd::Stop).unwrap();
    tx.send(Cmd::Play { path, start_ms: 0 }).unwrap();
    for _ in 0..200 {
        if device.0.lock().unwrap().bytes.len() == 19_200 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(device.0.lock().unwrap().bytes.len(), 19_200);
    drop(tx);
    thread.join().unwrap();
}

// The screen.

type Sent = Rc<RefCell<Vec<String>>>;

fn player(dir: PathBuf, signed_in: bool) -> (Harness<Player>, Sent, Sent) {
    let engine_log: Sent = Rc::default();
    let link_log: Sent = Rc::default();
    let (e, l) = (engine_log.clone(), link_log.clone());
    let p = Player::new(
        dir,
        signed_in,
        Box::new(move |cmd| e.borrow_mut().push(format!("{cmd:?}"))),
        Box::new(move |job| {
            l.borrow_mut().push(match job {
                Job::ListLocal => String::from("local"),
                Job::ListRemote => String::from("remote"),
                Job::Fetch(id) => format!("fetch {id}"),
                Job::Push(entries) => format!("push {}", entries.len()),
            })
        }),
    );
    (Harness::new(p, 1920, 1080), engine_log, link_log)
}

fn items(names: &[&str]) -> Vec<Item> {
    names
        .iter()
        .map(|n| Item {
            id: format!("{}/data/music/{n}.mp3", library::LOCAL_PREFIX),
            kind: "audio".into(),
            title: (*n).into(),
            duration_ms: Some(600_000),
            extra: Default::default(),
        })
        .collect()
}

fn walk_to_list_and_open(h: &mut Harness<Player>) {
    h.app().list.select(0);
    h.app().activate(0);
}

#[test]
fn opening_a_row_plays_it_and_the_queue_goes_on_to_the_next() {
    let (mut h, engine, link) = player(temp("open"), false);
    assert_eq!(*link.borrow(), vec!["local"], "no account, no library request");
    assert!(engine.borrow()[0].contains("Volume(-24)"), "{:?}", engine.borrow());
    h.app().message(Msg::Local(items(&["one", "two"])));
    walk_to_list_and_open(&mut h);
    assert!(engine.borrow().last().unwrap().contains("one.mp3"));
    assert!(h.app().playing);
    h.app().message(Msg::Engine(Report::Ended));
    assert!(engine.borrow().last().unwrap().contains("two.mp3"));
    h.app().message(Msg::Engine(Report::Ended));
    assert!(engine.borrow().last().unwrap().contains("Stop"));
    assert!(!h.app().playing);
}

#[test]
fn a_file_from_the_command_line_plays_once_the_list_has_it() {
    let (mut h, engine, _) = player(temp("argfile"), false);
    let wanted = format!("{}/data/music/two.mp3", library::LOCAL_PREFIX);
    h.app().open_on_start = Some(wanted.clone());
    h.app().message(Msg::Local(items(&["one", "two", "three"])));
    assert!(h.app().playing);
    assert_eq!(h.app().queue.current().map(|i| i.id.clone()), Some(wanted));
    assert!(engine.borrow().last().unwrap().contains("two.mp3"), "{:?}", engine.borrow());
    // A second list does not start it again.
    let before = engine.borrow().len();
    h.app().message(Msg::Local(items(&["one", "two", "three"])));
    assert_eq!(engine.borrow().len(), before);
}

#[test]
fn a_file_the_list_does_not_have_plays_nothing_and_says_so() {
    let (mut h, engine, _) = player(temp("argmissing"), false);
    h.app().open_on_start = Some(format!("{}/data/music/notes.txt", library::LOCAL_PREFIX));
    h.app().message(Msg::Local(items(&["one"])));
    assert!(!h.app().playing);
    assert!(h.app().message.contains("not one Player can play"));
    assert!(!engine.borrow().iter().any(|c| c.contains("Play")));
    assert_eq!(h.app().open_on_start, None);
}

#[test]
fn space_pauses_and_goes_on() {
    let (mut h, engine, _) = player(temp("space"), false);
    h.app().message(Msg::Local(items(&["one"])));
    walk_to_list_and_open(&mut h);
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    h.key(keys::KEY_SPACE);
    assert!(engine.borrow().last().unwrap().contains("Pause"), "{:?}", engine.borrow());
    h.app().message(Msg::Engine(Report::Paused { ms: 5000 }));
    assert!(!h.app().playing);
    h.key(keys::KEY_SPACE);
    assert!(engine.borrow().last().unwrap().contains("Resume"));
}

#[test]
fn it_starts_again_where_it_was_left_even_after_the_program_ended() {
    let dir = temp("resume");
    {
        let (mut h, _, _) = player(dir.clone(), false);
        h.app().message(Msg::Local(items(&["one"])));
        walk_to_list_and_open(&mut h);
        h.app().message(Msg::Engine(Report::Position { ms: 120_000, duration_ms: Some(600_000) }));
        h.app().message(Msg::Engine(Report::Paused { ms: 120_000 }));
        h.app().on_quit();
    }
    let (mut h, engine, _) = player(dir, false);
    h.app().message(Msg::Local(items(&["one"])));
    walk_to_list_and_open(&mut h);
    // `resume_point` backs up two seconds.
    assert!(engine.borrow().last().unwrap().contains("start_ms: 118000"), "{:?}", engine.borrow());
}

#[test]
fn the_volume_stays_between_the_limits() {
    let (mut h, engine, _) = player(temp("volume"), false);
    for _ in 0..10 {
        h.app().change_volume(3);
    }
    assert_eq!(h.app().volume, app::MAX_VOLUME);
    assert!(engine.borrow().last().unwrap().contains("Volume(-12)"));
    for _ in 0..40 {
        h.app().change_volume(-3);
    }
    assert_eq!(h.app().volume, app::MIN_VOLUME);
}

#[test]
fn a_library_item_is_fetched_then_played_and_a_late_file_for_another_is_ignored() {
    let (mut h, engine, link) = player(temp("fetch"), true);
    assert!(link.borrow().contains(&String::from("remote")));
    let remote = vec![
        Item { id: "m1".into(), kind: "audio".into(), title: "Memo".into(), duration_ms: Some(1000), extra: Default::default() },
        Item { id: "m2".into(), kind: "audio".into(), title: "Other".into(), duration_ms: None, extra: Default::default() },
    ];
    h.app().message(Msg::Remote(Ok(remote)));
    h.app().tab = Tab::Library;
    h.app().activate(0);
    assert_eq!(link.borrow().last().unwrap(), "fetch m1");
    let plays = |e: &Sent| e.borrow().iter().filter(|c| c.contains("Play")).count();
    assert_eq!(plays(&engine), 0);
    h.app().message(Msg::Fetched { id: "m2".into(), result: Ok(PathBuf::from("/x/m2")) });
    assert_eq!(plays(&engine), 0, "not what should play");
    h.app().message(Msg::Fetched { id: "m1".into(), result: Ok(PathBuf::from("/x/m1")) });
    assert_eq!(plays(&engine), 1);
    assert!(engine.borrow().last().unwrap().contains("/x/m1"));
}

#[test]
fn a_failed_fetch_says_why() {
    let (mut h, _, _) = player(temp("fail"), true);
    h.app().message(Msg::Remote(Ok(vec![Item {
        id: "m1".into(),
        kind: "audio".into(),
        title: "Memo".into(),
        duration_ms: None,
        extra: Default::default(),
    }])));
    h.app().tab = Tab::Library;
    h.app().activate(0);
    h.app().message(Msg::Fetched { id: "m1".into(), result: Err("offline".into()) });
    assert_eq!(h.app().message, "offline");
    assert!(!h.app().playing);
}

#[test]
fn enter_on_a_fresh_list_plays_the_first_row() {
    let (mut h, engine, _) = player(temp("enter"), false);
    h.app().message(Msg::Local(items(&["one", "two"])));
    h.core.input(lucina_app::Input::Wheel(0));
    h.core.paint();
    assert_eq!(h.app().list.selected, Some(0));
    h.key(keys::KEY_ENTER);
    assert!(engine.borrow().last().unwrap().contains("one.mp3"), "{:?}", engine.borrow());
    assert_eq!(h.app().queue.current().unwrap().title, "one");
}

// The intents.

#[test]
fn play_intent_plays_a_file_and_seeks_to_at() {
    let dir = temp("intent-play");
    let path = dir.join("song.wav");
    wav(&path, 48_000 * 10);
    let (mut h, engine, _) = player(dir.clone(), false);
    let r = h
        .intent("player.play", json!({"path": path.display().to_string()}))
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(
        r.entities,
        vec![lucina_app::intent::Entity::new(
            "track",
            &format!("file:{}", path.display()),
            "song"
        )]
    );
    assert_eq!(r.value["title"], json!("song"));
    assert_eq!(r.value["path"], json!(path.display().to_string()));
    assert!(
        engine.borrow().last().unwrap().contains("song.wav"),
        "{:?}",
        engine.borrow()
    );
    assert!(h.app().playing);
    assert_eq!(
        h.app().queue.current().unwrap().id,
        format!("file:{}", path.display())
    );

    let r = h
        .intent(
            "player.play",
            json!({"path": path.display().to_string(), "at": 5}),
        )
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.value["at_ms"], json!(5000));
    assert!(
        engine.borrow().last().unwrap().contains("Seek(5000)"),
        "{:?}",
        engine.borrow()
    );
}

#[test]
fn play_intent_finds_the_file_among_the_local_items() {
    let dir = temp("intent-here");
    let path = dir.join("a.wav");
    wav(&path, 4800);
    let (mut h, engine, _) = player(dir.clone(), false);
    h.app().message(Msg::Local(library::scan(std::slice::from_ref(&dir))));
    let r = h
        .intent("player.play", json!({"path": path.display().to_string()}))
        .unwrap();
    assert!(r.ok, "{r:?}");
    assert!(
        engine.borrow().last().unwrap().contains("a.wav"),
        "{:?}",
        engine.borrow()
    );
    assert_eq!(h.app().queue.current().unwrap().title, "a");
}

#[test]
fn play_intent_refuses_what_is_not_an_audio_file() {
    let dir = temp("intent-bad");
    let (mut h, _, _) = player(dir.clone(), false);
    let missing = dir.join("nope.wav");
    let r = h
        .intent(
            "player.play",
            json!({"path": missing.display().to_string()}),
        )
        .unwrap();
    assert!(!r.ok && r.text.contains("not a file"), "{r:?}");
    let text = dir.join("note.txt");
    std::fs::write(&text, b"x").unwrap();
    let r = h
        .intent("player.play", json!({"path": text.display().to_string()}))
        .unwrap();
    assert!(!r.ok && r.text.contains("not an audio file"), "{r:?}");
    assert!(!h.app().playing);
    assert!(h.app().queue.current().is_none());
}

#[test]
fn pause_and_resume_intents_toggle_like_the_button() {
    let dir = temp("intent-pause");
    let path = dir.join("a.wav");
    wav(&path, 48_000 * 10);
    let (mut h, engine, _) = player(dir.clone(), false);
    let r = h.intent("player.pause", json!({})).unwrap();
    assert!(!r.ok && r.text.contains("Nothing is playing"), "{r:?}");
    let r = h.intent("player.resume", json!({})).unwrap();
    assert!(!r.ok && r.text.contains("Nothing to resume"), "{r:?}");

    h.intent("player.play", json!({"path": path.display().to_string()}))
        .unwrap();
    let r = h.intent("player.pause", json!({})).unwrap();
    assert!(r.ok && r.text.contains("Paused a"), "{r:?}");
    assert!(
        engine.borrow().last().unwrap().contains("Pause"),
        "{:?}",
        engine.borrow()
    );
    h.app().message(Msg::Engine(Report::Paused { ms: 5000 }));
    assert!(!h.app().playing);
    let r = h.intent("player.resume", json!({})).unwrap();
    assert!(r.ok && r.text.contains("Resumed a"), "{r:?}");
    assert!(
        engine.borrow().last().unwrap().contains("Resume"),
        "{:?}",
        engine.borrow()
    );
}

#[test]
fn now_intent_says_what_is_playing_and_never_fails() {
    let dir = temp("intent-now");
    let path = dir.join("a.wav");
    wav(&path, 48_000 * 10);
    let (mut h, _, _) = player(dir.clone(), false);
    let r = h.intent("player.now", json!({})).unwrap();
    assert!(
        r.ok && r.value["playing"] == json!(false) && r.value["position_ms"] == json!(0),
        "{r:?}"
    );
    assert!(r.entities.is_empty());
    h.intent("player.play", json!({"path": path.display().to_string()}))
        .unwrap();
    h.app().message(Msg::Engine(Report::Position {
        ms: 2000,
        duration_ms: Some(10_000),
    }));
    let r = h.intent("player.now", json!({})).unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.value["playing"], json!(true));
    assert_eq!(r.value["title"], json!("a"));
    assert_eq!(r.value["id"], json!(format!("file:{}", path.display())));
    assert_eq!(r.value["position_ms"], json!(2000));
    assert_eq!(r.value["duration_ms"], json!(10_000));
    assert_eq!(r.entities[0].kind, "track");
}

// The library.

#[test]
fn the_scan_finds_audio_files_in_order_and_skips_the_rest() {
    let dir = temp("scan");
    std::fs::create_dir_all(dir.join("album")).unwrap();
    for name in ["b.mp3", "a.FLAC", "album/c.m4a", "note.txt", "cover.jpg"] {
        std::fs::write(dir.join(name), b"x").unwrap();
    }
    let found = library::scan(&[dir.clone(), dir.join("not-there")]);
    let titles: Vec<&str> = found.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, vec!["a", "c", "b"]);
    assert!(found.iter().all(|i| library::is_local(&i.id)));
    assert_eq!(library::local_path(&found[0].id), Some(dir.join("a.FLAC")));
}

#[test]
fn the_servers_answers_parse_as_the_web_app_types_them() {
    let page = json!({
        "items": [{"id": "abc", "kind": "audio", "title": "Memo", "mime": "audio/mp4", "size": 1234, "duration_ms": 65000, "poster_url": "/x"}],
        "next_cursor": "c2",
    });
    let (items, next) = library::items_from_page(&page).unwrap();
    assert_eq!(items[0].id, "abc");
    assert_eq!(items[0].duration_ms, Some(65000));
    assert_eq!(next.as_deref(), Some("c2"));
    assert!(library::items_from_page(&json!({"oops": 1})).is_err());

    let acks = library::acks_from(&json!({"acks": [{"id": "abc", "updated_at": 7}], "entries": []}));
    assert_eq!(acks, vec![(String::from("abc"), 7)]);

    let entries: Vec<Entry> = serde_json::from_value(json!([
        {"id": "abc", "position_ms": 5000, "duration_ms": 65000, "finished": false, "updated_at": 7, "device": null}
    ]))
    .unwrap();
    assert_eq!(entries[0].position_ms, 5000);
}

struct Counting {
    fetches: u32,
}

impl Server for Counting {
    fn audio_items(&mut self) -> Result<Vec<Item>, String> {
        Ok(Vec::new())
    }
    fn file(&mut self, _id: &str) -> Result<Vec<u8>, String> {
        self.fetches += 1;
        Ok(b"bytes".to_vec())
    }
    fn progress(&mut self) -> Result<Vec<Entry>, String> {
        Ok(Vec::new())
    }
    fn push(&mut self, _entries: &[Entry]) -> Result<Vec<(String, u64)>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn a_fetched_file_is_kept_and_an_id_cannot_leave_the_cache() {
    let dir = temp("cache");
    let mut server = Counting { fetches: 0 };
    let first = crate::fetch(&mut server, &dir, "../../etc/x").unwrap();
    assert!(first.starts_with(&dir));
    assert_eq!(first.parent(), Some(dir.as_path()));
    let again = crate::fetch(&mut server, &dir, "../../etc/x").unwrap();
    assert_eq!(first, again);
    assert_eq!(server.fetches, 1);
}
