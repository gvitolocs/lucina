//! Which system a game file is for, from its extension.

/// A system Fortuna can open a game for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Platform {
    /// Nintendo DS (`.nds`): Fortuna's own boot path.
    Ds = 0,
    /// DSi-enhanced or DSi-only (`.dsi`).
    Dsi = 1,
    /// Game Boy Advance (`.gba`).
    Gba = 2,
    /// Game Boy (`.gb`).
    Gb = 3,
    /// Game Boy Color (`.gbc`).
    Gbc = 4,
}

impl Platform {
    pub const ALL: [Platform; 5] = [Platform::Ds, Platform::Dsi, Platform::Gba, Platform::Gb, Platform::Gbc];

    /// The bit of this platform in a [`PlatformMask`].
    pub const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    /// Whether games for this platform carry a DS banner (icon, palette and titles) Fortuna can read.
    pub const fn has_banner(self) -> bool {
        matches!(self, Platform::Ds | Platform::Dsi)
    }

    /// The program that opens games of this platform (the same one [`crate::names::kind`] picks).
    pub const fn runner(self) -> crate::names::Runner {
        match self {
            Platform::Ds | Platform::Dsi => crate::names::Runner::Nds,
            Platform::Gba => crate::names::Runner::Gba,
            Platform::Gb | Platform::Gbc => crate::names::Runner::GameBoy,
        }
    }

    /// The short label the filter pills show.
    pub const fn label(self) -> &'static str {
        match self {
            Platform::Ds | Platform::Dsi => "DS",
            Platform::Gba => "GBA",
            Platform::Gb | Platform::Gbc => "Game Boy",
        }
    }

    pub const fn from_u8(v: u8) -> Option<Platform> {
        match v {
            0 => Some(Platform::Ds),
            1 => Some(Platform::Dsi),
            2 => Some(Platform::Gba),
            3 => Some(Platform::Gb),
            4 => Some(Platform::Gbc),
            _ => None,
        }
    }
}

/// A set of platforms: the filter pills. The pills group DS with DSi and Game Boy with Game Boy Color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformMask(pub u8);

impl PlatformMask {
    pub const ALL: PlatformMask = PlatformMask(0x1f);
    pub const DS: PlatformMask = PlatformMask(Platform::Ds.bit() | Platform::Dsi.bit());
    pub const GBA: PlatformMask = PlatformMask(Platform::Gba.bit());
    pub const GAME_BOY: PlatformMask = PlatformMask(Platform::Gb.bit() | Platform::Gbc.bit());

    pub const fn has(self, p: Platform) -> bool {
        self.0 & p.bit() != 0
    }
}

/// The platform of a file name and the length of its stem (the name without the dot and extension), or `None` for
/// a file that is not a game. Case does not matter (`GAME.NDS` is a DS game).
pub fn from_name(name: &[u8]) -> Option<(Platform, usize)> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    if dot == 0 {
        return None; // ".nds" alone is a hidden file, not a game
    }
    let ext = &name[dot + 1..];
    let p = if eq_ascii(ext, b"nds") {
        Platform::Ds
    } else if eq_ascii(ext, b"dsi") {
        Platform::Dsi
    } else if eq_ascii(ext, b"gba") {
        Platform::Gba
    } else if eq_ascii(ext, b"gbc") {
        Platform::Gbc
    } else if eq_ascii(ext, b"gb") {
        Platform::Gb
    } else {
        return None;
    };
    Some((p, dot))
}

fn eq_ascii(a: &[u8], lower: &[u8]) -> bool {
    a.len() == lower.len() && a.iter().zip(lower).all(|(&x, &y)| crate::rules::fold(x) == y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(from_name(b"Mario Kart DS.nds"), Some((Platform::Ds, 13)));
        assert_eq!(from_name(b"GAME.NDS"), Some((Platform::Ds, 4)));
        assert_eq!(from_name(b"x.dsi"), Some((Platform::Dsi, 1)));
        assert_eq!(from_name(b"Golden Sun.gba"), Some((Platform::Gba, 10)));
        assert_eq!(from_name(b"tetris.gb"), Some((Platform::Gb, 6)));
        assert_eq!(from_name(b"zelda.GBC"), Some((Platform::Gbc, 5)));
        assert_eq!(from_name(b"a.b.gba"), Some((Platform::Gba, 3)));
    }

    #[test]
    fn not_games() {
        assert_eq!(from_name(b".nds"), None);
        assert_eq!(from_name(b"readme.txt"), None);
        assert_eq!(from_name(b"save.sav"), None);
        assert_eq!(from_name(b"noext"), None);
        assert_eq!(from_name(b"game.nds.bak"), None);
        assert_eq!(from_name(b"game.gbaa"), None);
    }

    #[test]
    fn agrees_with_names_kind() {
        use crate::names::{kind, Kind};
        for n in ["a.nds", "B.DSI", "c.gba", "d.gb", "e.GBC", "f.txt", ".nds", "g", "h.nds.bak", "i.png"] {
            let ours = from_name(n.as_bytes()).map(|(p, _)| p.runner());
            let theirs = match kind(n, false) {
                Kind::Game(r) => Some(r),
                _ => None,
            };
            assert_eq!(ours, theirs, "{n}");
            if let Some((_, stem)) = from_name(n.as_bytes()) {
                assert_eq!(&n[..stem], crate::names::stem(n));
            }
        }
    }

    #[test]
    fn masks() {
        assert!(PlatformMask::DS.has(Platform::Dsi));
        assert!(!PlatformMask::DS.has(Platform::Gba));
        assert!(PlatformMask::GAME_BOY.has(Platform::Gbc));
        for p in Platform::ALL {
            assert!(PlatformMask::ALL.has(p));
            assert_eq!(Platform::from_u8(p as u8), Some(p));
        }
        assert_eq!(Platform::from_u8(5), None);
    }
}
