//! An in-process stand-in for the Secretary server, std only: the routes [`crate::client`] uses, with the contract's status
//! codes. It records every request body, so a test can say where a value went.
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const TOKEN: &str = "sec_FAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAK";
pub const DEVICE_ID: &str = "dev_juno";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum QrStatus {
    Pending,
    Approved,
    Denied,
    Collected,
}

struct QrEntry {
    status: QrStatus,
    device_name: Option<String>,
    created: Instant,
}

#[derive(Default)]
struct Inner {
    qr: HashMap<String, QrEntry>,
    qr_count: u32,
    qr_ttl_secs: u64,
    approve_after: Option<u64>,
    items: Vec<Value>,
    next_id: u64,
    chats: HashMap<String, Value>,
    bodies: Vec<(String, String)>,
    decisions: Vec<Value>,
    approvals: Vec<Value>,
    secret_requests: Vec<Value>,
    projects: Value,
    secrets_exist: Vec<String>,
    secret_folders: Vec<String>,
    jobs: HashMap<String, Value>,
    expired_cursor: Option<String>,
    token_revoked: bool,
    reply_delay_ms: u64,
    chat_fails: u32,
    streams_opened: u32,
    last_event_ids: Vec<Option<String>>,
    /// Ends every open stream, as the server does on its own.
    stream_epoch: u32,
    /// Lucina's calls for `lucina-devd`, in order, the ids handed out, and what it answered.
    juno_calls: VecDeque<Value>,
    juno_handed: HashSet<String>,
    juno_results: Vec<(String, Value)>,
    juno_polls: u32,
    /// Each result also goes to this file as a JSON line, for a test in a guest.
    juno_results_file: Option<std::path::PathBuf>,
}

/// The running server. Dropping it leaves the thread; a test process ends soon enough.
#[derive(Clone)]
pub struct Fake {
    base: String,
    inner: Arc<(Mutex<Inner>, Condvar)>,
}

fn tl_id(n: u64) -> String {
    format!("tl_{n:06}")
}

impl Fake {
    pub fn start() -> Fake {
        Self::start_on(0)
    }

