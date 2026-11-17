//! The shell's holder of the gesture session: where the pad's records come from, and what frame they leave.
//!
//! `docs/design/juno-window-manager.md` section 4b puts the touchpad driver's recognised gestures on `/dev/lucina-gesture` as
//! whole 32-byte records, pollable and woken from the interrupt path, and section 6 says what three and four fingers do with
//! them. `lucina-wm` is the middle: [`lucina_wm::session::Session`] owns the rules' controller with the row of spaces and turns a
//! record into the controller's input, and [`lucina_wm::source`] reads records from the device or from a recording. This module is
//! the shell's side of that: it opens the source once, reads what is ready without ever waiting on the pad, hands the records to
//! the session, and says whether the frame moved, which is all the loop needs to decide to redraw. [`crate::wm_scene::scene_ops`]
//! then turns the frame into compositor ops.
//!
//! The reading is bounded twice over: a device source is read only when a zero-timeout `poll` says a record is waiting, and at
//! most [`MAX_RECORDS_PER_DRAIN`] records go through one [`GestureFeed::drain`]. So one loop turn never waits on the pad and never
//! drowns in it, which is what the 60 Hz budget of stage 4 (section 8) needs.
//!
//! Slice W wires this into the shell's loop; until then the binary calls none of it and the tests below call the rest.
#![allow(dead_code)] // GestureFeed::open has no caller in the loop yet: slice W adds the gesture device to its poll set

use std::io;
use std::os::fd::RawFd;

use lucina_wm::session::Session;
use lucina_wm::source::{DeviceSource, GestureSource};
use lucina_wm_rules::controller::Scene;
use lucina_wm_rules::spaces::Spaces;
use lucina_wm_rules::switcher::AppId;
use lucina_wm_rules::tile::Rect;

/// The node the touchpad driver feeds (section 4b). The shell is its only opener: a second open fails with `EBUSY`, which is
/// why nothing here looks for the device again once it has one.
const DEVICE_PATH: &str = "/dev/lucina-gesture";

/// Most records one [`drain`](GestureFeed::drain) reads. The kernel's queue holds 128 (`lucina_gesture::QUEUE_LEN`), so a pad
/// never reaches the cap; it is there for what is not a pad — a replay file, or a source that says it is ready forever — so one
/// loop turn cannot be held by a flood.
const MAX_RECORDS_PER_DRAIN: usize = 256;

/// The session, the source its records come from, and the frame they left.
pub struct GestureFeed {
    session: Session,
    source: Box<dyn GestureSource>,
    /// The screen the session was made on, kept because [`set_apps`](GestureFeed::set_apps) rebuilds the session on it.
    screen: Rect,
    /// The user's invert setting, kept for the same reason, and because `set_invert` is the only way to the session's copy.
    invert: bool,
    /// The instant of `last`, so that a rebuilt frame is the frame of the same instant and not of a made-up one.
    last_us: u64,
    /// The last frame built, by `drain` or by `scene`: what the next `drain` compares its frame with to say whether it moved.
    last: Scene,
}

impl GestureFeed {
    /// The feed of this machine's pad: `/dev/lucina-gesture`, and, when this build has the `gesture-replay` feature on, the
    /// recording `LUCINA_GESTURE_REPLAY` names if there is no device (section 4b; off by default, so a shipped image replays
    /// nothing). `None` when neither is there, or when what is there is not a stream this build reads — the shell then carries on
    /// without gestures, and the keys of section 6 still do everything the pad does. Nothing here waits or panics: a missing node
    /// is `None`, and a header of another version or record length is logged and `None`, as section 4b asks.
    pub fn open(screen: (u32, u32), invert: bool) -> Option<GestureFeed> {
        Some(GestureFeed::with_source(
            screen,
            invert,
            Self::machine_source()?,
        ))
    }

    /// A feed over a source of your own: the tests, and slice W for a recording whose path it chose itself rather than through
    /// the environment. The row holds no app until [`set_apps`](GestureFeed::set_apps) hands the shell's list, so the view starts
    /// on Home alone.
    pub fn with_source(
        screen: (u32, u32),
        invert: bool,
        source: Box<dyn GestureSource>,
    ) -> GestureFeed {
        let screen = Rect {
            x: 0,
            y: 0,
            w: screen.0 as i32,
            h: screen.1 as i32,
        };
        let mut session = Session::new(screen, &[]);
        session.set_invert(invert);
        let mut last = Scene::new();
        session.scene(0, &mut last);
        GestureFeed {
            session,
            source,
            screen,
            invert,
            last_us: 0,
            last,
        }
    }

