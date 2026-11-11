//! The engine on frames laid out as the Book Go's pad sends them: 1876 x 1164 units at 16 per mm, frames every 8 ms.

extern crate std;

use super::*;
use crate::ptp::{MT_TOOL_FINGER, Slot};
use std::vec::Vec;

const FRAME_US: u64 = 8_000;

fn pad() -> Touchpad {
    Touchpad::new(1876, 1164, 16, 16, 5, Config::juno())
}

/// A frame with these contacts: (tracking id, x, y, palm).
fn frame(contacts: &[(i32, i32, i32, bool)], left: bool) -> Frame {
    let mut slots = [Slot {
        tracking_id: -1,
        key: 0,
        tool: MT_TOOL_FINGER,
        x: 0,
        y: 0,
    }; MAX_SLOTS];
    for (i, &(id, x, y, palm)) in contacts.iter().enumerate() {
        slots[i] = Slot {
            tracking_id: id,
            key: i as i32,
            tool: if palm { MT_TOOL_PALM } else { MT_TOOL_FINGER },
            x,
            y,
        };
    }
    Frame {
        timestamp_us: 0,
        slots,
        num_slots: 5,
        left,
    }
}

struct Run {
    pad: Touchpad,
    t: u64,
    out: Vec<Output>,
}

impl Run {
    fn new() -> Run {
        Run {
            pad: pad(),
            t: 1_000_000,
            out: Vec::new(),
        }
    }

    fn frame(&mut self, contacts: &[(i32, i32, i32, bool)]) {
        self.frame_left(contacts, false);
    }

    fn frame_left(&mut self, contacts: &[(i32, i32, i32, bool)], left: bool) {
        self.t += FRAME_US;
        let f = frame(contacts, left);
        let t = self.t;
        let out = &mut self.out;
        self.pad.frame(&f, t, &mut |o| out.push(o));
    }

    /// Lets time pass in 10 ms ticks, as the driver does.
    fn wait(&mut self, us: u64) {
        let end = self.t + us;
        while self.t < end {
            self.t += KINETIC_TICK_US;
            let t = self.t;
            let out = &mut self.out;
            self.pad.tick(t, &mut |o| out.push(o));
        }
    }

    fn take(&mut self) -> Vec<Output> {
        core::mem::take(&mut self.out)
    }
}

fn motion(out: &[Output]) -> (i32, i32) {
    out.iter().fold((0, 0), |(x, y), o| match o {
        Output::Motion { dx, dy } => (x + dx, y + dy),
        _ => (x, y),
    })
}

fn scroll(out: &[Output]) -> (i32, i32) {
    out.iter().fold((0, 0), |(x, y), o| match o {
        Output::Scroll { dx, dy } => (x + dx, y + dy),
        _ => (x, y),
    })
}

fn buttons(out: &[Output]) -> Vec<(Button, bool)> {
    out.iter()
        .filter_map(|o| match o {
            Output::Button { button, pressed } => Some((*button, *pressed)),
            _ => None,
        })
        .collect()
}

#[test]
fn one_finger_moves_the_pointer() {
    let mut r = Run::new();
    for i in 0..30 {
        r.frame(&[(0, 600 + i * 8, 500, false)]);
    }
    let out = r.take();
    let (dx, dy) = motion(&out);
    assert!(dx > 0 && dy == 0, "rightward motion, got ({dx}, {dy})");
    assert_eq!(scroll(&out), (0, 0));
    assert_eq!(r.pad.gesture_state(), GestureState::PointerMotion);
    r.frame(&[]);
    r.wait(400_000);
    // A finger that moved is not a tap.
    assert!(buttons(&r.take()).is_empty());
}

