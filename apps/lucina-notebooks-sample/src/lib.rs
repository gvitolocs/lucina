//! The real `lucina-notebooks` engine (SQLite, its routes, its workers, its citation check) served over HTTP on loopback,
//! with made-up documents and a made-up model behind its traits. Only the model, the embeddings, Keep and speech are stand-ins:
//! on the server those are DeepSeek or Qwen, EmbeddingGemma, p1 and kokoro.
//!
//! The app reaches it the way it reaches the real server, through `lucina_account::Client` and `/v1/notebooks`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use lucina_notebooks::embed::{Embedder, Kind, normalize};
use lucina_notebooks::ingest::{Basic, Extracted, Extractor};
use lucina_notebooks::keep::{Keep, KeepNote};
use lucina_notebooks::llm::{Ask, Delta, Llm, Role};
use lucina_notebooks::podcast::Tts;
use lucina_notebooks::store::Store;
use lucina_notebooks::{Config, Deps, Notebooks, api};
use serde_json::{Value, json};

/// The address the QEMU test tile uses. Not the fake account server's port (8931).
pub const SAMPLE_PORT: u16 = 8932;

/// Answers from the passages it was given, citing the first two, so the citation check and the chips run for real.
pub struct MadeUpModel {
    /// Between words of a streamed answer.
    pub pace: Duration,
}

fn between<'a>(s: &'a str, a: &str, b: &str) -> Option<&'a str> {
    let at = s.find(a)? + a.len();
    let end = s[at..].find(b)? + at;
    Some(&s[at..end])
}

fn first_sentence(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.split_inclusive(". ").next().unwrap_or("").trim_end().trim_end_matches('.').to_owned()
}

fn answer(system: &str) -> String {
    let mut bits = Vec::new();
    let mut rest = system;
    while let Some(at) = rest.find("<passage id=\"") {
        rest = &rest[at..];
        let Some(id) = between(rest, "id=\"", "\"") else { break };
        let Some(open) = rest.find(">\n") else { break };
        let Some(close) = rest.find("</passage>") else { break };
        if id.starts_with("source:") && bits.len() < 2 {
            bits.push(format!("- {} [{id}]", first_sentence(&rest[open + 2..close])));
        }
        rest = &rest[close..];
    }
    if bits.is_empty() {
        return "The sources in this notebook do not say. Add one that does and ask again.".into();
    }
    format!("**From your sources:**\n\n{}\n\nThis answer was written by a made-up model for testing.", bits.join("\n"))
}

fn summary(text: &str) -> String {
    // The engine's prompt has its header, then the source after a blank line.
    let first = first_sentence(text.split_once("\n\n").map_or(text, |x| x.1));
    format!(
        "## Summary\n\n{first}.\n\n- A made-up transformation wrote this.\n- It only repeats the opening of the source."
    )
}

#[async_trait]
impl Llm for MadeUpModel {
    async fn complete(&self, ask: Ask, on_delta: Option<Delta>) -> anyhow::Result<(String, String)> {
        let model = format!("{:?}", ask.model).to_lowercase();
        let reply = match ask.purpose {
            "notebook_podcast_outline" => json!({"segments": [
                {"name": "Opening", "description": "What the notebook is about", "size": "short"},
                {"name": "Main points", "description": "The sources in turn", "size": "short"},
            ]})
            .to_string(),
            "notebook_podcast_script" => json!({"dialogue": [
                {"speaker": "Lucina", "text": "Today we go through this notebook, which is made up for testing."},
                {"speaker": "Marco", "text": "And nothing in this episode is a real recording."},
            ]})
            .to_string(),
            "notebook_chat" => answer(&ask.turns.iter().find(|t| t.role == Role::System).map_or(String::new(), |t| t.text.clone())),
            _ => summary(ask.turns.last().map_or("", |t| t.text.as_str())),
        };
        if let Some(mut f) = on_delta {
            for w in reply.split_inclusive(' ') {
                f(w);
                tokio::time::sleep(self.pace).await;
            }
        }
        Ok((reply, model))
    }
}

/// Letters as a vector: enough to tell texts apart, as the engine's own tests do.
struct Letters;