    pub fn start_on(port: u16) -> Fake {
        let listener = TcpListener::bind(("127.0.0.1", port)).expect("binds");
        let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("an address").port());
        let inner = Arc::new((Mutex::new(Inner { qr_ttl_secs: 120, next_id: 1, projects: json!({"projects": []}), ..Inner::default() }), Condvar::new()));
        let fake = Fake { base, inner };
        let f = fake.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let f = f.clone();
                std::thread::spawn(move || {
                    let _ = f.serve(conn);
                });
            }
        });
        fake
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_approve_after(&self, secs: Option<u64>) {
        self.lock().approve_after = secs;
    }

    pub fn set_qr_ttl(&self, secs: u64) {
        self.lock().qr_ttl_secs = secs;
    }

    /// As the owner tapping Approve on the phone: every waiting request.
    pub fn approve_all(&self) {
        for e in self.lock().qr.values_mut() {
            if e.status == QrStatus::Pending {
                e.status = QrStatus::Approved;
            }
        }
    }

    pub fn deny_all(&self) {
        for e in self.lock().qr.values_mut() {
            if e.status == QrStatus::Pending {
                e.status = QrStatus::Denied;
            }
        }
    }

    pub fn qr_requests(&self) -> u32 {
        self.lock().qr_count
    }

    pub fn qr_device_names(&self) -> Vec<Option<String>> {
        self.lock().qr.values().map(|e| e.device_name.clone()).collect()
    }

    pub fn revoke(&self) {
        self.lock().token_revoked = true;
    }

    pub fn set_reply_delay_ms(&self, ms: u64) {
        self.lock().reply_delay_ms = ms;
    }

    /// The next `n` chat requests get a 502 without being stored.
    pub fn fail_chats(&self, n: u32) {
        self.lock().chat_fails = n;
    }

    pub fn expire_cursor(&self, cursor: &str) {
        self.lock().expired_cursor = Some(cursor.to_owned());
    }

    pub fn streams_opened(&self) -> u32 {
        self.lock().streams_opened
    }

    pub fn last_event_ids(&self) -> Vec<Option<String>> {
        self.lock().last_event_ids.clone()
    }

    pub fn end_streams(&self) {
        self.lock().stream_epoch += 1;
        self.inner.1.notify_all();
    }

    /// A call for `lucina-devd`, as Lucina's bridge hands it out: `{"id", "tool", "arguments", "deadlineMs"}`.
    pub fn queue_juno_call(&self, call: Value) {
        self.lock().juno_calls.push_back(call);
        self.inner.1.notify_all();
    }

    /// The results posted so far: `(call id, body)`.
    pub fn juno_results(&self) -> Vec<(String, Value)> {
        self.lock().juno_results.clone()
    }

    /// How many times `next` was asked.
    pub fn juno_polls(&self) -> u32 {
        self.lock().juno_polls
    }

    /// Waits until `n` results have come, up to `timeout`.
    pub fn wait_juno_results(&self, n: usize, timeout: Duration) -> bool {
        let t0 = Instant::now();
        let mut g = self.lock();
        while g.juno_results.len() < n {
            let left = timeout.saturating_sub(t0.elapsed());
            if left.is_zero() {
                return false;
            }
            g = self.inner.1.wait_timeout(g, left).unwrap_or_else(|p| p.into_inner()).0;
        }
        true
    }

    pub fn set_juno_results_file(&self, path: &std::path::Path) {
        self.lock().juno_results_file = Some(path.to_path_buf());
    }

    /// `GET /v1/device/juno/next?wait=`: the next call, or `{"call": null}` after up to `wait` seconds (25 at most).
    fn juno_next(&self, query: &HashMap<String, String>) -> (u16, Value) {
        let wait = Duration::from_secs(query.get("wait").and_then(|w| w.parse().ok()).unwrap_or(0).min(25));
        let t0 = Instant::now();
        let mut g = self.lock();
        g.juno_polls += 1;
        loop {
            if let Some(call) = g.juno_calls.pop_front() {
                if let Some(id) = call["id"].as_str() {
                    g.juno_handed.insert(id.to_owned());
                }
                return (200, json!({"call": call}));
            }
            let left = wait.saturating_sub(t0.elapsed());
            if left.is_zero() {
                return (200, json!({"call": null}));
            }
            g = self.inner.1.wait_timeout(g, left).unwrap_or_else(|p| p.into_inner()).0;
        }
    }

    /// `POST /v1/device/juno/calls/{id}/result`. An id never handed out is unknown, as on the server.
    fn juno_result(&self, id: &str, body: &Value) -> (u16, Value) {
        let mut g = self.lock();
        if body["ok"].as_bool().is_none() {
            return (400, json!({"error": {"code": "invalid_request", "message": "ok must be true or false"}}));
        }
        if !g.juno_handed.remove(id) {
            return (404, json!({"error": {"code": "call_not_found", "message": "no such call"}}));
        }
        g.juno_results.push((id.to_owned(), body.clone()));
        if let Some(path) = g.juno_results_file.clone()
            && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(f, "{}", json!({"id": id, "result": body}));
        }
        drop(g);
        self.inner.1.notify_all();
        (200, json!({"ok": true}))
    }

    /// Every body sent to `path`, in order.
    pub fn bodies(&self, path: &str) -> Vec<String> {
        self.lock().bodies.iter().filter(|(p, _)| p == path).map(|(_, b)| b.clone()).collect()
    }

    pub fn all_bodies(&self) -> Vec<(String, String)> {
        self.lock().bodies.clone()
    }

    /// Adds an item as Lucina or another client would. `extra` is merged in. Returns its id.
    pub fn add_item(&self, role: &str, channel: &str, text: &str, extra: Value) -> String {
        let id = {
            let mut g = self.lock();
            let n = g.next_id;
            g.next_id += 1;
            let id = tl_id(n);
            let mut item = json!({"id": id, "ts": 1_793_000_000_000i64 + (n as i64) * 60_000, "channel": channel, "role": role, "text": text});
            if let (Some(o), Some(e)) = (item.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            g.items.push(item);
            id
        };
        self.inner.1.notify_all();
        id
    }

    pub fn add_decision(&self, d: Value) {
        self.lock().decisions.push(d);
    }

    pub fn add_approval(&self, a: Value) {
        self.lock().approvals.push(a);
    }

    pub fn add_secret_request(&self, r: Value) {
        self.lock().secret_requests.push(r);
    }

    pub fn set_projects(&self, p: Value) {
        self.lock().projects = p;
    }

    pub fn add_existing_secret(&self, name: &str) {
        self.lock().secrets_exist.push(name.to_owned());
    }

    pub fn add_secret_folder(&self, name: &str) {
        self.lock().secret_folders.push(name.to_owned());
    }

    pub fn add_job(&self, id: &str, job: Value) {
        self.lock().jobs.insert(id.to_owned(), job);
    }

    /// A chat, a decision and approvals to look at.
    pub fn fill_demo(&self) {
        self.add_item("user", "ios", "Che impegni ho domani?", json!({"meta": {"device": DEVICE_ID}}));
        self.add_item("lucina", "ios", "Domani hai 2 impegni", json!({"meta": {"device": DEVICE_ID, "format": "v1", "lang": "it",
            "bubbles": ["Domani hai **2 impegni** 📅", "Il primo alle *10:15*, poi palestra."],
            "blocks": [
                {"type": "card", "emoji": "📅", "title": "Lezione: Sistemi distribuiti", "subtitle": "10:15–12:00 · Nygaard 395", "tone": "info"},
                {"type": "checklist", "title": "Da portare", "items": [{"text": "Laptop", "done": true}, {"text": "Appunti del capitolo 4"}]},
                {"type": "stat", "items": [{"label": "Peso", "value": "71.2 kg", "delta": "-0.3"}, {"label": "Passi", "value": "8 412"}]}
            ],
            "actions": [
                {"id": "details", "label": "Dettagli", "emoji": "🔎", "kind": "reply", "payload": {"text": "Dammi i dettagli"}},
                {"id": "no", "label": "Va bene così", "kind": "dismiss"}
            ]}}));
        self.add_item("lucina", "other", "Ti serve una chiave per Meshy: GBGO_TEST_KEY", json!({"meta": {"kind": "secret_request", "format": "v1",
            "bubbles": ["Claude Code mi chiede una chiave: `MESHY_API_KEY`"],
            "actions": [{"id": "s", "label": "Aggiungi", "emoji": "🔐", "kind": "secret", "payload": {"mode": "save", "name": "MESHY_API_KEY", "request": "req_demo"}}]}}));
        // As the live server stores a decision: bubbles cut, the text whole, its answer on another device.
        let answers = |id: &str| json!({"meta": {"kind": "decision", "format": "v1",
            "bubbles": ["claude (LucinaOS coordinator) needs a decision: May it stay on the disk?", "It would be plaintext.The…"],
            "actions": [
                {"id": "o1", "label": "1. Store it", "kind": "reply", "payload": {"text": format!("Decision {id}: 1. Store it")}},
                {"id": "o2", "label": "2. Don't", "kind": "reply", "payload": {"text": format!("Decision {id}: 2. Don't")}}
            ]}});
        let question = "claude (LucinaOS coordinator (local_8721b9)) needs a decision: May it stay on the disk?\nIt would be plaintext.\n\n- anyone with the disk can read it\n- it works at boot\n\nTap an option (or open Secretary → Decisions), or answer \"Decision dc-x: <your words>\".";
        self.add_item("lucina", "other", question, answers("dc-demo0"));
        self.add_item("lucina", "other", question, answers("dc-demo1"));
        self.add_item("lucina", "other", "✅ Saved `GBGO_WIFI_PSK` in Lucina / prod / / (fingerprint 53e709e8)", json!({"meta": {"kind": "secret_receipt"}}));
        self.add_secret_request(json!({"id": "req_demo", "name": "MESHY_API_KEY", "project": "lucina", "environment": "prod", "path": "/", "reason": "per generare i modelli", "who": "Claude Code", "status": "active"}));
        self.set_projects(json!({"projects": [{"id": "p1", "name": "Lucina", "alias": "lucina", "environments": [{"slug": "dev", "name": "Dev"}, {"slug": "prod", "name": "Prod"}]}]}));
        self.add_decision(json!({"decision_id": "dc-demo1", "status": "pending", "question": "Unire il ramo juno-secretary su main?", "options": ["Sì, unisci", "Aspetta i test sul laptop", "No"], "recommendation": "Aspetta i test sul laptop", "impact": "medium", "urgency": "normal", "reversible": true, "asking_client": "claude-code", "asking_session": "juno-secretary", "created_at": "2026-11-01T22:00:00.000Z", "evidence": [{"kind": "test", "text": "cargo test: 41 passed", "tier": 1}]}));
        self.add_decision(json!({"decision_id": "dc-demo0", "status": "decided", "question": "Riavviare il gateway?", "options": ["Sì", "No"], "chosen_option": "Sì", "decided_by": "lucina", "reason": "Nessuno lo sta usando.", "impact": "low", "urgency": "low", "reversible": true, "asking_client": "codex", "created_at": "2026-11-01T20:00:00.000Z"}));
        self.add_approval(json!({"id": "ap1", "requester": {"kind": "agent", "name": "Lucina Agents", "detail": "juno-apps"}, "action": "deploy.lucina-agentd", "summary": "Deploy lucina-agentd from main", "details": "lagent deploy lucina-agentd --note 'pipeline'", "risk": "medium", "sensitive": false, "payloadHash": "hash1", "createdAt": "2026-11-01T22:05:00.000Z", "expiresAt": "2099-01-01T00:00:00.000Z", "status": "pending"}));
        self.add_approval(json!({"id": "ap2", "requester": {"kind": "service", "name": "Infisical broker"}, "action": "secret.reveal", "summary": "Show INFISICAL_ROOT to a shell", "details": "reveal INFISICAL_ROOT", "risk": "high", "sensitive": true, "payloadHash": "hash2", "createdAt": "2026-11-01T22:06:00.000Z", "expiresAt": "2099-01-01T00:00:00.000Z", "status": "pending"}));
    }

    fn serve(&self, conn: TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(conn.try_clone()?);
        let mut first = String::new();
        reader.read_line(&mut first)?;
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or("").to_owned();
        let target = parts.next().unwrap_or("").to_owned();
        let mut headers: HashMap<String, String> = HashMap::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_lowercase(), v.trim().to_owned());
            }
        }
        let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut raw = vec![0; length];
        reader.read_exact(&mut raw)?;
        let body_text = String::from_utf8_lossy(&raw).into_owned();
        let body: Value = serde_json::from_str(&body_text).unwrap_or(Value::Null);
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
        let query: HashMap<String, String> = query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_owned(), percent_decode(v)))
            .collect();
        if method != "GET" {
            self.lock().bodies.push((path.to_owned(), body_text));
        }
        let authed = {
            let g = self.lock();
            !g.token_revoked && headers.get("authorization").map(String::as_str) == Some(&format!("Bearer {TOKEN}"))
        };
        let public = path == "/v1/health" || path.starts_with("/v1/session/qr");
        // Never 401, as the server has it.
        if method == "GET" && path == "/v1/session" && !authed {
            return reply(conn, 200, &json!({"authenticated": false, "deviceName": null}));
        }
        if !public && !authed {
            return reply(conn, 401, &json!({"error": {"code": "unauthorized", "message": "unknown device token"}}));
        }
        if path == "/v1/timeline/stream" {
            return self.stream(conn, headers.get("last-event-id").cloned());
        }
        let (status, value) = self.route(&method, path, &query, &body);
        reply(conn, status, &value)
    }

    fn stream(&self, mut conn: TcpStream, last: Option<String>) -> std::io::Result<()> {
        let epoch = {
            let mut g = self.lock();
            g.streams_opened += 1;
            g.last_event_ids.push(last.clone());
            g.stream_epoch
        };
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n: ping\n\n")?;
        let mut sent = last.unwrap_or_else(|| {
            let g = self.lock();
            g.items.last().and_then(|i| i["id"].as_str()).unwrap_or("").to_owned()
        });
        let mut g = self.lock();
        loop {
            if g.stream_epoch != epoch {
                return Ok(());
            }
            let fresh: Vec<Value> = g.items.iter().filter(|i| i["id"].as_str().unwrap_or("") > sent.as_str()).cloned().collect();
            if fresh.is_empty() {
                let (next, timeout) = self.inner.1.wait_timeout(g, Duration::from_secs(5)).unwrap_or_else(|p| p.into_inner());
                g = next;
                if timeout.timed_out() {
                    drop(g);
                    conn.write_all(b": ping\n\n")?;
                    g = self.lock();
                }
                continue;
            }
            drop(g);
            for i in &fresh {
                let id = i["id"].as_str().unwrap_or("");
                write!(conn, "event: item\nid: {id}\ndata: {i}\n\n")?;
                sent = id.to_owned();
            }
            conn.flush()?;
            g = self.lock();
        }
    }

    fn route(&self, method: &str, path: &str, query: &HashMap<String, String>, body: &Value) -> (u16, Value) {
        let err = |s: u16, c: &str| (s, json!({"error": {"code": c, "message": c}}));
        match (method, path) {
            ("GET", "/v1/health") => (200, json!({"ok": true, "version": "fake"})),
            ("GET", "/v1/session") => (200, json!({"authenticated": true, "deviceId": DEVICE_ID, "deviceName": "Juno (Galaxy Book Go)"})),
            ("POST", "/v1/session/qr") => {
                let mut g = self.lock();
                g.qr_count += 1;
                let n = g.qr_count;
                let ttl = g.qr_ttl_secs;
                let poll = format!("poll{n}");
                g.qr.insert(poll.clone(), QrEntry { status: QrStatus::Pending, device_name: body["device"]["name"].as_str().map(str::to_owned), created: Instant::now() });
                (200, json!({"code": format!("code{n}"), "shortCode": format!("{:06}", 100_000 + n), "pollToken": poll, "approveUrl": format!("https://getsecretary.tech/approve?c=code{n}"), "expiresIn": ttl}))
            }
            ("GET", "/v1/session/qr/poll") => {
                let mut g = self.lock();
                let ttl = g.qr_ttl_secs;
                let approve_after = g.approve_after;
                let Some(e) = g.qr.get_mut(query.get("t").map_or("", String::as_str)) else {
                    return (200, json!({"status": "expired"}));
                };
                if e.status == QrStatus::Pending && approve_after.is_some_and(|s| e.created.elapsed().as_secs() >= s) {
                    e.status = QrStatus::Approved;
                }
                match e.status {
                    QrStatus::Pending if e.created.elapsed().as_secs() >= ttl => (200, json!({"status": "expired"})),
                    QrStatus::Pending => (200, json!({"status": "pending"})),
                    QrStatus::Denied => (200, json!({"status": "denied"})),
                    QrStatus::Collected => (200, json!({"status": "expired"})),
                    QrStatus::Approved => {
                        e.status = QrStatus::Collected;
                        (200, json!({"status": "approved", "token": TOKEN, "deviceId": DEVICE_ID, "deviceName": e.device_name}))
                    }
                }
            }
            ("GET", "/v1/timeline") => self.timeline(query),
            ("GET", "/v1/device/juno/next") => self.juno_next(query),
            ("GET", "/v1/timeline/conversations") => (200, json!({"items": [
                {"key": format!("device:{DEVICE_ID}"), "kind": "device", "title": "Juno", "count": 3, "lastTs": 1_793_000_000_000i64, "lastRole": "lucina", "lastText": "Ciao"},
                {"key": "device:dev_iphone", "kind": "device", "title": "The owner - iPhone", "count": 120, "lastTs": 1_793_000_100_000i64, "lastRole": "user", "lastText": "Cosa c'è domani?"},
                {"key": "telegram", "kind": "telegram", "title": "Telegram", "count": 40, "lastTs": 1_792_000_000_000i64, "lastRole": "lucina", "lastText": "Fatto."},
            ]})),
            ("POST", "/v1/chat") => self.chat(body),
            ("GET", "/v1/decisions") => {
                let g = self.lock();
                let status = query.get("status");
                let items: Vec<&Value> = g.decisions.iter().filter(|d| status.is_none_or(|s| d["status"].as_str() == Some(s))).collect();
                (200, json!({"items": items}))
            }
            ("GET", "/v1/approvals") => {
                let g = self.lock();
                let items: Vec<&Value> = g.approvals.iter().filter(|a| a["status"] == "pending").collect();
                (200, json!({"approvals": items}))
            }
            ("GET", "/v1/secrets/catalog") => (200, self.lock().projects.clone()),
            ("GET", "/v1/secrets/names") => {
                let g = self.lock();
                let secrets: Vec<Value> = g.secrets_exist.iter().map(|k| json!({"key": k, "version": 1, "updated_at": "2026-11-01T22:00:00.000Z"})).collect();
                (200, json!({"project": query.get("project"), "environment": query.get("environment"), "path": query.get("path"), "folders": g.secret_folders, "secrets": secrets}))
            }
            ("POST", "/v1/secrets/projects") => {
                let name = body["name"].as_str().unwrap_or("").to_owned();
                if body["confirmed"] != true || name.is_empty() {
                    return (422, json!({"error": {"code": "invalid_request", "message": "confirm the name"}}));
                }
                let project = json!({"id": format!("p-{name}"), "name": name, "environments": [{"slug": "dev", "name": "Dev"}, {"slug": "prod", "name": "Prod"}]});
                let mut g = self.lock();
                if let Some(a) = g.projects["projects"].as_array_mut() {
                    a.push(project.clone());
                }
                (200, json!({"project": project, "receipt": "Created"}))
            }
            ("GET", "/v1/secrets/requests") => {
                let g = self.lock();
                let active = g.secret_requests.iter().find(|r| r["status"] == "active").map(|r| r["id"].clone());
                let queued = g.secret_requests.iter().filter(|r| r["status"] == "queued").count();
                (200, json!({"requests": g.secret_requests, "active": active, "queued": queued}))
            }
            ("POST", "/v1/secrets") => {
                let mut g = self.lock();
                let name = body["name"].as_str().unwrap_or("").to_owned();
                if body["confirmed"] != true {
                    return err(400, "not_confirmed");
                }
                if g.secrets_exist.contains(&name) && body["overwrite"] != true {
                    return err(409, "exists");
                }
                let action = if g.secrets_exist.contains(&name) { "updated" } else { "created" };
                g.secrets_exist.push(name.clone());
                if let Some(r) = body["request"].as_str() {
                    if let Some(entry) = g.secret_requests.iter_mut().find(|e| e["id"] == r) {
                        entry["status"] = "saved".into();
                        entry["fingerprint"] = "ab12cd34".into();
                        entry["action"] = action.into();
                    }
                }
                (200, json!({"key": name, "project": {"id": body["project"], "name": "x"}, "environment": body["environment"], "path": body["path"], "action": action, "fingerprint": "ab12cd34", "receipt": "r"}))
            }
            _ => self.route_id(method, path, body),
        }
    }

    fn route_id(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let err = |s: u16, c: &str| (s, json!({"error": {"code": c, "message": c}}));
        let seg: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (method, seg.as_slice()) {
            ("GET", ["v1", "decisions", id]) => match self.lock().decisions.iter().find(|d| d["decision_id"] == *id) {
                Some(d) => (200, d.clone()),
                None => err(404, "decision_not_found"),
            },
            ("GET", ["v1", "approvals", id]) => match self.lock().approvals.iter().find(|a| a["id"] == *id) {
                Some(a) => (200, a.clone()),
                None => err(404, "approval_not_found"),
            },
            ("POST", ["v1", "approvals", id, "decide"]) => {
                let mut g = self.lock();
                let Some(a) = g.approvals.iter_mut().find(|a| a["id"] == *id) else { return err(404, "approval_not_found") };
                if a["status"] != "pending" {
                    return err(409, "approval_decided");
                }
                if a["payloadHash"] != body["payloadHash"] {
                    return err(409, "payload_mismatch");
                }
                let approve = body["decision"] == "approve";
                if approve && a["sensitive"] == true {
                    return err(403, "biometric_required");
                }
                a["status"] = if approve { "approved" } else { "denied" }.into();
                (200, json!({"status": a["status"]}))
            }
            ("POST", ["v1", "device", "juno", "calls", id, "result"]) => self.juno_result(id, body),
            ("GET", ["v1", "jobs", id]) => match self.lock().jobs.get(*id) {
                Some(j) => (200, j.clone()),
                None => err(404, "job_not_found"),
            },
            ("POST", ["v1", "secrets", "requests", id, "dismiss"]) => {
                let mut g = self.lock();
                match g.secret_requests.iter_mut().find(|e| e["id"] == *id) {
                    Some(e) => {
                        e["status"] = "dismissed".into();
                        (200, json!({"status": "dismissed"}))
                    }
                    None => err(404, "not_found"),
                }
            }
            _ => err(404, "not_found"),
        }
    }

    fn timeline(&self, query: &HashMap<String, String>) -> (u16, Value) {
        let g = self.lock();
        let limit: usize = query.get("limit").and_then(|l| l.parse().ok()).unwrap_or(50);
        if let (Some(a), Some(e)) = (query.get("after"), &g.expired_cursor) {
            if a == e {
                return (410, json!({"error": {"code": "cursor_expired", "message": "gone"}}));
            }
        }
        let conv = query.get("conversation");
        let matches = |i: &Value| match conv {
            None => crate::timeline::Item::parse(i).is_some_and(|x| x.is_mine(Some(DEVICE_ID))),
            Some(c) => crate::timeline::Item::parse(i).is_some_and(|x| x.conversation_key() == *c),
        };
        let newest = g.items.last().and_then(|i| i["id"].as_str()).map(str::to_owned);
        if let Some(after) = query.get("after") {
            let fresh: Vec<&Value> = g.items.iter().filter(|i| i["id"].as_str().unwrap_or("") > after.as_str() && matches(i)).collect();
            let more = fresh.len() > limit;
            let page: Vec<&Value> = fresh.into_iter().take(limit).collect();
            let cursor = if more { page.last().and_then(|i| i["id"].as_str()).map(str::to_owned) } else { newest };
            return (200, json!({"items": page, "nextBefore": null, "cursor": cursor, "hasMore": more}));
        }
        let before = query.get("before");
        let older: Vec<&Value> = g.items.iter().filter(|i| before.is_none_or(|b| i["id"].as_str().unwrap_or("") < b.as_str()) && matches(i)).collect();
        let start = older.len().saturating_sub(limit);
        let page: Vec<&Value> = older[start..].iter().rev().copied().collect();
        let next = if start > 0 { page.last().and_then(|i| i["id"].as_str()).map(str::to_owned) } else { None };
        (200, json!({"items": page, "nextBefore": next, "cursor": newest, "hasMore": false}))
    }

    fn chat(&self, body: &Value) -> (u16, Value) {
        let delay = {
            let mut g = self.lock();
            if g.chat_fails > 0 {
                g.chat_fails -= 1;
                return (502, json!({"error": {"code": "lucina_unreachable", "message": "offline"}}));
            }
            g.reply_delay_ms
        };
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        let cmid = body["clientMessageId"].as_str().unwrap_or("").to_owned();
        if let Some(first) = self.lock().chats.get(&cmid) {
            let mut r = first.clone();
            r["duplicate"] = true.into();
            return (200, r);
        }
        let text = body["message"].as_str().unwrap_or("").to_owned();
        let meta = json!({"device": DEVICE_ID});
        let user = self.add_item("user", "web", &text, json!({"clientMessageId": cmid, "meta": meta}));
        let reply_text = format!("Hai detto: {text}");
        let lucina = self.add_item("lucina", "web", &reply_text, json!({"clientMessageId": cmid, "replyTo": user, "meta": meta}));
        let r = json!({"reply": reply_text, "action": "reply", "job": null, "sessionId": format!("secretary_{}", body["sessionId"].as_str().unwrap_or("")), "clientMessageId": cmid, "userItemId": user, "lucinaItemId": lucina});
        self.lock().chats.insert(cmid, r.clone());
        (200, r)
    }
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn reply(mut conn: TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let text = body.to_string();
    write!(conn, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())
}
