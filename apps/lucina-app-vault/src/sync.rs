//! Keeps `/data/vault` in step with the owner's vault on buildhost, through the account server's `/v1/vault` routes (Lucina
//! forwards them to the canonical folder, `/home/user/mnt/mybook/Obsidian`). Every decision and write is the shared core's
//! (`lucina_vault::sync`, SecretaryIOS): compare-and-swap on both sides, conflicts kept as `Note (conflict <date> juno).md`,
//! deletions into `.trash`, the rules proved by Verus there. This file is only the way to the server and the state file.
//!
//! The state (the revision each file had at the last sync, and a stat cache) is `/data/lucina/vault/sync.json`, outside the
//! vault. A damaged state file stops sync rather than start over, which would turn every difference into a conflict copy.

use std::path::{Path, PathBuf};

use lucina_account::client::ApiError;
use lucina_account::Client;
use lucina_vault::sync::{self, Entry, Folder, Manifest, Options, RemoteError, Report, State, DEFAULT_MAX_SIZE};
use serde_json::Value;

/// The vault that follows the server.
pub const SYNCED_ROOT: &str = "/data/vault";
/// How often an open vault app syncs by itself (also at start, on F8 and after each save).
pub const EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// `%XX` for everything but unreserved characters, `/` included, as the server decodes the query.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn remote_error(e: ApiError) -> RemoteError {
    match e {
        ApiError::Http(409, ..) => RemoteError::Conflict,
        ApiError::Unauthorized(_) => {
            lucina_account::system::report_rejected();
            RemoteError::Other("signed out".into())
        }
        e => RemoteError::Other(e.to_string()),
    }
}

/// The server side of a sync.
pub struct Server {
    client: Client,
}

impl Server {
    pub fn new(client: Client) -> Server {
        Server { client }
    }
}

fn entries(v: &Value) -> Result<Manifest, RemoteError> {
    let files = v["files"].as_array().ok_or_else(|| RemoteError::Other("the server sent no file list".into()))?;
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let (Some(path), Some(size), Some(rev)) = (f["path"].as_str(), f["size"].as_u64(), f["rev"].as_str()) else {
            return Err(RemoteError::Other("the server's file list is not in the expected form".into()));
        };
        out.push(Entry { path: path.to_owned(), size, rev: rev.to_owned() });
    }
    Ok(Manifest { files: out })
}

impl sync::Remote for Server {
    fn manifest(&mut self) -> Result<Manifest, RemoteError> {
        let v = self.client.get_json("/v1/vault/manifest").map_err(remote_error)?;
        entries(&v)
    }

    fn get(&mut self, rel: &str) -> Result<(Vec<u8>, String), RemoteError> {
        let (bytes, rev) = self
            .client
            .get_file_with(&format!("/v1/vault/file?path={}", encode(rel)), DEFAULT_MAX_SIZE, "x-vault-revision")
            .map_err(remote_error)?;
        let rev = rev.ok_or_else(|| RemoteError::Other("the server sent a file without its revision".into()))?;
        Ok((bytes, rev))
    }

    fn put(&mut self, rel: &str, bytes: &[u8], expect: Option<&str>) -> Result<String, RemoteError> {
        let q = format!("/v1/vault/file?path={}&expect={}", encode(rel), expect.map_or("absent".to_owned(), encode));
        let v = self.client.send_bytes("PUT", &q, bytes).map_err(remote_error)?;
        v["rev"].as_str().map(str::to_owned).ok_or_else(|| RemoteError::Other("the server did not say the new revision".into()))
    }

    fn trash(&mut self, rel: &str, expect: &str) -> Result<(), RemoteError> {
        let q = format!("/v1/vault/trash?path={}&expect={}", encode(rel), encode(expect));
        self.client.send_bytes("POST", &q, b"").map(|_| ()).map_err(remote_error)
    }
}

/// Where the sync state lives for a vault at `root`: `<state dir>/sync.json` for `/data/vault`, a name per vault otherwise.
pub fn state_path(state_dir: &Path, root: &Path) -> PathBuf {
    if root == Path::new("/data/vault") {
        return state_dir.join("sync.json");
    }
    let name: String = root.display().to_string().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    state_dir.join(format!("sync-{name}.json"))
}