#[async_trait]
impl Embedder for Letters {
    async fn embed(&self, _kind: Kind, texts: &[String], _title: &str) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0f32; 26];
                for c in t.to_lowercase().bytes().filter(u8::is_ascii_lowercase) {
                    v[usize::from(c - b'a')] += 1.0;
                }
                normalize(&mut v);
                v
            })
            .collect())
    }

    fn name(&self) -> String {
        "letters".into()
    }
}

/// A Keep of a few made-up notes, in memory.
#[derive(Default)]
pub struct MadeUpKeep {
    notes: Mutex<HashMap<String, KeepNote>>,
}

impl MadeUpKeep {
    fn with(notes: &[(&str, &str, &str)]) -> MadeUpKeep {
        let k = MadeUpKeep::default();
        for (slug, title, content) in notes {
            k.notes.lock().unwrap().insert(
                (*slug).into(),
                KeepNote {
                    slug: (*slug).into(),
                    title: (*title).into(),
                    content: (*content).into(),
                    updated_at: Some("2026-10-29T10:00:00Z".into()),
                },
            );
        }
        k
    }
}

#[async_trait]
impl Keep for MadeUpKeep {
    async fn get(&self, slug: &str) -> anyhow::Result<Option<KeepNote>> {
        Ok(self.notes.lock().unwrap().get(slug).cloned())
    }

    async fn create(&self, title: &str, content: &str, _labels: &[String]) -> anyhow::Result<KeepNote> {
        let slug = lucina_notebooks::ids::slug(title, 40);
        let n = KeepNote {
            slug: slug.clone(),
            title: title.into(),
            content: content.into(),
            updated_at: Some(lucina_notebooks::ids::now()),
        };
        self.notes.lock().unwrap().insert(slug, n.clone());
        Ok(n)
    }
}

/// A fifth of a second of silence, for every line.
struct Silence;

#[async_trait]
impl Tts for Silence {
    async fn speak(&self, _text: &str, _voice: &str) -> anyhow::Result<Vec<u8>> {
        let n: u32 = 9600;
        let mut b = Vec::with_capacity(44 + n as usize);
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + n).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&24_000u32.to_le_bytes());
        b.extend_from_slice(&48_000u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&n.to_le_bytes());
        b.resize(44 + n as usize, 0);
        Ok(b)
    }
}

/// Reads text and files as the engine's `Basic` does, and has no network: a link fails, as a dead page does.
struct NoNetwork;

#[async_trait]
impl Extractor for NoNetwork {
    async fn url(&self, _url: &str, _work: &Path) -> anyhow::Result<Extracted> {
        anyhow::bail!("this test has no network, so the page could not be read")
    }

    async fn file(&self, path: &Path, name: &str, work: &Path) -> anyhow::Result<Extracted> {
        Basic.file(path, name, work).await
    }

    async fn sandbox(&self, path: &str, dest: &Path) -> anyhow::Result<PathBuf> {
        Basic.sandbox(path, dest).await
    }
}

/// What a test can do to the server between requests.
#[derive(Clone, Default)]
pub struct Faults {
    /// While set, every request is answered 401, as a revoked device token is.
    pub revoked: Arc<AtomicBool>,
    /// While set, every request is answered 503, as when Link or buildhost is down.
    pub down: Arc<AtomicBool>,
    /// Requests served.
    pub requests: Arc<AtomicU64>,
}

/// `GET /v1/notes` on p1, as far as the note picker reads it.
async fn list_notes(keep: Arc<MadeUpKeep>) -> axum::Json<Value> {
    let mut list: Vec<KeepNote> = keep.notes.lock().unwrap().values().cloned().collect();
    list.sort_by(|a, b| a.slug.cmp(&b.slug));
    let notes: Vec<Value> = list
        .iter()
        .map(|n| json!({"slug": n.slug, "title": n.title, "excerpt": lucina_notebooks::keep::excerpt(&n.content), "updated_at": n.updated_at}))
        .collect();
    axum::Json(json!({"notes": notes}))
}

