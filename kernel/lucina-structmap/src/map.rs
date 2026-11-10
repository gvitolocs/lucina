//! A program's published state, by name: the map file's format, and the reader that turns the program's own copy of each
//! published region into named values. Every read goes through [`crate::rules`]: [`rules::copy_offset`] for the bounds (inside the
//! map's region and inside the copy), [`rules::read_plan`] for excluded fields, [`rules::find_map`] for the build id.
//!
//! A map lists fields at addresses inside named regions, in the program's own address space (for a Game Boy game, the console's
//! bus: WRAM is `0xC000..0xE000`). The program hands the reader a copy of each region it publishes, taken under its own lock, so a
//! value never tears across a frame. Nothing here reads another process.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::rules::{self, Lookup};

/// The most fields one map may name (the screen tree holds at most 4096 nodes in all).
pub const MAX_FIELDS: usize = 1024;

/// A map file.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateMap {
    pub program: String,
    /// Lowercase hex. For a program we build, its ELF build id; for a game, the SHA-1 of its ROM.
    pub build_id: String,
    pub regions: BTreeMap<String, RegionSpec>,
    #[serde(default)]
    pub enums: BTreeMap<String, Vec<String>>,
    pub fields: Vec<Field>,
}

/// One region the program publishes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionSpec {
    pub base: u64,
    pub size: u64,
}

/// One named field.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    pub region: String,
    pub addr: u64,
    #[serde(rename = "type")]
    pub ty: Ty,
    /// Bytes, for `bcd`, `gb_text` and `bytes`.
    #[serde(default)]
    pub len: u64,
    /// A key of [`StateMap::enums`]: the value names an entry.
    #[serde(default, rename = "enum")]
    pub enum_name: Option<String>,
    /// The entry is `value - enum_base`.
    #[serde(default)]
    pub enum_base: u64,
    /// Why this field is never read (for example `credential`). The reader names it and returns nothing else.
    #[serde(default)]
    pub excluded: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ty {
    U8,
    U16be,
    U16le,
    U32le,
    U64le,
    Bool,
    /// Binary-coded decimal, most significant byte first.
    Bcd,
    /// Text in the Game Boy game charmap, ended by 0x50.
    GbText,
    /// Tiles of a Game Boy Pokémon screen row: every byte through the charmap, no terminator (box borders as `+`, `-`, `|`).
    GbTiles,
    /// One byte of flags; with an enum, the names of the bits that are set (bit 0 is the first entry).
    Bits,
    /// Raw bytes, shown as hex.
    Bytes,
}

impl Ty {
    /// The bytes a field of this type reads: fixed, or the field's `len`.
    pub fn width(self, len: u64) -> u64 {
        match self {
            Ty::U8 | Ty::Bool | Ty::Bits => 1,
            Ty::U16be | Ty::U16le => 2,
            Ty::U32le => 4,
            Ty::U64le => 8,
            Ty::Bcd | Ty::GbText | Ty::GbTiles | Ty::Bytes => len,
        }
    }

    fn takes_len(self) -> bool {
        matches!(self, Ty::Bcd | Ty::GbText | Ty::GbTiles | Ty::Bytes)
    }
}

/// One value read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value {
    /// What she reads: `5`, `PALLET_TOWN (0)`, `BOULDER, CASCADE`, `LUCINA`.
    pub text: String,
    /// The number behind it, for numbers, enums, BCD and bit sets.
    pub number: Option<u64>,
}

impl StateMap {
    /// A map from its JSON text, checked ([`StateMap::check`]).
    pub fn parse(json: &str) -> Result<StateMap, String> {
        let m: StateMap = serde_json::from_str(json).map_err(|e| format!("map: {e}"))?;
        m.check()?;
        Ok(m)
    }

