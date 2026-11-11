//! `/v1/agents/*` behind a trait: the account's client, or sample mode's server, which runs the same JSON and the same stream
//! with made-up agents.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_account::sse::Block;
use lucina_account::{ApiError, Client};
use serde_json::{Value, json};

pub const B: &str = "/v1/agents";

pub trait Blocks: Send {
    /// Blocks until the next block. `Ok(None)`: the server ended the response.
    fn next_block(&mut self) -> Result<Option<Block>, ApiError>;
}

impl Blocks for lucina_account::client::SseStream {
    fn next_block(&mut self) -> Result<Option<Block>, ApiError> {
        lucina_account::client::SseStream::next_block(self)
    }
}

pub trait Api: Send + Sync {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError>;
    fn stream(&self, path: &str, last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError>;
}

pub struct Account(pub Client);

impl Api for Account {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let empty = json!({});
        match method {
            "GET" => self.0.get_json(path),
            "POST" => self.0.post_json(path, body.unwrap_or(&empty)),
            m => self.0.send_json(m, path, body.unwrap_or(&empty)),
        }
    }

    fn stream(&self, path: &str, last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError> {
        Ok(Box::new(self.0.sse(path, last_id)?))
    }
}

/// `encodeURIComponent`.
pub fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The route says "not deployed here": the feature hides instead of showing an error. `notDeployed`.
pub fn not_deployed(e: &ApiError) -> bool {
    matches!(e.status(), Some(404 | 405 | 501))
}

// ---- Sample mode ----

const A1: &str = "agt_sample_running";
const A2: &str = "agt_sample_failed";
const A3: &str = "agt_sample_done";
const WS: &str = "wks_sample";

pub fn iso_now(now_ms: i64) -> String {
    iso(now_ms)
}

fn iso(now_ms: i64) -> String {
    let s = now_ms.div_euclid(1000);
    let (days, rem) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

struct SampleState {
    now: i64,
    snapshots: BTreeMap<String, Value>,
    timelines: BTreeMap<String, Vec<Value>>,
    permissions: Vec<Value>,
    terminals: Vec<Value>,
    schedule_status: String,
    streams: Vec<mpsc::Sender<Block>>,
    term_streams: Vec<mpsc::Sender<Block>>,
    next_event: u64,
    calls: Vec<(String, String, Value)>,
}

/// The server of `LUCINA_APP_SAMPLE=1` and the tests.
#[derive(Clone)]
pub struct Sample(Arc<Mutex<SampleState>>);

fn snapshot(id: &str, title: &str, provider: &str, status: &str, now: i64, min: i64, extra: Value) -> Value {
    let at = iso(now - min * 60_000);
    let mut s = json!({
        "id": id, "provider": provider, "cwd": "/srv/work/sample-project", "workspaceId": WS, "model": "sample-model-1", "createdAt": at, "updatedAt": at,
        "lastUserMessageAt": at, "status": status, "capabilities": {"supportsStreaming": true, "supportsSessionPersistence": true, "supportsDynamicModes": false,
        "supportsMcpServers": true, "supportsReasoningStream": true, "supportsToolInvocations": true}, "currentModeId": null, "availableModes": [],
        "pendingPermissions": [], "persistence": null, "title": title, "labels": {},
    });
    for (k, v) in extra.as_object().expect("an object") {
        s[k] = v.clone();
    }
    s
}

impl Sample {
    pub fn new(now: i64) -> Sample {
        let row = |seq: u64, item: Value| json!({"seq": seq, "timestamp": iso(now - (40 - seq as i64) * 30_000), "turnId": "t1", "item": item});
        let tool = |seq: u64, call: &str, name: &str, status: &str, detail: Value| row(seq, json!({"type": "tool_call", "callId": call, "name": name, "status": status, "detail": detail, "error": null}));
        let timeline = vec![
            row(1, json!({"type": "user_message", "text": "Add a retry to the upload and run the tests."})),
            row(2, json!({"type": "reasoning", "text": "The upload lives in sample.rs; a lost connection should try "})),
            row(3, json!({"type": "reasoning", "text": "again three times before it gives up."})),
            row(4, json!({"type": "assistant_message", "text": "I'll read the upload "})),
            row(5, json!({"type": "assistant_message", "text": "code first."})),
            tool(6, "c1", "read", "completed", json!({"type": "read", "filePath": "/srv/work/sample-project/src/sample.rs", "content": "fn upload() -> Result<(), Error> {\n    send()\n}"})),
            tool(7, "c2", "edit", "completed", json!({"type": "edit", "filePath": "/srv/work/sample-project/src/sample.rs", "unifiedDiff": "@@ -1,3 +1,3 @@\n-    send()\n+    retry(3, send)"})),
            row(8, json!({"type": "todo", "items": [{"text": "Add the retry", "completed": true}, {"text": "Run the tests", "completed": false}]})),
            tool(9, "c3", "shell", "completed", json!({"type": "shell", "command": "cargo test -p sample", "output": "test result: ok. 12 passed", "exitCode": 0})),
            row(10, json!({"type": "notification", "level": "info", "message": "Context is at 60%."})),
            row(11, json!({"type": "compaction", "status": "completed"})),
            row(12, json!({"type": "assistant_message", "text": "The retry is in and the tests pass. I want to push the branch next."})),
            tool(13, "c4", "shell", "running", json!({"type": "shell", "command": "git push origin sample-branch"})),
        ];
        let failed = vec![
            row(1, json!({"type": "user_message", "text": "Rebuild the index."})),
            row(2, json!({"type": "error", "message": "The provider stopped answering."})),
        ];
        let request = json!({"id": "perm-1", "provider": "claude", "name": "Bash", "kind": "tool", "title": "Run git push", "description": "Pushes the branch", "detail": {"type": "shell", "command": "git push origin sample-branch"}});
        let mut snapshots = BTreeMap::new();
        snapshots.insert(A1.to_owned(), snapshot(A1, "Upload retry", "claude", "running", now, 1, json!({"pendingPermissions": [request], "lastUsage": {"inputTokens": 183_000, "cachedInputTokens": 96_000, "outputTokens": 12_400, "totalCostUsd": 1.27}})));
        snapshots.insert(A2.to_owned(), snapshot(A2, "Index rebuild", "codex", "error", now, 25, json!({"lastError": "The provider stopped answering.", "lastUsage": {"inputTokens": 8200, "outputTokens": 310, "totalCostUsd": 0.04}})));
        snapshots.insert(A3.to_owned(), snapshot(A3, "Tidy the docs", "claude", "idle", now, 90, json!({"gate": {"verdict": "verified", "details": ["diffgate: clean"], "proofCommand": "cargo test -p sample", "round": 0}})));
        let mut timelines = BTreeMap::new();
        timelines.insert(A1.to_owned(), timeline);
        timelines.insert(A2.to_owned(), failed);
        timelines.insert(A3.to_owned(), vec![row(1, json!({"type": "assistant_message", "text": "The docs are tidy."}))]);
        Sample(Arc::new(Mutex::new(SampleState {
            now,
            snapshots,
            timelines,
            permissions: vec![
                json!({"id": "perm-1", "kind": "agent", "agentId": A1, "name": "Bash", "title": "Run git push", "description": "Pushes the branch", "details": {"command": "git push origin sample-branch"}, "status": "pending", "createdAt": iso(now - 60_000), "expiresAt": iso(now + 540_000)}),
                json!({"id": "ext-1", "kind": "external", "source": "infisical-broker", "title": "Hand SAMPLE_NAME to a service", "details": {"secret": "SAMPLE_NAME", "caller": "svc-sample"}, "status": "pending", "createdAt": iso(now - 30_000), "expiresAt": iso(now + 300_000), "approvalId": "ap-1"}),
                json!({"id": "perm-0", "kind": "agent", "agentId": A3, "name": "Write", "title": "Write README.md", "status": "allowed", "createdAt": iso(now - 3_600_000), "expiresAt": iso(now - 3_000_000), "resolution": {"behavior": "allow", "by": "owner", "at": iso(now - 3_500_000)}}),
            ],
            terminals: vec![json!({"id": "term-1", "name": "shell", "title": "shell", "cwd": "/srv/work/sample-project", "workspaceId": WS, "rows": 24, "cols": 80, "status": "running"})],
            schedule_status: "active".into(),
            streams: Vec::new(),
            term_streams: Vec::new(),
            next_event: 1,
            calls: Vec::new(),
        })))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SampleState> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// What was asked of it, in order, without the reads.
    pub fn writes(&self) -> Vec<(String, Value)> {
        self.lock().calls.iter().filter(|(m, _, _)| m != "GET").map(|(_, p, b)| (p.clone(), b.clone())).collect()
    }

    pub fn reads(&self, path_starts: &str) -> usize {
        self.lock().calls.iter().filter(|(m, p, _)| m == "GET" && p.starts_with(path_starts)).count()
    }

    fn publish(st: &mut SampleState, ev: &Value) {
        let id = st.next_event;
        st.next_event += 1;
        let block = Block { event: "message".into(), data: ev.to_string(), id: Some(id.to_string()) };
        st.streams.retain(|s| s.send(block.clone()).is_ok());
    }

    fn item_of(snap: &Value) -> Value {
        let s: lucina_agents::types::AgentSnapshot = serde_json::from_value(snap.clone()).expect("the sample's snapshot parses");
        serde_json::to_value(crate::store::item_from_snapshot(&s)).expect("serializes")
    }

    fn append(st: &mut SampleState, id: &str, item: Value) {
        let epoch = "sample-epoch";
        let now = st.now;
        let rows = st.timelines.entry(id.to_owned()).or_default();
        let seq = rows.len() as u64 + 1;
        let at = iso(now);
        rows.push(json!({"seq": seq, "timestamp": at, "turnId": "t2", "item": item}));
        let ev = json!({"type": "agent_stream", "agentId": id, "seq": seq, "epoch": epoch, "timestamp": at, "event": {"type": "timeline", "turnId": "t2", "item": item}});
        Self::publish(st, &ev);
    }

    fn set_status(st: &mut SampleState, id: &str, status: &str) -> Value {
        let snap = st.snapshots.get_mut(id).expect("known");
        snap["status"] = status.into();
        let snap = snap.clone();
        Self::publish(st, &json!({"type": "agent_state", "agent": snap}));
        snap
    }

    fn not_found() -> ApiError {
        ApiError::Http(404, "not_found".into(), "Not here".into())
    }

    fn route(st: &mut SampleState, method: &str, path: &str, body: &Value) -> Result<Value, ApiError> {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let archived = query.contains("archived=true");
        let rest = path.strip_prefix(B).unwrap_or(path);
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        match (method, parts.as_slice()) {
            ("GET", ["items"]) => {
                let agents: Vec<Value> = st.snapshots.values().filter(|s| s["archivedAt"].is_string() == archived).map(Self::item_of).collect();
                Ok(json!({"agents": agents}))
            }
            ("GET", []) => {
                let agents: Vec<Value> = st.snapshots.values().filter(|s| s["archivedAt"].is_string() == archived).cloned().collect();
                Ok(json!({"agents": agents}))
            }
            ("GET", ["permissions"]) => Ok(json!({"permissions": st.permissions})),
            ("GET", ["workspaces"]) => Ok(json!({"workspaces": [{"workspaceId": WS, "projectId": "prj-sample", "cwd": "/srv/work/sample-project", "name": "sample-branch", "projectDisplayName": "Sample project", "isolation": "worktree", "branch": "sample-branch"}]})),
            ("GET", ["projects"]) => Ok(json!({"projects": [{"projectId": "prj-sample", "rootPath": "/srv/work/sample-project", "name": "Sample project"}]})),
            ("GET", ["providers"]) => Ok(json!([
                {"id": "claude", "label": "Claude", "modes": [{"id": "default", "label": "Default"}, {"id": "plan", "label": "Plan"}], "models": [{"id": "sample-model-1", "label": "Sample model 1"}, {"id": "sample-model-2", "label": "Sample model 2"}]},
                {"id": "codex", "label": "Codex", "modes": [], "models": [{"id": "sample-model-3", "label": "Sample model 3"}]},
            ])),
            ("GET", ["search"]) => {
                let q = query.split('&').find_map(|kv| kv.strip_prefix("q=")).unwrap_or("").replace("%20", " ").to_lowercase();
                let results: Vec<Value> = st
                    .timelines
                    .iter()
                    .filter(|(_, rows)| rows.iter().any(|r| r["item"]["text"].as_str().is_some_and(|t| t.to_lowercase().contains(&q))))
                    .map(|(id, _)| {
                        let s = &st.snapshots[id];
                        json!({"agentId": id, "title": s["title"], "provider": s["provider"], "status": s["status"], "cwd": s["cwd"], "updatedAt": s["updatedAt"], "matches": [{"field": "timeline", "snippet": format!("…{q}…")}]})
                    })
                    .collect();
                Ok(json!({"query": q, "results": results}))
            }
            ("GET", ["schedules"]) => Ok(json!({"schedules": [{"id": "sch-1", "name": "Nightly tests", "prompt": "Run the nightly tests", "cadence": {"type": "cron", "expression": "0 3 * * *", "timezone": "UTC"}, "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/srv/work/sample-project"}}, "status": st.schedule_status, "createdAt": iso(st.now - 86_400_000), "updatedAt": iso(st.now - 86_400_000), "nextRunAt": iso(st.now + 7_200_000), "lastRunAt": null, "pausedAt": null, "expiresAt": null, "maxRuns": null, "runs": [], "createdBy": "sample"}]})),
            ("POST", ["schedules", _, action]) => {
                st.schedule_status = match *action {
                    "pause" => "paused".into(),
                    "resume" => "active".into(),
                    _ => st.schedule_status.clone(),
                };
                Ok(json!({"id": "sch-1", "name": "Nightly tests", "prompt": "Run the nightly tests", "cadence": {"type": "cron", "expression": "0 3 * * *", "timezone": "UTC"}, "target": {"type": "new-agent", "config": {"provider": "claude", "cwd": "/srv/work/sample-project"}}, "status": st.schedule_status, "createdAt": iso(st.now - 86_400_000), "updatedAt": iso(st.now), "nextRunAt": iso(st.now + 7_200_000), "lastRunAt": null, "pausedAt": null, "expiresAt": null, "maxRuns": null, "runs": [], "createdBy": "sample"}))
            }
            ("GET", ["usage"]) => Ok(json!({"providers": [
                {"provider": "claude", "agents": 2, "running": 1, "inputTokens": 183_000, "cachedInputTokens": 96_000, "outputTokens": 12_400, "totalCostUsd": 1.27},
                {"provider": "codex", "agents": 1, "running": 0, "inputTokens": 8200, "cachedInputTokens": 0, "outputTokens": 310, "totalCostUsd": 0.04},
            ], "schedules": {"active": 1, "paused": 0, "completed": 2}})),
            ("GET", ["pipeline", "board"]) => Ok(json!({
                "workspaces": [
                    {"workspaceId": WS, "project": "Sample project", "branch": "sample-branch", "cwd": "/srv/work/sample-project", "state": "open", "ahead": 3, "behind": 0, "dirty": true, "idleDays": 0, "procs": 4, "rssMb": 512, "task": {"title": "Upload retry", "session": "local_sample"}},
                    {"workspaceId": "wks_other", "project": "Sample project", "branch": "old-branch", "cwd": "/srv/work/old", "state": "stale", "ahead": 1, "behind": 9, "idleDays": 12},
                ],
                "queue": [{"jobId": "job-1", "kind": "merge", "state": "running", "repo": "sample", "branch": "other-branch", "createdAt": iso(st.now - 120_000)}],
                "recentJobs": [{"jobId": "job-0", "kind": "deploy", "state": "succeeded", "runtime": "sample-server", "createdAt": iso(st.now - 3_600_000), "finishedAt": iso(st.now - 3_500_000)}],
                "leases": [{"device": "laptop", "about": "the test laptop", "lease": {"holder": "local_sample", "purpose": "a test", "until": iso(st.now + 1_800_000)}}],
                "deploys": [{"runtime": "sample-server", "commit": "abcdef0123", "recorded": iso(st.now - 3_500_000), "by": "lagent"}],
                "mainCheckouts": [{"path": "/srv/work/main", "dirtyFiles": 2, "procs": 0}],
                "unmanaged": [],
            })),
            ("GET", ["terminals"]) => Ok(json!({"terminals": st.terminals})),
            ("POST", ["terminals"]) => {
                let t = json!({"id": format!("term-{}", st.terminals.len() + 1), "name": "shell", "title": "shell", "cwd": "/srv/work/sample-project", "workspaceId": body["workspaceId"], "rows": 24, "cols": 80, "status": "running"});
                st.terminals.push(t.clone());
                Ok(t)
            }
            ("DELETE", ["terminals", id]) => {
                st.terminals.retain(|t| t["id"] != *id);
                Self::publish(st, &json!({"type": "terminal_removed", "terminalId": id}));
                Ok(json!({"terminalId": id, "success": true}))
            }
            ("POST", ["terminals", id, "input"]) => {
                let text = body["keys"][0].as_str().unwrap_or("").to_owned();
                // Base64 of an echo, as the real stream sends bytes.
                let out = b64(format!("$ {text}\r\nsample output of {text}\r\n").as_bytes());
                let block = Block { event: "output".into(), data: json!({"b64": out}).to_string(), id: None };
                st.term_streams.retain(|s| s.send(block.clone()).is_ok());
                Ok(json!({"terminalId": id, "keysSent": 2}))
            }
            ("GET", ["workspaces", _, "diff"]) => Ok(json!({"cwd": "/srv/work/sample-project", "diffTooLarge": false, "error": null, "files": [
                {"path": "src/sample.rs", "isNew": false, "isDeleted": false, "additions": 1, "deletions": 1, "status": "ok", "hunks": [{"oldStart": 1, "oldCount": 3, "newStart": 1, "newCount": 3, "lines": [
                    {"type": "context", "content": "fn upload() -> Result<(), Error> {"}, {"type": "remove", "content": "    send()"}, {"type": "add", "content": "    retry(3, send)"}, {"type": "context", "content": "}"}]}]},
                {"path": "docs/retry.md", "isNew": true, "isDeleted": false, "additions": 2, "deletions": 0, "status": "ok", "hunks": [{"oldStart": 0, "oldCount": 0, "newStart": 1, "newCount": 2, "lines": [
                    {"type": "add", "content": "# Retry"}, {"type": "add", "content": "Three tries, then it gives up."}]}]},
                {"path": "assets/logo.png", "isNew": false, "isDeleted": false, "additions": 0, "deletions": 0, "status": "binary", "hunks": []},
            ]})),
            ("GET", ["workspaces", _, "files"]) => {
                let dir = query.split('&').find_map(|kv| kv.strip_prefix("path=")).unwrap_or("").replace("%2F", "/");
                let entries = if dir.is_empty() {
                    json!([{"name": "src", "path": "src", "kind": "directory", "size": 0, "modifiedAt": iso(st.now)}, {"name": "README.md", "path": "README.md", "kind": "file", "size": 2048, "modifiedAt": iso(st.now)}, {"name": "logo.png", "path": "logo.png", "kind": "file", "size": 40_960, "modifiedAt": iso(st.now)}])
                } else {
                    json!([{"name": "sample.rs", "path": "src/sample.rs", "kind": "file", "size": 96, "modifiedAt": iso(st.now)}])
                };
                Ok(json!({"path": dir, "entries": entries}))
            }
            ("GET", ["workspaces", _, "file"]) => {
                let p = query.split('&').find_map(|kv| kv.strip_prefix("path=")).unwrap_or("").replace("%2F", "/");
                if p.ends_with(".png") {
                    Ok(json!({"path": p, "kind": "file", "encoding": "base64", "size": 40_960, "modifiedAt": iso(st.now)}))
                } else {
                    Ok(json!({"path": p, "kind": "file", "encoding": "utf-8", "content": "# Sample\n\nA made-up file in a made-up workspace.\n", "size": 48, "modifiedAt": iso(st.now)}))
                }
            }
            ("GET", ["workspaces", _, "scripts"]) => Ok(json!({"scripts": [
                {"scriptName": "dev", "type": "service", "port": 5173, "lifecycle": "running", "health": "healthy", "command": "npm run dev"},
                {"scriptName": "test", "type": "script", "lifecycle": "stopped", "exitCode": 0, "command": "cargo test"},
            ]})),
            ("GET", ["workspaces", _, "services"]) => Ok(json!({"services": [{"name": "preview", "kind": "detected", "port": 8080, "path": "/x"}]})),
            ("POST", ["workspaces", _, "scripts", name, action]) => Ok(json!({"scriptName": name, "type": "script", "lifecycle": if *action == "start" { "running" } else { "stopped" }})),
            ("POST", ["workspaces"]) => Ok(json!({"workspace": {"workspaceId": WS, "projectId": "prj-sample", "cwd": "/srv/work/sample-project", "name": "new-worktree"}})),
            ("POST", ["permissions", id]) => {
                let Some(p) = st.permissions.iter_mut().find(|p| p["id"] == *id) else { return Err(Self::not_found()) };
                if p["status"] != "pending" {
                    return Err(ApiError::Http(409, "answered".into(), "Already answered".into()));
                }
                p["status"] = if body["behavior"] == "allow" { "allowed".into() } else { "denied".into() };
                let p = p.clone();
                Self::publish(st, &json!({"type": "permission", "permission": p}));
                Ok(p)
            }
            ("POST", []) => {
                let id = format!("agt_new_{}", st.snapshots.len());
                // The created agent lands with the title, labels, workspace and model the request asked for.
                let mut extra = json!({});
                if let Some(l) = body.get("labels") {
                    extra["labels"] = l.clone();
                }
                let title = body["title"].as_str().or(body["prompt"].as_str()).unwrap_or("New run");
                let mut s = snapshot(&id, title, body["provider"].as_str().unwrap_or("claude"), "running", st.now, 0, extra);
                for k in ["workspaceId", "model"] {
                    if let Some(x) = body.get(k) {
                        s[k] = x.clone();
                    }
                }
                st.snapshots.insert(id.clone(), s.clone());
                if let Some(p) = body["prompt"].as_str() {
                    Self::append(st, &id, json!({"type": "user_message", "text": p}));
                }
                Ok(s)
            }
            ("GET", [id]) => st.snapshots.get(*id).cloned().ok_or_else(Self::not_found),
            ("PATCH", [id]) => {
                let Some(s) = st.snapshots.get_mut(*id) else { return Err(Self::not_found()) };
                s["title"] = body["title"].clone();
                Ok(s.clone())
            }
            ("GET", [id, "timeline"]) => {
                let after = query.split('&').find_map(|kv| kv.strip_prefix("afterSeq=")).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                let Some(rows) = st.timelines.get(*id) else { return Err(Self::not_found()) };
                let page: Vec<&Value> = rows.iter().filter(|r| r["seq"].as_u64().unwrap_or(0) > after).collect();
                Ok(json!({"epoch": "sample-epoch", "rows": page, "nextSeq": rows.len() + 1}))
            }
            ("GET", [id, "prompts"]) => {
                let Some(rows) = st.timelines.get(*id) else { return Err(Self::not_found()) };
                let prompts: Vec<Value> = rows.iter().filter(|r| r["item"]["type"] == "user_message").map(|r| json!({"seq": r["seq"], "timestamp": r["timestamp"], "preview": r["item"]["text"]})).collect();
                Ok(json!({"epoch": "sample-epoch", "prompts": prompts}))
            }
            ("POST", [id, "send"]) => {
                Self::append(st, id, json!({"type": "user_message", "text": body["prompt"]}));
                Ok(json!({"agentId": id, "disposition": if body["activeTurnBehavior"] == "steer" { "steered" } else { "queued" }}))
            }
            ("POST", [id, "stop"]) => {
                Self::set_status(st, id, "idle");
                Ok(json!({"agentId": id, "stopped": true}))
            }
            ("POST", [id, "archive"]) => {
                let at = iso(st.now);
                let Some(s) = st.snapshots.get_mut(*id) else { return Err(Self::not_found()) };
                s["archivedAt"] = at.clone().into();
                Ok(json!({"agentId": id, "archivedAt": at}))
            }
            ("POST", [id, "unarchive"]) => {
                let Some(s) = st.snapshots.get_mut(*id) else { return Err(Self::not_found()) };
                if let Some(o) = s.as_object_mut() {
                    o.remove("archivedAt");
                }
                Ok(s.clone())
            }
            ("POST", [id, "detach"]) => st.snapshots.get(*id).cloned().ok_or_else(Self::not_found),
            ("POST", [id, "permissions", req]) => {
                let found = st.permissions.iter_mut().find(|p| p["id"] == *req && p["agentId"] == *id);
                let Some(p) = found else { return Err(Self::not_found()) };
                if p["status"] != "pending" {
                    return Err(ApiError::Http(409, "answered".into(), "Already answered".into()));
                }
                p["status"] = if body["behavior"] == "allow" { "allowed".into() } else { "denied".into() };
                let p = p.clone();
                Self::publish(st, &json!({"type": "permission", "permission": p}));
                if let Some(s) = st.snapshots.get_mut(*id) {
                    s["pendingPermissions"] = json!([]);
                    let s = s.clone();
                    Self::publish(st, &json!({"type": "agent_state", "agent": s}));
                }
                Ok(json!({"agentId": id, "requestId": req, "behavior": body["behavior"]}))
            }
            _ => Err(Self::not_found()),
        }
    }
}

/// Base64, for the sample's terminal bytes.
pub fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

struct ChannelBlocks {
    rx: mpsc::Receiver<Block>,
    first: Vec<Block>,
}

impl Blocks for ChannelBlocks {
    fn next_block(&mut self) -> Result<Option<Block>, ApiError> {
        if !self.first.is_empty() {
            return Ok(Some(self.first.remove(0)));
        }
        loop {
            match self.rx.recv_timeout(Duration::from_secs(120)) {
                Ok(b) => return Ok(Some(b)),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(None),
            }
        }
    }
}

impl Api for Sample {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let mut st = self.lock();
        let b = body.cloned().unwrap_or(Value::Null);
        st.calls.push((method.to_owned(), path.to_owned(), b.clone()));
        let r = Self::route(&mut st, method, path, &b)?;
        // Through text, as the wire goes.
        serde_json::from_str(&r.to_string()).map_err(|e| ApiError::Protocol(e.to_string()))
    }

    fn stream(&self, path: &str, _last_id: Option<&str>) -> Result<Box<dyn Blocks>, ApiError> {
        let (tx, rx) = mpsc::channel();
        let mut st = self.lock();
        if path.starts_with(&format!("{B}/terminals/")) {
            st.term_streams.push(tx);
            let snapshot = Block { event: "snapshot".into(), data: json!({"data": "\u{1b}[32m$\u{1b}[0m cargo test\r\ntest result: ok. 12 passed\r\n"}).to_string(), id: None };
            return Ok(Box::new(ChannelBlocks { rx, first: vec![snapshot] }));
        }
        st.streams.push(tx);
        Ok(Box::new(ChannelBlocks { rx, first: Vec::new() }))
    }
}
