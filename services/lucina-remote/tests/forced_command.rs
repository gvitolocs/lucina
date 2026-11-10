//! lucina-sshd and juno-wifi-push on the host, with OpenSSH as the client: a key with `command=` runs that and nothing else,
//! and a dummy passphrase crosses to its stdin without showing up anywhere else.

use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

const DUMMY: &str = "dummy-passphrase-0123";

struct Rig {
    dir: PathBuf,
    port: u16,
    server: Child,
    server_log: PathBuf,
    restricted: PathBuf,
    full: PathBuf,
    known_hosts: PathBuf,
    received: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn keygen(path: &Path) -> String {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "test", "-f"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    fs::read_to_string(path.with_extension("pub")).unwrap()
}

fn rig(name: &str) -> Option<Rig> {
    if Command::new("ssh")
        .arg("-V")
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        eprintln!("skipped: no OpenSSH client");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("lucina-sshd-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let restricted = dir.join("restricted");
    let full = dir.join("full");
    let host = dir.join("host");
    let restricted_pub = keygen(&restricted);
    let full_pub = keygen(&full);
    let host_pub = keygen(&host);
    let received = dir.join("received");
    let authorized = dir.join("authorized_keys");
    fs::write(
        &authorized,
        format!(
            "restrict,command=\"cat > {} ; echo forced ; tty -s && echo has-tty ; echo env=$LC_TEST\" {restricted_pub}{full_pub}",
            received.display()
        ),
    )
    .unwrap();
    let known_hosts = dir.join("known_hosts");
    let mut fields = host_pub.split_whitespace();
    fs::write(
        &known_hosts,
        format!(
            "juno {} {}\n",
            fields.next().unwrap(),
            fields.next().unwrap()
        ),
    )
    .unwrap();
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let server_log = dir.join("server.log");
    let log = fs::File::create(&server_log).unwrap();
    let server = Command::new(env!("CARGO_BIN_EXE_lucina-sshd"))
        .args([
            "--listen",
            &format!("127.0.0.1:{port}"),
            "--shell",
            "/bin/sh",
            "-v",
        ])
        .arg("--host-key")
        .arg(&host)
        .arg("--authorized-keys")
        .arg(&authorized)
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap();
    let started = Instant::now();
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the server did not start"
        );
        sleep(Duration::from_millis(20));
    }
    Some(Rig {
        dir,
        port,
        server,
        server_log,
        restricted,
        full,
        known_hosts,
        received,
    })
}

impl Rig {
    fn ssh(&self, key: &Path, extra: &[&str], command: &[&str], stdin: &str) -> (bool, String) {
        use std::io::Write;
        let mut child = Command::new("ssh")
            .args(["-F", "none", "-i"])
            .arg(key)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=yes",
            ])
            .arg("-o")
            .arg(format!("UserKnownHostsFile={}", self.known_hosts.display()))
            .args([
                "-o",
                "HostKeyAlias=juno",
                "-o",
                "LogLevel=ERROR",
                "-o",
                "SetEnv=LC_TEST=sneaked",
            ])
            .args(["-p", &self.port.to_string()])
            .args(extra)
            .arg("root@127.0.0.1")
            .args(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }
}

#[test]
fn a_restricted_key_runs_its_command_whatever_is_asked() {
    let Some(r) = rig("forced") else { return };
    let marker = r.dir.join("escaped");
    let (ok, out) = r.ssh(
        &r.restricted,
        &[],
        &[&format!("touch {}", marker.display())],
        "from stdin\n",
    );
    assert!(ok, "{out}");
    assert!(out.contains("forced"), "{out}");
    assert!(!marker.exists(), "the client's command ran");
    assert_eq!(fs::read_to_string(&r.received).unwrap(), "from stdin\n");
    assert!(
        out.contains("env=\n"),
        "a client's variable reached it: {out}"
    );
}

#[test]
fn a_restricted_key_gets_no_shell_and_no_terminal() {
    let Some(r) = rig("noshell") else { return };
    // A shell asked for: the command runs instead.
    let (ok, out) = r.ssh(&r.restricted, &["-T"], &[], "exit\n");
    assert!(ok, "{out}");
    assert!(out.contains("forced"), "{out}");
    // A terminal insisted on: refused, and OpenSSH gives up.
    let (_, out) = r.ssh(&r.restricted, &["-tt"], &[], "exit\n");
    assert!(out.contains("PTY allocation request failed"), "{out}");
    assert!(!out.contains("has-tty"), "{out}");
    // A terminal merely wished for: refused, and the command runs without one.
    let (ok, out) = r.ssh(&r.restricted, &["-o", "RequestTTY=auto"], &[], "exit\n");
    assert!(ok, "{out}");
    assert!(out.contains("forced") && !out.contains("has-tty"), "{out}");
}