    /// The map holds together: a usable build id, regions that exist and are not empty, unique field names, every field wholly
    /// inside its region (by the same rule a read uses), enums that exist.
    pub fn check(&self) -> Result<(), String> {
        let id = decode_hex(&self.build_id)
            .ok_or_else(|| format!("map {}: build_id is not hex", self.program))?;
        if id.len() < rules::MIN_ID || id.len() > rules::MAX_ID {
            return Err(format!(
                "map {}: build_id is {} bytes, not {}-{}",
                self.program,
                id.len(),
                rules::MIN_ID,
                rules::MAX_ID
            ));
        }
        if self.fields.len() > MAX_FIELDS {
            return Err(format!(
                "map {}: {} fields, at most {MAX_FIELDS}",
                self.program,
                self.fields.len()
            ));
        }
        for (name, r) in &self.regions {
            if r.size == 0 || r.base.checked_add(r.size).is_none() {
                return Err(format!(
                    "map {}: region {name} is empty or runs past the address space",
                    self.program
                ));
            }
        }
        let mut names = BTreeSet::new();
        for f in &self.fields {
            if !names.insert(f.name.as_str()) {
                return Err(format!(
                    "map {}: two fields are called {}",
                    self.program, f.name
                ));
            }
            if f.name.is_empty() || f.name.len() > 80 {
                return Err(format!(
                    "map {}: field name {:?} is empty or over 80 bytes",
                    self.program, f.name
                ));
            }
            let r = self
                .regions
                .get(&f.region)
                .ok_or_else(|| format!("field {}: no region {}", f.name, f.region))?;
            if !f.ty.takes_len() && f.len != 0 {
                return Err(format!(
                    "field {}: a {:?} has a fixed size, len is not for it",
                    f.name, f.ty
                ));
            }
            let width = f.ty.width(f.len);
            if rules::copy_offset(r.base, r.size, r.size, f.addr, width).is_none() {
                return Err(format!(
                    "field {}: {width} bytes at {:#x} are not inside region {}",
                    f.name, f.addr, f.region
                ));
            }
            if let Some(e) = &f.enum_name
                && !self.enums.contains_key(e)
            {
                return Err(format!("field {}: no enum {e}", f.name));
            }
        }
        Ok(())
    }

    /// The fields that may be read, in map order: excluded ones never are.
    pub fn plan(&self) -> Vec<usize> {
        let excluded: Vec<bool> = self.fields.iter().map(|f| f.excluded.is_some()).collect();
        let wanted: Vec<usize> = (0..self.fields.len()).collect();
        rules::read_plan(&excluded, &wanted)
    }

