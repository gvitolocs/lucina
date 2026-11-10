//! The device token and where it lives. Only `lucina-accountd` keeps it; programs ask that service (the crate's root).
//!
//! * [`RAM_PATH`], under `/run`, in memory, mode 0600: lost at every restart, and then the phone has to approve again;
//! * [`DATA_PATH`], on /data, mode 0600 in a 0700 folder, only when the service is told to keep it there. The disk is not
//!   encrypted and everything runs as root, so the modes keep out nobody with the disk in hand; revoking the device on the
//!   server ends it.
//!
//! Never a password. `Debug` shows nothing, the memory is wiped on drop, and nothing in this crate prints it.
use std::fmt;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use zeroize::Zeroize;

/// The service's copy, so that a restart of the service keeps the sign-in. The apps' cache files sit beside it.
pub const RAM_PATH: &str = "/run/lucina/secretary/credential";
/// The same on /data, which survives a restart of the machine.
pub const DATA_PATH: &str = "/data/lucina/secretary/credential";
/// Set by the tests and the fake server's demos.
pub const PATH_ENV: &str = "LUCINA_SECRETARY_CREDENTIAL";

/// `sec_…`: the server's device token.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl Token {
    /// `sec_` + 43 base64url characters, the server's only format. `None` for anything else.
    pub fn parse(s: &str) -> Option<Token> {
        let rest = s.strip_prefix("sec_")?;
        (rest.len() == 43 && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
            .then(|| Token(s.to_owned()))
    }

    /// For the `Authorization` header and the service's socket, and nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Where apps' cache files go, beside the service's copy of the token: [`PATH_ENV`] or [`RAM_PATH`].
pub fn path() -> PathBuf {
    std::env::var_os(PATH_ENV).map(PathBuf::from).unwrap_or_else(|| PathBuf::from(RAM_PATH))
}

/// A file beside the token, 0600, for what apps keep between openings: in RAM, gone at a restart. `name` is theirs.
pub fn write_private(name: &str, bytes: &[u8]) -> io::Result<()> {
    let p = path().with_file_name(name);
    let dir = p.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = p.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(bytes)?;
    drop(f);
    std::fs::rename(&tmp, p)
}

pub fn read_private(name: &str) -> Option<Vec<u8>> {
    std::fs::read(path().with_file_name(name)).ok()
}

pub fn remove_private(name: &str) {
    let _ = std::fs::remove_file(path().with_file_name(name));
}

/// `Ok(None)` without a file. A file that holds no valid token is an error, not a sign-in.
pub fn load(path: &Path) -> io::Result<Option<Token>> {
    let mut text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let token = Token::parse(text.trim());
    text.zeroize();
    token.map(Some).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed credential"))
}

/// Atomic, mode 0600 from creation on, the directory 0700.
pub fn save(path: &Path, token: &Token) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
    f.write_all(token.expose().as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)?;
    // The name reaches the disk now, not at the next periodic sync. tmpfs may refuse: it has nothing to flush.
    if let Ok(folder) = std::fs::File::open(dir) {
        let _ = folder.sync_all();
    }
    Ok(())
}

/// Gone, or was never there.
pub fn remove(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
fn test_token() -> Token {
    Token::parse(&format!("sec_{}", "a".repeat(43))).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn saved_private_loaded_back_and_never_printed() {
        let dir = std::env::temp_dir().join(format!("lucina-account-cred-{}", std::process::id()));
        let p = dir.join("sub/credential");
        let t = test_token();
        save(&p, &t).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(p.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(load(&p).unwrap(), Some(t.clone()));
        assert!(!format!("{t:?}").contains("aaaa"));
        remove(&p).unwrap();
        remove(&p).unwrap();
        assert_eq!(load(&p).unwrap(), None);
        std::fs::write(&p, "not a token").unwrap();
        assert!(load(&p).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_the_servers_format_parses() {
        assert!(Token::parse("sec_short").is_none());
        assert!(Token::parse(&format!("svc_{}", "a".repeat(43))).is_none());
        assert!(Token::parse(&format!("sec_{}!", "a".repeat(42))).is_none());
    }
}