async fn gate(State(f): State<Faults>, req: Request, next: Next) -> Response {
    f.requests.fetch_add(1, Ordering::SeqCst);
    if f.revoked.load(Ordering::SeqCst) {
        let body = json!({"error": {"code": "unauthorized", "message": "This device was removed."}});
        return (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response();
    }
    if f.down.load(Ordering::SeqCst) {
        let body = json!({"error": {"code": "unavailable", "message": "The notebooks service is unavailable."}});
        return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response();
    }
    next.run(req).await
}

pub struct Sample {
    pub engine: Notebooks,
    pub faults: Faults,
    pub addr: SocketAddr,
    pub base: String,
    /// The first notebook with made-up data, when it was asked for.
    pub first: Option<String>,
    dir: PathBuf,
}

impl Drop for Sample {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scratch_dir() -> anyhow::Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let base = std::env::var_os("LUCINA_NOTEBOOKS_SAMPLE_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let dir = base.join(format!("lucina-notebooks-sample-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    for d in ["files", "podcasts", "work"] {
        std::fs::create_dir_all(dir.join(d)).context("the sample's folder")?;
    }
    Ok(dir)
}

/// The engine with its workers running, on `port` (0 for any free one). Call inside a tokio runtime.
pub async fn serve(port: u16, pace: Duration, seed: bool) -> anyhow::Result<Sample> {
    let dir = scratch_dir()?;
    let keep = Arc::new(MadeUpKeep::with(NOTES));
    let mut cfg = Config::new(&dir);
    // Mixing an episode needs ffmpeg, which the test image does not have: an episode stops there and says so.
    cfg.ffmpeg = PathBuf::from("/usr/bin/ffmpeg");
    let engine = Notebooks::with_store(
        cfg,
        Deps {
            extractor: Arc::new(NoNetwork),
            embedder: Some(Arc::new(Letters)),
            llm: Arc::new(MadeUpModel { pace }),
            keep: keep.clone(),
            tts: Arc::new(Silence),
        },
        Store::in_memory()?,
    )?;
    engine.start();
    let faults = Faults::default();
    let notes = keep.clone();
    let app = axum::Router::new()
        .route("/v1/notes", axum::routing::get(move || list_notes(notes.clone())))
        .nest("/v1/notebooks", api::router(engine.clone()))
        .layer(middleware::from_fn_with_state(faults.clone(), gate));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.context("binding the sample's port")?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let base = format!("http://{addr}");
    let first = if seed { Some(seed_data(&engine).await?) } else { None };
    Ok(Sample { engine, faults, addr, base, first, dir })
}

const NOTES: &[(&str, &str, &str)] = &[
    (
        "exam-checklist",
        "Exam checklist",
        "Read the two lectures again. Do the snooping exercise. Ask about the lab on Thursday. Bring the calculator.",
    ),
    (
        "lecture-1-notes",
        "Lecture 1 notes",
        "Port security limits how many hardware addresses one switch port accepts. The lecturer said this comes up every year.",
    ),
    ("reading-list", "Reading list", "Chapter four first, then the two papers on rate limits."),
];

const LECTURE: &[&str] = &[
    "DHCP snooping makes the switch keep a table of which address it saw given to which port. It drops offers that arrive on untrusted ports, which stops a rogue server from handing out a false gateway. Uplinks are marked as trusted by hand, and everything else is untrusted by default.",
    "The binding table that snooping builds is also what dynamic ARP inspection reads. A port that claims an address it was never given has its ARP replies dropped, so a neighbour cannot take over the gateway's address. Without the table this check has nothing to compare against, so the two features are usually turned on together.",
    "Port security limits how many hardware addresses a port accepts. When the limit is passed the port can drop the extra frames, log them, or shut itself down. The sticky variant learns the first address it sees and writes it to the running configuration, so the limit holds after a cable is moved.",
    "An attacker who floods a switch with made-up source addresses fills its address table, and the switch then floods every frame out of every port as a hub would. Port security stops this at the edge. It does nothing against a device that keeps one address and lies about what it serves, which is why these features are taught together.",
    "VLAN hopping relies on a port that negotiates a trunk or on a double tag that matches the native VLAN. The fixes are dull: turn negotiation off, put access ports in access mode, and give the native VLAN a number nothing uses. None of this costs throughput, and none of it needs new hardware.",
    "Spanning tree has its own attack, where a host claims to be the best root bridge and pulls traffic through itself. Root guard refuses such claims on ports that should never lead to the root, and BPDU guard shuts an access port that sends any bridge traffic at all. Both are one line of configuration per port.",
    "Logging matters more than any single feature. A switch that drops a rogue offer without telling anyone hides the one signal that someone plugged in the wrong box. Send the violations to a central log and look at their rate over time, not at single lines.",
];

const PAPER: &[&str] = &[
    "This made-up paper compares three ways of limiting the rate of DHCP requests on a campus network. The first counts per port, the second per hardware address, and the third per client identifier. A per-port limit was the simplest and caught all of the flooding in the trace.",
    "A per-address limit missed floods that changed the address on every request, which is what the common tools do. The per-identifier limit did no better, because the identifier is also chosen by the client. The authors conclude that the limit belongs on the port, where the client cannot choose it.",
    "The trace covers forty thousand requests over eleven days in a made-up residence hall. About one request in nine was a retry, so a limit below four per second started to hurt ordinary laptops waking up together. Six per second was the lowest value with no complaints.",
];

fn doc(parts: &[&str]) -> String {
    parts.join("\n\n")
}

async fn local(engine: &Notebooks, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    use axum::body::Body;
    use axum::http::{Request, header};
    use tower_service::Service as _;
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))?;
    let mut router = api::router(engine.clone());
    let resp = router.call(req).await?;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024).await?;
    anyhow::ensure!(status.is_success(), "{method} {path}: {status} {}", String::from_utf8_lossy(&bytes));
    Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn id_of(v: &Value) -> anyhow::Result<String> {
    v["id"].as_str().map(str::to_owned).context("no id in the answer")
}

