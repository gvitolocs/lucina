//! Input through the path real input takes: events written to an evdev device reach every program reading it (kernel patch
//! 0406, as Linux's `evdev_write`). The toolkit reads every device and does not mind which one a key comes from. Shared by
//! `lucina-app-desk`'s host and `lucina-devd`.
//!
//! The pointer has no absolute device on this laptop, so an absolute position is written as a move far to the corner, where the
//! toolkit's pointer stops, and then the distance to the position. All of it goes in one write, so that nobody draws in between.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io::Write;

use lucina_app::keys;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
/// Further than any panel: a move by this ends in the corner.
pub const FAR: i32 = 32000;

/// Evdev events of one `struct input_event`, 24 bytes on a 64-bit machine. The kernel stamps the time.
pub fn push(out: &mut Vec<u8>, type_: u16, code: u16, value: i32) {
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&type_.to_ne_bytes());
    out.extend_from_slice(&code.to_ne_bytes());
    out.extend_from_slice(&value.to_ne_bytes());
}

pub fn syn(out: &mut Vec<u8>) {
    push(out, EV_SYN, 0, 0);
}

/// Where the events go, so that the tests can read them.
pub trait Sink: Send {
    fn write_events(&mut self, bytes: &[u8]);
}

impl Sink for File {
    fn write_events(&mut self, bytes: &[u8]) {
        if let Err(e) = self.write_all(bytes) {
            eprintln!("lucina-inject: input: {e}");
        }
    }
}

pub struct Injector {
    sink: Box<dyn Sink>,
    down: BTreeSet<u16>,
    buttons: BTreeSet<u16>,
    last: Option<(i32, i32)>,
}

