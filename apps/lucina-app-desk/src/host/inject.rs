//! Input from the client, through the path real input takes: events written to an evdev device reach every program reading it
//! (kernel patch 0406, as Linux's `evdev_write`). The evdev part is `lucina-inject`, which `lucina-devd` shares; this file turns
//! RustDesk's mouse masks and key actions into it.
//!
//! The pointer has no absolute device on this laptop, so an absolute position is written as a move far to the corner, where the
//! toolkit's pointer stops, and then the distance to the position. All of it goes in one write, so that nobody draws in between.

use lucina_app::keys;
pub use lucina_inject::Sink;
use lucina_inject::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
#[cfg(test)]
use lucina_inject::{EV_KEY, EV_REL, EV_SYN, FAR, REL_WHEEL, REL_X, REL_Y};
use lucinadesk_core::hbb::ControlKey;
use lucinadesk_core::host::KeyAction;

const KIND_MOVE: i32 = 0;
const KIND_DOWN: i32 = 1;
const KIND_UP: i32 = 2;
const KIND_WHEEL: i32 = 3;
const KIND_MOVE_RELATIVE: i32 = 5;

/// For a test without a machine to type on: says what it got.
#[derive(Default)]
pub struct LogSink {
    n: u64,
}

impl Sink for LogSink {
    fn write_events(&mut self, bytes: &[u8]) {
        for ev in bytes.chunks(24) {
            let (t, c) = (u16::from_ne_bytes([ev[16], ev[17]]), u16::from_ne_bytes([ev[18], ev[19]]));
            let v = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
            if t != lucina_inject::EV_SYN && self.n < 200 {
                self.n += 1;
                eprintln!("lucina-app-desk: host: would inject type {t} code {c} value {v}");
            }
        }
    }
}

pub struct Injector {
    inner: lucina_inject::Injector,
}

/// The evdev code of a `ControlKey`, for clients not in Map mode.
fn control_code(k: i32) -> Option<u16> {
    use ControlKey as C;
    let c = C::try_from(k).ok()?;
    Some(match c {
        C::Alt => keys::KEY_LEFTALT,
        C::Backspace => keys::KEY_BACKSPACE,
        C::CapsLock => keys::KEY_CAPSLOCK,
        C::Control => keys::KEY_LEFTCTRL,
        C::Delete => 111,
        C::DownArrow => keys::KEY_DOWN,
        C::End => keys::KEY_END,
        C::Escape => keys::KEY_ESC,
        C::F1 => keys::KEY_F1,
        C::F2 => keys::KEY_F2,
        C::F3 => keys::KEY_F3,
        C::F4 => 62,
        C::F5 => 63,
        C::F6 => 64,
        C::F7 => 65,
        C::F8 => 66,
        C::F9 => 67,
        C::F10 => 68,
        C::F11 => 87,
        C::F12 => 88,
        C::Home => keys::KEY_HOME,
        C::LeftArrow => keys::KEY_LEFT,
        C::Meta => 125,
        C::PageDown => keys::KEY_PAGEDOWN,
        C::PageUp => keys::KEY_PAGEUP,
        C::Return => keys::KEY_ENTER,
        C::RightArrow => keys::KEY_RIGHT,
        C::Shift => keys::KEY_LEFTSHIFT,
        C::Space => keys::KEY_SPACE,
        C::Tab => keys::KEY_TAB,
        C::UpArrow => keys::KEY_UP,
        C::RShift => keys::KEY_RIGHTSHIFT,
        C::RControl => keys::KEY_RIGHTCTRL,
        C::RAlt => keys::KEY_RIGHTALT,
        C::NumpadEnter | C::Divide | C::Multiply | C::Subtract | C::Add | C::Decimal => return None,
        _ => return None,
    })
}

impl Injector {
    /// The first input device there is. All of them reach the same readers.
    pub fn open() -> Result<Injector, String> {
        lucina_inject::Injector::open().map(|inner| Injector { inner })
    }

    pub fn new(sink: Box<dyn Sink>) -> Injector {
        Injector { inner: lucina_inject::Injector::new(sink) }
    }