#[test]
fn two_fingers_moving_down_scroll_up_with_natural_scrolling_then_glide() {
    let mut r = Run::new();
    // Two fingers 25 mm apart, moving down 2 mm per frame for 160 ms.
    for i in 0..20 {
        let y = 300 + i * 32;
        r.frame(&[(0, 700, y, false), (1, 1100, y, false)]);
    }
    let out = r.take();
    let (sx, sy) = scroll(&out);
    assert_eq!(r.pad.gesture_state(), GestureState::Scroll);
    assert_eq!(sx, 0, "the vertical lock keeps x out");
    // Natural scrolling: fingers down move the content down, which is the wheel turned up (positive).
    assert!(sy > 0, "scrolled up, got {sy}");
    assert_eq!(motion(&out), (0, 0), "no pointer motion while scrolling");
    // libinput's units: 2 mm a frame = 32 units, the constant filter 0.9 * 0.2968 * 1000 / 406 per unit, 8 v120 each.
    let per_frame = 32.0 * 0.9 * 0.2968 * 1000.0 / 406.0 * V120_PER_UNIT;
    assert!(
        (f64::from(sy) - per_frame * 18.0).abs() < per_frame * 3.0,
        "{sy} vs ~{}",
        per_frame * 18.0
    );
    // Lift: the scroll stops, and kinetic scrolling goes on the same way, slower and slower, then ends.
    r.frame(&[]);
    let out = r.take();
    assert!(
        out.contains(&Output::ScrollStop { kinetic: true }),
        "{out:?}"
    );
    r.wait(100_000);
    let first = scroll(&r.take()).1;
    r.wait(100_000);
    let second = scroll(&r.take()).1;
    assert!(
        first > 0 && second > 0 && second < first,
        "glide {first} then {second}"
    );
    r.wait(5_000_000);
    r.take();
    assert_eq!(r.pad.deadline(), None, "the glide ended");
}

#[test]
fn a_touch_stops_the_glide() {
    let mut r = Run::new();
    for i in 0..20 {
        let y = 300 + i * 32;
        r.frame(&[(0, 700, y, false), (1, 1100, y, false)]);
    }
    r.frame(&[]);
    r.wait(50_000);
    r.take();
    r.frame(&[(2, 900, 600, false)]);
    r.wait(200_000);
    assert_eq!(scroll(&r.take()), (0, 0));
}

#[test]
fn two_fingers_sideways_scroll_horizontally() {
    let mut r = Run::new();
    for i in 0..20 {
        let x = 500 + i * 32;
        r.frame(&[(0, x, 400, false), (1, x, 700, false)]);
    }
    let (sx, sy) = scroll(&r.take());
    // Natural: fingers right move the content right, which is scrolling left (negative HWHEEL).
    assert!(sx < 0 && sy == 0, "({sx}, {sy})");
}

#[test]
fn two_finger_tap_is_a_right_click() {
    let mut r = Run::new();
    r.frame(&[(0, 700, 500, false), (1, 1000, 500, false)]);
    r.frame(&[(0, 700, 500, false), (1, 1000, 500, false)]);
    r.frame(&[]);
    r.wait(300_000);
    assert_eq!(
        buttons(&r.take()),
        [(Button::Right, true), (Button::Right, false)]
    );
}

#[test]
fn one_finger_tap_is_a_left_click() {
    let mut r = Run::new();
    r.frame(&[(0, 700, 500, false)]);
    r.frame(&[(0, 701, 500, false)]);
    r.frame(&[]);
    r.wait(300_000);
    assert_eq!(
        buttons(&r.take()),
        [(Button::Left, true), (Button::Left, false)]
    );
}

#[test]
fn a_palm_moves_nothing_and_does_not_tap() {
    let mut r = Run::new();
    for i in 0..20 {
        r.frame(&[(0, 600 + i * 10, 500, true)]);
    }
    r.frame(&[]);
    r.wait(300_000);
    let out = r.take();
    assert_eq!(motion(&out), (0, 0));
    assert!(buttons(&out).is_empty());
}

#[test]
fn a_palm_next_to_two_fingers_does_not_stop_the_scroll() {
    let mut r = Run::new();
    for i in 0..20 {
        let y = 300 + i * 32;
        r.frame(&[
            (0, 700, y, false),
            (1, 1100, y, false),
            (2, 1700, 900, true),
        ]);
    }
    assert_eq!(r.pad.gesture_state(), GestureState::Scroll);
    assert!(scroll(&r.take()).1 > 0);
}

#[test]
fn a_click_in_the_bottom_right_corner_is_a_right_click() {
    let mut r = Run::new();
    // The bottom 10 mm (y > 1164 - 160), right of 62.5% of the width.
    r.frame(&[(0, 1700, 1100, false)]);
    r.wait(200_000);
    r.frame_left(&[(0, 1700, 1100, false)], true);
    r.frame_left(&[(0, 1700, 1100, false)], false);
    let b = buttons(&r.take());
    assert_eq!(b, [(Button::Right, true), (Button::Right, false)]);
    // Higher up the same click is left.
    let mut r = Run::new();
    r.frame(&[(0, 900, 500, false)]);
    r.wait(200_000);
    r.frame_left(&[(0, 900, 500, false)], true);
    r.frame_left(&[(0, 900, 500, false)], false);
    let b = buttons(&r.take());
    assert_eq!(b, [(Button::Left, true), (Button::Left, false)]);
}