impl Injector {
    /// The first input device there is. All of them reach the same readers.
    pub fn open() -> Result<Injector, String> {
        let mut names: Vec<String> = std::fs::read_dir("/dev/input")
            .map_err(|e| format!("/dev/input: {e}"))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("event"))
            .collect();
        names.sort();
        for n in names {
            if let Ok(f) = OpenOptions::new()
                .write(true)
                .open(format!("/dev/input/{n}"))
            {
                return Ok(Injector::new(Box::new(f)));
            }
        }
        Err("no input device can be written to".into())
    }

    pub fn new(sink: Box<dyn Sink>) -> Injector {
        Injector {
            sink,
            down: BTreeSet::new(),
            buttons: BTreeSet::new(),
            last: None,
        }
    }

    fn write(&mut self, e: &[u8]) {
        if !e.is_empty() {
            self.sink.write_events(e);
        }
    }

    /// The pointer to a position on the screen, in its pixels. The same position again writes nothing.
    pub fn move_to(&mut self, x: i32, y: i32) {
        if self.last == Some((x, y)) {
            return;
        }
        self.last = Some((x, y));
        let mut e = Vec::with_capacity(24 * 5);
        push(&mut e, EV_REL, REL_X, -FAR);
        push(&mut e, EV_REL, REL_Y, -FAR);
        push(&mut e, EV_REL, REL_X, x.max(0));
        push(&mut e, EV_REL, REL_Y, y.max(0));
        syn(&mut e);
        self.write(&e);
    }

    /// Someone else may have moved the pointer since (the owner's own mouse): the next [`Self::move_to`] goes through the corner
    /// even to the same position.
    pub fn forget_position(&mut self) {
        self.last = None;
    }

    /// The pointer by a distance. Where it ends is the toolkit's to know, so the next [`Self::move_to`] goes through the corner.
    pub fn move_by(&mut self, dx: i32, dy: i32) {
        self.last = None;
        let mut e = Vec::with_capacity(24 * 3);
        push(&mut e, EV_REL, REL_X, dx);
        push(&mut e, EV_REL, REL_Y, dy);
        syn(&mut e);
        self.write(&e);
    }

    /// A mouse button, where the pointer already is.
    pub fn button(&mut self, code: u16, down: bool) {
        if down {
            self.buttons.insert(code);
        } else {
            self.buttons.remove(&code);
        }
        let mut e = Vec::with_capacity(24 * 2);
        push(&mut e, EV_KEY, code, down as i32);
        syn(&mut e);
        self.write(&e);
    }

    /// Wheel steps: `dy` up and down, `dx` sideways.
    pub fn wheel(&mut self, dy: i32, dx: i32) {
        let mut e = Vec::with_capacity(24 * 3);
        if dy != 0 {
            push(&mut e, EV_REL, REL_WHEEL, dy);
        }
        if dx != 0 {
            push(&mut e, EV_REL, REL_HWHEEL, dx);
        }
        syn(&mut e);
        self.write(&e);
    }

    fn key_event(&mut self, e: &mut Vec<u8>, code: u16, down: bool) {
        if down {
            self.down.insert(code);
        } else {
            self.down.remove(&code);
        }
        push(e, EV_KEY, code, down as i32);
        syn(e);
    }

    /// A key down or up, and nothing else.
    pub fn set_key(&mut self, code: u16, down: bool) {
        let mut e = Vec::with_capacity(24 * 2);
        self.key_event(&mut e, code, down);
        self.write(&e);
    }

    /// Down and up, with modifiers around them, in one write.
    pub fn press(&mut self, code: u16, modifiers: &[u16]) {
        let mut e = Vec::with_capacity(24 * 2 * (2 + 2 * modifiers.len()));
        for &m in modifiers {
            self.key_event(&mut e, m, true);
        }
        self.key_event(&mut e, code, true);
        self.key_event(&mut e, code, false);
        for &m in modifiers.iter().rev() {
            self.key_event(&mut e, m, false);
        }
        self.write(&e);
    }

    /// A character on the US keyboard the toolkit maps, with Shift where it needs it. False, and nothing written, for a
    /// character it has no key for.
    pub fn type_char(&mut self, c: char) -> bool {
        let found = match c {
            '\n' => Some((keys::KEY_ENTER, false)),
            '\t' => Some((keys::KEY_TAB, false)),
            c => keys::code_for(c),
        };
        let Some((code, shift)) = found else {
            return false;
        };
        let held: &[u16] = if shift { &[keys::KEY_LEFTSHIFT] } else { &[] };
        self.press(code, held);
        true
    }

    /// Nothing stays held.
    pub fn release_all(&mut self) {
        let mut e = Vec::new();
        for code in std::mem::take(&mut self.down) {
            push(&mut e, EV_KEY, code, 0);
            syn(&mut e);
        }
        for code in std::mem::take(&mut self.buttons) {
            push(&mut e, EV_KEY, code, 0);
            syn(&mut e);
        }
        self.write(&e);
    }
}

const MODIFIERS: [(&str, u16); 7] = [
    ("ctrl", keys::KEY_LEFTCTRL),
    ("control", keys::KEY_LEFTCTRL),
    ("shift", keys::KEY_LEFTSHIFT),
    ("alt", keys::KEY_LEFTALT),
    ("meta", keys::KEY_LEFTMETA),
    ("super", keys::KEY_LEFTMETA),
    ("win", keys::KEY_LEFTMETA),
];

fn modifier(name: &str) -> Option<u16> {
    MODIFIERS.iter().find(|(n, _)| *n == name).map(|&(_, c)| c)
}

