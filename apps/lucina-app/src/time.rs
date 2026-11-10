//! Local time without a time zone database in the initramfs: a POSIX `TZ` rule
//! (`CET-1CEST,M3.5.0,M10.5.0/3`, the last line of a TZif file) turned into an offset from UTC.
//!
//! `/etc/lucina/timezone` holds an IANA name or a POSIX rule. A name is looked up in
//! `/usr/share/zoneinfo` when that exists, then in a small table; the default is
//! Europe/Copenhagen, the owner's zone.

use std::fs;

pub const CONFIG: &str = "/etc/lucina/timezone";
pub const DEFAULT_NAME: &str = "Europe/Copenhagen";

/// The rules of the zones the system knows without a database.
const BUILT_IN: &[(&str, &str)] = &[
    ("Europe/Copenhagen", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Rome", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/London", "GMT0BST,M3.5.0/1,M10.5.0"),
    ("UTC", "UTC0"),
];

/// A day of the year as `Mm.w.d`: weekday `d` (0 = Sunday) of week `w` (5 = the last) of month `m`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Transition {
    month: u32,
    week: u32,
    weekday: u32,
    /// Seconds after local midnight.
    time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Dst {
    name: String,
    offset: i64,
    start: Transition,
    end: Transition,
}

/// A parsed POSIX `TZ` rule. Offsets are seconds east of UTC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Zone {
    /// The rule as written, for the `TZ` of programs started from the terminal.
    pub rule: String,
    name: String,
    offset: i64,
    dst: Option<Dst>,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The civil year of a day count since 1970-01-01.
fn year_of_days(days: i64) -> i64 {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    yoe + era * 400 + if mp >= 10 { 1 } else { 0 }
}

impl Transition {
    /// Seconds since the epoch, as local wall-clock seconds of `year`.
    fn local_seconds(&self, year: i64) -> i64 {
        let first = days_from_civil(year, self.month, 1);
        let first_weekday = (first + 4).rem_euclid(7);
        let mut day = first
            + (self.weekday as i64 - first_weekday).rem_euclid(7)
            + (self.week as i64 - 1) * 7;
        let next_month = if self.month == 12 {
            days_from_civil(year + 1, 1, 1)
        } else {
            days_from_civil(year, self.month + 1, 1)
        };
        while day >= next_month {
            day -= 7;
        }
        day * 86400 + self.time
    }
}

struct Cursor<'a>(&'a str);

impl Cursor<'_> {
    fn name(&mut self) -> Option<String> {
        if let Some(rest) = self.0.strip_prefix('<') {
            let end = rest.find('>')?;
            let name = rest[..end].to_string();
            self.0 = &rest[end + 1..];
            return Some(name);
        }
        let end = self
            .0
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(self.0.len());
        if end < 3 {
            return None;
        }
        let name = self.0[..end].to_string();
        self.0 = &self.0[end..];
        Some(name)
    }

    fn number(&mut self) -> Option<i64> {
        let end = self
            .0
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(self.0.len());
        let n = self.0[..end].parse().ok()?;
        self.0 = &self.0[end..];
        Some(n)
    }

    fn eat(&mut self, c: char) -> bool {
        match self.0.strip_prefix(c) {
            Some(rest) => {
                self.0 = rest;
                true
            }
            None => false,
        }
    }

    /// `[+-]hh[:mm[:ss]]` in seconds.
    fn time(&mut self) -> Option<i64> {
        let negative = if self.eat('-') {
            true
        } else {
            self.eat('+');
            false
        };
        let mut t = self.number()? * 3600;
        if self.eat(':') {
            t += self.number()? * 60;
            if self.eat(':') {
                t += self.number()?;
            }
        }
        Some(if negative { -t } else { t })
    }

    /// `Mm.w.d[/time]`; the Julian-day forms are not used by any zone we ship.
    fn transition(&mut self) -> Option<Transition> {
        if !self.eat('M') {
            return None;
        }
        let month = self.number()? as u32;
        if !self.eat('.') {
            return None;
        }
        let week = self.number()? as u32;
        if !self.eat('.') {
            return None;
        }
        let weekday = self.number()? as u32;
        let time = if self.eat('/') {
            self.time()?
        } else {
            2 * 3600
        };
        ((1..=12).contains(&month) && (1..=5).contains(&week) && weekday <= 6).then_some(
            Transition {
                month,
                week,
                weekday,
                time,
            },
        )
    }
}

impl Zone {
    pub fn utc() -> Zone {
        Zone {
            rule: "UTC0".into(),
            name: "UTC".into(),
            offset: 0,
            dst: None,
        }
    }

    /// Parses a POSIX `TZ` rule.
    pub fn parse(rule: &str) -> Option<Zone> {
        let mut c = Cursor(rule.trim());
        let name = c.name()?;
        // POSIX offsets count west of UTC.
        let offset = -c.time()?;
        let mut zone = Zone {
            rule: rule.trim().to_string(),
            name,
            offset,
            dst: None,
        };
        if c.0.is_empty() {
            return Some(zone);
        }
        let dst_name = c.name()?;
        let dst_offset = if c.0.starts_with(',') {
            offset + 3600
        } else {
            -c.time()?
        };
        if !c.eat(',') {
            return None;
        }
        let start = c.transition()?;
        if !c.eat(',') {
            return None;
        }
        let end = c.transition()?;
        if !c.0.is_empty() {
            return None;
        }
        zone.dst = Some(Dst {
            name: dst_name,
            offset: dst_offset,
            start,
            end,
        });
        Some(zone)
    }

