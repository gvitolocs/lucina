//! Log levels. `Error` is the most severe and compares lowest, so a record is on when `level <= max`.

/// A record's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl Level {
    /// The name written to the `lvl` key.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }

    /// A level name, case-insensitive, surrounding blanks ignored. Anything else is `None`.
    pub fn parse(text: &str) -> Option<Level> {
        let t = text.trim();
        let table = [
            ("error", Level::Error),
            ("warn", Level::Warn),
            ("info", Level::Info),
            ("debug", Level::Debug),
            ("trace", Level::Trace),
        ];
        table
            .iter()
            .find(|(n, _)| t.eq_ignore_ascii_case(n))
            .map(|(_, l)| *l)
    }
}

/// The level a process starts at when nothing says otherwise.
pub const DEFAULT_LEVEL: Level = Level::Info;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_in_any_case() {
        assert_eq!(Level::parse("error"), Some(Level::Error));
        assert_eq!(Level::parse(" WARN "), Some(Level::Warn));
        assert_eq!(Level::parse("Info"), Some(Level::Info));
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("TRACE"), Some(Level::Trace));
    }

    #[test]
    fn rejects_other_text() {
        assert_eq!(Level::parse(""), None);
        assert_eq!(Level::parse("warning"), None);
        assert_eq!(Level::parse("3"), None);
        assert_eq!(Level::parse("in fo"), None);
    }

    #[test]
    fn severity_orders_error_lowest() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
        for l in [
            Level::Error,
            Level::Warn,
            Level::Info,
            Level::Debug,
            Level::Trace,
        ] {
            assert_eq!(Level::parse(l.as_str()), Some(l));
        }
    }
}
