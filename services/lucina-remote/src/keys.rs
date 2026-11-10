//! OpenSSH key files: `authorized_keys` (ed25519 lines) and an unencrypted ed25519 private key
//! (`ssh-keygen -t ed25519 -N ''`), the host key.

use sha2::{Digest, Sha256};

const ED25519: &str = "ssh-ed25519";

/// An ed25519 public key allowed to log in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedKey {
    pub key: [u8; 32],
    pub comment: String,
    /// The `command="..."` option: this key runs that and nothing else, with no terminal and none of the client's variables.
    pub command: Option<String>,
}

impl AuthorizedKey {
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.key)
    }
}

/// The OpenSSH fingerprint of an ed25519 public key: SHA-256 of its wire encoding, base64 without padding.
pub fn fingerprint(key: &[u8; 32]) -> String {
    let mut blob = Vec::with_capacity(51);
    put_string(&mut blob, ED25519.as_bytes());
    put_string(&mut blob, key);
    let mut s = base64_encode(&Sha256::digest(&blob));
    while s.ends_with('=') {
        s.pop();
    }
    format!("SHA256:{s}")
}

/// The options of an `authorized_keys` line, if any, and the rest of the line. `None` if they are not ones this server knows.
fn split_options(line: &str) -> Option<(Option<String>, &str)> {
    // What this server does regardless: no forwarding, no agent, no X11, no user rc. `no-pty` and `restrict` are enforced for
    // a key with a command, and a line must have one.
    const ENFORCED: [&str; 6] = [
        "restrict",
        "no-pty",
        "no-port-forwarding",
        "no-agent-forwarding",
        "no-X11-forwarding",
        "no-user-rc",
    ];
    if line.starts_with(ED25519) || line.starts_with("ssh-") || line.starts_with("ecdsa-") {
        return Some((None, line));
    }
    let mut command = None;
    let mut rest = line;
    loop {
        let name_end = rest.find(|c: char| c == ',' || c == '=' || c.is_whitespace())?;
        let (name, after) = rest.split_at(name_end);
        if name == "command" {
            let quoted = after.strip_prefix("=\"")?;
            let mut value = String::new();
            let mut chars = quoted.char_indices();
            let end = loop {
                let (i, c) = chars.next()?;
                match c {
                    '"' => break i,
                    '\\' => {
                        let (_, next) = chars.next()?;
                        if next != '"' {
                            value.push('\\');
                        }
                        value.push(next);
                    }
                    other => value.push(other),
                }
            };
            if command.is_some() || value.is_empty() {
                return None;
            }
            command = Some(value);
            rest = &quoted[end + 1..];
        } else if ENFORCED.contains(&name) {
            rest = after;
        } else {
            return None;
        }
        if let Some(next) = rest.strip_prefix(',') {
            rest = next;
        } else {
            let rest = rest.trim_start();
            return command.is_some().then_some((command, rest));
        }
    }
}

/// Reads the `ssh-ed25519` keys of an `authorized_keys` file. Other key types, comments and blank lines are skipped.
///
/// A line may start with options, for a key limited to one command: `command="..."` and the restrictions this server
/// enforces anyway. Any other option, or options without a command, makes the line skipped: a key must never get more
/// than its line says.
pub fn parse_authorized_keys(text: &str) -> Vec<AuthorizedKey> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (command, rest) = split_options(line)?;
            let mut fields = rest.split_whitespace();
            if fields.next()? != ED25519 {
                return None;
            }
            let blob = base64_decode(fields.next()?)?;
            let mut r = Reader(&blob);
            if r.string()? != ED25519.as_bytes() {
                return None;
            }
            let key: [u8; 32] = r.string()?.try_into().ok()?;
            if !r.0.is_empty() {
                return None;
            }
            Some(AuthorizedKey {
                key,
                comment: fields.collect::<Vec<_>>().join(" "),
                command,
            })
        })
        .collect()
}

/// Why a private key file was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    NotOpenSsh,
    Encrypted,
    NotEd25519,
    Malformed,
}

impl core::fmt::Display for KeyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            KeyError::NotOpenSsh => "not an OpenSSH private key",
            KeyError::Encrypted => "the key is encrypted (generate it with -N '')",
            KeyError::NotEd25519 => "not an ed25519 key",
            KeyError::Malformed => "malformed key",
        })
    }
}

