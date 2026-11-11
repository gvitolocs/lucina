//! The local Qwen mode, unchanged from when the whole app was this: the model stays on buildhost; this program sets up what the
//! agent needs and runs it: `codecor.pkg` (static `oc-cli`, from `agent/build.sh`) copied to RAM, the machine's own SSH key,
//! the forward to Qwen's gateway, restarted if it dies, and `oc-cli chat`. See `docs/research/juno-coder.md`.
//!
//! Reached as `lucina-app-coder --terminal [dir]`: CodeCor's "Open in terminal (local Qwen)" shows that line
//! ([`terminal_command_line`]) for the owner to type in Juno's terminal (F2). The shell has no request that types a command
//! with arguments for an app, on purpose: `open` refuses arguments for terminal programs. The app's own chat screen drives
//! `oc-cli acp` instead ([`start_acp`], the ACP client in `crate::acp`), so the local mode needs no terminal at all.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const RUN_DIR: &str = "/tmp/lucina-coder";
const PKG: &str = "codecor.pkg";
/// Where the key and the sessions stay across a restart, on a machine that runs from its root partition.
const DATA_DIR: &str = "/data/lucinaos/coder";
/// Otherwise in RAM, and each restart needs buildhost to learn a new key.
const RAM_DIR: &str = "/run/lucina/coder";

/// buildhost's sshd, the LAN route. The away route needs this key on the Oracle jump too, which isn't decided.
const ROUTES: &[&str] = &["nez@192.168.1.55"];
/// buildhost's host key: anything else is refused.
const HOST_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIP0nWtRoCfIqFO284ZLnsuulijlquk9Z6g17iN7mLz2U";
/// The gateway's own API key, put here by `scripts/juno-coder-authorize.sh`: a private file, sent only through the forward.
const GATEWAY_KEY: &str = "gateway_key";
const AUTHORIZE_OPTIONS: &str = "restrict,port-forwarding,permitopen=\"127.0.0.1:11436\",from=\"192.168.1.0/24\",command=\"/usr/sbin/nologin\"";

/// The agent's settings for Qwen behind the forward. The same values as `qcode`: thinking medium, compacting at 60%.
const AGENT_ENV: &[(&str, &str)] = &[
    ("OC_BASE_URL", "http://127.0.0.1:11436/v1"),
    ("OC_MODEL", "openai/qwen3.8:27b-q4-72k"),
    ("OC_CONTEXT_WINDOW", "81920"),
    ("OC_COMPACT_BUFFER", "32768"),
    (
        "OC_EXTRA_BODY",
        r#"{"chat_template_kwargs":{"enable_thinking":true,"reasoning_effort":"medium"},"temperature":1.0,"top_p":0.95,"top_k":20,"min_p":0.0,"presence_penalty":0.0}"#,
    ),
];

/// Looking needs no question. Changing files and running commands ask, as upstream asks for everything.
const PERMISSIONS: &str = r#"[{"action":"read","resource":"*","effect":"allow"},{"action":"glob","resource":"*","effect":"allow"},{"action":"grep","resource":"*","effect":"allow"},{"action":"todowrite","resource":"*","effect":"allow"}]"#;

/// `SIGTERM` or the terminal going away: everything started here stops.
static LEAVE: AtomicBool = AtomicBool::new(false);

extern "C" fn on_leave(_: libc::c_int) {
    LEAVE.store(true, Ordering::SeqCst);
}

fn state_dir(data_mounted: bool) -> PathBuf {
    PathBuf::from(if data_mounted { DATA_DIR } else { RAM_DIR })
}

/// The agent as a program in RAM: the partition the package is on may be unmounted while it runs.
fn install_agent(app_dir: &Path, run_dir: &Path) -> Result<PathBuf, String> {
    let pkg = app_dir.join(PKG);
    let agent = run_dir.join("oc-cli");
    let size = std::fs::metadata(&pkg)
        .map_err(|e| format!("{}: {e}", pkg.display()))?
        .len();
    if std::fs::metadata(&agent).is_ok_and(|m| m.len() == size) {
        return Ok(agent);
    }
    std::fs::create_dir_all(run_dir).map_err(|e| e.to_string())?;
    let tmp = run_dir.join("oc-cli.tmp");
    std::fs::copy(&pkg, &tmp).map_err(|e| format!("copy {}: {e}", pkg.display()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &agent).map_err(|e| e.to_string())?;
    Ok(agent)
}

/// The machine's key, made here the first time; it never leaves this file. Returns the public line.
fn ensure_key(agent: &Path, dir: &Path) -> Result<(PathBuf, String), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let (key, public) = (dir.join("tunnel_key"), dir.join("tunnel_key.pub"));
    if !key.exists() {
        let out = Command::new(agent)
            .args(["tunnel", "keygen"])
            .arg(&key)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
        }
        std::fs::write(&public, &out.stdout).map_err(|e| e.to_string())?;
    }
    let line =
        std::fs::read_to_string(&public).map_err(|e| format!("{}: {e}", public.display()))?;
    Ok((key, line.trim().to_string()))
}

