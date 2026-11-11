//! Replays: input reports in the Book Go pad's own layout go through `ptp::Contacts`, `Touchpad` and `GestureEncoder` as the
//! kernel's loop runs them, and the records that come out are compared with the golden streams in
//! `drivers/lucina-gesture/tests/golden/` (`<name>.lgst`, the stream bytes, and `<name>.ptr`, the same reports as
//! `/dev/lucina-hid-test` lines, so that the laptop and QEMU can replay the same input).
//!
//! `UPDATE_GOLDEN=1 cargo test -p lucina-i2c-hid replay` writes both files; without it any difference fails.

extern crate std;

use crate::gesture_out::GestureEncoder;
use crate::ptp::tests::{format as pad_format, report};
use crate::ptp::{Contacts, Frame};
use crate::touchpad::{Config, Output, Touchpad};
use lucina_gesture::rules::{self, Op, RuleKind, RulePhase};
use lucina_gesture::{
    FLAG_CANCELLED, FLAG_NATURAL, GestureRecord, Kind, Phase, SCALE_ONE, StreamReader, from_q16,
    rebase_to_zero, write_stream,
};
use std::fmt::Write as _;
use std::format;
use std::string::String;
use std::vec;
use std::vec::Vec;

const FRAME_US: u64 = 8_000;
const START_US: u64 = 1_000_000;

/// One finger of a frame: contact ID (0 to 4), x, y.
type Finger = (u8, u16, u16);

/// The kernel's loop on a recorded pad: reports in, time advanced to every deadline between them.
struct Replay {
    contacts: Contacts,
    pad: Touchpad,
    enc: Option<GestureEncoder>,
    now: u64,
    /// The contacts of the previous frame, so that a lifted one is reported once with its tip up.
    down: Vec<Finger>,
    reports: Vec<Vec<u8>>,
    records: Vec<GestureRecord>,
    outputs: Vec<Output>,
}

