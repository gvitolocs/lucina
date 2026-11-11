//! `GET /v1/system/overview` behind a trait, the worker thread that sends it, and the made-up server of sample mode and the tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lucina_account::{ApiError, Client};
use serde_json::{Value, json};

use crate::model::Overview;

/// The last overview, in RAM beside the token.
pub const CACHE_FILE: &str = "server-health-cache.json";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerError {
    SignedOut,
    /// What the page says after "Couldn't refresh:".
    Failed(String),
}

pub trait Server: Send {
    /// `fresh` skips the server's 30 s cache.
    fn overview(&mut self, fresh: bool) -> Result<Overview, ServerError>;
}

pub struct Account {
    pub client: Client,
}

fn parse(v: Value) -> Result<Overview, ServerError> {
    serde_json::from_value(v).map_err(|e| ServerError::Failed(format!("unexpected answer: {e}")))
}

impl Server for Account {
    fn overview(&mut self, fresh: bool) -> Result<Overview, ServerError> {
        let path = if fresh { "/v1/system/overview?fresh=1" } else { "/v1/system/overview" };
        match self.client.get_json(path) {
            Ok(v) => parse(v),
            Err(ApiError::Unauthorized(_)) => Err(ServerError::SignedOut),
            Err(e) => Err(ServerError::Failed(e.to_string())),
        }
    }
}

