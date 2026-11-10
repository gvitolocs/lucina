//! What an app declares about its intents, what a call carries, and what it answers. The shapes follow Apple's App Intents
//! (typed parameters, `AppEntity` results, an intent that opens its app or not) and Android's AppFunctions (a schema the system
//! indexes, called by an agent with permission); the effect class and the undo record are ours. The JSON is the protocol of
//! docs/research/juno-intents.md.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::rules::{self, ArgRule, Effect, Entry, Given, Kind, State, WriteKind};

/// One argument of an intent.
#[derive(Clone, Copy, Debug)]
pub struct ArgSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub required: bool,
    /// For texts and paths: the longest, in bytes.
    pub max_len: u64,
    /// For numbers.
    pub min: i64,
    pub max: i64,
}

impl ArgSpec {
    pub const fn text(name: &'static str, description: &'static str, required: bool, max_len: u64) -> ArgSpec {
        ArgSpec { name, description, kind: Kind::Text, required, max_len, min: 0, max: 0 }
    }
    pub const fn path(name: &'static str, description: &'static str, required: bool) -> ArgSpec {
        ArgSpec { name, description, kind: Kind::Path, required, max_len: rules::MAX_PATH as u64, min: 0, max: 0 }
    }
    pub const fn id(name: &'static str, description: &'static str, required: bool) -> ArgSpec {
        ArgSpec { name, description, kind: Kind::Id, required, max_len: rules::MAX_ID as u64, min: 0, max: 0 }
    }
    pub const fn int(name: &'static str, description: &'static str, required: bool, min: i64, max: i64) -> ArgSpec {
        ArgSpec { name, description, kind: Kind::Int, required, max_len: 0, min, max }
    }
    pub const fn flag(name: &'static str, description: &'static str) -> ArgSpec {
        ArgSpec { name, description, kind: Kind::Bool, required: false, max_len: 0, min: 0, max: 0 }
    }

    fn info(&self) -> ArgInfo {
        ArgInfo { name: self.name.to_string(), kind: self.kind, required: self.required, max_len: self.max_len, min: self.min, max: self.max }
    }

    fn schema(&self) -> Value {
        let mut s = match self.kind {
            Kind::Text => json!({"type": "string", "maxLength": self.max_len}),
            Kind::Path => json!({"type": "string", "maxLength": self.max_len, "pattern": "^/"}),
            Kind::Id => json!({"type": "string", "maxLength": self.max_len, "pattern": "^[A-Za-z0-9_.:-]+$"}),
            Kind::Int => json!({"type": "integer", "minimum": self.min, "maximum": self.max}),
            Kind::Bool => json!({"type": "boolean"}),
        };
        s["description"] = Value::from(self.description);
        // So that a reader of the listing gets the rules' type back without guessing it from the pattern.
        s["x-kind"] = Value::from(kind_name(self.kind));
        s
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Text => "text",
        Kind::Path => "path",
        Kind::Id => "id",
        Kind::Int => "int",
        Kind::Bool => "bool",
    }
}

/// One argument as a listing describes it: what [`ArgSpec`] is for an app, owned, for lucina-devd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgInfo {
    pub name: String,
    pub kind: Kind,
    pub required: bool,
    pub max_len: u64,
    pub min: i64,
    pub max: i64,
}

impl ArgInfo {
    fn rule(&self) -> ArgRule {
        ArgRule { kind: self.kind, required: self.required, max_len: self.max_len, min: self.min, max: self.max }
    }
}

