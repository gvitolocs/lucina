//! `LUCINA_APP_SAMPLE=1`: a small made-up library in the process, in the real server's JSON (a test compares the two), so an app can be
//! seen and tested with no account. The pictures are painted here, or read from `LUCINA_SAMPLE_PHOTOS`: `<n>-s.jpg` and `<n>-l.jpg`, the
//! real server's two sizes, so that a test image can run real JPEGs through the real decoder.

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lucina_app_support::dates;
use serde_json::{Value, json};

use crate::media::{Connect, Media, MediaError};

const DAY_MS: i64 = 86_400_000;

#[derive(Clone)]
struct Photo {
    id: String,
    seed: u32,
    kind: &'static str,
    taken_ms: i64,
    width: u32,
    height: u32,
    favorite: bool,
    rotation: i64,
    title: String,
    caption: Option<String>,
    trashed_at: Option<String>,
    archived: bool,
    albums: Vec<String>,
    device: Option<&'static str>,
    source: &'static str,
    camera: Option<&'static str>,
}

#[derive(Clone)]
struct File {
    id: String,
    folder: &'static str,
    path: &'static str,
    size: u64,
    kind: &'static str,
    mime: &'static str,
    doc_type: &'static str,
    preview: &'static str,
    language: Option<&'static str>,
    modified_ms: i64,
    thumb: bool,
    snippet_words: &'static str,
}

struct State {
    photos: Vec<Photo>,
    files: Vec<File>,
    albums: Vec<(String, String)>,
    next_album: u32,
    photos_dir: Option<String>,
    photo_files: u32,
}

/// The sample server. Clones share the library.
#[derive(Clone)]
pub struct Fake {
    state: Arc<Mutex<State>>,
    /// While set, every call fails as a lost connection does.
    pub offline: Arc<AtomicBool>,
    /// While set, the token is dead.
    pub signed_out: Arc<AtomicBool>,
    /// Picture requests answered so far.
    pub fetches: Arc<AtomicU64>,
    /// While set, a picture request answers 429.
    pub rate_limited: Arc<AtomicBool>,
}

const FILES: &[(&str, &str, u64, &str, &str, &str, &str, Option<&str>, bool, &str)] = &[
    ("Lezioni", "syllabus.pdf", 182_000, "file", "application/pdf", "pdf", "pdf", None, true, "the course plan and the exam dates"),
    ("Lezioni", "thesis-draft.docx", 512_000, "file", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "document", "office", None, false, "chapter one"),
    ("Lezioni", "budget.xlsx", 48_000, "file", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "spreadsheet", "office", None, false, ""),
    ("Lezioni", "defence.pptx", 8_400_000, "file", "application/vnd.openxmlformats-officedocument.presentationml.presentation", "presentation", "office", None, false, ""),
    ("Lezioni", "whiteboard.jpg", 2_400_000, "photo", "image/jpeg", "image", "image", None, true, ""),
    ("Lezioni", "lecture-recording.mp4", 640_000_000, "video", "video/mp4", "video", "video", None, true, ""),
    ("Lezioni", "voice-note.m4a", 3_100_000, "audio", "audio/mp4", "audio", "audio", None, false, ""),
    ("Lezioni", "Digital Forensics/notes.md", 6_400, "file", "text/markdown", "text", "text", Some("Markdown"), false, "timeline of a disk image"),
    ("Lezioni", "Digital Forensics/carve.rs", 2_100, "file", "text/x-rust", "code", "code", Some("Rust"), false, "fn carve"),
    ("Lezioni", "Digital Forensics/disk-images.zip", 91_000_000, "file", "application/zip", "archive", "archive", None, false, ""),
    ("Lezioni", "Compilers/parser.py", 9_800, "file", "text/x-python", "code", "code", Some("Python"), false, "def parse"),
    ("Lezioni", "Compilers/exam-2025.pdf", 340_000, "file", "application/pdf", "pdf", "pdf", None, true, "grammar question"),
    ("CraftIT", "invoices-2026.xlsx", 77_000, "file", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "spreadsheet", "office", None, false, ""),
    ("CraftIT", "readme.txt", 900, "file", "text/plain", "text", "text", Some("Plain Text"), false, "where everything is"),
    ("CraftIT", "firmware.bin", 2_000_000, "file", "application/octet-stream", "other", "none", None, false, ""),
];

