//! The one thread that talks to the server, so that the screen never waits for the network.
//!
//! Commands run in the order they were made. One that cannot reach the server stays at the head and
//! is tried again, with those behind it (`flush` in web/src/state/tasks.ts); op_ids make a repeat
//! harmless. When several loads wait, only the newest runs.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use secretary_tasks::{Response, View, ViewQuery};

use crate::server::{Server, ServerError};
use crate::store::Queued;

pub const POLL: Duration = Duration::from_secs(60);
pub const RETRY: Duration = Duration::from_secs(20);

pub enum Job {
    Load(ViewQuery),
    /// A list worth having ready; dropped when anything more urgent waits.
    Prefetch(ViewQuery),
    Command(Queued, ViewQuery),
    Sync,
    /// The clock: retry what waits, and refresh the list on screen.
    Tick,
}

#[derive(Debug)]
pub enum Msg {
    Loaded(ViewQuery, Box<View>),
    Acked(String, ViewQuery, Box<Response>),
    Refused(String, String),
    /// The connection is gone; what waits stays queued.
    Offline(String),
    Online,
    SignedOut(String),
    Synced(Result<(), String>),
    /// The sign-in screen has kept a token.
    SignedIn,
    /// The sign-in screen has something new to draw.
    Changed,
}

pub struct Worker {
    pub server: Box<dyn Server>,
    pub tz: String,
    /// What the next tick refreshes.
    pub current: ViewQuery,
    waiting: VecDeque<(Queued, ViewQuery)>,
    offline: bool,
}

impl Worker {
    pub fn new(server: Box<dyn Server>, tz: String, current: ViewQuery) -> Worker {
        Worker {
            server,
            tz,
            current,
            waiting: VecDeque::new(),
            offline: false,
        }
    }

    #[cfg(test)]
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    fn went_offline(&mut self, m: String, out: &mut Vec<Msg>) {
        if !self.offline {
            self.offline = true;
            out.push(Msg::Offline(m));
        }
    }

    fn came_back(&mut self, out: &mut Vec<Msg>) {
        if self.offline {
            self.offline = false;
            out.push(Msg::Online);
        }
    }

    fn flush(&mut self, out: &mut Vec<Msg>) {
        while let Some((q, view)) = self.waiting.pop_front() {
            match self.server.command(&q.op_id, &self.tz, &q.command, &view) {
                Ok(r) => {
                    self.came_back(out);
                    out.push(Msg::Acked(q.op_id, view, Box::new(r)));
                }
                Err(ServerError::Offline(m)) => {
                    self.waiting.push_front((q, view));
                    self.went_offline(m, out);
                    return;
                }
                Err(ServerError::SignedOut(m)) => {
                    self.waiting.push_front((q, view));
                    out.push(Msg::SignedOut(m));
                    return;
                }
                Err(ServerError::Refused { message, code, .. }) => {
                    self.came_back(out);
                    let text = if message.is_empty() {
                        format!("The server refused this ({code}).")
                    } else {
                        message
                    };
                    out.push(Msg::Refused(q.op_id, text));
                }
            }
        }
    }

    fn load(&mut self, q: ViewQuery, out: &mut Vec<Msg>) {
        match self.server.state(&q, &self.tz) {
            Ok(v) => {
                self.came_back(out);
                out.push(Msg::Loaded(q, Box::new(v)));
            }
            Err(ServerError::Offline(m)) => self.went_offline(m, out),
            Err(ServerError::SignedOut(m)) => out.push(Msg::SignedOut(m)),
            Err(ServerError::Refused { message, .. }) => {
                out.push(Msg::Refused(String::new(), message));
            }
        }
    }

    /// One job: what the app is told afterwards.
    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        match job {
            Job::Load(q) => {
                self.current = q.clone();
                self.load(q, &mut out);
            }
            Job::Prefetch(q) => self.load(q, &mut out),
            Job::Command(q, view) => {
                self.current = view.clone();
                self.waiting.push_back((q, view));
                // While offline the head has just failed: wait for the tick.
                if !self.offline {
                    self.flush(&mut out);
                }
            }
            Job::Sync => {
                let r = self.server.sync();
                out.push(Msg::Synced(r.map_err(|e| match e {
                    ServerError::Offline(m) | ServerError::SignedOut(m) => m,
                    ServerError::Refused { message, .. } => message,
                })));
                let q = self.current.clone();
                self.load(q, &mut out);
            }
            Job::Tick => {
                self.flush(&mut out);
                if self.waiting.is_empty() {
                    let q = self.current.clone();
                    self.load(q, &mut out);
                }
            }
        }
        out
    }

    /// The thread's loop: ends when the app does.
    pub fn run(mut self, jobs: Receiver<Job>, send: impl Fn(Msg)) {
        let mut queue: VecDeque<Job> = VecDeque::new();
        loop {
            if queue.is_empty() {
                let wait = if self.offline || !self.waiting.is_empty() {
                    RETRY
                } else {
                    POLL
                };
                match jobs.recv_timeout(wait) {
                    Ok(j) => queue.push_back(j),
                    Err(RecvTimeoutError::Timeout) => queue.push_back(Job::Tick),
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            queue.extend(jobs.try_iter());
            let job = next_job(&mut queue);
            if let Some(job) = job {
                for m in self.handle(job) {
                    send(m);
                }
            }
        }
    }
}

/// The next job worth doing: a load overtaken by a newer load is dropped (typing a search, walking
/// through lists), and a prefetch waits for everything else.
pub fn next_job(queue: &mut VecDeque<Job>) -> Option<Job> {
    let newest_load = queue.iter().rposition(|j| matches!(j, Job::Load(_)));
    let mut i = 0;
    queue.retain(|j| {
        let keep = match j {
            Job::Load(_) => Some(i) == newest_load,
            _ => true,
        };
        i += 1;
        keep
    });
    // A prefetch waits until nothing else does.
    if let Some(pos) = queue.iter().position(|j| !matches!(j, Job::Prefetch(_))) {
        return queue.remove(pos);
    }
    queue.pop_front()
}
