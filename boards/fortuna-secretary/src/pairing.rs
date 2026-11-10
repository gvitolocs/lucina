//! QR sign-in as Juno does it (`services/lucina-account/src/signin.rs`, after the web's `QrLogin.tsx`), without I/O:
//! ask for a code (`POST /v1/session/qr` as a device), show it as a QR code and six digits, poll every 2 s; an
//! expired code is replaced on its own up to five times, then a press starts again. The owner approves on a device
//! that is already signed in; nothing here can approve. The approving poll is the only answer that ever holds the
//! token, once.
//!
//! The kernel calls `tick` every frame, does the request it names, and hands the answer back.

use alloc::string::String;

use crate::api::{PollAnswer, QrStart};
use crate::rules::{self, ACT_PAUSE, ACT_POLL, ACT_START};

/// What failed, as far as the pairing cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fail {
    /// 429: try again after this many seconds.
    RateLimited(u32),
    /// No answer, or an answer that is not one.
    Other,
}

/// What the sign-in screen shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase<'a> {
    Loading,
    Ready { approve_url: &'a str, short_code: Option<&'a str>, seconds_left: u64 },
    Denied,
    /// Five codes went unanswered: a press asks again.
    Paused,
    Unavailable { rate_limited: bool, retry_s: u64 },
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Unavailable { rate_limited: bool },
    Denied,
    Paused,
    Done,
}

struct Live {
    start: QrStart,
    expires: u64,
}

/// One pairing. Times are milliseconds of a monotonic clock.
pub struct Pairing {
    state: State,
    live: Option<Live>,
    codes: u32,
    next: u64,
}

/// What the kernel should do now.
#[derive(Debug, PartialEq, Eq)]
pub enum Want<'a> {
    Nothing,
    /// `POST /v1/session/qr`.
    Start,
    /// `GET /v1/session/qr/poll?t=<poll token>`.
    Poll(&'a str),
    /// The screen changed (a code expired and paused).
    Changed,
}

impl Default for Pairing {
    fn default() -> Self {
        Pairing::new(0)
    }
}

impl Pairing {
    pub fn new(now: u64) -> Pairing {
        Pairing { state: State::Waiting, live: None, codes: 0, next: now }
    }

    fn active(&self) -> bool {
        matches!(self.state, State::Waiting | State::Unavailable { .. })
    }

