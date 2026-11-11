//! Time as the server writes it (RFC 3339), and as Keep shows it (`web/src/lib/keepTime.ts`). No date crate: this is all
//! the calendar these apps need, and a program here is about 1 MB.

use lucina_app::time::Zone;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Milliseconds since 1970 of `2026-11-01T09:00:00.123Z` or `...+01:00`. `None` for anything else.
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (
        n(0..4)?,
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut at = 19;
    let mut ms = 0i64;
    if b.get(at) == Some(&b'.') {
        at += 1;
        let start = at;
        while b.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        let frac = format!("{:0<3}", &s[start..at]);
        ms = frac[..3].parse().ok()?;
    }
    let offset = match b.get(at) {
        Some(b'Z') | Some(b'z') | None => 0,
        Some(sign @ (b'+' | b'-')) => {
            let oh = n(at + 1..at + 3)?;
            let om = n(at + 4..at + 6).or_else(|| n(at + 3..at + 5)).unwrap_or(0);
            let v = oh * 3600 + om * 60;
            if *sign == b'-' { -v } else { v }
        }
        _ => return None,
    };
    Some((days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se - offset) * 1000 + ms)
}

/// `Date.prototype.toISOString`.
pub fn to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let t = secs.rem_euclid(86400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        t / 3600,
        t / 60 % 60,
        t % 60,
        ms.rem_euclid(1000)
    )
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// One point in time on the local calendar.
struct Local {
    day: i64,
    y: i64,
    m: i64,
    d: i64,
    secs: i64,
}

fn local(ms: i64, zone: &Zone) -> Local {
    let utc = ms.div_euclid(1000);
    let (off, _) = zone.at(utc);
    let t = utc + off;
    let day = t.div_euclid(86400);
    let (y, m, d) = civil_from_days(day);
    Local {
        day,
        y,
        m,
        d,
        secs: t.rem_euclid(86400),
    }
}

fn clock(l: &Local) -> String {
    format!("{:02}:{:02}", l.secs / 3600, l.secs / 60 % 60)
}

fn day_month(l: &Local, now: &Local) -> String {
    let month = MONTHS[(l.m - 1) as usize];
    if l.y == now.y {
        format!("{} {month}", l.d)
    } else {
        format!("{} {month} {}", l.d, l.y)
    }
}

/// `editedLabel`: "14:30" today, "24 Oct" this year, "24 Oct 2025" before.
pub fn edited_label(iso: &str, now_ms: i64, zone: &Zone) -> String {
    let Some(ms) = parse_iso(iso) else {
        return String::new();
    };
    let (l, now) = (local(ms, zone), local(now_ms, zone));
    if l.day == now.day {
        clock(&l)
    } else {
        day_month(&l, &now)
    }
}

/// `formatDate`: "24 Oct 2025", whatever the year.
pub fn date_label(iso: &str, zone: &Zone) -> String {
    let Some(ms) = parse_iso(iso) else {
        return String::new();
    };
    let l = local(ms, zone);
    format!("{} {} {}", l.d, MONTHS[(l.m - 1) as usize], l.y)
}

/// `reminderLabel`: "Today, 18:00", "Tomorrow, 08:00", "24 Oct, 09:00".
pub fn reminder_label(iso: &str, now_ms: i64, zone: &Zone) -> String {
    let Some(ms) = parse_iso(iso) else {
        return String::new();
    };
    let (l, now) = (local(ms, zone), local(now_ms, zone));
    let day = match l.day - now.day {
        0 => "Today".to_owned(),
        1 => "Tomorrow".to_owned(),
        _ => day_month(&l, &now),
    };
    format!("{day}, {}", clock(&l))
}

/// The local midnight (as UTC ms) `days` after `now`, plus `h` hours. Near a clock change it is off by that hour for the
/// hours after the change; a reminder at 08:00 is not.
fn at(now_ms: i64, days: i64, h: i64, zone: &Zone) -> i64 {
    let l = local(now_ms, zone);
    let naive = (l.day + days) * 86400 + h * 3600;
    let (off, _) = zone.at(naive);
    (naive - off) * 1000
}