/// One run: the folder at `root` against `remote`, state loaded and saved (also after a failure, so that what settled
/// stays settled). `progress` hears each path acted on.
pub fn run(
    root: &Path,
    state_file: &Path,
    remote: &mut dyn sync::Remote,
    today: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<Report, String> {
    let folder = Folder::open(root).map_err(|e| format!("{}: {e}", root.display()))?;
    // A first sync into a folder that already holds files would send an old copy to the canonical vault as new notes, or
    // make a conflict copy of each: it waits for a person to move them aside.
    if !state_file.exists() && !folder.manifest().map_err(|e| e.to_string())?.files.is_empty() {
        return Err(format!(
            "{} already holds files and has never synced; move them aside (for example to {}-old) and sync again",
            root.display(),
            root.display()
        ));
    }
    let mut state = State::load(state_file).map_err(|e| format!("the sync state is damaged, so sync stopped: {e}"))?;
    let opts = Options { device: "juno".into(), date: today.to_owned(), max_size: DEFAULT_MAX_SIZE };
    let out = sync::run(&folder, remote, &mut state, &opts, progress);
    let saved = state.save(state_file);
    let report = out.map_err(|e| e.to_string())?;
    saved.map_err(|e| format!("could not keep the sync state: {e}"))?;
    Ok(report)
}

/// The status line after a run.
pub fn summary(r: &Report) -> String {
    let mut parts = Vec::new();
    if r.pulled > 0 {
        parts.push(format!("{} from the server", r.pulled));
    }
    if r.pushed > 0 {
        parts.push(format!("{} sent", r.pushed));
    }
    if r.trashed_here + r.trashed_there > 0 {
        parts.push(format!("{} to the trash", r.trashed_here + r.trashed_there));
    }
    if !r.conflicts.is_empty() {
        parts.push(format!("{} kept as conflict copies", r.conflicts.len()));
    }
    if r.deferred > 0 {
        parts.push(format!("{} changed meanwhile, next time", r.deferred));
    }
    if r.large > 0 {
        parts.push(format!("{} over 64 MB left out", r.large));
    }
    if !r.errors.is_empty() {
        parts.push(format!("{} failed ({})", r.errors.len(), r.errors[0]));
    }
    if parts.is_empty() {
        "In step with the server.".into()
    } else {
        format!("Synced: {}.", parts.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_encoded_whole() {
        assert_eq!(encode("A/Note one.md"), "A%2FNote%20one.md");
        assert_eq!(encode("café&x=1#y?.md"), "caf%C3%A9%26x%3D1%23y%3F.md");
        assert_eq!(encode("plain-name_1.~md"), "plain-name_1.~md");
    }

    #[test]
    fn the_server_list_is_checked() {
        let ok = serde_json::json!({"files": [{"path": "a.md", "size": 1, "rev": "r"}]});
        assert_eq!(entries(&ok).unwrap().files.len(), 1);
        assert!(entries(&serde_json::json!({"files": [{"path": "a.md"}]})).is_err());
        assert!(entries(&serde_json::json!({})).is_err());
    }

    #[test]
    fn state_files_sit_outside_the_vault() {
        let s = Path::new("/data/lucina/vault");
        assert_eq!(state_path(s, Path::new("/data/vault")), Path::new("/data/lucina/vault/sync.json"));
        assert_eq!(state_path(s, Path::new("/data/vault/Owner")), Path::new("/data/lucina/vault/sync-_data_vault_Owner.json"));
    }

    /// A whole run against a folder playing the server, through the same `run` the app calls.
    #[test]
    fn a_run_pulls_then_keeps_both_on_a_conflict() {
        struct Local(Folder);
        impl sync::Remote for Local {
            fn manifest(&mut self) -> Result<Manifest, RemoteError> {
                self.0.manifest().map_err(|e| RemoteError::Other(e.to_string()))
            }
            fn get(&mut self, rel: &str) -> Result<(Vec<u8>, String), RemoteError> {
                self.0.read(rel).map_err(|e| RemoteError::Other(e.to_string()))
            }
            fn put(&mut self, rel: &str, b: &[u8], expect: Option<&str>) -> Result<String, RemoteError> {
                self.0.put(rel, b, expect, false).map_err(|e| match e {
                    lucina_vault::Error::Conflict { .. } => RemoteError::Conflict,
                    e => RemoteError::Other(e.to_string()),
                })
            }
            fn trash(&mut self, rel: &str, expect: &str) -> Result<(), RemoteError> {
                self.0.trash(rel, expect).map(|_| ()).map_err(|e| RemoteError::Other(e.to_string()))
            }
        }
        let d = tempfile::tempdir().unwrap();
        let (here, there) = (d.path().join("vault"), d.path().join("server"));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(there.join("A")).unwrap();
        std::fs::write(there.join("A/Note.md"), "server").unwrap();
        let state = d.path().join("state/sync.json");
        let mut server = Local(Folder::open(&there).unwrap());
        std::fs::write(here.join("stray.md"), "old copy").unwrap();
        assert!(run(&here, &state, &mut server, "2026-11-08", &mut |_| {}).unwrap_err().contains("never synced"));
        std::fs::remove_file(here.join("stray.md")).unwrap();
        let r = run(&here, &state, &mut server, "2026-11-08", &mut |_| {}).unwrap();
        assert_eq!(r.pulled, 1);
        std::fs::write(here.join("A/Note.md"), "juno").unwrap();
        std::fs::write(there.join("A/Note.md"), "mac").unwrap();
        let r = run(&here, &state, &mut server, "2026-11-08", &mut |_| {}).unwrap();
        assert_eq!(r.conflicts, vec!["A/Note (conflict 2026-11-08 juno).md".to_owned()]);
        assert_eq!(std::fs::read_to_string(there.join("A/Note (conflict 2026-11-08 juno).md")).unwrap(), "juno");
        assert_eq!(std::fs::read_to_string(here.join("A/Note.md")).unwrap(), "mac");
        assert!(summary(&r).contains("conflict"));
        std::fs::write(&state, "{").unwrap();
        assert!(run(&here, &state, &mut server, "2026-11-08", &mut |_| {}).unwrap_err().contains("damaged"));
    }
}
