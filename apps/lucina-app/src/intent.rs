//! App intents: typed functions an app offers, so that Lucina does a job in one call instead of clicking through the screens
//! (docs/research/juno-ai-native.md, layer 2; the protocol is docs/research/juno-intents.md). An app lists them in
//! [`App::intents`] and answers them in [`App::intent`], calling the same functions its buttons call, never a second copy.
//!
//! How a call reaches the app:
//! * **Started for it.** lucina-devd (or the shell for it) starts the app with `LUCINA_INTENT` holding the [`Call`] as JSON and
//!   `LUCINA_INTENT_REPLY` the file for the [`Reply`]. The runtime checks the arguments against the intent's schema and hands
//!   the call to the app before its first frame. With `LUCINA_INTENT_HEADLESS=1` (intents that don't show the app) it runs
//!   with no screen and no input, and ends once it has answered, or after `LUCINA_INTENT_TIMEOUT_MS` (60 s).
//! * **Already running:** over the app's socket (stage B), through the same [`dispatch`].
//! * **In tests:** [`crate::Harness::intent`].
//!
//! `<program> --intents` prints the intents as JSON and ends without touching the screen: lucina-devd's listing.

use std::os::fd::RawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use lucina_intents::rules::{Effect, Kind, State, WriteKind};
pub use lucina_intents::spec::{
    self, ArgSpec, Call, Entity, GONE, IntentSpec, Reply, Undo, UndoEntry, Write, stamp_of, there,
};
pub use lucina_intents::rules;

use crate::App;

/// Where an answer goes.
enum Sink {
    /// The file `LUCINA_INTENT_REPLY` names: written whole (a temporary file, then a rename), mode 0600.
    File(PathBuf),
    /// Kept for whoever waits: the headless loop, a test. `wake` is written to when it arrives.
    Slot(Arc<Mutex<Option<Reply>>>, RawFd),
    /// The app's socket (lucina-ax), for an app that is running: the reply line goes back to lucina-devd.
    Socket(lucina_ax::proto::Waiting),
}

/// The way back for one call. An app may keep it in its state and answer later, from [`App::message`], once its thread is done.
/// Dropped without an answer (the app ended, or forgot), it answers that the app did not.
pub struct Responder {
    sink: Option<Sink>,
    name: String,
}

impl Responder {
    fn new(sink: Sink, name: &str) -> Responder {
        Responder { sink: Some(sink), name: name.to_string() }
    }

    /// A responder whose answer lands in the returned slot.
    pub fn slot(name: &str) -> (Responder, Arc<Mutex<Option<Reply>>>) {
        let slot = Arc::new(Mutex::new(None));
        (Responder::new(Sink::Slot(slot.clone(), -1), name), slot)
    }

    /// A responder whose answer goes back over the app's socket.
    pub(crate) fn socket(waiting: lucina_ax::proto::Waiting, name: &str) -> Responder {
        Responder::new(Sink::Socket(waiting), name)
    }

    /// The intent this answers.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Answer the call.
    pub fn send(mut self, reply: Reply) {
        self.deliver(reply);
    }

    fn deliver(&mut self, reply: Reply) {
        match self.sink.take() {
            Some(Sink::File(path)) => {
                if let Err(e) = write_reply(&path, &reply) {
                    eprintln!("intent {}: cannot write the answer to {}: {e}", self.name, path.display());
                }
            }
            Some(Sink::Slot(slot, wake)) => {
                if let Ok(mut s) = slot.lock() {
                    *s = Some(reply);
                }
                crate::app::wake_fd(wake);
            }
            Some(Sink::Socket(waiting)) => match serde_json::to_value(&reply) {
                Ok(v) => waiting.reply(&v),
                Err(e) => eprintln!("intent {}: cannot send the answer: {e}", self.name),
            },
            None => {}
        }
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        if self.sink.is_some() {
            let text = format!("{}: the app ended before it answered", self.name);
            self.deliver(Reply::failed(text));
        }
    }
}

