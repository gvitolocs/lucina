//! The host's settings, in one file only root reads. What a client must have to get in is the server's key, which hbbs and hbbr ask
//! for, and the password, of which this file holds a salted hash.

use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use lucinadesk_core::host::{Identity, PasswordRecord};

pub const PATH: &str = "/data/lucina/desk/host";

#[derive(Clone)]
pub struct HostConfig {
    pub id: String,
    /// `host:port` of hbbs's WebSocket; hbbr's is the next port.
    pub server: String,
    /// `host:port` of hbbs's UDP port.
    pub udp: String,
    /// What a client dials for the relay.
    pub client_relay: String,
    /// The server's public key, base64.
    pub key: String,
    pub seed: [u8; 32],
    pub uuid: Vec<u8>,
    pub password: PasswordRecord,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

impl HostConfig {
    pub fn parse(text: &str) -> Result<HostConfig, String> {
        let get = |name: &str| -> Result<String, String> {
            text.lines()
                .find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == name).map(|(_, v)| v.trim().to_owned()))
                .ok_or_else(|| format!("no {name} in the host settings"))
        };
        let bytes = |name: &str| -> Result<Vec<u8>, String> { unhex(&get(name)?).ok_or_else(|| format!("{name} is not hex")) };
        Ok(HostConfig {
            id: get("id")?,
            server: get("server")?,
            udp: get("udp")?,
            client_relay: get("client_relay")?,
            key: get("key")?,
            seed: bytes("seed")?.try_into().map_err(|_| "the seed must be 32 bytes")?,
            uuid: bytes("uuid")?,
            password: PasswordRecord {
                salt: get("salt")?,
                h1: bytes("h1")?.try_into().map_err(|_| "h1 must be 32 bytes")?,
            },
        })
    }

    pub fn text(&self) -> String {
        format!(
            "id = {}\nserver = {}\nudp = {}\nclient_relay = {}\nkey = {}\nseed = {}\nuuid = {}\nsalt = {}\nh1 = {}\n",
            self.id,
            self.server,
            self.udp,
            self.client_relay,
            self.key,
            hex(&self.seed),
            hex(&self.uuid),
            self.password.salt,
            hex(&self.password.h1),
        )
    }

    pub fn load(path: &Path) -> Result<HostConfig, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        HostConfig::parse(&text)
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        f.write_all(self.text().as_bytes()).map_err(|e| e.to_string())
    }

    pub fn identity(&self) -> Identity {
        Identity { id: self.id.clone(), signing: lucinadesk_core::host::signing_key(&self.seed), uuid: self.uuid.clone() }
    }

    pub fn rendezvous_url(&self) -> String {
        format!("ws://{}", self.server)
    }

    pub fn relay_url(&self) -> String {
        match self.server.rsplit_once(':').and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?))) {
            Some((h, p)) => format!("ws://{h}:{}", p.wrapping_add(1)),
            None => format!("ws://{}:21129", self.server),
        }
    }
}

/// Settings from a test or from the person, keeping the machine's identity if it has one: a new password does not make it a new machine.
pub fn setup(path: &Path, id: &str, server: &str, udp: &str, client_relay: &str, key: &str, password: &str) -> Result<HostConfig, String> {
    if password.is_empty() {
        return Err("a password is needed".into());
    }
    if id.len() < 6 {
        return Err("hbbs wants an ID of six characters or more".into());
    }
    let (seed, uuid) = match HostConfig::load(path) {
        Ok(old) => (old.seed, old.uuid),
        Err(_) => {
            let mut seed = [0u8; 32];
            let mut uuid = vec![0u8; 16];
            lucinadesk_core::crypto::random(&mut seed);
            lucinadesk_core::crypto::random(&mut uuid);
            (seed, uuid)
        }
    };
    let cfg = HostConfig {
        id: id.to_owned(),
        server: server.to_owned(),
        udp: udp.to_owned(),
        client_relay: client_relay.to_owned(),
        key: key.trim().to_owned(),
        seed,
        uuid,
        password: PasswordRecord::new(password),
    };
    cfg.save(path)?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("desk-host-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("host")
    }

    #[test]
    fn settings_round_trip_keep_no_password_and_only_root_reads_them() {
        let p = temp("a");
        let cfg = setup(&p, "junohost", "10.0.0.1:21128", "10.0.0.1:21116", "10.0.0.1:21117", "KEY=\n", "my secret").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("my secret"));
        let back = HostConfig::load(&p).unwrap();
        assert_eq!(back.text(), cfg.text());
        assert_eq!(back.key, "KEY=");
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(cfg.rendezvous_url(), "ws://10.0.0.1:21128");
        assert_eq!(cfg.relay_url(), "ws://10.0.0.1:21129");
    }

    #[test]
    fn a_new_password_keeps_the_identity_and_changes_the_hash() {
        let p = temp("b");
        let one = setup(&p, "junohost", "a:1", "a:2", "a:3", "K", "one").unwrap();
        let two = setup(&p, "junohost", "a:1", "a:2", "a:3", "K", "two").unwrap();
        assert_eq!(one.seed, two.seed);
        assert_eq!(one.uuid, two.uuid);
        assert_ne!(one.password.h1, two.password.h1);
    }

    #[test]
    fn what_is_refused() {
        let p = temp("c");
        assert!(setup(&p, "junohost", "a:1", "a:2", "a:3", "K", "").is_err());
        assert!(setup(&p, "short", "a:1", "a:2", "a:3", "K", "x").is_err());
        assert!(HostConfig::parse("id = a\n").is_err());
        assert!(HostConfig::load(Path::new("/nonexistent/host")).is_err());
    }
}