/// The document of `web/src/test/serverFixtures.ts`, through the same JSON as the wire, with the ages counted from `now`.
pub fn sample_json(now: i64) -> Value {
    let iso = |ms: i64| {
        let s = ms.div_euclid(1000);
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
    };
    let ago = |min: i64| iso(now - min * 60_000);
    let series = |f: &dyn Fn(i64) -> Value| (0..96).map(f).collect::<Vec<_>>();
    let history = json!({
        "step_s": 900,
        "end": iso((now / 900_000 + 1) * 900_000),
        "tctl": series(&|i| if i == 40 { Value::Null } else { json!(61 + i % 36) }),
        "load1": series(&|i| json!(1 + i % 9)),
        "mem": series(&|_| json!(33)),
    });
    let host = |extra: Value| {
        let mut h = json!({
            "role": null, "state": "ok", "reachable": true, "limited": false, "last_report": ago(2), "uptime": "23d 4h",
            "os": "Ubuntu 26.04.1 LTS", "kernel": "Linux 7.0.0-42-generic", "cpus": 16, "load": [9.58, 5.03, 2.48], "mem_used_pct": 33.3, "tctl": 95.1,
            "disks": [{"mount": "/", "used_pct": 77.6, "free_gb": 422, "size_gb": 1885}], "history": history, "findings": [],
        });
        for (k, v) in extra.as_object().expect("an object") {
            h[k] = v.clone();
        }
        h
    };
    json!({
        "generated_at": ago(1),
        "overall": "crit",
        "attention_count": 2,
        "flapping_count": 1,
        "checks_ok": 122,
        "checks_total": 150,
        "sources": {"catalog": {"ok": true, "at": ago(5), "error": null}, "sentinel": {"ok": true, "at": ago(2), "error": null}, "ops": {"ok": true, "at": ago(2), "error": null}},
        "hosts": [
            host(json!({"id": "buildhost", "name": "buildhost", "component_id": "buildhost", "role": "home server", "state": "warn", "findings": ["Tctl 95 °C"],
                "disks": [{"mount": "/", "used_pct": 77.6, "free_gb": 422, "size_gb": 1885}, {"mount": "/data", "used_pct": 93, "free_gb": 70, "size_gb": 1000}]})),
            host(json!({"id": "buildhost2", "name": "buildhost2", "limited": true, "history": null, "load": null, "tctl": null, "mem_used_pct": null, "disks": []})),
            host(json!({"id": "pi-home", "name": "pi-home", "state": "crit", "reachable": false, "last_report": ago(300), "history": null})),
        ],
        "attention": [
            {"key": "catalog:gmaps-server", "sources": ["catalog"], "severity": "crit", "title": "gmaps-server", "component_id": "gmaps-server", "host": "buildhost", "group": "Lucina",
             "project": "secretary", "kind": "service", "criticality": "medium", "summary": "Not running; port 18210 not listening",
             "details": ["user unit gmaps-server.service: inactive/dead", "port 18210: not listening"], "since": ago(130), "failures_24h": 142,
             "impact": {"broken": [{"id": "lucina-maps", "name": "Lucina Maps"}], "degraded": [], "public_hostnames_broken": ["maps.example.com"]}, "caused": ["lucina-maps"],
             "logs": "journalctl --user -u gmaps-server -n 100 --no-pager", "restart": "systemctl --user restart gmaps-server", "runbook": "## Fix\nRestart it."},
            {"key": "sentinel:buildhost:timer:backup", "sources": ["sentinel"], "severity": "warn", "title": "backup", "host": "buildhost", "group": "Lucina", "project": "homelab",
             "kind": "service", "criticality": "medium", "summary": "Last run 26 h ago", "details": [], "since": ago(130), "caused": []},
            {"key": "catalog:pokoin-api", "sources": ["catalog"], "severity": "warn", "title": "Pokoin API", "component_id": "pokoin-api", "host": "p1", "project": "pokoin",
             "summary": "Restarted 7 times", "details": [], "since": ago(20), "failures_24h": 7, "flapping": true, "caused": []},
        ],
        "services": [
            {"name": "Lucina", "state": "crit", "projects": [{"id": "secretary", "state": "crit", "components": [
                {"id": "gmaps-server", "name": "gmaps-server", "kind": "service", "host": "buildhost", "state": "crit", "criticality": "medium", "description": "Maps backend"},
                {"id": "lucina-maps", "name": "Lucina Maps", "kind": "service", "host": "buildhost", "state": "crit", "criticality": "low", "description": null},
                {"id": "secretary-server", "name": "Secretary server", "kind": "service", "host": "buildhost", "state": "ok", "criticality": "critical", "description": "The API"},
                {"id": "secretary-repo", "name": "SecretaryIOS", "kind": "repo", "host": "buildhost", "state": "ok", "criticality": "low", "description": null},
            ]}]},
            {"name": "Pokoin", "state": "ok", "projects": [{"id": "pokoin", "state": "ok", "components": [
                {"id": "pokoin-api", "name": "Pokoin API", "kind": "container", "host": "p1", "state": "ok", "criticality": "high", "description": "The Pokoin API"},
            ]}]},
        ],
        "timeline": [
            {"at": ago(10), "kind": "change", "title": "gmaps-server went down", "host": "buildhost", "from": "ok", "to": "crit"},
            {"at": ago(30), "kind": "deploy", "title": "secretary-server deployed", "project": "secretary"},
            {"at": ago(60), "kind": "incident", "title": "buildhost overheating", "project": "buildhost", "severity": "high",
             "detail": "Tctl stayed above the limit for eleven minutes while three sessions built at once; background encodes paused themselves and the machine came back under ninety degrees on its own."},
            {"at": ago(60 * 24 * 3), "kind": "decision", "title": "Move backups to p1", "project": "homelab"},
        ],
    })
}

/// Sample mode and the tests: the document above, through `serde_json` as the wire goes.
pub struct Fake {
    pub doc: Value,
    pub offline: Arc<AtomicBool>,
    pub signed_out: Arc<AtomicBool>,
    pub calls: Arc<std::sync::Mutex<Vec<bool>>>,
}

impl Fake {
    pub fn new(doc: Value) -> Fake {
        Fake { doc, offline: Arc::default(), signed_out: Arc::default(), calls: Arc::default() }
    }
}

impl Server for Fake {
    fn overview(&mut self, fresh: bool) -> Result<Overview, ServerError> {
        self.calls.lock().unwrap_or_else(|p| p.into_inner()).push(fresh);
        if self.signed_out.load(Ordering::SeqCst) {
            return Err(ServerError::SignedOut);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(ServerError::Failed("no connection".into()));
        }
        parse(serde_json::from_str(&self.doc.to_string()).map_err(|e| ServerError::Failed(e.to_string()))?)
    }
}