    /// The descriptor to poll beside the shell's evdev ones, `None` when the source has none — a replay is ready until it ends,
    /// and `drain` reads it as though it had arrived. Poll it for reading; when it fires, call [`drain`](GestureFeed::drain).
    pub fn fd(&self) -> Option<RawFd> {
        self.source.raw_fd()
    }

    /// Reads what is ready and feeds it, and says whether the frame it leaves is not the frame before it, or is still moving
    /// (`animating`) and wants another frame. Both halves are what the loop needs: the first whether to redraw at all, the second
    /// whether to ask for the next vblank.
    ///
    /// Never waits: a source with a descriptor is read only once `poll` reports it readable, and a read that would block ends the
    /// drain. Ends too at the end of a replay, at a record the reader cannot decode, and at [`MAX_RECORDS_PER_DRAIN`] records.
    /// What is left unread waits for the next turn, and the session keeps the gesture that is open across the two, since a
    /// gesture is the records between its begin and its end, not one call.
    ///
    /// `now_us` is the clock the shell hands [`scene`](GestureFeed::scene): springs run on absolute time, so a drain that comes a
    /// frame late is not a frame wrong.
    pub fn drain(&mut self, now_us: u64) -> bool {
        let mut fed = 0;
        while fed < MAX_RECORDS_PER_DRAIN && self.ready() {
            match self.source.next() {
                Ok(Some(rec)) => {
                    self.session.feed(&rec);
                    fed += 1;
                }
                // The replay ran out, or the stream broke (a short read, a record that does not decode, or, on a descriptor that
                // lied about being ready, `WouldBlock`). Stop here: the loop keeps its rhythm and the session keeps its state.
                Ok(None) | Err(_) => break,
            }
        }
        if fed == 0 {
            // Nothing went in, so nothing moved except a spring that was already running, and that the frame says.
            return self.last.animating;
        }
        let mut frame = Scene::new();
        self.session.scene(now_us, &mut frame);
        let moved = frame != self.last || frame.animating;
        self.last = frame;
        self.last_us = now_us;
        moved
    }

    /// The frame for `now_us`, the session's own, into `out` — the `Scene` [`crate::wm_scene::scene_ops`] takes. Call it once per
    /// vblank while frames come back `animating`, and once after any other input (a key, `set_apps`), exactly as
    /// `lucina_wm::session::scene` asks; it is also what ends a settle, so skipping it leaves the view as it was.
    pub fn scene(&mut self, now_us: u64, out: &mut Scene) {
        self.session.scene(now_us, out);
        self.last_us = now_us;
        self.last = *out;
    }

    /// The shell's app list, in the order the row of spaces should show it: Home first, then one space per app, apps past the
    /// row's capacity getting none — the row is `lucina_wm::session::Session`'s to build and this hands it the list again.
    ///
    /// The session builds the row when it is made and gives no way to add, close or move a space afterwards, so a new list is a
    /// new session: the view comes back on the first app and a settle in flight is dropped. Call it when the list has changed and
    /// no finger is on the pad (slice W knows both), and draw the frame that follows as after any input. Keeping the space you
    /// were on across the change needs a session that can be updated in place, which is `lucina-wm`'s to grow, not this file's.
    pub fn set_apps(&mut self, apps: &[AppId]) {
        self.session = Session::new(self.screen, apps);
        self.session.set_invert(self.invert);
        // The frame the new row shows at the instant the loop last looked at one, so that a new list is not itself a change
        // `drain` reports: it answers for the records it read. Draw what follows as after any other input, with `scene`.
        let at = self.last_us;
        self.session.scene(at, &mut self.last);
    }

    /// The user's setting: natural or inverted swipes, which is the one place the shell reverses content-follows-fingers
    /// (section 4b keeps the driver's deltas raw). Kept here so a later `set_apps` rebuild does not lose it.
    pub fn set_invert(&mut self, invert: bool) {
        self.invert = invert;
        self.session.set_invert(invert);
    }

    /// The space you are looking at, for the dot row and for the shell's own bookkeeping.
    pub fn active(&self) -> usize {
        self.session.active()
    }

