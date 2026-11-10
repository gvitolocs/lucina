use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;

use lucina_intents::rules::WriteKind;
use lucina_intents::spec::{ArgSpec, IntentSpec, Write};

use super::*;

/// A memo app that is a shell script: it lists its intents, and answers each from a file the test wrote.
const CREATE: IntentSpec = IntentSpec {
    name: "memo.create",
    description: "A new memo.",
    args: &[ArgSpec::text("body", "Its text.", true, 1000)],
    effect: Effect::Write,
    shows: false,
    inverse: Some("memo.trash"),
};
const TRASH: IntentSpec = IntentSpec {
    name: "memo.trash",
    description: "A memo to the trash.",
    args: &[ArgSpec::id("id", "The memo.", true)],
    effect: Effect::Destructive,
    shows: false,
    inverse: None,
};
const SHOW: IntentSpec = IntentSpec { name: "memo.show", description: "Shows a memo.", args: &[], effect: Effect::Read, shows: true, inverse: None };
const QUIT: IntentSpec = IntentSpec { name: "memo.quit", description: "Ends without answering.", args: &[], effect: Effect::Read, shows: false, inverse: None };
const HANG: IntentSpec = IntentSpec { name: "memo.hang", description: "Never answers.", args: &[], effect: Effect::Read, shows: false, inverse: None };
const PAIR: IntentSpec = IntentSpec { name: "memo.pair", description: "Approves a pairing.", args: &[], effect: Effect::OwnerOnly, shows: false, inverse: None };

const SCRIPT: &str = r#"#!/bin/sh
d=$(dirname "$0")
if [ "$1" = "--intents" ]; then [ "$LUCINA_FB" = /run/lucina/no-screen-for-intent-listing ] || exit 9; cat "$d/listing.json"; exit 0; fi
[ "$LUCINA_FB" = /run/lucina/no-screen-for-intent-listing ] || exit 9
case "$LUCINA_INTENT" in
  *'"name":"memo.create"'*) cat "$d/create.json" > "$LUCINA_INTENT_REPLY.tmp"; mv "$LUCINA_INTENT_REPLY.tmp" "$LUCINA_INTENT_REPLY" ;;
  *'"name":"memo.trash"'*'"undo"'*) cat "$d/trash.json" > "$LUCINA_INTENT_REPLY" ;;
  *'"name":"memo.trash"'*) echo '{"ok":true,"text":"trashed, no undo"}' > "$LUCINA_INTENT_REPLY" ;;
  *'"name":"memo.quit"'*) exit 3 ;;
  *'"name":"memo.hang"'*) sleep 5 ;;
esac
"#;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lucina-intent-host-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn memo_app(d: &Path) -> Manifest {
    let all: Vec<Value> = [CREATE, TRASH, SHOW, QUIT, HANG, PAIR].iter().map(IntentSpec::to_json).collect();
    std::fs::write(d.join("listing.json"), Value::Array(all).to_string()).unwrap();
    let w = Write {
        kind: WriteKind::Create,
        target: "memo:m_1".into(),
        before: spec::GONE.into(),
        after: spec::there(b"v1").into(),
        undo: Undo::Inverse { intent: "memo.trash".into(), args: json!({"id": "m_1"}) },
    };
    let create = Reply::done("Made memo m_1").with_entity(spec::Entity::new("memo", "m_1", "Milk")).with_write(w);
    std::fs::write(d.join("create.json"), serde_json::to_string(&create).unwrap()).unwrap();
    std::fs::write(d.join("trash.json"), serde_json::to_string(&Reply::done("Trashed m_1")).unwrap()).unwrap();
    let exec = d.join("memo.sh");
    std::fs::write(&exec, SCRIPT).unwrap();
    std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = format!("id = memo\ntitle = Memo\nexec = {}\nintents = memo.create, memo.trash, memo.show, memo.quit, memo.hang, memo.pair, memo.ghost\n", exec.display());
    Manifest::parse(&text).unwrap()
}

#[derive(Default)]
struct FakeHost {
    front: Option<String>,
    opened: RefCell<Vec<(String, Value)>>,
    /// What `open_for` writes as the app's answer, as an app started by the shell would.
    show_reply: Option<Reply>,
    allowed_root: PathBuf,
}