fn tunnel_args(key: &Path, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = ["tunnel", "run", "--key"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push(key.to_string_lossy().into_owned());
    args.extend(["--known-host".to_string(), HOST_KEY.to_string()]);
    for r in ROUTES {
        args.extend(["--route".to_string(), r.to_string()]);
    }
    args.extend(extra.iter().map(|s| s.to_string()));
    args
}

fn authorize_line(public: &str) -> String {
    format!("{AUTHORIZE_OPTIONS} {public} juno-coder-tunnel")
}

/// A single try, to say what is wrong before the first question to the model. The `Err` is the sentence `--terminal`
/// prints, so the app's chat screen can show the same one.
fn check_route(agent: &Path, key: &Path, public: &str) -> Result<(), String> {
    let out = Command::new(agent)
        .args(tunnel_args(key, &["--check"]))
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let why = String::from_utf8_lossy(&o.stderr);
            let mut msg = format!("Can't reach Qwen on buildhost:\n{}", why.trim());
            if why.contains("refused the key") {
                msg += &format!(
                    "\nbuildhost doesn't know this machine's key yet: run scripts/juno-coder-authorize.sh from the Mac, which adds this line to its ~/.ssh/authorized_keys:\n{}",
                    authorize_line(public)
                );
            }
            Err(msg)
        }
        Err(e) => Err(format!("can't run the tunnel: {e}")),
    }
}

/// The gateway's key, or the sentence `--terminal` prints when it is missing.
fn gateway_key(state: &Path) -> Result<PathBuf, String> {
    let k = state.join(GATEWAY_KEY);
    if !k.exists() {
        return Err(format!(
            "No key for Qwen's gateway at {}: run scripts/juno-coder-authorize.sh from the Mac.",
            k.display()
        ));
    }
    Ok(k)
}

/// What was set up to run the agent: the RAM copy, the key, the state dir.
#[derive(Debug)]
struct Prepared {
    agent: PathBuf,
    key: PathBuf,
    state: PathBuf,
    data: bool,
}

/// Everything both modes do before the agent starts: the package copied to RAM, the machine's SSH key, the route
/// check, the gateway key. The `Err` sentences are the ones `--terminal` prints.
fn prepare(app_dir: &Path, run_dir: &Path) -> Result<Prepared, String> {
    let agent =
        install_agent(app_dir, run_dir).map_err(|e| format!("The agent is not installed: {e}"))?;
    let data = Path::new("/data").is_dir();
    let state = state_dir(data);
    let (key, public) = ensure_key(&agent, &state).map_err(|e| format!("No SSH key: {e}"))?;
    check_route(&agent, &key, &public)?;
    gateway_key(&state)?;
    Ok(Prepared {
        agent,
        key,
        state,
        data,
    })
}

/// The agent's environment, the same for `chat` and `acp`: the gateway key file, the settings for Qwen behind the
/// forward, the permissions default, and the sessions under the state dir when /data is there.
fn agent_env(cmd: &mut Command, gateway_key: &Path, state: &Path, data: bool) {
    cmd.env_remove("OC_BASE")
        .env("OC_API_KEY_FILE", gateway_key)
        .envs(AGENT_ENV.iter().copied());
    if std::env::var_os("OC_PERMISSIONS").is_none() {
        cmd.env("OC_PERMISSIONS", PERMISSIONS);
    }
    if data {
        cmd.env("OC_DATA", state.join("sessions"));
    }
}

fn start_tunnel(agent: &Path, key: &Path, log: &Path) -> Option<Child> {
    let output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .ok()?;
    let mut cmd = Command::new(agent);
    cmd.args(tunnel_args(key, &[]))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(output);
    // Its own group: Ctrl+C at the agent is not for the forward.
    cmd.process_group(0);
    cmd.spawn().ok()
}

