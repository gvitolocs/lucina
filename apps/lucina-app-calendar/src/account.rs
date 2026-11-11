//! The real server, over `lucina-account`: the routes of web/src/api/calendar.ts.

use chrono::{DateTime, SecondsFormat, Utc};
use lucina_account::{ApiError, Client, system};
use secretary_calendar::Event;
use serde_json::{Value, json};

use crate::server::{Draft, Server, ServerError};

pub const CACHE_FILE: &str = "calendar-cache.json";

pub struct Account {
    pub client: Client,
}

fn error(e: ApiError) -> ServerError {
    match e {
        ApiError::Network(m) => ServerError::Offline(m),
        ApiError::RateLimited(s) => ServerError::Offline(format!("too many requests; wait {s} s")),
        ApiError::Http(502..=504, _, m) => ServerError::Offline(m),
        ApiError::Unauthorized(m) => {
            system::report_rejected();
            ServerError::SignedOut(m)
        }
        ApiError::Http(status, code, message) => ServerError::Refused {
            status,
            code,
            message,
        },
        ApiError::Protocol(m) => ServerError::Refused {
            status: 0,
            code: "protocol".into(),
            message: m,
        },
    }
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

fn protocol(m: String) -> ServerError {
    ServerError::Refused {
        status: 0,
        code: "protocol".into(),
        message: m,
    }
}

/// `{created|updated, event}`: `event` can be null, and then the next load brings it.
fn event_in(v: Value) -> Result<Option<Event>, ServerError> {
    match v.get("event") {
        None | Some(Value::Null) => Ok(None),
        Some(e) => serde_json::from_value(e.clone())
            .map(Some)
            .map_err(|e| protocol(format!("Unexpected event: {e}"))),
    }
}

/// `{from, to, timeZone, events, sources}`. One event the app can't read doesn't cost the others.
pub fn events_in(v: &Value) -> Result<Vec<Event>, ServerError> {
    let list = v
        .get("events")
        .and_then(Value::as_array)
        .ok_or_else(|| protocol("the answer had no events".into()))?;
    Ok(list
        .iter()
        .filter_map(|e| serde_json::from_value(e.clone()).ok())
        .collect())
}

impl Server for Account {
    fn events(
        &mut self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Event>, ServerError> {
        let at = |t: DateTime<Utc>| encode(&t.to_rfc3339_opts(SecondsFormat::Millis, true));
        let v = self
            .client
            .get_json(&format!(
                "/v1/calendar/events?from={}&to={}",
                at(from),
                at(to)
            ))
            .map_err(error)?;
        events_in(&v)
    }

    fn create(&mut self, d: &Draft) -> Result<Option<Event>, ServerError> {
        let body = serde_json::to_value(d).map_err(|e| protocol(e.to_string()))?;
        event_in(
            self.client
                .post_json("/v1/calendar/events", &body)
                .map_err(error)?,
        )
    }

    fn update(&mut self, id: &str, d: &Draft) -> Result<Option<Event>, ServerError> {
        let mut body = serde_json::to_value(d).map_err(|e| protocol(e.to_string()))?;
        body["confirm"] = json!(true);
        let path = format!("/v1/calendar/events/{}", encode(id));
        event_in(
            self.client
                .send_json("PATCH", &path, &body)
                .map_err(error)?,
        )
    }

    fn delete(&mut self, id: &str) -> Result<(), ServerError> {
        let path = format!("/v1/calendar/events/{}?confirm=true", encode(id));
        self.client
            .send_json("DELETE", &path, &json!({}))
            .map(|_| ())
            .map_err(error)
    }
}