/// The arguments of an intent from its JSON Schema, as [`IntentSpec::schema`] writes it.
pub fn args_of(schema: &Value) -> Result<Vec<ArgInfo>, String> {
    let props = schema["properties"].as_object().ok_or("the schema has no properties")?;
    let required: Vec<&str> = schema["required"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for (name, p) in props {
        let kind = match p["x-kind"].as_str() {
            Some("text") => Kind::Text,
            Some("path") => Kind::Path,
            Some("id") => Kind::Id,
            Some("int") => Kind::Int,
            Some("bool") => Kind::Bool,
            other => return Err(format!("{name}: unknown kind {other:?}")),
        };
        out.push(ArgInfo {
            name: name.clone(),
            kind,
            required: required.contains(&name.as_str()),
            max_len: p["maxLength"].as_u64().unwrap_or(0),
            min: p["minimum"].as_i64().unwrap_or(0),
            max: p["maximum"].as_i64().unwrap_or(0),
        });
    }
    Ok(out)
}

/// Whether `args` fits the arguments `spec` of `intent`: an object, every argument acceptable to [`rules::args_ok`], nothing the
/// intent does not take. The one check, whether an app or lucina-devd makes it.
pub fn check_args(intent: &str, spec: &[ArgInfo], args: &Value) -> Result<(), String> {
    let empty = Map::new();
    let obj = match args {
        Value::Object(o) => o,
        Value::Null => &empty,
        _ => return Err(format!("{intent}: the arguments are an object")),
    };
    let unknown: Vec<&String> = obj.keys().filter(|k| !spec.iter().any(|a| a.name == k.as_str())).collect();
    let rules_: Vec<ArgRule> = spec.iter().map(ArgInfo::rule).collect();
    let given_: Vec<Given> = spec.iter().map(|a| given(obj.get(&a.name))).collect();
    if rules::args_ok(&rules_, &given_, unknown.len() as u64) {
        return Ok(());
    }
    if let Some(k) = unknown.first() {
        let mut names: Vec<&str> = spec.iter().map(|a| a.name.as_str()).collect();
        names.sort_unstable();
        let names = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        return Err(format!("{intent}: no argument {k:?} (it takes {names})"));
    }
    let bad = spec.iter().zip(&given_).find(|(a, g)| !rules::arg_ok(a.rule(), **g)).map(|(a, _)| a);
    Err(match bad {
        Some(a) if matches!(given(obj.get(&a.name)), Given::Missing | Given::Null) => {
            format!("{intent}: {} is needed ({}); it takes {}", a.name, kind_words(a), takes(spec))
        }
        Some(a) => format!("{intent}: {} is not {}; it takes {}", a.name, kind_words(a), takes(spec)),
        None => format!("{intent}: bad arguments"),
    })
}

/// An intent as a listing (`<program> --intents`) describes it: [`IntentSpec`], owned.
#[derive(Clone, Debug, PartialEq)]
pub struct IntentInfo {
    pub app: String,
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub args: Vec<ArgInfo>,
    pub effect: Effect,
    pub shows: bool,
    pub inverse: Option<String>,
}

impl IntentInfo {
    pub fn from_json(v: &Value) -> Result<IntentInfo, String> {
        let name = v["name"].as_str().ok_or("an intent with no name")?.to_string();
        let app = name.split_once('.').map(|(a, _)| a.to_string()).ok_or_else(|| format!("{name}: not <app>.<verb>"))?;
        let effect = v["effect"].as_str().and_then(effect_from).ok_or_else(|| format!("{name}: no effect class"))?;
        let schema = v["args"].clone();
        let args = args_of(&schema).map_err(|e| format!("{name}: {e}"))?;
        Ok(IntentInfo {
            app,
            description: v["description"].as_str().unwrap_or("").to_string(),
            schema,
            args,
            effect,
            shows: v["shows"].as_bool().unwrap_or(false),
            inverse: v["inverse"].as_str().map(str::to_string),
            name,
        })
    }

    pub fn to_json(&self) -> Value {
        json!({
            "app": self.app,
            "name": self.name,
            "description": self.description,
            "args": self.schema,
            "effect": effect_name(self.effect),
            "shows": self.shows,
            "inverse": self.inverse,
        })
    }

    pub fn check(&self, args: &Value) -> Result<(), String> {
        check_args(&self.name, &self.args, args)
    }
}

/// One intent an app offers. `name` is `<app id>.<verb>`: `notes.create`, `player.play`.
#[derive(Clone, Copy, Debug)]
pub struct IntentSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub args: &'static [ArgSpec],
    pub effect: Effect,
    /// Brings the app to the front (`play`, `open`); `false` runs it without the screen (`search`, `create`).
    pub shows: bool,
    /// The intent that undoes this one, when it is a write the app can reverse (`notes.create` → `notes.trash`).
    pub inverse: Option<&'static str>,
}

pub fn effect_name(e: Effect) -> &'static str {
    match e {
        Effect::Read => "read",
        Effect::Write => "write",
        Effect::Destructive => "destructive",
        Effect::External => "external",
        Effect::OwnerOnly => "owner_only",
    }
}

pub fn effect_from(s: &str) -> Option<Effect> {
    Some(match s {
        "read" => Effect::Read,
        "write" => Effect::Write,
        "destructive" => Effect::Destructive,
        "external" => Effect::External,
        "owner_only" => Effect::OwnerOnly,
        _ => return None,
    })
}

