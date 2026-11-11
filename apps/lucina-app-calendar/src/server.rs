//! What the app needs from the Secretary server: the routes `web/src/api/calendar.ts` uses, behind a
//! trait. [`Fake`] behaves as secretary-server's calendar.rs does where it matters here: a new event
//! lands in Lucina's own calendar, `iphone:` and `ics:` events are refused with `read_only_source`,
//! and the events come out of the engine's own expansion.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use secretary_calendar::view::SourceItems;
use secretary_calendar::{Event, Item, Moment, Source, Window, window};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// No answer. A create may or may not have arrived, so nothing is ever sent again by itself.
    Offline(String),
    SignedOut(String),
    Refused {
        status: u16,
        code: String,
        message: String,
    },
}

impl ServerError {
    pub fn text(&self) -> String {
        match self {
            ServerError::Offline(m) => format!("No connection: {m}"),
            ServerError::SignedOut(m) => format!("Signed out: {m}"),
            ServerError::Refused { message, code, .. } if message.is_empty() => code.clone(),
            ServerError::Refused { message, .. } => message.clone(),
        }
    }
}

/// `EventDraft` in web/src/api/calendar.ts: times as UTC instants, all-day as dates with the
/// exclusive end.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Draft {
    pub title: String,
    pub start: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<String>,
    pub all_day: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conference_url: Option<String>,
}

pub trait Server: Send {
    /// `GET /v1/calendar/events?from&to`.
    fn events(&mut self, from: DateTime<Utc>, to: DateTime<Utc>)
    -> Result<Vec<Event>, ServerError>;
    /// `POST /v1/calendar/events`, with no `target`: Lucina's own calendar.
    fn create(&mut self, d: &Draft) -> Result<Option<Event>, ServerError>;
    /// `PATCH /v1/calendar/events/{id}` with `confirm: true`: pressing Save is the confirmation.
    fn update(&mut self, id: &str, d: &Draft) -> Result<Option<Event>, ServerError>;
    /// `DELETE /v1/calendar/events/{id}?confirm=true`.
    fn delete(&mut self, id: &str) -> Result<(), ServerError>;
}

fn moment(s: &str) -> Option<Moment> {
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Moment::Date(d));
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| Moment::Utc(t.with_timezone(&Utc)))
}

fn bad(code: &str, message: &str, status: u16) -> ServerError {
    ServerError::Refused {
        status,
        code: code.into(),
        message: message.into(),
    }
}

/// The server's calendars in this process: the sample on the device, and the tests.
pub struct Fake {
    pub tz: Tz,
    /// Lucina's own calendar: the only one that can be written here.
    pub lucina: Vec<Item>,
    /// The EventKit snapshot from the iPhone: read-only on the server.
    pub iphone: Vec<Item>,
    pub offline: Arc<AtomicBool>,
    /// Writes that reached the server, refused or not.
    pub writes: Arc<AtomicU64>,
    pub next: u64,
}

impl Fake {
    pub fn new(tz: Tz, lucina: Vec<Item>, iphone: Vec<Item>) -> Fake {
        Fake {
            tz,
            lucina,
            iphone,
            offline: Arc::new(AtomicBool::new(false)),
            writes: Arc::new(AtomicU64::new(0)),
            next: 1,
        }
    }

    fn check(&self) -> Result<(), ServerError> {
        if self.offline.load(Ordering::SeqCst) {
            Err(ServerError::Offline("the fake server is offline".into()))
        } else {
            Ok(())
        }
    }

    fn event_of(&self, uid: &str) -> Option<Event> {
        let item = self.lucina.iter().find(|i| i.uid == uid)?;
        let start = item.start.as_ref()?.instant(self.tz);
        let w = Window {
            from: start - chrono::Duration::days(1),
            to: start + chrono::Duration::days(1),
        };
        let src = [SourceItems {
            source: Source::Lucina,
            key: "",
            label: Some("Lucina"),
            items: std::slice::from_ref(item),
        }];
        window(&src, w, self.tz).into_iter().next()
    }

    fn fill(item: &mut Item, d: &Draft) -> Result<(), ServerError> {
        if d.title.trim().is_empty() {
            return Err(bad("invalid", "title is required", 422));
        }
        let start = moment(&d.start).ok_or_else(|| bad("invalid", "start: bad time", 422))?;
        let end = match &d.end {
            Some(e) => Some(moment(e).ok_or_else(|| bad("invalid", "end: bad time", 422))?),
            None => None,
        };
        item.title = d.title.clone();
        item.start = Some(start);
        item.end = end;
        if let Some(l) = &d.location {
            item.location = Some(l.clone()).filter(|x| !x.is_empty());
        }
        if let Some(n) = &d.description {
            item.description = Some(n.clone()).filter(|x| !x.is_empty());
        }
        if let Some(v) = &d.conference_url {
            item.conference_url = Some(v.clone()).filter(|x| !x.is_empty());
        }
        Ok(())
    }

    /// `target()` in secretary-server's calendar.rs.
    fn own<'a>(id: &'a str) -> Result<&'a str, ServerError> {
        match id.split_once(':') {
            Some(("lucina", o)) if !o.is_empty() => Ok(o),
            Some(("ics" | "iphone", _)) => Err(bad(
                "read_only_source",
                "only Google Calendar events can be changed here (ICS feeds are read-only; iPhone events change on the phone)",
                422,
            )),
            _ => Err(bad("event_not_found", "no such event", 404)),
        }
    }
}

impl Server for Fake {
    fn events(
        &mut self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, ServerError> {
        self.check()?;
        let sources = [
            SourceItems {
                source: Source::Lucina,
                key: "",
                label: Some("Lucina"),
                items: &self.lucina,
            },
            SourceItems {
                source: Source::Iphone,
                key: "",
                label: Some("iPhone"),
                items: &self.iphone,
            },
        ];
        Ok(window(&sources, Window { from, to }, self.tz))
    }

    fn create(&mut self, d: &Draft) -> Result<Option<Event>, ServerError> {
        self.check()?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        let uid = format!("ev_{}", self.next);
        self.next += 1;
        let mut item = Item {
            uid: uid.clone(),
            added_by: Some("Juno".into()),
            ..Item::default()
        };
        Self::fill(&mut item, d)?;
        self.lucina.push(item);
        Ok(self.event_of(&uid))
    }

    fn update(&mut self, id: &str, d: &Draft) -> Result<Option<Event>, ServerError> {
        self.check()?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        let own = Self::own(id)?.to_owned();
        let item = self
            .lucina
            .iter_mut()
            .find(|i| i.uid == own)
            .ok_or_else(|| bad("event_not_found", "no such event", 404))?;
        Self::fill(item, d)?;
        Ok(self.event_of(&own))
    }

    fn delete(&mut self, id: &str) -> Result<(), ServerError> {
        self.check()?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        let own = Self::own(id)?.to_owned();
        self.lucina.retain(|i| i.uid != own);
        Ok(())
    }
}