    /// The field called `name`.
    pub fn field(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    /// Field `i`, read from the program's copies of its regions (`(region name, bytes from the region's base)`).
    pub fn read(&self, i: usize, copies: &[(&str, &[u8])]) -> Result<Value, String> {
        let f = self.fields.get(i).ok_or_else(|| format!("no field #{i}"))?;
        if let Some(why) = &f.excluded {
            return Err(format!("{}: never read ({why})", f.name));
        }
        let r = self
            .regions
            .get(&f.region)
            .ok_or_else(|| format!("{}: no region {}", f.name, f.region))?;
        let copy = copies
            .iter()
            .find(|(n, _)| *n == f.region)
            .map(|(_, b)| *b)
            .ok_or_else(|| format!("{}: the program published no copy of {}", f.name, f.region))?;
        let width = f.ty.width(f.len);
        let off = rules::copy_offset(r.base, r.size, copy.len() as u64, f.addr, width).ok_or_else(
            || {
                format!(
                    "{}: {width} bytes at {:#x} are outside the copy of {}",
                    f.name, f.addr, f.region
                )
            },
        )?;
        // copy_offset proved off + width <= copy.len().
        let bytes = copy
            .get(off as usize..(off + width) as usize)
            .ok_or_else(|| format!("{}: outside the copy", f.name))?;
        Ok(self.decode(f, bytes))
    }

    /// Every field the plan allows, by name.
    pub fn read_all(&self, copies: &[(&str, &[u8])]) -> Vec<(&str, Result<Value, String>)> {
        self.plan()
            .into_iter()
            .map(|i| (self.fields[i].name.as_str(), self.read(i, copies)))
            .collect()
    }

    fn entry(&self, f: &Field, n: u64) -> Option<&str> {
        let list = self.enums.get(f.enum_name.as_ref()?)?;
        let at = n.checked_sub(f.enum_base)?;
        list.get(usize::try_from(at).ok()?).map(String::as_str)
    }

    fn decode(&self, f: &Field, b: &[u8]) -> Value {
        let number = |n: u64| -> Value {
            let text = match (&f.enum_name, self.entry(f, n)) {
                (None, _) => n.to_string(),
                (Some(_), Some(name)) => format!("{name} ({n})"),
                // Below the enum's base: the game's "nothing here" (an empty party slot, no item).
                (Some(_), None) if n < f.enum_base => format!("none ({n})"),
                (Some(_), None) => format!("unknown ({n})"),
            };
            Value {
                text,
                number: Some(n),
            }
        };
        match f.ty {
            Ty::U8 => number(b[0] as u64),
            Ty::U16be => number(u16::from_be_bytes([b[0], b[1]]) as u64),
            Ty::U16le => number(u16::from_le_bytes([b[0], b[1]]) as u64),
            Ty::U32le => number(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64),
            Ty::U64le => number(u64::from_le_bytes([
                b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
            ])),
            Ty::Bool => Value {
                text: (b[0] != 0).to_string(),
                number: Some(b[0] as u64),
            },
            Ty::Bcd => match bcd(b) {
                Some(n) => Value {
                    text: n.to_string(),
                    number: Some(n),
                },
                None => Value {
                    text: format!("not BCD ({})", hex(b)),
                    number: None,
                },
            },
            Ty::GbText => Value {
                text: gb_text(b),
                number: None,
            },
            Ty::GbTiles => Value {
                text: gb_tiles(b),
                number: None,
            },
            Ty::Bits => {
                let v = b[0];
                let text = match f.enum_name.as_ref().and_then(|e| self.enums.get(e)) {
                    Some(list) => {
                        let set: Vec<&str> = (0..8)
                            .filter(|i| v & (1 << i) != 0)
                            .map(|i| list.get(i).map_or("?", String::as_str))
                            .collect();
                        if set.is_empty() {
                            "none".to_string()
                        } else {
                            set.join(", ")
                        }
                    }
                    None => format!("{v:#010b}"),
                };
                Value {
                    text,
                    number: Some(v as u64),
                }
            }
            Ty::Bytes => Value {
                text: hex(b),
                number: None,
            },
        }
    }
}

/// The one map whose build id is `build_id` (hex), by [`rules::find_map`]: two maps with that id are refused.
pub fn pick<'a>(maps: &'a [StateMap], build_id: &str) -> Result<&'a StateMap, String> {
    let want = decode_hex(build_id).ok_or_else(|| format!("build id {build_id:?} is not hex"))?;
    let ids: Vec<Vec<u8>> = maps
        .iter()
        .map(|m| decode_hex(&m.build_id).unwrap_or_default())
        .collect();
    match rules::find_map(&ids, &want) {
        Lookup::Found(i) => Ok(&maps[i]),
        Lookup::NotFound => Err(format!("no map for build {build_id}")),
        Lookup::Duplicate => Err(format!("two maps claim build {build_id}: none is used")),
        Lookup::BadId => Err(format!(
            "build id {build_id} is too short or too long to trust"
        )),
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn bcd(b: &[u8]) -> Option<u64> {
    let mut n: u64 = 0;
    for &x in b {
        let (hi, lo) = ((x >> 4) as u64, (x & 15) as u64);
        if hi > 9 || lo > 9 {
            return None;
        }
        n = n.checked_mul(100)?.checked_add(hi * 10 + lo)?;
    }
    Some(n)
}

/// Gen 1 text, up to its 0x50 terminator. The charmap is lucina-pokemon's (`src/game.rs`, `glyph`, from pret/pokered
/// `constants/charmap.asm`); tiles outside the font read as a space.
pub fn gb_text(b: &[u8]) -> String {
    b.iter()
        .take_while(|&&c| c != 0x50)
        .map(|&c| glyph(c))
        .collect::<String>()
        .trim()
        .to_string()
}

/// A screen row of tiles, as lucina-pokemon's `row`: every tile through the charmap, with the text box's borders.
pub fn gb_tiles(b: &[u8]) -> String {
    b.iter()
        .map(|&c| match c {
            0x79 | 0x7b | 0x7d | 0x7e => "+",
            0x7a => "-",
            0x7c => "|",
            0xec => "▷",
            0xed => "▶",
            0xee => "▼",
            _ => glyph(c),
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn glyph(c: u8) -> &'static str {
    const UP: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    const LO: [&str; 26] = [
        "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ];
    const DIG: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
    match c {
        0x80..=0x99 => UP[(c - 0x80) as usize],
        0xa0..=0xb9 => LO[(c - 0xa0) as usize],
        0xf6..=0xff => DIG[(c - 0xf6) as usize],
        0x9a => "(",
        0x9b => ")",
        0x9c => ":",
        0x9d => ";",
        0x9e => "[",
        0x9f => "]",
        0xba => "é",
        0xbb => "'d",
        0xbc => "'l",
        0xbd => "'s",
        0xbe => "'t",
        0xbf => "'v",
        0xe4 => "'r",
        0xe5 => "'m",
        0xe0 => "'",
        0xe1 => "PK",
        0xe2 => "MN",
        0xe3 => "-",
        0xe6 => "?",
        0xe7 => "!",
        0xe8 => ".",
        0xef => "♂",
        0xf0 => "¥",
        0xf1 => "×",
        0xf3 => "/",
        0xf4 => ",",
        0xf5 => "♀",
        _ => " ",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"{
      "program": "Test", "build_id": "00112233445566778899aabbccddeeff00112233",
      "regions": { "wram": { "base": 49152, "size": 8192 } },
      "enums": { "map": ["PALLET_TOWN", "VIRIDIAN_CITY"], "badge": ["BOULDER", "CASCADE"], "species": ["RHYDON"] },
      "fields": [
        { "name": "map.id", "region": "wram", "addr": 54110, "type": "u8", "enum": "map" },
        { "name": "money", "region": "wram", "addr": 54087, "type": "bcd", "len": 3 },
        { "name": "badges", "region": "wram", "addr": 54102, "type": "bits", "enum": "badge" },
        { "name": "player.name", "region": "wram", "addr": 53592, "type": "gb_text", "len": 11 },
        { "name": "hp", "region": "wram", "addr": 53612, "type": "u16be" },
        { "name": "sp", "region": "wram", "addr": 53611, "type": "u8", "enum": "species", "enum_base": 1 },
        { "name": "secret", "region": "wram", "addr": 49152, "type": "bytes", "len": 4, "excluded": "credential" }
      ] }"#;

    fn wram() -> Vec<u8> {
        let mut w = vec![0u8; 8192];
        w[0xd35e - 0xc000] = 1;
        w[0xd347 - 0xc000..0xd34a - 0xc000].copy_from_slice(&[0x01, 0x23, 0x45]);
        w[0xd356 - 0xc000] = 0b11;
        w[0xd158 - 0xc000..0xd15f - 0xc000]
            .copy_from_slice(&[0x8b, 0x94, 0x82, 0x88, 0x8d, 0x80, 0x50]);
        w[0xd16c - 0xc000..0xd16e - 0xc000].copy_from_slice(&[0x01, 0x02]);
        w
    }

    #[test]
    fn reads_every_type() {
        let m = StateMap::parse(MAP).unwrap();
        let w = wram();
        let got: BTreeMap<&str, String> = m
            .read_all(&[("wram", &w)])
            .into_iter()
            .map(|(n, v)| (n, v.unwrap().text))
            .collect();
        assert_eq!(got["map.id"], "VIRIDIAN_CITY (1)");
        assert_eq!(got["money"], "12345");
        assert_eq!(got["badges"], "BOULDER, CASCADE");
        assert_eq!(got["player.name"], "LUCINA");
        assert_eq!(got["hp"], "258");
        assert_eq!(got["sp"], "none (0)");
        assert!(
            !got.contains_key("secret"),
            "excluded fields are never planned"
        );
        assert!(m.read(m.field("secret").unwrap(), &[("wram", &w)]).is_err());
    }

    #[test]
    fn a_short_copy_refuses_what_it_lacks() {
        let m = StateMap::parse(MAP).unwrap();
        let w = vec![0u8; 0x1000];
        assert!(m.read(m.field("map.id").unwrap(), &[("wram", &w)]).is_err());
        assert!(m.read(m.field("map.id").unwrap(), &[]).is_err());
    }

    #[test]
    fn bad_maps_are_refused() {
        let past = MAP.replace("\"addr\": 54110", "\"addr\": 57344");
        assert!(StateMap::parse(&past).unwrap_err().contains("not inside"));
        let edge = MAP.replace("\"addr\": 53612", "\"addr\": 57343");
        assert!(
            StateMap::parse(&edge).is_err(),
            "a u16 at the last byte runs out"
        );
        let twice = MAP.replace("\"name\": \"hp\"", "\"name\": \"money\"");
        assert!(StateMap::parse(&twice).is_err());
        let short = MAP.replace("00112233445566778899aabbccddeeff00112233", "0011");
        assert!(StateMap::parse(&short).is_err());
        let enum_missing = MAP.replace("\"enum\": \"map\"", "\"enum\": \"maps\"");
        assert!(StateMap::parse(&enum_missing).is_err());
    }

    #[test]
    fn pick_by_build_id() {
        let m = StateMap::parse(MAP).unwrap();
        let maps = vec![m.clone()];
        assert!(pick(&maps, &m.build_id).is_ok());
        assert!(pick(&maps, "ffffffffffffffffffffffffffffffffffffffff").is_err());
        let two = vec![m.clone(), m.clone()];
        assert!(pick(&two, &m.build_id).unwrap_err().contains("two maps"));
    }

    #[test]
    fn bcd_and_text() {
        assert_eq!(bcd(&[0x99, 0x99, 0x99]), Some(999999));
        assert_eq!(bcd(&[0x1a]), None);
        assert_eq!(gb_text(&[0x80, 0xe1, 0xe2, 0x50, 0x80]), "APKMN");
        assert_eq!(
            gb_tiles(&[0xed, 0x85, 0x88, 0x86, 0x87, 0x93, 0x7f, 0x50, 0x7c, 0x7f]),
            "▶FIGHT  |"
        );
    }
}
