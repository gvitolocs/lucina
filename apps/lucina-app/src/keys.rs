//! Keys: modifier state from evdev key events, the US layout, software key repeat, and the bytes a
//! key sends to the terminal (what the Linux console and xterm send).
//!
//! Key codes are evdev's (`include/uapi/linux/input-event-codes.h`), the same for a USB keyboard,
//! the Book Go's i2c-hid one and QEMU's virtio keyboard.

use std::time::{Duration, Instant};

pub const KEY_ESC: u16 = 1;
pub const KEY_1: u16 = 2;
pub const KEY_2: u16 = 3;
pub const KEY_3: u16 = 4;
pub const KEY_4: u16 = 5;
pub const KEY_5: u16 = 6;
pub const KEY_0: u16 = 11;
pub const KEY_MINUS: u16 = 12;
pub const KEY_EQUAL: u16 = 13;
pub const KEY_R: u16 = 19;
pub const KEY_T: u16 = 20;
pub const KEY_I: u16 = 23;
pub const KEY_O: u16 = 24;
pub const KEY_LEFTBRACE: u16 = 26;
pub const KEY_RIGHTBRACE: u16 = 27;
pub const KEY_S: u16 = 31;
pub const KEY_D: u16 = 32;
pub const KEY_F: u16 = 33;
pub const KEY_G: u16 = 34;
pub const KEY_N: u16 = 49;
pub const KEY_SLASH: u16 = 53;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_ENTER: u16 = 28;
pub const KEY_A: u16 = 30;
pub const KEY_X: u16 = 45;
pub const KEY_C: u16 = 46;
pub const KEY_V: u16 = 47;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_SPACE: u16 = 57;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_F1: u16 = 59;
pub const KEY_POWER: u16 = 116;
/// What Linux's hid-input.c makes of the HID consumer usages 0x70 and 0x6F (display brightness down and up).
pub const KEY_BRIGHTNESSDOWN: u16 = 224;
pub const KEY_BRIGHTNESSUP: u16 = 225;
pub const KEY_F2: u16 = 60;
pub const KEY_F3: u16 = 61;
pub const KEY_KPENTER: u16 = 96;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_HOME: u16 = 102;
pub const KEY_UP: u16 = 103;
pub const KEY_PAGEUP: u16 = 104;
pub const KEY_LEFT: u16 = 105;
pub const KEY_RIGHT: u16 = 106;
pub const KEY_END: u16 = 107;
pub const KEY_DOWN: u16 = 108;
pub const KEY_PAGEDOWN: u16 = 109;
pub const KEY_INSERT: u16 = 110;
pub const KEY_DELETE: u16 = 111;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;

/// Mouse and touch buttons (also EV_KEY codes).
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_TOUCH: u16 = 0x14a;

const REPEAT_DELAY: Duration = Duration::from_millis(500);
const REPEAT_EVERY: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
    pub caps: bool,
}

/// A key press (or a repeat) of a key that is not a modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Press {
    pub code: u16,
    pub mods: Mods,
}

/// Modifier state and software repeat. The kernel's own repeat events (value 2) are ignored, so a
/// held key repeats at the same rate whatever the keyboard driver does.
#[derive(Default)]
pub struct Keyboard {
    lshift: bool,
    rshift: bool,
    lctrl: bool,
    rctrl: bool,
    lalt: bool,
    ralt: bool,
    meta: u8,
    caps: bool,
    held: Option<(u16, Instant)>,
}

impl Keyboard {
    pub fn mods(&self) -> Mods {
        Mods {
            shift: self.lshift || self.rshift,
            ctrl: self.lctrl || self.rctrl,
            alt: self.lalt || self.ralt,
            meta: self.meta != 0,
            caps: self.caps,
        }
    }

    /// One EV_KEY event (`value` 0 = release, 1 = press, 2 = kernel repeat).
    pub fn feed(&mut self, code: u16, value: i32, now: Instant) -> Option<Press> {
        let down = value != 0;
        match code {
            KEY_LEFTSHIFT => self.lshift = down,
            KEY_RIGHTSHIFT => self.rshift = down,
            KEY_LEFTCTRL => self.lctrl = down,
            KEY_RIGHTCTRL => self.rctrl = down,
            KEY_LEFTALT => self.lalt = down,
            KEY_RIGHTALT => self.ralt = down,
            KEY_LEFTMETA | KEY_RIGHTMETA => {
                let bit = if code == KEY_LEFTMETA { 1 } else { 2 };
                if down {
                    self.meta |= bit
                } else {
                    self.meta &= !bit
                }
            }
            KEY_CAPSLOCK => {
                if value == 1 {
                    self.caps = !self.caps;
                }
            }
            c if c >= BTN_LEFT => {}
            _ => {
                return match value {
                    1 => {
                        self.held = Some((code, now + REPEAT_DELAY));
                        Some(Press {
                            code,
                            mods: self.mods(),
                        })
                    }
                    0 => {
                        if self.held.is_some_and(|(h, _)| h == code) {
                            self.held = None;
                        }
                        None
                    }
                    _ => None,
                };
            }
        }
        // A modifier change ends the repeat (Shift released while 'a' is held, say).
        self.held = None;
        None
    }