impl IntentSpec {
    /// The app's id: the part of the name before the first `.`.
    pub fn app(&self) -> &'static str {
        self.name.split_once('.').map_or(self.name, |(a, _)| a)
    }

    /// The arguments as a JSON Schema object.
    pub fn schema(&self) -> Value {
        let props: Map<String, Value> = self.args.iter().map(|a| (a.name.to_string(), a.schema())).collect();
        let required: Vec<&str> = self.args.iter().filter(|a| a.required).map(|a| a.name).collect();
        json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
    }

    /// What `intents` lists for it.
    pub fn to_json(&self) -> Value {
        json!({
            "app": self.app(),
            "name": self.name,
            "description": self.description,
            "args": self.schema(),
            "effect": effect_name(self.effect),
            "shows": self.shows,
            "inverse": self.inverse,
        })
    }

    /// Whether `args` fits: an object, every argument acceptable to [`rules::args_ok`], nothing the intent does not take.
    pub fn check(&self, args: &Value) -> Result<(), String> {
        let infos: Vec<ArgInfo> = self.args.iter().map(ArgSpec::info).collect();
        check_args(self.name, &infos, args)
    }
}

/// The arguments as she should send them: `body (needed), title`, so that a refused call can be put right in one go.
fn takes(spec: &[ArgInfo]) -> String {
    let mut a: Vec<&ArgInfo> = spec.iter().collect();
    a.sort_by(|x, y| y.required.cmp(&x.required).then(x.name.cmp(&y.name)));
    let names: Vec<String> = a.iter().map(|a| if a.required { format!("{} (needed)", a.name) } else { a.name.clone() }).collect();
    if names.is_empty() { "no arguments".into() } else { names.join(", ") }
}

fn kind_words(a: &ArgInfo) -> String {
    match a.kind {
        Kind::Text => format!("a text of at most {} bytes", a.max_len),
        Kind::Path => "an absolute path with no empty, . or .. part".into(),
        Kind::Id => "an id (letters, digits, _ - . :)".into(),
        Kind::Int => format!("a whole number from {} to {}", a.min, a.max),
        Kind::Bool => "true or false".into(),
    }
}

/// A JSON value as the rules see it.
pub fn given(v: Option<&Value>) -> Given {
    match v {
        None => Given::Missing,
        Some(Value::Null) => Given::Null,
        Some(Value::Bool(_)) => Given::Bool,
        Some(Value::Number(n)) => n.as_i64().map_or(Given::Other, Given::Int),
        Some(Value::String(s)) => {
            let b = s.as_bytes();
            Given::Text { len: b.len() as u64, clean: rules::text_clean(b), path: rules::path_ok(b), id: rules::id_ok(b) }
        }
        Some(_) => Given::Other,
    }
}

/// A typed reference in a result, as App Intents' `AppEntity`: the same `type:id` is on the tree node that shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entity {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

impl Entity {
    pub fn new(kind: &str, id: &str, title: &str) -> Entity {
        Entity { kind: kind.into(), id: id.into(), title: title.into() }
    }
}

/// One call of an intent, as it reaches the app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// The bridge's call id, for the reply and the audit.
    pub call: String,
    pub name: String,
    #[serde(default)]
    pub args: Value,
    /// Set when this call undoes an earlier write: the journal's entry for it. The app checks the target with
    /// [`rules::undo_to`] before it changes anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<UndoEntry>,
}