    pub fn phase(&self, now: u64) -> Phase<'_> {
        match (self.state, &self.live) {
            (State::Denied, _) => Phase::Denied,
            (State::Paused, _) => Phase::Paused,
            (State::Done, _) => Phase::Done,
            (State::Unavailable { rate_limited }, _) => Phase::Unavailable { rate_limited, retry_s: self.next.saturating_sub(now) / 1000 },
            (State::Waiting, Some(l)) => Phase::Ready {
                approve_url: &l.start.approve_url,
                short_code: l.start.short_code.as_deref(),
                seconds_left: l.expires.saturating_sub(now) / 1000,
            },
            (State::Waiting, None) => Phase::Loading,
        }
    }

    pub fn tick(&mut self, now: u64) -> Want<'_> {
        let has = self.live.is_some();
        let expires = self.live.as_ref().map_or(0, |l| l.expires);
        match rules::pairing_action(self.active(), now, self.next, has, expires, self.codes) {
            ACT_POLL => {
                self.next = now + rules::POLL_MS;
                match &self.live {
                    Some(l) => Want::Poll(&l.start.poll_token),
                    None => Want::Nothing,
                }
            }
            ACT_START => {
                self.live = None;
                // One request at a time: no second start until this one is answered.
                self.next = u64::MAX;
                Want::Start
            }
            ACT_PAUSE => {
                self.live = None;
                self.state = State::Paused;
                Want::Changed
            }
            _ => Want::Nothing,
        }
    }

    pub fn on_started(&mut self, now: u64, r: Result<QrStart, Fail>) {
        match r {
            Ok(start) => {
                self.codes += 1;
                let expires = now + start.expires_in.min(3600) * 1000;
                self.live = Some(Live { start, expires });
                self.state = State::Waiting;
                self.next = now + rules::POLL_MS;
            }
            Err(Fail::RateLimited(s)) => {
                self.state = State::Unavailable { rate_limited: true };
                self.next = now + s.max(1) as u64 * 1000;
            }
            Err(Fail::Other) => {
                self.state = State::Unavailable { rate_limited: false };
                self.next = now + rules::RETRY_MS;
            }
        }
    }

    /// The poll's answer; the token when it was approved.
    pub fn on_polled(&mut self, now: u64, r: Result<PollAnswer, Fail>) -> Option<String> {
        match r {
            Ok(PollAnswer::Pending) => {
                if matches!(self.state, State::Unavailable { .. }) {
                    self.state = State::Waiting;
                }
                None
            }
            Ok(PollAnswer::Approved(token)) => {
                self.live = None;
                self.state = State::Done;
                Some(token)
            }
            Ok(PollAnswer::Denied) => {
                self.live = None;
                self.state = State::Denied;
                None
            }
            Ok(PollAnswer::Expired) => {
                self.live = None;
                self.next = now;
                None
            }
            // A network blip: the next tick polls again, as the web page does.
            Err(Fail::RateLimited(s)) => {
                self.next = now + s.max(1) as u64 * 1000;
                None
            }
            Err(Fail::Other) => None,
        }
    }

    /// The button after Denied or Paused: a new round of codes.
    pub fn again(&mut self, now: u64) {
        self.codes = 0;
        self.live = None;
        self.state = State::Waiting;
        self.next = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(n: u32) -> QrStart {
        QrStart {
            approve_url: alloc::format!("https://getsecretary.tech/approve?c={n}"),
            short_code: Some("123456".into()),
            poll_token: alloc::format!("p{n}"),
            expires_in: 120,
        }
    }

    #[test]
    fn approve_path() {
        let mut p = Pairing::new(0);
        assert_eq!(p.phase(0), Phase::Loading);
        assert_eq!(p.tick(0), Want::Start);
        assert_eq!(p.tick(10), Want::Nothing, "one start at a time");
        p.on_started(100, Ok(start(1)));
        assert!(matches!(p.phase(100), Phase::Ready { seconds_left: 120, .. }));
        assert_eq!(p.tick(1000), Want::Nothing);
        assert_eq!(p.tick(2100), Want::Poll("p1"));
        assert_eq!(p.on_polled(2200, Ok(PollAnswer::Pending)), None);
        assert_eq!(p.tick(4100), Want::Poll("p1"));
        let t = p.on_polled(4200, Ok(PollAnswer::Approved("sec_x".into())));
        assert_eq!(t.as_deref(), Some("sec_x"));
        assert_eq!(p.phase(4200), Phase::Done);
        assert_eq!(p.tick(9000), Want::Nothing);
    }

    #[test]
    fn expiry_renews_five_times_then_pauses() {
        let mut p = Pairing::new(0);
        let mut now = 0;
        for n in 1..=5 {
            assert_eq!(p.tick(now), Want::Start, "code {n}");
            p.on_started(now, Ok(start(n)));
            now += 121_000;
        }
        assert_eq!(p.tick(now), Want::Changed);
        assert_eq!(p.phase(now), Phase::Paused);
        assert_eq!(p.tick(now + 5000), Want::Nothing);
        p.again(now);
        assert_eq!(p.tick(now), Want::Start);
    }

    #[test]
    fn rate_limits_errors_and_denial() {
        let mut p = Pairing::new(0);
        assert_eq!(p.tick(0), Want::Start);
        p.on_started(0, Err(Fail::RateLimited(30)));
        assert_eq!(p.phase(1000), Phase::Unavailable { rate_limited: true, retry_s: 29 });
        assert_eq!(p.tick(29_000), Want::Nothing);
        assert_eq!(p.tick(30_000), Want::Start);
        p.on_started(30_000, Err(Fail::Other));
        assert_eq!(p.tick(39_999), Want::Nothing);
        assert_eq!(p.tick(40_000), Want::Start);
        p.on_started(40_000, Ok(start(2)));
        assert_eq!(p.tick(42_000), Want::Poll("p2"));
        assert_eq!(p.on_polled(42_000, Ok(PollAnswer::Denied)), None);
        assert_eq!(p.phase(42_000), Phase::Denied);
        assert_eq!(p.tick(50_000), Want::Nothing);
    }

    #[test]
    fn an_expired_answer_asks_for_a_new_code_at_once() {
        let mut p = Pairing::new(0);
        p.tick(0);
        p.on_started(0, Ok(start(1)));
        assert_eq!(p.tick(2000), Want::Poll("p1"));
        p.on_polled(2000, Ok(PollAnswer::Expired));
        assert_eq!(p.tick(2001), Want::Start);
    }
}
