//! A small client for the Agent Client Protocol, as `oc-cli acp` speaks it (opencode-rust main de4f5f5, README "ACP",
//! `crates/oc-cli/tests/acp.rs` shows the wire): JSON-RPC 2.0, one object per line on the child's stdio. The client
//! sends `initialize` (protocolVersion 1), `session/new {cwd, mcpServers: []}`, `session/prompt` and `session/cancel`,
//! and answers the agent's `session/request_permission` with `{outcome: {outcome: "selected", optionId}}`
//! (allow_once / reject_once) or `{outcome: {outcome: "cancelled"}}`. A reader thread turns the agent's lines into
//! [`AcpEvent`]s for the app thread, as the stream does for the server's events.
//!
//! The parsing and serialising are pure ([`parse_line`], [`request_line`], …), and the updates map onto the same store
//! rows the Agents app draws ([`apply_update`], [`apply_permission`]): `agent_message_chunk` → an assistant message,
//! `agent_thought_chunk` → reasoning, `tool_call` / `tool_call_update` → tool rows (title, kind, status, rawInput,
//! content), `session/request_permission` → a permission card whose `diff` content shows as numbered old/new rows.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, TryRecvError};

use lucina_agents::permission::{PermissionKind, PermissionRequest};
use lucina_agents::timeline::{
    TimelineItem, TimelineRow, ToolCall, ToolCallDetail, ToolCallStatus,
};
use lucina_agents_client::api::iso_now;
use lucina_agents_client::model::PermissionView;
use lucina_agents_client::store::{Store, Timeline, TlStatus};
use serde_json::{Map, Value, json};

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: i64 = 1;

/// One line from the agent, parsed.
#[derive(Clone, Debug, PartialEq)]
pub enum AcpEvent {
    /// A `session/update` notification: the `params.update` object.
    Update(Value),
    /// The agent's `session/request_permission`: its request id, the tool call it describes, the options.
    Permission {
        id: i64,
        tool_call: Value,
        options: Vec<Value>,
    },
    /// The answer to one of our requests: its id, and the `result` or `error`.
    Response {
        id: i64,
        result: Option<Value>,
        error: Option<Value>,
    },
    /// The agent's stdout closed.
    Closed,
}

// ---- The wire: pure parsing and serialising ----

/// One JSON-RPC line from the agent into an event. `None` for a line that is not JSON-RPC or not ours.
pub fn parse_line(line: &str) -> Option<AcpEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    if let Some(method) = v.get("method").and_then(Value::as_str) {
        let id = v.get("id").and_then(Value::as_i64).unwrap_or(-1);
        match method {
            "session/update" => Some(AcpEvent::Update(v["params"]["update"].clone())),
            "session/request_permission" => Some(AcpEvent::Permission {
                id,
                tool_call: v["params"]["toolCall"].clone(),
                options: v["params"]["options"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            }),
            _ => None,
        }
    } else if let Some(id) = v.get("id").and_then(Value::as_i64) {
        Some(AcpEvent::Response {
            id,
            result: v.get("result").cloned(),
            error: v.get("error").cloned(),
        })
    } else {
        None
    }
}

/// A request with an id, as one line (the trailing newline included).
pub fn request_line(id: i64, method: &str, params: Value) -> String {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": id, "method": method, "params": params,
    }))
    .unwrap_or_default();
    s.push('\n');
    s
}

/// A notification (no id), as one line.
pub fn notification_line(method: &str, params: Value) -> String {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "method": method, "params": params,
    }))
    .unwrap_or_default();
    s.push('\n');
    s
}

/// A response to one of the agent's requests, as one line.
pub fn response_line(id: i64, result: Value) -> String {
    let mut s = serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": id, "result": result,
    }))
    .unwrap_or_default();
    s.push('\n');
    s
}