#[test]
fn spreading_two_fingers_is_a_pinch_not_a_scroll() {
    let mut r = Run::new();
    for i in 0..20 {
        r.frame(&[
            (0, 900 - i * 16, 500 - i * 8, false),
            (1, 1000 + i * 16, 600 + i * 8, false),
        ]);
    }
    assert_eq!(r.pad.gesture_state(), GestureState::Pinch);
    assert_eq!(scroll(&r.take()), (0, 0));
}

#[test]
fn math_helpers_are_exact_enough() {
    for x in [1e-12, 0.5, 2.0, 16.0, 1e12] {
        let s = sqrt(x);
        assert!(((s * s - x) / x).abs() < 1e-15, "sqrt {x}");
    }
    assert_eq!(sqrt(0.0), 0.0);
    // e^-x against known values.
    for (x, e) in [
        (0.0, 1.0),
        (1.0, 0.36787944117144233),
        (4.0, 0.01831563888873418),
        (40.0, 4.248354255291589e-18),
    ] {
        assert!(
            ((exp_neg(x) - e) / e).abs() < 1e-14,
            "exp(-{x}) = {} vs {e}",
            exp_neg(x)
        );
    }
    let pi = core::f64::consts::PI;
    for (y, x, a) in [
        (1.0, 1.0, pi / 4.0),
        (1.0, -1.0, 3.0 * pi / 4.0),
        (-1.0, 0.0, -pi / 2.0),
        (0.3, 2.0, 0.14888994760949725),
    ] {
        assert!((atan2(y, x) - a).abs() < 1e-15, "atan2({y}, {x})");
    }
}

#[test]
fn directions_are_libinputs() {
    // Small moves: three neighbouring octants.
    assert_eq!(xy_get_direction(1.0, 1.0), S | SE | E);
    // Down 5 mm: S, and with r = 4.0 exactly, d1 = 4 and d2 = 4.
    assert_eq!(xy_get_direction(0.0, 5.0), S);
    // Up and right at 45 degrees: r = 1.0, NE.
    assert_eq!(xy_get_direction(5.0, -5.0), NE);
    assert!(same_directions(S, S | SE));
    assert!(!same_directions(N, S));
}

// ---- hold, swipe and pinch gestures (libinput's gesture_notify_*) ----

fn gestures(out: &[Output]) -> Vec<Gesture> {
    out.iter()
        .filter_map(|o| match o {
            Output::Gesture(g) => Some(*g),
            _ => None,
        })
        .collect()
}

fn of_kind(out: &[Output], kind: GestureKind) -> Vec<Gesture> {
    gestures(out)
        .into_iter()
        .filter(|g| g.kind == kind)
        .collect()
}

/// libinput's unaccelerated delta of `units` device units on this pad: the x-scaled delta through the constant filter
/// (0.9 * 0.2968 of the 1000-dpi normalized delta; dpi = 16 * 25.4 as an integer, 406).
fn unaccel(units: f64) -> f64 {
    units * 0.9 * 0.2968 * 1000.0 / 406.0
}

fn no_pointer_or_scroll(out: &[Output]) -> bool {
    out.iter().all(|o| matches!(o, Output::Gesture(_)))
}

#[test]
fn three_fingers_moving_left_swipe() {
    let mut r = Run::new();
    let mut out = Vec::new();
    let mut begin_frame = None;
    let frames = 40;
    for i in 0..frames {
        let x = 1400 - i * 8;
        r.frame(&[
            (0, x, 500, false),
            (1, x + 200, 500, false),
            (2, x + 400, 500, false),
        ]);
        let f = r.take();
        if begin_frame.is_none() && gestures(&f).iter().any(|g| g.phase == GesturePhase::Begin) {
            begin_frame = Some(i);
        }
        out.extend(f);
    }
    r.frame(&[]);
    out.extend(r.take());
    r.wait(400_000);
    out.extend(r.take());
    let g = of_kind(&out, GestureKind::Swipe);
    assert_eq!(
        g.first().map(|g| (g.phase, g.fingers)),
        Some((GesturePhase::Begin, 3)),
        "{out:?}"
    );
    assert_eq!(
        (g[0].dx, g[0].dy, g[0].scale, g[0].angle),
        (0.0, 0.0, 1.0, 0.0)
    );
    let updates: Vec<_> = g
        .iter()
        .filter(|g| g.phase == GesturePhase::Update)
        .collect();
    // Every frame after the begin frame moves 8 units to the left.
    let n = (frames - 1 - begin_frame.expect("a begin")) as usize;
    assert_eq!(updates.len(), n);
    let sum: f64 = updates.iter().map(|g| g.dx).sum();
    let want = -unaccel(8.0 * n as f64);
    assert!(((sum - want) / want).abs() < 0.01, "{sum} vs {want}");
    assert!(updates.iter().all(|g| g.dy == 0.0 && g.fingers == 3));
    let last = g.last().unwrap();
    assert_eq!(
        (last.phase, last.cancelled, last.fingers),
        (GesturePhase::End, false, 3)
    );
    assert!(
        out.iter().all(|o| matches!(o, Output::Gesture(_))),
        "no scroll, motion or button: {out:?}"
    );
    assert_eq!(
        gestures(&out)
            .iter()
            .filter(|g| g.kind != GestureKind::Swipe)
            .count(),
        0
    );
}