fn write_reply(path: &std::path::Path, reply: &Reply) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(reply).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("part");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(&bytes)?;
    }
    std::fs::rename(&tmp, path)
}

/// The intent of `A` called `name`.
pub fn find<A: App>(name: &str) -> Option<&'static IntentSpec> {
    A::intents().iter().find(|s| s.name == name)
}

/// Hand a call to the app, once it is known to be one of its intents with acceptable arguments; otherwise answer why not. The
/// one door every way in goes through.
pub fn dispatch<A: App>(app: &mut A, call: Call, reply: Responder) {
    let Some(spec) = find::<A>(&call.name) else {
        let names: Vec<&str> = A::intents().iter().map(|s| s.name).collect();
        let has = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        return reply.send(Reply::failed(format!("no intent {} here (this app has: {has})", call.name)));
    };
    if let Err(e) = spec.check(&call.args) {
        return reply.send(Reply::failed(e));
    }
    if call.undo.is_some() && !A::intents().iter().any(|s| s.inverse == Some(spec.name)) {
        return reply.send(Reply::failed(format!("{} undoes nothing", spec.name)));
    }
    app.intent(call, reply);
}

/// A call that came over the app's socket: handed to the app like any other, its answer going back on the same connection.
pub(crate) fn from_socket<A: App>(app: &mut A, call: serde_json::Value, waiting: lucina_ax::proto::Waiting) {
    match serde_json::from_value::<Call>(call) {
        Ok(call) => {
            let r = Responder::socket(waiting, &call.name);
            dispatch(app, call, r);
        }
        Err(e) => Responder::socket(waiting, "intent").send(Reply::failed(format!("the call is not readable: {e}"))),
    }
}

/// What `--intents` prints.
pub fn listing<A: App>() -> String {
    let all: Vec<serde_json::Value> = A::intents().iter().map(IntentSpec::to_json).collect();
    serde_json::Value::Array(all).to_string()
}

/// The call this process was started for, if any, and where its answer goes. A call that can't be read is answered at once.
pub(crate) fn from_env(wake: Option<(Arc<Mutex<Option<Reply>>>, RawFd)>) -> Option<(Call, Responder)> {
    let text = std::env::var("LUCINA_INTENT").ok()?;
    let sink = match (wake, std::env::var_os("LUCINA_INTENT_REPLY")) {
        (Some((slot, fd)), _) => Sink::Slot(slot, fd),
        (None, Some(path)) => Sink::File(PathBuf::from(path)),
        (None, None) => {
            eprintln!("intent: LUCINA_INTENT without LUCINA_INTENT_REPLY, ignored");
            return None;
        }
    };
    match serde_json::from_str::<Call>(&text) {
        Ok(call) => {
            let r = Responder::new(sink, &call.name);
            Some((call, r))
        }
        Err(e) => {
            Responder::new(sink, "intent").send(Reply::failed(format!("the call is not readable: {e}")));
            None
        }
    }
}

/// The reply file of a headless run, for the end of [`run_headless`].
pub(crate) fn reply_path() -> Option<PathBuf> {
    std::env::var_os("LUCINA_INTENT_REPLY").map(PathBuf::from)
}

pub(crate) fn write_to(path: &std::path::Path, reply: &Reply) {
    if let Err(e) = write_reply(path, reply) {
        eprintln!("intent: cannot write the answer to {}: {e}", path.display());
    }
}

