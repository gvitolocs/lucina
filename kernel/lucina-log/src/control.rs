//! Level control: the file `/run/lucina/log/levels` (`source=level` lines, `*=level` for the default, `#` comments), the
//! `LUCINA_LOG` environment default, and the lazy re-read. There is no timer and no polling: the file is looked at only from
//! a log call, when [`rules::recheck_due`] says a second has passed, and read only when its mtime or size changed.

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use crate::level::{DEFAULT_LEVEL, Level};
use crate::rules;

/// The control file's default place.
pub const CONTROL_PATH: &str = "/run/lucina/log/levels";
/// How often a log call looks at the control file at most.
pub const RECHECK_MS: i64 = 1000;

/// What a control file says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Levels {
    /// The `*=level` line.
    pub default: Option<Level>,
    /// The `source=level` lines; a later line for the same source wins.
    pub sources: Vec<(String, Level)>,
}

impl Levels {
    /// Parses the file's text. A line that is not `name=level` with a known level is ignored; so are blank lines and `#` lines.
    pub fn parse(text: &str) -> Levels {
        let mut out = Levels::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((name, level)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let Some(level) = Level::parse(level) else {
                continue;
            };
            if name == "*" {
                out.default = Some(level);
            } else if !name.is_empty() && !name.contains(char::is_whitespace) {
                match out.sources.iter_mut().find(|(n, _)| n == name) {
                    Some(slot) => slot.1 = level,
                    None => out.sources.push((name.to_owned(), level)),
                }
            }
        }
        out
    }

    /// The level for `source`: its own line, else the `*` line, else `fallback`.
    pub fn level_for(&self, source: &str, fallback: Level) -> Level {
        self.sources
            .iter()
            .find(|(n, _)| n == source)
            .map(|(_, l)| *l)
            .or(self.default)
            .unwrap_or(fallback)
    }
}