    /// The offset east of UTC and the abbreviation in force at `utc` (seconds since the epoch).
    pub fn at(&self, utc: i64) -> (i64, &str) {
        let Some(dst) = &self.dst else {
            return (self.offset, &self.name);
        };
        let year = year_of_days((utc + self.offset).div_euclid(86400));
        let start = dst.start.local_seconds(year) - self.offset;
        let end = dst.end.local_seconds(year) - dst.offset;
        let in_dst = if start < end {
            utc >= start && utc < end
        } else {
            utc >= start || utc < end
        };
        if in_dst {
            (dst.offset, &dst.name)
        } else {
            (self.offset, &self.name)
        }
    }

    /// `HH:MM ZZZ` at `utc`.
    pub fn clock(&self, utc: i64) -> String {
        let (offset, name) = self.at(utc);
        let day = (utc + offset).rem_euclid(86400);
        format!("{:02}:{:02} {name}", day / 3600, day % 3600 / 60)
    }

    /// Seconds to the next minute boundary of the wall clock (zones may have odd offsets).
    pub fn until_next_minute(&self, utc: i64) -> u64 {
        (60 - (utc + self.at(utc).0).rem_euclid(60)) as u64
    }
}

/// The rule in a TZif file: its last line, between the final two newlines.
fn tzif_footer(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"TZif") {
        return None;
    }
    let body = bytes.strip_suffix(b"\n")?;
    let start = body.iter().rposition(|&b| b == b'\n')? + 1;
    String::from_utf8(body[start..].to_vec()).ok()
}

/// The zone for what the config file says (`text`): an IANA name or a POSIX rule.
fn resolve(text: &str, zoneinfo: impl Fn(&str) -> Option<Vec<u8>>) -> Option<Zone> {
    let name = text.trim();
    if let Some(rule) = zoneinfo(name).as_deref().and_then(tzif_footer) {
        return Zone::parse(&rule);
    }
    if let Some((_, rule)) = BUILT_IN.iter().find(|(n, _)| *n == name) {
        return Zone::parse(rule);
    }
    Zone::parse(name)
}

/// Reads the configured zone. Returns the name as configured and the zone; an unknown name
/// falls back to UTC rather than to a wrong local time.
pub fn load() -> (String, Zone) {
    let text = fs::read_to_string(CONFIG).unwrap_or_else(|_| DEFAULT_NAME.to_string());
    let name = text.trim().to_string();
    let zoneinfo = |n: &str| {
        (!n.contains("..") && !n.contains(','))
            .then(|| fs::read(format!("/usr/share/zoneinfo/{n}")).ok())?
    };
    match resolve(&name, zoneinfo) {
        Some(zone) => (name, zone),
        None => (format!("{name} (unknown: UTC)"), Zone::utc()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copenhagen() -> Zone {
        Zone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    /// Checked against `TZ=Europe/Copenhagen date -d @<t>` (glibc, tzdata).
    #[test]
    fn copenhagen_changes_at_the_right_second() {
        let z = copenhagen();
        assert_eq!(z.clock(1_774_745_999), "01:59 CET");
        assert_eq!(z.clock(1_774_746_000), "03:00 CEST");
        assert_eq!(z.clock(1_792_889_999), "02:59 CEST");
        assert_eq!(z.clock(1_792_890_000), "02:00 CET");
        // 2026-11-01 13:05:00 UTC.
        assert_eq!(z.clock(1_793_538_300), "14:05 CET");
    }

    #[test]
    fn parses_rules() {
        assert_eq!(Zone::parse("UTC0").unwrap().clock(3600), "01:00 UTC");
        let ny = Zone::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // 2026-07-01 12:00 UTC and 2026-01-01 12:00 UTC.
        assert_eq!(ny.clock(1_782_907_200), "08:00 EDT");
        assert_eq!(ny.clock(1_767_268_800), "07:00 EST");
        // A numeric name, half-hour offset, no daylight saving.
        assert_eq!(Zone::parse("<+0530>-5:30").unwrap().clock(0), "05:30 +0530");
        // Southern hemisphere: the summer spans the new year.
        let syd = Zone::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(syd.clock(1_767_268_800), "23:00 AEDT");
        assert_eq!(syd.clock(1_782_907_200), "22:00 AEST");
        assert_eq!(Zone::parse("nonsense"), None);
        assert_eq!(Zone::parse("CET-1CEST"), None);
        assert_eq!(Zone::parse("Europe/Atlantis"), None);
    }

    #[test]
    fn config_names_and_rules() {
        let none = |_: &str| None;
        assert_eq!(
            resolve("Europe/Copenhagen\n", none).unwrap().rule,
            "CET-1CEST,M3.5.0,M10.5.0/3"
        );
        assert_eq!(resolve("EST5", none).unwrap().rule, "EST5");
        assert_eq!(resolve("Mars/Olympus", none), None);
        // A TZif file's footer wins over the table.
        let file = |_: &str| Some(b"TZif2 binary\n\0\0\nJST-9\n".to_vec());
        assert_eq!(resolve("Europe/Copenhagen", file).unwrap().rule, "JST-9");
    }

    #[test]
    fn minute_boundaries() {
        let z = copenhagen();
        assert_eq!(z.until_next_minute(1_793_538_300), 60);
        assert_eq!(z.until_next_minute(1_793_538_359), 1);
    }

    #[test]
    fn years_of_days() {
        assert_eq!(year_of_days(0), 1970);
        assert_eq!(year_of_days(days_from_civil(2026, 12, 31)), 2026);
        assert_eq!(year_of_days(days_from_civil(2027, 1, 1)), 2027);
        assert_eq!(year_of_days(days_from_civil(2024, 2, 29)), 2024);
    }
}
