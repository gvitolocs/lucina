//! The last lists and the unsent commands in RAM beside the token, so that the app shows its lists the
//! instant it starts and commands survive leaving it (the web app's localStorage outbox). Gone at a
//! restart: the owner's tasks don't reach the disk through here.

use lucina_account::credential;

use crate::store::Saved;

const FILE: &str = "tasks-cache.json";

pub fn load() -> Option<Saved> {
    serde_json::from_slice(&credential::read_private(FILE)?).ok()
}

/// Quick, and allowed to fail: the app works without it.
pub fn save(s: &Saved) {
    if let Ok(bytes) = serde_json::to_vec(s) {
        let _ = credential::write_private(FILE, &bytes);
    }
}

