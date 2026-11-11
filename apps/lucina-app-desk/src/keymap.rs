//! A key on this keyboard, as an evdev code, in the codes of the host's system. RustDesk's Map mode moves keys by position:
//! the host's layout decides what they mean, as it does for a local keyboard.

/// What the host says it runs (`PeerInfo.platform`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Host {
    Linux,
    Mac,
    Windows,
}

impl Host {
    pub fn from_platform(platform: &str) -> Host {
        match platform {
            "Mac OS" | "macOS" => Host::Mac,
            "Windows" => Host::Windows,
            _ => Host::Linux,
        }
    }
}

/// The host's code for this key, or `None` for a key it has no code for.
pub fn host_code(host: Host, evdev: u16) -> Option<u32> {
    match host {
        // An X keycode is the evdev code plus 8; RustDesk's Wayland path expects the same.
        Host::Linux => Some(evdev as u32 + 8),
        Host::Mac => mac_code(evdev),
        Host::Windows => windows_code(evdev),
    }
}

/// Set 1 scan codes. Up to the keypad they equal evdev's; the ones after it carry the E0 prefix.
fn windows_code(evdev: u16) -> Option<u32> {
    Some(match evdev {
        1..=83 => evdev as u32,
        87 => 0x57,
        88 => 0x58,
        96 => 0xe01c,
        97 => 0xe01d,
        98 => 0xe035,
        99 => 0xe037,
        100 => 0xe038,
        102 => 0xe047,
        103 => 0xe048,
        104 => 0xe049,
        105 => 0xe04b,
        106 => 0xe04d,
        107 => 0xe04f,
        108 => 0xe050,
        109 => 0xe051,
        110 => 0xe052,
        111 => 0xe053,
        125 => 0xe05b,
        126 => 0xe05c,
        127 => 0xe05d,
        _ => return None,
    })
}

/// macOS virtual key codes (`kVK_*` in Carbon's Events.h).
fn mac_code(evdev: u16) -> Option<u32> {
    const LETTERS_AND_DIGITS: &[(u16, u32)] = &[
        (1, 53),   // Esc
        (2, 18),   // 1
        (3, 19),
        (4, 20),
        (5, 21),
        (6, 23),   // 5
        (7, 22),   // 6
        (8, 26),
        (9, 28),
        (10, 25),
        (11, 29),  // 0
        (12, 27),  // -
        (13, 24),  // =
        (14, 51),  // Backspace
        (15, 48),  // Tab
        (16, 12),  // Q
        (17, 13),
        (18, 14),
        (19, 15),
        (20, 17),  // T
        (21, 16),  // Y
        (22, 32),
        (23, 34),
        (24, 31),
        (25, 35),  // P
        (26, 33),  // [
        (27, 30),  // ]
        (28, 36),  // Enter
        (29, 59),  // Left Ctrl
        (30, 0),   // A
        (31, 1),
        (32, 2),
        (33, 3),
        (34, 5),   // G
        (35, 4),   // H
        (36, 38),
        (37, 40),
        (38, 37),  // L
        (39, 41),  // ;
        (40, 39),  // '
        (41, 50),  // `
        (42, 56),  // Left Shift
        (43, 42),  // backslash
        (44, 6),   // Z
        (45, 7),
        (46, 8),
        (47, 9),
        (48, 11),  // B
        (49, 45),
        (50, 46),  // M
        (51, 43),  // ,
        (52, 47),  // .
        (53, 44),  // /
        (54, 60),  // Right Shift
        (55, 67),  // keypad *
        (56, 58),  // Left Alt = Option
        (57, 49),  // Space
        (58, 57),  // Caps Lock
        (59, 122), // F1
        (60, 120),
        (61, 99),
        (62, 118),
        (63, 96),
        (64, 97),
        (65, 98),
        (66, 100),
        (67, 101),
        (68, 109), // F10
        (87, 103), // F11
        (88, 111), // F12
        (97, 62),  // Right Ctrl
        (100, 61), // Right Alt
        (102, 115), // Home
        (103, 126), // Up
        (104, 116), // Page Up
        (105, 123), // Left
        (106, 124), // Right
        (107, 119), // End
        (108, 125), // Down
        (109, 121), // Page Down
        (111, 117), // Delete
        (125, 55), // Left Meta = Command
        (126, 54), // Right Meta
    ];
    LETTERS_AND_DIGITS.iter().find(|(e, _)| *e == evdev).map(|(_, k)| *k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_key_on_each_system() {
        // A is evdev 30.
        assert_eq!(host_code(Host::Linux, 30), Some(38));
        assert_eq!(host_code(Host::Mac, 30), Some(0));
        assert_eq!(host_code(Host::Windows, 30), Some(30));
        // Up is evdev 103.
        assert_eq!(host_code(Host::Linux, 103), Some(111));
        assert_eq!(host_code(Host::Mac, 103), Some(126));
        assert_eq!(host_code(Host::Windows, 103), Some(0xe048));
        // The Mac's Command key is the Meta key here.
        assert_eq!(host_code(Host::Mac, 125), Some(55));
        assert_eq!(host_code(Host::Mac, 400), None);
    }

    #[test]
    fn the_mac_table_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for evdev in 0..256 {
            if let Some(c) = host_code(Host::Mac, evdev) {
                assert!(seen.insert(c), "kVK {c} twice, at evdev {evdev}");
            }
        }
        assert!(seen.len() > 75);
    }

    #[test]
    fn platforms() {
        assert_eq!(Host::from_platform("Mac OS"), Host::Mac);
        assert_eq!(Host::from_platform("Windows"), Host::Windows);
        assert_eq!(Host::from_platform("Linux"), Host::Linux);
    }
}