    /// When the held key repeats next, if one is held.
    pub fn next_repeat(&self) -> Option<Instant> {
        self.held.map(|(_, t)| t)
    }

    /// The repeat that is due at `now`, if any.
    pub fn repeat(&mut self, now: Instant) -> Option<Press> {
        let (code, due) = self.held?;
        if now < due {
            return None;
        }
        self.held = Some((code, now + REPEAT_EVERY));
        Some(Press {
            code,
            mods: self.mods(),
        })
    }

    /// Forget held keys (a device went away, or the view changed).
    pub fn release_all(&mut self) {
        *self = Keyboard {
            caps: self.caps,
            ..Keyboard::default()
        };
    }
}

/// The character a key types on a US keyboard, if it types one.
pub fn char_for(code: u16, m: Mods) -> Option<char> {
    const PLAIN: &[u8; 58] =
        b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
    const SHIFT: &[u8; 58] =
        b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";
    let keypad = match code {
        71 => Some('7'),
        72 => Some('8'),
        73 => Some('9'),
        74 => Some('-'),
        75 => Some('4'),
        76 => Some('5'),
        77 => Some('6'),
        78 => Some('+'),
        79 => Some('1'),
        80 => Some('2'),
        81 => Some('3'),
        82 => Some('0'),
        83 => Some('.'),
        98 => Some('/'),
        _ => None,
    };
    if keypad.is_some() {
        return keypad;
    }
    let i = code as usize;
    if i >= PLAIN.len() {
        return None;
    }
    let plain = PLAIN[i];
    if !(plain.is_ascii_graphic() || plain == b' ') {
        return None;
    }
    let shifted = if plain.is_ascii_lowercase() {
        m.shift != m.caps
    } else {
        m.shift
    };
    Some(if shifted { SHIFT[i] } else { plain } as char)
}

/// The key and Shift state that type `c` on a US keyboard.
pub fn code_for(c: char) -> Option<(u16, bool)> {
    for code in 1..58u16 {
        for shift in [false, true] {
            let m = Mods {
                shift,
                ..Mods::default()
            };
            if char_for(code, m) == Some(c) {
                return Some((code, shift));
            }
        }
    }
    None
}