impl Host for FakeHost {
    fn front(&self) -> Option<String> {
        self.front.clone()
    }
    fn open_for(&self, app: &str, call: &Value, reply: &Path) -> Result<(), String> {
        self.opened.borrow_mut().push((app.into(), call.clone()));
        if let Some(r) = &self.show_reply {
            std::fs::write(reply, serde_json::to_string(r).unwrap()).unwrap();
        }
        Ok(())
    }
    fn open(&self, app: &str) -> Result<Value, String> {
        self.opened.borrow_mut().push((app.into(), Value::Null));
        Ok(json!({"ok": true, "opening": app}))
    }
    fn close(&self) -> Result<Value, String> {
        Ok(json!({"ok": true, "closing": self.front}))
    }
    fn apps(&self) -> Result<Value, String> {
        Ok(json!({"apps": [{"id": "memo", "title": "Memo"}], "front": self.front}))
    }
    fn file_allowed(&self, path: &Path) -> Result<(), String> {
        if path.starts_with(&self.allowed_root) { Ok(()) } else { Err(format!("{} is off limits", path.display())) }
    }
}

struct World {
    d: PathBuf,
    catalog: Catalog,
    journal: Journal,
    host: FakeHost,
}

impl World {
    fn new(name: &str) -> World {
        let d = dir(name);
        let m = memo_app(&d);
        let catalog = Catalog::load(&[m], Duration::from_secs(5));
        let journal = Journal::at(&d.join("undo"));
        let host = FakeHost { allowed_root: d.join("home"), ..FakeHost::default() };
        World { d, catalog, journal, host }
    }
    fn runner(&self) -> Runner<'_, FakeHost> {
        Runner { catalog: &self.catalog, journal: &self.journal, host: &self.host, reply_dir: self.d.join("replies"), timeout: Duration::from_millis(1500) }
    }
}

#[test]
fn the_catalog_reads_programs_and_says_what_does_not_match() {
    let w = World::new("catalog");
    assert!(w.catalog.find("memo.create").is_some());
    assert!(w.catalog.find("shell.open").is_some());
    assert!(w.catalog.find("memo.ghost").is_none());
    assert!(w.catalog.problems.iter().any(|p| p.contains("memo.ghost is in its manifest but the program does not offer it")), "{:?}", w.catalog.problems);
    // Without an app named: the shell's and the app in front's only.
    let none = w.catalog.list(None, None);
    assert!(none["intents"].as_array().unwrap().iter().all(|i| i["app"] == "shell"));
    let front = w.catalog.list(None, Some("memo"));
    assert!(front["intents"].as_array().unwrap().iter().any(|i| i["name"] == "memo.create"));
    assert_eq!(w.catalog.list(Some("memo"), None)["intents"].as_array().unwrap().len(), 6);
    assert_eq!(none["apps_with_intents"], json!(["shell", "memo"]));
}

#[test]
fn a_program_that_lists_garbage_is_a_problem_not_a_failure() {
    let d = dir("garbage");
    let exec = d.join("bad.sh");
    std::fs::write(&exec, "#!/bin/sh\necho not json\n").unwrap();
    std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
    let m = Manifest::parse(&format!("id = bad\ntitle = Bad\nexec = {}\nintents = bad.go\n", exec.display())).unwrap();
    let c = Catalog::load(&[m], Duration::from_secs(5));
    assert!(c.find("bad.go").is_none());
    assert!(c.problems[0].starts_with("bad:"), "{:?}", c.problems);
}

#[test]
fn create_then_undo_through_the_inverse_and_only_once() {
    let w = World::new("undo");
    let r = w.runner();
    let v = r.call("c1", "memo.create", &json!({"body": "milk"}), false, 1000).unwrap();
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["undoable"], json!(true));
    assert_eq!(v["entities"][0]["id"], json!("m_1"));
    let action = v["action"].as_str().unwrap().to_string();
    let list = r.undo(None, "c2", 2000).unwrap();
    assert_eq!(list["actions"][0]["action"], json!(action));
    // The undo runs memo.trash, a destructive intent, with no decision: undoing her own write is a write.
    let u = r.undo(Some(&action), "c3", 3000).unwrap();
    assert_eq!(u["ok"], json!(true), "{u}");
    assert_eq!(u["text"], json!("Trashed m_1"));
    assert_eq!(u["undid"], json!(action));
    assert_eq!(u["effect"], json!("write"));
    assert!(r.undo(Some(&action), "c4", 4000).unwrap_err().contains("undone already"));
    assert!(r.undo(None, "c5", 5000).unwrap()["actions"].as_array().unwrap().is_empty());
    let lines = w.journal.lines().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1].undoes, action);
}