#[test]
fn four_fingers_moving_up_swipe() {
    let mut r = Run::new();
    for i in 0..30 {
        let y = 900 - i * 8;
        r.frame(&[
            (0, 500, y, false),
            (1, 700, y, false),
            (2, 900, y, false),
            (3, 1100, y, false),
        ]);
    }
    r.frame(&[]);
    r.wait(300_000);
    let out = r.take();
    let g = of_kind(&out, GestureKind::Swipe);
    assert_eq!(
        g.first().map(|g| (g.phase, g.fingers)),
        Some((GesturePhase::Begin, 4)),
        "{out:?}"
    );
    let dy: f64 = g.iter().map(|g| g.dy).sum();
    assert!(dy < 0.0, "up is negative, got {dy}");
    assert!(g.iter().all(|g| g.dx == 0.0 && g.fingers == 4));
    assert_eq!(
        g.last().map(|g| (g.phase, g.cancelled)),
        Some((GesturePhase::End, false))
    );
    assert!(no_pointer_or_scroll(&out));
}

#[test]
fn a_finger_added_to_a_swipe_cancels_it_after_the_switch_timeout() {
    let mut r = Run::new();
    for i in 0..30 {
        let x = 1400 - i * 8;
        r.frame(&[
            (0, x, 500, false),
            (1, x + 200, 500, false),
            (2, x + 400, 500, false),
        ]);
    }
    let before = r.take();
    assert!(
        of_kind(&before, GestureKind::Swipe)
            .iter()
            .any(|g| g.phase == GesturePhase::Begin)
    );
    assert_eq!(r.pad.gesture_state(), GestureState::Swipe);
    // A fourth finger: the count change waits for the switch timeout (100 ms), nothing is sent meanwhile.
    let x = 1400 - 30 * 8;
    r.frame(&[
        (0, x, 500, false),
        (1, x + 200, 500, false),
        (2, x + 400, 500, false),
        (3, x + 600, 500, false),
    ]);
    r.wait(60_000);
    assert!(
        of_kind(&r.take(), GestureKind::Swipe)
            .iter()
            .all(|g| g.phase != GesturePhase::End)
    );
    r.wait(100_000);
    let g = of_kind(&r.take(), GestureKind::Swipe);
    assert_eq!(g.len(), 1);
    assert_eq!(
        (g[0].phase, g[0].cancelled, g[0].fingers),
        (GesturePhase::End, true, 3)
    );
}

#[test]
fn spreading_two_fingers_sends_a_pinch_with_a_growing_scale() {
    let mut r = Run::new();
    let mut out = Vec::new();
    for i in 0..20 {
        r.frame(&[
            (0, 900 - i * 12, 500, false),
            (1, 1000 + i * 12, 500, false),
        ]);
        out.extend(r.take());
    }
    r.frame(&[]);
    out.extend(r.take());
    let g = of_kind(&out, GestureKind::Pinch);
    assert_eq!(
        (g[0].phase, g[0].fingers, g[0].scale, g[0].angle),
        (GesturePhase::Begin, 2, 1.0, 0.0),
        "{out:?}"
    );
    let updates: Vec<_> = g
        .iter()
        .filter(|g| g.phase == GesturePhase::Update)
        .collect();
    assert!(updates.len() > 5);
    assert!(updates.windows(2).all(|w| w[1].scale > w[0].scale));
    assert!(updates[0].scale > 1.0 && updates.last().unwrap().scale > 2.0);
    // Both fingers move apart symmetrically: the centre stays, the angle stays.
    assert!(
        updates
            .iter()
            .all(|g| g.dx == 0.0 && g.dy == 0.0 && g.angle == 0.0)
    );
    let end = g.last().unwrap();
    assert_eq!((end.phase, end.cancelled), (GesturePhase::End, false));
    assert_eq!(end.scale, updates.last().unwrap().scale);
    assert!(no_pointer_or_scroll(&out));
}