/// The forward for as long as `stop` is unset; it comes back if it dies.
fn supervise_tunnel(
    agent: PathBuf,
    key: PathBuf,
    log: PathBuf,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut child = start_tunnel(&agent, &key, &log);
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(250));
            let gone = match child.as_mut() {
                Some(c) => !matches!(c.try_wait(), Ok(None)),
                None => true,
            };
            if gone && !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(2));
                child = start_tunnel(&agent, &key, &log);
            }
        }
        if let Some(mut c) = child {
            let _ = c.kill();
            let _ = c.wait();
        }
    })
}

/// The line the shell types into its terminal for a program that runs there (`lucina-shell apps.rs terminal_command`), with
/// this program's own arguments after its path.
pub fn terminal_command_line(exe: &str, app_dir: &str, work: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    format!(
        "LUCINA_APP_DIR={} {} --terminal {}\n",
        quote(app_dir),
        quote(exe),
        quote(work)
    )
}

/// `--terminal [dir]`: the local Qwen mode. `work` is the project folder, `/data/projects` (or `/tmp/projects` without /data)
/// when it is not given.
pub fn run(work: Option<PathBuf>) -> i32 {
    // SAFETY: the handler only stores to an atomic. SIGINT is the agent's: it stops the turn, not this program.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, on_leave as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, on_leave as *const () as libc::sighandler_t);
    }
    let app_dir = std::env::var_os("LUCINA_APP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let run_dir = Path::new(RUN_DIR);
    let p = match prepare(&app_dir, run_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let gateway_key = gateway_key(&p.state).expect("prepare checked it");
    let work = work.unwrap_or_else(|| {
        PathBuf::from(if p.data {
            "/data/projects"
        } else {
            "/tmp/projects"
        })
    });
    if let Err(e) = std::fs::create_dir_all(&work) {
        eprintln!("{}: {e}", work.display());
        return 1;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let tunnel = supervise_tunnel(
        p.agent.clone(),
        p.key,
        run_dir.join("tunnel.log"),
        stop.clone(),
    );
    let mut chat = Command::new(&p.agent);
    chat.arg("chat").arg(&work);
    agent_env(&mut chat, &gateway_key, &p.state, p.data);
    // SAFETY: only async-signal-safe calls between fork and exec: the agent gets the default for what this program ignores.
    unsafe {
        chat.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
    let code = match chat.spawn() {
        Ok(mut c) => loop {
            if LEAVE.load(Ordering::SeqCst) {
                // SAFETY: a plain signal to a child still waited for.
                unsafe { libc::kill(c.id() as libc::pid_t, libc::SIGTERM) };
                let _ = c.wait();
                break 143;
            }
            match c.try_wait() {
                Ok(Some(status)) => break status.code().unwrap_or(1),
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(e) => {
                    eprintln!("{e}");
                    break 1;
                }
            }
        },
        Err(e) => {
            eprintln!("can't start the agent: {e}");
            1
        }
    };
    stop.store(true, Ordering::SeqCst);
    let _ = tunnel.join();
    code
}

/// A local agent running in CodeCor's own chat screen: the tunnel supervisor and the `oc-cli acp` child. Dropping it
/// stops the child and the tunnel.
pub struct LocalAgent {
    /// The ACP session with the child.
    pub acp: crate::acp::Acp,
    stop: Arc<AtomicBool>,
    tunnel: Option<std::thread::JoinHandle<()>>,
}

impl LocalAgent {
    /// Without a tunnel, for the tests and the sample.
    #[cfg(test)]
    pub fn from_acp(acp: crate::acp::Acp) -> LocalAgent {
        LocalAgent {
            acp,
            stop: Arc::new(AtomicBool::new(false)),
            tunnel: None,
        }
    }
}

impl Drop for LocalAgent {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.tunnel.take() {
            let _ = t.join();
        }
        // `acp`'s own Drop stops the child.
    }
}

/// The same preparation as `--terminal` (package copy to RAM, the machine's SSH key, the route check, the gateway key,
/// the tunnel supervisor), then `<ram copy of oc-cli> acp` in `work`, with the same env as `oc-cli chat` (AGENT_ENV,
/// OC_API_KEY_FILE, OC_DATA under the state dir's sessions; PERMISSIONS default). The `Err` sentences are the ones
/// `--terminal` prints.
pub fn start_acp(work: &Path) -> Result<LocalAgent, String> {
    let app_dir = std::env::var_os("LUCINA_APP_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let run_dir = Path::new(RUN_DIR);
    let p = prepare(&app_dir, run_dir)?;
    let gateway_key = gateway_key(&p.state)?;
    std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    let stop = Arc::new(AtomicBool::new(false));
    let tunnel = supervise_tunnel(
        p.agent.clone(),
        p.key,
        run_dir.join("tunnel.log"),
        stop.clone(),
    );
    let mut cmd = Command::new(&p.agent);
    cmd.arg("acp").current_dir(work);
    agent_env(&mut cmd, &gateway_key, &p.state, p.data);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            Ok(())
        });
    }
    match crate::acp::Acp::spawn(&mut cmd) {
        Ok(acp) => Ok(LocalAgent {
            acp,
            stop,
            tunnel: Some(tunnel),
        }),
        Err(e) => {
            stop.store(true, Ordering::SeqCst);
            let _ = tunnel.join();
            Err(format!("can't start the agent: {e}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorized_line_allows_only_the_gateway_and_no_shell() {
        let line = authorize_line("ssh-ed25519 AAAA");
        assert!(line.starts_with("restrict,port-forwarding,permitopen=\"127.0.0.1:11436\","));
        assert!(line.contains("command=\"/usr/sbin/nologin\""));
        assert!(line.ends_with("ssh-ed25519 AAAA juno-coder-tunnel"));
    }

    #[test]
    fn state_is_on_data_only_when_it_is_there() {
        assert_eq!(state_dir(true), Path::new(DATA_DIR));
        assert_eq!(state_dir(false), Path::new(RAM_DIR));
    }

    #[test]
    fn the_server_key_is_always_pinned() {
        let args = tunnel_args(Path::new("/k"), &["--check"]);
        let at = args.iter().position(|a| a == "--known-host").unwrap();
        assert!(args[at + 1].starts_with("ssh-ed25519 "));
        assert!(args.iter().any(|a| a == "--route"));
    }

    #[test]
    fn the_package_is_copied_once_and_runs_from_ram() {
        let (app, run) = (tempdir("app"), tempdir("run"));
        std::fs::write(app.join(PKG), b"program").unwrap();
        let agent = install_agent(&app, &run).unwrap();
        assert_eq!(std::fs::read(&agent).unwrap(), b"program");
        assert_eq!(
            std::fs::metadata(&agent).unwrap().permissions().mode() & 0o111,
            0o111
        );
        std::fs::remove_file(app.join(PKG)).unwrap();
        assert!(install_agent(&app, &run).is_err(), "no package, no agent");
    }

    #[test]
    fn the_key_is_made_once() {
        let dir = tempdir("key");
        let fake = dir.join("fake-oc");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho private > \"$3\"\necho ssh-ed25519 AAAAtest\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let state = dir.join("state");
        let (key, public) = ensure_key(&fake, &state).unwrap();
        assert_eq!(public, "ssh-ed25519 AAAAtest");
        assert_eq!(std::fs::read_to_string(&key).unwrap().trim(), "private");
        std::fs::write(&fake, "#!/bin/sh\nexit 9\n").unwrap();
        assert_eq!(ensure_key(&fake, &state).unwrap().1, "ssh-ed25519 AAAAtest");
    }

    #[test]
    fn the_terminal_line_quotes_like_the_shell_and_adds_the_dir() {
        assert_eq!(
            terminal_command_line(
                "/tmp/lucina-apps/coder",
                "/usr/share/lucina/apps",
                "/data/projects/demo"
            ),
            "LUCINA_APP_DIR='/usr/share/lucina/apps' '/tmp/lucina-apps/coder' --terminal '/data/projects/demo'\n"
        );
        assert_eq!(
            terminal_command_line("/a'b", "/d", "/w'x"),
            "LUCINA_APP_DIR='/d' '/a'\\''b' --terminal '/w'\\''x'\n"
        );
    }

    #[test]
    fn the_preparation_says_the_same_sentences_as_the_terminal() {
        let app = tempdir("app");
        let run = tempdir("run");
        // No codecor.pkg in the app dir: the terminal's first sentence.
        let e = prepare(&app, &run).unwrap_err();
        assert!(e.starts_with("The agent is not installed: "), "{e}");
    }

    #[test]
    fn the_missing_gateway_key_sentence_names_the_script() {
        let state = tempdir("state");
        let e = gateway_key(&state).unwrap_err();
        assert!(e.starts_with("No key for Qwen's gateway at "), "{e}");
        assert!(e.contains("scripts/juno-coder-authorize.sh"), "{e}");
        std::fs::write(state.join(GATEWAY_KEY), b"k").unwrap();
        assert_eq!(gateway_key(&state).unwrap(), state.join(GATEWAY_KEY));
    }

    fn tempdir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "lucina-coder-test-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
