//! The home dashboard's data from outside the shell, gathered on threads so the shell's loop never waits for the network or a
//! disk (docs/design/phone-concept.md, Juno; apps/lucina-glass/src/home/data.rs says what each card needs).
//!
//! Every value is a real answer: the calendar app's own route and cache, Lucina's decisions, the agents' pipeline board, the
//! tasks counts, the event hub, and the recorder's metadata files. A source with nothing to say, or one that cannot be reached,
//! sends `None`, an empty list or `Gone`, and the home leaves its card out.
//!
//! * One coordinator thread (`home-feed`) waits on the shell's `Ask` channel until the next source is due. It asks the network
//!   nothing while the screen is off, from `Ask::Sleep` until `Ask::Refresh`.
//! * Each fetch runs on its own short thread, one at a time per source, so a slow source never delays the others.
//! * The event hub is read on its own thread (`home-events`): it blocks in the hub's `wait`, so there is nothing to poll.
//! * The pure conversions (events to a week, the deadline's words, decisions to notes, the board to the Build card, ...) take
//!   plain values and are tested on the host.

use std::collections::{BTreeMap, HashMap};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Datelike, Duration as Days, NaiveDate, NaiveDateTime, Timelike, Utc};
use chrono_tz::Tz;
use lucina_account::{ApiError, Client, credential, system};
use lucina_agents_client::model::{BoardJob, PipelineBoard};
use lucina_app::events::Reader;
use lucina_app::events::wire::{self, Event as HubEvent, Filter};
use lucina_glass::home::{Build, Day, Deadline, Dot, Media, Note, Upload, Week};
use secretary_calendar::model::{day_start, local_to_utc};
use secretary_calendar::{Event, Source as CalSource};
use serde_json::Value;

use crate::home_cache;

/// How often each source is read while the screen is on.
const CALENDAR_EVERY: Duration = Duration::from_secs(5 * 60);
const DECISIONS_EVERY: Duration = Duration::from_secs(2 * 60);
const AGENTS_EVERY: Duration = Duration::from_secs(60);
const TASKS_EVERY: Duration = Duration::from_secs(5 * 60);
const UPLOADS_EVERY: Duration = Duration::from_secs(60);

/// The working day the week's hour bars measure.
const FIRST_HOUR: u32 = 8;
const LAST_HOUR: u32 = 18;
/// Characters of a title or question on a card before "…".
const FACT_CHARS: usize = 40;
/// Notifications kept from the decisions and from the event hub.
const NOTES_MAX: usize = 5;
/// A note this young says "Just now"; older ones say the time.
const JUST_NOW_MS: i64 = 2 * 60 * 1000;
/// A recording's metadata file untouched for this long is not an upload in progress (the recorder rewrites it after every chunk).
const UPLOAD_ALIVE: Duration = Duration::from_secs(10 * 60);
/// The hub's `wait` returns after this long even with nothing to report; the notes' ages are refreshed then.
const EVENT_WAIT_MS: u64 = 60_000;
/// The calendar's own cache of months (apps/lucina-app-calendar/src/account.rs).
const CALENDAR_CACHE: &str = "calendar-cache.json";
const DEFAULT_TZ: Tz = chrono_tz::Europe::Copenhagen;
/// Every request a Home source makes gives up after this long (the account client's default is 15 s, which left a card missing
/// for that long when the network was half up).
const HOME_TIMEOUT: Duration = Duration::from_secs(3);
/// How long, and how often, the feed looks for the cache while /data is not mounted yet.
const CACHE_WAIT: Duration = Duration::from_secs(10);
const CACHE_RETRY: Duration = Duration::from_secs(1);

/// One source's latest answer. Each replaces that source's previous answer whole. `None` in a field means "this source has
/// nothing for that card": the card is left out (never sample text).
#[derive(Clone, Debug, PartialEq)]
pub enum Update {
    /// From the calendar: the next event as the deadline card, this week's days, and when the next event starts (for the
    /// dock's countdown, shown only within the hour).
    Calendar {
        deadline: Option<Deadline>,
        week: Option<Week>,
        next_start: Option<SystemTime>,
    },
    /// Lucina's pending decisions: their count, and the newest ones as notifications.
    Decisions { pending: u32, notes: Vec<Note> },
    /// The agents: how many run, and the pipeline's current job as the Build card.
    Agents { running: u32, build: Option<Build> },
    /// Tasks due today and overdue.
    /// With each count, its top tasks (id, title): today's soonest first, the overdue ones most recently due first.
    Tasks {
        due_today: u32,
        overdue: u32,
        today: Vec<(String, String)>,
        overdue_tasks: Vec<(String, String)>,
    },
    /// What the machine said happened (an app crashed, a notice), as notifications, newest first.
    Events { notes: Vec<Note> },
    /// What the player plays or last played.
    Media(Option<Media>),
    /// Uploads going out.
    Uploads(Option<Upload>),
    /// A source cannot be reached at all (signed out, offline): its cards go.
    Gone(Source),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Calendar,
    Decisions,
    Agents,
    Tasks,
}

/// What the shell tells the thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Look again now (the screen came on, the home came back).
    Refresh,
    /// The screen is off: stop asking the network until `Refresh`.
    Sleep,
    /// A notification was cleared on the screen.
    Dismissed,
}

pub struct Feed {
    rx: Receiver<(Update, bool)>,
    tx: Sender<Ask>,
    wake: OwnedFd,
}

/// Where the sources put their answers: the channel, and one byte on the wake pipe for the shell's `poll`.
#[derive(Clone)]
struct Sink {
    tx: Sender<(Update, bool)>,
    wake: Arc<OwnedFd>,
    cache: Arc<home_cache::Shared>,
}

impl Sink {
    /// A live answer: replaces a cached card, is written to the cache when it is a real one, and a `Gone` does not remove a
    /// cached card in the first minute.
    fn send(&self, u: Update) {
        if !self.cache.admit_live(&u, Instant::now()) {
            return;
        }
        if lucina_account::client::clock_is_set() {
            self.cache.persist(Path::new(home_cache::DIR), &u, now_ms());
        }
        self.push(u, false);
    }

    /// The last known answer of a source, read from the cache before the network has said anything.
    fn send_cached(&self, u: Update) {
        if self.cache.admit_cached(&u) {
            self.push(u, true);
        }
    }

    fn push(&self, u: Update, cached: bool) {
        if self.tx.send((u, cached)).is_err() {
            return;
        }
        let byte = 1u8;
        // SAFETY: writing one byte from a local to our own non-blocking pipe. A full pipe (EAGAIN) already has the shell awake
        // and is ignored on purpose.
        let _ = unsafe { libc::write(self.wake.as_raw_fd(), (&byte as *const u8).cast(), 1) };
    }
}

