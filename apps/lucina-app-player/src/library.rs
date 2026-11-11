//! What can be played: audio files on this laptop, and the media library's `kind=audio` items (`web/src/api/player.ts`).

use std::path::{Path, PathBuf};

use lucina_account::Client;
use secretary_player::progress::Entry;
use secretary_player::queue::Item;
use serde_json::{Value, json};

/// Local items have this in front of their path, so that their ids never meet the server's.
pub const LOCAL_PREFIX: &str = "file:";
const EXTENSIONS: [&str; 12] = ["mp3", "m4a", "mp4", "aac", "flac", "ogg", "oga", "wav", "aif", "aiff", "mka", "alac"];
const MAX_DEPTH: usize = 6;
/// The whole file comes first, then it plays. A bound on what a mistake can cost.
const DOWNLOAD_LIMIT: u64 = 512 << 20;
const MAX_ITEMS: usize = 2000;

pub fn is_local(id: &str) -> bool {
    id.starts_with(LOCAL_PREFIX)
}

pub fn local_path(id: &str) -> Option<PathBuf> {
    id.strip_prefix(LOCAL_PREFIX).map(PathBuf::from)
}

/// Whether the name is one Player plays: an audio file's extension.
fn has_audio_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Whether this file is one Player can play: there, and an audio file by its extension.
pub fn is_audio_file(path: &Path) -> bool {
    path.is_file() && has_audio_extension(path)
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if depth < MAX_DEPTH {
                walk(&path, depth + 1, out);
            }
        } else if has_audio_extension(&path) {
            out.push(path);
        }
    }
}

/// One file as a local item, the way `scan` lists it.
pub fn item_of(path: PathBuf) -> Item {
    Item {
        id: format!("{LOCAL_PREFIX}{}", path.display()),
        kind: String::from("audio"),
        title: path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
        duration_ms: None,
        extra: Default::default(),
    }
}

/// The audio files under `dirs`, sorted by path. Directories that are not there are fine.
pub fn scan(dirs: &[PathBuf]) -> Vec<Item> {
    let mut paths = Vec::new();
    for dir in dirs {
        walk(dir, 0, &mut paths);
    }
    paths.sort();
    paths.truncate(MAX_ITEMS);
    paths.into_iter().map(item_of).collect()
}

/// The routes this app uses, so that tests and the sample can stand in.
pub trait Server: Send {
    /// `GET /v1/media/items?kind=audio`, every page.
    fn audio_items(&mut self) -> Result<Vec<Item>, String>;
    /// `POST /v1/media/items/{id}/play`, then the file.
    fn file(&mut self, id: &str) -> Result<Vec<u8>, String>;
    /// `GET /v1/media/progress`.
    fn progress(&mut self) -> Result<Vec<Entry>, String>;
    /// `POST /v1/media/progress`: what the server took, `(id, updated_at)`, as `Book::acknowledge` wants them.
    fn push(&mut self, entries: &[Entry]) -> Result<Vec<(String, u64)>, String>;
}

pub struct Account {
    pub client: Client,
}

pub fn items_from_page(page: &Value) -> Result<(Vec<Item>, Option<String>), String> {
    let items = page
        .get("items")
        .cloned()
        .ok_or_else(|| String::from("an answer with no items"))
        .and_then(|v| serde_json::from_value::<Vec<Item>>(v).map_err(|e| format!("unexpected items: {e}")))?;
    let next = page.get("next_cursor").and_then(Value::as_str).map(str::to_owned);
    Ok((items, next))
}

pub fn acks_from(answer: &Value) -> Vec<(String, u64)> {
    answer
        .get("acks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|ack| Some((ack.get("id")?.as_str()?.to_owned(), ack.get("updated_at")?.as_u64()?)))
                .collect()
        })
        .unwrap_or_default()
}

impl Server for Account {
    fn audio_items(&mut self) -> Result<Vec<Item>, String> {
        let mut all = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut path = String::from("/v1/media/items?kind=audio&limit=200");
            if let Some(cursor) = &cursor {
                path.push_str("&cursor=");
                path.push_str(cursor);
            }
            let page = self.client.get_json(&path).map_err(|e| e.chat_text())?;
            let (items, next) = items_from_page(&page)?;
            all.extend(items);
            cursor = next;
            if cursor.is_none() || all.len() >= MAX_ITEMS {
                return Ok(all);
            }
        }
    }

    fn file(&mut self, id: &str) -> Result<Vec<u8>, String> {
        // The original file, whatever the planner would pick for a browser: the decoder reads it here.
        let info = self
            .client
            .post_json(&format!("/v1/media/items/{id}/play"), &json!({"client": "web_chrome", "device": "juno"}))
            .map_err(|e| e.chat_text())?;
        let url = info
            .get("file_url")
            .and_then(Value::as_str)
            .filter(|u| u.starts_with('/'))
            .ok_or_else(|| String::from("the server gave no file to play"))?;
        self.client.get_bytes(url, DOWNLOAD_LIMIT).map_err(|e| e.chat_text())
    }

    fn progress(&mut self) -> Result<Vec<Entry>, String> {
        let page = self.client.get_json("/v1/media/progress?since=0").map_err(|e| e.chat_text())?;
        serde_json::from_value(page.get("entries").cloned().unwrap_or_else(|| json!([])))
            .map_err(|e| format!("unexpected progress: {e}"))
    }

    fn push(&mut self, entries: &[Entry]) -> Result<Vec<(String, u64)>, String> {
        let answer = self
            .client
            .post_json("/v1/media/progress", &json!({ "entries": entries }))
            .map_err(|e| e.chat_text())?;
        Ok(acks_from(&answer))
    }
}