/// `reminderPresets`: later today (18:00, or in three hours after 15:00), tomorrow 08:00, next Monday 08:00.
pub fn reminder_presets(now_ms: i64, zone: &Zone) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    let l = local(now_ms, zone);
    let evening = at(now_ms, 0, 18, zone);
    if evening - now_ms > 30 * 60_000 {
        out.push(("Later today", evening));
    } else if l.secs / 3600 < 21 {
        let later = (now_ms + 3 * 3_600_000) / 3_600_000 * 3_600_000;
        if local(later, zone).day == l.day {
            out.push(("Later today", later));
        }
    }
    out.push(("Tomorrow", at(now_ms, 1, 8, zone)));
    // 1970-01-01 was a Thursday: Sunday is 0, as in getDay().
    let weekday = (l.day + 4).rem_euclid(7);
    let to_monday = if (8 - weekday) % 7 == 0 {
        7
    } else {
        (8 - weekday) % 7
    };
    out.push(("Next week", at(now_ms, to_monday, 8, zone)));
    out
}

/// A version 4 UUID, in the canonical form the shopping routes ask for (`web/src/lib/ids.ts`).
pub fn uuid() -> String {
    let mut b = [0u8; 16];
    random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// Random bytes: `getrandom`, which waits for the kernel to be seeded, falling back on the clock and an address only if the call
/// itself fails. For names of things (the ids of items and notes), not for secrets.
pub fn random(b: &mut [u8]) {
    // SAFETY: fills a live buffer of its own length.
    let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    if n != b.len() as isize {
        let mut x = now_ms() as u64 ^ (b.as_ptr() as u64).rotate_left(32);
        for v in b.iter_mut() {
            x = x.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1);
            *v = (x >> 56) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cph() -> Zone {
        Zone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    #[test]
    fn iso_goes_there_and_back() {
        let ms = parse_iso("2026-10-22T11:30:00.123Z").unwrap();
        assert_eq!(to_iso(ms), "2026-10-22T11:30:00.123Z");
        assert_eq!(
            parse_iso("2026-11-01T09:00:00+01:00"),
            parse_iso("2026-11-01T08:00:00Z")
        );
        assert_eq!(parse_iso("nonsense"), None);
    }

    #[test]
    fn edited_labels_follow_keep() {
        let now = parse_iso("2026-11-01T20:00:00Z").unwrap();
        assert_eq!(edited_label("2026-11-01T13:30:00Z", now, &cph()), "14:30");
        assert_eq!(edited_label("2026-10-24T09:00:00Z", now, &cph()), "24 Oct");
        assert_eq!(
            edited_label("2025-10-24T09:00:00Z", now, &cph()),
            "24 Oct 2025"
        );
        assert_eq!(edited_label("bad", now, &cph()), "");
    }

    #[test]
    fn reminder_labels_name_today_and_tomorrow() {
        let now = parse_iso("2026-11-01T08:00:00Z").unwrap();
        assert_eq!(
            reminder_label("2026-11-01T17:00:00Z", now, &cph()),
            "Today, 18:00"
        );
        assert_eq!(
            reminder_label("2026-11-02T07:00:00Z", now, &cph()),
            "Tomorrow, 08:00"
        );
        assert_eq!(
            reminder_label("2026-11-24T08:00:00Z", now, &cph()),
            "24 Nov, 09:00"
        );
    }

    // web/src/lib/keepTime.ts: Sunday 2026-11-01, 09:00 in Copenhagen.
    #[test]
    fn reminder_presets_follow_keep() {
        let now = parse_iso("2026-11-01T08:00:00Z").unwrap();
        let p = reminder_presets(now, &cph());
        let labels: Vec<String> = p
            .iter()
            .map(|(n, at)| format!("{n} {}", reminder_label(&to_iso(*at), now, &cph())))
            .collect();
        assert_eq!(
            labels,
            [
                "Later today Today, 18:00",
                "Tomorrow Tomorrow, 08:00",
                "Next week Tomorrow, 08:00"
            ]
        );
        // Late in the evening there is no "later today".
        let late = parse_iso("2026-11-01T22:00:00Z").unwrap();
        assert_eq!(reminder_presets(late, &cph())[0].0, "Tomorrow");
    }

    #[test]
    fn uuids_are_canonical_and_differ() {
        let (a, b) = (uuid(), uuid());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
        assert!(a.split('-').map(str::len).eq([8, 4, 4, 4, 12]));
    }
}
