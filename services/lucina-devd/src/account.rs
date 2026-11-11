//! The machine's sign-in state for the `account` tool and the poll loop: one watcher follows `lucina-accountd` through
//! `lucina-account-link` and keeps the last state and events, so a call can wait for the sign-in or see it was signed out.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_account::system::{Link, Status};

/// How many events the snapshot keeps.
const EVENTS: usize = 50;

/// The sign-in details of a `ready` state; everything is optional, the snapshot never holds more than these fields.
#[derive(Clone, Debug, Default)]
pub struct AccountInfo {
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    pub paired_at: Option<String>,
    pub confirmed_at: Option<String>,
}

struct Inner {
    signed_in: bool,
    state: String,
    info: AccountInfo,
    sign_outs: u64,
    events: VecDeque<(u64, String)>,
}

/// The machine's sign-in state, fed by one thread that follows the account service.
pub struct AccountWatch {
    inner: Mutex<Inner>,
    changed: Condvar,
}

fn state_of(status: &Status) -> (&'static str, AccountInfo) {
    match status {
        Status::SignedIn {
            device_id,
            device_name,
            paired_at,
            confirmed_at,
            ..
        } => {
            let secs = |t: &Option<u64>| t.map(|s| s.to_string());
            (
                "ready",
                AccountInfo {
                    device_id: device_id.clone(),
                    device_name: device_name.clone(),
                    paired_at: secs(paired_at),
                    confirmed_at: secs(confirmed_at),
                },
            )
        }
        Status::Checking | Status::SigningIn { .. } => ("checking", AccountInfo::default()),
        Status::SignedOut => ("signed_out", AccountInfo::default()),
        Status::Starting => ("starting", AccountInfo::default()),
    }
}

impl AccountWatch {
    /// Starts one thread that follows the link, reconnecting after 2 s if the link drops.
    pub fn start() -> Arc<AccountWatch> {
        let watch = Arc::new(AccountWatch::manual_inner());
        let w = Arc::clone(&watch);
        std::thread::spawn(move || {
            let link = Link::system();
            let _following = link.follow(move |status| {
                let (state, info) = match status {
                    Some(s) => state_of(&s),
                    // The service is gone: nobody is signed in.
                    None => ("signed_out", AccountInfo::default()),
                };
                w.push(state, info);
            });
            // `follow` reconnects on its own; if it ever returns, wait 2 s and follow again.
            loop {
                std::thread::sleep(Duration::from_secs(2));
            }
        });
        watch
    }

    /// For tests: a watch fed by hand.
    pub fn manual() -> Arc<AccountWatch> {
        Arc::new(AccountWatch::manual_inner())
    }

    fn manual_inner() -> AccountWatch {
        AccountWatch {
            inner: Mutex::new(Inner {
                signed_in: false,
                state: "starting".to_owned(),
                info: AccountInfo::default(),
                sign_outs: 0,
                events: VecDeque::new(),
            }),
            changed: Condvar::new(),
        }
    }

    /// Records a state; used by the following thread and by tests.
    pub fn push(&self, state: &str, info: AccountInfo) {
        let t_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut inner = self.inner.lock().unwrap();
        if state == "signed_out" && inner.state != "signed_out" {
            inner.sign_outs += 1;
        }
        inner.signed_in = state == "ready";
        inner.state = state.to_owned();
        inner.info = info;
        inner.events.push_back((t_ms, state.to_owned()));
        while inner.events.len() > EVENTS {
            inner.events.pop_front();
        }
        drop(inner);
        self.changed.notify_all();
    }

    pub fn signed_in(&self) -> bool {
        self.inner.lock().unwrap().signed_in
    }

    /// Blocks until signed in or `timeout`; true if signed in.
    pub fn wait_signed_in(&self, timeout: Duration) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if inner.signed_in {
                return true;
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let (guard, _) = self.changed.wait_timeout(inner, left).unwrap();
            inner = guard;
        }
    }

    /// A counter that goes up on every sign-out, so a running call can see it was signed out meanwhile.
    pub fn sign_outs(&self) -> u64 {
        self.inner.lock().unwrap().sign_outs
    }

    /// `{signed_in, state, device_id, device_name, paired_at, confirmed_at, events: [{t_ms, state}]}`, last 50 events.
    pub fn snapshot(&self) -> serde_json::Value {
        let inner = self.inner.lock().unwrap();
        serde_json::json!({
            "signed_in": inner.signed_in,
            "state": inner.state,
            "device_id": inner.info.device_id,
            "device_name": inner.info.device_name,
            "paired_at": inner.info.paired_at,
            "confirmed_at": inner.info.confirmed_at,
            "events": inner.events.iter().map(|(t, s)| serde_json::json!({"t_ms": t, "state": s})).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> AccountInfo {
        AccountInfo {
            device_id: Some("dev_1".to_owned()),
            device_name: Some("juno".to_owned()),
            paired_at: Some("1700000000".to_owned()),
            confirmed_at: Some("1700000100".to_owned()),
        }
    }

    #[test]
    fn transitions_and_snapshot_fields() {
        let w = AccountWatch::manual();
        assert!(!w.signed_in());
        w.push("checking", AccountInfo::default());
        assert!(!w.signed_in());
        w.push("ready", info());
        assert!(w.signed_in());
        let s = w.snapshot();
        assert_eq!(s["signed_in"], true);
        assert_eq!(s["state"], "ready");
        assert_eq!(s["device_id"], "dev_1");
        assert_eq!(s["device_name"], "juno");
        assert_eq!(s["paired_at"], "1700000000");
        assert_eq!(s["confirmed_at"], "1700000100");
        assert_eq!(s["events"].as_array().unwrap().len(), 2);
        w.push("signed_out", AccountInfo::default());
        assert!(!w.signed_in());
        assert_eq!(w.sign_outs(), 1);
        assert_eq!(w.snapshot()["state"], "signed_out");
    }

    #[test]
    fn the_event_list_is_capped_at_50() {
        let w = AccountWatch::manual();
        for i in 0..60 {
            w.push(
                if i % 2 == 0 { "ready" } else { "signed_out" },
                AccountInfo::default(),
            );
        }
        let s = w.snapshot();
        let events = s["events"].as_array().unwrap();
        assert_eq!(events.len(), 50);
        assert_eq!(events[0]["state"], "ready");
        assert_eq!(events[49]["state"], "signed_out");
        assert!(w.sign_outs() > 1);
    }

    #[test]
    fn wait_signed_in_times_out_and_wakes() {
        let w = AccountWatch::manual();
        let start = std::time::Instant::now();
        assert!(!w.wait_signed_in(Duration::from_millis(100)));
        assert!(start.elapsed() >= Duration::from_millis(100));
        let w2 = AccountWatch::manual();
        let waiter = {
            let w2 = Arc::clone(&w2);
            std::thread::spawn(move || w2.wait_signed_in(Duration::from_secs(5)))
        };
        std::thread::sleep(Duration::from_millis(50));
        w2.push("ready", info());
        assert!(waiter.join().unwrap());
    }

    #[test]
    fn sign_outs_counts_each_sign_out() {
        let w = AccountWatch::manual();
        w.push("ready", info());
        w.push("signed_out", AccountInfo::default());
        w.push("signed_out", AccountInfo::default());
        w.push("ready", info());
        w.push("signed_out", AccountInfo::default());
        assert_eq!(w.sign_outs(), 2);
    }
}
