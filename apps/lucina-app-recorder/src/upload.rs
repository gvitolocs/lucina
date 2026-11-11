//! Sending a recording to lucina-memo, as `web/src/lib/memoRecorder.ts` does: create (idempotent by `clientId`), chunks at `Upload-Offset`, complete
//! with the SHA-256. The server's byte count decides where to go on from, so a broken upload goes on where the server is. What is sent and
//! how far is in a file beside the recording, written after every chunk, so that it outlasts a restart.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `CHUNK` of `memoRecorder.ts`.
pub const CHUNK: u64 = 4 << 20;
pub const MIME: &str = "audio/wav";
pub const SOURCE: &str = "juno";

/// What the server says about a memo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoState {
    pub id: String,
    pub received_bytes: u64,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadError {
    /// No answer: try again later.
    Offline(String),
    /// The server looked at it and said no.
    Refused(String),
}

/// `web/src/api/memos.ts`.
pub trait Memos: Send {
    fn create(&mut self, meta: &Meta) -> Result<MemoState, UploadError>;
    fn detail(&mut self, id: &str) -> Result<MemoState, UploadError>;
    /// One chunk at `offset`: the server's byte count, also when the offset was wrong.
    fn put(&mut self, id: &str, offset: u64, chunk: &[u8]) -> Result<u64, UploadError>;
    fn complete(&mut self, id: &str, sha256: &str) -> Result<MemoState, UploadError>;
}

/// The file beside a recording. `done` means the server has all of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub client_id: String,
    pub recorded_at: String,
    pub timezone: String,
    pub duration_sec: f64,
    pub size_bytes: u64,
    #[serde(default)]
    pub server_id: Option<String>,
    #[serde(default)]
    pub uploaded: u64,
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub error: Option<String>,
}

pub fn meta_path(recording: &Path) -> PathBuf {
    recording.with_extension("json")
}

pub fn load_meta(recording: &Path) -> Option<Meta> {
    serde_json::from_slice(&std::fs::read(meta_path(recording)).ok()?).ok()
}

pub fn save_meta(recording: &Path, meta: &Meta) {
    if let Ok(bytes) = serde_json::to_vec(meta) {
        let _ = lucina_app_support::files::write_atomic(&meta_path(recording), &bytes);
    }
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Sends one recording as far as it goes. `progress` hears each new byte count. `Ok` means the server has it all; `Err` leaves the state so
/// that a later call goes on.
pub fn send(memos: &mut dyn Memos, recording: &Path, meta: &mut Meta, progress: &mut dyn FnMut(u64)) -> Result<(), UploadError> {
    let result = send_inner(memos, recording, meta, progress);
    meta.error = result.as_ref().err().map(|e| match e {
        UploadError::Offline(m) | UploadError::Refused(m) => m.clone(),
    });
    save_meta(recording, meta);
    result
}

fn send_inner(memos: &mut dyn Memos, recording: &Path, meta: &mut Meta, progress: &mut dyn FnMut(u64)) -> Result<(), UploadError> {
    if meta.done {
        return Ok(());
    }
    let state = match meta.server_id.clone() {
        None => {
            let state = memos.create(meta)?;
            meta.server_id = Some(state.id.clone());
            save_meta(recording, meta);
            state
        }
        Some(id) => memos.detail(&id)?,
    };
    if state.status != "uploading" {
        meta.done = true;
        meta.uploaded = meta.size_bytes;
        return Ok(());
    }
    let local = |e: std::io::Error| UploadError::Refused(format!("cannot read the recording: {e}"));
    let mut file = File::open(recording).map_err(local)?;
    let mut offset = state.received_bytes;
    let mut chunk = vec![0u8; CHUNK as usize];
    while offset < meta.size_bytes {
        let want = CHUNK.min(meta.size_bytes - offset) as usize;
        file.seek(SeekFrom::Start(offset)).map_err(local)?;
        file.read_exact(&mut chunk[..want]).map_err(local)?;
        let next = memos.put(&state.id, offset, &chunk[..want])?;
        if next <= offset {
            return Err(UploadError::Refused(String::from("The upload is not moving forward.")));
        }
        offset = next;
        meta.uploaded = offset;
        save_meta(recording, meta);
        progress(offset);
    }
    let sha = sha256_file(recording).map_err(local)?;
    memos.complete(&state.id, &sha)?;
    meta.done = true;
    Ok(())
}

/// Over `lucina-account`.
pub struct Account {
    pub client: lucina_account::Client,
}

fn from_api(e: lucina_account::ApiError) -> UploadError {
    use lucina_account::ApiError;
    match e {
        ApiError::Network(m) => UploadError::Offline(m),
        ApiError::RateLimited(s) => UploadError::Offline(format!("too many requests; wait {s} s")),
        ApiError::Http(502..=504, _, m) => UploadError::Offline(m),
        other => UploadError::Refused(other.chat_text()),
    }
}

pub fn memo_from(value: &serde_json::Value) -> Result<MemoState, UploadError> {
    let memo = value.get("memo").unwrap_or(value);
    let id = memo.get("id").and_then(|v| v.as_str());
    let received = memo.get("receivedBytes").and_then(|v| v.as_u64());
    let status = memo.get("status").and_then(|v| v.as_str());
    match (id, received, status) {
        (Some(id), Some(received_bytes), Some(status)) => Ok(MemoState {
            id: id.to_owned(),
            received_bytes,
            status: status.to_owned(),
        }),
        _ => Err(UploadError::Refused(String::from("an answer that is not a memo"))),
    }
}

pub fn create_body(meta: &Meta) -> serde_json::Value {
    serde_json::json!({
        "clientId": meta.client_id,
        "recordedAt": meta.recorded_at,
        "timezone": meta.timezone,
        "durationSec": meta.duration_sec,
        "mime": MIME,
        "sizeBytes": meta.size_bytes,
        "source": SOURCE,
    })
}

impl Memos for Account {
    fn create(&mut self, meta: &Meta) -> Result<MemoState, UploadError> {
        memo_from(&self.client.post_json("/v1/memos", &create_body(meta)).map_err(from_api)?)
    }

    fn detail(&mut self, id: &str) -> Result<MemoState, UploadError> {
        memo_from(&self.client.get_json(&format!("/v1/memos/{id}")).map_err(from_api)?)
    }

    fn put(&mut self, id: &str, offset: u64, chunk: &[u8]) -> Result<u64, UploadError> {
        self.client.put_upload(&format!("/v1/memos/{id}/audio"), offset, chunk).map_err(from_api)
    }

    fn complete(&mut self, id: &str, sha256: &str) -> Result<MemoState, UploadError> {
        memo_from(
            &self
                .client
                .post_json(&format!("/v1/memos/{id}/complete"), &serde_json::json!({ "sha256": sha256 }))
                .map_err(from_api)?,
        )
    }
}
