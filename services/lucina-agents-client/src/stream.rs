//! The stream's blocks as events: what `GET /v1/agents/stream` sends, parsed once here so both apps handle it the same.

use lucina_account::sse::Block;
use serde_json::{Value, json};

/// A block as an event value: `stream.reset` and frames with a `type` pass, anything else is a plain change (`None`).
pub fn block_event(b: &Block) -> Option<Value> {
    if b.event == "stream.reset" {
        return Some(json!({"type": "stream.reset"}));
    }
    match serde_json::from_str::<Value>(&b.data) {
        Ok(v) if v["type"].is_string() => Some(v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_become_events_or_changes() {
        let reset = Block {
            event: "stream.reset".into(),
            data: String::new(),
            id: Some("1".into()),
        };
        assert_eq!(block_event(&reset), Some(json!({"type": "stream.reset"})));
        let typed = Block {
            event: "message".into(),
            data: r#"{"type":"agent_state","agent":{}}"#.into(),
            id: Some("2".into()),
        };
        assert_eq!(
            block_event(&typed).map(|v| v["type"].clone()),
            Some(json!("agent_state"))
        );
        let untyped = Block {
            event: "message".into(),
            data: r#"{"hello":1}"#.into(),
            id: None,
        };
        assert_eq!(block_event(&untyped), None);
        let junk = Block {
            event: "message".into(),
            data: "not json".into(),
            id: None,
        };
        assert_eq!(block_event(&junk), None);
    }
}
