//! What the app needs from the Secretary server: the routes `web/src/api/tasks.ts` uses
//! (`GET /v1/tasks/state`, `POST /v1/tasks/command`, `POST /v1/tasks/sync`), behind a trait, so that
//! the app runs against the real account and against [`Fake`] alike.
//!
//! [`Fake`] is the secretary-server route without the HTTP: the same engine, the same JSON request,
//! the same status for each refusal (`status_for` in secretary-server's tasks.rs).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use secretary_tasks::{Command, Engine, Request, Response, View, ViewQuery};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    /// No answer, or 502/503/504: the command is kept and sent again (`offline()` in
    /// web/src/state/tasks.ts).
    Offline(String),
    /// 401: the device token is revoked.
    SignedOut(String),
    /// The server looked at it and said no.
    Refused {
        status: u16,
        code: String,
        message: String,
    },
}

pub trait Server: Send {
    /// `GET /v1/tasks/state`.
    fn state(&mut self, q: &ViewQuery, tz: &str) -> Result<View, ServerError>;
    /// `POST /v1/tasks/command`.
    fn command(
        &mut self,
        op_id: &str,
        tz: &str,
        command: &Command,
        view: &ViewQuery,
    ) -> Result<Response, ServerError>;
    /// `POST /v1/tasks/sync`.
    fn sync(&mut self) -> Result<(), ServerError>;
}

fn refused(r: Response) -> ServerError {
    let code = r.code.unwrap_or_else(|| "error".into());
    let status = match code.as_str() {
        "conflict" => 409,
        "storage_error" => 500,
        _ => 422,
    };
    ServerError::Refused {
        status,
        code,
        message: r.error.unwrap_or_default(),
    }
}

/// The server's engine in this process: the sample data on the device, and the tests.
pub struct Fake {
    pub engine: Engine,
    /// While set, every call fails as a lost connection does.
    pub offline: Arc<AtomicBool>,
    /// Commands that reached the engine, replays included.
    pub seen: Arc<AtomicU64>,
    /// A fixed time for tests; 0 is the real clock.
    pub now_ms: u64,
}

impl Fake {
    pub fn new(engine: Engine) -> Fake {
        Fake {
            engine,
            offline: Arc::new(AtomicBool::new(false)),
            seen: Arc::new(AtomicU64::new(0)),
            now_ms: 0,
        }
    }

    fn now(&self) -> u64 {
        if self.now_ms == 0 {
            secretary_tasks::now_ms()
        } else {
            self.now_ms
        }
    }

    fn check_online(&self) -> Result<(), ServerError> {
        if self.offline.load(Ordering::SeqCst) {
            Err(ServerError::Offline("the fake server is offline".into()))
        } else {
            Ok(())
        }
    }

    fn run(&mut self, request: &Request) -> Result<Response, ServerError> {
        let json = serde_json::to_string(request).map_err(|e| ServerError::Refused {
            status: 422,
            code: "invalid_command".into(),
            message: e.to_string(),
        })?;
        let now = self.now();
        let r = self.engine.handle_at(&json, now);
        if r.ok { Ok(r) } else { Err(refused(r)) }
    }
}

impl Server for Fake {
    fn state(&mut self, q: &ViewQuery, tz: &str) -> Result<View, ServerError> {
        self.check_online()?;
        let r = self.run(&Request {
            op_id: None,
            expected_revision: None,
            timezone: tz.into(),
            command: Command::Snapshot {
                view: Some(q.clone()),
            },
            view: None,
        })?;
        r.view.ok_or_else(|| ServerError::Refused {
            status: 500,
            code: "no_view".into(),
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
        self.check_online()?;
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.run(&Request {
            op_id: Some(op_id.into()),
            expected_revision: None,
            timezone: tz.into(),
            command: command.clone(),
            view: Some(view.clone()),
        })
    }

    fn sync(&mut self) -> Result<(), ServerError> {
        self.check_online()?;
        Err(ServerError::Refused {
            status: 409,
            code: "todoist_not_connected".into(),
            message: "Todoist isn't connected on this sample".into(),
        })
    }
}