    /// The row of spaces, which is what maps a scene layer's `space` index to the app shown there, and so to its surface.
    pub fn spaces(&self) -> &Spaces {
        self.session.spaces()
    }

    /// Where this machine's records come from: the pad, else the recording the environment names, else nothing.
    fn machine_source() -> Option<Box<dyn GestureSource>> {
        match DeviceSource::open(DEVICE_PATH) {
            Ok(device) => Some(Box::new(device)),
            // No node at all: no pad on this machine, which is the usual case off Juno and the temporary case before stage 2.
            Err(e) if e.kind() == io::ErrorKind::NotFound => replay_source(),
            // The node is there but the stream is not one this build reads, or is not readable: refuse it loudly (section 4b)
            // rather than guess at a version, and go without gestures.
            Err(e) => {
                eprintln!("lucina-shell: {DEVICE_PATH} refused: {e}");
                None
            }
        }
    }

    /// Whether a read now would find a record. A source without a descriptor (a replay, an in-memory list) is ready until it
    /// says otherwise; a device only once `poll` reports it, so [`next`](GestureSource::next) cannot wait.
    fn ready(&self) -> bool {
        let Some(fd) = self.source.raw_fd() else {
            return true;
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is the one `pollfd` this descriptor owns, sized as `poll` expects, and a timeout of 0 waits for nothing;
        // `poll` writes only `revents` in it.
        let n = unsafe { libc::poll(&mut pfd, 1, 0) };
        n > 0 && pfd.revents & libc::POLLIN != 0
    }
}

/// The recording `LUCINA_GESTURE_REPLAY` names, when this build reads the environment at all. A feature for the bench and for
/// tuning the constants of section 6 from a recording, never for an image (section 4b), so the default build has no such path.
#[cfg(feature = "gesture-replay")]
fn replay_source() -> Option<Box<dyn GestureSource>> {
    match lucina_wm::source::ReplaySource::from_env() {
        Ok(Some(replay)) => Some(Box::new(replay)),
        // Nothing named: this machine simply has no gesture source.
        Ok(None) => None,
        Err(e) => {
            eprintln!("lucina-shell: LUCINA_GESTURE_REPLAY refused: {e}");
            None
        }
    }
}

#[cfg(not(feature = "gesture-replay"))]
fn replay_source() -> Option<Box<dyn GestureSource>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_gesture::{GestureRecord, Kind, Phase, SCALE_ONE};
    use lucina_wm::source::ReplaySource;
    use lucina_wm_rules::controller::Mode;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::rc::Rc;

    const SCREEN: (u32, u32) = (1280, 800);
    const SCREEN_RECT: Rect = Rect {
        x: 0,
        y: 0,
        w: 1280,
        h: 800,
    };
    /// Three apps, so the row is Home plus four spaces and the view starts on space 1 (what `Session::new` does).
    const APPS: [AppId; 3] = [AppId(1), AppId(2), AppId(3)];

    fn golden_bytes(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/../../drivers/lucina-gesture/tests/golden/{name}.lgst",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn golden_records(name: &str) -> Vec<GestureRecord> {
        let bytes = golden_bytes(name);
        let mut src = ReplaySource::unpaced(&bytes).expect("a golden stream this build reads");
        let mut v = Vec::new();
        while let Some(r) = src.next().unwrap() {
            v.push(r);
        }
        v
    }

    fn last_time(recs: &[GestureRecord]) -> u64 {
        recs.last().map_or(0, |r| r.time_us)
    }

    /// A feed that plays the recording `name`, over the apps `apps`.
    fn replay_feed(name: &str, apps: &[AppId], invert: bool) -> GestureFeed {
        let src =
            ReplaySource::unpaced(&golden_bytes(name)).expect("a golden stream this build reads");
        let mut feed = GestureFeed::with_source(SCREEN, invert, Box::new(src));
        feed.set_apps(apps);
        feed
    }

    /// A feed over records already in memory, with the counter the drain tests read.
    fn memory_feed(
        recs: &[GestureRecord],
        apps: &[AppId],
        invert: bool,
    ) -> (GestureFeed, Rc<RefCell<usize>>) {
        let taken = Rc::new(RefCell::new(0usize));
        let feed = GestureFeed::with_source(
            SCREEN,
            invert,
            Box::new(Memory {
                rest: recs.iter().copied().collect(),
                taken: taken.clone(),
            }),
        );
        let mut feed = feed;
        feed.set_apps(apps);
        (feed, taken)
    }

