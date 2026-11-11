//! An app's cache and outbox files. Juno has no ordinary writable disk yet, so they live under `/run` in memory and last
//! until the laptop restarts: leaving an app and coming back is instant, a reboot starts from the server.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// `$LUCINA_APP_DATA/<app>`, or `/run/lucina/<app>`. Private to the owner, like everything an account app stores.
pub fn data_dir(app: &str) -> PathBuf {
    let base = std::env::var_os("LUCINA_APP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/lucina"));
    let dir = base.join(app);
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir);
    dir
}

/// Write the whole file and rename it into place, so that a kill (the shell's SIGKILL) leaves the old copy or the new.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, path)
}

type SaveJob = Box<dyn FnOnce() -> Option<(PathBuf, Vec<u8>)> + Send>;

/// Serializes and writes on a thread of its own, so that a large cache never holds up a key press.
pub struct Saver {
    jobs: mpsc::Sender<SaveJob>,
}

impl Saver {
    pub fn spawn() -> Saver {
        let (jobs, rx) = mpsc::channel::<SaveJob>();
        std::thread::spawn(move || {
            while let Ok(job) = rx.recv() {
                if let Some((path, bytes)) = job() {
                    if let Err(e) = write_atomic(&path, &bytes) {
                        eprintln!("cache: {}: {e}", path.display());
                    }
                }
            }
        });
        Saver { jobs }
    }

    pub fn save(&self, job: impl FnOnce() -> Option<(PathBuf, Vec<u8>)> + Send + 'static) {
        let _ = self.jobs.send(Box::new(job));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_file_replaces_the_old_one_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("lucina-files-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!p.with_extension("tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