impl Replay {
    fn new(encode: bool) -> Replay {
        Replay {
            contacts: Contacts::new(pad_format(), 5),
            pad: Touchpad::new(1876, 1164, 16, 16, 5, Config::juno()),
            enc: encode.then(GestureEncoder::new),
            now: START_US,
            down: Vec::new(),
            reports: Vec::new(),
            records: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// One Output of the engine: kept, and a gesture also encoded.
    fn sink(
        outputs: &mut Vec<Output>,
        enc: &mut Option<GestureEncoder>,
        records: &mut Vec<GestureRecord>,
        now: u64,
        o: Output,
    ) {
        outputs.push(o);
        if let (Output::Gesture(g), Some(enc)) = (o, enc.as_mut()) {
            enc.feed(&g, now, Config::juno().natural_scroll, &mut |r| {
                records.push(r)
            });
        }
    }

    /// Lets time pass to `target`, calling the engine's and the contacts' timers at each deadline on the way.
    fn advance_to(&mut self, target: u64) {
        for _ in 0..100_000 {
            let due = [self.pad.deadline(), self.contacts.sticky_deadline()]
                .into_iter()
                .flatten()
                .min();
            match due {
                Some(d) if d <= target => {
                    self.now = self.now.max(d);
                    let now = self.now;
                    let Replay {
                        contacts,
                        pad,
                        enc,
                        outputs,
                        records,
                        ..
                    } = self;
                    contacts.tick(
                        now,
                        &mut |f: &Frame| {
                            pad.frame(f, now, &mut |o| Replay::sink(outputs, enc, records, now, o))
                        },
                        &mut |_| {},
                    );
                    pad.tick(now, &mut |o| Replay::sink(outputs, enc, records, now, o));
                    // a deadline the tick did not consume would loop for ever: step past it
                    if [pad.deadline(), contacts.sticky_deadline()]
                        .into_iter()
                        .flatten()
                        .min()
                        == Some(d)
                    {
                        self.now += 1;
                    }
                }
                _ => break,
            }
        }
        self.now = self.now.max(target);
    }

    /// One 8 ms frame with these fingers down (and the ones that were down and are not now lifted): the reports of the
    /// hybrid mode, one contact each, the contact count in the first.
    fn frame(&mut self, fingers: &[Finger]) {
        self.advance_to(self.now + FRAME_US);
        let mut contacts: Vec<(Finger, bool)> = fingers.iter().map(|&f| (f, true)).collect();
        for &d in &self.down {
            if !fingers.iter().any(|f| f.0 == d.0) {
                contacts.push((d, false));
            }
        }
        let scan = (self.now / 100) as u16;
        for (i, &((id, x, y), tip)) in contacts.iter().enumerate() {
            let count = if i == 0 { contacts.len() as u8 } else { 0 };
            let rep = report(tip, true, id, x, y, scan, count, false).to_vec();
            let now = self.now;
            let Replay {
                contacts: c,
                pad,
                enc,
                outputs,
                records,
                ..
            } = self;
            c.report(
                &rep,
                now,
                &mut |f: &Frame| {
                    pad.frame(f, now, &mut |o| Replay::sink(outputs, enc, records, now, o))
                },
                &mut |_| {},
            );
            self.reports.push(rep);
        }
        self.down = fingers.to_vec();
    }

    /// The fingers stay where they are for `us`, reports every 8 ms as a real pad sends them.
    fn rest(&mut self, fingers: &[Finger], us: u64) {
        for _ in 0..us / FRAME_US {
            self.frame(fingers);
        }
    }

    /// No finger on the pad for `us`.
    fn wait(&mut self, us: u64) {
        self.advance_to(self.now + us);
    }
}

fn row(n: u8, x: i32, y: i32, gap_x: i32, gap_y: i32) -> Vec<Finger> {
    (0..n)
        .map(|i| {
            (
                i,
                (x + i32::from(i) * gap_x) as u16,
                (y + i32::from(i) * gap_y) as u16,
            )
        })
        .collect()
}

/// (cos, sin) through a Taylor series: enough for these angles, and no libm.
fn cos_sin(x: f64) -> (f64, f64) {
    let (mut c, mut s, mut tc, mut ts) = (1.0, x, 1.0, x);
    for n in 1..20 {
        let n = f64::from(n);
        tc *= -x * x / ((2.0 * n - 1.0) * (2.0 * n));
        ts *= -x * x / ((2.0 * n) * (2.0 * n + 1.0));
        c += tc;
        s += ts;
    }
    (c, s)
}

fn swipe(r: &mut Replay, n: u8, frames: i32, step: (i32, i32)) {
    // Start far enough from the edges that the whole move stays on the pad.
    let (x0, y0) = (700 - step.0 * frames / 2, 600 - step.1 * frames / 2);
    for i in 0..frames {
        r.frame(&row(n, x0 + step.0 * i, y0 + step.1 * i, 200, 0));
    }
    r.frame(&[]);
    r.wait(400_000);
}

/// `n` fingers on a ring of radius `radius(i)` around (950, 600).
fn pinch(r: &mut Replay, n: u8, frames: i32, radius: impl Fn(i32) -> i32) {
    for i in 0..frames {
        let rad = radius(i);
        let fingers: Vec<Finger> = (0..n)
            .map(|k| {
                let th = f64::from(k) * 2.0 * core::f64::consts::PI / f64::from(n);
                let (c, s) = cos_sin(th);
                (
                    k,
                    (950 + (f64::from(rad) * c).round() as i32) as u16,
                    (600 + (f64::from(rad) * s).round() as i32) as u16,
                )
            })
            .collect();
        r.frame(&fingers);
    }
    r.frame(&[]);
    r.wait(400_000);
}

const ROTATE_FRAMES: i32 = 20;

struct Scenario {
    name: &'static str,
    run: fn(&mut Replay),
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "swipe3-left",
        run: |r| swipe(r, 3, 40, (-8, 0)),
    },
    Scenario {
        name: "swipe3-right",
        run: |r| swipe(r, 3, 40, (8, 0)),
    },
    Scenario {
        name: "swipe3-up",
        run: |r| swipe(r, 3, 40, (0, -8)),
    },
    Scenario {
        name: "swipe3-down",
        run: |r| swipe(r, 3, 40, (0, 8)),
    },
    Scenario {
        name: "swipe4-up",
        run: |r| swipe(r, 4, 30, (0, -8)),
    },
    Scenario {
        name: "swipe4-down",
        run: |r| swipe(r, 4, 30, (0, 8)),
    },
    Scenario {
        name: "pinch2-out",
        run: |r| {
            for i in 0..20 {
                r.frame(&[
                    (0, (900 - i * 12) as u16, 500),
                    (1, (1000 + i * 12) as u16, 500),
                ]);
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "pinch2-in",
        run: |r| {
            for i in 0..20 {
                r.frame(&[
                    (0, (600 + i * 12) as u16, 500),
                    (1, (1300 - i * 12) as u16, 500),
                ]);
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "pinch4-in",
        run: |r| pinch(r, 4, 24, |i| 420 - i * 12),
    },
    Scenario {
        name: "pinch4-out",
        run: |r| pinch(r, 4, 24, |i| 120 + i * 12),
    },
    Scenario {
        name: "pinch2-rotate",
        run: |r| {
            // 2 degrees a frame about (950, 600), 300 units (18.75 mm) from the centre.
            for i in 0..ROTATE_FRAMES {
                let th = f64::from(i) * 2.0 * core::f64::consts::PI / 180.0;
                let (c, s) = cos_sin(th);
                let (dx, dy) = ((300.0 * c).round() as i32, (300.0 * s).round() as i32);
                r.frame(&[
                    (0, (950 + dx) as u16, (600 + dy) as u16),
                    (1, (950 - dx) as u16, (600 - dy) as u16),
                ]);
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "hold3-lift",
        run: |r| {
            // With tapping on, three fingers hold when the tap machine times out, 180 ms.
            let f = row(3, 600, 600, 300, 0);
            r.rest(&f, 400_000);
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "swipe3-to-4",
        run: |r| {
            let mut x = 1400;
            for _ in 0..30 {
                r.frame(&row(3, x, 500, 200, 0));
                x -= 8;
            }
            // a fourth finger: the count change waits for the 100 ms switch timeout, then a new gesture begins
            for _ in 0..40 {
                r.frame(&row(4, x, 500, 200, 0));
                x -= 8;
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "scroll2-down",
        run: |r| {
            for i in 0..20 {
                let y = (300 + i * 32) as u16;
                r.frame(&[(0, 700, y), (1, 1100, y)]);
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
    Scenario {
        name: "pointer1",
        run: |r| {
            for i in 0..30 {
                r.frame(&[(0, (600 + i * 8) as u16, 500)]);
            }
            r.frame(&[]);
            r.wait(400_000);
        },
    },
];

fn scenario(name: &str) -> &'static Scenario {
    SCENARIOS.iter().find(|s| s.name == name).unwrap()
}

fn replay(name: &str) -> Replay {
    let mut r = Replay::new(true);
    (scenario(name).run)(&mut r);
    r
}

fn stream_bytes(records: &[GestureRecord]) -> Vec<u8> {
    let mut b = vec![0u8; 32 + 32 * records.len()];
    assert_eq!(write_stream(records, &mut b), Some(b.len()));
    b
}

fn ptr_text(reports: &[Vec<u8>]) -> String {
    let mut s = String::new();
    for r in reports {
        s.push_str("ptr ");
        for b in r {
            write!(s, "{b:02x}").unwrap();
        }
        s.push('\n');
    }
    s
}

fn golden_dir() -> String {
    format!(
        "{}/../../drivers/lucina-gesture/tests/golden",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// The rebased records of a scenario, and what the golden files must hold.
fn rebased(r: &Replay) -> Vec<GestureRecord> {
    let mut rs = r.records.clone();
    rebase_to_zero(&mut rs);
    rs
}

fn decode(bytes: &[u8]) -> Vec<GestureRecord> {
    StreamReader::new(bytes)
        .expect("header")
        .map(|r| r.expect("record"))
        .collect()
}

/// The stream passes `encode_step`'s rules: replayed through it, every record is what it would write, nothing is left open,
/// numbers are one more per gesture, and the numbers in the record are the kind's.
fn check_rules(name: &str, rs: &[GestureRecord]) {
    let mut open = None;
    let mut next_seq = rs.first().map_or(0, |r| r.seq);
    let mut times = 0;
    for (i, r) in rs.iter().enumerate() {
        let kind = match r.kind {
            Kind::Swipe => RuleKind::Swipe,
            Kind::Pinch => RuleKind::Pinch,
            Kind::Hold => RuleKind::Hold,
        };
        let ev = match r.phase {
            Phase::Begin => rules::begin(kind, r.fingers),
            Phase::Update => rules::update(kind),
            Phase::End => rules::end(kind, r.cancelled()),
        };
        let s = rules::encode_step(open, next_seq, ev);
        // The one record this notification writes (a begin with a gesture open would write an end first: not in a stream).
        let e = if ev.op == Op::Begin {
            s.second
        } else {
            s.first
        };
        assert!(
            s.first.is_none() || ev.op != Op::Begin,
            "{name} #{i}: begin over an open gesture"
        );
        let e = e.unwrap_or_else(|| {
            panic!("{name} #{i}: {r:?} is not what the rules write (open {open:?})")
        });
        assert_eq!(
            (e.kind, e.fingers, e.seq, e.cancelled),
            (kind, r.fingers, r.seq, r.cancelled()),
            "{name} #{i}"
        );
        assert_eq!(
            e.phase,
            match r.phase {
                Phase::Begin => RulePhase::Begin,
                Phase::Update => RulePhase::Update,
                Phase::End => RulePhase::End,
            },
            "{name} #{i}"
        );
        open = s.open;
        next_seq = s.next_seq;
        assert!(r.time_us >= times, "{name} #{i}: time goes back");
        times = r.time_us;
        assert_eq!(
            r.flags & FLAG_NATURAL,
            FLAG_NATURAL,
            "{name} #{i}: natural scrolling is on"
        );
        assert_eq!(
            r.flags & !(FLAG_NATURAL | FLAG_CANCELLED),
            0,
            "{name} #{i}: stray flags"
        );
        if r.phase != Phase::End {
            assert!(!r.cancelled(), "{name} #{i}: cancelled on a non-end");
        }
        if r.phase != Phase::Update {
            assert_eq!(
                (r.dx, r.dy, r.angle),
                (0, 0, 0),
                "{name} #{i}: begin and end carry no motion"
            );
        }
        if r.kind != Kind::Pinch {
            assert_eq!(
                (r.scale, r.angle),
                (SCALE_ONE, 0),
                "{name} #{i}: scale and angle are a pinch's"
            );
        }
        if r.kind == Kind::Pinch && r.phase == Phase::Begin {
            assert_eq!(r.scale, SCALE_ONE, "{name} #{i}");
        }
    }
    assert!(
        open.is_none(),
        "{name}: the stream ends with a gesture open"
    );
}

fn sum(rs: &[GestureRecord], f: impl Fn(&GestureRecord) -> i64) -> i64 {
    rs.iter().map(f).sum()
}

fn kinds(rs: &[GestureRecord]) -> Vec<(Kind, Phase, u8, bool)> {
    rs.iter()
        .map(|r| (r.kind, r.phase, r.fingers, r.cancelled()))
        .collect()
}

fn begins(rs: &[GestureRecord]) -> Vec<(Kind, u8)> {
    rs.iter()
        .filter(|r| r.phase == Phase::Begin)
        .map(|r| (r.kind, r.fingers))
        .collect()
}

fn check_scenario(name: &str, r: &Replay) {
    let rs = rebased(r);
    check_rules(name, &rs);
    let motion_x = sum(&rs, |r| i64::from(r.dx));
    let motion_y = sum(&rs, |r| i64::from(r.dy));
    let last_scale = rs
        .iter()
        .rev()
        .find(|r| r.phase == Phase::End)
        .map(|r| r.scale);
    match name {
        "swipe3-left" | "swipe3-right" | "swipe3-up" | "swipe3-down" | "swipe4-up"
        | "swipe4-down" => {
            let n = if name.starts_with("swipe3") { 3 } else { 4 };
            assert_eq!(begins(&rs), [(Kind::Swipe, n)], "{name}");
            assert!(
                rs.iter().all(|r| r.kind == Kind::Swipe && r.fingers == n),
                "{name}"
            );
            let last = rs.last().unwrap();
            assert_eq!(
                (last.phase, last.cancelled()),
                (Phase::End, false),
                "{name}"
            );
            // dx and dy are the fingers' direction, not the content's
            match name {
                "swipe3-left" => assert!(
                    motion_x < 0 && motion_y == 0,
                    "{name}: {motion_x} {motion_y}"
                ),
                "swipe3-right" => assert!(
                    motion_x > 0 && motion_y == 0,
                    "{name}: {motion_x} {motion_y}"
                ),
                "swipe3-up" | "swipe4-up" => assert!(
                    motion_y < 0 && motion_x == 0,
                    "{name}: {motion_x} {motion_y}"
                ),
                _ => assert!(
                    motion_y > 0 && motion_x == 0,
                    "{name}: {motion_x} {motion_y}"
                ),
            }
        }
        "pinch2-out" | "pinch2-in" | "pinch4-out" | "pinch4-in" => {
            let n = if name.starts_with("pinch2") { 2 } else { 4 };
            assert_eq!(begins(&rs), [(Kind::Pinch, n)], "{name}: {:?}", kinds(&rs));
            let scale = from_q16(last_scale.unwrap());
            if name.ends_with("out") {
                assert!(scale > 1.0, "{name}: ends at {scale}");
            } else {
                assert!(scale < 1.0, "{name}: ends at {scale}");
            }
            let end = rs.last().unwrap();
            assert_eq!((end.phase, end.cancelled()), (Phase::End, false), "{name}");
        }
        "pinch2-rotate" => {
            let begin = rs
                .iter()
                .position(|r| r.phase == Phase::Begin && r.kind == Kind::Pinch)
                .expect("a pinch");
            assert!(rs.iter().all(|r| r.kind == Kind::Pinch));
            // the frame of the begin: the first record's time is the origin, and the record times are the unrebased ones' offsets
            let first_frame = START_US + FRAME_US;
            let begin_frame = ((r.records[begin].time_us - first_frame) / FRAME_US) as i32;
            let angle: f64 = rs.iter().map(|r| from_q16(r.angle)).sum();
            let want = 2.0 * f64::from(ROTATE_FRAMES - 1 - begin_frame);
            assert!(
                (angle - want).abs() < 1.0,
                "{name}: {angle} vs {want} degrees"
            );
        }
        "hold3-lift" => {
            assert_eq!(
                kinds(&rs),
                [
                    (Kind::Hold, Phase::Begin, 3, false),
                    (Kind::Hold, Phase::End, 3, false)
                ],
                "{name}"
            );
        }
        "swipe3-to-4" => {
            let k = kinds(&rs);
            assert_eq!(
                begins(&rs),
                [(Kind::Swipe, 3), (Kind::Swipe, 4)],
                "{name}: {k:?}"
            );
            let first_end = k.iter().position(|e| e.1 == Phase::End).unwrap();
            assert_eq!(
                k[first_end],
                (Kind::Swipe, Phase::End, 3, true),
                "{name}: the 3-finger swipe ends cancelled"
            );
            assert_eq!(
                rs[first_end].seq + 1,
                rs.last().unwrap().seq,
                "{name}: the next gesture has the next number"
            );
            let last = rs.last().unwrap();
            assert_eq!(
                (last.phase, last.fingers, last.cancelled()),
                (Phase::End, 4, false),
                "{name}"
            );
        }
        "scroll2-down" => {
            assert!(
                rs.iter().all(|r| r.kind == Kind::Hold),
                "{name}: no swipe or pinch: {:?}",
                kinds(&rs)
            );
            let scrolled = r.outputs.iter().any(|o| matches!(o, Output::Scroll { .. }));
            assert!(scrolled, "{name}: the scroll is still a scroll");
        }
        "pointer1" => {
            assert!(
                rs.iter().all(|r| r.kind == Kind::Hold),
                "{name}: {:?}",
                kinds(&rs)
            );
            assert!(
                r.outputs.iter().any(|o| matches!(o, Output::Motion { .. })),
                "{name}"
            );
        }
        _ => panic!("no check for {name}"),
    }
}

#[test]
fn every_scenario_is_golden_and_right() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).unwrap();
    for s in SCENARIOS {
        let r = replay(s.name);
        let rs = rebased(&r);
        check_scenario(s.name, &r);
        let (lgst, ptr) = (stream_bytes(&rs), ptr_text(&r.reports));
        let (lgst_path, ptr_path) = (
            format!("{dir}/{}.lgst", s.name),
            format!("{dir}/{}.ptr", s.name),
        );
        if update {
            std::fs::write(&lgst_path, &lgst).unwrap();
            std::fs::write(&ptr_path, &ptr).unwrap();
            std::eprintln!(
                "golden {}: {} records, {} reports",
                s.name,
                rs.len(),
                r.reports.len()
            );
            continue;
        }
        let want = std::fs::read(&lgst_path)
            .unwrap_or_else(|e| panic!("{lgst_path}: {e} (UPDATE_GOLDEN=1 writes it)"));
        assert!(
            want == lgst,
            "{}: the records differ from {lgst_path}; decoded now {:?} golden {:?}",
            s.name,
            rs,
            decode(&want)
        );
        let want = std::fs::read_to_string(&ptr_path).unwrap_or_else(|e| panic!("{ptr_path}: {e}"));
        assert!(
            want == ptr,
            "{}: the reports differ from {ptr_path}",
            s.name
        );
    }
}

/// The golden stream, read back, is what a run produces: the file is a stream a reader can take.
#[test]
fn golden_streams_decode_to_the_replay() {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        return;
    }
    for s in SCENARIOS {
        let bytes = std::fs::read(format!("{}/{}.lgst", golden_dir(), s.name)).unwrap();
        assert_eq!(decode(&bytes), rebased(&replay(s.name)), "{}", s.name);
    }
}

/// The gesture records are a side channel: with the encoder on or off the pad's pointer, scroll and button outputs are the
/// same, and (for the two scenarios that are not gestures) there is nothing else in them but holds.
#[test]
fn scroll_and_pointer_outputs_are_the_engines_alone() {
    for name in ["scroll2-down", "pointer1"] {
        let with = replay(name);
        let mut alone = Replay::new(false);
        (scenario(name).run)(&mut alone);
        let strip = |o: &[Output]| {
            o.iter()
                .copied()
                .filter(|o| !matches!(o, Output::Gesture(_)))
                .collect::<Vec<_>>()
        };
        let (a, b) = (strip(&with.outputs), strip(&alone.outputs));
        assert!(!a.is_empty(), "{name}");
        assert_eq!(a, b, "{name}");
        assert!(alone.records.is_empty());
        // and the gesture outputs themselves are the same too
        assert_eq!(with.outputs, alone.outputs, "{name}");
    }
}

/// A reset or a suspend with a gesture open ends it, cancelled, once.
#[test]
fn a_reset_closes_the_open_gesture() {
    let mut r = Replay::new(true);
    for i in 0..20 {
        r.frame(&row(3, 1200 - i * 8, 500, 200, 0));
    }
    assert!(
        r.records.last().is_some_and(|x| x.phase == Phase::Update),
        "mid-swipe"
    );
    let now = r.now;
    let mut outs: Vec<Output> = Vec::new();
    r.pad.cancel_gestures(now, &mut |o| outs.push(o));
    let mut recs = Vec::new();
    for o in &outs {
        if let Output::Gesture(g) = o {
            let enc = r.enc.as_mut().unwrap();
            enc.feed(g, now, true, &mut |x| recs.push(x));
        }
    }
    let enc = r.enc.as_mut().unwrap();
    assert!(!enc.is_open());
    enc.reset(now, true, &mut |x| recs.push(x));
    let ends: Vec<_> = recs
        .iter()
        .map(|x| (x.kind, x.phase, x.cancelled()))
        .collect();
    assert_eq!(ends, [(Kind::Swipe, Phase::End, true)], "{outs:?}");
    // nothing open: the reset adds nothing, and a second cancel nothing either
    let mut again = Vec::new();
    r.pad.cancel_gestures(now, &mut |o| again.push(o));
    assert!(
        again.iter().all(|o| !matches!(o, Output::Gesture(_))),
        "{again:?}"
    );
}
