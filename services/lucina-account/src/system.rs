//! What an app calls for the machine's account: [`client`] when it needs the server, [`report_rejected`] when the server said
//! 401, and `ui::SignInView` when there is no sign-in. `lucina-accountd` ([`crate::service`]) holds the token behind these.
pub use lucina_account_link::{report_rejected, token, Link, Pairing, Status};

use crate::client::{Client, BASE_ENV};

/// A client with the machine's token, or `None` when nobody is signed in. Waits at most a second, and only at boot or while
/// the service asks the server about a token; it answers from memory otherwise.
pub fn client() -> Option<Client> {
    let token = token()?;
    Some(Client::from_env(Some(token)).unwrap_or_else(|| panic!("{BASE_ENV} must be https, or http to this machine")))
}
