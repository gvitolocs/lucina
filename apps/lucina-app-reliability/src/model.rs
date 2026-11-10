//! What the screens read: the event log as the worker loaded it, with its crash groups, and the words and times built from it.
//! Pure, so all of it is tested without a screen.

use lucina_app::time::Zone;
use lucina_evlog::bucket::{self, Group};
use lucina_evlog::event::Event;
use lucina_evlog::providers;
use lucina_evlog::store::Meta;

/// The log, read once: events by record number, the log's drop counts and the crash groups.
pub struct Data {
    pub events: Vec<Event>,
    pub meta: Meta,
    pub groups: Vec<Group>,
}

impl Data {
    pub fn new(mut events: Vec<Event>, meta: Meta) -> Data {
        events.sort_by_key(|e| e.record);
        let groups = bucket::groups(&events);
        Data {
            events,
            meta,
            groups,
        }
    }

    pub fn event(&self, record: u64) -> Option<&Event> {
        self.events
            .binary_search_by_key(&record, |e| e.record)
            .ok()
            .map(|i| &self.events[i])
    }

    pub fn group(&self, bucket_id: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.bucket.id == bucket_id)
    }
}

/// An event's name: the provider table's, else the provider and ID.
pub fn title(e: &Event) -> String {
    providers::known(&e.provider, e.id)
        .map_or_else(|| format!("{} {}", e.provider, e.id), |d| d.name.to_owned())
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Seconds east of UTC in force at `ms`.
pub fn offset_s(zone: &Zone, ms: i64) -> i32 {
    zone.at(ms.div_euclid(1000)).0 as i32
}

fn local_secs(zone: &Zone, ms: i64) -> i64 {
    ms.div_euclid(1000) + zone.at(ms.div_euclid(1000)).0
}

/// The local day number (days since 1970-01-01) of a time.
pub fn local_day(zone: &Zone, ms: i64) -> i64 {
    local_secs(zone, ms).div_euclid(86_400)
}

pub fn hhmm(zone: &Zone, ms: i64) -> String {
    let s = local_secs(zone, ms).rem_euclid(86_400);
    format!("{:02}:{:02}", s / 3600, s % 3600 / 60)
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn weekday(days: i64) -> &'static str {
    // 1970-01-01 was a Thursday.
    WEEKDAYS[(days + 3).rem_euclid(7) as usize]
}

pub fn month(m: u32) -> &'static str {
    MONTHS[(m.clamp(1, 12) - 1) as usize]
}

/// "2026-11-07" into (weekday, day, month name).
pub fn parts_of_date(date: &str) -> (&'static str, u32, &'static str) {
    let mut it = date.split('-').map(|x| x.parse::<i64>().unwrap_or(1));
    let (y, m, d) = (
        it.next().unwrap_or(1970),
        it.next().unwrap_or(1) as u32,
        it.next().unwrap_or(1) as u32,
    );
    (weekday(days_from_civil(y, m, d)), d, month(m))
}

/// "7 Nov".
pub fn short_date(zone: &Zone, ms: i64) -> String {
    let (_, m, d) = lucina_evlog::timeline::civil(local_day(zone, ms));
    format!("{d} {}", month(m))
}

/// "Sat 7 Nov".
pub fn long_date(zone: &Zone, ms: i64) -> String {
    let days = local_day(zone, ms);
    let (_, m, d) = lucina_evlog::timeline::civil(days);
    format!("{} {d} {}", weekday(days), month(m))
}

/// "14:32", "~14:32" when the time was worked out, "--:--" when unknown.
pub fn event_clock(zone: &Zone, e: &Event) -> String {
    match e.time_ms {
        Some(t) => format!(
            "{}{}",
            if e.time_estimated { "~" } else { "" },
            hhmm(zone, t)
        ),
        None => "--:--".to_owned(),
    }
}

/// The index as shown: one decimal, a dash before the log began.
pub fn index_text(i: Option<f64>) -> String {
    i.map_or_else(|| "—".to_owned(), |v| format!("{v:.1}"))
}

/// Problems a day counts in the status line: Juno and app failures.
pub fn problems(d: &lucina_evlog::timeline::Day) -> u32 {
    d.juno_failures + d.app_failures
}

/// "boots 7, 13, 16", at most eight of them.
pub fn boots_text(boots: &[u32]) -> String {
    let mut b = boots.to_vec();
    b.sort_unstable();
    let more = b.len().saturating_sub(8);
    let list = b
        .iter()
        .take(8)
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    match (b.len(), more) {
        (0, _) => "no boot number".to_owned(),
        (1, _) => format!("boot {list}"),
        (_, 0) => format!("boots {list}"),
        (_, m) => format!("boots {list} and {m} more"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturday_the_seventh() {
        let d = days_from_civil(2026, 11, 7);
        assert_eq!(weekday(d), "Sat");
        assert_eq!(parts_of_date("2026-11-07"), ("Sat", 7, "Nov"));
        assert_eq!(weekday(0), "Thu");
    }

    #[test]
    fn clocks_follow_the_zone() {
        let z = Zone::utc();
        assert_eq!(
            hhmm(&z, 1_794_009_600_000 + 14 * 3_600_000 + 32 * 60_000),
            "14:32"
        );
        assert_eq!(short_date(&z, 1_794_009_600_000), "7 Nov");
        assert_eq!(boots_text(&[16, 7, 13]), "boots 7, 13, 16");
        assert_eq!(boots_text(&[3]), "boot 3");
    }
}
