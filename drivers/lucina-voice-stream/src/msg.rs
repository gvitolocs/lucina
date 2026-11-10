//! The JSON on the voice socket (LucinaVoice protocol sections 2 and 10): the `start` this client sends and the messages the
//! server sends back. Audio frames are binary and not here. A message of a type this client does not know is [`Server::Unknown`]
//! and is ignored; so is a key it does not know.
use serde_json::{json, Map, Value};

pub const SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// A spoken (or shown) reply.
    Command,
    /// A voice note: a chat message with the audio and its transcript.
    Note,
}

/// What `start` asks for. Juno sends 16 kHz PCM, `channel: "web"`, and the main chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    pub purpose: Purpose,
    pub source: String,
    pub lang: Option<String>,
    pub session_id: Option<String>,
    pub reply_to: Option<String>,
}

impl Start {
    /// A voice note: `mode: "ptt"`, `source: "juno-chat"`.
    pub fn note() -> Start {
        Start { purpose: Purpose::Note, source: "juno-chat".into(), lang: None, session_id: None, reply_to: None }
    }

    /// A voice command: `mode: "vad"` (the server ends the turn on a pause), the answer as text and as speech.
    pub fn command() -> Start {
        Start { purpose: Purpose::Command, source: "juno-chat".into(), lang: None, session_id: None, reply_to: None }
    }

    pub fn to_json(&self) -> String {
        let mut m = Map::new();
        m.insert("type".into(), json!("start"));
        m.insert("codec".into(), json!("pcm16"));
        m.insert("rate".into(), json!(SAMPLE_RATE));
        m.insert("thread".into(), json!("main"));
        m.insert("channel".into(), json!("web"));
        if let Some(l) = &self.lang {
            m.insert("lang".into(), json!(l));
        }
        if let Some(s) = &self.session_id {
            m.insert("sessionId".into(), json!(s));
        }
        match self.purpose {
            Purpose::Note => {
                m.insert("mode".into(), json!("ptt"));
                m.insert("purpose".into(), json!("note"));
                m.insert("source".into(), json!(self.source));
                if let Some(r) = &self.reply_to {
                    m.insert("replyTo".into(), json!(r));
                }
            }
            Purpose::Command => {
                m.insert("mode".into(), json!("vad"));
                m.insert("purpose".into(), json!("command"));
                m.insert("text".into(), json!(true));
                m.insert("speak".into(), json!(true));
            }
        }
        Value::Object(m).to_string()
    }
}

/// `{"type":"end"}`: the owner let go.
pub const END: &str = r#"{"type":"end"}"#;
/// `{"type":"cancel"}`: drop the turn.
pub const CANCEL: &str = r#"{"type":"cancel"}"#;