#[test]
fn a_key_without_options_still_runs_what_it_asks() {
    let Some(r) = rig("full") else { return };
    let (ok, out) = r.ssh(&r.full, &[], &["echo", "plain"], "");
    assert!(ok, "{out}");
    assert!(out.contains("plain") && !out.contains("forced"), "{out}");
}

#[test]
fn the_push_delivers_a_dummy_value_and_shows_it_nowhere() {
    let Some(r) = rig("push") else { return };
    let secret_file = r.dir.join("secret");
    fs::write(&secret_file, format!("{DUMMY}\n")).unwrap();
    let host_file = r.dir.join("juno-host");
    fs::write(&host_file, "127.0.0.1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_juno-wifi-push"))
        .env("GBGO_WIFI_PSK_FILE", &secret_file)
        .args(["--port", &r.port.to_string()])
        .arg("--key")
        .arg(&r.restricted)
        .arg("--known-hosts")
        .arg(&r.known_hosts)
        .arg("--host-file")
        .arg(&host_file)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert_eq!(
        fs::read_to_string(&r.received).unwrap(),
        format!("psk={DUMMY}\n")
    );
    assert!(!text.contains(DUMMY), "printed: {text}");
    assert!(
        !fs::read_to_string(&r.server_log).unwrap().contains(DUMMY),
        "in the server's log"
    );
}

#[test]
fn the_push_refuses_a_machine_with_another_host_key() {
    let Some(r) = rig("pin") else { return };
    let secret_file = r.dir.join("secret");
    fs::write(&secret_file, format!("{DUMMY}\n")).unwrap();
    let host_file = r.dir.join("juno-host");
    fs::write(&host_file, "127.0.0.1\n").unwrap();
    let other = r.dir.join("other");
    let other_pub = keygen(&other);
    let mut fields = other_pub.split_whitespace();
    let wrong = r.dir.join("wrong_known_hosts");
    fs::write(
        &wrong,
        format!(
            "juno {} {}\n",
            fields.next().unwrap(),
            fields.next().unwrap()
        ),
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_juno-wifi-push"))
        .env("GBGO_WIFI_PSK_FILE", &secret_file)
        .args(["--port", &r.port.to_string()])
        .arg("--key")
        .arg(&r.restricted)
        .arg("--known-hosts")
        .arg(&wrong)
        .arg("--host-file")
        .arg(&host_file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        !r.received.exists(),
        "the value reached a machine with the wrong key"
    );
}

#[test]
fn the_push_needs_the_file_and_a_sane_value() {
    let Some(r) = rig("refuse") else { return };
    let run = |env: Option<&Path>| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_juno-wifi-push"));
        c.env_remove("GBGO_WIFI_PSK_FILE").args([
            "--key",
            "/nonexistent",
            "--known-hosts",
            "/nonexistent",
        ]);
        if let Some(p) = env {
            c.env("GBGO_WIFI_PSK_FILE", p);
        }
        c.output().unwrap()
    };
    assert_eq!(run(None).status.code(), Some(2));
    let short = r.dir.join("short");
    fs::write(&short, "abc").unwrap();
    let out = run(Some(&short));
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("abc"));
}

#[test]
fn a_value_with_another_fingerprint_is_never_sent() {
    let Some(r) = rig("fingerprint") else { return };
    let secret_file = r.dir.join("secret");
    fs::write(&secret_file, format!("{DUMMY}\n")).unwrap();
    let host_file = r.dir.join("juno-host");
    fs::write(&host_file, "127.0.0.1\n").unwrap();
    let push = |fingerprint: &str| {
        Command::new(env!("CARGO_BIN_EXE_juno-wifi-push"))
            .env("GBGO_WIFI_PSK_FILE", &secret_file)
            .args(["--port", &r.port.to_string(), "--fingerprint", fingerprint])
            .arg("--key")
            .arg(&r.restricted)
            .arg("--known-hosts")
            .arg(&r.known_hosts)
            .arg("--host-file")
            .arg(&host_file)
            .output()
            .unwrap()
    };
    let out = push("00000000");
    assert_eq!(out.status.code(), Some(2));
    assert!(!r.received.exists(), "sent in spite of the fingerprint");
    assert!(!String::from_utf8_lossy(&out.stderr).contains(DUMMY));
    // The right one: sha256sum of the file's bytes, line break and all, as the broker would have stored them.
    let sum = Command::new("sha256sum")
        .arg(&secret_file)
        .output()
        .unwrap();
    let right = String::from_utf8_lossy(&sum.stdout)[..8].to_string();
    let out = push(&right);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(&r.received).unwrap(),
        format!("psk={DUMMY}\n")
    );
}