fn iso(ms: i64) -> String {
    dates::to_iso(ms)
}

impl Fake {
    /// `photos` pictures over the days before `now_ms`, about nine a day.
    pub fn new(now_ms: i64, photos: u32) -> Fake {
        let mut list = Vec::with_capacity(photos as usize);
        for i in 0..photos {
            let id = format!("itm_sample{i:05}");
            let taken_ms = now_ms - (i as i64 / 9) * DAY_MS - i as i64 * 1_800_000;
            let portrait = i % 7 == 3;
            let (device, source, camera) = match i % 5 {
                3 => (None, "web-chat", None),
                4 if i % 10 == 4 => (None, "lucina", None),
                _ => (Some("iphone16"), "phone-ios", Some("Apple iPhone 16e")),
            };
            list.push(Photo {
                title: format!("IMG_{:04}.{}", 1000 + i, if i % 23 == 11 { "MOV" } else { "HEIC" }),
                seed: i,
                kind: if i % 23 == 11 { "video" } else { "photo" },
                taken_ms,
                width: if portrait { 3024 } else { 4032 },
                height: if portrait { 4032 } else { 3024 },
                favorite: i % 11 == 2,
                rotation: 0,
                caption: None,
                trashed_at: None,
                archived: false,
                albums: if i % 4 == 0 { vec!["alb_holiday".to_owned()] } else { Vec::new() },
                device,
                source,
                camera,
                id,
            });
        }
        let files = FILES
            .iter()
            .enumerate()
            .map(|(i, f)| File {
                id: format!("itm_file{i:04}"),
                folder: f.0,
                path: f.1,
                size: f.2,
                kind: f.3,
                mime: f.4,
                doc_type: f.5,
                preview: f.6,
                language: f.7,
                thumb: f.8,
                snippet_words: f.9,
                modified_ms: now_ms - (i as i64 + 1) * 3 * DAY_MS,
            })
            .collect();
        let photos_dir = std::env::var("LUCINA_SAMPLE_PHOTOS").ok();
        let photo_files = photos_dir.as_deref().map_or(0, |d| {
            std::fs::read_dir(d).map_or(0, |r| r.flatten().filter(|e| e.file_name().to_string_lossy().ends_with("-s.jpg")).count() as u32)
        });
        Fake {
            state: Arc::new(Mutex::new(State {
                photos: list,
                files,
                albums: vec![("alb_holiday".into(), "Holiday".into()), ("alb_scans".into(), "Scans".into())],
                next_album: 0,
                photos_dir,
                photo_files,
            })),
            offline: Arc::new(AtomicBool::new(false)),
            signed_out: Arc::new(AtomicBool::new(false)),
            fetches: Arc::new(AtomicU64::new(0)),
            rate_limited: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn connect(&self) -> Connect {
        let me = self.clone();
        Arc::new(move || Some(Box::new(me.clone()) as Box<dyn Media>))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn check(&self) -> Result<(), MediaError> {
        if self.signed_out.load(Ordering::SeqCst) {
            return Err(MediaError::SignedOut);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(MediaError::Offline("no connection".into()));
        }
        Ok(())
    }
}

fn not_found() -> MediaError {
    MediaError::Refused { status: 404, code: "not_found".into(), message: "not found".into() }
}

fn split_query(path: &str) -> (&str, HashMap<String, String>) {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let mut m = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        m.insert(k.to_owned(), unescape(v));
    }
    (p, m)
}

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn photo_json(p: &Photo) -> Value {
    let id = &p.id;
    let video = p.kind == "video";
    json!({
        "album": null,
        "albums": p.albums,
        "camera": p.camera,
        "caption": p.caption,
        "codecs": {"audio": null, "video": null},
        "created_at": iso(p.taken_ms),
        "device": p.device,
        "device_name": p.device.map(|_| "The owner's iPhone"),
        "drive": null,
        "duration_ms": if video { Some(12_400) } else { None },
        "edited": false,
        "favorite": p.favorite,
        "hash": format!("{:064x}", p.seed),
        "height": p.height,
        "id": id,
        "kind": p.kind,
        "links": null,
        "live_pair": null,
        "mime": if video { "video/quicktime" } else { "image/heic" },
        "origin": {"filename": p.title, "folder": null, "imported_from": null},
        "place": if p.seed % 3 == 0 { json!({"name": "Aarhus", "admin1": "Central Jutland", "admin2": null, "country": "Denmark", "lat": 56.16, "lon": 10.2}) } else { Value::Null },
        "poster_url": null,
        "rotation": p.rotation,
        "size": 2_400_000 + p.seed as u64 * 1000,
        "source": p.source,
        "stack": null,
        "taken_at": iso(p.taken_ms),
        "thumb_state": "ready",
        "thumb_url": null,
        "thumbhash": null,
        "title": p.title,
        "trash_expires_at": p.trashed_at.as_ref().map(|_| "2030-01-01T00:00:00.000Z"),
        "trashed_at": p.trashed_at,
        "urls": {
            "archive": null,
            "download": format!("/v1/media/items/{id}/original?download=1"),
            "grid": format!("/v1/media/items/{id}/r/grid?v=1"),
            "large": format!("/v1/media/items/{id}/thumb?size=1024&v=1"),
            "original": format!("/v1/media/items/{id}/original"),
            "pdf": null,
            "play": if video { Some(format!("/v1/media/items/{id}/play")) } else { None },
            "poster": if video { Some(format!("/v1/media/items/{id}/poster?v=1")) } else { None },
            "preview": format!("/v1/media/items/{id}/r/preview?v=1"),
            "text": null,
            "thumb": format!("/v1/media/items/{id}/thumb?size=256&v=1"),
        },
        "version": 1,
        "width": p.width,
    })
}

fn file_json(f: &File, snippet: Option<&str>) -> Value {
    let id = &f.id;
    let name = f.path.rsplit('/').next().unwrap_or(f.path);
    let dir = f.path.rfind('/').map_or("", |i| &f.path[..i]);
    let ext = crate::drive::ext_of(name);
    let texty = matches!(f.doc_type, "text" | "code" | "pdf" | "document" | "spreadsheet" | "presentation");
    let mut v = json!({
        "album": f.folder,
        "albums": [],
        "camera": null,
        "caption": null,
        "codecs": {"audio": null, "video": null},
        "created_at": iso(f.modified_ms),
        "device": null,
        "device_name": null,
        "drive": {"dir": dir, "ext": ext, "folder": f.folder, "language": f.language, "modified": iso(f.modified_ms), "name": name, "path": f.path, "preview": f.preview, "text": if texty { "ready" } else { "none" }, "type": f.doc_type},
        "duration_ms": if f.kind == "video" { Some(3_000_000) } else if f.kind == "audio" { Some(61_000) } else { None },
        "edited": false,
        "favorite": false,
        "hash": "0",
        "height": null,
        "id": id,
        "kind": f.kind,
        "links": null,
        "live_pair": null,
        "mime": f.mime,
        "origin": {"filename": name, "folder": null, "imported_from": f.folder},
        "place": null,
        "poster_url": null,
        "rotation": 0,
        "size": f.size,
        "source": "folder",
        "stack": null,
        "taken_at": iso(f.modified_ms),
        "thumb_state": if f.thumb { "ready" } else { "none" },
        "thumb_url": null,
        "thumbhash": null,
        "title": name,
        "trash_expires_at": null,
        "trashed_at": null,
        "urls": {
            "archive": if f.doc_type == "archive" { Some(format!("/v1/media/items/{id}/archive")) } else { None },
            "download": format!("/v1/media/items/{id}/original?download=1"),
            "grid": if f.thumb { Some(format!("/v1/media/items/{id}/r/grid?v=1")) } else { None },
            "large": if f.thumb { Some(format!("/v1/media/items/{id}/thumb?size=1024&v=1")) } else { None },
            "original": format!("/v1/media/items/{id}/original"),
            "pdf": if f.doc_type == "pdf" { Some(format!("/v1/media/items/{id}/pdf")) } else { None },
            "play": if matches!(f.kind, "video" | "audio") { Some(format!("/v1/media/items/{id}/play")) } else { None },
            "poster": if f.thumb && matches!(f.kind, "video" | "audio") { Some(format!("/v1/media/items/{id}/poster?v=1")) } else { None },
            "preview": if f.thumb { Some(format!("/v1/media/items/{id}/r/preview?v=1")) } else { None },
            "text": if texty { Some(format!("/v1/media/items/{id}/text")) } else { None },
            "thumb": if f.thumb { Some(format!("/v1/media/items/{id}/thumb?size=256&v=1")) } else { None },
        },
        "version": 1,
        "width": null,
    });
    if let Some(s) = snippet {
        v["snippet"] = json!(s);
    }
    v
}

fn month_of(ms: i64) -> String {
    let (y, m, _) = crate::drive::civil(ms.div_euclid(DAY_MS));
    format!("{y}-{m:02}")
}

fn origin_of(p: &Photo) -> String {
    match p.device {
        Some(d) => format!("device:{d}"),
        None if p.source.starts_with("lucina") => "lucina".into(),
        None => "chat".into(),
    }
}

impl State {
    fn gallery_items(&self, q: &HashMap<String, String>) -> Vec<&Photo> {
        let kinds = q.get("kind").map_or("photo,video", String::as_str);
        let words = q.get("q").map(|w| w.to_lowercase());
        self.photos
            .iter()
            .filter(|p| kinds.split(',').any(|k| k == p.kind))
            .filter(|p| p.trashed_at.is_some() == q.contains_key("trashed"))
            .filter(|p| p.archived == q.contains_key("archived"))
            .filter(|p| !q.contains_key("favorite") || p.favorite)
            .filter(|p| q.get("album").is_none_or(|a| p.albums.contains(a)))
            .filter(|p| q.get("origin").is_none_or(|o| o == "all" || *o == origin_of(p)))
            .filter(|p| words.as_ref().is_none_or(|w| p.title.to_lowercase().contains(w) || p.caption.as_ref().is_some_and(|c| c.to_lowercase().contains(w))))
            .collect()
    }

    fn items(&self, q: &HashMap<String, String>) -> Value {
        let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200);
        let start: usize = q.get("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
        if q.get("scope").map(String::as_str) == Some("drive") {
            let matches: Vec<(&File, Option<&str>)> = self
                .files
                .iter()
                .filter(|f| q.get("type").is_none_or(|t| t == f.doc_type))
                .filter_map(|f| match q.get("q") {
                    None => Some((f, None)),
                    Some(w) => {
                        let w = w.to_lowercase();
                        if f.path.to_lowercase().contains(&w) {
                            Some((f, None))
                        } else if f.snippet_words.to_lowercase().contains(&w) {
                            Some((f, Some(f.snippet_words)))
                        } else {
                            None
                        }
                    }
                })
                .filter(|(f, _)| q.get("album").is_none_or(|a| a == f.folder))
                .collect();
            let total = matches.len();
            let page: Vec<Value> = matches.iter().skip(start).take(limit).map(|(f, s)| file_json(f, *s)).collect();
            let next = (start + page.len() < total).then(|| (start + page.len()).to_string());
            return json!({"items": page, "next_cursor": next, "total": total});
        }
        let matches = self.gallery_items(q);
        let page: Vec<Value> = matches.iter().skip(start).take(limit).map(|p| photo_json(p)).collect();
        let next = (start + page.len() < matches.len()).then(|| (start + page.len()).to_string());
        json!({"albums": [], "items": page, "next_cursor": next, "total": matches.len()})
    }

    fn browse(&self, q: &HashMap<String, String>) -> Value {
        let folder = q.get("folder").cloned().unwrap_or_default();
        let path = q.get("path").cloned().unwrap_or_default();
        let prefix = if path.is_empty() { String::new() } else { format!("{path}/") };
        let words = q.get("q").map(|w| w.to_lowercase());
        let mut names: Vec<(String, String, u64, u64)> = Vec::new();
        let mut files: Vec<(&File, Option<&str>)> = Vec::new();
        for f in self.files.iter().filter(|f| f.folder == folder && f.path.starts_with(&prefix)) {
            if q.get("type").is_some_and(|t| t != f.doc_type) {
                continue;
            }
            if let Some(w) = &words {
                if f.path.to_lowercase().contains(w) {
                    files.push((f, None));
                } else if f.snippet_words.to_lowercase().contains(w) {
                    files.push((f, Some(f.snippet_words)));
                }
                continue;
            }
            let rest = &f.path[prefix.len()..];
            match rest.split_once('/') {
                None => files.push((f, None)),
                Some((sub, _)) => match names.iter_mut().find(|n| n.0 == sub) {
                    Some(n) => {
                        n.2 += 1;
                        n.3 += f.size;
                    }
                    None => names.push((sub.to_owned(), format!("{prefix}{sub}"), 1, f.size)),
                },
            }
        }
        let sort = q.get("sort").map_or(if words.is_some() { "modified" } else { "name" }, String::as_str);
        let default_asc = matches!(sort, "name" | "type");
        let asc = q.get("order").map_or(default_asc, |o| o == "asc");
        match sort {
            "size" => files.sort_by_key(|f| f.0.size),
            "modified" => files.sort_by_key(|f| f.0.modified_ms),
            "type" => files.sort_by_key(|f| f.0.doc_type),
            _ => files.sort_by_key(|f| f.0.path.to_lowercase()),
        }
        if !asc {
            files.reverse();
        }
        names.sort();
        let total = files.len();
        let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(200);
        let start: usize = q.get("offset").and_then(|c| c.parse().ok()).unwrap_or(0);
        let page: Vec<Value> = files.iter().skip(start).take(limit).map(|(f, s)| file_json(f, *s)).collect();
        let next = (start + page.len() < total).then_some(start + page.len());
        let mut crumbs = vec![json!({"name": folder, "path": ""})];
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        for (i, p) in parts.iter().enumerate() {
            crumbs.push(json!({"name": p, "path": parts[..=i].join("/")}));
        }
        json!({
            "breadcrumbs": crumbs,
            "files": page,
            "folder": folder,
            "folders": names.iter().map(|n| json!({"name": n.0, "path": n.1, "files": n.2, "bytes": n.3, "modified": null})).collect::<Vec<_>>(),
            "next_offset": next,
            "path": path,
            "total": total,
        })
    }

    fn roots(&self) -> Value {
        let mut folders: Vec<Value> = Vec::new();
        for name in ["CraftIT", "Lezioni"] {
            let of: Vec<&File> = self.files.iter().filter(|f| f.folder == name).collect();
            folders.push(json!({
                "bytes": of.iter().map(|f| f.size).sum::<u64>(),
                "files": of.len(),
                "folder": name,
                "modified": of.iter().map(|f| f.modified_ms).max().map(iso),
            }));
        }
        json!({"albums": [{"count": 2, "id": "alb_scans", "title": "Scans"}], "converters": {"office_pdf": false, "pdf_text": true, "pdf_thumbs": true}, "folders": folders})
    }

    fn sources(&self) -> Value {
        let count = |p: &Photo| (p.kind == "photo", p.kind == "video");
        let (mut photos, mut videos) = (0, 0);
        let mut by: Vec<(String, &str, &str, u32, u32)> = vec![
            ("device:iphone16".into(), "The owner's iPhone", "iphone", 0, 0),
            ("chat".into(), "Chat & shares", "chat", 0, 0),
            ("lucina".into(), "Lucina", "lucina", 0, 0),
        ];
        for p in self.photos.iter().filter(|p| p.trashed_at.is_none()) {
            let (ph, vi) = count(p);
            photos += u32::from(ph);
            videos += u32::from(vi);
            if let Some(s) = by.iter_mut().find(|s| s.0 == origin_of(p)) {
                s.3 += u32::from(ph);
                s.4 += u32::from(vi);
            }
        }
        json!({
            "sources": by.iter().map(|s| json!({"id": s.0, "label": s.1, "kind": s.2, "photos": s.3, "videos": s.4, "latest_taken_at": null})).collect::<Vec<_>>(),
            "all": {"photos": photos, "videos": videos},
        })
    }

    fn months(&self, q: &HashMap<String, String>) -> Value {
        let mut out: Vec<(String, u32)> = Vec::new();
        for p in self.gallery_items(q) {
            let m = month_of(p.taken_ms);
            match out.iter_mut().find(|o| o.0 == m) {
                Some(o) => o.1 += 1,
                None => out.push((m, 1)),
            }
        }
        json!({"months": out.iter().map(|(m, c)| json!({"month": m, "count": c})).collect::<Vec<_>>()})
    }

    fn album_json(&self, id: &str, title: &str) -> Value {
        let of: Vec<&Photo> = self.photos.iter().filter(|p| p.albums.iter().any(|a| a == id) && p.trashed_at.is_none()).collect();
        json!({"count": of.len(), "cover_item": of.first().map(|p| p.id.clone()), "created_at": "2026-01-01T00:00:00.000Z", "id": id, "system_key": null, "title": title, "updated_at": "2026-01-01T00:00:00.000Z"})
    }

    fn get(&mut self, path: &str) -> Result<Value, MediaError> {
        let (p, q) = split_query(path);
        let tail = p.strip_prefix("/v1/media/").ok_or_else(not_found)?;
        match tail {
            "drive" => return Ok(self.roots()),
            "drive/browse" => return Ok(self.browse(&q)),
            "items" => return Ok(self.items(&q)),
            "items/months" => return Ok(self.months(&q)),
            "sources" => return Ok(self.sources()),
            "albums" => {
                let albums: Vec<Value> = self.albums.iter().map(|(i, t)| self.album_json(i, t)).collect();
                return Ok(json!({"albums": albums}));
            }
            "items/search" => return Err(MediaError::Refused { status: 503, code: "unavailable".into(), message: "smart search is off".into() }),
            "embeddings/status" => return Ok(json!({"enabled": false, "indexed": 0, "pending": 0, "total": 0})),
            _ => {}
        }
        if let Some(id) = tail.strip_prefix("albums/") {
            let (i, t) = self.albums.iter().find(|a| a.0 == id).ok_or_else(not_found)?;
            return Ok(self.album_json(i, t));
        }
        let rest = tail.strip_prefix("items/").ok_or_else(not_found)?;
        let (id, what) = rest.split_once('/').unwrap_or((rest, ""));
        match what {
            "" => match self.photos.iter().find(|x| x.id == id) {
                Some(x) => Ok(photo_json(x)),
                None => self.files.iter().find(|f| f.id == id).map(|f| file_json(f, None)).ok_or_else(not_found),
            },
            "details" => {
                let x = self.photos.iter().find(|x| x.id == id).ok_or_else(not_found)?;
                Ok(json!({
                    "category": null,
                    "exif": x.camera.map(|_| json!({"make": "Apple", "model": "iPhone 16e", "lens": "iPhone 16e back camera 4.2mm f/1.6", "iso": 64, "exposure_s": 0.004, "f_number": 1.6, "focal_mm": 4.2, "offset": "+01:00"})),
                    "item": photo_json(x),
                    "people": [],
                }))
            }
            "text" => {
                let f = self.files.iter().find(|f| f.id == id).ok_or_else(not_found)?;
                Ok(sample_text(f))
            }
            "archive" => {
                let f = self.files.iter().find(|f| f.id == id).ok_or_else(not_found)?;
                if f.doc_type != "archive" {
                    return Err(MediaError::Refused { status: 415, code: "unsupported".into(), message: "not an archive".into() });
                }
                Ok(json!({
                    "format": "zip",
                    "total": 3,
                    "truncated": false,
                    "entries": [
                        {"path": "images/", "size": 0, "dir": true, "modified": null},
                        {"path": "images/laptop.dd", "size": 64_000_000, "dir": false, "modified": null},
                        {"path": "images/usb-stick.dd", "size": 27_000_000, "dir": false, "modified": null},
                    ],
                }))
            }
            _ => Err(not_found()),
        }
    }

    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, MediaError> {
        let (p, _) = split_query(path);
        let tail = p.strip_prefix("/v1/media/").ok_or_else(not_found)?;
        if tail == "albums" && method == "POST" {
            self.next_album += 1;
            let id = format!("alb_new{}", self.next_album);
            let title = body["title"].as_str().unwrap_or("Album").to_owned();
            self.albums.push((id.clone(), title.clone()));
            return Ok(self.album_json(&id, &title));
        }
        if tail == "items/batch" {
            let op = body["op"].as_str().unwrap_or("");
            let album = body["album"].as_str().map(str::to_owned);
            let mut done = Vec::new();
            for id in body["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if op == "purge" {
                    self.photos.retain(|x| x.id != id);
                    done.push(id.to_owned());
                    continue;
                }
                let Some(x) = self.photos.iter_mut().find(|x| x.id == id) else { continue };
                match op {
                    "trash" => x.trashed_at = Some("2026-11-02T10:00:00.000Z".into()),
                    "restore" => x.trashed_at = None,
                    "favorite" => x.favorite = true,
                    "unfavorite" => x.favorite = false,
                    "archive" => x.archived = true,
                    "unarchive" => x.archived = false,
                    "album_add" => {
                        if let Some(a) = &album {
                            if !x.albums.contains(a) {
                                x.albums.push(a.clone());
                            }
                        }
                    }
                    "album_remove" => x.albums.retain(|a| Some(a) != album.as_ref()),
                    _ => return Err(MediaError::Refused { status: 400, code: "bad".into(), message: "unknown op".into() }),
                }
                done.push(id.to_owned());
            }
            return Ok(json!({"done": done, "failed": []}));
        }
        let id = tail.strip_prefix("items/").ok_or_else(not_found)?;
        if method == "PATCH" {
            let x = self.photos.iter_mut().find(|x| x.id == id).ok_or_else(not_found)?;
            if let Some(f) = body["favorite"].as_bool() {
                x.favorite = f;
            }
            if let Some(r) = body["rotation"].as_i64() {
                x.rotation = r;
            }
            if let Some(c) = body["caption"].as_str() {
                x.caption = Some(c.to_owned());
            }
            if let Some(t) = body["title"].as_str() {
                x.title = t.to_owned();
            }
            return Ok(photo_json(x));
        }
        Err(not_found())
    }

    fn picture(&self, path: &str) -> Result<Vec<u8>, MediaError> {
        let (p, q) = split_query(path);
        let tail = p.strip_prefix("/v1/media/items/").ok_or_else(not_found)?;
        let (id, what) = tail.split_once('/').ok_or_else(not_found)?;
        let large = what == "poster" || q.get("size").is_some_and(|s| s.parse::<u32>().unwrap_or(0) > 512);
        let (seed, w, h, rot) = match self.photos.iter().find(|x| x.id == id) {
            Some(x) => {
                let portrait = x.height > x.width;
                (x.seed, if portrait { 3 } else { 4 }, if portrait { 4 } else { 3 }, x.rotation)
            }
            None => {
                let i = self.files.iter().position(|f| f.id == id).ok_or_else(not_found)?;
                if !self.files[i].thumb && what != "original" {
                    return Err(not_found());
                }
                (1000 + i as u32, 4, 3, 0)
            }
        };
        let _ = rot;
        if let (Some(dir), n @ 1..) = (&self.photos_dir, self.photo_files) {
            let file = format!("{dir}/{}-{}.jpg", seed % n, if large { "l" } else { "s" });
            return std::fs::read(file).map_err(|_| not_found());
        }
        let (pw, ph) = if large { (w * 240, h * 240) } else { (w * 80, h * 80) };
        Ok(painted(seed, pw, ph))
    }
}

fn sample_text(f: &File) -> Value {
    let body = match f.doc_type {
        "code" => "fn carve(image: &[u8]) -> Vec<Range<usize>> {\n    // Every JPEG start marker in the image.\n    let mut found = Vec::new();\n    for i in 0..image.len() {\n        if image[i..].starts_with(&[0xff, 0xd8, 0xff]) {\n            found.push(i..i);\n        }\n    }\n    found\n}\n".to_owned(),
        _ => {
            let mut s = format!("{}\n\n", f.snippet_words);
            for i in 1..=80 {
                s.push_str(&format!("Line {i} of the sample text, from the made-up library.\n"));
            }
            s
        }
    };
    let lines: Vec<Value> = if f.doc_type == "code" {
        body.lines()
            .map(|l| {
                let trimmed = l.trim_start();
                if trimmed.starts_with("//") {
                    json!([["com", l]])
                } else if let Some(rest) = l.strip_prefix("fn ") {
                    json!([["kw", "fn "], ["", rest]])
                } else {
                    json!([["", l]])
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    json!({
        "language": f.language,
        "source": "text",
        "truncated": false,
        "bytes": body.len(),
        "text": body,
        "lines": if f.doc_type == "code" { Value::Array(lines) } else { Value::Null },
    })
}

/// A picture of a hill and a sun, different for every seed, as a PNG: quick to make, and the real decoder reads it.
pub fn painted(seed: u32, w: u32, h: u32) -> Vec<u8> {
    let s = seed as f32;
    let hue = (s * 47.0) % 360.0;
    let sky_top = hsv(hue, 0.55, 0.85);
    let sky_low = hsv((hue + 40.0) % 360.0, 0.3, 1.0);
    let hill_a = hsv((hue + 150.0) % 360.0, 0.55, 0.55);
    let hill_b = hsv((hue + 170.0) % 360.0, 0.6, 0.35);
    let (sun_x, sun_y) = (0.2 + ((s * 0.37).sin() + 1.0) * 0.3, 0.2 + ((s * 0.71).cos() + 1.0) * 0.1);
    let phase = s * 0.9;
    let mut rgb = Vec::with_capacity(w as usize * h as usize * 3);
    for y in 0..h {
        let fy = y as f32 / h as f32;
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let far = 0.58 + 0.08 * (fx * 7.0 + phase).sin();
            let near = 0.74 + 0.06 * (fx * 11.0 + phase * 1.7).sin();
            let c = if fy > near {
                hill_b
            } else if fy > far {
                hill_a
            } else {
                let d = ((fx - sun_x) * w as f32 / h as f32).powi(2) + (fy - sun_y).powi(2);
                if d < 0.006 { [255, 240, 200] } else { mix(sky_top, sky_low, fy / far.max(0.01)) }
            };
            rgb.extend_from_slice(&c);
        }
    }
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.set_compression(png::Compression::Fast);
        if let Ok(mut wr) = e.write_header() {
            let _ = wr.write_image_data(&rgb);
        }
    }
    out
}

fn mix(a: [u8; 3], b: [u8; 3], k: f32) -> [u8; 3] {
    let k = k.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k) as u8)
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8]
}

impl Media for Fake {
    fn get(&mut self, path: &str) -> Result<Value, MediaError> {
        self.check()?;
        self.lock().get(path)
    }

    fn bytes(&mut self, path: &str, _limit: u64) -> Result<Vec<u8>, MediaError> {
        self.check()?;
        if self.rate_limited.load(Ordering::SeqCst) {
            return Err(MediaError::RateLimited(1));
        }
        self.fetches.fetch_add(1, Ordering::SeqCst);
        self.lock().picture(path)
    }

    fn send(&mut self, method: &str, path: &str, body: &Value) -> Result<Value, MediaError> {
        self.check()?;
        self.lock().send(method, path, body)
    }

    fn download(&mut self, path: &str, out: &mut dyn Write, progress: &mut dyn FnMut(u64) -> bool) -> Result<u64, MediaError> {
        self.check()?;
        let bytes = self.lock().picture(path).or_else(|_| Ok::<_, MediaError>(b"sample file from the made-up library\n".to_vec()))?;
        out.write_all(&bytes).map_err(|e| MediaError::Offline(e.to_string()))?;
        progress(bytes.len() as u64);
        Ok(bytes.len() as u64)
    }
}