/// How long a headless run waits for its answer.
pub(crate) fn headless_timeout() -> std::time::Duration {
    let ms = std::env::var("LUCINA_INTENT_TIMEOUT_MS").ok().and_then(|s| s.parse().ok()).unwrap_or(60_000u64);
    std::time::Duration::from_millis(ms.clamp(100, 600_000))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Harness, Ui};
    use serde_json::json;

    /// A counter with three intents: one answered at once, one answered from a message, one that never answers.
    struct Counter {
        n: i64,
        waiting: Option<Responder>,
        forgotten: Vec<Responder>,
    }

    const INTENTS: &[IntentSpec] = &[
        IntentSpec {
            name: "counter.add",
            description: "Add to the count.",
            args: &[ArgSpec::int("by", "How much.", true, 1, 100)],
            effect: Effect::Write,
            shows: false,
            inverse: Some("counter.take"),
        },
        IntentSpec {
            name: "counter.take",
            description: "Take from the count (the undo of add).",
            args: &[ArgSpec::int("by", "How much.", true, 1, 100)],
            effect: Effect::Write,
            shows: false,
            inverse: None,
        },
        IntentSpec { name: "counter.later", description: "Answered from a message.", args: &[], effect: Effect::Read, shows: true, inverse: None },
        IntentSpec { name: "counter.never", description: "Never answered.", args: &[], effect: Effect::Read, shows: false, inverse: None },
    ];

    impl App for Counter {
        type Msg = ();
        fn view(&mut self, ui: &mut Ui) {
            ui.label(&format!("{}", self.n));
        }
        fn message(&mut self, _: ()) {
            if let Some(r) = self.waiting.take() {
                r.send(Reply::done(format!("count is {}", self.n)).with_value(json!(self.n)));
            }
        }
        fn intents() -> &'static [IntentSpec] {
            INTENTS
        }
        fn intent(&mut self, call: Call, reply: Responder) {
            let by = call.args["by"].as_i64().unwrap_or(0);
            match call.name.as_str() {
                "counter.add" => {
                    let before = there(self.n.to_string().as_bytes());
                    self.n += by;
                    let w = Write {
                        kind: WriteKind::Change,
                        target: "counter:main".into(),
                        before: before.into(),
                        after: there(self.n.to_string().as_bytes()).into(),
                        undo: Undo::Inverse { intent: "counter.take".into(), args: json!({"by": by}) },
                    };
                    reply.send(Reply::done("added").with_write(w))
                }
                "counter.take" => {
                    let e: rules::Entry = call.undo.expect("an undo").into();
                    let now = there(self.n.to_string().as_bytes());
                    match rules::undo_to(now, e) {
                        Some(_) => {
                            self.n -= by;
                            reply.send(Reply::done("taken"))
                        }
                        None => reply.send(Reply::failed("the count changed since")),
                    }
                }
                "counter.later" => self.waiting = Some(reply),
                _ => self.forgotten.push(reply),
            }
        }
    }

    fn harness() -> Harness<Counter> {
        Harness::new(Counter { n: 0, waiting: None, forgotten: Vec::new() }, 200, 100)
    }

    #[test]
    fn an_intent_runs_with_checked_arguments() {
        let mut h = harness();
        let r = h.intent("counter.add", json!({"by": 5})).unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(h.app().n, 5);
        let w = r.write.unwrap();
        assert_eq!(w.entry().unwrap().op, rules::UndoOp::PutBack);
        for (args, why) in [(json!({}), "by is needed"), (json!({"by": 0}), "by is not"), (json!({"by": 1, "x": 1}), "no argument")] {
            let r = h.intent("counter.add", args).unwrap();
            assert!(!r.ok && r.text.contains(why), "{r:?}");
        }
        assert_eq!(h.app().n, 5, "a refused call changes nothing");
        let r = h.intent("counter.fly", json!({})).unwrap();
        assert!(r.text.contains("no intent counter.fly") && r.text.contains("counter.add"), "{r:?}");
    }

    #[test]
    fn an_undo_goes_only_to_an_inverse_and_only_while_the_target_is_unchanged() {
        let mut h = harness();
        let w = h.intent("counter.add", json!({"by": 3})).unwrap().write.unwrap();
        let entry: UndoEntry = w.entry().unwrap().into();
        let undo = |args| Call { call: "u".into(), name: "counter.take".into(), args, undo: Some(entry) };
        // Not an inverse of anything: refused before the app sees it.
        let r = h.intent_call(Call { name: "counter.later".into(), ..undo(json!({})) }).unwrap();
        assert!(r.text.contains("undoes nothing"), "{r:?}");
        h.app().n += 1; // the owner changed it
        assert!(!h.intent_call(undo(json!({"by": 3}))).unwrap().ok);
        h.app().n -= 1;
        assert!(h.intent_call(undo(json!({"by": 3}))).unwrap().ok);
        assert_eq!(h.app().n, 0);
    }

    #[test]
    fn a_late_answer_and_a_forgotten_one() {
        let mut h = harness();
        assert!(h.intent("counter.later", json!({})).is_none());
        assert!(h.reply().is_none());
        h.app().message(());
        let r = h.reply().unwrap();
        assert_eq!(r.value, json!(0));
        assert!(h.intent("counter.never", json!(null)).is_none());
        h.app().forgotten.clear();
        let r = h.reply().unwrap();
        assert!(!r.ok && r.text.contains("ended before it answered"), "{r:?}");
    }

    #[test]
    fn a_running_app_takes_intents_over_its_socket_and_answers_late_ones_there() {
        let dir = std::env::temp_dir().join(format!("lucina-app-intent-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sock = dir.join("1.sock");
        let mut server = lucina_ax::Server::bind_at(&sock).unwrap();
        let mut core = crate::app::Core::new(Counter { n: 0, waiting: None, forgotten: Vec::new() }, 200, 100);
        let ask = |call: serde_json::Value| {
            let sock = sock.clone();
            std::thread::spawn(move || lucina_ax::proto::request(&sock, &json!({"op": "intent", "call": call}), std::time::Duration::from_secs(5)).unwrap())
        };
        let serve_until = |server: &mut lucina_ax::Server, core: &mut crate::app::Core<Counter>, t: &std::thread::JoinHandle<serde_json::Value>| {
            while !t.is_finished() {
                crate::app::serve_ax(server, core, "counter");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let t = ask(json!({"call": "s1", "name": "counter.add", "args": {"by": 2}}));
        serve_until(&mut server, &mut core, &t);
        let r: Reply = serde_json::from_value(t.join().unwrap()).unwrap();
        assert!(r.ok && r.write.is_some(), "{r:?}");
        assert_eq!(core.app.n, 2);
        // Checked as at the start: a bad argument never reaches the app.
        let t = ask(json!({"call": "s2", "name": "counter.add", "args": {"by": 0}}));
        serve_until(&mut server, &mut core, &t);
        assert!(!serde_json::from_value::<Reply>(t.join().unwrap()).unwrap().ok);
        // Answered after a message, on the same connection.
        let t = ask(json!({"call": "s3", "name": "counter.later", "args": {}}));
        while core.app.waiting.is_none() {
            crate::app::serve_ax(&mut server, &mut core, "counter");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        core.message(());
        let r: Reply = serde_json::from_value(t.join().unwrap()).unwrap();
        assert_eq!(r.value, json!(2));
        // Trees still work beside intents.
        let t = std::thread::spawn({
            let sock = sock.clone();
            move || lucina_ax::proto::request(&sock, &json!({"op": "tree"}), std::time::Duration::from_secs(5)).unwrap()
        });
        serve_until(&mut server, &mut core, &t);
        let tree = t.join().unwrap();
        assert!(tree.get("error").is_none() && tree.get("version").is_some(), "{tree}");
        drop(server);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_listing_and_the_reply_file() {
        let list: serde_json::Value = serde_json::from_str(&listing::<Counter>()).unwrap();
        assert_eq!(list[0]["name"], json!("counter.add"));
        assert_eq!(list[0]["inverse"], json!("counter.take"));
        assert_eq!(list[0]["args"]["required"], json!(["by"]));
        let dir = std::env::temp_dir().join(format!("lucina-app-intent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c1.json");
        Responder::new(Sink::File(path.clone()), "counter.add").send(Reply::done("ok"));
        let back: Reply = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(back.ok);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        drop(Responder::new(Sink::File(path.clone()), "counter.add"));
        let back: Reply = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!back.ok);
        std::fs::remove_dir_all(&dir).ok();
    }
}