/// [`rules::Entry`] on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UndoEntry {
    pub before: StateJson,
    pub after: StateJson,
    pub op: UndoOpJson,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateJson {
    pub exists: bool,
    /// As a string: JSON numbers lose bits past 2^53.
    #[serde(with = "stamp_str")]
    pub stamp: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoOpJson {
    TakeAway,
    PutBack,
    BringBack,
}

mod stamp_str {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl From<State> for StateJson {
    fn from(s: State) -> StateJson {
        StateJson { exists: s.exists, stamp: s.stamp }
    }
}
impl From<StateJson> for State {
    fn from(s: StateJson) -> State {
        State { exists: s.exists, stamp: s.stamp }
    }
}
impl From<Entry> for UndoEntry {
    fn from(e: Entry) -> UndoEntry {
        UndoEntry {
            before: e.before.into(),
            after: e.after.into(),
            op: match e.op {
                rules::UndoOp::TakeAway => UndoOpJson::TakeAway,
                rules::UndoOp::PutBack => UndoOpJson::PutBack,
                rules::UndoOp::BringBack => UndoOpJson::BringBack,
            },
        }
    }
}
impl From<UndoEntry> for Entry {
    fn from(e: UndoEntry) -> Entry {
        Entry {
            before: e.before.into(),
            after: e.after.into(),
            op: match e.op {
                UndoOpJson::TakeAway => rules::UndoOp::TakeAway,
                UndoOpJson::PutBack => rules::UndoOp::PutBack,
                UndoOpJson::BringBack => rules::UndoOp::BringBack,
            },
        }
    }
}

/// How to undo a write, as the journal keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Undo {
    /// Call the app's inverse intent with these arguments (the entry is added as [`Call::undo`]).
    Inverse { intent: String, args: Value },
    /// Put a file back: `before` is `blob:<sha256>` of its old bytes, or `None` when it was not there.
    File { path: String, before: Option<String> },
    /// Put a setting back.
    Setting { key: String, old: Value },
}

/// The write an intent made, so the journal can record it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Write {
    #[serde(with = "kind_str")]
    pub kind: WriteKind,
    /// What it wrote: `note:n_42`, `file:/data/home/a.txt`, `setting:zone`.
    pub target: String,
    pub before: StateJson,
    pub after: StateJson,
    pub undo: Undo,
}

mod kind_str {
    use crate::rules::WriteKind;
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &WriteKind, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match v {
            WriteKind::Create => "create",
            WriteKind::Change => "change",
            WriteKind::Remove => "remove",
        })
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<WriteKind, D::Error> {
        match String::deserialize(d)?.as_str() {
            "create" => Ok(WriteKind::Create),
            "change" => Ok(WriteKind::Change),
            "remove" => Ok(WriteKind::Remove),
            other => Err(serde::de::Error::custom(format!("no write kind {other:?}"))),
        }
    }
}

impl Write {
    /// The rules' undo entry, or why the write cannot have one.
    pub fn entry(&self) -> Result<Entry, String> {
        rules::entry_for(self.kind, self.before.into(), self.after.into())
            .ok_or_else(|| format!("{}: the states before and after do not fit a {:?}", self.target, self.kind))
    }
}

/// What an app answers to a [`Call`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    /// One or two sentences for her: what happened, or why not.
    pub text: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub value: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<Entity>,
    /// Set by a write that can be undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Write>,
}

impl Reply {
    pub fn done(text: impl Into<String>) -> Reply {
        Reply { ok: true, text: text.into(), ..Reply::default() }
    }
    pub fn failed(text: impl Into<String>) -> Reply {
        Reply { ok: false, text: text.into(), ..Reply::default() }
    }
    pub fn with_value(mut self, v: Value) -> Reply {
        self.value = v;
        self
    }
    pub fn with_entity(mut self, e: Entity) -> Reply {
        self.entities.push(e);
        self
    }
    pub fn with_write(mut self, w: Write) -> Reply {
        self.write = Some(w);
        self
    }
}

/// A stamp for a target's content: FNV-1a over its bytes (a note's `updated_at`, a setting's value), never 0, since 0 is "not
/// there". Not a security hash: it tells "changed since" apart, and the journal keeps file contents by SHA-256.
pub fn stamp_of(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if h == 0 { 1 } else { h }
}

/// The state of something that is there with this content.
pub fn there(content: &[u8]) -> State {
    State { exists: true, stamp: stamp_of(content) }
}

/// The state of something that is not there.
pub const GONE: State = State { exists: false, stamp: 0 };

#[cfg(test)]
mod tests {
    use super::*;

    const CREATE: IntentSpec = IntentSpec {
        name: "notes.create",
        description: "A new note.",
        args: &[ArgSpec::text("title", "Its title.", false, 200), ArgSpec::text("body", "Its text.", true, 20_000)],
        effect: Effect::Write,
        shows: false,
        inverse: Some("notes.trash"),
    };
    const PLAY: IntentSpec = IntentSpec {
        name: "player.play",
        description: "Play a file.",
        args: &[ArgSpec::path("path", "The file.", true), ArgSpec::int("at", "Start here, in seconds.", false, 0, 86_400)],
        effect: Effect::Write,
        shows: true,
        inverse: None,
    };

