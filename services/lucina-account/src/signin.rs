//! QR sign-in (`web/src/views/QrLogin.tsx`, `docs/server-api.md` "QR sign-in"): ask for a code, show it as a QR code and six
//! digits, poll every 2 s. An expired code is replaced on its own, up to [`MAX_CODES`] times; after that the app shows a
//! button. As a device request (`{"device": {"name"}}`) the approving poll carries a device token and no cookie, and it comes
//! only once: if the answer is lost, the owner revokes that device and pairs again.
//!
//! The owner approves on a device that is already signed in. Nothing here can approve.
use std::time::{Duration, Instant};

pub use lucina_account_link::{Phase, Qr, QUIET};

use crate::client::{ApiError, Client, QrPoll, QrRequest};
use crate::credential::Token;

pub const POLL: Duration = Duration::from_secs(2);
pub const MAX_CODES: u32 = 5;
/// How this laptop appears on the approve screen and in the device list.
pub const DEVICE_NAME: &str = "Juno (Galaxy Book Go)";

struct Live {
    request: QrRequest,
    qr: Box<Qr>,
    expires: Instant,
}

/// The state machine behind that screen, on a [`Client`] without a token. Call [`SignIn::tick`] often; it does at most one
/// request, and only when one is due.
pub struct SignIn {
    device_name: String,
    live: Option<Live>,
    codes: u32,
    next: Instant,
    state: State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Waiting,
    Denied,
    Paused,
    Unavailable { message: String, rate_limited: bool },
    Done,
}

/// What a tick found.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing new.
    Idle,
    /// The screen changed.
    Changed,
    /// Approved. Save it; it is never sent again.
    Token(Token),
}

impl SignIn {
    pub fn new(device_name: &str) -> SignIn {
        SignIn { device_name: device_name.to_owned(), live: None, codes: 0, next: Instant::now(), state: State::Waiting }
    }

    pub fn phase(&self, now: Instant) -> Phase {
        match &self.state {
            State::Denied => Phase::Denied,
            State::Paused => Phase::Paused,
            State::Done => Phase::SignedIn,
            State::Unavailable { message, rate_limited } => {
                Phase::Unavailable { message: message.clone(), rate_limited: *rate_limited }
            }
            State::Waiting => match &self.live {
                None => Phase::Loading,
                Some(l) => Phase::Ready {
                    qr: l.qr.clone(),
                    short_code: l.request.short_code.clone(),
                    seconds_left: l.expires.saturating_duration_since(now).as_secs(),
                },
            },
        }
    }

    /// When the code on the screen stops being good, if there is one.
    pub fn live_until(&self) -> Option<Instant> {
        self.live.as_ref().map(|l| l.expires)
    }

    /// The link behind the QR code on the screen.
    pub fn approve_url(&self) -> Option<&str> {
        self.live.as_ref().map(|l| l.request.approve_url.as_str())
    }

    /// The button: a new round of codes.
    pub fn again(&mut self, now: Instant) {
        self.codes = 0;
        self.live = None;
        self.state = State::Waiting;
        self.next = now;
    }

    fn start(&mut self, client: &Client, now: Instant) -> Step {
        if self.codes >= MAX_CODES {
            self.live = None;
            self.state = State::Paused;
            return Step::Changed;
        }
        match client.qr_start(&self.device_name) {
            Ok(request) => {
                let Some(qr) = Qr::encode(&request.approve_url) else {
                    self.state = State::Unavailable { message: "The sign-in link does not fit a QR code".into(), rate_limited: false };
                    return Step::Changed;
                };
                self.codes += 1;
                self.live = Some(Live { expires: now + Duration::from_secs(request.expires_in), qr: Box::new(qr), request });
                self.state = State::Waiting;
                self.next = now + POLL;
                Step::Changed
            }
            Err(ApiError::RateLimited(s)) => {
                // Shares the budget of the password form: try again when the server says.
                self.next = now + Duration::from_secs(s.max(1));
                self.state = State::Unavailable { message: format!("Too many sign-in attempts; trying again in {s} s"), rate_limited: true };
                Step::Changed
            }
            Err(e) => {
                self.next = now + Duration::from_secs(10);
                self.state = State::Unavailable { message: e.to_string(), rate_limited: false };
                Step::Changed
            }
        }
    }

    pub fn tick(&mut self, client: &Client, now: Instant) -> Step {
        match self.state {
            State::Denied | State::Paused | State::Done => return Step::Idle,
            State::Waiting | State::Unavailable { .. } => {}
        }
        if now < self.next {
            return Step::Idle;
        }
        let Some(live) = &self.live else { return self.start(client, now) };
        if now >= live.expires {
            self.live = None;
            return self.start(client, now);
        }
        self.next = now + POLL;
        match client.qr_poll(&live.request.poll_token) {
            Ok(QrPoll::Pending) => {
                if matches!(self.state, State::Unavailable { .. }) {
                    self.state = State::Waiting;
                    return Step::Changed;
                }
                Step::Idle
            }
            Ok(QrPoll::Approved(token)) => {
                self.live = None;
                self.state = State::Done;
                Step::Token(token)
            }
            Ok(QrPoll::Denied) => {
                self.live = None;
                self.state = State::Denied;
                Step::Changed
            }
            Ok(QrPoll::Expired) => {
                self.live = None;
                self.start(client, now)
            }
            // A network blip: the next tick polls again, as the web page does.
            Err(ApiError::RateLimited(s)) => {
                self.next = now + Duration::from_secs(s.max(1));
                Step::Idle
            }
            Err(_) => Step::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_approve_link_encodes() {
        let q = Qr::encode("https://getsecretary.tech/approve?c=AAAAAAAAAAAAAAAAAAAAAA").unwrap();
        assert!(q.is_dark(0, 0) && q.is_dark(q.width - 1, 0) && q.is_dark(0, q.width - 1));
        assert!(!q.is_dark(q.width, 0));
    }
}
