//! lucina-app-coder: CodeCor on Juno, a native front end to every coding agent (phase A: the Projects and Project screens;
//! phase B: the New agent form; phase C: the Agent screen with Chat and Changes, diff approvals, and Sessions; phase D:
//! the intents of `manifest.app`, listed by `--intents`; phase E: the local Agent screen, `oc-cli acp` in CodeCor's own
//! chat, no terminal; phase F: `--check-local`, that same local turn headless, one line per event).
//! `LUCINA_APP_SAMPLE=1` runs on made-up agents and no account, and the title says so.
//! `--terminal [dir]` keeps the local Qwen mode of `docs/research/juno-coder.md` in the terminal, `--check-local <dir>
//! <answer> <prompt…>` proves the local chat on the device without a screen, and `--render <file> [screen]` draws a
//! screen with the sample data as a picture.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

mod account;
mod acp;
mod app;
mod check_local;
mod coordinator;
mod intents;
mod local;
mod new_agent;
mod projects;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use app::{CodeCor, Msg};
use lucina_agents_client::api::{Account, Api};
use lucina_app::Harness;
use projects::ProjectKey;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn account_api() -> Option<Arc<dyn Api>> {
    Some(Arc::new(Account(account::client()?)))
}

/// `--render <file> [what]`: the screen as a picture, with the sample data. `what` is `projects` (the default), `project`,
/// `new-agent` (the form on a Lucina project, Codex greyed out), `agent` (the Agent screen's Chat, with a pending
/// permission card carrying a diff), `changes` (its Changes tab) or `local` (the local Agent screen, with sample ACP
/// events including a diff permission).
pub(crate) fn render_to(path: &str, what: &str) {
    let api: Arc<dyn Api> = app::sample_api(now_ms());
    let mut h = Harness::new(CodeCor::new(Some(api), None).sample(), 1920, 1080);
    h.app().start();
    match what {
        "project" => {
            h.app()
                .open_project(ProjectKey::Remote("prj-sample".to_owned()));
        }
        "new-agent" => {
            let key = ProjectKey::Remote("prj-lucina".to_owned());
            h.app().open_project(key.clone());
            h.app().open_new_agent(key, new_agent::Role::Worker);
            if let Some(f) = h.app().new_agent.as_mut() {
                f.title.set("Fix the login redirect");
                f.prompt
                    .set("The login redirect loops back to the form. Fix it and run the tests.");
            }
        }
        "local" => {
            h.app()
                .open_local_sample(PathBuf::from("/data/projects/demo-app"));
        }
        "agent" | "changes" => {
            let key = ProjectKey::Remote("prj-sample".to_owned());
            h.app().open_project(key.clone());
            h.app().open_agent(key, "agt_sample_running".to_owned());
            if what == "changes" {
                h.app().switch_agent_tab(
                    "agt_sample_running",
                    lucina_agents_client::agent::Tab::Changes,
                );
            }
        }
        _ => {}
    }
    for _ in 0..3 {
        h.core.input(lucina_app::Input::Wheel(0));
        h.core.paint();
    }
    std::fs::write(path, h.ppm()).expect("write the picture");
}

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // The local Qwen mode, in the terminal: the launcher of docs/research/juno-coder.md, unchanged.
        Some("--terminal") => std::process::exit(local::run(args.next().map(PathBuf::from))),
        // The local Agent screen's turn, headless: one line per event, no model text.
        Some("--check-local") => {
            let rest: Vec<String> = args.collect();
            std::process::exit(check_local::main_args(&rest))
        }
        Some("--render") => {
            let path = args.next().unwrap_or_else(|| "codecor.ppm".into());
            render_to(&path, &args.next().unwrap_or_default());
        }
        _ => lucina_app::run("lucina-app-coder", |tx| {
            if std::env::var_os("LUCINA_APP_SAMPLE").is_some() {
                let api: Arc<dyn Api> = app::sample_api(now_ms());
                let mut c = CodeCor::new(Some(api), Some(tx)).sample();
                c.start();
                return c;
            }
            let signin_tx = tx.clone();
            let api = account_api();
            let signed = api.is_some();
            let sign_in: Box<dyn FnMut() -> lucina_account::ui::SignInView> = Box::new(move || {
                let (a, b) = (signin_tx.clone(), signin_tx.clone());
                account::sign_in(
                    Arc::new(move || a.send(Msg::Changed)),
                    Arc::new(move || b.send(Msg::SignedIn)),
                )
            });
            let mut c = CodeCor::new(api, Some(tx)).with_sign_in(sign_in, Box::new(account_api));
            if signed {
                c.start();
            }
            c
        }),
    }
}
