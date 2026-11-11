//! What is in memory: the server's events per month, unconfirmed changes laid over them, an index per
//! day; and the editor's form, ported from `web/src/lib/calendar.ts`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, Utc};
use chrono_tz::Tz;
use lucina_app::{Rgb, Theme};
use secretary_calendar::model::{day_start, iso, local_to_utc};
use secretary_calendar::{Event, Source};

use crate::server::Draft;

pub fn month_of(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

pub fn add_months(month: NaiveDate, n: i32) -> NaiveDate {
    let total = month.year() * 12 + month.month0() as i32 + n;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1)
        .unwrap_or(month)
}

/// One month as instants in the owner's zone: what one load asks for.
pub fn month_range(month: NaiveDate, tz: Tz) -> (DateTime<Utc>, DateTime<Utc>) {
    (day_start(tz, month), day_start(tz, add_months(month, 1)))
}

// ---- Colours and names: lib/calendar.ts ----

const PALETTE: [u32; 10] = [
    0xe5534b, 0xe8912d, 0xc9a227, 0x3fb950, 0x14a3a3, 0x1f9bcf, 0x4f7cf0, 0xa371f7, 0xdb61a2,
    0xa0785a,
];

fn hash(s: &str) -> u32 {
    let mut h: u32 = 2166136261;
    for c in s.encode_utf16() {
        h ^= u32::from(c);
        h = h.wrapping_mul(16777619);
    }
    h
}

/// `calendarName`: the legend name of an event's calendar.
pub fn calendar_name(e: &Event) -> String {
    if e.source == Source::Lucina {
        return "Lucina".into();
    }
    match e.calendar.as_deref().map(str::trim) {
        Some(c) if !c.is_empty() => c.to_owned(),
        _ => match e.source {
            Source::Google => "Google".into(),
            Source::Iphone => "iPhone".into(),
            s => s.as_str().to_owned(),
        },
    }
}

/// `calendarColor`: Lucina's is the accent, Google's is blue, the rest a stable pick by name.
pub fn calendar_color(name: &str, source: Source, t: &Theme) -> Rgb {
    if source == Source::Lucina {
        return t.accent;
    }
    if source == Source::Google && (name == "Google" || name.is_empty()) {
        return Rgb::hex(0x4f7cf0);
    }
    Rgb::hex(PALETTE[hash(name) as usize % PALETTE.len()])
}

/// `plainNotes`: Google descriptions can be HTML.
pub fn plain_notes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('>') else {
            out.push_str(&rest[i..]);
            rest = "";
            break;
        };
        let tag = rest[i + 1..i + j].to_ascii_lowercase();
        if tag.starts_with("br")
            || ["/p", "/div", "/li", "/h1", "/h2", "/h3"].contains(&tag.as_str())
        {
            out.push('\n');
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    let mut s = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ");
    while s.contains("\n\n\n") {
        s = s.replace("\n\n\n", "\n\n");
    }
    s.trim().to_owned()
}

fn ymd(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

pub fn local(ms: i64, tz: Tz) -> chrono::NaiveDateTime {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&tz)
        .naive_local()
}

/// `whenLabel`: "Fri 30 Oct · 13:30–14:30", "Fri 30 Oct, 23:00 – Sat 31 Oct, 01:00", "Fri 30 Oct – Sun 1 Nov".
pub fn when_label(e: &Event, tz: Tz) -> String {
    const DAY: &str = "%a %-d %b";
    if e.all_day {
        let (Some(first), Some(end)) = (ymd(&e.start), ymd(&e.end)) else {
            return e.start.clone();
        };
        let last = end - Duration::days(1);
        return if last > first {
            format!("{} – {}", first.format(DAY), last.format(DAY))
        } else {
            first.format(DAY).to_string()
        };
    }
    let (s, en) = (local(e.start_ms, tz), local(e.end_ms, tz));
    if s.date() == en.date() || e.end_ms <= e.start_ms {
        format!(
            "{} · {}–{}",
            s.format(DAY),
            s.format("%H:%M"),
            en.format("%H:%M")
        )
    } else {
        format!(
            "{}, {} – {}, {}",
            s.format(DAY),
            s.format("%H:%M"),
            en.format(DAY),
            en.format("%H:%M")
        )
    }
}

// ---- The form ----

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Form {
    pub title: String,
    pub all_day: bool,
    pub start_date: String,
    pub start_time: String,
    /// Inclusive for all-day events: the form shows the last day, the server wants the day after.
    pub end_date: String,
    pub end_time: String,
    pub location: String,
    pub notes: String,
    pub video: String,
}