/// `initialize` with protocolVersion 1.
pub fn initialize(id: i64) -> String {
    request_line(
        id,
        "initialize",
        json!({"protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {}}),
    )
}

/// `session/new {cwd, mcpServers: []}`.
pub fn new_session(id: i64, cwd: &Path) -> String {
    request_line(
        id,
        "session/new",
        json!({"cwd": cwd.to_string_lossy(), "mcpServers": []}),
    )
}

/// `session/prompt` with one text block.
pub fn prompt(id: i64, session_id: &str, text: &str) -> String {
    request_line(
        id,
        "session/prompt",
        json!({"sessionId": session_id, "prompt": [{"type": "text", "text": text}]}),
    )
}

/// `session/cancel` (a notification).
pub fn cancel(session_id: &str) -> String {
    notification_line("session/cancel", json!({"sessionId": session_id}))
}

/// The answer to a `session/request_permission`: allow_once or reject_once.
pub fn permission_result(id: i64, allow: bool) -> String {
    response_line(
        id,
        json!({"outcome": {"outcome": "selected", "optionId": if allow { "allow_once" } else { "reject_once" }}}),
    )
}

/// The answer that cancels a `session/request_permission` (the turn is being stopped).
pub fn permission_cancelled(id: i64) -> String {
    response_line(id, json!({"outcome": {"outcome": "cancelled"}}))
}

// ---- The client over a child's stdio ----

/// A running `oc-cli acp` child: requests go in on stdin, its lines come out as [`AcpEvent`]s on
/// [`Acp::try_recv`]. Dropping it stops the child.
pub struct Acp {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<AcpEvent>,
    next_id: i64,
    /// The session `session/new` made, once its answer arrived.
    pub session_id: Option<String>,
}

impl Acp {
    /// Spawn `cmd` with piped stdio and start the reader thread. The thread ends with [`AcpEvent::Closed`].
    pub fn spawn(cmd: &mut Command) -> std::io::Result<Acp> {
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = std::sync::mpsc::channel::<AcpEvent>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Some(ev) = parse_line(line.trim()) {
                            if tx.send(ev).is_err() {
                                break;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(AcpEvent::Closed);
        });
        Ok(Acp {
            child,
            stdin,
            rx,
            next_id: 1,
            session_id: None,
        })
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// One line to the agent.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.flush()
    }

    /// `initialize`.
    pub fn initialize(&mut self) -> std::io::Result<i64> {
        let id = self.next_id();
        self.send_line(&initialize(id))?;
        Ok(id)
    }

    /// `session/new` in `cwd`.
    pub fn new_session(&mut self, cwd: &Path) -> std::io::Result<i64> {
        let id = self.next_id();
        self.send_line(&new_session(id, cwd))?;
        Ok(id)
    }

    /// `session/prompt` on the current session. `Ok(None)` when there is no session yet.
    pub fn prompt(&mut self, text: &str) -> std::io::Result<Option<i64>> {
        let Some(session_id) = self.session_id.clone() else {
            return Ok(None);
        };
        let id = self.next_id();
        self.send_line(&prompt(id, &session_id, text))?;
        Ok(Some(id))
    }

    /// `session/cancel` on the current session.
    pub fn cancel(&mut self) -> std::io::Result<()> {
        if let Some(session_id) = self.session_id.clone() {
            self.send_line(&cancel(&session_id))?;
        }
        Ok(())
    }

    /// Answer an open `session/request_permission`: allow_once or reject_once.
    pub fn answer_permission(&mut self, request_id: i64, allow: bool) -> std::io::Result<()> {
        self.send_line(&permission_result(request_id, allow))
    }

    /// Answer an open `session/request_permission` with cancelled (the turn is being stopped).
    pub fn cancel_permission(&mut self, request_id: i64) -> std::io::Result<()> {
        self.send_line(&permission_cancelled(request_id))
    }

    /// The next event, if one has arrived.
    pub fn try_recv(&self) -> Option<AcpEvent> {
        match self.rx.try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(AcpEvent::Closed),
        }
    }

    /// Wait up to `d` for the next event (the tests drive the client with it).
    #[cfg(test)]
    pub fn recv_timeout(&self, d: std::time::Duration) -> Option<AcpEvent> {
        self.rx.recv_timeout(d).ok()
    }
}

impl Drop for Acp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ---- The updates onto the store's rows, as the Agents app draws them ----

/// The store's local agent: its timeline and snapshot under `id` (`local:<path>`), if not there yet.
pub fn ensure_local(store: &mut Store, id: &str, path: &Path, now: i64) {
    store
        .timelines
        .entry(id.to_owned())
        .or_insert_with(|| Timeline {
            epoch: "local".to_owned(),
            rows: Vec::new(),
            status: TlStatus::Ready,
            error: None,
        });
    store.snapshots.entry(id.to_owned()).or_insert_with(|| {
        serde_json::from_value(json!({
            "id": id, "provider": "oc-cli", "cwd": path.to_string_lossy(),
            "createdAt": iso_now(now), "updatedAt": iso_now(now), "lastUserMessageAt": null,
            "status": "idle",
            "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsDynamicModes": false,
                "supportsMcpServers": true, "supportsReasoningStream": true, "supportsToolInvocations": true},
            "currentModeId": null, "availableModes": [], "pendingPermissions": [], "persistence": null,
            "title": lucina_agents_client::model::base_name(&path.to_string_lossy()),
        }))
        .expect("the local snapshot's fields")
    });
}

