//! The routes of `/v1/media` behind a trait, so that sample mode and the tests run the same code as the real thing.

use std::io::Write;
use std::sync::Arc;

use lucina_account::{ApiError, Client};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaError {
    /// No answer, or 502/503/504. The request may or may not have arrived.
    Offline(String),
    /// 401: the device token is revoked.
    SignedOut,
    /// 429: wait this many seconds.
    RateLimited(u64),
    /// The server looked at it and said no.
    Refused { status: u16, code: String, message: String },
}

impl MediaError {
    pub fn status(&self) -> Option<u16> {
        match self {
            MediaError::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// What a person reads.
    pub fn text(&self) -> String {
        match self {
            MediaError::Offline(m) => format!("Can't reach the server: {m}"),
            MediaError::SignedOut => "Signed out".into(),
            MediaError::RateLimited(s) => format!("Too many requests; wait {s} s"),
            MediaError::Refused { status, message, .. } if message.is_empty() => format!("The server said no ({status})"),
            MediaError::Refused { message, .. } => message.clone(),
        }
    }
}

impl From<ApiError> for MediaError {
    fn from(e: ApiError) -> MediaError {
        match e {
            ApiError::Network(m) => MediaError::Offline(m),
            ApiError::RateLimited(s) => MediaError::RateLimited(s),
            ApiError::Http(502..=504, _, m) => MediaError::Offline(m),
            ApiError::Unauthorized(_) => MediaError::SignedOut,
            ApiError::Http(status, code, message) => MediaError::Refused { status, code, message },
            ApiError::Protocol(message) => MediaError::Refused { status: 0, code: "protocol".into(), message },
        }
    }
}

/// One way to reach the media server. Blocking: call it on a thread.
pub trait Media: Send {
    /// `GET`, the body as JSON.
    fn get(&mut self, path: &str) -> Result<Value, MediaError>;
    /// `GET` a small file, up to `limit` bytes.
    fn bytes(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, MediaError>;
    /// `POST`, `PATCH` or `DELETE`, with a JSON body.
    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, MediaError>;
    /// `GET` a file of any size into `out`, a piece at a time. `progress` gets the bytes so far, and ends it by answering false.
    fn download(&mut self, path: &str, out: &mut dyn Write, progress: &mut dyn FnMut(u64) -> bool) -> Result<u64, MediaError>;
}

/// Makes a [`Media`] for a thread. `None` when this machine is not signed in.
pub type Connect = Arc<dyn Fn() -> Option<Box<dyn Media>> + Send + Sync>;

/// The real server, over `lucina-account`.
pub struct Account {
    pub client: Client,
}

impl Media for Account {
    fn get(&mut self, path: &str) -> Result<Value, MediaError> {
        Ok(self.client.get_json(path)?)
    }

    fn bytes(&mut self, path: &str, limit: u64) -> Result<Vec<u8>, MediaError> {
        Ok(self.client.get_bytes(path, limit)?)
    }

    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, MediaError> {
        Ok(match method {
            "POST" => self.client.post_json(path, body)?,
            m => self.client.send_json(m, path, body)?,
        })
    }

    fn download(&mut self, path: &str, out: &mut dyn Write, progress: &mut dyn FnMut(u64) -> bool) -> Result<u64, MediaError> {
        Ok(self.client.download(path, out, progress)?)
    }
}

/// The machine's sign-in: a [`Connect`] for the running system.
pub fn system_connect() -> Connect {
    Arc::new(|| lucina_account::system::client().map(|client| Box::new(Account { client }) as Box<dyn Media>))
}

/// A [`Connect`] to a server with no sign-in, on this machine: the real engine in the tests.
pub fn local_connect(base: &str) -> Connect {
    let base = base.to_owned();
    Arc::new(move || Client::new(&base, None).map(|client| Box::new(Account { client }) as Box<dyn Media>))
}