    #[test]
    fn the_schema_says_what_check_takes() {
        let s = CREATE.schema();
        assert_eq!(s["required"], json!(["body"]));
        assert_eq!(s["properties"]["title"]["maxLength"], json!(200));
        assert_eq!(s["additionalProperties"], json!(false));
        assert_eq!(CREATE.to_json()["effect"], json!("write"));
        assert_eq!(CREATE.app(), "notes");
    }

    #[test]
    fn check_takes_good_arguments_and_says_what_is_wrong() {
        assert!(CREATE.check(&json!({"body": "milk"})).is_ok());
        assert!(CREATE.check(&json!({"title": "Shop", "body": "milk"})).is_ok());
        assert!(CREATE.check(&json!({"title": null, "body": "milk"})).is_ok());
        assert!(CREATE.check(&json!({})).unwrap_err().contains("body is needed"));
        // The refusal says what the intent takes, so the next call can be right.
        assert_eq!(CREATE.check(&json!({"title": "x"})).unwrap_err(), "notes.create: body is needed (a text of at most 20000 bytes); it takes body (needed), title");
        assert!(CREATE.check(&json!({"body": "milk", "colour": "red"})).unwrap_err().contains("no argument \"colour\""));
        assert!(CREATE.check(&json!({"body": 3})).unwrap_err().contains("body is not a text"));
        assert!(CREATE.check(&json!({"body": "a\u{0}b"})).is_err());
        assert!(CREATE.check(&json!({"body": "x".repeat(20_001)})).is_err());
        assert!(CREATE.check(&json!(["milk"])).is_err());
        assert!(PLAY.check(&json!({"path": "/data/music/a.flac", "at": 30})).is_ok());
        for bad in ["a.flac", "/data/../etc/passwd", "/data//a", "/data/./a", "/data/", "/"] {
            assert!(PLAY.check(&json!({"path": bad})).is_err(), "{bad}");
        }
        assert!(PLAY.check(&json!({"path": "/data/a", "at": -1})).is_err());
        assert!(PLAY.check(&json!({"path": "/data/a", "at": 1.5})).is_err());
        assert!(PLAY.check(&json!({"path": "/data/.hidden/a..b"})).is_ok());
    }

    #[test]
    fn a_listing_reads_back_into_the_same_checks() {
        for spec in [CREATE, PLAY] {
            let info = IntentInfo::from_json(&spec.to_json()).unwrap();
            assert_eq!(info.name, spec.name);
            assert_eq!(info.effect, spec.effect);
            assert_eq!(info.args.len(), spec.args.len());
            for args in [json!({"body": "milk"}), json!({}), json!({"path": "/data/a", "at": 3}), json!({"path": "a"}), json!({"x": 1})] {
                assert_eq!(info.check(&args), spec.check(&args), "{} {args}", spec.name);
            }
        }
        assert!(IntentInfo::from_json(&json!({"name": "x.y", "effect": "loud", "args": {"properties": {}}})).is_err());
    }

    #[test]
    fn a_write_gives_the_rules_entry_and_travels_as_json() {
        let w = Write {
            kind: WriteKind::Create,
            target: "note:n_1".into(),
            before: GONE.into(),
            after: there(b"2026-11-04T10:00:00Z").into(),
            undo: Undo::Inverse { intent: "notes.trash".into(), args: json!({"id": "n_1"}) },
        };
        let e = w.entry().unwrap();
        assert_eq!(e.op, rules::UndoOp::TakeAway);
        let text = serde_json::to_string(&Reply::done("Made it.").with_write(w.clone())).unwrap();
        assert!(text.contains("\"kind\":\"create\""), "{text}");
        let back: Reply = serde_json::from_str(&text).unwrap();
        assert_eq!(back.write, Some(w));
        let bad = Write { kind: WriteKind::Change, before: GONE.into(), ..back.write.unwrap() };
        assert!(bad.entry().is_err());
        let wire: UndoEntry = e.into();
        assert_eq!(Entry::from(wire), e);
        assert!(serde_json::to_string(&wire).unwrap().contains("\"op\":\"take_away\""));
    }

    #[test]
    fn stamps_tell_contents_apart_and_are_never_zero() {
        assert_ne!(stamp_of(b"a"), stamp_of(b"b"));
        assert_ne!(stamp_of(b""), 0);
        assert!(there(b"x").exists);
    }
}
