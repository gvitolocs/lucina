//! juno-wifi-push: hands the home Wi-Fi passphrase to Juno's `lucina-wifi`, from buildhost, with nobody seeing it.
//!
//! It runs inside `juno-wifi-psk.service` (`scripts/juno-wifi-psk-install.sh`), started by `infisical-broker exec --file
//! GBGO_WIFI_PSK_FILE=secret://...`: the value is in a sealed in-memory file, not in the environment or on a command line.
//! It goes to the stdin of `ssh` as `psk=<value>`, to Juno's restricted key (`boot/remote/authorized_keys`), whose only
//! command is `lucina-wifi connect --stdin`, and with Juno's host key pinned, so that it can reach no other machine.
//!
//! ```text
//! juno-wifi-push --key FILE --known-hosts FILE [--fingerprint HEX] [--host-file FILE] [--ssh PATH] [--port N]
//! ```
//!
//! `--host-file` holds where Juno is, `juno001.example.internal` if it is absent: anyone may change that, since the pinned key
//! decides who gets the value. `--fingerprint` is the broker's receipt for the value, the first 8 hex digits of its sha256:
//! anything else is refused before any connection. Nothing here prints the value or its fingerprint, and a value that cannot
//! be a passphrase is refused without saying why beyond its shape.

// Juno with no C library: Eyra is the libc and the program start, in Rust (docs/research/juno-pure-rust.md).
#[cfg(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64"))]
extern crate eyra;

use std::{
    env, fs,
    io::Write,
    process::{Command, ExitCode, Stdio},
};

use sha2::{Digest, Sha256};

const DEFAULT_HOST: &str = "juno001.example.internal";
/// What `known_hosts` calls Juno, whatever address it has today.
const HOST_ALIAS: &str = "juno";
const VAR: &str = "GBGO_WIFI_PSK_FILE";

struct Args {
    key: String,
    known_hosts: String,
    fingerprint: Option<String>,
    host_file: Option<String>,
    ssh: String,
    port: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let (mut key, mut known_hosts, mut fingerprint, mut host_file, mut port) =
        (None, None, None, None, None);
    let mut ssh = "/usr/bin/ssh".to_string();
    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--key" => key = Some(value()?),
            "--known-hosts" => known_hosts = Some(value()?),
            "--fingerprint" => fingerprint = Some(value()?),
            "--host-file" => host_file = Some(value()?),
            "--ssh" => ssh = value()?,
            "--port" => port = Some(value()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        key: key.ok_or("--key is required")?,
        known_hosts: known_hosts.ok_or("--known-hosts is required")?,
        fingerprint,
        host_file,
        ssh,
        port,
    })
}

/// A WPA passphrase is 8 to 63 printable ASCII characters, and a PSK 64 hex digits. A trailing line break is not part of it.
fn check_value(raw: &[u8]) -> Result<&[u8], String> {
    let value = raw.strip_suffix(b"\n").unwrap_or(raw);
    let value = value.strip_suffix(b"\r").unwrap_or(value);
    if !(8..=64).contains(&value.len()) {
        return Err(format!("the value is {} bytes, not 8 to 64", value.len()));
    }
    if !value.iter().all(|b| (0x20..0x7f).contains(b)) {
        return Err("the value has bytes a passphrase cannot have".to_string());
    }
    if value.len() == 64 && !value.iter().all(u8::is_ascii_hexdigit) {
        return Err("a 64-byte value must be hex digits".to_string());
    }
    Ok(value)
}

/// The broker's fingerprint (`grant::fingerprint` of infisical-mcp): the first 8 hex digits of the value's sha256.
fn fingerprint(value: &[u8]) -> String {
    Sha256::digest(value)[..4]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn close_on_exec(path: &str) {
    if let Some(fd) = path
        .strip_prefix("/proc/self/fd/")
        .and_then(|n| n.parse::<i32>().ok())
    {
        // SAFETY: Only sets a flag on a descriptor number, which does nothing if it is not open.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
}

fn host(args: &Args) -> Result<String, String> {
    let name = args
        .host_file
        .as_ref()
        .and_then(|f| fs::read_to_string(f).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_string());
    if name.starts_with('-') || name.chars().any(char::is_whitespace) {
        return Err("the host file does not hold a host name".to_string());
    }
    Ok(name)
}

fn run() -> Result<i32, String> {
    let args = parse_args()?;
    let path = env::var(VAR)
        .map_err(|_| format!("{VAR} is not set: run this through infisical-broker exec --file"))?;
    close_on_exec(&path);
    let mut raw = fs::read(&path).map_err(|err| format!("{VAR}: {}", err.kind()))?;
    let result = (|| {
        if let Some(expected) = &args.fingerprint
            && fingerprint(&raw) != expected.to_ascii_lowercase()
        {
            return Err(
                "the value does not have the expected fingerprint: nothing was sent".to_string(),
            );
        }
        let value = check_value(&raw)?;
        let target = host(&args)?;
        let mut command = Command::new(&args.ssh);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args(["-F", "none", "-T", "-i", &args.key])
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "StrictHostKeyChecking=yes",
            ])
            .args(["-o", &format!("UserKnownHostsFile={}", args.known_hosts)])
            .args(["-o", &format!("HostKeyAlias={HOST_ALIAS}")])
            .args(["-o", "ConnectTimeout=15", "-o", "LogLevel=ERROR"]);
        if let Some(port) = &args.port {
            command.args(["-p", port]);
        }
        command.arg(format!("root@{target}"));
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|err| format!("{}: {}", args.ssh, err.kind()))?;
        {
            let mut stdin = child.stdin.take().ok_or("no stdin for ssh")?;
            let mut line = Vec::with_capacity(value.len() + 5);
            line.extend_from_slice(b"psk=");
            line.extend_from_slice(value);
            line.push(b'\n');
            let written = stdin.write_all(&line);
            line.fill(0);
            written.map_err(|err| format!("ssh's stdin: {}", err.kind()))?;
        }
        let status = child.wait().map_err(|err| format!("ssh: {}", err.kind()))?;
        Ok(status.code().unwrap_or(255))
    })();
    raw.fill(0);
    result
}

fn main() -> ExitCode {
    match run() {
        Ok(0) => {
            println!("juno-wifi-push: delivered, and lucina-wifi finished");
            ExitCode::SUCCESS
        }
        Ok(code) => {
            eprintln!("juno-wifi-push: ssh or lucina-wifi ended with status {code}");
            ExitCode::from(code.clamp(1, 255) as u8)
        }
        Err(why) => {
            eprintln!("juno-wifi-push: {why}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The vector in infisical-mcp's own test.
    #[test]
    fn the_fingerprint_is_the_brokers() {
        assert_eq!(fingerprint(b"abc"), "ba7816bf");
    }

    #[test]
    fn values() {
        assert!(check_value(b"short").is_err());
        assert_eq!(check_value(b"long enough\n").unwrap(), b"long enough");
        assert_eq!(check_value(b"long enough\r\n").unwrap(), b"long enough");
        assert!(check_value(b"two\nlines here").is_err());
        assert!(check_value(&[b'a'; 65]).is_err());
        assert!(check_value(&[b'g'; 64]).is_err());
        assert!(check_value(&[b'a'; 64]).is_ok());
    }

    #[test]
    fn errors_never_hold_the_value() {
        let secret = b"secret\x01value";
        let why = check_value(secret).unwrap_err();
        assert!(!why.contains("secret"));
    }
}