    /// `mask` = buttons << 3 | kind, as the client sends it.
    pub fn mouse(&mut self, mask: i32, x: i32, y: i32) {
        let (kind, buttons) = (mask & 7, mask >> 3);
        match kind {
            KIND_MOVE => self.inner.move_to(x, y),
            // Where the pointer already is. A stock client's press says (0, 0), and stock RustDesk's input_service reads a position only from a move.
            KIND_DOWN | KIND_UP => {
                for (bit, code) in [(1, BTN_LEFT), (2, BTN_RIGHT), (4, BTN_MIDDLE)] {
                    if buttons & bit != 0 {
                        self.inner.button(code, kind == KIND_DOWN);
                    }
                }
            }
            KIND_WHEEL => self.inner.wheel(y, x),
            KIND_MOVE_RELATIVE => self.inner.move_by(x, y),
            _ => {}
        }
    }

    pub fn key(&mut self, k: KeyAction) {
        let mods = |list: &[i32]| -> Vec<u16> { list.iter().filter_map(|&m| control_code(m)).collect() };
        match k {
            KeyAction::Position { code, down } => {
                // X keycodes are evdev's plus 8.
                let evdev = code.saturating_sub(8);
                if evdev == 0 || evdev >= 0x100 {
                    return;
                }
                self.inner.set_key(evdev as u16, down);
            }
            KeyAction::Char { c, down, press, modifiers } => {
                let Some((code, shift)) = char::from_u32(c).and_then(keys::code_for) else { return };
                let mut held = mods(&modifiers);
                if shift && !held.contains(&keys::KEY_LEFTSHIFT) {
                    held.push(keys::KEY_LEFTSHIFT);
                }
                if press {
                    self.inner.press(code, &held);
                } else {
                    self.inner.set_key(code, down);
                }
            }
            KeyAction::Control { key, down, press, modifiers } => {
                let Some(code) = control_code(key) else { return };
                if press {
                    self.inner.press(code, &mods(&modifiers));
                } else {
                    self.inner.set_key(code, down);
                }
            }
            KeyAction::Text(s) => {
                // Only what has a key: a newline or a tab in the text is skipped, as before.
                for c in s.chars() {
                    if keys::code_for(c).is_some() {
                        self.inner.type_char(c);
                    }
                }
            }
        }
    }

    /// One key, by its evdev code, as the browser host sends it.
    pub fn key_code(&mut self, code: u16, down: bool) {
        self.inner.set_key(code, down);
    }

    /// Nothing stays held when the client goes.
    pub fn release_all(&mut self) {
        self.inner.release_all();
    }
}

/// The browser host's input (lucina-desk-web maps voxl's commands to these): the same writes as for a RustDesk client.
impl lucina_desk_web::Input for Injector {
    /// Through the corner, in one write, as `mouse` does for RustDesk's absolute moves.
    fn move_to(&mut self, x: i32, y: i32) {
        self.mouse(KIND_MOVE, x, y);
    }

    fn button(&mut self, code: u16, down: bool) {
        self.inner.button(code, down);
    }

    /// Down and right are positive here; evdev's wheel counts up as positive.
    fn wheel(&mut self, dx: i32, dy: i32) {
        self.mouse(KIND_WHEEL, dx, -dy);
    }

    fn key(&mut self, code: u16, down: bool) {
        self.key_code(code, down);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_inject::REL_HWHEEL;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<(u16, u16, i32)>>>);

    impl Sink for Rec {
        fn write_events(&mut self, bytes: &[u8]) {
            assert_eq!(bytes.len() % 24, 0);
            for ev in bytes.chunks(24) {
                let t = u16::from_ne_bytes([ev[16], ev[17]]);
                let c = u16::from_ne_bytes([ev[18], ev[19]]);
                let v = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
                if t != EV_SYN {
                    self.0.lock().unwrap().push((t, c, v));
                }
            }
        }
    }

    fn injector() -> (Injector, Rec) {
        let r = Rec::default();
        (Injector::new(Box::new(r.clone())), r)
    }