#[test]
fn destructive_needs_a_decision_bound_to_the_call_and_owner_only_never_runs() {
    let w = World::new("gate");
    let r = w.runner();
    let args = json!({"id": "m_9"});
    let v = r.call("c1", "memo.trash", &args, false, 1000).unwrap();
    assert_eq!(v["needs_decision"], json!(true));
    assert_eq!(v["effect"], json!("destructive"));
    assert_eq!(v["call_hash"], json!(call_hash("memo.trash", &args)));
    assert_ne!(call_hash("memo.trash", &args), call_hash("memo.trash", &json!({"id": "m_8"})));
    assert!(w.journal.lines().unwrap().is_empty(), "nothing ran, nothing journaled");
    let v = r.call("c2", "memo.trash", &args, true, 1000).unwrap();
    assert_eq!(v["ok"], json!(true), "{v}");
    assert_eq!(v["undoable"], json!(false), "this trash gave no undo");
    assert_eq!(w.journal.lines().unwrap().len(), 1, "a write with no undo is still in the journal");
    assert!(r.call("c3", "memo.pair", &json!({}), true, 1000).unwrap_err().contains("owner's own"));
}

#[test]
fn arguments_are_checked_before_any_program_runs() {
    let w = World::new("args");
    let r = w.runner();
    assert!(r.call("c1", "memo.create", &json!({}), false, 1).unwrap_err().contains("body is needed"));
    assert!(r.call("c1", "memo.create", &json!({"body": "x", "colour": 1}), false, 1).unwrap_err().contains("no argument"));
    assert!(r.call("c1", "memo.fly", &json!({}), false, 1).unwrap_err().contains("no intent memo.fly"));
    assert!(r.call("../x", "memo.create", &json!({"body": "x"}), false, 1).unwrap_err().contains("not a call id"));
    assert!(!w.d.join("replies").exists());
}

#[test]
fn an_app_that_ends_or_hangs_without_answering() {
    let w = World::new("hang");
    let r = w.runner();
    assert!(r.call("c1", "memo.quit", &json!({}), false, 1).unwrap_err().contains("without answering"));
    let t = Instant::now();
    assert!(r.call("c2", "memo.hang", &json!({}), false, 1).unwrap_err().contains("no answer"));
    assert!(t.elapsed() < Duration::from_secs(5), "the wait has a limit");
}

#[test]
fn an_intent_that_shows_goes_through_the_shell_and_the_shells_own_intents() {
    let mut w = World::new("shows");
    w.host.show_reply = Some(Reply::done("Showing"));
    let r = w.runner();
    let v = r.call("c1", "memo.show", &json!({}), false, 1).unwrap();
    assert_eq!(v["text"], json!("Showing"));
    let opened = w.host.opened.borrow().clone();
    assert_eq!(opened[0].0, "memo");
    assert_eq!(opened[0].1["name"], json!("memo.show"));
    assert_eq!(opened[0].1["call"], json!("c1"));
    let v = r.call("c2", "shell.open", &json!({"app": "memo"}), false, 1).unwrap();
    assert_eq!(v["text"], json!("Opening memo"));
    assert_eq!(v["undoable"], json!(false));
    assert!(r.call("c3", "shell.open", &json!({"app": "../sh"}), false, 1).is_err());
    assert_eq!(r.call("c4", "shell.apps", &json!({}), false, 1).unwrap()["value"]["apps"][0]["id"], json!("memo"));
}

/// A file write of hers, recorded the way lucina-devd's `write_file` will.
fn write_file(w: &World, path: &Path, bytes: &[u8], ts: u64) -> String {
    let (before, undo) = before_file_write(&w.journal, path).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    let kind = if before.exists { WriteKind::Change } else { WriteKind::Create };
    let write = Write { kind, target: format!("file:{}", path.display()), before: before.into(), after: spec::there(bytes).into(), undo };
    let line = w.journal.record(ts, "f", "write_file", Effect::Write, Some(write)).unwrap();
    assert!(line.undoable);
    line.action
}

#[test]
fn file_undos_remove_what_she_made_and_put_back_what_she_overwrote() {
    let w = World::new("files");
    let r = w.runner();
    let new = w.d.join("home/new.txt");
    let a = write_file(&w, &new, b"hers", 1);
    assert!(r.undo(Some(&a), "u1", 2).unwrap()["ok"].as_bool().unwrap());
    assert!(!new.exists());

    let old = w.d.join("home/old.txt");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::write(&old, b"his").unwrap();
    let a = write_file(&w, &old, b"hers", 3);
    assert_eq!(r.undo(Some(&a), "u2", 4).unwrap()["text"], json!(format!("{} has its old content back", old.display())));
    assert_eq!(std::fs::read(&old).unwrap(), b"his");
}