/// Reads the 32-byte seed of an unencrypted OpenSSH ed25519 private key
/// (`openssh-key-v1`, PROTOCOL.key in OpenSSH).
pub fn parse_openssh_ed25519_private(text: &str) -> Result<[u8; 32], KeyError> {
    const BEGIN: &str = "-----BEGIN OPENSSH PRIVATE KEY-----";
    const END: &str = "-----END OPENSSH PRIVATE KEY-----";
    let start = text.find(BEGIN).ok_or(KeyError::NotOpenSsh)? + BEGIN.len();
    let end = text[start..].find(END).ok_or(KeyError::NotOpenSsh)? + start;
    let blob = base64_decode(&text[start..end]).ok_or(KeyError::Malformed)?;
    let rest = blob
        .strip_prefix(b"openssh-key-v1\0")
        .ok_or(KeyError::NotOpenSsh)?;
    let mut r = Reader(rest);
    let m = KeyError::Malformed;
    let cipher = r.string().ok_or(KeyError::Malformed)?;
    let kdf = r.string().ok_or(KeyError::Malformed)?;
    r.string().ok_or(KeyError::Malformed)?; // kdf options
    if cipher != b"none" || kdf != b"none" {
        return Err(KeyError::Encrypted);
    }
    if r.u32().ok_or(KeyError::Malformed)? != 1 {
        return Err(m);
    }
    let public = r.string().ok_or(KeyError::Malformed)?;
    let mut p = Reader(r.string().ok_or(KeyError::Malformed)?);
    let (check1, check2) = (
        p.u32().ok_or(KeyError::Malformed)?,
        p.u32().ok_or(KeyError::Malformed)?,
    );
    if check1 != check2 {
        return Err(KeyError::Malformed);
    }
    if p.string().ok_or(KeyError::Malformed)? != ED25519.as_bytes() {
        return Err(KeyError::NotEd25519);
    }
    let pk: [u8; 32] = p
        .string()
        .and_then(|s| s.try_into().ok())
        .ok_or(KeyError::Malformed)?;
    let sk = p.string().ok_or(KeyError::Malformed)?;
    // The private part is seed || public key.
    if sk.len() != 64 || sk[32..] != pk {
        return Err(KeyError::Malformed);
    }
    // The public key section must be the same key.
    let mut pr = Reader(public);
    if pr.string() != Some(ED25519.as_bytes()) || pr.string() != Some(&pk[..]) {
        return Err(KeyError::Malformed);
    }
    Ok(sk[..32].try_into().unwrap())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (head, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_be_bytes(*head))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let len = self.u32()? as usize;
        if self.0.len() < len {
            return None;
        }
        let (s, rest) = self.0.split_at(len);
        self.0 = rest;
        Some(s)
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, as OpenSSH writes it; whitespace is ignored.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits, mut pad) = (0u32, 0u32, 0usize);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None;
        }
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (pad <= 2 && acc == 0).then_some(out)
}

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.len();
        let v = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= n {
                out.push(ALPHABET[(v >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The owner's Mac key (public), whose fingerprint `ssh-keygen -l` prints as below.
    const OWNER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILKv2pQKzxjtEAutV4+FkMnzNTV3Iugl6Xa2ip40HDaa owner@mac-nas-setup";

    // A throwaway key made with `ssh-keygen -t ed25519 -N '' -C test` for these tests only.
    const TEST_PRIVATE: &str = include_str!("../tests/data/test_ed25519");
    const TEST_PUBLIC: &str = include_str!("../tests/data/test_ed25519.pub");
    const TEST_FINGERPRINT: &str = include_str!("../tests/data/test_ed25519.fp");

    #[test]
    fn owner_key_fingerprint() {
        let keys = parse_authorized_keys(OWNER);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].comment, "owner@mac-nas-setup");
        assert_eq!(
            keys[0].fingerprint(),
            "SHA256:shUxEZ0HIvAiJsZGqpId7wv6BQ8jiaWJMrnO4ZstqHg"
        );
    }

    // What the image ships: the owner's key, in full.
    #[test]
    fn the_shipped_keys_mean_what_they_should() {
        let keys = parse_authorized_keys(include_str!("../../../boot/remote/authorized_keys"));
        // The owner's Mac, CodeCor's juno_bash tool on buildhost (owner, 2026-11-07: each call is approved first), and buildhost's own
        // key for the buildhost-side Juno tools (coordinator, 2026-11-08).
        let comments: Vec<&str> = keys.iter().map(|k| k.comment.as_str()).collect();
        assert_eq!(comments, ["owner@mac-nas-setup", "codecor-juno-bash@buildhost", "buildhost-migration"]);
        assert!(keys.iter().all(|k| k.command.is_none()));
    }

    #[test]
    fn a_key_may_be_limited_to_one_command() {
        let line = format!("restrict,command=\"/sbin/x --y \\\"z\\\"\" {OWNER}");
        let keys = parse_authorized_keys(&line);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].command.as_deref(), Some("/sbin/x --y \"z\""));
        assert_eq!(keys[0].comment, "owner@mac-nas-setup");
        assert_eq!(parse_authorized_keys(OWNER)[0].command, None);
    }

    // Skipped rather than given more than the line says.
    #[test]
    fn options_this_server_does_not_know_lose_the_key() {
        for line in [
            format!("from=\"10.0.0.1\" {OWNER}"),
            format!("restrict {OWNER}"),
            format!("no-pty {OWNER}"),
            format!("command=\"a\",from=\"b\" {OWNER}"),
            format!("command=\"unterminated {OWNER}"),
            format!("command=\"\" {OWNER}"),
            format!("command=\"a\",command=\"b\" {OWNER}"),
            format!("command=noquotes {OWNER}"),
        ] {
            assert!(parse_authorized_keys(&line).is_empty(), "{line}");
        }
    }

    #[test]
    fn authorized_keys_skip_other_lines() {
        let text = format!(
            "# comment\n\nssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQ== rsa\n{OWNER}\nssh-ed25519 !!!bad\n  {TEST_PUBLIC}"
        );
        let keys = parse_authorized_keys(&text);
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[1].fingerprint(),
            TEST_FINGERPRINT.split_whitespace().nth(1).unwrap()
        );
    }

    #[test]
    fn private_key_matches_public() {
        let seed = parse_openssh_ed25519_private(TEST_PRIVATE).unwrap();
        let sk = sunset::ed25519_dalek::SigningKey::from_bytes(&seed);
        let public = parse_authorized_keys(TEST_PUBLIC);
        assert_eq!(sk.verifying_key().to_bytes(), public[0].key);
    }

    #[test]
    fn private_key_refusals() {
        assert_eq!(
            parse_openssh_ed25519_private("hello"),
            Err(KeyError::NotOpenSsh)
        );
        let broken = TEST_PRIVATE.replacen("b3BlbnNzaC1rZXktdjE", "b3BlbnNzaC1rZXktdjF", 1);
        assert!(parse_openssh_ed25519_private(&broken).is_err());
    }

    #[test]
    fn base64_round_trip() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_decode("Zm9v YmFy"), Some(b"foobar".to_vec()));
        assert_eq!(base64_decode("Zg=a"), None);
    }
}
