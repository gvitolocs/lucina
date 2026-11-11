//! The real server, over `lucina-account`: the routes of web/src/api/tasks.ts.

use lucina_account::{ApiError, Client, system};
use secretary_tasks::{Command, Response, View, ViewQuery};
use serde_json::json;

use crate::server::{Server, ServerError};

pub struct Account {
    pub client: Client,
}

/// `offline()` in web/src/state/tasks.ts, and a 401 forgets the token, as every Juno app does.
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

/// `GET /v1/tasks/state`'s query.
pub fn state_path(q: &ViewQuery, tz: &str) -> String {
    let mut p = format!("/v1/tasks/state?timezone={}", encode(tz));
    if let Some(v) = &q.kind {
        p += &format!("&view={}", encode(v));
    }
    if let Some(v) = &q.id {
        p += &format!("&id={}", encode(v));
    }
    if let Some(v) = &q.query {
        p += &format!("&query={}", encode(v));
    }
    if let Some(v) = q.days {
        p += &format!("&days={v}");
    }
    p
}

fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, ServerError> {
    serde_json::from_value(v).map_err(|e| ServerError::Refused {
        status: 0,
        code: "protocol".into(),
        message: format!("Unexpected answer: {e}"),
    })
}

impl Server for Account {
    fn state(&mut self, q: &ViewQuery, tz: &str) -> Result<View, ServerError> {
        let v = self.client.get_json(&state_path(q, tz)).map_err(error)?;
        let r: Response = parse(v)?;
        r.view.ok_or_else(|| ServerError::Refused {
            status: 0,
            code: "protocol".into(),
            message: "the answer had no view".into(),
        })
    }

    fn command(
        &mut self,
        op_id: &str,
        tz: &str,
        command: &Command,
        view: &ViewQuery,
    ) -> Result<Response, ServerError> {
        let body = json!({"op_id": op_id, "timezone": tz, "command": command, "view": view});
        parse(
            self.client
                .post_json("/v1/tasks/command", &body)
                .map_err(error)?,
        )
    }

    fn sync(&mut self) -> Result<(), ServerError> {
        self.client
            .post_json("/v1/tasks/sync", &json!({}))
            .map(|_| ())
            .map_err(error)
    }
}