#[test]
fn rotating_two_fingers_sends_the_angle() {
    let mut r = Run::new();
    let mut out = Vec::new();
    let mut begin_frame = None;
    let frames = 20;
    // 2 degrees a frame about (950, 600), 300 units (18.75 mm) from the centre.
    for i in 0..frames {
        let th = f64::from(i) * 2.0 * core::f64::consts::PI / 180.0;
        let (c, s) = (cos_sin(th).0, cos_sin(th).1);
        let (dx, dy) = ((300.0 * c).round() as i32, (300.0 * s).round() as i32);
        r.frame(&[
            (0, 950 + dx, 600 + dy, false),
            (1, 950 - dx, 600 - dy, false),
        ]);
        let f = r.take();
        if begin_frame.is_none()
            && gestures(&f)
                .iter()
                .any(|g| g.kind == GestureKind::Pinch && g.phase == GesturePhase::Begin)
        {
            begin_frame = Some(i);
        }
        out.extend(f);
    }
    let g = of_kind(&out, GestureKind::Pinch);
    assert_eq!(g[0].phase, GesturePhase::Begin, "{out:?}");
    let angle: f64 = g.iter().map(|g| g.angle).sum();
    let want = 2.0 * f64::from(frames - 1 - begin_frame.expect("a begin"));
    assert!((angle - want).abs() < 1.0, "{angle} vs {want} degrees");
    // The centre does not move, the distance does not change.
    assert!(g.iter().all(|g| g.scale > 0.98 && g.scale < 1.02));
}

/// (cos, sin) through the crate's own atan2-free route: a Taylor series is enough for these angles.
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

#[test]
fn a_resting_finger_holds_then_motion_cancels_the_hold() {
    let mut r = Run::new();
    r.frame(&[(0, 900, 600, false)]);
    r.wait(30_000);
    assert!(
        gestures(&r.take()).is_empty(),
        "not before the quick hold timeout (40 ms)"
    );
    r.wait(170_000);
    let g = gestures(&r.take());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].fingers, g[0].cancelled),
        (GestureKind::Hold, GesturePhase::Begin, 1, false)
    );
    assert_eq!(r.pad.gesture_state(), GestureState::Hold);
    // 2 mm in a frame: not hold and motion, so the hold ends cancelled and the pointer moves, as without the hold.
    r.frame(&[(0, 932, 600, false)]);
    r.frame(&[(0, 964, 600, false)]);
    let out = r.take();
    let end = gestures(&out);
    assert_eq!(end.len(), 1, "{out:?}");
    assert_eq!(
        (end[0].kind, end[0].phase, end[0].cancelled, end[0].fingers),
        (GestureKind::Hold, GesturePhase::End, true, 1)
    );
    assert!(motion(&out).0 > 0, "{out:?}");
    let first = out
        .iter()
        .position(|o| matches!(o, Output::Gesture(_)))
        .unwrap();
    let first_motion = out
        .iter()
        .position(|o| matches!(o, Output::Motion { .. }))
        .unwrap();
    assert!(
        first < first_motion,
        "the hold ends before the pointer moves"
    );
    assert_eq!(r.pad.gesture_state(), GestureState::PointerMotion);
}

#[test]
fn a_small_move_while_holding_is_hold_and_motion() {
    let mut r = Run::new();
    r.frame(&[(0, 900, 600, false)]);
    r.wait(200_000);
    r.take();
    // 0.25 mm: under HOLD_AND_MOTION_THRESHOLD, the hold goes on and the pointer moves anyway.
    for i in 1..=4 {
        r.frame(&[(0, 900 + i, 600, false)]);
    }
    assert_eq!(r.pad.gesture_state(), GestureState::HoldAndMotion);
    assert!(gestures(&r.take()).is_empty(), "no second begin, no end");
    // Past the threshold the hold ends cancelled.
    r.frame(&[(0, 930, 600, false)]);
    let out = r.take();
    let g = gestures(&out);
    assert_eq!(g.len(), 1, "{out:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].cancelled),
        (GestureKind::Hold, GesturePhase::End, true)
    );
    assert_eq!(r.pad.gesture_state(), GestureState::PointerMotion);
}