    /// Records drained, then frames 16 ms apart until the view rests; the feed and the frame it rests on.
    fn settle(mut feed: GestureFeed, from_us: u64) -> (GestureFeed, Scene) {
        feed.drain(from_us);
        let mut scene = Scene::new();
        let mut t = from_us;
        loop {
            t += 16_000;
            feed.scene(t, &mut scene);
            assert!(
                t <= from_us + 3_000_000,
                "still moving 3 s after the last record"
            );
            if !scene.animating {
                break;
            }
        }
        (feed, scene)
    }

    /// The golden `name` played through a feed of `apps` and `invert`, come to rest.
    fn run(name: &str, apps: &[AppId], invert: bool) -> (GestureFeed, Scene) {
        let recs = golden_records(name);
        settle(replay_feed(name, apps, invert), last_time(&recs))
    }

    /// A 3-finger swipe of `dx` per update over `updates` records, begun at `t`: begin, updates, end.
    fn swipe(t: u64, dx: i32, updates: usize) -> Vec<GestureRecord> {
        let mut v = Vec::with_capacity(updates + 2);
        for i in 0..updates + 2 {
            let phase = match i {
                0 => Phase::Begin,
                _ if i == updates + 1 => Phase::End,
                _ => Phase::Update,
            };
            v.push(GestureRecord {
                time_us: t + (i as u64) * 8_000,
                kind: Kind::Swipe,
                phase,
                fingers: 3,
                flags: 0,
                dx: if phase == Phase::Update { dx } else { 0 },
                dy: 0,
                scale: SCALE_ONE,
                angle: 0,
                seq: 1,
            });
        }
        v
    }

    /// A source of records that are already in memory: no descriptor, so a drain reads until the queue is empty.
    struct Memory {
        rest: VecDeque<GestureRecord>,
        taken: Rc<RefCell<usize>>,
    }

    impl GestureSource for Memory {
        fn next(&mut self) -> io::Result<Option<GestureRecord>> {
            match self.rest.pop_front() {
                Some(rec) => {
                    *self.taken.borrow_mut() += 1;
                    Ok(Some(rec))
                }
                None => Ok(None),
            }
        }

        fn raw_fd(&self) -> Option<RawFd> {
            None
        }
    }

    /// A source whose read would block: what the device says between two records.
    struct Stalled;

    impl GestureSource for Stalled {
        fn next(&mut self) -> io::Result<Option<GestureRecord>> {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }

        fn raw_fd(&self) -> Option<RawFd> {
            None
        }
    }

    #[test]
    fn swipe3_left_moves_one_space() {
        let (feed, scene) = run("swipe3-left", &APPS, false);
        assert_eq!(feed.active(), 2, "one space to the right of the start");
        assert_eq!(scene.mode, Mode::Spaces);
        assert_eq!(scene.len, 1, "at rest the frame is the one space you see");
    }

    #[test]
    fn swipe3_up_opens_the_overview() {
        let (feed, scene) = run("swipe3-up", &APPS, false);
        assert_eq!(scene.mode, Mode::Overview);
        assert_eq!(
            feed.active(),
            1,
            "the overview recentres, it does not switch"
        );
        assert_eq!(
            scene.len,
            feed.spaces().len(),
            "every space of the row is a layer"
        );
    }

    #[test]
    fn a_cancelled_gesture_returns_to_where_it_began() {
        // `swipe3-to-4` is a 3-finger swipe whose finger count changes on the pad: the driver closes that gesture cancelled and
        // opens a 4-finger one in its place (section 4b). Only the records up to the cancelled end are the first gesture.
        let all = golden_records("swipe3-to-4");
        let cut = all
            .iter()
            .position(|r| r.cancelled())
            .expect("the recording holds a cancelled end");
        let part = &all[..=cut];
        let (feed, taken) = memory_feed(part, &APPS, false);
        let (feed, scene) = settle(feed, last_time(part));
        assert_eq!(
            *taken.borrow(),
            part.len(),
            "the whole first gesture went in"
        );
        assert_eq!(feed.active(), 1, "back at the space the gesture began on");
        assert_eq!(scene.mode, Mode::Spaces);
        assert!(!scene.animating, "the spring home is over");

        // What follows is a gesture of its own, and the cancel does not carry into it: the 4-finger swipe commits.
        let (whole, scene) = run("swipe3-to-4", &APPS, false);
        assert_ne!(whole.active(), 1, "the 4-finger gesture moved the view");
        assert_eq!(scene.mode, Mode::Spaces);
        assert_eq!(scene.len, 1);
    }