/// A key by its evdev name, case aside: `enter`, `escape`, `f1`, `pagedown`, `a`, `5`, `/`… A single character must be one the
/// keyboard types without Shift.
pub fn key_code(name: &str) -> Option<u16> {
    let name = name.to_ascii_lowercase();
    if let Some(c) = modifier(&name) {
        return Some(c);
    }
    let code = match name.as_str() {
        "enter" | "return" => keys::KEY_ENTER,
        "escape" | "esc" => keys::KEY_ESC,
        "tab" => keys::KEY_TAB,
        "space" => keys::KEY_SPACE,
        "backspace" => keys::KEY_BACKSPACE,
        "delete" | "del" => keys::KEY_DELETE,
        "insert" => keys::KEY_INSERT,
        "home" => keys::KEY_HOME,
        "end" => keys::KEY_END,
        "pageup" => keys::KEY_PAGEUP,
        "pagedown" => keys::KEY_PAGEDOWN,
        "up" => keys::KEY_UP,
        "down" => keys::KEY_DOWN,
        "left" => keys::KEY_LEFT,
        "right" => keys::KEY_RIGHT,
        "capslock" => keys::KEY_CAPSLOCK,
        "minus" => keys::KEY_MINUS,
        "equal" => keys::KEY_EQUAL,
        "f1" => keys::KEY_F1,
        "f2" => keys::KEY_F2,
        "f3" => keys::KEY_F3,
        "f4" => 62,
        "f5" => 63,
        "f6" => 64,
        "f7" => 65,
        "f8" => 66,
        "f9" => 67,
        "f10" => 68,
        "f11" => 87,
        "f12" => 88,
        one => {
            let mut chars = one.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            return match keys::code_for(c) {
                Some((code, false)) => Some(code),
                _ => None,
            };
        }
    };
    Some(code)
}