impl Feed {
    /// Start the thread. It sends an update as each source answers, and writes a byte to the wake pipe for each.
    pub fn start() -> Option<Feed> {
        let mut pipe = [-1; 2];
        // SAFETY: pipe2 fills the two-element array.
        if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } < 0 {
            return None;
        }
        // SAFETY: both descriptors were just made; each gets one owner.
        let (r, w) = unsafe {
            use std::os::fd::FromRawFd;
            (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1]))
        };
        let (utx, urx) = std::sync::mpsc::channel::<(Update, bool)>();
        let (atx, arx) = std::sync::mpsc::channel::<Ask>();
        let sink = Sink {
            tx: utx,
            wake: Arc::new(w),
            cache: Arc::new(home_cache::Shared::new(Instant::now())),
        };
        let events_sink = sink.clone();
        std::thread::Builder::new()
            .name("home-feed".into())
            .spawn(move || coordinate(arx, sink))
            .ok()?;
        // The event hub is local and blocking: its own thread, which ends at once when the hub's socket is not there.
        let _ = std::thread::Builder::new()
            .name("home-events".into())
            .spawn(move || read_events(events_sink));
        Some(Feed {
            rx: urx,
            tx: atx,
            wake: r,
        })
    }

    /// The descriptor the shell polls: readable when updates are waiting.
    pub fn fd(&self) -> RawFd {
        self.wake.as_raw_fd()
    }

    /// Every update that has come, in order; empties the wake pipe.
    pub fn drain(&mut self) -> Vec<Update> {
        self.drain_marked().into_iter().map(|(u, _)| u).collect()
    }

    /// [`Self::drain`], each update with whether it is a last known value read from the cache (`true`) rather than a live
    /// answer. A cached one is real but may be hours old; the first live answer of its source replaces it.
    pub fn drain_marked(&mut self) -> Vec<(Update, bool)> {
        let mut buf = [0u8; 64];
        // SAFETY: reading into a local buffer from our own non-blocking pipe.
        while unsafe { libc::read(self.wake.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0 {
        }
        self.rx.try_iter().collect()
    }

    pub fn ask(&self, a: Ask) {
        let _ = self.tx.send(a);
    }
}

// ---- The coordinator ----

/// One polled source: when it is next due, and whether a fetch of it is still running.
struct Slot {
    every: Duration,
    due: Instant,
    busy: Arc<AtomicBool>,
}

impl Slot {
    fn new(every: Duration, now: Instant) -> Slot {
        Slot {
            every,
            due: now,
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Runs `fetch` on its own thread if this source is due and its last fetch is over; the next time is `every` from now.
    fn run(&mut self, now: Instant, name: &str, fetch: impl FnOnce() + Send + 'static) {
        if now < self.due {
            return;
        }
        self.due = now + self.every;
        if self.busy.swap(true, Ordering::AcqRel) {
            return;
        }
        let guard = Busy(self.busy.clone());
        // If the thread cannot start, the closure and its guard drop with the failed spawn and clear the flag.
        let _ = std::thread::Builder::new()
            .name(format!("home-{name}"))
            .spawn(move || {
                let _guard = guard;
                fetch();
            });
    }
}

/// Clears a source's "fetch running" flag when its thread ends, even by a panic.
struct Busy(Arc<AtomicBool>);

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn coordinate(asks: Receiver<Ask>, sink: Sink) {
    let tz = timezone();
    let start = Instant::now();
    let mut calendar = Slot::new(CALENDAR_EVERY, start);
    let mut decisions = Slot::new(DECISIONS_EVERY, start);
    let mut agents = Slot::new(AGENTS_EVERY, start);
    let mut tasks = Slot::new(TASKS_EVERY, start);
    let mut uploads = Slot::new(UPLOADS_EVERY, start);
    let mut awake = true;
    // The last known values go out before any fetch. /data is mounted about 2.5 s after the kernel: until the cache directory is
    // there, look again every second for CACHE_WAIT, on this thread's own wake-ups.
    let mut cache_pending = !load_cache(&sink);
    loop {
        if cache_pending && (start.elapsed() >= CACHE_WAIT || load_cache(&sink)) {
            cache_pending = false;
        }
        let now = Instant::now();
        if awake {
            let s = sink.clone();
            calendar.run(now, "calendar", move || {
                traced("calendar", || fetch_calendar(&s, tz))
            });
            let s = sink.clone();
            decisions.run(now, "decisions", move || {
                traced("decisions", || fetch_decisions(&s))
            });
            let s = sink.clone();
            agents.run(now, "agents", move || traced("agents", || fetch_agents(&s)));
            let s = sink.clone();
            tasks.run(now, "tasks", move || {
                traced("tasks", || fetch_tasks(&s, tz))
            });
            let s = sink.clone();
            uploads.run(now, "uploads", move || {
                traced("uploads", || {
                    s.send(Update::Uploads(pending_uploads(
                        &recordings_dir(),
                        SystemTime::now(),
                    )));
                    Outcome::Ok
                })
            });
        }
        // Asleep: nothing is due, so wait for the shell without a timeout.
        let next = if awake {
            [&calendar, &decisions, &agents, &tasks, &uploads]
                .iter()
                .map(|s| s.due)
                .min()
        } else {
            None
        };
        let mut wait = next.map(|due| due.saturating_duration_since(Instant::now()));
        if cache_pending {
            wait = Some(wait.map_or(CACHE_RETRY, |w| w.min(CACHE_RETRY)));
        }
        let ask = match wait {
            Some(w) => asks.recv_timeout(w),
            None => asks.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match ask {
            Ok(Ask::Refresh) => {
                awake = true;
                let now = Instant::now();
                for s in [
                    &mut calendar,
                    &mut decisions,
                    &mut agents,
                    &mut tasks,
                    &mut uploads,
                ] {
                    s.due = now;
                }
            }
            Ok(Ask::Sleep) => awake = false,
            // The shell keeps the list of cleared ids itself; the sources have nothing to do about it.
            Ok(Ask::Dismissed) => {}
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// `/etc/lucina/timezone` as an IANA name, as the calendar app reads it; Copenhagen otherwise.
fn timezone() -> Tz {
    std::fs::read_to_string("/etc/lucina/timezone")
        .ok()
        .and_then(|s| secretary_calendar::parse_tz(&s))
        .unwrap_or(DEFAULT_TZ)
}

/// How a fetch ended, for the per-source trace line.
enum Outcome {
    /// It answered and the answer went to the shell.
    Ok,
    /// The source is `Gone` without a failed call (nobody signed in, no clock, an answer that says nothing).
    Gone,
    /// A call failed; the reason is a few words.
    Error(String),
}

/// A failed call in a few words, never the server's text.
fn reason(e: &ApiError) -> String {
    match e {
        ApiError::Unauthorized(_) => "unauthorized".into(),
        ApiError::RateLimited(s) => format!("rate limited {s}s"),
        ApiError::Http(status, _, _) => format!("http {status}"),
        ApiError::Network(m) => {
            let short: String = m.chars().take(48).collect();
            format!("network {short}")
        }
        ApiError::Protocol(_) => "protocol".into(),
    }
}

/// Runs one fetch and logs one line to stderr when it ends: `lucina-shell: home source <name> <ms> ms <ok|gone|error: why>`.
/// Always on (one line per fetch, every 1 to 5 minutes per source): it is how a slow source gets named.
fn traced(name: &str, fetch: impl FnOnce() -> Outcome) {
    let t = Instant::now();
    let outcome = fetch();
    let ms = t.elapsed().as_millis();
    match outcome {
        Outcome::Ok => eprintln!("lucina-shell: home source {name} {ms} ms ok"),
        Outcome::Gone => eprintln!("lucina-shell: home source {name} {ms} ms gone"),
        Outcome::Error(why) => eprintln!("lucina-shell: home source {name} {ms} ms error: {why}"),
    }
}

/// Sends every cached value that can be shown, if the cache directory is there; whether it was.
fn load_cache(sink: &Sink) -> bool {
    let dir = Path::new(home_cache::DIR);
    if !dir.is_dir() {
        return false;
    }
    for u in home_cache::load_all(dir, now_ms(), lucina_account::client::clock_is_set()) {
        sink.send_cached(u);
    }
    true
}

/// A signed-in client, or `Gone` for `source` when nobody is signed in.
fn client_or_gone(sink: &Sink, source: Source) -> Option<Client> {
    let client = system::client();
    if client.is_none() {
        sink.send(Update::Gone(source));
    }
    client
}

/// What a failed call means for the home: a 401 tells the account service to forget the token, as every Juno app does.
fn failed(e: &ApiError) {
    if matches!(e, ApiError::Unauthorized(_)) {
        system::report_rejected();
    }
}

// ---- Calendar ----

/// Local wall-clock time of an instant.
fn local(ms: i64, tz: Tz) -> NaiveDateTime {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&tz)
        .naive_local()
}

fn ymd(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok()
}

fn ms_of(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

/// The first and last day an all-day event covers (its end date is exclusive).
fn all_day_span(e: &Event) -> Option<(NaiveDate, NaiveDate)> {
    let first = ymd(&e.start)?;
    let last = ymd(&e.end).map_or(first, |x| x - Days::days(1)).max(first);
    Some((first, last))
}

/// The monday of the week `now_ms` is in, in the owner's zone.
fn monday_of(now_ms: i64, tz: Tz) -> NaiveDate {
    let today = local(now_ms, tz).date();
    today - Days::days(i64::from(today.weekday().num_days_from_monday()))
}

/// What one calendar read asks for: this week and the next, so that the deadline can be past Sunday.
fn window(now_ms: i64, tz: Tz) -> (NaiveDate, NaiveDate) {
    let monday = monday_of(now_ms, tz);
    (monday, monday + Days::days(14))
}

/// Events once each (the cache's months overlap), all-day first and then by start, as the calendar app orders them.
fn ordered(events: &[Event]) -> Vec<&Event> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out: Vec<&Event> = events
        .iter()
        .filter(|e| seen.insert((e.id.as_str(), e.start_ms)))
        .collect();
    out.sort_by(|a, b| {
        (!a.all_day, a.start_ms, b.end_ms, &a.title)
            .cmp(&(!b.all_day, b.start_ms, a.end_ms, &b.title))
    });
    out
}

/// Sorted, overlapping or touching intervals joined.
fn merged(mut spans: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    spans.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in spans {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Minutes since local midnight.
fn minute_of_day(ms: i64, tz: Tz) -> u32 {
    let t = local(ms, tz);
    t.hour() * 60 + t.minute()
}

/// "9–10, 14–15": the busy spans of a day in whole hours, rounded outward (a start down, an end up), neighbours joined.
fn ranges_text(busy: &[(i64, i64)], day: (i64, i64), tz: Tz) -> String {
    let mut hours: Vec<(u32, u32)> = Vec::new();
    for &(s, e) in busy {
        let from = if s <= day.0 {
            0
        } else {
            minute_of_day(s, tz) / 60
        };
        let to = if e >= day.1 {
            24
        } else {
            minute_of_day(e, tz).div_ceil(60)
        };
        match hours.last_mut() {
            Some(last) if from <= last.1 => last.1 = last.1.max(to),
            _ => hours.push((from, to)),
        }
    }
    hours
        .iter()
        .map(|(a, b)| format!("{a}–{b}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One day of the agenda: its dots, its busy minutes in each working hour and its busy ranges in words.
fn day_of(events: &[&Event], date: NaiveDate, today: NaiveDate, now_ms: i64, tz: Tz) -> Day {
    let day = (
        ms_of(day_start(tz, date)),
        ms_of(day_start(tz, date + Days::days(1))),
    );
    let mut dots: Vec<Dot> = Vec::new();
    let mut busy: Vec<(i64, i64)> = Vec::new();
    for e in events {
        let own = if e.source == CalSource::Lucina {
            Dot::Own
        } else {
            Dot::Other
        };
        if e.all_day {
            if all_day_span(e).is_some_and(|(first, last)| first <= date && date <= last) {
                dots.push(own);
            }
            continue;
        }
        let (s, en) = (e.start_ms, e.end_ms.max(e.start_ms));
        // An event that ends exactly at midnight does not touch the next day; one with no length belongs to the day it is on.
        if !(s < day.1 && (en > day.0 || (s == en && s >= day.0))) {
            continue;
        }
        dots.push(if s <= now_ms && now_ms < en {
            Dot::Now
        } else {
            own
        });
        // An event marked free (it does not block the time) does not count as busy.
        if !e.free && s.max(day.0) < en.min(day.1) {
            busy.push((s.max(day.0), en.min(day.1)));
        }
    }
    // Three dots fit: the one happening now is never the one cut.
    dots.sort_by_key(|d| match d {
        Dot::Now => 0,
        Dot::Own => 1,
        Dot::Other => 2,
    });
    dots.truncate(3);
    let busy = merged(busy);
    let hours = (FIRST_HOUR..LAST_HOUR)
        .map(|h| {
            let at = |h: u32| {
                ms_of(local_to_utc(
                    tz,
                    date.and_hms_opt(h, 0, 0).expect("an hour of the day"),
                ))
            };
            let slot = (at(h), at(h + 1));
            let ms: i64 = busy
                .iter()
                .map(|&(s, e)| (e.min(slot.1) - s.max(slot.0)).max(0))
                .sum();
            (ms / 60_000) as u32
        })
        .collect();
    Day {
        n: date.day(),
        name: date.format("%a").to_string(),
        dots,
        today: date == today,
        hours,
        ranges: ranges_text(&busy, day, tz),
    }
}

/// Monday to Sunday of the week `now_ms` is in, today selected. On a Saturday or Sunday it is the coming week, Monday selected:
/// the day chips are Monday to Friday, and the week ahead is what is worth seeing then.
pub fn week_of(events: &[Event], now_ms: i64, tz: Tz) -> Week {
    let events = ordered(events);
    let today = local(now_ms, tz).date();
    let weekday = today.weekday().num_days_from_monday() as usize;
    let weekend = weekday >= 5;
    let monday = monday_of(now_ms, tz) + Days::days(if weekend { 7 } else { 0 });
    let days = (0..7)
        .map(|i| day_of(&events, monday + Days::days(i), today, now_ms, tz))
        .collect();
    Week {
        days,
        selected: if weekend { 0 } else { weekday },
        first_hour: FIRST_HOUR,
        last_hour: LAST_HOUR,
    }
}

/// A title or question as one line of at most [`FACT_CHARS`] characters, then "…".
fn cut(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > FACT_CHARS {
        let mut out: String = one.chars().take(FACT_CHARS).collect();
        out.push('…');
        out
    } else {
        one
    }
}

/// "Now", "Today 15:00", "Tomorrow 09:30", "Tue 09:30" (within the week), "Tue 17 Nov 09:30" (further).
pub fn deadline_line(start_ms: i64, now_ms: i64, tz: Tz) -> String {
    if start_ms <= now_ms {
        return "Now".into();
    }
    let at = local(start_ms, tz);
    let time = at.format("%H:%M");
    match (at.date() - local(now_ms, tz).date()).num_days() {
        0 => format!("Today {time}"),
        1 => format!("Tomorrow {time}"),
        2..=6 => format!("{} {time}", at.format("%a")),
        _ => format!("{} {time}", at.format("%a %-d %b")),
    }
}

/// The next timed event that has not ended, as the deadline card. All-day events have no time to count down to and are left out.
pub fn deadline_of(events: &[Event], now_ms: i64, tz: Tz) -> Option<Deadline> {
    let e = ordered(events)
        .into_iter()
        .filter(|e| {
            !e.all_day && (e.end_ms > now_ms || (e.end_ms <= e.start_ms && e.start_ms >= now_ms))
        })
        .min_by_key(|e| (e.start_ms, e.end_ms))?;
    Some(Deadline {
        fact: cut(&e.title),
        line: deadline_line(e.start_ms, now_ms, tz),
        soon: e.start_ms <= now_ms + 60 * 60 * 1000,
    })
}

/// When the next timed event starts, if it is still to come.
pub fn next_start_of(events: &[Event], now_ms: i64) -> Option<SystemTime> {
    let ms = events
        .iter()
        .filter(|e| !e.all_day && e.start_ms > now_ms)
        .map(|e| e.start_ms)
        .min()?;
    Some(UNIX_EPOCH + Duration::from_millis(ms as u64))
}

/// Everything the calendar gives the home, from events and the time.
pub fn calendar_update(events: &[Event], now_ms: i64, tz: Tz) -> Update {
    Update::Calendar {
        deadline: deadline_of(events, now_ms, tz),
        week: Some(week_of(events, now_ms, tz)),
        next_start: next_start_of(events, now_ms),
    }
}

/// `{from, to, timeZone, events, sources}`: one event the app cannot read does not cost the others.
fn events_in(v: &Value) -> Option<Vec<Event>> {
    Some(
        v.get("events")?
            .as_array()?
            .iter()
            .filter_map(|e| serde_json::from_value(e.clone()).ok())
            .collect(),
    )
}

/// The calendar app's saved months (`calendar-cache.json`: month start to events), if they cover every month of the window.
/// A month the app never opened is not "a free week": then there is no cache answer and the home waits for the server.
fn cached_events(from: NaiveDate, to: NaiveDate) -> Option<Vec<Event>> {
    let saved: BTreeMap<String, Vec<Value>> =
        serde_json::from_slice(&credential::read_private(CALENDAR_CACHE)?).ok()?;
    let mut out = Vec::new();
    let mut month = from.with_day(1)?;
    while month < to {
        let list = saved.get(&month.format("%Y-%m-%d").to_string())?;
        out.extend(
            list.iter()
                .filter_map(|e| serde_json::from_value::<Event>(e.clone()).ok()),
        );
        month = NaiveDate::from_ymd_opt(
            month.year() + i32::from(month.month() == 12),
            month.month() % 12 + 1,
            1,
        )?;
    }
    Some(out)
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn fetch_calendar(sink: &Sink, tz: Tz) -> Outcome {
    // Before the clock is set (Juno starts in 1970) there is no week to show.
    if !lucina_account::client::clock_is_set() {
        sink.send(Update::Gone(Source::Calendar));
        return Outcome::Gone;
    }
    let Some(client) = client_or_gone(sink, Source::Calendar) else {
        return Outcome::Gone;
    };
    let now_ms = Utc::now().timestamp_millis();
    let (first, last) = window(now_ms, tz);
    // What the calendar app saved last time goes first: on screen before any network.
    let mut sent = false;
    if let Some(events) = cached_events(first, last) {
        sink.send(calendar_update(&events, now_ms, tz));
        sent = true;
    }
    let at = |d: NaiveDate| {
        encode(&day_start(tz, d).to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
    };
    match client.get_json_within(
        &format!("/v1/calendar/events?from={}&to={}", at(first), at(last)),
        HOME_TIMEOUT,
    ) {
        Ok(v) => match events_in(&v) {
            Some(events) => {
                sink.send(calendar_update(&events, Utc::now().timestamp_millis(), tz));
                Outcome::Ok
            }
            None if !sent => {
                sink.send(Update::Gone(Source::Calendar));
                Outcome::Gone
            }
            None => Outcome::Gone,
        },
        Err(e) => {
            failed(&e);
            // Offline with last time's months on screen: they stay (they are real, only older). Nothing to show otherwise.
            if !sent || matches!(e, ApiError::Unauthorized(_)) {
                sink.send(Update::Gone(Source::Calendar));
            }
            Outcome::Error(reason(&e))
        }
    }
}

// ---- Decisions ----

/// How many decisions wait, and the newest five as notifications.
pub fn decision_notes(all: &[lucina_account::decisions::Decision]) -> (u32, Vec<Note>) {
    let mut pending: Vec<&lucina_account::decisions::Decision> =
        all.iter().filter(|d| d.status == "pending").collect();
    pending.sort_by(|a, b| (&b.created_at, &b.id).cmp(&(&a.created_at, &a.id)));
    let notes = pending
        .iter()
        .take(NOTES_MAX)
        .map(|d| Note {
            id: format!("decision:{}", d.id),
            fact: cut(&d.question),
            line: "Lucina asks".into(),
            urgent: d.urgency.eq_ignore_ascii_case("high"),
            app: Some("decisions".into()),
            at: None,
        })
        .collect();
    (pending.len() as u32, notes)
}

fn fetch_decisions(sink: &Sink) -> Outcome {
    let Some(client) = client_or_gone(sink, Source::Decisions) else {
        return Outcome::Gone;
    };
    // `Client::decisions(Some("pending"), 100)` with the Home timeout instead of the default one.
    match client.get_json_within("/v1/decisions?limit=100&status=pending", HOME_TIMEOUT) {
        Ok(v) => {
            let list: Vec<lucina_account::decisions::Decision> = v["items"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(lucina_account::decisions::Decision::parse)
                        .collect()
                })
                .unwrap_or_default();
            let (pending, notes) = decision_notes(&list);
            sink.send(Update::Decisions { pending, notes });
            Outcome::Ok
        }
        Err(e) => {
            failed(&e);
            sink.send(Update::Gone(Source::Decisions));
            Outcome::Error(reason(&e))
        }
    }
}

// ---- Agents ----

/// Agents that run now, from `GET /v1/agents/items` (`{agents: [...]}`): not archived and not Lucina's own internal ones.
pub fn agents_running(items: &Value) -> u32 {
    let list = items
        .get("agents")
        .and_then(Value::as_array)
        .or_else(|| items.as_array());
    let running = |a: &&Value| {
        let status = a.get("status").and_then(Value::as_str);
        let internal = a
            .get("labels")
            .and_then(|l| l.get("lucina.internal"))
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty());
        let archived = a.get("archivedAt").is_some_and(|v| !v.is_null());
        matches!(status, Some("running" | "working")) && !internal && !archived
    };
    list.map_or(0, |l| l.iter().filter(running).count() as u32)
}

/// The Build card from the pipeline board, while a job runs: the jobs of the last 12 hours (queued, running, finished) are the
/// track's phases, the running one is the current phase, and the share finished is the percentage. The board says nothing about
/// a job's own steps, so nothing finer is shown. At most six dots: a longer day is drawn to scale.
pub fn build_of(board: &PipelineBoard, now: chrono::DateTime<chrono::Utc>) -> Option<Build> {
    let recent = |j: &&BoardJob| {
        chrono::DateTime::parse_from_rfc3339(&j.created_at).map_or(true, |t| {
            (now - t.with_timezone(&chrono::Utc)).num_hours() < 12
        })
    };
    let mut jobs: Vec<&BoardJob> = Vec::new();
    for j in board.queue.iter().chain(&board.recent_jobs).filter(recent) {
        if !jobs.iter().any(|k| k.job_id == j.job_id) {
            jobs.push(j);
        }
    }
    let job = jobs
        .iter()
        .filter(|j| j.state == "running")
        .max_by(|a, b| a.created_at.cmp(&b.created_at))?;
    let n = jobs.len() as u32;
    let done = jobs
        .iter()
        .filter(|j| j.state != "running" && j.state != "queued")
        .count() as u32;
    let title = if job.kind == "merge" || job.kind.is_empty() {
        "Build".to_owned()
    } else {
        let word = job
            .kind
            .split(|c: char| !c.is_alphanumeric())
            .next()
            .unwrap_or("");
        let mut chars = word.chars();
        chars.next().map_or_else(
            || "Build".to_owned(),
            |c| c.to_uppercase().collect::<String>() + chars.as_str(),
        )
    };
    let phases = n.clamp(1, 6);
    let phase = ((done * phases) / n.max(1) + 1).min(phases);
    Some(Build {
        title,
        phase,
        phases,
        percent: done * 100 / n.max(1),
        live: true,
        line: format!("Job {} of {}", (done + 1).min(n), n),
        app: Some("agents".into()),
    })
}

fn fetch_agents(sink: &Sink) -> Outcome {
    let Some(client) = client_or_gone(sink, Source::Agents) else {
        return Outcome::Gone;
    };
    let running = match client.get_json_within("/v1/agents/items", HOME_TIMEOUT) {
        Ok(v) => agents_running(&v),
        Err(e) => {
            failed(&e);
            sink.send(Update::Gone(Source::Agents));
            return Outcome::Error(reason(&e));
        }
    };
    // The board is its own route; without it the running count still stands and there is no Build card.
    let build = client
        .get_json_within("/v1/agents/pipeline/board", HOME_TIMEOUT)
        .ok()
        .and_then(|v| serde_json::from_value::<PipelineBoard>(v).ok())
        .and_then(|b| build_of(&b, chrono::Utc::now()));
    sink.send(Update::Agents { running, build });
    Outcome::Ok
}

// ---- Tasks ----

/// `GET /v1/tasks/state`: the view's counts. `today` counts the overdue ones too, so what is due today is the rest.
/// The top three tasks of today and of the overdue ones, from the state's list (`view.list.groups[].tasks[]`): today's soonest
/// first (timed before untimed), the overdue ones most recently due first. Titles are cut as notifications are.
pub fn task_tops(state: &Value) -> (Vec<(String, String)>, Vec<(String, String)>) {
    // (date, time, id, title) for each.
    let mut today: Vec<(String, String, String, String)> = Vec::new();
    let mut overdue: Vec<(String, String, String, String)> = Vec::new();
    let groups = state
        .get("view")
        .and_then(|v| v.get("list"))
        .and_then(|l| l.get("groups"))
        .and_then(Value::as_array);
    for g in groups.into_iter().flatten() {
        for t in g
            .get("tasks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let (Some(id), Some(content)) = (
                t.get("id").and_then(Value::as_str),
                t.get("content").and_then(Value::as_str),
            ) else {
                continue;
            };
            let due = t.get("due");
            let rel = due.and_then(|d| d.get("rel_days")).and_then(Value::as_i64);
            let time = due
                .and_then(|d| d.get("time"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let date = due
                .and_then(|d| d.get("date"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let row = (date, time, id.to_string(), cut(content));
            match rel {
                Some(r) if r < 0 => overdue.push(row),
                Some(0) => today.push(row),
                _ => {}
            }
        }
    }
    // Today: timed first, by time; then the untimed in the list's order.
    today.sort_by(|a, b| (a.1.is_empty(), &a.1).cmp(&(b.1.is_empty(), &b.1)));
    // Overdue: the most recently due first.
    overdue.sort_by(|a, b| (&b.0, &b.1).cmp(&(&a.0, &a.1)));
    let split = |v: Vec<(String, String, String, String)>| -> Vec<(String, String)> {
        v.into_iter().take(3).map(|r| (r.2, r.3)).collect()
    };
    (split(today), split(overdue))
}

pub fn task_counts(state: &Value) -> Option<(u32, u32)> {
    let counts = state.get("view")?.get("counts")?;
    let n = |k: &str| {
        counts
            .get(k)?
            .as_u64()
            .map(|v| v.min(u64::from(u32::MAX)) as u32)
    };
    let (today, overdue) = (n("today")?, n("overdue")?);
    Some((today.saturating_sub(overdue), overdue))
}

fn fetch_tasks(sink: &Sink, tz: Tz) -> Outcome {
    let Some(client) = client_or_gone(sink, Source::Tasks) else {
        return Outcome::Gone;
    };
    match client.get_json_within(
        &format!("/v1/tasks/state?timezone={}", encode(tz.name())),
        HOME_TIMEOUT,
    ) {
        Ok(v) => match task_counts(&v) {
            Some((due_today, overdue)) => {
                let (today, overdue_tasks) = task_tops(&v);
                sink.send(Update::Tasks {
                    due_today,
                    overdue,
                    today,
                    overdue_tasks,
                });
                Outcome::Ok
            }
            None => {
                sink.send(Update::Gone(Source::Tasks));
                Outcome::Gone
            }
        },
        Err(e) => {
            failed(&e);
            sink.send(Update::Gone(Source::Tasks));
            Outcome::Error(reason(&e))
        }
    }
}

// ---- Uploads ----

fn recordings_dir() -> PathBuf {
    std::env::var_os("LUCINA_RECORDINGS")
        .map_or_else(|| PathBuf::from("/data/recordings"), PathBuf::from)
}

/// The recordings still going to the server, counted from the recorder's own files: next to each `<name>.wav` is a `<name>.json`
/// (`upload::Meta`: `done`, `size_bytes`, `uploaded`, `error`) that the recorder rewrites after every chunk. One counts as going
/// out when it is not done, has no error, and was written within [`UPLOAD_ALIVE`]: a file that stopped changing belongs to a
/// recorder that is not running, and an error means the last try failed. `None` when nothing is going out.
pub fn pending_uploads(dir: &Path, now: SystemTime) -> Option<Upload> {
    let mut n = 0u32;
    let (mut sent, mut size) = (0u64, 0u64);
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let fresh = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age <= UPLOAD_ALIVE);
        let Some(meta) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        let (Some(total), false) = (
            meta.get("size_bytes").and_then(Value::as_u64),
            meta.get("done").and_then(Value::as_bool).unwrap_or(false),
        ) else {
            continue;
        };
        if !fresh || meta.get("error").is_some_and(|e| !e.is_null()) {
            continue;
        }
        n += 1;
        size += total;
        sent += meta
            .get("uploaded")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(total);
    }
    (n > 0).then(|| Upload {
        label: format!("Uploading {n}"),
        fraction: (size > 0).then(|| (sent as f64 / size as f64) as f32),
    })
}

// ---- The event hub ----

/// An event the home shows as a notification, as it was seen.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    seq: u64,
    /// Wall-clock milliseconds when it happened, from the hub's stamp.
    at_ms: i64,
    fact: String,
    urgent: bool,
    app: Option<String>,
}

/// The event's app: the hub's `app`, else its `app` field, else the subject `app:<id>`.
fn app_of(e: &HubEvent) -> Option<String> {
    e.app
        .clone()
        .or_else(|| {
            e.fields
                .get("app")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            e.subject
                .as_deref()
                .and_then(|s| s.strip_prefix("app:"))
                .map(str::to_owned)
        })
        .filter(|a| !a.is_empty())
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

/// A crash, a notice or a finished download as a notification; any other event is none.
fn seen_of(e: &HubEvent, at_ms: i64) -> Option<Seen> {
    let field = |k: &str| {
        e.fields
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let app = app_of(e);
    let (fact, urgent) = match e.kind.as_str() {
        "app.crashed" => (format!("{} stopped", capitalised(app.as_deref()?)), true),
        "notice.shown" => (cut(field("text")?), false),
        "download.done" => (
            cut(field("title").or(field("name")).or(field("file"))?),
            false,
        ),
        _ => return None,
    };
    Some(Seen {
        seq: e.seq,
        at_ms,
        fact,
        urgent,
        app,
    })
}

/// The newest [`NOTES_MAX`] as notes, newest first: "Just now" while young, the time of day after.
fn event_notes(seen: &[Seen], now_ms: i64, tz: Tz) -> Vec<Note> {
    let mut sorted: Vec<&Seen> = seen.iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.seq));
    sorted
        .into_iter()
        .take(NOTES_MAX)
        .map(|s| Note {
            id: format!("event:{}", s.seq),
            fact: s.fact.clone(),
            line: if now_ms - s.at_ms < JUST_NOW_MS {
                "Just now".into()
            } else {
                local(s.at_ms, tz).format("%H:%M").to_string()
            },
            urgent: s.urgent,
            app: s.app.clone(),
            at: Some(s.at_ms),
        })
        .collect()
}

/// What `media.*` events change: titles are remembered by subject, because `media.stopped` does not carry one.
#[derive(Default)]
struct MediaTracker {
    titles: HashMap<String, String>,
}

impl MediaTracker {
    /// The player's new state after this event, if it is a media event with something to say.
    fn apply(&mut self, e: &HubEvent) -> Option<Media> {
        let playing = match e.kind.as_str() {
            "media.started" => true,
            "media.stopped" | "media.ended" => false,
            _ => return None,
        };
        let subject = e.subject.clone().unwrap_or_default();
        // The app that plays it: the hub's app id, else the one the subject's prefix belongs to (the video app names its
        // subjects `item:` and `file:`).
        let app = e.app.clone().or_else(|| {
            (subject.starts_with("item:") || subject.starts_with("file:"))
                .then(|| "video".to_owned())
        })?;
        let title = e
            .fields
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        if let Some(t) = &title {
            if self.titles.len() >= 16 && !self.titles.contains_key(&subject) {
                self.titles.clear();
            }
            self.titles.insert(subject.clone(), t.clone());
        }
        let title = title.or_else(|| self.titles.get(&subject).cloned())?;
        Some(Media {
            title,
            playing,
            app,
        })
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// The wall-clock time of a hub stamp (`t_ms`, CLOCK_MONOTONIC milliseconds, the hub's own clock): now, less how long ago it was.
fn wall_of(t_ms: u64) -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime fills the timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    let mono = ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000;
    now_ms() - (mono - t_ms as i64).max(0)
}

fn read_events(sink: Sink) {
    let tz = timezone();
    let path = Path::new(wire::READ_SOCKET);
    let filter = Filter {
        kind: [
            "app.crashed",
            "notice.shown",
            "media.started",
            "media.stopped",
            "media.ended",
            "download.done",
        ]
        .map(String::from)
        .to_vec(),
        ..Filter::default()
    };
    let mut seen: Vec<Seen> = Vec::new();
    let mut tracker = MediaTracker::default();
    let mut sent: Option<Vec<Note>> = None;
    let mut ever_connected = false;
    loop {
        let mut reader = match Reader::connect(path) {
            Ok(r) => r,
            // Not on Juno, or the hub is not running yet: nothing here to read. After a good connection, the hub restarting is
            // worth another try.
            Err(_) if !ever_connected => return,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(30));
                continue;
            }
        };
        ever_connected = true;
        // Only what happens from now on: older events have no wall-clock time to show. What plays now is in the sticky state.
        let Ok(stats) = reader.stats() else {
            // A hub that answers the connection but not the question: wait before asking again, never spin.
            std::thread::sleep(Duration::from_secs(30));
            continue;
        };
        let mut since = stats.get("next").and_then(Value::as_u64).unwrap_or(0);
        if let Ok(state) = reader.state() {
            for e in state.events.iter().filter(|e| e.kind == "media.started") {
                if let Some(m) = tracker.apply(e) {
                    sink.send(Update::Media(Some(m)));
                }
            }
        }
        if sent.is_none() {
            sent = Some(Vec::new());
            sink.send(Update::Events { notes: Vec::new() });
        }
        // A failed wait (the hub went away) ends this loop; the connection is made again below.
        while let Ok(w) = reader.wait(since, &filter, EVENT_WAIT_MS) {
            since = w.next;
            if let Some(e) = w.event {
                if let Some(s) = seen_of(&e, wall_of(e.t_ms)) {
                    seen.push(s);
                    let keep = seen.len().saturating_sub(NOTES_MAX * 2);
                    seen.drain(..keep);
                }
                if let Some(m) = tracker.apply(&e) {
                    sink.send(Update::Media(Some(m)));
                }
            }
            // New events, and the old ones growing older ("Just now" turns into the time).
            let notes = event_notes(&seen, now_ms(), tz);
            if sent.as_ref() != Some(&notes) {
                sent = Some(notes.clone());
                sink.send(Update::Events { notes });
            }
        }
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_account::decisions::Decision;
    use serde_json::json;

    const TZ: Tz = chrono_tz::Europe::Copenhagen;

    /// Wed 4 Nov 2026 at `h:m` in Copenhagen (UTC+1), as milliseconds.
    fn at(day: u32, h: u32, m: u32) -> i64 {
        ms_of(local_to_utc(
            TZ,
            NaiveDate::from_ymd_opt(2026, 11, day)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap(),
        ))
    }

    fn event(id: &str, source: &str, start: i64, end: i64, title: &str) -> Event {
        serde_json::from_value(json!({
            "id": id, "source": source, "sourceId": id, "title": title,
            "start": "", "end": "", "allDay": false, "startMs": start, "endMs": end,
            "recurring": false, "editable": false,
        }))
        .unwrap()
    }

    fn all_day(id: &str, first: &str, end: &str) -> Event {
        serde_json::from_value(json!({
            "id": id, "source": "iphone", "sourceId": id, "title": "Holiday",
            "start": first, "end": end, "allDay": true, "startMs": 0, "endMs": 0,
            "recurring": false, "editable": false,
        }))
        .unwrap()
    }

    #[test]
    fn the_week_runs_monday_to_sunday_with_today_selected() {
        // Wednesday; on Sunday it is the coming week, Monday selected.
        let w = week_of(&[], at(4, 12, 0), TZ);
        let names: Vec<&str> = w.days.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]);
        assert_eq!(w.days[0].n, 2);
        assert_eq!(w.selected, 2);
        assert!(w.days[2].today && !w.days[1].today);
        assert_eq!((w.first_hour, w.last_hour), (8, 18));
        let sunday = week_of(&[], at(8, 20, 0), TZ);
        assert_eq!((sunday.days[0].n, sunday.selected), (9, 0));
        // No events: free days, ten empty hours, no dots.
        assert!(
            w.days
                .iter()
                .all(|d| d.dots.is_empty() && d.ranges.is_empty() && d.hours == vec![0; 10])
        );
    }

    #[test]
    fn dots_hours_and_ranges_come_from_the_events() {
        let now = at(4, 9, 30);
        let events = vec![
            event("lucina:1", "lucina", at(4, 9, 0), at(4, 10, 0), "Standup"),
            // Overlaps the first and runs on: 9:30-10:30 adds only the half hour after 10:00.
            event("google:2", "google", at(4, 9, 30), at(4, 10, 30), "Review"),
            event("iphone:3", "iphone", at(4, 14, 0), at(4, 15, 0), "Dentist"),
            event("iphone:4", "iphone", at(4, 16, 0), at(4, 16, 30), "Call"),
            all_day("iphone:5", "2026-11-04", "2026-11-05"),
        ];
        let w = week_of(&events, now, TZ);
        let wed = &w.days[2];
        // Both running events first, then the rest: three of the five fit.
        assert_eq!(wed.dots, vec![Dot::Now, Dot::Now, Dot::Other]);
        // Busy minutes per hour from 8: 9:00-10:30 is 60 + 30 (overlap counted once), 14-15 is 60, 16-16:30 is 30. All-day: none.
        assert_eq!(wed.hours, vec![0, 60, 30, 0, 0, 0, 60, 0, 30, 0]);
        assert_eq!(wed.ranges, "9–11, 14–15, 16–17");
        // The all-day event is a dot on its day only.
        assert!(w.days[1].dots.is_empty() && w.days[3].dots.is_empty());
        // Dots are an event each, at most three.
        let many: Vec<Event> = (0..6)
            .map(|i| {
                event(
                    &format!("x{i}"),
                    "iphone",
                    at(5, 9 + i, 0),
                    at(5, 9 + i, 30),
                    "x",
                )
            })
            .collect();
        assert_eq!(week_of(&many, now, TZ).days[3].dots, vec![Dot::Other; 3]);
    }

    #[test]
    fn ranges_round_outward_and_join_neighbours() {
        let events = vec![
            event("a", "iphone", at(4, 9, 15), at(4, 10, 0), "a"),
            event("b", "iphone", at(4, 10, 30), at(4, 11, 10), "b"),
            event("c", "iphone", at(4, 14, 0), at(4, 15, 0), "c"),
        ];
        let w = week_of(&events, at(4, 8, 0), TZ);
        // 9:15-10:00 is 9-10, 10:30-11:10 is 10-12: joined into 9-12.
        assert_eq!(w.days[2].ranges, "9–12, 14–15");
        // An event over midnight is on both days, cut at the day's edge.
        let night = vec![event("n", "iphone", at(4, 23, 0), at(5, 1, 0), "night")];
        let w = week_of(&night, at(4, 8, 0), TZ);
        assert_eq!(w.days[2].ranges, "23–24");
        assert_eq!(w.days[3].ranges, "0–1");
        // One that ends exactly at midnight stays off the next day.
        let to_midnight = vec![event("m", "iphone", at(4, 22, 0), at(5, 0, 0), "m")];
        assert!(
            week_of(&to_midnight, at(4, 8, 0), TZ).days[3]
                .dots
                .is_empty()
        );
        // A free event is a dot but not busy time.
        let mut free = event("f", "iphone", at(4, 9, 0), at(4, 10, 0), "free");
        free.free = true;
        let w = week_of(&[free], at(4, 8, 0), TZ);
        assert_eq!((w.days[2].dots.len(), w.days[2].ranges.as_str()), (1, ""));
    }

    #[test]
    fn the_deadline_says_when_in_plain_words() {
        let now = at(4, 12, 0);
        assert_eq!(deadline_line(at(4, 11, 0), now, TZ), "Now");
        assert_eq!(deadline_line(at(4, 15, 0), now, TZ), "Today 15:00");
        assert_eq!(deadline_line(at(5, 9, 30), now, TZ), "Tomorrow 09:30");
        assert_eq!(deadline_line(at(6, 9, 30), now, TZ), "Fri 09:30");
        assert_eq!(deadline_line(at(10, 9, 30), now, TZ), "Tue 09:30");
        assert_eq!(deadline_line(at(17, 9, 30), now, TZ), "Tue 17 Nov 09:30");
    }

    #[test]
    fn the_deadline_is_the_next_event_that_has_not_ended() {
        let now = at(4, 12, 0);
        let events = vec![
            event("past", "iphone", at(4, 9, 0), at(4, 10, 0), "Over"),
            event(
                "later",
                "iphone",
                at(4, 15, 0),
                at(4, 16, 0),
                "  Later   one ",
            ),
            event(
                "soon",
                "lucina",
                at(4, 12, 40),
                at(4, 13, 0),
                "A very long title that will be cut because nobody reads it",
            ),
            all_day("day", "2026-11-04", "2026-11-05"),
        ];
        let d = deadline_of(&events, now, TZ).unwrap();
        assert_eq!(d.fact, "A very long title that will be cut becau…");
        assert_eq!(d.line, "Today 12:40");
        assert!(d.soon);
        let d = deadline_of(&events[..2], now, TZ).unwrap();
        assert_eq!(
            (d.fact.as_str(), d.line.as_str(), d.soon),
            ("Later one", "Today 15:00", false)
        );
        // One under way is "Now" and soon; none at all (or only all-day ones) is no card.
        let on = vec![event("on", "iphone", at(4, 11, 30), at(4, 12, 30), "On")];
        let d = deadline_of(&on, now, TZ).unwrap();
        assert_eq!((d.line.as_str(), d.soon), ("Now", true));
        assert_eq!(deadline_of(&[], now, TZ), None);
        assert_eq!(deadline_of(&events[3..], now, TZ), None);
    }

    #[test]
    fn next_start_is_only_for_events_still_to_come() {
        let now = at(4, 12, 0);
        let events = vec![
            event("on", "iphone", at(4, 11, 0), at(4, 13, 0), "On"),
            event("next", "iphone", at(4, 15, 0), at(4, 16, 0), "Next"),
        ];
        let t = next_start_of(&events, now).unwrap();
        assert_eq!(
            t.duration_since(UNIX_EPOCH).unwrap().as_millis() as i64,
            at(4, 15, 0)
        );
        assert_eq!(next_start_of(&events[..1], now), None);
    }

    fn decision(id: &str, status: &str, question: &str, urgency: &str, created: &str) -> Decision {
        Decision {
            id: id.into(),
            status: status.into(),
            question: question.into(),
            urgency: urgency.into(),
            created_at: created.into(),
            ..Decision::default()
        }
    }

    #[test]
    fn decisions_become_notes_newest_first() {
        let mut all = vec![
            decision(
                "dc-1",
                "pending",
                "Old question",
                "low",
                "2026-11-01T10:00:00Z",
            ),
            decision(
                "dc-2",
                "decided",
                "Answered",
                "high",
                "2026-11-04T10:00:00Z",
            ),
            decision(
                "dc-3",
                "pending",
                "Ship the\n   kernel patch with the long, long, long name?",
                "high",
                "2026-11-03T10:00:00Z",
            ),
        ];
        for i in 4..9 {
            all.push(decision(
                &format!("dc-{i}"),
                "pending",
                "Filler",
                "medium",
                &format!("2026-10-0{i}T10:00:00Z"),
            ));
        }
        let (pending, notes) = decision_notes(&all);
        assert_eq!(pending, 7);
        assert_eq!(notes.len(), 5);
        assert_eq!(notes[0].id, "decision:dc-3");
        assert_eq!(notes[0].fact, "Ship the kernel patch with the long, lon…");
        assert_eq!(
            (
                notes[0].line.as_str(),
                notes[0].urgent,
                notes[0].app.as_deref()
            ),
            ("Lucina asks", true, Some("decisions"))
        );
        assert_eq!(notes[1].id, "decision:dc-1");
        assert!(!notes[1].urgent);
        assert_eq!(decision_notes(&[]), (0, vec![]));
    }

    fn job(kind: &str, state: &str, created: &str) -> BoardJob {
        BoardJob {
            job_id: created.into(),
            kind: kind.into(),
            state: state.into(),
            created_at: created.into(),
            ..BoardJob::default()
        }
    }

    #[test]
    fn the_board_becomes_the_build_card() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-04T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let board = PipelineBoard {
            queue: vec![
                job("merge", "queued", "2026-11-04T10:05:00Z"),
                job("merge", "running", "2026-11-04T10:00:00Z"),
            ],
            recent_jobs: vec![job("deploy", "succeeded", "2026-11-04T09:00:00Z")],
            ..PipelineBoard::default()
        };
        let b = build_of(&board, now).unwrap();
        assert_eq!(
            (b.title.as_str(), b.phase, b.phases, b.percent, b.live),
            ("Build", 2, 3, 33, true)
        );
        assert_eq!(b.app.as_deref(), Some("agents"));
        // The count is of jobs: one of three finished, so the running one is the second.
        assert_eq!(b.line, "Job 2 of 3");
        // A job from yesterday is not part of today's count.
        let mut old = board.clone();
        old.recent_jobs
            .push(job("merge", "succeeded", "2026-11-03T08:00:00Z"));
        assert_eq!(build_of(&old, now).unwrap().line, "Job 2 of 3");
        // The newest running job wins, and another kind names itself.
        let board = PipelineBoard {
            queue: vec![
                job("merge", "running", "2026-11-04T10:00:00Z"),
                job("deploy", "running", "2026-11-04T10:10:00Z"),
            ],
            ..PipelineBoard::default()
        };
        assert_eq!(build_of(&board, now).unwrap().title, "Deploy");
        // Nothing running (queued and finished jobs do not count): no card.
        let idle = PipelineBoard {
            queue: vec![job("merge", "queued", "2026-11-04T10:00:00Z")],
            ..PipelineBoard::default()
        };
        assert_eq!(build_of(&idle, now), None);
        assert_eq!(build_of(&PipelineBoard::default(), now), None);
    }

    #[test]
    fn running_agents_skip_archived_and_internal_ones() {
        let items = json!({"agents": [
            {"id": "a", "status": "running"},
            {"id": "b", "status": "idle"},
            {"id": "c", "status": "running", "archivedAt": "2026-11-01T00:00:00Z"},
            {"id": "d", "status": "running", "labels": {"lucina.internal": "1"}},
            {"id": "e", "status": "running", "labels": {}, "archivedAt": null},
        ]});
        assert_eq!(agents_running(&items), 2);
        assert_eq!(agents_running(&json!({})), 0);
    }

    #[test]
    fn the_top_tasks_come_in_order() {
        let t = |id: &str, c: &str, date: &str, time: Option<&str>, rel: i64| json!({"id": id, "content": c, "due": {"date": date, "time": time, "rel_days": rel}});
        let state = json!({"view": {"list": {"groups": [
            {"kind": "overdue", "tasks": [t("a", "Oldest", "2026-11-01", None, -8), t("b", "Yesterday", "2026-11-08", None, -1), t("c", "Last week", "2026-11-03", None, -6)]},
            {"kind": "date", "tasks": [t("d", "Untimed", "2026-11-09", None, 0), t("e", "At nine", "2026-11-09", Some("09:00"), 0), t("f", "At eight", "2026-11-09", Some("08:00"), 0)]}
        ]}}});
        let (today, overdue) = task_tops(&state);
        assert_eq!(
            today.iter().map(|x| x.1.as_str()).collect::<Vec<_>>(),
            ["At eight", "At nine", "Untimed"]
        );
        assert_eq!(
            overdue.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
        assert_eq!(task_tops(&json!({"ok": true})), (vec![], vec![]));
    }

    #[test]
    fn task_counts_split_overdue_from_today() {
        let state = json!({"ok": true, "view": {"counts": {"inbox": 3, "today": 5, "overdue": 2, "upcoming": 9}}});
        assert_eq!(task_counts(&state), Some((3, 2)));
        assert_eq!(task_counts(&json!({"ok": false})), None);
    }

    fn hub_event(
        seq: u64,
        kind: &str,
        app: Option<&str>,
        subject: Option<&str>,
        fields: Value,
    ) -> HubEvent {
        serde_json::from_value(json!({
            "seq": seq, "src": "app", "src_seq": seq, "t_ms": 0, "kind": kind, "app": app, "subject": subject,
            "fields": fields,
        }))
        .unwrap()
    }

    #[test]
    fn crashes_and_notices_become_notes() {
        let crash = hub_event(
            7,
            "app.crashed",
            Some("calendar"),
            Some("app:calendar"),
            json!({"app": "calendar", "what": "exit 139"}),
        );
        let s = seen_of(&crash, at(4, 14, 5)).unwrap();
        assert_eq!(
            (s.fact.as_str(), s.urgent, s.app.as_deref()),
            ("Calendar stopped", true, Some("calendar"))
        );
        let by_subject = hub_event(8, "app.crashed", None, Some("app:video"), json!({}));
        assert_eq!(seen_of(&by_subject, 0).unwrap().fact, "Video stopped");
        let notice = hub_event(
            9,
            "notice.shown",
            None,
            None,
            json!({"text": "Battery low"}),
        );
        let n = seen_of(&notice, at(4, 14, 6)).unwrap();
        assert_eq!(
            (n.fact.as_str(), n.urgent, n.app),
            ("Battery low", false, None)
        );
        let download = hub_event(
            10,
            "download.done",
            Some("drive"),
            None,
            json!({"name": "Report.pdf"}),
        );
        assert_eq!(seen_of(&download, 0).unwrap().fact, "Report.pdf");
        // No words, no card; other events are not notes.
        assert_eq!(
            seen_of(&hub_event(11, "notice.shown", None, None, json!({})), 0),
            None
        );
        assert_eq!(
            seen_of(
                &hub_event(12, "app.opened", Some("video"), None, json!({})),
                0
            ),
            None
        );
        // Newest first, young ones "Just now", older ones the time; at most five.
        let all: Vec<Seen> = (1..=7)
            .map(|i| Seen {
                seq: i,
                at_ms: at(4, 14, 5),
                fact: format!("n{i}"),
                urgent: false,
                app: None,
            })
            .collect();
        let notes = event_notes(&all, at(4, 14, 5) + 30_000, TZ);
        assert_eq!(notes.len(), 5);
        assert_eq!(
            (notes[0].id.as_str(), notes[0].line.as_str()),
            ("event:7", "Just now")
        );
        assert_eq!(event_notes(&all, at(4, 14, 30), TZ)[0].line, "14:05");
    }

    #[test]
    fn media_events_set_the_player_card() {
        let mut t = MediaTracker::default();
        let started = hub_event(
            1,
            "media.started",
            Some("video"),
            Some("item:42"),
            json!({"title": "Dune"}),
        );
        assert_eq!(
            t.apply(&started),
            Some(Media {
                title: "Dune".into(),
                playing: true,
                app: "video".into()
            })
        );
        // `media.stopped` carries no title: the one remembered for the subject is used.
        let stopped = hub_event(
            2,
            "media.stopped",
            Some("video"),
            Some("item:42"),
            json!({"reason": "left"}),
        );
        assert_eq!(
            t.apply(&stopped),
            Some(Media {
                title: "Dune".into(),
                playing: false,
                app: "video".into()
            })
        );
        // An unknown title is no card; the app comes from the subject's prefix when the hub gave none.
        let other = hub_event(3, "media.stopped", Some("video"), Some("item:7"), json!({}));
        assert_eq!(t.apply(&other), None);
        let ended = hub_event(
            4,
            "media.ended",
            None,
            Some("file:/data/a.mp4"),
            json!({"title": "a"}),
        );
        assert_eq!(
            t.apply(&ended).map(|m| (m.app, m.playing)),
            Some(("video".into(), false))
        );
        assert_eq!(
            t.apply(&hub_event(5, "app.crashed", None, None, json!({}))),
            None
        );
    }

    #[test]
    fn uploads_count_only_the_recordings_going_out() {
        let dir = std::env::temp_dir().join(format!("home-feed-uploads-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, v: Value| std::fs::write(dir.join(name), v.to_string()).unwrap();
        assert_eq!(pending_uploads(&dir, SystemTime::now()), None);
        write(
            "a.json",
            json!({"client_id": "a", "size_bytes": 1000, "uploaded": 250, "done": false, "error": null}),
        );
        write(
            "b.json",
            json!({"client_id": "b", "size_bytes": 1000, "uploaded": 750, "done": false}),
        );
        write(
            "done.json",
            json!({"client_id": "d", "size_bytes": 1000, "uploaded": 1000, "done": true}),
        );
        write(
            "failed.json",
            json!({"client_id": "f", "size_bytes": 1000, "uploaded": 0, "done": false, "error": "No connection"}),
        );
        write("other.json", json!({"something": "else"}));
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let u = pending_uploads(&dir, SystemTime::now()).unwrap();
        assert_eq!(u.label, "Uploading 2");
        assert_eq!(u.fraction, Some(0.5));
        // Files nobody has written to for a while are not an upload in progress.
        assert_eq!(
            pending_uploads(&dir, SystemTime::now() + Duration::from_secs(3600)),
            None
        );
        assert_eq!(
            pending_uploads(&dir.join("missing"), SystemTime::now()),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