    #[test]
    fn drain_reads_at_most_256_records() {
        let mut flood = Vec::new();
        for _ in 0..7 {
            flood.extend(golden_records("swipe3-left"));
        }
        assert_eq!(flood.len(), 259);
        let (mut feed, taken) = memory_feed(&flood, &APPS, false);
        feed.drain(last_time(&flood));
        assert_eq!(*taken.borrow(), MAX_RECORDS_PER_DRAIN, "one call's worth");
        feed.drain(last_time(&flood) + 8_000);
        assert_eq!(
            *taken.borrow(),
            flood.len(),
            "the rest comes on the next turn"
        );
        feed.drain(last_time(&flood) + 16_000);
        assert_eq!(
            *taken.borrow(),
            flood.len(),
            "and an empty source reads nothing"
        );
    }

    #[test]
    fn no_device_and_no_replay_gives_no_feed() {
        if Path::new(DEVICE_PATH).exists() || std::env::var_os("LUCINA_GESTURE_REPLAY").is_some() {
            // Something to open: a real Juno pad, or a recording this build was told to replay. Which is the other test's subject.
            return;
        }
        assert!(GestureFeed::open(SCREEN, false).is_none());
    }

    #[test]
    fn set_apps_keeps_the_space_row_in_sync() {
        let feed = replay_feed("swipe3-left", &[], false);
        assert_eq!(feed.spaces().len(), 1, "Home alone with no app");
        let (feed, scene) = settle(feed, last_time(&golden_records("swipe3-left")));
        assert_eq!(feed.active(), 0, "a swipe has nowhere to go");
        assert_eq!(scene.len, 1);

        let mut feed = replay_feed("swipe3-left", &[], false);
        feed.set_apps(&APPS);
        assert_eq!(feed.spaces().len(), 4, "Home plus one per app");
        assert_eq!(feed.active(), 1, "the view starts on the first app");
        let mut scene = Scene::new();
        feed.scene(0, &mut scene);
        assert_eq!(scene.mode, Mode::Spaces);
        assert_eq!(
            scene.len, 1,
            "the frame holds a space of the new row, not the old one"
        );
    }

    #[test]
    fn the_frame_is_the_controller_s_own() {
        // The feed must add no behaviour of its own: the same records through the same session give the same frame.
        let recs = golden_records("swipe3-up");
        let t = last_time(&recs);
        let mut feed = replay_feed("swipe3-up", &APPS, false);
        let mut hand = Session::new(SCREEN_RECT, &APPS);
        assert!(feed.drain(t));
        let mut a = Scene::new();
        let mut b = Scene::new();
        for i in 0..400 {
            let now = t + i * 16_000;
            feed.scene(now, &mut a);
            if i == 0 {
                for r in &recs {
                    hand.feed(r);
                }
            }
            hand.scene(now, &mut b);
            assert_eq!(a, b, "the frame at {now}");
            if !a.animating && i > 0 {
                break;
            }
        }
        assert_eq!(feed.active(), hand.active());
        assert_eq!(a.mode, Mode::Overview);
    }

    #[test]
    fn a_device_file_reads_like_a_replay() {
        let path = std::env::temp_dir().join(format!(
            "lucina-shell-{}-swipe3-left.lgst",
            std::process::id()
        ));
        std::fs::write(&path, golden_bytes("swipe3-left")).unwrap();
        let device = DeviceSource::open(&path).unwrap();
        let mut feed = GestureFeed::with_source(SCREEN, false, Box::new(device));
        feed.set_apps(&APPS);
        assert!(feed.fd().is_some(), "the device has a descriptor to poll");
        let t = last_time(&golden_records("swipe3-left"));
        let (feed, _) = settle(feed, t);
        assert_eq!(feed.active(), 2, "the same records through the same drain");
        std::fs::remove_file(&path).unwrap();

        let replay = replay_feed("swipe3-left", &APPS, false);
        assert_eq!(replay.fd(), None, "a replay has nothing to poll");
    }

