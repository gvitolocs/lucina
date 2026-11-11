//! The gallery routes on a worker thread, one request at a time. Pictures have their own queue (`lucina_files::pictures`), and a download a
//! thread of its own.

use std::path::PathBuf;

use lucina_app::Image;
use lucina_app_support::net::enc;
use lucina_files::media::{Connect, Media, MediaError};
use lucina_files::pictures::{Done, square_crop};
use serde_json::{Value, json};

use crate::model::{self, Album, Item, Query, Source};

/// `BATCH_MAX`.
const BATCH_MAX: usize = 500;
/// `SMART_PAGE_MAX`.
const SMART_PAGE_MAX: u32 = 200;

pub enum Job {
    /// `listGalleryPage`.
    Page { generation: u64, query: Query, cursor: Option<String>, limit: u32 },
    Sources,
    Months { query: Query },
    Albums,
    /// Are the smart search routes on?
    Status,
    Details { id: String },
    /// `patchItem`; `undo` puts the local copy back if it fails.
    Patch { id: String, body: Value, undo: Box<Item> },
    Batch { op: &'static str, ids: Vec<String>, album: Option<String>, also_from_device: bool },
    CreateAlbum { title: String, add: Vec<String> },
    Placeholders { side: u32, wanted: Vec<(String, String)> },
    Download { url: String, dest: PathBuf },
}

pub enum Msg {
    Changed,
    SignedIn,
    SignedOut,
    Page { generation: u64, result: Result<(Vec<Item>, Option<String>), String> },
    /// The server has no smart search: words from now on.
    SmartOff,
    Sources(Result<(Vec<Source>, u64), String>),
    Months(Result<Vec<(String, u64)>, String>),
    Albums(Result<Vec<Album>, String>),
    SmartReady(bool),
    Details { id: String, result: Result<Value, String> },
    Patched(Result<Box<Item>, (Box<Item>, String)>),
    Batched { op: &'static str, album: Option<String>, done: Vec<String>, failed: usize, error: Option<String> },
    AlbumMade(Result<Album, String>),
    Picture(Done),
    Placeholder(String, Image),
    Download(Result<PathBuf, String>),
    DownloadProgress(u64),
}

pub struct Worker {
    media: Box<dyn Media>,
    connect: Connect,
    send: std::sync::Arc<dyn Fn(Msg) + Send + Sync>,
}

/// A ThumbHash as a tile: the smallest picture of it, filled out to the tile's side.
pub fn placeholder_tile(hash: &str, side: u32) -> Option<Image> {
    let f = secretary_image::placeholder(&model::base64(hash)?).ok()?;
    let px = f.rgba.chunks_exact(4).map(|p| 0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect();
    let small = square_crop(&Image { w: f.width, h: f.height, px });
    // Nearest: it is a blur of colours, there for a moment.
    let (s, n) = (side as usize, small.w as usize);
    let mut out = Vec::with_capacity(s * s);
    for y in 0..s {
        let row = (y * n / s) * n;
        for x in 0..s {
            out.push(small.px[row + x * n / s]);
        }
    }
    Some(Image { w: side, h: side, px: out })
}

impl Worker {
    pub fn new(media: Box<dyn Media>, connect: Connect, send: std::sync::Arc<dyn Fn(Msg) + Send + Sync>) -> Worker {
        Worker { media, connect, send }
    }

    pub fn handle(&mut self, job: Job) -> Vec<Msg> {
        let m = &mut *self.media;
        let dead = |e: &MediaError| *e == MediaError::SignedOut;
        match job {
            Job::Page { generation, query, cursor, limit } => {
                let mut out = Vec::new();
                let smart = query.smart && !query.q.trim().is_empty();
                let mut result = if smart {
                    let qs = model::query_string(&query, cursor.as_deref(), limit.min(SMART_PAGE_MAX));
                    m.get(&format!("/v1/media/items/search?{qs}"))
                } else {
                    let qs = model::query_string(&query, cursor.as_deref(), limit);
                    m.get(&format!("/v1/media/items?{qs}"))
                };
                // `smartPager`: refused means off; the same query by words, and words from now on.
                if smart && matches!(result, Err(MediaError::Refused { status: 503 | 404 | 501, .. })) {
                    out.push(Msg::SmartOff);
                    let qs = model::query_string(&query, cursor.as_deref(), limit);
                    result = m.get(&format!("/v1/media/items?{qs}"));
                }
                match result {
                    Err(e) if dead(&e) => out.push(Msg::SignedOut),
                    r => out.push(Msg::Page {
                        generation,
                        result: r.map_err(|e| e.text()).map(|b| {
                            let items = b["items"].as_array().into_iter().flatten().filter_map(model::parse_item).collect();
                            (items, b["next_cursor"].as_str().filter(|s| !s.is_empty()).map(str::to_owned))
                        }),
                    }),
                }
                out
            }
            Job::Sources => match m.get("/v1/media/sources") {
                Err(e) if dead(&e) => vec![Msg::SignedOut],
                r => vec![Msg::Sources(r.map_err(|e| e.text()).map(|b| {
                    let all = b["all"]["photos"].as_u64().unwrap_or(0) + b["all"]["videos"].as_u64().unwrap_or(0);
                    (model::parse_sources(&b), all)
                }))],
            },
            Job::Months { query } => {
                let mut qs = format!("origin={}&kind={}", enc(query.origin.as_deref().unwrap_or("all")), query.kind.unwrap_or("photo%2Cvideo"));
                if query.trashed {
                    qs.push_str("&trashed=true");
                }
                if query.favorite {
                    qs.push_str("&favorite=true");
                }
                match m.get(&format!("/v1/media/items/months?{qs}")) {
                    Err(e) if dead(&e) => vec![Msg::SignedOut],
                    r => vec![Msg::Months(r.map_err(|e| e.text()).map(|b| model::parse_months(&b)))],
                }
            }
            Job::Albums => match m.get("/v1/media/albums?scope=gallery") {
                Err(e) if dead(&e) => vec![Msg::SignedOut],
                r => vec![Msg::Albums(r.map_err(|e| e.text()).map(|b| model::parse_albums(&b)))],
            },
            Job::Status => {
                // `smartReady`. Any failure is "no": the plain listing works without it.
                let on = m.get("/v1/media/embeddings/status").is_ok_and(|s| s["enabled"] == true && s["indexed"].as_u64().unwrap_or(0) > 0);
                vec![Msg::SmartReady(on)]
            }
            Job::Details { id } => match m.get(&format!("/v1/media/items/{}/details", enc(&id))) {
                Err(e) if dead(&e) => vec![Msg::SignedOut],
                r => vec![Msg::Details { id, result: r.map_err(|e| e.text()) }],
            },
            Job::Patch { id, body, undo } => match m.send("PATCH", &format!("/v1/media/items/{}", enc(&id)), &body) {
                Err(e) if dead(&e) => vec![Msg::SignedOut],
                Err(_) => vec![Msg::Patched(Err((undo, "Couldn't save the change. Try again.".to_owned())))],
                Ok(v) => match model::parse_item(&v) {
                    Some(item) => vec![Msg::Patched(Ok(Box::new(item)))],
                    None => vec![Msg::Patched(Err((undo, "Couldn't save the change. Try again.".to_owned())))],
                },
            },
            Job::Batch { op, ids, album, also_from_device } => {
                let (mut done, mut failed, mut error) = (Vec::new(), 0usize, None);
                for chunk in ids.chunks(BATCH_MAX) {
                    let mut body = json!({"op": op, "ids": chunk});
                    if let Some(a) = &album {
                        body["album"] = json!(a);
                    }
                    if also_from_device {
                        body["also_from_device"] = json!(true);
                    }
                    match m.send("POST", "/v1/media/items/batch", &body) {
                        Ok(r) => {
                            done.extend(r["done"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(str::to_owned)));
                            failed += r["failed"].as_array().map_or(0, Vec::len);
                        }
                        Err(e) if dead(&e) => return vec![Msg::SignedOut],
                        Err(e) => {
                            error = Some(e.text());
                            break;
                        }
                    }
                }
                vec![Msg::Batched { op, album, done, failed, error }]
            }
            Job::CreateAlbum { title, add } => match m.send("POST", "/v1/media/albums", &json!({"title": title})) {
                Err(e) if dead(&e) => vec![Msg::SignedOut],
                Err(_) => vec![Msg::AlbumMade(Err("Couldn't create the album. Try again.".to_owned()))],
                Ok(v) => {
                    let Some(id) = v["id"].as_str().map(str::to_owned) else {
                        return vec![Msg::AlbumMade(Err("Couldn't create the album. Try again.".to_owned()))];
                    };
                    let album = Album { id: id.clone(), title: v["title"].as_str().unwrap_or(&title).to_owned(), count: 0, system: false, cover: None };
                    let mut out = vec![Msg::AlbumMade(Ok(album))];
                    if !add.is_empty() {
                        out.extend(self.handle(Job::Batch { op: "album_add", ids: add, album: Some(id), also_from_device: false }));
                    }
                    out
                }
            },
            Job::Placeholders { side, wanted } => wanted.into_iter().filter_map(|(id, hash)| Some(Msg::Placeholder(id, placeholder_tile(&hash, side)?))).collect(),
            Job::Download { url, dest } => {
                let (connect, send) = (self.connect.clone(), self.send.clone());
                std::thread::spawn(move || {
                    let r = download(&connect, &url, &dest, &*send);
                    send(Msg::Download(r.map(|()| dest)));
                });
                Vec::new()
            }
        }
    }
}

/// Whole or not at all: the file appears under its name when every byte is there.
pub fn download(connect: &Connect, url: &str, dest: &std::path::Path, send: &(dyn Fn(Msg) + Send + Sync)) -> Result<(), String> {
    let mut m = connect().ok_or("not signed in")?;
    let tmp = dest.with_extension("part");
    let mut f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut last = 0u64;
    let r = m.download(url, &mut f, &mut |bytes| {
        if bytes - last >= 8 << 20 {
            last = bytes;
            send(Msg::DownloadProgress(bytes));
        }
        true
    });
    drop(f);
    match r {
        Ok(_) => std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display())),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.text())
        }
    }
}
