//! The one thread that talks to the server. Unlike Tasks there is no outbox: the calendar's create has
//! no op_id, so a create whose answer was lost could be a duplicate if it were sent again. As in
//! web/src/views/Calendar.tsx, a write that fails comes back to the owner, who decides.

use std::collections::{BTreeSet, VecDeque};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use chrono::NaiveDate;
use chrono_tz::Tz;
use secretary_calendar::Event;

use crate::server::{Draft, Server, ServerError};
use crate::store::month_range;

/// `REFRESH_MS` in Calendar.tsx.
pub const REFRESH: Duration = Duration::from_secs(300);

pub enum Job {
    /// The months on screen; a later one replaces it.
    Visible(Vec<NaiveDate>),
    /// Months worth having ready.
    Prefetch(Vec<NaiveDate>),
    Create(u64, Draft),
    Update(u64, String, Draft),
    Delete(u64, String),
    Tick,
}

pub enum Msg {
    Month(NaiveDate, Vec<Event>),
    Created(u64, Option<Box<Event>>),
    Updated(u64, Option<Box<Event>>),
    Deleted(u64),
    /// The write didn't happen, or nobody knows: the reason for the owner.
    WriteFailed(u64, String),
    Offline(String),
    Online,
    SignedOut(String),
    /// The sign-in screen has kept a token.
    SignedIn,
    /// The sign-in screen has something new to draw.
    Changed,
}

pub struct Worker {
    pub server: Box<dyn Server>,
    pub tz: Tz,
    visible: Vec<NaiveDate>,
    offline: bool,
}

impl Worker {
    pub fn new(server: Box<dyn Server>, tz: Tz) -> Worker {
        Worker {
            server,
            tz,
            visible: Vec::new(),
            offline: false,
        }
    }

    fn load(&mut self, month: NaiveDate, out: &mut Vec<Msg>) -> bool {
        let (from, to) = month_range(month, self.tz);
        match self.server.events(from, to) {
            Ok(list) => {
                if self.offline {
                    self.offline = false;
                    out.push(Msg::Online);
                }
                out.push(Msg::Month(month, list));
                true
            }
            Err(ServerError::Offline(m)) => {
                if !self.offline {
                    self.offline = true;
                    out.push(Msg::Offline(m));
                }
                false
            }
            Err(ServerError::SignedOut(m)) => {
                out.push(Msg::SignedOut(m));
                false
            }
            Err(e) => {
                out.push(Msg::Offline(e.text()));
                false
            }
        }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let mut out = Vec::new();
        match job {
            Job::Visible(months) => {
                self.visible = months.clone();
                for m in months {
                    if !self.load(m, &mut out) {
                        break;
                    }
                }
            }
            Job::Prefetch(months) => {
                for m in months {
                    if !self.load(m, &mut out) {
                        break;
                    }
                }
            }
            Job::Tick => {
                for m in self.visible.clone() {
                    if !self.load(m, &mut out) {
                        break;
                    }
                }
            }
            Job::Create(op, d) => match self.server.create(&d) {
                Ok(e) => out.push(Msg::Created(op, e.map(Box::new))),
                Err(ServerError::SignedOut(m)) => out.push(Msg::SignedOut(m)),
                Err(e) => out.push(Msg::WriteFailed(op, e.text())),
            },
            Job::Update(op, id, d) => match self.server.update(&id, &d) {
                Ok(e) => out.push(Msg::Updated(op, e.map(Box::new))),
                Err(e) => out.push(Msg::WriteFailed(op, e.text())),
            },
            Job::Delete(op, id) => match self.server.delete(&id) {
                Ok(()) => out.push(Msg::Deleted(op)),
                Err(e) => out.push(Msg::WriteFailed(op, e.text())),
            },
        }
        out
    }

    pub fn run(mut self, jobs: Receiver<Job>, send: impl Fn(Msg)) {
        let mut queue: VecDeque<Job> = VecDeque::new();
        loop {
            if queue.is_empty() {
                match jobs.recv_timeout(REFRESH) {
                    Ok(j) => queue.push_back(j),
                    Err(RecvTimeoutError::Timeout) => queue.push_back(Job::Tick),
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            queue.extend(jobs.try_iter());
            if let Some(job) = next_job(&mut queue) {
                for m in self.handle(job) {
                    send(m);
                }
            }
        }
    }
}

/// Writes in order first; only the newest `Visible` matters, and a prefetch waits for all of it.
pub fn next_job(queue: &mut VecDeque<Job>) -> Option<Job> {
    let newest = queue.iter().rposition(|j| matches!(j, Job::Visible(_)));
    let mut i = 0;
    queue.retain(|j| {
        let keep = !matches!(j, Job::Visible(_)) || Some(i) == newest;
        i += 1;
        keep
    });
    if queue.iter().any(|j| !matches!(j, Job::Prefetch(_))) {
        let wanted: BTreeSet<NaiveDate> = queue
            .iter()
            .filter_map(|j| match j {
                Job::Visible(m) => Some(m.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect();
        for j in queue.iter_mut() {
            if let Job::Prefetch(m) = j {
                m.retain(|x| !wanted.contains(x));
            }
        }
        let pos = queue.iter().position(|j| !matches!(j, Job::Prefetch(_)))?;
        return queue.remove(pos);
    }
    queue.pop_front()
}