fn parse_hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// `formForRange`: a new event for a time range. All-day `end` is exclusive.
pub fn form_for_range(start: i64, end: i64, all_day: bool, tz: Tz) -> Form {
    let (s, e) = (local(start, tz), local(end, tz));
    let last = if all_day { local(end - 1, tz) } else { e };
    Form {
        all_day,
        start_date: s.format("%Y-%m-%d").to_string(),
        start_time: s.format("%H:%M").to_string(),
        end_date: last.max(s).format("%Y-%m-%d").to_string(),
        end_time: e.format("%H:%M").to_string(),
        ..Form::default()
    }
}

/// `formForEvent`.
pub fn form_for_event(e: &Event, tz: Tz) -> Form {
    let text = Form {
        title: if e.title == "(no title)" {
            String::new()
        } else {
            e.title.clone()
        },
        location: e.location.clone().unwrap_or_default(),
        notes: plain_notes(e.description.as_deref().unwrap_or("")),
        video: e.conference_url.clone().unwrap_or_default(),
        ..Form::default()
    };
    if e.all_day {
        let first = ymd(&e.start).unwrap_or_default();
        let last = ymd(&e.end)
            .map_or(first, |x| x - Duration::days(1))
            .max(first);
        return Form {
            all_day: true,
            start_date: first.format("%Y-%m-%d").to_string(),
            start_time: "09:00".into(),
            end_date: last.format("%Y-%m-%d").to_string(),
            end_time: "10:00".into(),
            ..text
        };
    }
    let (s, en) = (local(e.start_ms, tz), local(e.end_ms, tz));
    Form {
        start_date: s.format("%Y-%m-%d").to_string(),
        start_time: s.format("%H:%M").to_string(),
        end_date: en.format("%Y-%m-%d").to_string(),
        end_time: en.format("%H:%M").to_string(),
        ..text
    }
}