#[test]
fn a_hold_and_motion_posts_the_motion_of_pointer_motion() {
    // The same frames at the same times, with the hold timer cancelled in the second run: the pointer moves alike.
    // Tapping is off, so the tap machine does not hold the small moves back.
    let run = |hold: bool| {
        let mut r = Run::new();
        r.pad = Touchpad::new(
            1876,
            1164,
            16,
            16,
            5,
            Config {
                tap: TapConfig::libinput_default(false),
                ..Config::juno()
            },
        );
        r.frame(&[(0, 900, 600, false)]);
        if !hold {
            r.pad.hold_deadline = None;
        }
        // 7 frames of 0.06 mm: under HOLD_AND_MOTION_THRESHOLD in all, and past the 40 ms of the quick hold.
        for i in 1..=7 {
            r.frame(&[(0, 900 + i, 600, false)]);
        }
        let out = r.take();
        (
            r.pad.gesture_state(),
            motion(&out),
            r.pad.motion_rest,
            of_kind(&out, GestureKind::Hold).len(),
        )
    };
    let (state, held, rest, holds) = run(true);
    assert_eq!((state, holds), (GestureState::HoldAndMotion, 1));
    let (state, plain, plain_rest, holds) = run(false);
    assert_eq!((state, holds), (GestureState::PointerMotion, 0));
    assert_eq!((held, rest), (plain, plain_rest));
    assert!(held.0 != 0 || rest.0 > 0.0, "the pointer moved");
}

#[test]
fn a_resting_finger_that_lifts_ends_the_hold_uncancelled() {
    let mut r = Run::new();
    r.frame(&[(0, 900, 600, false)]);
    r.wait(200_000);
    r.take();
    r.frame(&[]);
    let g = gestures(&r.take());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].cancelled, g[0].fingers),
        (GestureKind::Hold, GesturePhase::End, false, 1)
    );
    assert_eq!(r.pad.gesture_state(), GestureState::None);
}

#[test]
fn three_resting_fingers_hold_with_the_tap_timeout() {
    let mut r = Run::new();
    r.frame(&[
        (0, 600, 600, false),
        (1, 900, 600, false),
        (2, 1200, 600, false),
    ]);
    // With tapping on, three fingers hold when the tap machine times out (180 ms), not on the hold timer.
    r.wait(150_000);
    assert!(gestures(&r.take()).is_empty());
    r.wait(100_000);
    let g = gestures(&r.take());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].fingers),
        (GestureKind::Hold, GesturePhase::Begin, 3)
    );
    r.frame(&[]);
    let out = r.take();
    let g = gestures(&out);
    assert_eq!(
        (g[0].phase, g[0].cancelled, g[0].fingers),
        (GesturePhase::End, false, 3)
    );
    assert!(buttons(&out).is_empty(), "a held touch is not a tap");
}

#[test]
fn a_quick_hold_after_a_scroll_begins_after_40_ms() {
    let mut r = Run::new();
    for i in 0..20 {
        let y = 300 + i * 32;
        r.frame(&[(0, 700, y, false), (1, 1100, y, false)]);
    }
    r.frame(&[]);
    r.take();
    r.wait(20_000);
    r.frame(&[(2, 700, 600, false), (3, 1100, 600, false)]);
    r.take();
    r.wait(30_000);
    assert!(gestures(&r.take()).is_empty(), "not yet");
    r.wait(20_000);
    let g = gestures(&r.take());
    assert_eq!(g.len(), 1, "{g:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].fingers),
        (GestureKind::Hold, GesturePhase::Begin, 2)
    );
}

#[test]
fn a_scroll_after_a_hold_cancels_the_hold() {
    let mut r = Run::new();
    r.frame(&[(0, 700, 300, false), (1, 1100, 300, false)]);
    r.wait(100_000);
    let g = gestures(&r.take());
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].fingers),
        (GestureKind::Hold, GesturePhase::Begin, 2)
    );
    let mut out = Vec::new();
    for i in 1..20 {
        r.frame(&[
            (0, 700, 300 + i * 32, false),
            (1, 1100, 300 + i * 32, false),
        ]);
        out.extend(r.take());
    }
    let g = gestures(&out);
    assert_eq!(g.len(), 1, "{out:?}");
    assert_eq!(
        (g[0].kind, g[0].phase, g[0].cancelled),
        (GestureKind::Hold, GesturePhase::End, true)
    );
    assert_eq!(r.pad.gesture_state(), GestureState::Scroll);
    assert!(scroll(&out).1 > 0);
}
