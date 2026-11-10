//! `lucina-intent`: the `intents`, `intent` and `undo` calls of lucina-devd, from a shell on the laptop, for checks before
//! Lucina's bridge reaches them. The same code path as devd (`devd::Intents::run`), the same journal, the same audit fields.
//!
//! ```text
//! lucina-intent intents [app]
//! lucina-intent intent <app.verb> ['<json args>'] [--decided]
//! lucina-intent undo [action]
//! ```
//! `--decided` stands for a decision the server checked; only use it after one, as lucina-rs would.
//! `LUCINA_INTENT_MANIFESTS` (folders, `:`-separated) replaces the laptop's app folders.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_intent_host::devd::{self, Intents, ShellHost};
use lucina_intents::journal::Journal;
use serde_json::{Value, json};

/// The folders she may change through an undo: the owner's files. lucina-devd uses its own places and off-limits list.
const PLACES: &[&str] = &["/data/home", "/data/music", "/data/recordings", "/run/lucina/files"];

fn allowed(p: &Path) -> Result<(), String> {
    let s = p.to_string_lossy();
    if lucina_intents::rules::path_ok(s.as_bytes()) && PLACES.iter().any(|d| p.starts_with(d)) {
        Ok(())
    } else {
        Err(format!("{}: not under {}", p.display(), PLACES.join(", ")))
    }
}

fn manifest_dirs() -> Vec<PathBuf> {
    match std::env::var_os("LUCINA_INTENT_MANIFESTS") {
        Some(v) => std::env::split_paths(&v).collect(),
        None => devd::laptop_manifest_dirs(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    let call_id = format!("cli_{now_ms}");
    let decided = args.iter().any(|a| a == "--decided");
    let rest: Vec<&str> = args.iter().map(String::as_str).filter(|a| *a != "--decided").collect();
    let (tool, targs) = match rest.as_slice() {
        ["intents"] => ("intents", json!({})),
        ["intents", app] => ("intents", json!({"app": app})),
        ["intent", name] => ("intent", json!({"name": name, "args": {}})),
        ["intent", name, a] => match serde_json::from_str::<Value>(a) {
            Ok(v) => ("intent", json!({"name": name, "args": v})),
            Err(e) => {
                eprintln!("lucina-intent: the arguments are not JSON: {e}");
                std::process::exit(2);
            }
        },
        ["undo"] => ("undo", json!({})),
        ["undo", action] => ("undo", json!({"action": action})),
        _ => {
            eprintln!("usage: lucina-intent intents [app] | intent <app.verb> ['<json>'] [--decided] | undo [action]");
            std::process::exit(2);
        }
    };
    let host = ShellHost { socket: PathBuf::from(devd::SHELL_SOCKET), allowed: Box::new(allowed) };
    // Off the laptop (a check on buildhost): LUCINA_UNDO_DIR, LUCINA_INTENT_REPLY_DIR and LUCINA_SHELL_SOCKET point elsewhere.
    let env_or = |k: &str, d: &str| std::env::var_os(k).map_or_else(|| PathBuf::from(d), PathBuf::from);
    let host = ShellHost { socket: env_or("LUCINA_SHELL_SOCKET", devd::SHELL_SOCKET), ..host };
    let intents = Intents::new(
        host,
        Journal::at(&env_or("LUCINA_UNDO_DIR", devd::UNDO_DIR)),
        manifest_dirs(),
        env_or("LUCINA_INTENT_REPLY_DIR", devd::REPLY_DIR),
        Duration::from_secs(60),
    );
    match intents.run(tool, &targs, &call_id, decided, now_ms) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            if v["ok"] == json!(false) {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("lucina-intent: {e}");
            std::process::exit(1);
        }
    }
}
