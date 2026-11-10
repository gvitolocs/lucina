//! X keysyms, as voxl's web client sends them (`web/src/client/keyboard.ts`: Latin-1 characters as themselves, named keys
//! as 0xffxx), to Linux evdev key codes (`linux/input-event-codes.h`) on a US layout.
//!
//! The keysym of a character already says whether Shift was involved ("A" vs "a", "!" vs "1"), so the table says which
//! keysyms need Shift: the caller presses or releases Shift around a key whose need differs from what is held.

/// An evdev key code and whether a US keyboard needs Shift held to type it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mapped {
    pub code: u16,
    pub shift: bool,
}

pub const KEY_LEFTSHIFT: u16 = 42;

const fn k(code: u16) -> Option<Mapped> {
    Some(Mapped { code, shift: false })
}

const fn s(code: u16) -> Option<Mapped> {
    Some(Mapped { code, shift: true })
}

/// Evdev codes of a..z, in alphabetical order (they follow the QWERTY rows, not the alphabet).
const LETTERS: [u16; 26] = [
    30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, // a b c d e f g h i j k l m
    49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44, // n o p q r s t u v w x y z
];
/// Evdev codes of 0..9: KEY_0 is 11, after KEY_9.
const DIGITS: [u16; 10] = [11, 2, 3, 4, 5, 6, 7, 8, 9, 10];

/// `None` for keysyms a US keyboard has no key for (the caller drops them).
pub fn map(keysym: u32) -> Option<Mapped> {
    match keysym {
        0x20 => k(57),
        0x61..=0x7a => k(LETTERS[(keysym - 0x61) as usize]),
        0x41..=0x5a => s(LETTERS[(keysym - 0x41) as usize]),
        0x30..=0x39 => k(DIGITS[(keysym - 0x30) as usize]),
        // Shifted digits row.
        0x21 => s(2),  // !
        0x40 => s(3),  // @
        0x23 => s(4),  // #
        0x24 => s(5),  // $
        0x25 => s(6),  // %
        0x5e => s(7),  // ^
        0x26 => s(8),  // &
        0x2a => s(9),  // *
        0x28 => s(10), // (
        0x29 => s(11), // )
        // Punctuation keys, both halves.
        0x60 => k(41), // `
        0x7e => s(41), // ~
        0x2d => k(12), // -
        0x5f => s(12), // _
        0x3d => k(13), // =
        0x2b => s(13), // +
        0x5b => k(26), // [
        0x7b => s(26), // {
        0x5d => k(27), // ]
        0x7d => s(27), // }
        0x5c => k(43), // backslash
        0x7c => s(43), // |
        0x3b => k(39), // ;
        0x3a => s(39), // :
        0x27 => k(40), // '
        0x22 => s(40), // "
        0x2c => k(51), // ,
        0x3c => s(51), // <
        0x2e => k(52), // .
        0x3e => s(52), // >
        0x2f => k(53), // /
        0x3f => s(53), // ?
        // Editing and movement.
        0xff08 => k(14),  // BackSpace
        0xff09 => k(15),  // Tab
        0xfe20 => s(15),  // ISO_Left_Tab
        0xff0d => k(28),  // Return
        0xff8d => k(96),  // KP_Enter
        0xff1b => k(1),   // Escape
        0xffff => k(111), // Delete
        0xff63 => k(110), // Insert
        0xff50 => k(102), // Home
        0xff57 => k(107), // End
        0xff55 => k(104), // Prior
        0xff56 => k(109), // Next
        0xff51 => k(105), // Left
        0xff52 => k(103), // Up
        0xff53 => k(106), // Right
        0xff54 => k(108), // Down
        // F1-F10 are 59-68, F11 and F12 87 and 88, F13-F24 183-194.
        0xffbe..=0xffc7 => k(59 + (keysym - 0xffbe) as u16),
        0xffc8 => k(87),
        0xffc9 => k(88),
        0xffca..=0xffd5 => k(183 + (keysym - 0xffca) as u16),
        // Modifiers.
        0xffe1 => k(KEY_LEFTSHIFT),
        0xffe2 => k(54),          // Shift_R
        0xffe3 => k(29),          // Control_L
        0xffe4 => k(97),          // Control_R
        0xffe5 => k(58),          // Caps_Lock
        0xffe7 | 0xffe9 => k(56), // Meta_L, Alt_L
        0xffe8 | 0xffea | 0xfe03 => k(100), // Meta_R, Alt_R, ISO_Level3_Shift (AltGr)
        0xffeb => k(125),         // Super_L
        0xffec => k(126),         // Super_R
        // Others.
        0xff67 => k(127), // Menu -> KEY_COMPOSE
        0xff61 => k(99),  // Print -> KEY_SYSRQ
        0xff13 => k(119), // Pause
        0xff14 => k(70),  // Scroll_Lock
        0xff7f => k(69),  // Num_Lock
        // Keypad.
        0xffb0 => k(82),
        0xffb1 => k(79),
        0xffb2 => k(80),
        0xffb3 => k(81),
        0xffb4 => k(75),
        0xffb5 => k(76),
        0xffb6 => k(77),
        0xffb7 => k(71),
        0xffb8 => k(72),
        0xffb9 => k(73),
        0xffaa => k(55), // KP_Multiply
        0xffab => k(78), // KP_Add
        0xffad => k(74), // KP_Subtract
        0xffae => k(83), // KP_Decimal
        0xffaf => k(98), // KP_Divide
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(code: u16, shift: bool) -> Option<Mapped> {
        Some(Mapped { code, shift })
    }

    #[test]
    fn every_printable_ascii_maps() {
        for c in 0x20..=0x7e {
            assert!(map(c).is_some(), "{c:#x}");
        }
        for c in 0x61..=0x7a {
            let (lo, up) = (map(c).unwrap(), map(c - 0x20).unwrap());
            assert_eq!(lo.code, up.code);
            assert!(!lo.shift && up.shift);
        }
    }

    #[test]
    fn spot_checks_against_input_event_codes() {
        let cases = [
            (0x61, 30, false), // a
            (0x71, 16, false), // q
            (0x77, 17, false), // w
            (0x7a, 44, false), // z
            (0x6d, 50, false), // m
            (0x41, 30, true),
            (0x30, 11, false),
            (0x31, 2, false),
            (0x21, 2, true),
            (0x40, 3, true),
            (0x29, 11, true),
            (0x7e, 41, true),
            (0x60, 41, false),
            (0x7c, 43, true),
            (0x3a, 39, true),
            (0x22, 40, true),
            (0x3f, 53, true),
            (0xff0d, 28, false),
            (0xffbe, 59, false),
            (0xffc7, 68, false),
            (0xffc9, 88, false),
            (0xffd5, 194, false),
            (0xfe20, 15, true),
            (0xffe1, 42, false),
            (0xffeb, 125, false),
            (0xffb0, 82, false),
        ];
        for (ks, code, shift) in cases {
            assert_eq!(map(ks), m(code, shift), "{ks:#x}");
        }
    }

    #[test]
    fn unicode_and_latin1_have_no_key() {
        assert_eq!(map(0x0100_0000 + 0xe9), None);
        assert_eq!(map(0xe9), None);
        assert_eq!(map(0), None);
    }

    #[test]
    fn no_two_characters_share_a_key_and_shift() {
        let mut seen = std::collections::HashMap::new();
        for c in 0x20..=0x7e {
            let k = map(c).unwrap();
            if let Some(prev) = seen.insert((k.code, k.shift), c) {
                panic!("{prev:#x} and {c:#x} both map to {k:?}");
            }
        }
    }
}
