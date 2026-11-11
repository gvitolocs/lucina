//! Juno's connection to the owner's Secretary account (`docs/server-api.md` in SecretaryIOS).
//!
//! * [`client`]: the `/v1` routes the apps use, blocking, over rustls with the bundled roots.
//! * [`signin`]: QR sign-in: Juno shows a QR code and a 6-digit code, the owner approves on his phone, Juno gets a device
//!   token. No password ever passes through here.
//! * [`service`]: `lucina-accountd`, which holds that token for the whole machine. Programs reach it through [`system`]. The
//!   token's `Debug` prints nothing, and nothing in this crate logs it.
//! * [`timeline`], [`rich`], [`engine`]: the chat, on `secretary-chat`'s merge, the same rules as the iPhone's.
//! * [`secrets`]: the paste-and-OK card's value and the composer's guard: a secret goes through `POST /v1/secrets`, never
//!   through chat.
//! * [`decisions`]: the Decisions and Approvals pages' rules.
pub mod client;
pub use lucina_account_link::credential;
pub mod decisions;
pub mod engine;
#[cfg(feature = "fake")]
pub mod fake;
pub mod rich;
pub mod secrets;
pub mod service;
pub mod signin;
pub mod sse;
pub mod system;
pub mod timeline;
#[cfg(feature = "ui")]
pub use lucina_account_link::ui;

pub use client::{ApiError, Client, DEFAULT_BASE};
pub use credential::Token;