/// `defaultStart`: the next full hour today, or 09:00 on the day being looked at.
pub fn default_start(day: NaiveDate, now_ms: i64, tz: Tz) -> i64 {
    let now = local(now_ms, tz);
    if day == now.date() {
        let top = now
            .date()
            .and_hms_opt(now.format("%H").to_string().parse().unwrap_or(0), 0, 0);
        let t = top.unwrap_or(now) + Duration::hours(1);
        local_to_utc(tz, t).timestamp_millis()
    } else {
        local_to_utc(tz, day.and_hms_opt(9, 0, 0).expect("nine")).timestamp_millis()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormError {
    NeedTitle,
    NeedEndAfterStart,
}

impl FormError {
    pub fn text(self) -> &'static str {
        match self {
            FormError::NeedTitle => "Give the event a title.",
            FormError::NeedEndAfterStart => {
                "The end has to come after the start. Dates are YYYY-MM-DD, times HH:MM."
            }
        }
    }
}

/// `draftFromForm`: the form to the request body, or what's wrong.
pub fn draft_from_form(f: &Form, tz: Tz) -> Result<Draft, FormError> {
    let title = f.title.trim();
    if title.is_empty() {
        return Err(FormError::NeedTitle);
    }
    let mut d = Draft {
        title: title.to_owned(),
        all_day: f.all_day,
        location: Some(f.location.trim().to_owned()),
        description: Some(f.notes.trim().to_owned()),
        conference_url: Some(f.video.trim().to_owned()).filter(|v| !v.is_empty()),
        ..Draft::default()
    };
    let (sd, ed) = (ymd_exact(&f.start_date), ymd_exact(&f.end_date));
    if f.all_day {
        let (Some(s), Some(e)) = (sd, ed) else {
            return Err(FormError::NeedEndAfterStart);
        };
        if e < s {
            return Err(FormError::NeedEndAfterStart);
        }
        d.start = s.format("%Y-%m-%d").to_string();
        d.end = Some((e + Duration::days(1)).format("%Y-%m-%d").to_string());
        return Ok(d);
    }
    let (Some(sd), Some(ed), Some(st), Some(et)) =
        (sd, ed, parse_hm(&f.start_time), parse_hm(&f.end_time))
    else {
        return Err(FormError::NeedEndAfterStart);
    };
    let s = local_to_utc(tz, sd.and_time(st));
    let e = local_to_utc(tz, ed.and_time(et));
    if e <= s {
        return Err(FormError::NeedEndAfterStart);
    }
    d.start = iso(s);
    d.end = Some(iso(e));
    Ok(d)
}

fn ymd_exact(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d").ok()
}

/// `shiftEnd`: moving the start keeps the event's length.
pub fn shift_end(prev: &Form, next: &Form, tz: Tz) -> Form {
    let mut out = next.clone();
    if prev.all_day != next.all_day {
        return out;
    }
    if next.all_day {
        if prev.start_date == next.start_date {
            return out;
        }
        if let (Some(a), Some(b), Some(n)) = (
            ymd_exact(&prev.start_date),
            ymd_exact(&prev.end_date),
            ymd_exact(&next.start_date),
        ) {
            let days = (b - a).num_days().max(0);
            out.end_date = (n + Duration::days(days)).format("%Y-%m-%d").to_string();
        }
        return out;
    }
    let both = |d: &str, t: &str| Some(local_to_utc(tz, ymd_exact(d)?.and_time(parse_hm(t)?)));
    let (Some(ps), Some(pe), Some(ns)) = (
        both(&prev.start_date, &prev.start_time),
        both(&prev.end_date, &prev.end_time),
        both(&next.start_date, &next.start_time),
    ) else {
        return out;
    };
    if ps == ns {
        return out;
    }
    let len = (pe - ps).max(Duration::minutes(15));
    let ne = (ns + len).with_timezone(&tz).naive_local();
    out.end_date = ne.format("%Y-%m-%d").to_string();
    out.end_time = ne.format("%H:%M").to_string();
    out
}

// ---- Events in memory ----

/// A change the server hasn't confirmed.
#[derive(Clone, Debug)]
pub enum Change {
    Create {
        op: u64,
        ghost: Event,
        form: Form,
    },
    Update {
        op: u64,
        id: String,
        start_ms: i64,
        ghost: Event,
        form: Form,
    },
    Delete {
        op: u64,
        id: String,
        start_ms: i64,
    },
}

impl Change {
    pub fn op(&self) -> u64 {
        match self {
            Change::Create { op, .. } | Change::Update { op, .. } | Change::Delete { op, .. } => {
                *op
            }
        }
    }
}

pub struct Store {
    pub tz: Tz,
    pub now: fn() -> i64,
    pub months: BTreeMap<NaiveDate, Vec<Event>>,
    pub changes: Vec<Change>,
    pub hidden: BTreeSet<String>,
    /// All events on screen: merged, changes laid over them.
    pub events: Vec<Event>,
    /// Day to indices in `events`, in start order.
    pub by_day: HashMap<NaiveDate, Vec<usize>>,
    /// The calendars seen: name and source, sorted, Lucina first.
    pub calendars: Vec<(String, Source)>,
    next_op: u64,
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// The days an event covers in the zone.
pub fn days_of(e: &Event, tz: Tz) -> (NaiveDate, NaiveDate) {
    if e.all_day {
        let first = ymd(&e.start).unwrap_or_default();
        let last = ymd(&e.end)
            .map_or(first, |x| x - Duration::days(1))
            .max(first);
        (first, last)
    } else {
        let first = local(e.start_ms, tz).date();
        // An end exactly at midnight doesn't touch the next day.
        let last = local((e.end_ms - 1).max(e.start_ms), tz).date();
        (first, last)
    }
}

impl Store {
    pub fn new(tz: Tz) -> Store {
        Store {
            tz,
            now: now_ms,
            months: BTreeMap::new(),
            changes: Vec::new(),
            hidden: BTreeSet::new(),
            events: Vec::new(),
            by_day: HashMap::new(),
            calendars: vec![("Lucina".into(), Source::Lucina)],
            next_op: 0,
        }
    }

    pub fn today(&self) -> NaiveDate {
        local((self.now)(), self.tz).date()
    }

    pub fn next_op(&mut self) -> u64 {
        self.next_op += 1;
        self.next_op
    }

    /// Everything derived; called when something changes, never per frame.
    pub fn rebuild(&mut self) {
        let mut seen: BTreeSet<(&str, i64)> = BTreeSet::new();
        let mut all: Vec<Event> = Vec::new();
        for list in self.months.values() {
            for e in list {
                if seen.insert((e.id.as_str(), e.start_ms)) {
                    all.push(e.clone());
                }
            }
        }
        for c in &self.changes {
            match c {
                Change::Create { ghost, .. } => all.push(ghost.clone()),
                Change::Update {
                    id,
                    start_ms,
                    ghost,
                    ..
                } => {
                    all.retain(|e| !(&e.id == id && e.start_ms == *start_ms));
                    all.push(ghost.clone());
                }
                Change::Delete { id, start_ms, .. } => {
                    all.retain(|e| !(&e.id == id && e.start_ms == *start_ms));
                }
            }
        }
        let mut names: BTreeSet<(String, Source)> = BTreeSet::new();
        for e in &all {
            names.insert((calendar_name(e), e.source));
        }
        self.calendars = vec![("Lucina".to_owned(), Source::Lucina)];
        self.calendars
            .extend(names.into_iter().filter(|(n, _)| n != "Lucina"));
        all.retain(|e| !self.hidden.contains(&calendar_name(e)));
        // All-day first, then by start; the engine's own order within that.
        all.sort_by(|a, b| {
            (!a.all_day, a.start_ms, b.end_ms, &a.title)
                .cmp(&(!b.all_day, b.start_ms, a.end_ms, &b.title))
        });
        self.by_day.clear();
        for (i, e) in all.iter().enumerate() {
            let (first, last) = days_of(e, self.tz);
            let mut d = first;
            while d <= last && (d - first).num_days() < 400 {
                self.by_day.entry(d).or_default().push(i);
                d += Duration::days(1);
            }
        }
        self.events = all;
    }

    pub fn on_day(&self, d: NaiveDate) -> &[usize] {
        self.by_day.get(&d).map_or(&[], Vec::as_slice)
    }

    /// An event as the form would make it, shown at once. Temporary ids are never sent anywhere.
    pub fn ghost(&self, d: &Draft, op: u64, base: Option<&Event>) -> Event {
        let (start_ms, end_ms, start, end) = if d.all_day {
            let s = ymd(&d.start).unwrap_or_default();
            let e = d
                .end
                .as_deref()
                .and_then(ymd)
                .unwrap_or(s + Duration::days(1));
            (
                day_start(self.tz, s).timestamp_millis(),
                day_start(self.tz, e).timestamp_millis(),
                d.start.clone(),
                e.format("%Y-%m-%d").to_string(),
            )
        } else {
            let parse = |s: &str| {
                DateTime::parse_from_rfc3339(s)
                    .map(|t| t.timestamp_millis())
                    .unwrap_or_default()
            };
            let s = parse(&d.start);
            let e = d.end.as_deref().map_or(s + 3_600_000, parse);
            (s, e, d.start.clone(), d.end.clone().unwrap_or_default())
        };
        let mut ev = match base {
            Some(b) => b.clone(),
            None => Event {
                attendees: vec![],
                id: format!("pending:{op}"),
                source: Source::Lucina,
                source_id: format!("pending:{op}"),
                calendar: Some("Lucina".into()),
                title: String::new(),
                start: String::new(),
                end: String::new(),
                all_day: false,
                start_ms: 0,
                end_ms: 0,
                location: None,
                description: None,
                url: None,
                conference_url: None,
                added_by: None,
                alarms: vec![],
                alarms_default: false,
                free: false,
                travel_min: None,
                leave_alert: false,
                recurring: false,
                editable: false,
            },
        };
        ev.title = d.title.clone();
        ev.start = start;
        ev.end = end;
        ev.all_day = d.all_day;
        ev.start_ms = start_ms;
        ev.end_ms = end_ms;
        ev.location = d.location.clone().filter(|x| !x.is_empty());
        ev.description = d.description.clone().filter(|x| !x.is_empty());
        ev.conference_url = d.conference_url.clone();
        ev
    }

    /// An event the server has confirmed: in memory until the next load brings its truth.
    pub fn confirmed(&mut self, e: Event) {
        let month = month_of(days_of(&e, self.tz).0);
        let list = self.months.entry(month).or_default();
        list.retain(|x| !(x.id == e.id && x.start_ms == e.start_ms));
        list.push(e);
    }

    pub fn remove(&mut self, id: &str, start_ms: i64) {
        for list in self.months.values_mut() {
            list.retain(|x| !(x.id == id && x.start_ms == start_ms));
        }
    }

    /// An event already there with this title and start: on the iPhone it would be a duplicate.
    pub fn duplicate_of(&self, d: &Draft, except: Option<&str>) -> Option<&Event> {
        let ghost = self.ghost(d, 0, None);
        self.events.iter().find(|e| {
            Some(e.id.as_str()) != except
                && e.start_ms == ghost.start_ms
                && e.all_day == ghost.all_day
                && e.title.trim().eq_ignore_ascii_case(ghost.title.trim())
        })
    }
}

/// Side-by-side columns for overlapping events in one day: index into the input, its column, and the
/// columns of its cluster.
pub fn overlap_columns(spans: &[(i64, i64)]) -> Vec<(usize, usize)> {
    let mut order: Vec<usize> = (0..spans.len()).collect();
    order.sort_by_key(|&i| (spans[i].0, -spans[i].1));
    let mut out = vec![(0usize, 1usize); spans.len()];
    let mut cluster: Vec<usize> = Vec::new();
    let mut cols: Vec<i64> = Vec::new();
    let mut cluster_end = i64::MIN;
    let finish = |cluster: &mut Vec<usize>, cols: &mut Vec<i64>, out: &mut Vec<(usize, usize)>| {
        for &i in cluster.iter() {
            out[i].1 = cols.len().max(1);
        }
        cluster.clear();
        cols.clear();
    };
    for i in order {
        let (s, e) = spans[i];
        if s >= cluster_end {
            finish(&mut cluster, &mut cols, &mut out);
            cluster_end = e;
        }
        cluster_end = cluster_end.max(e);
        let col = cols.iter().position(|&end| end <= s).unwrap_or_else(|| {
            cols.push(0);
            cols.len() - 1
        });
        cols[col] = e;
        out[i].0 = col;
        cluster.push(i);
    }
    finish(&mut cluster, &mut cols, &mut out);
    out
}