fn status_of(s: &str) -> ToolCallStatus {
    match s {
        "completed" => ToolCallStatus::Completed,
        "failed" => ToolCallStatus::Failed,
        "canceled" | "cancelled" => ToolCallStatus::Canceled,
        _ => ToolCallStatus::Running,
    }
}

fn chunk_text(update: &Value) -> String {
    update["content"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// One `session/update` onto the local agent's timeline rows: `agent_message_chunk` → assistant text,
/// `agent_thought_chunk` → reasoning, `user_message_chunk` → the owner's message, `tool_call` / `tool_call_update` →
/// tool rows (title, kind, status, rawInput, content). Unknown updates are ignored.
pub fn apply_update(store: &mut Store, id: &str, update: &Value, now: i64) {
    let Some(tl) = store.timelines.get_mut(id) else {
        return;
    };
    let seq = tl.rows.last().map_or(1, |r| r.seq + 1);
    let timestamp = iso_now(now);
    match update["sessionUpdate"].as_str().unwrap_or("") {
        "user_message_chunk" => tl.rows.push(TimelineRow {
            seq,
            timestamp,
            item: TimelineItem::UserMessage {
                text: chunk_text(update),
                message_id: None,
                client_message_id: None,
            },
            turn_id: None,
        }),
        "agent_message_chunk" => tl.rows.push(TimelineRow {
            seq,
            timestamp,
            item: TimelineItem::AssistantMessage {
                text: chunk_text(update),
                message_id: None,
            },
            turn_id: None,
        }),
        "agent_thought_chunk" => tl.rows.push(TimelineRow {
            seq,
            timestamp,
            item: TimelineItem::Reasoning {
                text: chunk_text(update),
            },
            turn_id: None,
        }),
        "tool_call" => tl.rows.push(TimelineRow {
            seq,
            timestamp,
            item: tool_call_item(update, status_of(update["status"].as_str().unwrap_or(""))),
            turn_id: None,
        }),
        "tool_call_update" => {
            let call_id = update["toolCallId"].as_str().unwrap_or("");
            let status = status_of(update["status"].as_str().unwrap_or(""));
            let output = update
                .get("rawOutput")
                .cloned()
                .or_else(|| update.get("content").cloned())
                .unwrap_or(Value::Null);
            let found = tl
                .rows
                .iter_mut()
                .rev()
                .find(|r| matches!(&r.item, TimelineItem::ToolCall(c) if c.call_id == call_id));
            match found {
                Some(row) => {
                    if let TimelineItem::ToolCall(c) = &mut row.item {
                        c.status = status;
                        let input = match &c.detail {
                            ToolCallDetail::Unknown { input, .. } => input.clone(),
                            _ => Value::Null,
                        };
                        c.detail = ToolCallDetail::Unknown {
                            input,
                            output: output.clone(),
                        };
                    }
                }
                None => tl.rows.push(TimelineRow {
                    seq,
                    timestamp,
                    item: tool_call_item(update, status),
                    turn_id: None,
                }),
            }
        }
        _ => {}
    }
}

/// The tool row of a `tool_call` update: the title as the name, the kind in the metadata, rawInput as the input and
/// rawOutput (or the content) as the output.
fn tool_call_item(update: &Value, status: ToolCallStatus) -> TimelineItem {
    let call_id = update["toolCallId"].as_str().unwrap_or_default().to_owned();
    let title = update["title"].as_str().unwrap_or("tool").to_owned();
    let kind = update["kind"].as_str().unwrap_or_default().to_owned();
    let raw_input = update.get("rawInput").cloned().unwrap_or(Value::Null);
    let raw_output = update
        .get("rawOutput")
        .cloned()
        .or_else(|| update.get("content").cloned())
        .unwrap_or(Value::Null);
    let mut metadata = Map::new();
    if !kind.is_empty() {
        metadata.insert("kind".to_owned(), kind.into());
    }
    TimelineItem::ToolCall(Box::new(ToolCall {
        agent_message: None,
        call_id,
        name: title,
        detail: ToolCallDetail::Unknown {
            input: raw_input,
            output: raw_output,
        },
        metadata: Some(metadata),
        status,
        error: Value::Null,
    }))
}

/// A `session/request_permission` onto the store: a pending permission card for the local agent, and the snapshot's
/// pending request carrying the tool call's `content` (whose `{"type":"diff", path, oldText, newText}` block the card
/// shows as numbered old/new rows).
pub fn apply_permission(store: &mut Store, id: &str, request_id: i64, tool_call: &Value, now: i64) {
    let rid = request_id.to_string();
    let title = tool_call["title"].as_str().unwrap_or("Tool").to_owned();
    let kind = tool_call["kind"].as_str().unwrap_or("tool").to_owned();
    let description = tool_call["content"]
        .as_array()
        .and_then(|c| {
            c.iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .map(str::to_owned);
    // The card's body: the content array (with the diff block), else the raw input.
    let mut details = Map::new();
    if let Some(c) = tool_call.get("content") {
        details.insert("content".to_owned(), c.clone());
    } else if let Some(ri) = tool_call.get("rawInput").and_then(Value::as_object) {
        details = ri.clone();
    }
    let p = PermissionView {
        id: rid.clone(),
        kind: "agent".to_owned(),
        agent_id: Some(id.to_owned()),
        source: None,
        name: Some(kind.clone()),
        title: title.clone(),
        description,
        details: Some(details.clone()),
        status: "pending".to_owned(),
        created_at: iso_now(now),
        expires_at: iso_now(now + 600_000),
        resolution: None,
        approval_id: None,
    };
    store.permissions.insert(p.key(), p);
    let snap = store.snapshots.entry(id.to_owned()).or_insert_with(|| {
        serde_json::from_value(json!({
            "id": id, "provider": "oc-cli", "cwd": "",
            "createdAt": iso_now(now), "updatedAt": iso_now(now), "lastUserMessageAt": null,
            "status": "idle",
            "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsDynamicModes": false,
                "supportsMcpServers": true, "supportsReasoningStream": true, "supportsToolInvocations": true},
            "currentModeId": null, "availableModes": [], "pendingPermissions": [], "persistence": null,
        }))
        .expect("the local snapshot's fields")
    });
    // The request the card reads the diff from: the content array, else the raw input.
    let input = if tool_call.get("content").is_some() {
        let mut m = Map::new();
        m.insert("content".to_owned(), tool_call["content"].clone());
        m
    } else {
        tool_call["rawInput"]
            .as_object()
            .cloned()
            .unwrap_or_default()
    };
    snap.pending_permissions.push(PermissionRequest {
        id: rid,
        provider: "oc-cli".to_owned(),
        name: kind,
        kind: PermissionKind::Tool,
        title: Some(title),
        description: None,
        input: Some(input),
        detail: Some(ToolCallDetail::Unknown {
            input: tool_call.get("rawInput").cloned().unwrap_or(Value::Null),
            output: Value::Null,
        }),
        suggestions: None,
        actions: None,
        metadata: None,
    });
}

/// The owner answered a permission: the card says how it ended and stays (the app adds its key to `pane.seen`).
pub fn answer_permission(store: &mut Store, id: &str, request_id: i64, allow: bool) {
    let key = format!("{id}/{request_id}");
    if let Some(p) = store.permissions.get_mut(&key) {
        p.status = if allow { "allowed" } else { "denied" }.to_owned();
    }
    if let Some(s) = store.snapshots.get_mut(id) {
        let rid = request_id.to_string();
        s.pending_permissions.retain(|r| r.id != rid);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lucina_agents_client::pages::{Act, Line, chat};
    use std::collections::{BTreeMap, HashSet};
    use std::path::PathBuf;
    use std::time::Duration;

    const NOW: i64 = 1_793_620_800_000;

    fn store_with_local() -> Store {
        let mut store = Store::default();
        ensure_local(&mut store, "local:/w", Path::new("/w"), NOW);
        store
    }

    // ---- The wire ----

    #[test]
    fn every_kind_of_line_parses() {
        let update = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}}}}"#;
        assert_eq!(
            parse_line(update),
            Some(AcpEvent::Update(
                json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hi"}})
            ))
        );
        let req = r#"{"jsonrpc":"2.0","id":42,"method":"session/request_permission","params":{"sessionId":"s","toolCall":{"toolCallId":"c1","title":"edit"},"options":[{"optionId":"allow_once"}]}}"#;
        assert_eq!(
            parse_line(req),
            Some(AcpEvent::Permission {
                id: 42,
                tool_call: json!({"toolCallId":"c1","title":"edit"}),
                options: vec![json!({"optionId":"allow_once"})],
            })
        );
        let resp = r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#;
        assert_eq!(
            parse_line(resp),
            Some(AcpEvent::Response {
                id: 3,
                result: Some(json!({"stopReason":"end_turn"})),
                error: None,
            })
        );
        let err = r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32601,"message":"no"}}"#;
        assert_eq!(
            parse_line(err),
            Some(AcpEvent::Response {
                id: 9,
                result: None,
                error: Some(json!({"code":-32601,"message":"no"})),
            })
        );
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line(r#"{"jsonrpc":"2.0","method":"fs/read"}"#), None);
        assert_eq!(parse_line(r#"{"jsonrpc":"2.0","result":{}}"#), None);
    }

    #[test]
    fn the_requests_serialise_as_the_wire_shows() {
        let line = initialize(1);
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}})
        );
        let v: Value =
            serde_json::from_str(new_session(2, Path::new("/data/projects/demo")).trim_end())
                .unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/data/projects/demo","mcpServers":[]}})
        );
        let v: Value = serde_json::from_str(prompt(3, "sess_1", "fix it").trim_end()).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"sess_1","prompt":[{"type":"text","text":"fix it"}]}})
        );
        // session/cancel is a notification: no id.
        let v: Value = serde_json::from_str(cancel("sess_1").trim_end()).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"sess_1"}})
        );
        assert!(v.get("id").is_none());
        let v: Value = serde_json::from_str(permission_result(42, true).trim_end()).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":42,"result":{"outcome":{"outcome":"selected","optionId":"allow_once"}}})
        );
        let v: Value = serde_json::from_str(permission_result(42, false).trim_end()).unwrap();
        assert_eq!(v["result"]["outcome"]["optionId"], json!("reject_once"));
        let v: Value = serde_json::from_str(permission_cancelled(42).trim_end()).unwrap();
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":42,"result":{"outcome":{"outcome":"cancelled"}}})
        );
        // A serialised request parses back as a response would not: method lines are not responses.
        assert!(matches!(
            parse_line(&request_line(7, "session/prompt", json!({}))),
            None
        ));
    }

    // ---- The update mapping ----

    #[test]
    fn each_update_kind_maps_to_its_row() {
        let mut store = store_with_local();
        let id = "local:/w";
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix it"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"one"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"two"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"tool_call","toolCallId":"call_1","title":"edit","kind":"edit","status":"pending","rawInput":{"path":"a.txt"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"tool_call","toolCallId":"call_2","title":"bash","kind":"execute","status":"pending","rawInput":{"command":"ls"}}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","rawOutput":{"output":"done"},"content":[{"type":"content","content":{"type":"text","text":"done"}}]}),
            NOW,
        );
        apply_update(
            &mut store,
            id,
            &json!({"sessionUpdate":"something_new"}),
            NOW,
        );
        let rows = &store.timelines.get(id).unwrap().rows;
        assert_eq!(
            rows.len(),
            6,
            "the unknown update adds nothing, the update folds in"
        );
        assert_eq!(rows[0].seq, 1);
        assert!(
            matches!(&rows[0].item, TimelineItem::UserMessage { text, .. } if text == "fix it")
        );
        assert!(matches!(&rows[1].item, TimelineItem::Reasoning { text } if text == "hmm"));
        assert!(
            matches!(&rows[2].item, TimelineItem::AssistantMessage { text, .. } if text == "one")
        );
        assert!(
            matches!(&rows[3].item, TimelineItem::AssistantMessage { text, .. } if text == "two")
        );
        match &rows[4].item {
            TimelineItem::ToolCall(c) => {
                assert_eq!(c.call_id, "call_1");
                assert_eq!(c.name, "edit");
                assert_eq!(
                    c.status,
                    ToolCallStatus::Completed,
                    "the update folds into the same call"
                );
                assert_eq!(c.metadata.as_ref().unwrap()["kind"], json!("edit"));
                assert_eq!(
                    c.detail,
                    ToolCallDetail::Unknown {
                        input: json!({"path":"a.txt"}),
                        output: json!({"output":"done"}),
                    }
                );
            }
            other => panic!("a tool call: {other:?}"),
        }
        match &rows[5].item {
            TimelineItem::ToolCall(c) => {
                assert_eq!(c.call_id, "call_2");
                assert_eq!(c.status, ToolCallStatus::Running, "pending runs");
            }
            other => panic!("a tool call: {other:?}"),
        }
        // The projection the Agents app draws merges the two message chunks into one entry.
        let entries = lucina_agents::projection::project_rows(rows);
        let texts: Vec<&str> = entries
            .iter()
            .filter_map(|e| match &e.item {
                TimelineItem::AssistantMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["onetwo"]);
    }

    #[test]
    fn a_permission_with_a_diff_maps_to_a_card_whose_diff_shows_as_numbered_rows() {
        let mut store = store_with_local();
        let id = "local:/w";
        let tool_call = json!({
            "toolCallId": "call_1", "title": "edit", "kind": "edit", "status": "pending",
            "rawInput": {"path": "/w/a.txt", "oldString": "bug", "newString": "fix"},
            "content": [
                {"type": "text", "text": "Fixing the bug"},
                {"type": "diff", "path": "/w/a.txt", "oldText": "the bug\n", "newText": "the fix\n"},
            ],
        });
        apply_permission(&mut store, id, 100, &tool_call, NOW);
        let key = format!("{id}/100");
        assert_eq!(store.permissions.get(&key).unwrap().status, "pending");
        assert_eq!(
            store.snapshots.get(id).unwrap().pending_permissions.len(),
            1
        );
        let lines = chat(
            &store,
            id,
            &HashSet::new(),
            &[key.clone()],
            &BTreeMap::new(),
            &HashSet::new(),
            NOW,
        );
        let first_diff = lines
            .iter()
            .position(|l| matches!(l, Line::Diff(_)))
            .expect("the diff rows");
        let allow = lines
            .iter()
            .position(|l| matches!(l, Line::Button { act: Act::Answer { key: k, allow: true }, .. } if *k == key))
            .expect("Allow");
        let deny = lines
            .iter()
            .position(|l| matches!(l, Line::Button { act: Act::Answer { key: k, allow: false }, .. } if *k == key))
            .expect("Deny");
        assert!(
            first_diff < allow && allow < deny,
            "the diff is above Allow and Deny"
        );
        let rows: Vec<(&str, &str, &str)> = lines
            .iter()
            .filter_map(|l| match l {
                Line::Diff(d) => Some((d.kind.as_str(), d.n.as_str(), d.content.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rows,
            [("remove", "1", "the bug"), ("add", "1", "the fix")],
            "numbered old/new rows"
        );
        let text = format!("{lines:?}");
        assert!(text.contains("/w/a.txt"), "the path is shown: {text}");
        assert!(
            text.contains("Fixing the bug"),
            "the text content is the description: {text}"
        );
        // Answering: the card says how it ended and the pending request is gone.
        answer_permission(&mut store, id, 100, true);
        assert_eq!(store.permissions.get(&key).unwrap().status, "allowed");
        assert!(
            store
                .snapshots
                .get(id)
                .unwrap()
                .pending_permissions
                .is_empty()
        );
        let lines = chat(
            &store,
            id,
            &HashSet::new(),
            &[key.clone()],
            &BTreeMap::new(),
            &HashSet::new(),
            NOW,
        );
        let text = format!("{lines:?}");
        assert!(text.contains("Allowed"), "{text}");
        assert!(
            !lines.iter().any(|l| matches!(l, Line::Button { .. })),
            "no buttons once answered: {text}"
        );
    }

    // ---- A whole turn against a fake agent process ----

    /// The fake `oc-cli acp`: answers initialize and session/new, streams a turn with a permission request that
    /// carries a diff, and logs every line it gets to `$FAKE_LOG`.
    pub(crate) const FAKE_AGENT: &str = r#"#!/bin/sh
log="${FAKE_LOG:-/dev/null}"
prompt_id=""
while IFS= read -r line; do
  echo "$line" >> "$log"
  id=$(printf '%s' "$line" | sed -n 's/.*"id": *\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":1,\"agentCapabilities\":{\"loadSession\":true},\"authMethods\":[]}}"
      ;;
    *'"method":"session/new"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"sessionId\":\"sess_fake\"}}"
      ;;
    *'"method":"session/prompt"'*)
      prompt_id=$id
      echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"fix it"}}}}'
      echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}}'
      echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"tool_call","toolCallId":"call_1","title":"edit","kind":"edit","status":"pending","rawInput":{"path":"a.txt"}}}}'
      echo '{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{"sessionId":"sess_fake","toolCall":{"toolCallId":"call_1","title":"edit","kind":"edit","status":"pending","rawInput":{"path":"a.txt"},"content":[{"type":"diff","path":"/w/a.txt","oldText":"the bug","newText":"the fix"}]},"options":[{"optionId":"allow_once","name":"Allow once","kind":"allow_once"},{"optionId":"reject_once","name":"Reject","kind":"reject_once"}]}}'
      ;;
    *'"outcome"'*) : ;;
    *'"method":"session/cancel"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$prompt_id,\"result\":{\"stopReason\":\"cancelled\"}}"
      ;;
  esac
  case "$line" in
    *'"optionId":"allow_once"'*)
      echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"tool_call_update","toolCallId":"call_1","status":"completed","rawOutput":{"output":"done"}}}}'
      echo '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"fixed"}}}}'
      echo "{\"jsonrpc\":\"2.0\",\"id\":$prompt_id,\"result\":{\"stopReason\":\"end_turn\"}}"
      ;;
    *'"optionId":"reject_once"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$prompt_id,\"result\":{\"stopReason\":\"end_turn\"}}"
      ;;
  esac
