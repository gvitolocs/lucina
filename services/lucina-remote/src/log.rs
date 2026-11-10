//! A logger for the `log` facade (which sunset uses) that writes `name: message` lines to stderr.

use std::io::Write;

struct StderrLogger {
    name: &'static str,
}

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            // One write per line, so that the lines of several threads do not mix.
            let line = format!("{}: {} {}\n", self.name, record.level(), record.args());
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {}
}

/// Installs the logger; `level` is the most verbose level shown.
pub fn init(name: &'static str, level: log::LevelFilter) {
    let logger = Box::leak(Box::new(StderrLogger { name }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}
