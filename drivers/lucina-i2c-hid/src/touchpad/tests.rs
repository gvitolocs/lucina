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
    let mut slots = [Slot { tracking_id: -1, key: 0, tool: MT_TOOL_FINGER, x: 0, y: 0 }; MAX_SLOTS];
    for (i, &(id, x, y, palm)) in contacts.iter().enumerate() {
        slots[i] = Slot { tracking_id: id, key: i as i32, tool: if palm { MT_TOOL_PALM } else { MT_TOOL_FINGER }, x, y };
    }
    Frame { timestamp_us: 0, slots, num_slots: 5, left }
}

struct Run {
    pad: Touchpad,
    t: u64,
    out: Vec<Output>,
}

impl Run {
    fn new() -> Run {
        Run { pad: pad(), t: 1_000_000, out: Vec::new() }
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
    assert!((f64::from(sy) - per_frame * 18.0).abs() < per_frame * 3.0, "{sy} vs ~{}", per_frame * 18.0);
    // Lift: the scroll stops, and kinetic scrolling goes on the same way, slower and slower, then ends.
    r.frame(&[]);
    let out = r.take();
    assert!(out.contains(&Output::ScrollStop { kinetic: true }), "{out:?}");
    r.wait(100_000);
    let first = scroll(&r.take()).1;
    r.wait(100_000);
    let second = scroll(&r.take()).1;
    assert!(first > 0 && second > 0 && second < first, "glide {first} then {second}");
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
    assert_eq!(buttons(&r.take()), [(Button::Right, true), (Button::Right, false)]);
}

#[test]
fn one_finger_tap_is_a_left_click() {
    let mut r = Run::new();
    r.frame(&[(0, 700, 500, false)]);
    r.frame(&[(0, 701, 500, false)]);
    r.frame(&[]);
    r.wait(300_000);
    assert_eq!(buttons(&r.take()), [(Button::Left, true), (Button::Left, false)]);
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
        r.frame(&[(0, 700, y, false), (1, 1100, y, false), (2, 1700, 900, true)]);
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
        r.frame(&[(0, 900 - i * 16, 500 - i * 8, false), (1, 1000 + i * 16, 600 + i * 8, false)]);
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
    for (x, e) in [(0.0, 1.0), (1.0, 0.36787944117144233), (4.0, 0.01831563888873418), (40.0, 4.248354255291589e-18)] {
        assert!(((exp_neg(x) - e) / e).abs() < 1e-14, "exp(-{x}) = {} vs {e}", exp_neg(x));
    }
    let pi = core::f64::consts::PI;
    for (y, x, a) in [(1.0, 1.0, pi / 4.0), (1.0, -1.0, 3.0 * pi / 4.0), (-1.0, 0.0, -pi / 2.0), (0.3, 2.0, 0.14888994760949725)] {
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