done
"#;

    pub(crate) fn fake_agent(log: &Path) -> Acp {
        let dir = tempdir("fake");
        let script = dir.join("fake-agent.sh");
        std::fs::write(&script, FAKE_AGENT).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = Command::new("sh");
        cmd.arg(&script).env("FAKE_LOG", log);
        Acp::spawn(&mut cmd).expect("the fake agent starts")
    }

    /// Read events until the response to `id` arrives; returns (events seen, the response).
    fn until_response(acp: &Acp, id: i64) -> (Vec<AcpEvent>, AcpEvent) {
        let mut seen = Vec::new();
        loop {
            match acp
                .recv_timeout(Duration::from_secs(10))
                .expect("an event within 10 s")
            {
                ev @ AcpEvent::Response { id: rid, .. } if rid == id => return (seen, ev),
                ev => seen.push(ev),
            }
        }
    }

    /// Read events until a `session/request_permission` arrives; returns (events before it, the request).
    fn until_permission(acp: &Acp) -> (Vec<AcpEvent>, AcpEvent) {
        let mut seen = Vec::new();
        loop {
            let ev = acp
                .recv_timeout(Duration::from_secs(10))
                .expect("an event within 10 s");
            if matches!(&ev, AcpEvent::Permission { .. }) {
                return (seen, ev);
            }
            seen.push(ev);
        }
    }

    #[test]
    fn a_whole_turn_against_a_fake_agent_end_to_end() {
        let log = tempdir("log").join("stdin.log");
        let mut acp = fake_agent(&log);
        // initialize, then session/new: the session id comes back in the response.
        let init = acp.initialize().unwrap();
        let (_, resp) = until_response(&acp, init);
        assert_eq!(
            resp,
            AcpEvent::Response {
                id: init,
                result: Some(
                    json!({"protocolVersion":1,"agentCapabilities":{"loadSession":true},"authMethods":[]})
                ),
                error: None,
            }
        );
        let new = acp.new_session(Path::new("/w")).unwrap();
        let (_, resp) = until_response(&acp, new);
        acp.session_id = Some(match &resp {
            AcpEvent::Response {
                result: Some(r), ..
            } => r["sessionId"].as_str().unwrap().to_owned(),
            other => panic!("session/new answered: {other:?}"),
        });
        assert_eq!(acp.session_id.as_deref(), Some("sess_fake"));
        // session/prompt: the turn streams, asks permission once, and ends after Allow.
        let prompt_id = acp.prompt("fix it").unwrap().expect("a session exists");
        let (mut seen, perm) = until_permission(&acp);
        let request_id = match &perm {
            AcpEvent::Permission { id, tool_call, .. } => {
                assert_eq!(tool_call["content"][0]["type"], json!("diff"));
                *id
            }
            other => panic!("a permission request: {other:?}"),
        };
        acp.answer_permission(request_id, true).unwrap();
        let (more, resp) = until_response(&acp, prompt_id);
        seen.extend(more);
        assert_eq!(
            resp,
            AcpEvent::Response {
                id: prompt_id,
                result: Some(json!({"stopReason": "end_turn"})),
                error: None,
            }
        );
        assert!(seen.iter().any(|e| matches!(e, AcpEvent::Update(u) if u["sessionUpdate"] == "agent_message_chunk" && u["content"]["text"] == "fixed")));
        assert!(seen.iter().any(|e| matches!(e, AcpEvent::Update(u) if u["sessionUpdate"] == "tool_call_update" && u["status"] == "completed")));
        // The whole turn went over the wire as the protocol says.
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(sent.contains(r#""method":"initialize""#), "{sent}");
        assert!(
            sent.contains(r#""method":"session/new","params":{"cwd":"/w","mcpServers":[]}"#),
            "{sent}"
        );
        assert!(sent.contains(r#""method":"session/prompt""#), "{sent}");
        assert!(sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"optionId":"allow_once","outcome":"selected"}}}"#), "{sent}");
    }

    #[test]
    fn allow_sends_allow_once_for_that_request_id_and_stop_sends_session_cancel() {
        let log = tempdir("log").join("stdin.log");
        let mut acp = fake_agent(&log);
        let init = acp.initialize().unwrap();
        until_response(&acp, init);
        let new = acp.new_session(Path::new("/w")).unwrap();
        let (_, resp) = until_response(&acp, new);
        acp.session_id = Some("sess_fake".to_owned());
        let _ = resp;
        // A turn that asks permission; Allow answers it with allow_once for that request id.
        let prompt_id = acp.prompt("fix it").unwrap().unwrap();
        let (mut seen, perm) = until_permission(&acp);
        let request_id = match &perm {
            AcpEvent::Permission {
                id,
                tool_call,
                options,
            } => {
                assert_eq!(tool_call["content"][0]["type"], json!("diff"));
                assert_eq!(
                    options
                        .iter()
                        .map(|o| o["optionId"].as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["allow_once", "reject_once"]
                );
                *id
            }
            other => panic!("a permission request: {other:?}"),
        };
        acp.answer_permission(request_id, true).unwrap();
        let (more, resp) = until_response(&acp, prompt_id);
        seen.extend(more);
        assert_eq!(
            resp,
            AcpEvent::Response {
                id: prompt_id,
                result: Some(json!({"stopReason": "end_turn"})),
                error: None,
            }
        );
        assert!(seen.iter().any(|e| matches!(e, AcpEvent::Update(u) if u["sessionUpdate"] == "agent_message_chunk" && u["content"]["text"] == "fixed")));
        assert!(seen.iter().any(|e| matches!(e, AcpEvent::Update(u) if u["sessionUpdate"] == "tool_call_update" && u["status"] == "completed")));
        // What the fake agent got on stdin: the allow_once answer for request 100.
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(
            sent.contains(r#"{"id":100,"jsonrpc":"2.0","result":{"outcome":{"optionId":"allow_once","outcome":"selected"}}}"#),
            "allow_once for the request id: {sent}"
        );
        assert!(sent.contains(r#""method":"session/prompt""#), "{sent}");
        // Stop: session/cancel goes out, and the turn ends as cancelled.
        let prompt_id = acp.prompt("again").unwrap().unwrap();
        let _ = until_permission(&acp);
        acp.cancel().unwrap();
        let (_, resp) = until_response(&acp, prompt_id);
        assert_eq!(
            resp,
            AcpEvent::Response {
                id: prompt_id,
                result: Some(json!({"stopReason": "cancelled"})),
                error: None,
            }
        );
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(
            sent.contains(r#""method":"session/cancel","params":{"sessionId":"sess_fake"}"#),
            "session/cancel: {sent}"
        );
        // Dropping the client stops the child.
        drop(acp);
    }

    #[test]
    fn the_updates_of_a_fake_turn_map_onto_the_store_rows() {
        let log = tempdir("log").join("stdin.log");
        let mut acp = fake_agent(&log);
        let init = acp.initialize().unwrap();
        until_response(&acp, init);
        let new = acp.new_session(Path::new("/w")).unwrap();
        let (_, resp) = until_response(&acp, new);
        acp.session_id = Some(match &resp {
            AcpEvent::Response {
                result: Some(r), ..
            } => r["sessionId"].as_str().unwrap().to_owned(),
            other => panic!("{other:?}"),
        });
        let prompt_id = acp.prompt("fix it").unwrap().unwrap();
        let (mut seen, perm) = until_permission(&acp);
        let request_id = match &perm {
            AcpEvent::Permission { id, .. } => *id,
            other => panic!("{other:?}"),
        };
        acp.answer_permission(request_id, true).unwrap();
        let (more, _) = until_response(&acp, prompt_id);
        seen.push(perm);
        seen.extend(more);
        // Feed the events through the mapping, as the app does.
        let mut store = store_with_local();
        let id = "local:/w";
        for ev in &seen {
            match ev {
                AcpEvent::Update(u) => apply_update(&mut store, id, u, NOW),
                AcpEvent::Permission {
                    id: rid, tool_call, ..
                } => apply_permission(&mut store, id, *rid, tool_call, NOW),
                _ => {}
            }
        }
        let rows = &store.timelines.get(id).unwrap().rows;
        let kinds: Vec<&str> = rows.iter().map(|r| r.item.kind()).collect();
        assert_eq!(
            kinds,
            [
                "user_message",
                "reasoning",
                "tool_call",
                "assistant_message"
            ],
            "the tool_call_update folds into the tool_call row"
        );
        assert!(store.permissions.contains_key(&format!("{id}/100")));
    }

    fn tempdir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "lucina-acp-test-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
