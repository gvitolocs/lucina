//! Starting another app's program over this one: Gallery and Drive open a video in `lucina-app-video`, so that the decoders are not linked
//! into them and a crash in a decoder cannot take them down. While the other program runs this app draws nothing and reads no input; when
//! it ends, the whole screen is drawn again. Ask for it with [`crate::Ui::run_program`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where an app's program is, as the shell would find it: `/usr/bin/lucina-app-<id>` when apps are in the image, or `<id>.bin` beside
/// this app's own files on the disk, copied out to memory. `None` when this machine does not have it.
pub fn find(id: &str) -> Option<PathBuf> {
    find_in(id, Path::new("/usr/bin"), std::env::var_os("LUCINA_APP_DIR").as_deref().map(Path::new), Path::new("/tmp"))
}

fn find_in(id: &str, bin: &Path, app_dir: Option<&Path>, tmp: &Path) -> Option<PathBuf> {
    let installed = bin.join(format!("lucina-app-{id}"));
    if installed.exists() {
        return Some(installed);
    }
    // Nothing runs from the partition, which any tool that writes to it can unmount: as the shell does, from a copy.
    let on_disk = app_dir?.join(format!("{id}.bin"));
    let copy = tmp.join(format!("lucina-app-{id}.{}", std::process::id()));
    std::fs::copy(&on_disk, &copy).ok()?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755)).ok()?;
    Some(copy)
}

/// A program for [`crate::Ui::run_program`].
pub struct Program {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl Program {
    /// The app `id` with these arguments. `None` when this machine does not have it.
    pub fn app(id: &str, args: &[&str]) -> Option<Program> {
        Some(Program { path: find(id)?, args: args.iter().map(|a| (*a).to_owned()).collect(), env: Vec::new() })
    }

    pub(crate) fn command(&self) -> Command {
        let mut c = Command::new(&self.path);
        c.args(&self.args);
        c.envs(self.env.iter().map(|(k, v)| (k, v)));
        c
    }
}

/// How the other program ended, for the app to say something if it went wrong. An app gets this from
/// [`crate::Ui::program_ended`] on its next `view`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    Ok,
    /// It failed or crashed: a sentence for a person, with what it last wrote.
    Failed(String),
    /// It could not be started.
    NotStarted(String),
}

/// What to do with the last lines of a program's error output, given how it ended.
pub(crate) fn ended_from(status: std::io::Result<std::process::ExitStatus>, tail: &str) -> Ended {
    use std::os::unix::process::ExitStatusExt;
    match status {
        Ok(s) if s.success() => Ended::Ok,
        Ok(s) => {
            let how = match s.signal() {
                Some(n) => format!("stopped by signal {n}"),
                None => format!("ended with status {}", s.code().unwrap_or(-1)),
            };
            let last = tail.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
            Ended::Failed(if last.is_empty() { format!("The player {how}.") } else { format!("The player {how}: {last}") })
        }
        Err(e) => Ended::NotStarted(format!("The player did not start: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn folder(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lucina-program-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn an_app_in_the_image_comes_first() {
        let bin = folder("bin");
        std::fs::write(bin.join("lucina-app-video"), "x").unwrap();
        assert_eq!(find_in("video", &bin, None, &bin), Some(bin.join("lucina-app-video")));
        assert_eq!(find_in("office", &bin, None, &bin), None);
    }

    #[test]
    fn an_app_on_the_disk_runs_from_a_copy() {
        use std::os::unix::fs::PermissionsExt;
        let (bin, dir, tmp) = (folder("none"), folder("apps"), folder("tmp"));
        std::fs::write(dir.join("video.bin"), "program").unwrap();
        let got = find_in("video", &bin, Some(&dir), &tmp).unwrap();
        assert!(got.starts_with(&tmp));
        assert_eq!(std::fs::read(&got).unwrap(), b"program");
        assert_eq!(std::fs::metadata(&got).unwrap().permissions().mode() & 0o111, 0o111);
        assert_eq!(find_in("office", &bin, Some(&dir), &tmp), None);
    }

    #[test]
    fn how_it_ended_reads_as_a_sentence() {
        let ok = std::process::ExitStatus::from_raw(0);
        assert_eq!(ended_from(Ok(ok), "whatever"), Ended::Ok);
        let code = std::process::ExitStatus::from_raw(3 << 8);
        assert_eq!(ended_from(Ok(code), "first\nlast line\n\n"), Ended::Failed("The player ended with status 3: last line".to_owned()));
        let sig = std::process::ExitStatus::from_raw(11);
        assert_eq!(ended_from(Ok(sig), ""), Ended::Failed("The player stopped by signal 11.".to_owned()));
        assert!(matches!(ended_from(Err(std::io::Error::other("no")), ""), Ended::NotStarted(_)));
    }
}