    #[test]
    fn a_position_goes_through_the_corner_and_lands_exactly() {
        let (mut i, r) = injector();
        i.mouse(KIND_MOVE, 300, 200);
        assert_eq!(
            *r.0.lock().unwrap(),
            vec![(EV_REL, REL_X, -FAR), (EV_REL, REL_Y, -FAR), (EV_REL, REL_X, 300), (EV_REL, REL_Y, 200)]
        );
        // The same again is nothing.
        r.0.lock().unwrap().clear();
        i.mouse(KIND_MOVE, 300, 200);
        assert!(r.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_click_stays_where_the_pointer_is_and_a_button_is_never_left_down() {
        let (mut i, r) = injector();
        i.mouse(KIND_MOVE, 300, 200);
        r.0.lock().unwrap().clear();
        // What a stock client sends: no position on a press, as in stock RustDesk's input_service.
        i.mouse(KIND_DOWN | 1 << 3, 0, 0);
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_KEY, BTN_LEFT, 1)]);
        r.0.lock().unwrap().clear();
        i.release_all();
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_KEY, BTN_LEFT, 0)]);
    }

    #[test]
    fn wheel() {
        let (mut i, r) = injector();
        i.mouse(KIND_WHEEL, 0, -2);
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_REL, REL_WHEEL, -2)]);
    }

    #[test]
    fn map_mode_keys_go_by_position_and_are_released_at_the_end() {
        let (mut i, r) = injector();
        i.key(KeyAction::Position { code: 38, down: true });
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_KEY, keys::KEY_A, 1)]);
        r.0.lock().unwrap().clear();
        i.release_all();
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_KEY, keys::KEY_A, 0)]);
    }

    #[test]
    fn a_code_outside_the_keyboard_is_ignored() {
        let (mut i, r) = injector();
        i.key(KeyAction::Position { code: 3, down: true });
        i.key(KeyAction::Position { code: 0x200, down: true });
        assert!(r.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_shifted_character_and_control_keys_with_modifiers() {
        let (mut i, r) = injector();
        i.key(KeyAction::Char { c: 'A' as u32, down: false, press: true, modifiers: vec![] });
        assert_eq!(
            *r.0.lock().unwrap(),
            vec![(EV_KEY, keys::KEY_LEFTSHIFT, 1), (EV_KEY, keys::KEY_A, 1), (EV_KEY, keys::KEY_A, 0), (EV_KEY, keys::KEY_LEFTSHIFT, 0)]
        );
        r.0.lock().unwrap().clear();
        i.key(KeyAction::Control { key: ControlKey::Return as i32, down: false, press: true, modifiers: vec![ControlKey::Control as i32] });
        assert_eq!(
            *r.0.lock().unwrap(),
            vec![(EV_KEY, keys::KEY_LEFTCTRL, 1), (EV_KEY, keys::KEY_ENTER, 1), (EV_KEY, keys::KEY_ENTER, 0), (EV_KEY, keys::KEY_LEFTCTRL, 0)]
        );
    }

    #[test]
    fn the_browser_hosts_input_goes_through_the_same_writes() {
        use lucina_desk_web::Input;
        let (mut i, r) = injector();
        Input::move_to(&mut i, 300, 200);
        Input::button(&mut i, BTN_LEFT, true);
        Input::wheel(&mut i, 1, 2);
        Input::key(&mut i, keys::KEY_A, true);
        assert_eq!(
            *r.0.lock().unwrap(),
            vec![
                (EV_REL, REL_X, -FAR),
                (EV_REL, REL_Y, -FAR),
                (EV_REL, REL_X, 300),
                (EV_REL, REL_Y, 200),
                (EV_KEY, BTN_LEFT, 1),
                (EV_REL, REL_WHEEL, -2),
                (EV_REL, REL_HWHEEL, 1),
                (EV_KEY, keys::KEY_A, 1),
            ]
        );
        r.0.lock().unwrap().clear();
        i.release_all();
        let mut up = r.0.lock().unwrap().clone();
        up.sort();
        assert_eq!(up, vec![(EV_KEY, keys::KEY_A, 0), (EV_KEY, BTN_LEFT, 0)]);
    }

    #[test]
    fn text_is_typed() {
        let (mut i, r) = injector();
        i.key(KeyAction::Text("a".into()));
        assert_eq!(*r.0.lock().unwrap(), vec![(EV_KEY, keys::KEY_A, 1), (EV_KEY, keys::KEY_A, 0)]);
    }
}
