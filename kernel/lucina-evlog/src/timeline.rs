//! Reliability Monitor's timeline: one column per day with its failures, warnings and information, and a stability index from
//! 1 to 10. Windows never published its formula; ours is below, short enough to show on screen, and every day lists the events
//! that cost it most, so the line can always be explained.

use serde::{Deserialize, Serialize};

use crate::event::{Category, Channel, Event, Level};

/// The formula, in the words the app shows.
pub const FORMULA: &str = "Each day scores its problems: a crash or unexpected reset 1, a failed install 0.5, an app crash \
0.25, a warning 0.05 (at most 0.5 a day). The index is 10 x e^(-P / 2.5), where P adds up the last 28 days' scores, each day \
counting 0.85 times as much as the day after it. So one crash today gives 6.7, four give 2.0, and a week without problems \
brings one crash back up to 8.8. It never goes below 1.";

const DAY_MS: i64 = 86_400_000;
const HISTORY_DAYS: usize = 28;
const DECAY: f64 = 0.85;
const SCALE: f64 = 2.5;

/// What one event costs its day.
pub fn weight(e: &Event) -> f64 {
    match (e.level, e.channel) {
        (Level::Critical, _) => 1.0,
        (Level::Error, Channel::Apps) => 0.25,
        (Level::Error, _) => 0.5,
        (Level::Warning, _) => 0.05,
        _ => 0.0,
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    pub record: u64,
    pub summary: String,
    /// What it takes off today's penalty, after its day's decay.
    pub weight: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// `2026-11-07`, local.
    pub date: String,
    /// Local midnight, unix ms.
    pub start_ms: i64,
    /// None before the log's first event: nothing was watching then.
    pub index: Option<f64>,
    pub juno_failures: u32,
    pub app_failures: u32,
    pub warnings: u32,
    pub info: u32,
    /// The day's events, oldest first.
    pub records: Vec<u64>,
    /// What costs the index most on this day, largest first, at most 3.
    pub why: Vec<Contribution>,
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant's `civil_from_days`).
pub fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn local_day(ms: i64, offset_s: i32) -> i64 {
    (ms + i64::from(offset_s) * 1000).div_euclid(DAY_MS)
}

/// The `n` days ending with the one holding `now_ms`, oldest first, in a zone `offset_s` east of UTC. Events without a time
/// are left out (the caller lists them apart).
pub fn days(events: &[Event], now_ms: i64, n: usize, offset_s: i32) -> Vec<Day> {
    let today = local_day(now_ms, offset_s);
    let first_day = today - n as i64 + 1;
    let earliest = events
        .iter()
        .filter_map(|e| e.time_ms)
        .map(|t| local_day(t, offset_s))
        .min();
    // Scores from 28 days before the first shown day, so the first column's index has its history.
    let span = n + HISTORY_DAYS;
    let base = today - span as i64 + 1;
    let mut score = vec![0.0f64; span];
    let mut warn = vec![0.0f64; span];
    let mut dayof: Vec<Vec<&Event>> = vec![Vec::new(); span];
    for e in events {
        let Some(t) = e.time_ms else { continue };
        let d = local_day(t, offset_s);
        if d < base || d > today {
            continue;
        }
        let i = (d - base) as usize;
        dayof[i].push(e);
        if e.level == Level::Warning {
            warn[i] += weight(e);
        } else {
            score[i] += weight(e);
        }
    }
    for i in 0..span {
        score[i] += warn[i].min(0.5);
    }
    let mut out = Vec::with_capacity(n);
    for d in first_day..=today {
        let i = (d - base) as usize;
        let mut penalty = 0.0;
        let mut why: Vec<Contribution> = Vec::new();
        for k in 0..HISTORY_DAYS.min(i + 1) {
            let f = DECAY.powi(k as i32);
            penalty += f * score[i - k];
            for e in &dayof[i - k] {
                let w = weight(e) * f;
                if w > 0.0 {
                    why.push(Contribution {
                        record: e.record,
                        summary: e.summary.clone(),
                        weight: w,
                    });
                }
            }
        }
        why.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(b.record.cmp(&a.record)));
        why.truncate(3);
        let index = earliest
            .filter(|&e| e <= d)
            .map(|_| ((10.0 * (-penalty / SCALE).exp()).clamp(1.0, 10.0) * 10.0).round() / 10.0);
        let mut evs = dayof[i].clone();
        evs.sort_by_key(|e| (e.time_ms, e.record));
        let count = |c: Category| evs.iter().filter(|e| e.category() == c).count() as u32;
        let (y, m, dd) = civil(d);
        out.push(Day {
            date: format!("{y:04}-{m:02}-{dd:02}"),
            start_ms: d * DAY_MS - i64::from(offset_s) * 1000,
            index,
            juno_failures: count(Category::JunoFailure),
            app_failures: count(Category::AppFailure),
            warnings: count(Category::Warning),
            info: count(Category::Info),
            records: evs.iter().map(|e| e.record).collect(),
            why,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(record: u64, level: Level, channel: Channel, t: i64) -> Event {
        let mut e = Event::new("T", 1, level, channel, format!("e{record}"));
        e.record = record;
        e.time_ms = Some(t);
        e
    }

    const NOV7: i64 = 1_794_009_600_000; // 2026-11-07 00:00 UTC

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(NOV7 / DAY_MS), (2026, 11, 7));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn the_formula_as_written() {
        let noon = NOV7 + 12 * 3_600_000;
        let one = days(&[ev(1, Level::Critical, Channel::System, noon)], noon, 3, 0);
        assert_eq!(one.len(), 3);
        assert_eq!(one[2].date, "2026-11-07");
        assert_eq!(one[2].index, Some(6.7));
        assert_eq!(
            one[0].index, None,
            "before the first event nothing was watching"
        );
        let four: Vec<Event> = (1..=4)
            .map(|r| ev(r, Level::Critical, Channel::System, noon))
            .collect();
        let d = days(&four, noon, 1, 0);
        assert_eq!(d[0].index, Some(2.0));
        assert_eq!(d[0].juno_failures, 4);
        assert_eq!(d[0].why.len(), 3);
        let later = noon + 7 * DAY_MS;
        let d = days(
            &[ev(1, Level::Critical, Channel::System, noon)],
            later,
            1,
            0,
        );
        assert_eq!(d[0].index, Some(8.8));
        assert_eq!(d[0].why[0].record, 1);
    }

    #[test]
    fn warnings_are_capped_and_categories_counted() {
        let noon = NOV7 + 12 * 3_600_000;
        let mut evs: Vec<Event> = (1..=40)
            .map(|r| ev(r, Level::Warning, Channel::System, noon))
            .collect();
        evs.push(ev(41, Level::Error, Channel::Apps, noon));
        evs.push(ev(42, Level::Info, Channel::System, noon));
        let d = days(&evs, noon, 1, 0);
        // 0.5 (capped warnings) + 0.25 = 0.75: 10 e^-0.3 = 7.4.
        assert_eq!(d[0].index, Some(7.4));
        assert_eq!(
            (
                d[0].juno_failures,
                d[0].app_failures,
                d[0].warnings,
                d[0].info
            ),
            (0, 1, 40, 1)
        );
    }

    #[test]
    fn a_zone_moves_the_day() {
        let late = NOV7 + 23 * 3_600_000; // 23:00 UTC = 00:00 the 8th at UTC+1
        let d = days(&[ev(1, Level::Info, Channel::System, late)], late, 1, 3600);
        assert_eq!(d[0].date, "2026-11-08");
        assert_eq!(d[0].records, vec![1]);
    }
}
