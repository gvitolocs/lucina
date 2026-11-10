//! The viewer's pointer and keys onto Juno's input, with the behaviour of voxl's `input.rs` (which drives X through XTEST):
//! X button numbers, wheel clicks, pixel scrolling gathered into notches, keysyms with Shift fixed up, and nothing left held.

use std::collections::HashMap;

use crate::Input;
use crate::keysym;

pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const KEY_LEFTSHIFT: u16 = 42;

const SHIFT_L: u32 = 0xffe1;
const SHIFT_R: u32 = 0xffe2;

/// Desktop pixels of scrolling that make one wheel notch. voxl uses 180 for X, whose toolkits scroll about 3 lines of a
/// bigger font a notch. Juno's toolkit scrolls 3 lines a notch too, at 1920x1080 with a smaller line, so a notch is less
/// distance on screen: 120 px keeps a trackpad's swipe moving the page about as far as the fingers went.
pub const PX_PER_NOTCH: f64 = 120.0;
/// voxl's cap on clicks from one message, so that a wild value cannot scroll for a minute.
const MAX_CLICKS: i32 = 20;

/// What `key` must do about Shift, as voxl's `ShiftFix`.
#[derive(Debug, PartialEq, Eq)]
enum ShiftFix {
    None,
    /// The symbol needs Shift and none is held (text from an on-screen keyboard, a paste).
    Add,
    /// The symbol is a plain character and Shift is held (e.g. a tapped lower-case letter with Shift latched).
    Lift,
}

pub struct Keys {
    pub(crate) input: Box<dyn Input>,
    /// Keysym -> evdev code of keys held down.
    pressed: HashMap<u32, u16>,
    /// X button numbers held.
    buttons: [bool; 16],
    /// Scroll in notch units not yet sent.
    scroll_acc: (f64, f64),
}

impl Keys {
    pub fn new(input: Box<dyn Input>) -> Keys {
        Keys { input, pressed: HashMap::new(), buttons: [false; 16], scroll_acc: (0.0, 0.0) }
    }

    pub fn move_to(&mut self, x: i32, y: i32) {
        self.input.move_to(x, y);
    }

    /// X numbering, as the web client sends it: 1 left, 2 middle, 3 right, 4/5 wheel up/down, 6/7 left/right, 8/9 back and
    /// forward. A wheel button scrolls on its press, as on X.
    pub fn button(&mut self, button: u8, down: bool) {
        match button {
            4..=7 => {
                if down {
                    let (dx, dy) = match button {
                        4 => (0, -1),
                        5 => (0, 1),
                        6 => (-1, 0),
                        _ => (1, 0),
                    };
                    self.input.wheel(dx, dy);
                }
            }
            _ => {
                let Some(code) = button_code(button) else { return };
                let b = &mut self.buttons[button as usize];
                if *b == down {
                    return;
                }
                *b = down;
                self.input.button(code, down);
            }
        }
    }

    /// `s,dx,dy`: whole wheel clicks, positive dy down and positive dx right.
    pub fn scroll(&mut self, dx: i32, dy: i32) {
        let (dx, dy) = (dx.clamp(-MAX_CLICKS, MAX_CLICKS), dy.clamp(-MAX_CLICKS, MAX_CLICKS));
        if dx != 0 || dy != 0 {
            self.input.wheel(dx, dy);
        }
    }

    /// `sp,dx,dy`: desktop pixels, gathered until they make whole notches; the rest carries into the next message.
    pub fn scroll_px(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.scroll_acc.0 += dx / PX_PER_NOTCH;
        self.scroll_acc.1 += dy / PX_PER_NOTCH;
        let (nx, ny) = (self.scroll_acc.0.trunc(), self.scroll_acc.1.trunc());
        self.scroll_acc.0 -= nx;
        self.scroll_acc.1 -= ny;
        self.scroll(nx as i32, ny as i32);
    }

    fn shift_held(&self) -> Vec<u16> {
        [SHIFT_L, SHIFT_R].iter().filter_map(|k| self.pressed.get(k).copied()).collect()
    }

    pub fn key(&mut self, keysym: u32, down: bool) {
        if !down {
            if let Some(code) = self.pressed.remove(&keysym) {
                self.input.key(code, false);
            }
            return;
        }
        if let Some(&code) = self.pressed.get(&keysym) {
            // Autorepeat: pressed again without a release, as voxl does on X.
            self.input.key(code, true);
            return;
        }
        let Some(m) = keysym::map(keysym) else { return };
        let held = self.shift_held();
        // Only characters have a shift level to fix; Shift with an arrow or Tab is meant.
        let character = keysym < 0x100;
        let fix = if m.shift && held.is_empty() {
            ShiftFix::Add
        } else if !m.shift && !held.is_empty() && character && m.code != KEY_LEFTSHIFT {
            ShiftFix::Lift
        } else {
            ShiftFix::None
        };
        self.pressed.insert(keysym, m.code);
        match fix {
            ShiftFix::None => self.input.key(m.code, true),
            ShiftFix::Add => {
                self.input.key(KEY_LEFTSHIFT, true);
                self.input.key(m.code, true);
                self.input.key(KEY_LEFTSHIFT, false);
            }
            ShiftFix::Lift => {
                for &h in &held {
                    self.input.key(h, false);
                }
                self.input.key(m.code, true);
                for &h in &held {
                    self.input.key(h, true);
                }
            }
        }
    }

