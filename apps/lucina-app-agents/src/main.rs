//! lucina-app-agents: Lucina Agents on Juno (`web/src/views/Agents.tsx`), as a program of its own on the toolkit.
//! `LUCINA_APP_SAMPLE=1` runs on made-up agents and no account, and the title says so.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod app;
#[cfg(test)]
mod tests;

pub use lucina_agents_client::{api, model, pages, store};

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use api::{Account, Api, Sample};
use app::{Agents, Msg, Section, Tab};
use lucina_app::Harness;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn account_api() -> Option<Arc<dyn Api>> {
    Some(Arc::new(Account(account::client()?)))
}

/// `--render <file> [what]`: the screen as a picture, with the sample data.
fn render(path: &str, what: &str) {
    let api: Arc<dyn Api> = Arc::new(Sample::new(now_ms()));
    let mut h = Harness::new(Agents::new(Some(api), None).sample(), 1920, 1080);
    h.app().start();
    let id = "agt_sample_running".to_owned();
    match what {
        "permissions" => h.app().switch(Section::Permissions),
        "schedules" => h.app().switch(Section::Schedules),
        "usage" => h.app().switch(Section::Usage),
        "pipeline" => h.app().switch(Section::Pipeline),
        "new" => h.app().open_new_run(),
        "changes" | "files" | "terminal" | "scripts" => {
            h.app().open_agent(id);
            h.app().switch_tab(match what {
                "changes" => Tab::Changes,
                "files" => Tab::Files,
                "terminal" => Tab::Terminal,
                _ => Tab::Scripts,
            });
        }
        "" => {}
        _ => h.app().open_agent(id),
    }
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    std::fs::write(path, h.ppm()).expect("write");
}

/// The `id`s (or `provider`s) of a list, whether it comes bare or under one key.
fn list_of(v: &serde_json::Value, key: &str) -> Vec<serde_json::Value> {
    v.as_array().or_else(|| v.get(key).and_then(|l| l.as_array())).cloned().unwrap_or_default()
}

/// `--check [seconds]`: the real route, without the screen (CodeCor M0): the machine's sign-in, p1's `/v1/agents`, agentd.
/// Prints counts and provider ids only: no titles, prompts or tokens.
fn check(secs: u64) -> i32 {
    let Some(api) = account_api() else {
        eprintln!("check: this machine is not signed in");
        return 2;
    };
    let t = std::time::Instant::now();
    let providers = match api.call("GET", "/v1/agents/providers", None) {
        Ok(v) => list_of(&v, "providers"),
        Err(e) => {
            eprintln!("check: providers: {e:?}");
            return 1;
        }
    };
    let ids: Vec<String> = providers
        .iter()
        .filter_map(|p| p.get("id").or_else(|| p.get("provider")).and_then(|s| s.as_str()).map(str::to_owned))
        .collect();
    println!("providers {} in {} ms: {}", ids.len(), t.elapsed().as_millis(), ids.join(" "));
    let t = std::time::Instant::now();
    match api.call("GET", "/v1/agents/items", None) {
        Ok(v) => {
            let items = list_of(&v, "items");
            let mut by: std::collections::BTreeMap<String, usize> = Default::default();
            for i in &items {
                let s = i.get("status").or_else(|| i.get("agent").and_then(|a| a.get("status")));
                *by.entry(s.and_then(|s| s.as_str()).unwrap_or("?").to_owned()).or_default() += 1;
            }
            println!("items {} in {} ms: {by:?}", items.len(), t.elapsed().as_millis());
        }
        Err(e) => {
            eprintln!("check: items: {e:?}");
            return 1;
        }
    }
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    let stream_api = api.clone();
    let t = std::time::Instant::now();
    std::thread::spawn(move || {
        let mut s = match stream_api.stream("/v1/agents/stream", None) {
            Ok(s) => s,
            Err(e) => return drop(tx.send(Err(format!("{e:?}")))),
        };
        loop {
            match s.next_block() {
                Ok(Some(b)) => {
                    let kind = serde_json::from_str::<serde_json::Value>(&b.data)
                        .ok()
                        .and_then(|v| v.get("type").and_then(|k| k.as_str()).map(str::to_owned))
                        .unwrap_or(if b.event.is_empty() { "message".into() } else { b.event.clone() });
                    if tx.send(Ok(kind)).is_err() {
                        return;
                    }
                }
                Ok(None) => return drop(tx.send(Err("server ended the stream".into()))),
                Err(e) => return drop(tx.send(Err(format!("{e:?}")))),
            }
        }
    });
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    let mut first_ms = None;
    let end = t + std::time::Duration::from_secs(secs);
    let mut code = 0;
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Ok(kind)) => {
                first_ms.get_or_insert(t.elapsed().as_millis());
                *kinds.entry(kind).or_default() += 1;
            }
            Ok(Err(why)) => {
                eprintln!("check: stream: {why}");
                code = 1;
                break;
            }
            Err(_) => break,
        }
    }
    println!("stream {secs} s: first event after {first_ms:?} ms, events {kinds:?}");
    code
}

fn main() {
    let mut args = std::env::args().skip(1);
    let first = args.next();
    if first.as_deref() == Some("--check") {
        std::process::exit(check(args.next().and_then(|s| s.parse().ok()).unwrap_or(20)));
    }
    if first.as_deref() == Some("--render") {
        let path = args.next().unwrap_or_else(|| "agents.ppm".into());
        render(&path, &args.next().unwrap_or_default());
        return;
    }
    lucina_app::run("lucina-app-agents", |tx| {
        if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
            let api: Arc<dyn Api> = Arc::new(Sample::new(now_ms()));
            let mut a = Agents::new(Some(api), Some(tx)).sample();
            a.start();
            return a;
        }
        let signin_tx = tx.clone();
        let api = account_api();
        let signed = api.is_some();
        let sign_in: Box<dyn FnMut() -> lucina_account::ui::SignInView> = Box::new(move || {
            let (a, b) = (signin_tx.clone(), signin_tx.clone());
            account::sign_in(Arc::new(move || a.send(Msg::Changed)), Arc::new(move || b.send(Msg::SignedIn)))
        });
        let mut a = Agents::new(api, Some(tx)).with_sign_in(sign_in, Box::new(account_api));
        if signed {
            a.start();
        }
        a
    });
}
