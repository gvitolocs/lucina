//! The loop: ask the server for Lucina's next call (a long poll with the machine's device token), run it, post its result, again.
//!
//! ```text
//! GET  /v1/device/juno/next?wait=25          {"call": null, "events": [...]} |
//!                                            {"call": {"id", "tool", "arguments", "deadlineMs", "decisionId"?, "internal"?}}
//! POST /v1/device/juno/calls/{id}/result     {"ok": true, "result": {...}} | {"ok": false, "error": "..."}
//! ```
//!
//! Nobody signed in: the tools are off, and the loop waits on the account link (`AccountWatch`) until somebody is. A sign-out while
//! a call runs abandons it: no result is posted. A 401: tell `lucina-accountd` (`system::report_rejected`; it checks with the
//! server and signs out only if the server says so) and wait again. No connection: 2 s, doubling to 60 s. One call at a time,
//! each on a thread of its own with its deadline, so a call that outlives it is answered "timed out" while the loop goes on.
//! `events` in the answer is reserved for Lucina → laptop; Lucina sends none on this generation, so the loop does not read it. `decisionId` (the server's, at the top of
//! the call) goes to the audit and marks the call decided; a `decision_id` in the arguments is dropped and counts for nothing. `internal` is the server's own flag (the secret keeper).
//! The log has the tool, the call's id, ok and the time; never the token, the arguments or the output.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_account::{ApiError, Client};
use serde_json::{Value, json};

use crate::account::AccountWatch;
use crate::audit::{Audit, Entry};
use crate::tools::{self, Machine};

/// A result is at most this, as JSON: a screenshot fits many times over.
pub const MAX_RESULT: usize = 8 << 20;

#[derive(Clone, Debug)]
pub struct Pace {
    pub signed_out: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// The `wait` the server is asked to hold a poll for.
    pub poll_wait_secs: u64,
    /// How long the poll may take in all: the wait and the way there and back.
    pub poll_timeout: Duration,
    pub post_timeout: Duration,
}

impl Default for Pace {
    fn default() -> Pace {
        Pace {
            signed_out: Duration::from_secs(15),
            backoff_min: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            poll_wait_secs: 25,
            poll_timeout: Duration::from_secs(40),
            post_timeout: Duration::from_secs(60),
        }
    }
}

pub struct Service {
    pub machine: Arc<Machine>,
    pub audit: Audit,
    pub pace: Pace,
    /// The sign-in, followed; the machine's own.
    pub account: Arc<AccountWatch>,
    /// The tool of a call whose thread is still running (one that timed out).
    busy: Arc<Mutex<Option<String>>>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// An id that can go in a path and a log as it is.
fn usable_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}

/// A tool name for the log: one of ours, or "unknown".
fn log_name(tool: &str) -> &str {
    if tools::spec(tool).is_some() {
        tool
    } else {
        "unknown"
    }
}

/// The call's time: `deadlineMs` is a number of milliseconds left, or a time (milliseconds since 1970) when it is that large.
pub fn budget(deadline_ms: Option<u64>, cap: Duration) -> Duration {
    let asked = match deadline_ms {
        None => return cap,
        Some(n) if n >= 1_000_000_000_000 => {
            if !lucina_account::client::clock_is_set() {
                return cap;
            }
            Duration::from_millis(n.saturating_sub(now_ms()))
        }
        Some(n) => Duration::from_millis(n),
    };
    asked.min(cap)
}

pub enum Answer {
    Ok(Value),
    Err(String),
}

impl Service {
    pub fn new(machine: Machine, audit: Audit, pace: Pace) -> Service {
        Service {
            account: Arc::clone(&machine.account),
            machine: Arc::new(machine),
            audit,
            pace,
            busy: Arc::new(Mutex::new(None)),
        }
    }

    /// Runs one call from the server as it came, within its time.
    pub fn answer(&self, tool: &str, arguments: &Value, deadline_ms: Option<u64>) -> Answer {
        self.answer_call("", false, false, tool, arguments, deadline_ms)
    }