/// The bytes a key sends to the program in the terminal (empty for keys that send nothing).
pub fn terminal_bytes(p: Press, app_cursor: bool) -> Vec<u8> {
    let m = p.mods;
    let arrow = |c: u8| {
        if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let mut out = match p.code {
        KEY_ENTER | KEY_KPENTER => vec![b'\r'],
        KEY_BACKSPACE => vec![0x7f],
        KEY_TAB => {
            if m.shift {
                b"\x1b[Z".to_vec()
            } else {
                vec![b'\t']
            }
        }
        KEY_ESC => vec![0x1b],
        KEY_UP => arrow(b'A'),
        KEY_DOWN => arrow(b'B'),
        KEY_RIGHT => arrow(b'C'),
        KEY_LEFT => arrow(b'D'),
        KEY_HOME => arrow(b'H'),
        KEY_END => arrow(b'F'),
        KEY_INSERT => b"\x1b[2~".to_vec(),
        KEY_DELETE => b"\x1b[3~".to_vec(),
        KEY_PAGEUP => b"\x1b[5~".to_vec(),
        KEY_PAGEDOWN => b"\x1b[6~".to_vec(),
        code => match char_for(code, m) {
            None => return Vec::new(),
            Some(c) if m.ctrl => match c.to_ascii_lowercase() {
                // Ctrl+A..Z -> 0x01..0x1a; the usual extras for Ctrl+Space, [, \, ], ^, _.
                l @ 'a'..='z' => vec![l as u8 - b'a' + 1],
                ' ' | '@' | '2' => vec![0],
                '[' | '3' => vec![0x1b],
                '\\' | '4' => vec![0x1c],
                ']' | '5' => vec![0x1d],
                '^' | '6' => vec![0x1e],
                '_' | '-' | '7' => vec![0x1f],
                '8' | '?' => vec![0x7f],
                other => other.to_string().into_bytes(),
            },
            Some(c) => c.to_string().into_bytes(),
        },
    };
    // Alt sends ESC first (xterm's metaSendsEscape, the Linux console's default).
    if m.alt && !out.is_empty() && out[0] != 0x1b {
        out.insert(0, 0x1b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: u16, mods: Mods) -> Press {
        Press { code, mods }
    }

    #[test]
    fn us_layout() {
        let none = Mods::default();
        let shift = Mods {
            shift: true,
            ..none
        };
        let caps = Mods { caps: true, ..none };
        assert_eq!(char_for(30, none), Some('a'));
        assert_eq!(char_for(30, shift), Some('A'));
        assert_eq!(char_for(30, caps), Some('A'));
        assert_eq!(
            char_for(
                30,
                Mods {
                    caps: true,
                    shift: true,
                    ..none
                }
            ),
            Some('a')
        );
        assert_eq!(char_for(2, shift), Some('!'));
        assert_eq!(char_for(2, caps), Some('1'));
        assert_eq!(char_for(5, shift), Some('$'));
        assert_eq!(char_for(10, shift), Some('('));
        assert_eq!(char_for(9, shift), Some('*'));
        assert_eq!(char_for(12, none), Some('-'));
        assert_eq!(char_for(40, shift), Some('"'));
        assert_eq!(char_for(43, none), Some('\\'));
        assert_eq!(char_for(53, none), Some('/'));
        assert_eq!(char_for(KEY_SPACE, none), Some(' '));
        assert_eq!(char_for(KEY_ENTER, none), None);
        assert_eq!(char_for(KEY_LEFTSHIFT, none), None);
        assert_eq!(char_for(KEY_F1, none), None);
        assert_eq!(char_for(79, none), Some('1'));
    }

    #[test]
    fn codes_type_their_characters_back() {
        for c in "aZ5%/ ~".chars() {
            let (code, shift) = code_for(c).unwrap();
            let m = Mods {
                shift,
                ..Mods::default()
            };
            assert_eq!(char_for(code, m), Some(c));
        }
        assert_eq!(code_for('é'), None);
    }

    #[test]
    fn terminal_sequences() {
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        assert_eq!(terminal_bytes(press(KEY_ENTER, none), false), b"\r");
        assert_eq!(terminal_bytes(press(KEY_BACKSPACE, none), false), b"\x7f");
        assert_eq!(terminal_bytes(press(46, ctrl), false), b"\x03"); // Ctrl+C
        assert_eq!(terminal_bytes(press(32, ctrl), false), b"\x04"); // Ctrl+D
        assert_eq!(terminal_bytes(press(KEY_UP, none), false), b"\x1b[A");
        assert_eq!(terminal_bytes(press(KEY_UP, none), true), b"\x1bOA");
        assert_eq!(terminal_bytes(press(KEY_DELETE, none), false), b"\x1b[3~");
        assert_eq!(
            terminal_bytes(press(48, Mods { alt: true, ..none }), false),
            b"\x1bb"
        );
        assert_eq!(terminal_bytes(press(KEY_F1, none), false), b"");
    }

    #[test]
    fn modifiers_and_repeat() {
        let t0 = Instant::now();
        let mut k = Keyboard::default();
        assert_eq!(k.feed(KEY_LEFTSHIFT, 1, t0), None);
        let p = k.feed(30, 1, t0).unwrap();
        assert!(p.mods.shift);
        // Kernel repeats are ignored; ours start after the delay.
        assert_eq!(k.feed(30, 2, t0), None);
        assert_eq!(k.repeat(t0 + Duration::from_millis(100)), None);
        assert_eq!(k.repeat(t0 + REPEAT_DELAY).map(|p| p.code), Some(30));
        assert_eq!(k.next_repeat(), Some(t0 + REPEAT_DELAY + REPEAT_EVERY));
        k.feed(30, 0, t0 + REPEAT_DELAY);
        assert_eq!(k.next_repeat(), None);
        assert_eq!(k.feed(KEY_LEFTSHIFT, 0, t0), None);
        assert!(!k.mods().shift);
        // Caps Lock toggles on press only.
        k.feed(KEY_CAPSLOCK, 1, t0);
        k.feed(KEY_CAPSLOCK, 0, t0);
        assert!(k.mods().caps);
        // Mouse buttons are not keys.
        assert_eq!(k.feed(BTN_LEFT, 1, t0), None);
    }
}