#[test]
fn a_file_changed_since_is_left_alone_and_stays_undoable() {
    let w = World::new("changed");
    let r = w.runner();
    let p = w.d.join("home/plan.txt");
    let a = write_file(&w, &p, b"hers", 1);
    std::fs::write(&p, b"his edit").unwrap();
    let u = r.undo(Some(&a), "u1", 2).unwrap();
    assert_eq!(u["ok"], json!(false));
    assert!(u["text"].as_str().unwrap().contains("changed since"));
    assert_eq!(std::fs::read(&p).unwrap(), b"his edit");
    assert_eq!(u["undid"], Value::Null);
    std::fs::write(&p, b"hers").unwrap();
    assert_eq!(r.undo(Some(&a), "u2", 3).unwrap()["ok"], json!(true));
}

#[test]
fn a_file_outside_her_places_is_never_touched_by_an_undo() {
    let w = World::new("places");
    let r = w.runner();
    let p = w.d.join("elsewhere/x.txt");
    let a = write_file(&w, &p, b"hers", 1);
    assert!(r.undo(Some(&a), "u1", 2).unwrap_err().contains("off limits"));
    assert!(p.exists());
}

/// The hash a decision is bound to, pinned to a known value: the server (lucina-rs `intent_hash`) checks the same input gives
/// the same hex. serde_json sorts maps while no crate in the build turns on `preserve_order`.
#[test]
fn the_call_hash_of_a_known_call() {
    let h = call_hash("notes.trash", &json!({"id": "n_1"}));
    assert_eq!(h, "7909bb75cf43dd51cfc500ddbabe49e7729582f224f8238afc37d830bf36a55c");
    // Key order in the call does not change it.
    let a: Value = serde_json::from_str(r#"{"b": 1, "a": 2}"#).unwrap();
    let b: Value = serde_json::from_str(r#"{"a": 2, "b": 1}"#).unwrap();
    assert_eq!(call_hash("x.y", &a), call_hash("x.y", &b));
}


/// An app that lists `listing` after `delay` seconds, laid out as the image has it (exec in its manifest).
fn lister(d: &Path, id: &str, delay: u32, listing: &str) -> Manifest {
    let exec = d.join(format!("{id}.sh"));
    std::fs::write(d.join(format!("{id}.json")), listing).unwrap();
    std::fs::write(&exec, format!("#!/bin/sh\nsleep {delay}\ncat {}\n", d.join(format!("{id}.json")).display())).unwrap();
    std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o755)).unwrap();
    Manifest::parse(&format!("id = {id}\ntitle = {id}\nexec = {}\nintents = {id}.go\n", exec.display())).unwrap()
}

fn go(id: &str) -> String {
    let spec = IntentSpec { name: "x.go", description: "Go.", args: &[], effect: Effect::Read, shows: false, inverse: None };
    let mut v = spec.to_json();
    v["name"] = json!(format!("{id}.go"));
    v["app"] = json!(id);
    Value::Array(vec![v]).to_string()
}

#[test]
fn the_apps_are_asked_at_once_and_a_slow_one_keeps_its_last_listing() {
    let d = dir("parallel");
    let apps: Vec<Manifest> = (0..4).map(|i| lister(&d, &format!("fast{i}"), 0, &go(&format!("fast{i}")))).collect();
    let slow_ok = lister(&d, "slow", 0, &go("slow"));
    let mut all = apps.clone();
    all.push(slow_ok);
    let first = Catalog::load_keeping(&all, Duration::from_secs(3), None);
    assert!(first.find("slow.go").is_some() && first.find("fast3.go").is_some(), "{:?}", first.problems);
    // Now the slow one takes 5 s, the others answer at once: the whole listing takes about the limit, not the sum.
    let slow = lister(&d, "slow", 5, &go("slow"));
    let mut all = apps.clone();
    all.push(slow.clone());
    let t = Instant::now();
    let c = Catalog::load_keeping(&all, Duration::from_millis(1500), Some(&first));
    assert!(t.elapsed() < Duration::from_millis(2500), "{:?}", t.elapsed());
    for i in 0..4 {
        assert!(c.find(&format!("fast{i}.go")).is_some(), "{:?}", c.problems);
    }
    assert!(c.find("slow.go").is_some(), "kept from the last catalog");
    assert!(c.problems.iter().any(|p| p.starts_with("slow: ") && p.contains("1500 ms") && p.ends_with("its last listing is kept")), "{:?}", c.problems);
    // With no last catalog, it is simply not there, and said so.
    let c = Catalog::load_keeping(&[slow], Duration::from_millis(500), None);
    assert!(c.find("slow.go").is_none());
    assert!(c.problems[0].starts_with("slow: ") && c.problems[0].contains("500 ms") && !c.problems[0].contains("kept"), "{:?}", c.problems);
}
