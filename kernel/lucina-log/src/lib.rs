//! Juno's user-space log library. Design: `docs/design/juno-logging.md`.
//!
//! One record is one JSON line of at most 2048 bytes, sent as a datagram to `/run/lucina/log.sock`:
//!
//! `{"t":<wall ms>,"m":<ms since boot>,"boot":"<32 hex>","seq":<n>,"lvl":"info","src":"shell","pid":<n>,"msg":"...","f":{"k":"v"}}`
//!
//! ```ignore
//! lucina_log::Logger::init("shell");
//! lucina_log::log_info!("shell: started {} windows", n);          // like eprintln!("shell: started {} windows", n)
//! lucina_log::log_warn!("link down"; iface = "wlan0", retries = 3);
//! lucina_log::log_error!("login failed"; user = name, password = lucina_log::redacted(&pw));
//! ```
//!
//! Fields come after a `;` (a `name = value` before it would be a format argument). Format arguments are not evaluated when
//! the level is off. A secret can only be logged through [`redacted`], which prints `"<redacted>"` and keeps nothing.
//!
//! - [`rules`]: the rate limiter, the re-read rule, the key alphabet and the byte budget, Verus-checked as shipped.
//! - [`level`]: the five levels.
//! - [`value`]: field values and [`Redacted`].
//! - [`encode`]: the JSON line, escaping and the 2048-byte cut.
//! - [`control`]: `/run/lucina/log/levels`, `LUCINA_LOG` and the lazy re-read (no timer).
//! - [`logger`]: level check, rate limit, socket send, stderr fallback.

pub mod control;
pub mod encode;
pub mod level;
pub mod logger;
pub mod rules;
pub mod value;

pub use level::Level;
pub use logger::{Config, Logger, global};
pub use value::{Redacted, Secret, Value, redacted};

/// Logs at an explicit level. Used by the `log_*` macros.
#[macro_export]
macro_rules! log_at {
    ($lvl:expr, $fmt:literal $(, $arg:expr)* ; $($k:ident = $v:expr),+ $(,)?) => {{
        let __lg = $crate::global();
        let __lvl: $crate::Level = $lvl;
        if __lg.enabled(__lvl) {
            __lg.emit(__lvl, ::core::format_args!($fmt $(, $arg)*), &[$((::core::stringify!($k), $crate::Value::from($v))),+]);
        }
    }};
    ($lvl:expr, $fmt:literal $(, $arg:expr)* $(,)?) => {{
        let __lg = $crate::global();
        let __lvl: $crate::Level = $lvl;
        if __lg.enabled(__lvl) {
            __lg.emit(__lvl, ::core::format_args!($fmt $(, $arg)*), &[]);
        }
    }};
}

/// Logs at `error`. See the crate docs for the syntax.
#[macro_export]
macro_rules! log_error { ($($t:tt)+) => { $crate::log_at!($crate::Level::Error, $($t)+) }; }
/// Logs at `warn`.
#[macro_export]
macro_rules! log_warn { ($($t:tt)+) => { $crate::log_at!($crate::Level::Warn, $($t)+) }; }
/// Logs at `info`.
#[macro_export]
macro_rules! log_info { ($($t:tt)+) => { $crate::log_at!($crate::Level::Info, $($t)+) }; }
/// Logs at `debug`.
#[macro_export]
macro_rules! log_debug { ($($t:tt)+) => { $crate::log_at!($crate::Level::Debug, $($t)+) }; }
/// Logs at `trace`.
#[macro_export]
macro_rules! log_trace { ($($t:tt)+) => { $crate::log_at!($crate::Level::Trace, $($t)+) }; }

#[cfg(test)]
mod macro_tests {
    use super::*;

    #[test]
    fn macros_accept_the_eprintln_patterns() {
        // The global logger points at /run/lucina; here nothing listens, so these fall back to stderr.
        let x = 5;
        let name = "n";
        log_info!("shell: text {x}");
        log_info!("shell: text {} {}", x, name);
        log_warn!("plain",);
        log_error!("with fields {x}"; a = 1, b = "two", c = true, d = redacted("s"));
        log_debug!("trailing comma"; a = 1,);
        log_trace!("fmt {}", x; k = x);
    }

    #[test]
    fn level_off_means_arguments_are_not_evaluated() {
        fn boom() -> i32 {
            panic!("evaluated")
        }
        // trace is off by default (info)
        log_trace!("never {}", boom());
        log_debug!("never"; k = boom());
    }
}