/// The level from `LUCINA_LOG`, if set and valid.
pub fn env_level() -> Option<Level> {
    std::env::var("LUCINA_LOG")
        .ok()
        .and_then(|v| Level::parse(&v))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sig {
    sec: i64,
    nsec: i64,
    len: u64,
}

/// The control file as one source sees it.
pub struct Control {
    path: PathBuf,
    process_default: Level,
    last_check_ms: Option<i64>,
    /// `None` until the first look; then the file's signature, `Some(None)` when it does not exist.
    seen: Option<Option<Sig>>,
    levels: Levels,
}

impl Control {
    /// `process_default` is the level when the file says nothing: `LUCINA_LOG`, else `info`.
    pub fn new(path: PathBuf, process_default: Option<Level>) -> Control {
        Control {
            path,
            process_default: process_default.unwrap_or(DEFAULT_LEVEL),
            last_check_ms: None,
            seen: None,
            levels: Levels::default(),
        }
    }

    /// Looks at the file if a look is due at `now_ms` (monotonic). Returns whether the file was read again.
    pub fn refresh(&mut self, now_ms: i64) -> bool {
        let now_ms = now_ms.max(0);
        if !rules::recheck_due(self.last_check_ms, now_ms, RECHECK_MS) {
            return false;
        }
        self.last_check_ms = Some(now_ms);
        let sig = std::fs::metadata(&self.path).ok().map(|m| Sig {
            sec: m.mtime(),
            nsec: m.mtime_nsec(),
            len: m.len(),
        });
        if self.seen == Some(sig) {
            return false;
        }
        self.seen = Some(sig);
        self.levels = match sig {
            None => Levels::default(),
            Some(_) => std::fs::read_to_string(&self.path)
                .map(|t| Levels::parse(&t))
                .unwrap_or_default(),
        };
        true
    }

    /// The level for `source` as of the last look.
    pub fn effective(&self, source: &str) -> Level {
        self.levels.level_for(source, self.process_default)
    }
}

#[cfg(test)]
pub(crate) mod testdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A private temp directory, removed on drop.
    pub struct TempDir(PathBuf);
    impl TempDir {
        pub fn new(tag: &str) -> TempDir {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "lucina-log-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testdir::TempDir;
    use super::*;

    #[test]
    fn parses_lines_and_ignores_the_rest() {
        let l = Levels::parse(
            "# comment\n\n*=warn\nshell = debug\nnetd=TRACE\nbad line\nx=loud\n=info\nsp ace=info\nshell=error\n  # indented\nnoequals\n",
        );
        assert_eq!(l.default, Some(Level::Warn));
        assert_eq!(
            l.level_for("shell", Level::Info),
            Level::Error,
            "last line wins"
        );
        assert_eq!(l.level_for("netd", Level::Info), Level::Trace);
        assert_eq!(l.level_for("other", Level::Info), Level::Warn, "falls to *");
        assert_eq!(
            l.level_for("x", Level::Info),
            Level::Warn,
            "x=loud was ignored"
        );
        assert_eq!(l.sources.len(), 2);
    }

    #[test]
    fn without_a_star_line_the_fallback_applies() {
        let l = Levels::parse("shell=debug\n");
        assert_eq!(l.level_for("other", Level::Info), Level::Info);
        assert_eq!(l.level_for("other", Level::Error), Level::Error);
    }

    #[test]
    fn missing_file_means_the_process_default() {
        let d = TempDir::new("ctl-missing");
        let mut c = Control::new(d.path().join("levels"), None);
        c.refresh(0);
        assert!(!c.refresh(5000), "still missing: nothing changed");
        assert_eq!(c.effective("shell"), Level::Info);
        let mut c = Control::new(d.path().join("levels"), Some(Level::Warn));
        c.refresh(0);
        assert_eq!(c.effective("shell"), Level::Warn);
    }

    #[test]
    fn reloads_only_when_due_and_changed() {
        let d = TempDir::new("ctl-reload");
        let p = d.path().join("levels");
        std::fs::write(&p, "shell=debug\n").unwrap();
        let mut c = Control::new(p.clone(), None);
        assert!(c.refresh(5000), "first look reads");
        assert_eq!(c.effective("shell"), Level::Debug);

        std::fs::write(&p, "shell=error\n*=trace\n").unwrap();
        assert!(!c.refresh(5500), "under a second: not looked at");
        assert_eq!(c.effective("shell"), Level::Debug);
        assert!(c.refresh(6000), "a second later: changed, read");
        assert_eq!(c.effective("shell"), Level::Error);
        assert_eq!(c.effective("netd"), Level::Trace);

        assert!(!c.refresh(7500), "looked at, unchanged: not read again");
        assert_eq!(c.effective("shell"), Level::Error);
    }

    #[test]
    fn deleting_the_file_goes_back_to_the_default() {
        let d = TempDir::new("ctl-delete");
        let p = d.path().join("levels");
        std::fs::write(&p, "*=trace\n").unwrap();
        let mut c = Control::new(p.clone(), None);
        c.refresh(0);
        assert_eq!(c.effective("a"), Level::Trace);
        std::fs::remove_file(&p).unwrap();
        assert!(c.refresh(1000));
        assert_eq!(c.effective("a"), Level::Info);
        std::fs::write(&p, "*=error\n").unwrap();
        assert!(c.refresh(2000));
        assert_eq!(c.effective("a"), Level::Error);
    }

    #[test]
    fn clock_going_backwards_still_rechecks() {
        let d = TempDir::new("ctl-back");
        let p = d.path().join("levels");
        std::fs::write(&p, "*=warn\n").unwrap();
        let mut c = Control::new(p.clone(), None);
        c.refresh(100_000);
        std::fs::write(&p, "*=debug and more\n*=debug\n").unwrap();
        assert!(c.refresh(10), "now < last: due");
        assert_eq!(c.effective("a"), Level::Debug);
    }
}