    /// [`Service::answer`] for the call `id`. `decided` is true only when the server sent a `decisionId`: it sends one only after
    /// checking that the decision answered go for this very call. `internal` when the server flagged the call as its own.
    pub fn answer_call(
        &self,
        id: &str,
        decided: bool,
        internal: bool,
        tool: &str,
        arguments: &Value,
        deadline_ms: Option<u64>,
    ) -> Answer {
        if let Some(why) = self.machine.missing(tool) {
            return Answer::Err(if tools::spec(tool).is_some_and(|s| !s.available) {
                format!("{tool}: {why}")
            } else if tools::spec(tool).is_some() {
                format!("{tool} is not on this laptop now: {why}")
            } else {
                why
            });
        }
        let mut arguments = arguments.clone();
        if let Some(o) = arguments.as_object_mut() {
            o.remove("decision_id");
        }
        let call = match tools::parse(tool, &arguments) {
            Ok(tools::Call::SecretSource { path, .. }) => {
                tools::Call::SecretSource { path, internal }
            }
            Ok(c) => c.from_call(id, decided),
            Err(e) => return Answer::Err(e),
        };
        if let Some(other) = self.busy.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Answer::Err(format!(
                "busy: the last call ({other}) has not finished yet"
            ));
        }
        let limit = budget(deadline_ms, call.cap());
        let (tx, rx) = std::sync::mpsc::channel();
        let machine = Arc::clone(&self.machine);
        let busy = Arc::clone(&self.busy);
        *busy.lock().unwrap_or_else(|p| p.into_inner()) = Some(tool.to_owned());
        std::thread::spawn(move || {
            let r = tools::run(&machine, call);
            *busy.lock().unwrap_or_else(|p| p.into_inner()) = None;
            let _ = tx.send(r);
        });
        match rx.recv_timeout(limit) {
            Ok(Ok(v)) => Answer::Ok(v),
            Ok(Err(e)) => Answer::Err(e),
            Err(_) => Answer::Err("timed out".into()),
        }
    }

    /// One call: run it, post what came of it, write the audit line.
    fn handle(&self, client: &Client, call: &Value, on_rejected: &dyn Fn()) {
        let Some(id) = call["id"].as_str().filter(|i| usable_id(i)) else {
            eprintln!("lucina-devd: a call without a usable id, dropped");
            return;
        };
        let tool = call["tool"].as_str().unwrap_or("");
        // Only the server's own `decisionId` counts, never one in the arguments.
        let decision_id = call["decisionId"]
            .as_str()
            .filter(|d| usable_id(d))
            .unwrap_or("")
            .to_owned();
        let sign_outs = self.account.sign_outs();
        let t0 = Instant::now();
        let answer = self.answer_call(
            id,
            !decision_id.is_empty(),
            call["internal"] == true,
            tool,
            &call["arguments"],
            call["deadlineMs"].as_u64(),
        );
        let ms = t0.elapsed().as_millis() as u64;
        if self.account.sign_outs() != sign_outs {
            eprintln!("lucina-devd: {id}: signed out while it ran; abandoned");
            return;
        }
        // Stage C's fields for the audit line: what she acted on, the undo action, and the effect class.
        let mut fields = [String::new(), String::new(), String::new()];
        if let Answer::Ok(result) = &answer {
            for (f, k) in fields.iter_mut().zip(["target", "undo", "effect"]) {
                *f = result[k].as_str().unwrap_or("").chars().take(128).collect();
            }
        }
        let mut redacted = Vec::new();
        let (ok, mut body) = match answer {
            Answer::Ok(mut result) => {
                if let Some(Value::Array(kinds)) =
                    result.as_object_mut().and_then(|o| o.remove("redacted"))
                {
                    redacted = kinds
                        .iter()
                        .filter_map(|k| k.as_str().map(str::to_owned))
                        .collect();
                }
                (true, json!({"ok": true, "result": result}))
            }
            Answer::Err(error) => (false, json!({"ok": false, "error": error})),
        };
        if body.to_string().len() > MAX_RESULT {
            body = json!({"ok": false, "error": format!("the result is larger than {} MiB", MAX_RESULT >> 20)});
        }
        let ok = ok && body["ok"] == true;
        eprintln!("lucina-devd: {id} {} ok={ok} {ms} ms", log_name(tool));
        let [target, undo, effect] = fields;
        let entry = Entry {
            ts_ms: now_ms(),
            id: id.to_owned(),
            tool: log_name(tool).to_owned(),
            ok,
            ms,
            decision_id,
            target,
            undo,
            effect,
            redacted,
        };
        if let Err(e) = self.audit.record_call(&entry) {
            eprintln!("lucina-devd: audit: {e}");
        }
        let path = format!("/v1/device/juno/calls/{id}/result");
        for attempt in 0..3 {
            match client.post_json_within(&path, &body, self.pace.post_timeout) {
                Ok(_) => return,
                Err(ApiError::Unauthorized(_)) => {
                    on_rejected();
                    return;
                }
                Err(ApiError::Network(e)) if attempt < 2 => {
                    eprintln!("lucina-devd: {id}: posting the result: {e}; again");
                    std::thread::sleep(self.pace.backoff_min);
                }
                Err(e) => {
                    eprintln!(
                        "lucina-devd: {id}: the result was not taken: {}",
                        e.status()
                            .map_or("no answer".to_string(), |s| s.to_string())
                    );
                    return;
                }
            }
        }
    }

    /// Polls until `stop` is set. `client` is asked each time, so a sign-in or a sign-out is seen at once.
    pub fn run(
        &self,
        client: &dyn Fn() -> Option<Client>,
        on_rejected: &dyn Fn(),
        stop: &AtomicBool,
    ) {
        let mut backoff = self.pace.backoff_min;
        let path = format!("/v1/device/juno/next?wait={}", self.pace.poll_wait_secs);
        let mut said_signed_out = false;
        // Said once when polling starts, and again when it resumes after a sign-out or a 401.
        let mut said_polling = false;
        while !stop.load(Ordering::Relaxed) {
            let now = if self.account.signed_in() {
                client()
            } else {
                None
            };
            let Some(c) = now else {
                said_polling = false;
                if !said_signed_out {
                    eprintln!(
                        "lucina-devd: nobody is signed in on this machine; the tools are off until somebody is"
                    );
                    said_signed_out = true;
                }
                // Woken by the account link at the sign-in; `signed_out` only bounds the wait so that `stop` is seen.
                self.account.wait_signed_in(self.pace.signed_out);
                if self.account.signed_in() && client().is_none() {
                    std::thread::sleep(self.pace.backoff_min);
                }
                continue;
            };
            said_signed_out = false;
            if !said_polling {
                let a = self.account.snapshot();
                eprintln!(
                    "lucina-devd: signed in as {} ({}); polling",
                    a["device_name"].as_str().unwrap_or("?"),
                    a["device_id"].as_str().unwrap_or("?")
                );
                said_polling = true;
            }
            match c.get_json_within(&path, self.pace.poll_timeout) {
                Ok(v) => {
                    backoff = self.pace.backoff_min;
                    // A call that came after a sign-out is not run.
                    if v["call"].is_object() && self.account.signed_in() {
                        self.handle(&c, &v["call"], on_rejected);
                    }
                }
                Err(ApiError::Unauthorized(_)) => {
                    eprintln!("lucina-devd: the server refused the sign-in");
                    said_polling = false;
                    on_rejected();
                    // accountd checks with the server (at most every 5 s); a sign-out wakes nobody, so look again later.
                    std::thread::sleep(self.pace.signed_out);
                }
                Err(ApiError::RateLimited(s)) => {
                    std::thread::sleep(Duration::from_secs(s.clamp(1, 120)))
                }
                Err(e) => {
                    eprintln!(
                        "lucina-devd: polling: {e}; again in {} s",
                        backoff.as_secs()
                    );
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(self.pace.backoff_max);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tests::test_machine;
    use lucina_account::Token;
    use lucina_account::fake::{Fake, TOKEN};
    use std::sync::atomic::AtomicU32;

    fn quick() -> Pace {
        Pace {
            signed_out: Duration::from_millis(20),
            backoff_min: Duration::from_millis(20),
            backoff_max: Duration::from_millis(80),
            poll_wait_secs: 1,
            poll_timeout: Duration::from_secs(5),
            post_timeout: Duration::from_secs(5),
        }
    }

    struct Running {
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
        rejected: Arc<AtomicU32>,
    }

    impl Running {
        fn stop(mut self) -> u32 {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                t.join().unwrap();
            }
            self.rejected.load(Ordering::Relaxed)
        }
    }

    fn start(name: &str, fake: &Fake, signed_in: bool) -> (std::path::PathBuf, Running) {
        let (root, m, _) = test_machine(name);
        let audit = Audit::at(&root.join("data/lucina/devd"));
        if signed_in {
            m.account.push("ready", Default::default());
        }
        let service = Service::new(m, audit, quick());
        let base = fake.base().to_owned();
        let stop = Arc::new(AtomicBool::new(false));
        let rejected = Arc::new(AtomicU32::new(0));
        let (s, r) = (Arc::clone(&stop), Arc::clone(&rejected));
        let thread = std::thread::spawn(move || {
            let client = move || {
                if signed_in {
                    Client::new(&base, Token::parse(TOKEN))
                } else {
                    None
                }
            };
            let on_rejected = move || {
                r.fetch_add(1, Ordering::Relaxed);
            };
            service.run(&client, &on_rejected, &s);
        });
        (
            root,
            Running {
                stop,
                thread: Some(thread),
                rejected,
            },
        )
    }

    fn result(fake: &Fake, id: &str) -> Value {
        fake.juno_results()
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, b)| b)
            .unwrap_or(Value::Null)
    }

    #[test]
    fn calls_from_the_server_are_run_and_answered_in_order() {
        let fake = Fake::start();
        let (root, running) = start("loop", &fake, true);
        std::fs::write(root.join("data/home/note.txt"), b"from the owner").unwrap();
        let cred = root
            .join("data/lucina/secretary/credential")
            .display()
            .to_string();
        for (id, tool, args) in [
            ("c1", "tools", json!({})),
            ("c2", "read_file", json!({"path": "note.txt"})),
            ("c3", "read_file", json!({"path": cred})),
            ("c4", "bash", json!({"command": "echo hi; exit 4"})),
            ("c5", "set_brightness", json!({"percent": 40})),
            ("c6", "click", json!({"x": -3, "y": 0})),
            ("c7", "launch_missiles", json!({})),
        ] {
            fake.queue_juno_call(
                json!({"id": id, "tool": tool, "arguments": args, "deadlineMs": 20000}),
            );
        }
        assert!(
            fake.wait_juno_results(7, Duration::from_secs(20)),
            "{:?}",
            fake.juno_results()
        );
        let order: Vec<String> = fake.juno_results().into_iter().map(|(i, _)| i).collect();
        assert_eq!(order, ["c1", "c2", "c3", "c4", "c5", "c6", "c7"]);
        let tools = result(&fake, "c1");
        assert_eq!(tools["ok"], true);
        assert!(
            tools["result"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "bash")
        );
        assert!(tools["result"]["text"].is_string());
        assert_eq!(result(&fake, "c2")["result"]["content"], "from the owner");
        let denied = result(&fake, "c3");
        assert_eq!(denied["ok"], false);
        assert!(denied["error"].as_str().unwrap().contains("refused"));
        let bash = result(&fake, "c4");
        assert_eq!(bash["result"]["exitCode"], 4);
        assert_eq!(bash["result"]["output"], "hi\n");
        assert!(
            result(&fake, "c5")["error"]
                .as_str()
                .unwrap()
                .contains("no panel brightness driver")
        );
        assert!(
            result(&fake, "c6")["error"]
                .as_str()
                .unwrap()
                .contains("x must be")
        );
        assert!(
            result(&fake, "c7")["error"]
                .as_str()
                .unwrap()
                .contains("no tool")
        );
        assert_eq!(running.stop(), 0);
        // The audit has a line per call and none of what went in or came out.
        let audit = std::fs::read_to_string(root.join("data/lucina/devd/audit.jsonl")).unwrap();
        assert_eq!(audit.lines().count(), 7);
        assert!(
            !audit.contains("from the owner")
                && !audit.contains("echo")
                && !audit.contains("note.txt")
        );
        assert!(audit.contains("\"tool\":\"unknown\""));
        // The fake took no other device route.
        assert!(
            fake.all_bodies()
                .iter()
                .all(|(p, _)| p.starts_with("/v1/device/juno/calls/"))
        );
    }

    #[test]
    fn secrets_are_redacted_off_limits_refused_and_reserved_names_say_so() {
        let fake = Fake::start();
        let (root, running) = start("stage2", &fake, true);
        let key = format!("sk-{}", "a1B2c3D4".repeat(4));
        std::fs::write(root.join("data/home/env"), format!("OPENAI={key}\n")).unwrap();
        for (id, tool, args, internal) in [
            ("r1", "read_file", json!({"path": "env"}), false),
            ("r2", "bash", json!({"command": "cat env; id -u"}), false),
            ("r3", "secret_source", json!({"path": "env"}), false),
            ("r4", "secret_source", json!({"path": "env"}), true),
            ("r5", "set_brightness", json!({"percent": 10}), false),
            (
                "r6",
                "setting_set",
                json!({"key": "nonsense", "value": 1}),
                false,
            ),
            ("r7", "account", json!({}), false),
            (
                "r8",
                "bash",
                json!({"command": "dd if=/dev/zero of=/dev/ufs0lun1 count=1"}),
                false,
            ),
            (
                "r9",
                "bash",
                json!({"command": "cat /data/lucina/secretary/credential"}),
                false,
            ),
            (
                "r10",
                "read_file",
                json!({"path": "env", "decision_id": "dec_model"}),
                false,
            ),
        ] {
            fake.queue_juno_call(
                json!({"id": id, "tool": tool, "arguments": args, "internal": internal}),
            );
        }
        fake.queue_juno_call(
            json!({"id": "r11", "tool": "read_file", "arguments": {"path": "env"}, "decisionId": "dec_1"}),
        );
        assert!(
            fake.wait_juno_results(11, Duration::from_secs(20)),
            "{:?}",
            fake.juno_results()
        );
        let r1 = result(&fake, "r1");
        assert_eq!(r1["result"]["content"], "OPENAI=[secret:OPENAI_API_KEY]\n");
        assert!(!r1.to_string().contains(&key) && r1["result"].get("redacted").is_none());
        let r2 = result(&fake, "r2");
        assert!(
            r2["result"]["output"]
                .as_str()
                .unwrap()
                .starts_with("OPENAI=[secret:OPENAI_API_KEY]\n"),
            "{r2}"
        );
        // As the signed-in user: devd's own uid, nothing dropped.
        let uid = unsafe { libc::getuid() }.to_string();
        assert!(
            r2["result"]["output"]
                .as_str()
                .unwrap()
                .contains(&format!("\n{uid}\n")),
            "{r2}"
        );
        assert!(
            result(&fake, "r3")["error"]
                .as_str()
                .unwrap()
                .contains("internal")
        );
        assert_eq!(
            result(&fake, "r4")["result"]["content"],
            format!("OPENAI={key}\n")
        );
        assert!(
            result(&fake, "r5")["error"]
                .as_str()
                .unwrap()
                .contains("not on this generation")
        );
        assert!(
            result(&fake, "r6")["error"]
                .as_str()
                .unwrap()
                .contains("not on this generation")
        );
        let acct = &result(&fake, "r7")["result"];
        assert_eq!(acct["signed_in"], true);
        assert_eq!(acct["state"], "ready");
        assert!(acct["account_events"].as_array().unwrap().len() == 1);
        assert!(!acct.to_string().contains(TOKEN));
        assert!(
            result(&fake, "r8")["error"]
                .as_str()
                .unwrap()
                .contains("UFS LUN 1 is off-limits")
        );
        assert!(
            result(&fake, "r9")["error"]
                .as_str()
                .unwrap()
                .contains("refused")
        );
        assert_eq!(result(&fake, "r10")["ok"], true);
        running.stop();
        let audit = std::fs::read_to_string(root.join("data/lucina/devd/audit.jsonl")).unwrap();
        assert!(!audit.contains(&key) && !audit.contains("a1B2"), "{audit}");
        let lines: Vec<Value> = audit
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let line = |id: &str| lines.iter().find(|l| l["id"] == id).unwrap().clone();
        assert_eq!(line("r1")["redacted"], "OPENAI_API_KEY");
        assert_eq!(line("r2")["redacted"], "OPENAI_API_KEY");
        assert_eq!(line("r4")["redacted"], "");
        // A decision id in the arguments counts for nothing; the server's own decisionId is audited.
        assert_eq!(line("r10")["decision_id"], "");
        assert_eq!(line("r11")["decision_id"], "dec_1");
        for k in ["target", "undo", "effect"] {
            assert_eq!(line("r1")[k], "", "{k}");
        }
    }

    #[test]
    fn a_sign_out_while_a_call_runs_abandons_it() {
        let fake = Fake::start();
        let (root, m, _) = test_machine("signout");
        m.account.push("ready", Default::default());
        let account = Arc::clone(&m.account);
        let service = Arc::new(Service::new(
            m,
            Audit::at(&root.join("data/lucina/devd")),
            quick(),
        ));
        let stop = Arc::new(AtomicBool::new(false));
        let (s, svc, base) = (
            Arc::clone(&stop),
            Arc::clone(&service),
            fake.base().to_owned(),
        );
        let t = std::thread::spawn(move || {
            svc.run(&move || Client::new(&base, Token::parse(TOKEN)), &|| {}, &s);
        });
        fake.queue_juno_call(json!({"id": "long", "tool": "bash", "arguments": {"command": "sleep 1"}, "deadlineMs": 5000}));
        std::thread::sleep(Duration::from_millis(400));
        account.push("signed_out", Default::default());
        std::thread::sleep(Duration::from_millis(1500));
        assert!(fake.juno_results().is_empty(), "{:?}", fake.juno_results());
        // Signed out: no polls until the sign-in comes back, then calls run again.
        let polls = fake.juno_polls();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(fake.juno_polls(), polls);
        account.push("ready", Default::default());
        fake.queue_juno_call(json!({"id": "back", "tool": "tools", "arguments": {}}));
        assert!(fake.wait_juno_results(1, Duration::from_secs(10)));
        assert_eq!(fake.juno_results()[0].0, "back");
        stop.store(true, Ordering::Relaxed);
        t.join().unwrap();
    }

    #[test]
    fn a_call_past_its_deadline_is_answered_timed_out_and_the_next_is_told_it_is_busy() {
        let fake = Fake::start();
        let (_, running) = start("deadline", &fake, true);
        fake.queue_juno_call(json!({"id": "slow", "tool": "bash", "arguments": {"command": "sleep 3", "timeoutSecs": 10}, "deadlineMs": 300}));
        fake.queue_juno_call(
            json!({"id": "next", "tool": "tools", "arguments": {}, "deadlineMs": 5000}),
        );
        assert!(fake.wait_juno_results(2, Duration::from_secs(10)));
        assert_eq!(
            result(&fake, "slow"),
            json!({"ok": false, "error": "timed out"})
        );
        assert!(
            result(&fake, "next")["error"]
                .as_str()
                .unwrap()
                .starts_with("busy")
        );
        // Once it has finished, calls run again.
        std::thread::sleep(Duration::from_secs(4));
        fake.queue_juno_call(json!({"id": "after", "tool": "tools", "arguments": {}}));
        assert!(fake.wait_juno_results(3, Duration::from_secs(10)));
        assert_eq!(result(&fake, "after")["ok"], true);
        running.stop();
    }

    #[test]
    fn a_refused_token_is_reported_and_nobody_signed_in_asks_nothing() {
        let fake = Fake::start();
        fake.revoke();
        let (_, running) = start("revoked", &fake, true);
        std::thread::sleep(Duration::from_millis(300));
        assert!(running.stop() >= 1);
        let fake = Fake::start();
        let (_, running) = start("signedout", &fake, false);
        std::thread::sleep(Duration::from_millis(200));
        running.stop();
        assert_eq!(fake.juno_polls(), 0);
    }

    #[test]
    fn a_call_without_a_usable_id_is_dropped_and_the_loop_goes_on() {
        let fake = Fake::start();
        let (_, running) = start("badid", &fake, true);
        fake.queue_juno_call(json!({"id": "../../v1/chat", "tool": "tools", "arguments": {}}));
        fake.queue_juno_call(json!({"tool": "tools"}));
        fake.queue_juno_call(json!({"id": "fine", "tool": "tools"}));
        assert!(fake.wait_juno_results(1, Duration::from_secs(10)));
        running.stop();
        assert_eq!(fake.juno_results().len(), 1);
        assert_eq!(fake.juno_results()[0].0, "fine");
    }

    #[test]
    fn deadlines_as_time_left_or_as_a_time() {
        let cap = Duration::from_secs(10);
        assert_eq!(budget(None, cap), cap);
        assert_eq!(budget(Some(500), cap), Duration::from_millis(500));
        assert_eq!(budget(Some(60_000), cap), cap);
        let soon = budget(Some(now_ms() + 2_000), cap);
        assert!(soon <= Duration::from_secs(2) && soon > Duration::from_secs(1));
        assert_eq!(budget(Some(now_ms() - 1_000), cap), Duration::ZERO);
    }

    #[test]
    fn ids_and_names_for_the_log() {
        assert!(usable_id("jc_01HX-a.b:c"));
        assert!(!usable_id(""));
        assert!(!usable_id("a/b"));
        assert!(!usable_id(&"a".repeat(129)));
        assert_eq!(log_name("bash"), "bash");
        assert_eq!(log_name("wifi_connect"), "wifi_connect");
        assert_eq!(log_name("cat /etc/shadow"), "unknown");
    }
}