    #[test]
    fn drain_says_whether_the_frame_moved() {
        let pinch = golden_records("pinch2-in");
        let (mut feed, _) = memory_feed(&pinch, &APPS, false);
        assert!(
            !feed.drain(last_time(&pinch)),
            "a pinch is not the controller's business (section 6)"
        );

        // A swipe that drags the view: true while the frame is off its rest, and false once the read is empty and the spring is done.
        let recs = swipe(1_000_000, -8_000, 4);
        let (mut feed, taken) = memory_feed(&recs, &APPS, false);
        assert!(
            feed.drain(last_time(&recs)),
            "the spaces shifted under the fingers"
        );
        let mut scene = Scene::new();
        let mut t = last_time(&recs);
        loop {
            t += 16_000;
            feed.scene(t, &mut scene);
            assert!(t <= last_time(&recs) + 3_000_000, "still moving");
            if !scene.animating {
                break;
            }
        }
        assert!(
            !feed.drain(t),
            "nothing left to read and nothing left moving"
        );
        assert_eq!(
            *taken.borrow(),
            recs.len(),
            "and the empty read took nothing"
        );
    }

    #[test]
    fn invert_moves_the_other_way() {
        let (plain, _) = run("swipe3-left", &APPS, false);
        assert_eq!(plain.active(), 2);
        let (backwards, _) = run("swipe3-left", &APPS, true);
        assert_eq!(
            backwards.active(),
            0,
            "the same finger motion, the other way"
        );

        // The setting changed at runtime reaches the session the same way the one `open` was given does.
        let mut also = replay_feed("swipe3-left", &APPS, false);
        also.set_invert(true);
        let t = last_time(&golden_records("swipe3-left"));
        also.drain(t);
        let (also, _) = settle(also, t);
        assert_eq!(
            also.active(),
            backwards.active(),
            "set_invert is open's flag"
        );
    }

    #[test]
    fn records_that_are_not_the_controller_s_change_nothing() {
        for name in ["scroll2-down", "pointer1", "hold3-lift", "pinch4-out"] {
            let (feed, scene) = run(name, &APPS, false);
            assert_eq!(feed.active(), 1, "{name} switched a space");
            assert_eq!(scene.mode, Mode::Spaces, "{name} opened the overview");
            assert_eq!(scene.len, 1, "{name} left more than one layer");
        }
    }

    #[test]
    fn a_source_that_hands_nothing_over_moves_nothing() {
        let (mut feed, taken) = memory_feed(&[], &APPS, false);
        assert!(!feed.drain(0), "an empty source at rest");
        assert_eq!(*taken.borrow(), 0);

        // A read that would block ends the drain where the ready records were, and the session stands as it was: this is the
        // shape of the device between two records, and the drain must not wait for it.
        let mut feed = GestureFeed::with_source(SCREEN, false, Box::new(Stalled));
        feed.set_apps(&APPS);
        assert_eq!(feed.fd(), None);
        assert!(!feed.drain(5_000_000), "a blocked read is not a frame");
        let mut scene = Scene::new();
        feed.scene(5_000_000, &mut scene);
        assert_eq!(scene.len, 1);
        assert_eq!(scene.mode, Mode::Spaces);
        assert!(!scene.animating);
    }

    /// That a recording named by `LUCINA_GESTURE_REPLAY` stands in for a missing pad. The branch is compiled only with the
    /// `gesture-replay` feature, so this test runs only under `cargo test -p lucina-shell --features gesture-replay`.
    #[cfg(feature = "gesture-replay")]
    #[test]
    fn a_replay_in_the_env_stands_in_for_the_pad() {
        const MARKER: &str = "LUCINA_GESTURE_FEED_CHILD";
        if std::env::var_os(MARKER).is_none() {
            if Path::new(DEVICE_PATH).exists() {
                return; // the pad wins over the environment, which is the order `open` documents
            }
            // A child, because the environment of a test process is shared with every other test's.
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("gesture_feed::tests::a_replay_in_the_env_stands_in_for_the_pad")
                .env(MARKER, "1")
                .env(
                    "LUCINA_GESTURE_REPLAY",
                    format!(
                        "{}/../../drivers/lucina-gesture/tests/golden/swipe3-left.lgst",
                        env!("CARGO_MANIFEST_DIR")
                    ),
                )
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "the child said: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let feed =
            GestureFeed::open(SCREEN, false).expect("the named replay stands in for the pad");
        assert_eq!(feed.fd(), None, "a replay has nothing to poll");
    }
}