/// A message from the server.
#[derive(Debug, Clone, PartialEq)]
pub enum Server {
    /// The turn is open. A server from before section 10 sends none of the keys, and `codec` missing is how that is told.
    Listening { purpose: Option<String>, codec: Option<String>, rate: Option<u32>, turn: Option<String> },
    /// The server heard enough (VAD end or the cap): stop sending.
    Endpoint,
    Transcript { text: String },
    Delta { text: String },
    Answer { text: String, replaces: bool },
    /// Speech follows as binary frames of s16le mono at `rate`.
    Audio { rate: u32 },
    Reply { text: String, share_id: Option<String>, item_id: Option<String> },
    Done { share_id: Option<String>, ms: Vec<(String, u64)> },
    Error { code: String, message: String, share_id: Option<String> },
    /// A voice note is stored (a chat message now); the device is free.
    Note { share_id: String, user_item_id: Option<String> },
    Unknown,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

/// Reads one text frame. `None` when it is not a JSON object with a `type`.
pub fn parse(text: &str) -> Option<Server> {
    let v: Value = serde_json::from_str(text).ok()?;
    let kind = v.get("type")?.as_str()?;
    Some(match kind {
        "listening" => Server::Listening {
            purpose: s(&v, "purpose"),
            codec: s(&v, "codec"),
            rate: v.get("rate").and_then(Value::as_u64).and_then(|r| u32::try_from(r).ok()),
            turn: s(&v, "turn"),
        },
        "endpoint" => Server::Endpoint,
        "transcript" => Server::Transcript { text: s(&v, "text").unwrap_or_default() },
        "delta" => Server::Delta { text: s(&v, "text").unwrap_or_default() },
        "answer" => Server::Answer {
            text: s(&v, "text").unwrap_or_default(),
            replaces: v.get("replaces").and_then(Value::as_bool).unwrap_or(false),
        },
        "audio" => Server::Audio { rate: v.get("rate").and_then(Value::as_u64).and_then(|r| u32::try_from(r).ok()).unwrap_or(24_000) },
        "reply" => Server::Reply { text: s(&v, "text").unwrap_or_default(), share_id: s(&v, "shareId"), item_id: s(&v, "itemId") },
        "done" => {
            let mut ms = Vec::new();
            if let Some(o) = v.get("ms").and_then(Value::as_object) {
                for (k, x) in o {
                    if let Some(n) = x.as_u64().or_else(|| x.as_f64().map(|f| f.max(0.0) as u64)) {
                        ms.push((k.clone(), n));
                    }
                }
            }
            Server::Done { share_id: s(&v, "shareId"), ms }
        }
        "error" => Server::Error {
            code: s(&v, "code").unwrap_or_else(|| "error".into()),
            message: s(&v, "message").unwrap_or_default(),
            share_id: s(&v, "shareId"),
        },
        "note" => match s(&v, "shareId") {
            Some(share_id) => Server::Note { share_id, user_item_id: s(&v, "userItemId") },
            None => Server::Unknown,
        },
        _ => Server::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(j: &str) -> Value {
        serde_json::from_str(j).unwrap()
    }

    #[test]
    fn a_note_start_has_the_keys_of_section_10() {
        let v = keys(&Start::note().to_json());
        assert_eq!(
            v,
            json!({"type":"start","mode":"ptt","purpose":"note","codec":"pcm16","rate":16000,"thread":"main","channel":"web","source":"juno-chat"})
        );
    }

    #[test]
    fn a_command_start_asks_for_text_and_speech() {
        let v = keys(&Start::command().to_json());
        assert_eq!(
            v,
            json!({"type":"start","mode":"vad","purpose":"command","codec":"pcm16","rate":16000,"thread":"main","channel":"web","text":true,"speak":true})
        );
    }

    #[test]
    fn optional_keys_appear_only_when_set() {
        let mut st = Start::note();
        st.lang = Some("it".into());
        st.session_id = Some("s_1".into());
        st.reply_to = Some("itm_9".into());
        let v = keys(&st.to_json());
        assert_eq!(v["lang"], "it");
        assert_eq!(v["sessionId"], "s_1");
        assert_eq!(v["replyTo"], "itm_9");
        let c = keys(&Start { reply_to: Some("x".into()), ..Start::command() }.to_json());
        assert!(c.get("replyTo").is_none() && c.get("source").is_none());
    }

    #[test]
    fn every_server_message_parses() {
        assert_eq!(
            parse(r#"{"type":"listening","purpose":"note","codec":"pcm16","rate":16000,"turn":"t1"}"#),
            Some(Server::Listening { purpose: Some("note".into()), codec: Some("pcm16".into()), rate: Some(16000), turn: Some("t1".into()) })
        );
        assert_eq!(
            parse(r#"{"type":"listening"}"#),
            Some(Server::Listening { purpose: None, codec: None, rate: None, turn: None })
        );
        assert_eq!(parse(r#"{"type":"endpoint"}"#), Some(Server::Endpoint));
        assert_eq!(parse(r#"{"type":"transcript","text":"ciao"}"#), Some(Server::Transcript { text: "ciao".into() }));
        assert_eq!(parse(r#"{"type":"delta","text":"Ci"}"#), Some(Server::Delta { text: "Ci".into() }));
        assert_eq!(
            parse(r#"{"type":"answer","text":"Ciao","replaces":true}"#),
            Some(Server::Answer { text: "Ciao".into(), replaces: true })
        );
        assert_eq!(parse(r#"{"type":"audio","rate":24000}"#), Some(Server::Audio { rate: 24000 }));
        assert_eq!(
            parse(r#"{"type":"reply","text":"Hi"}"#),
            Some(Server::Reply { text: "Hi".into(), share_id: None, item_id: None })
        );
        assert_eq!(
            parse(r#"{"type":"reply","shareId":"s1","text":"Hi","itemId":"itm_2"}"#),
            Some(Server::Reply { text: "Hi".into(), share_id: Some("s1".into()), item_id: Some("itm_2".into()) })
        );
        match parse(r#"{"type":"done","shareId":"s1","ms":{"stt":80,"note":300,"reply":2100,"total":2100}}"#) {
            Some(Server::Done { share_id, ms }) => {
                assert_eq!(share_id.as_deref(), Some("s1"));
                assert!(ms.contains(&("reply".to_owned(), 2100)) && ms.len() == 4);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse(r#"{"type":"error","code":"note_failed","message":"share refused (413)"}"#),
            Some(Server::Error { code: "note_failed".into(), message: "share refused (413)".into(), share_id: None })
        );
        assert_eq!(
            parse(r#"{"type":"note","shareId":"s1","userItemId":"itm_1"}"#),
            Some(Server::Note { share_id: "s1".into(), user_item_id: Some("itm_1".into()) })
        );
    }

    #[test]
    fn unknown_types_and_keys_are_ignored_and_garbage_is_none() {
        assert_eq!(parse(r#"{"type":"future","x":1}"#), Some(Server::Unknown));
        assert_eq!(parse(r#"{"type":"transcript","text":"a","extra":[1]}"#), Some(Server::Transcript { text: "a".into() }));
        assert_eq!(parse(r#"{"type":"note"}"#), Some(Server::Unknown));
        assert_eq!(parse("not json"), None);
        assert_eq!(parse(r#"{"text":"no type"}"#), None);
    }
}