/// `ctrl+shift+s`: the key, and the modifiers held around it in their order.
pub fn chord(spec: &str) -> Result<(u16, Vec<u16>), String> {
    let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(if spec.trim().is_empty() {
            "no key".into()
        } else {
            format!("\"{spec}\" has an empty part")
        });
    }
    if parts.len() > 4 {
        return Err("at most three modifiers and a key".into());
    }
    let (last, mods) = parts.split_last().ok_or("no key")?;
    let mut held = Vec::new();
    for m in mods {
        let code = modifier(&m.to_ascii_lowercase())
            .ok_or_else(|| format!("\"{m}\" is not a modifier"))?;
        if !held.contains(&code) {
            held.push(code);
        }
    }
    let code = key_code(last).ok_or_else(|| format!("unknown key \"{last}\""))?;
    Ok((code, held))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<Vec<(u16, u16, i32)>>>);

    impl Sink for Rec {
        fn write_events(&mut self, bytes: &[u8]) {
            assert_eq!(bytes.len() % 24, 0);
            for ev in bytes.chunks(24) {
                assert_eq!(&ev[..16], &[0u8; 16]);
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

    fn take(r: &Rec) -> Vec<(u16, u16, i32)> {
        std::mem::take(&mut *r.0.lock().unwrap())
    }

    #[test]
    fn a_position_goes_through_the_corner_once() {
        let (mut i, r) = injector();
        i.move_to(300, -5);
        assert_eq!(
            take(&r),
            vec![
                (EV_REL, REL_X, -FAR),
                (EV_REL, REL_Y, -FAR),
                (EV_REL, REL_X, 300),
                (EV_REL, REL_Y, 0)
            ]
        );
        i.move_to(300, -5);
        assert!(take(&r).is_empty());
        i.move_by(4, -2);
        assert_eq!(take(&r), vec![(EV_REL, REL_X, 4), (EV_REL, REL_Y, -2)]);
        // After a relative move the same position is written again.
        i.move_to(300, -5);
        assert_eq!(take(&r).len(), 4);
        i.forget_position();
        i.move_to(300, -5);
        assert_eq!(take(&r).len(), 4);
    }

    #[test]
    fn buttons_and_keys_are_released_at_the_end() {
        let (mut i, r) = injector();
        i.button(BTN_RIGHT, true);
        i.set_key(keys::KEY_A, true);
        assert_eq!(
            take(&r),
            vec![(EV_KEY, BTN_RIGHT, 1), (EV_KEY, keys::KEY_A, 1)]
        );
        i.release_all();
        assert_eq!(
            take(&r),
            vec![(EV_KEY, keys::KEY_A, 0), (EV_KEY, BTN_RIGHT, 0)]
        );
        i.release_all();
        assert!(take(&r).is_empty());
    }

    #[test]
    fn the_wheel_both_ways() {
        let (mut i, r) = injector();
        i.wheel(-2, 3);
        assert_eq!(
            take(&r),
            vec![(EV_REL, REL_WHEEL, -2), (EV_REL, REL_HWHEEL, 3)]
        );
    }

    #[test]
    fn a_press_holds_its_modifiers_around_it_and_leaves_nothing_down() {
        let (mut i, r) = injector();
        i.press(keys::KEY_S, &[keys::KEY_LEFTCTRL, keys::KEY_LEFTSHIFT]);
        assert_eq!(
            take(&r),
            vec![
                (EV_KEY, keys::KEY_LEFTCTRL, 1),
                (EV_KEY, keys::KEY_LEFTSHIFT, 1),
                (EV_KEY, keys::KEY_S, 1),
                (EV_KEY, keys::KEY_S, 0),
                (EV_KEY, keys::KEY_LEFTSHIFT, 0),
                (EV_KEY, keys::KEY_LEFTCTRL, 0)
            ]
        );
        i.release_all();
        assert!(take(&r).is_empty());
    }

    #[test]
    fn characters_are_typed_with_shift_where_needed() {
        let (mut i, r) = injector();
        assert!(i.type_char('a'));
        assert_eq!(
            take(&r),
            vec![(EV_KEY, keys::KEY_A, 1), (EV_KEY, keys::KEY_A, 0)]
        );
        assert!(i.type_char('A'));
        assert_eq!(
            take(&r),
            vec![
                (EV_KEY, keys::KEY_LEFTSHIFT, 1),
                (EV_KEY, keys::KEY_A, 1),
                (EV_KEY, keys::KEY_A, 0),
                (EV_KEY, keys::KEY_LEFTSHIFT, 0)
            ]
        );
        assert!(i.type_char('!'));
        assert_eq!(
            take(&r),
            vec![
                (EV_KEY, keys::KEY_LEFTSHIFT, 1),
                (EV_KEY, 2, 1),
                (EV_KEY, 2, 0),
                (EV_KEY, keys::KEY_LEFTSHIFT, 0)
            ]
        );
        assert!(i.type_char('\n'));
        assert_eq!(
            take(&r),
            vec![(EV_KEY, keys::KEY_ENTER, 1), (EV_KEY, keys::KEY_ENTER, 0)]
        );
        assert!(!i.type_char('é'));
        assert!(take(&r).is_empty());
    }

    #[test]
    fn keys_by_name() {
        assert_eq!(key_code("Enter"), Some(keys::KEY_ENTER));
        assert_eq!(key_code("esc"), Some(keys::KEY_ESC));
        assert_eq!(key_code("F1"), Some(keys::KEY_F1));
        assert_eq!(key_code("f12"), Some(88));
        assert_eq!(key_code("A"), Some(keys::KEY_A));
        assert_eq!(key_code("0"), Some(keys::KEY_0));
        assert_eq!(key_code("/"), Some(keys::KEY_SLASH));
        assert_eq!(key_code("super"), Some(keys::KEY_LEFTMETA));
        assert_eq!(key_code("!"), None);
        assert_eq!(key_code("foo"), None);
    }

    #[test]
    fn chords() {
        assert_eq!(
            chord("ctrl+shift+s"),
            Ok((keys::KEY_S, vec![keys::KEY_LEFTCTRL, keys::KEY_LEFTSHIFT]))
        );
        assert_eq!(chord(" Alt + F4 "), Ok((62, vec![keys::KEY_LEFTALT])));
        assert_eq!(chord("enter"), Ok((keys::KEY_ENTER, vec![])));
        assert_eq!(
            chord("ctrl+ctrl+c"),
            Ok((keys::KEY_C, vec![keys::KEY_LEFTCTRL]))
        );
        assert_eq!(chord("a+b"), Err("\"a\" is not a modifier".into()));
        assert_eq!(chord("ctrl+foo"), Err("unknown key \"foo\"".into()));
        assert_eq!(chord(""), Err("no key".into()));
        assert!(chord("+").is_err());
        assert!(chord("ctrl+").is_err());
        assert!(chord("ctrl+alt+shift+meta+a").is_err());
    }
}
