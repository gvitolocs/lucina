//! What Juno's account apps (`lucina-app-notes`, `lucina-app-shopping`) share. Their behaviour comes from SecretaryIOS
//! `web/src`; each function here names the web function it follows.
//!
//! - [`net`]: requests to `/v1` on a thread of their own, through a [`net::Transport`]: the real account client, a plain
//!   loopback client for tests against a local server, or an app's in-process fake.
//! - [`files`]: where an app keeps its cache and outbox, written whole and renamed into place.
//! - [`text`]: the two search foldings.
//! - [`dates`]: ISO timestamps, the labels Keep shows, ids.

pub mod dates;
pub mod files;
pub mod net;
pub mod text;