/// Made-up notebooks, made through the engine's own routes. Returns the first one's id.
async fn seed_data(engine: &Notebooks) -> anyhow::Result<String> {
    let nb = id_of(&local(engine, "POST", "/", json!({"name": "Network security", "description": "Made-up lecture material"})).await?)?;
    local(engine, "POST", &format!("/{nb}/sources"), json!({"kind": "text", "title": "Switch security lecture", "text": doc(LECTURE)})).await?;
    local(engine, "POST", &format!("/{nb}/sources"), json!({"kind": "text", "title": "Rate limits on DHCP", "text": doc(PAPER)})).await?;
    local(engine, "POST", &format!("/{nb}/sources"), json!({"kind": "url", "url": "https://example.com/dead-page"})).await?;
    local(engine, "POST", &format!("/{nb}/notes"), json!({"slug": "lecture-1-notes"})).await?;
    local(engine, "POST", &format!("/{nb}/notes"), json!({"slug": "exam-checklist"})).await?;

    wait_until_read(engine, &nb).await?;
    let chat = id_of(&local(engine, "POST", &format!("/{nb}/chats"), json!({"title": "How rogue servers are stopped"})).await?)?;
    local(engine, "POST", &format!("/chats/{chat}/messages"), json!({"text": "How is a rogue DHCP server stopped?"})).await?;

    let other = id_of(&local(engine, "POST", "/", json!({"name": "Thesis reading", "description": ""})).await?)?;
    local(engine, "POST", &format!("/{other}/sources"), json!({"kind": "text", "title": "Reading notes", "text": doc(PAPER)})).await?;
    wait_until_read(engine, &other).await?;
    let old = id_of(&local(engine, "POST", "/", json!({"name": "Last year's course", "description": "Finished"})).await?)?;
    local(engine, "PATCH", &format!("/{old}"), json!({"archived": true})).await?;
    // The first of the list in the QEMU test: the list sorts by the time of the last change, in seconds.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    local(engine, "PATCH", &format!("/{nb}"), json!({"description": "Made-up lecture material"})).await?;
    Ok(nb)
}

/// Text sources become ready through the engine's own workers.
async fn wait_until_read(engine: &Notebooks, nb: &str) -> anyhow::Result<()> {
    for _ in 0..600 {
        let v = local(engine, "GET", &format!("/{nb}"), Value::Null).await?;
        let busy = v["sources"].as_array().is_some_and(|l| {
            l.iter().any(|s| matches!(s["status"].as_str(), Some("queued" | "extracting" | "embedding")))
        });
        if !busy {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("the sources were not read in time")
}