    /// `r`, a viewer leaving, the host stopping: every key and button up.
    pub fn release_all(&mut self) {
        for (_, code) in std::mem::take(&mut self.pressed) {
            self.input.key(code, false);
        }
        for b in 0..self.buttons.len() {
            if std::mem::take(&mut self.buttons[b])
                && let Some(code) = button_code(b as u8)
            {
                self.input.button(code, false);
            }
        }
        self.scroll_acc = (0.0, 0.0);
    }
}

fn button_code(button: u8) -> Option<u16> {
    Some(match button {
        1 => BTN_LEFT,
        2 => BTN_MIDDLE,
        3 => BTN_RIGHT,
        8 => BTN_SIDE,
        9 => BTN_EXTRA,
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Call {
        Move(i32, i32),
        Button(u16, bool),
        Wheel(i32, i32),
        Key(u16, bool),
    }

    /// Writes down what it was asked to do.
    #[derive(Clone, Default)]
    pub struct Recorder(pub Arc<Mutex<Vec<Call>>>);

    impl Recorder {
        pub fn take(&self) -> Vec<Call> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    impl Input for Recorder {
        fn move_to(&mut self, x: i32, y: i32) {
            self.0.lock().unwrap().push(Call::Move(x, y));
        }
        fn button(&mut self, code: u16, down: bool) {
            self.0.lock().unwrap().push(Call::Button(code, down));
        }
        fn wheel(&mut self, dx: i32, dy: i32) {
            self.0.lock().unwrap().push(Call::Wheel(dx, dy));
        }
        fn key(&mut self, code: u16, down: bool) {
            self.0.lock().unwrap().push(Call::Key(code, down));
        }
    }

    fn keys() -> (Keys, Recorder) {
        let r = Recorder::default();
        (Keys::new(Box::new(r.clone())), r)
    }

    use Call::*;
    const KEY_A: u16 = 30;
    const KEY_1: u16 = 2;
    const KEY_LEFT: u16 = 105;

    #[test]
    fn buttons_are_x_numbers_and_wheel_buttons_click_once() {
        let (mut k, r) = keys();
        k.button(1, true);
        k.button(1, true);
        k.button(3, true);
        k.button(2, false);
        k.button(5, true);
        k.button(5, false);
        k.button(6, true);
        k.button(8, true);
        assert_eq!(r.take(), vec![Button(BTN_LEFT, true), Button(BTN_RIGHT, true), Wheel(0, 1), Wheel(-1, 0), Button(BTN_SIDE, true)]);
        k.release_all();
        let mut up = r.take();
        up.sort();
        assert_eq!(up, vec![Button(BTN_LEFT, false), Button(BTN_RIGHT, false), Button(BTN_SIDE, false)]);
    }

    #[test]
    fn pixel_scroll_makes_whole_notches_and_keeps_the_rest() {
        let (mut k, r) = keys();
        k.scroll_px(0.0, 100.0);
        assert!(r.take().is_empty());
        k.scroll_px(0.0, 30.0);
        assert_eq!(r.take(), vec![Wheel(0, 1)]);
        k.scroll_px(-250.0, -300.0);
        assert_eq!(r.take(), vec![Wheel(-2, -2)]);
        k.scroll(0, 1000);
        assert_eq!(r.take(), vec![Wheel(0, MAX_CLICKS)]);
        k.scroll_px(f64::NAN, 1.0);
        assert!(r.take().is_empty());
    }

    #[test]
    fn shift_is_added_for_a_shifted_symbol_and_lifted_for_a_plain_one() {
        let (mut k, r) = keys();
        // "!" with no Shift held: Shift around the press.
        k.key(0x21, true);
        k.key(0x21, false);
        assert_eq!(r.take(), vec![Key(KEY_LEFTSHIFT, true), Key(KEY_1, true), Key(KEY_LEFTSHIFT, false), Key(KEY_1, false)]);
        // Shift held and "a" arrives: Shift up around it, then down again.
        k.key(SHIFT_L, true);
        k.key(0x61, true);
        k.key(0x61, false);
        assert_eq!(r.take(), vec![Key(KEY_LEFTSHIFT, true), Key(KEY_LEFTSHIFT, false), Key(KEY_A, true), Key(KEY_LEFTSHIFT, true), Key(KEY_A, false)]);
        // Shift held and "A": as it is. Shift and an arrow: as it is, that is selecting.
        k.key(0x41, true);
        k.key(0xff51, true);
        assert_eq!(r.take(), vec![Key(KEY_A, true), Key(KEY_LEFT, true)]);
        k.release_all();
        let mut up = r.take();
        up.sort();
        assert_eq!(up, vec![Key(KEY_A, false), Key(KEY_LEFTSHIFT, false), Key(KEY_LEFT, false)]);
        // Nothing is held any more.
        k.release_all();
        assert!(r.take().is_empty());
    }

    #[test]
    fn autorepeat_presses_again_and_unknown_keysyms_do_nothing() {
        let (mut k, r) = keys();
        k.key(0x61, true);
        k.key(0x61, true);
        k.key(0x0100_00e9, true);
        k.key(0x0100_00e9, false);
        assert_eq!(r.take(), vec![Key(KEY_A, true), Key(KEY_A, true)]);
    }
}
