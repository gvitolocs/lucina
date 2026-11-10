//! Fortuna to the owner's Secretary account, without I/O: what to send and how to read the answers.
//!
//! - `http`: HTTP/1.1 heads and an incremental answer reader (lengths, chunks, caps).
//! - `api`: the `/v1` routes and fields Juno's apps use (sign-in, session, timeline, chat, Lucina Agents) and the
//!   share route the phones use for voice notes.
//! - `pairing`: the QR sign-in's state machine (Juno's `signin.rs`).
//! - `token`: the device token file on the SD card.
//! - `multipart`: the share's form body.
//! - `ws`: the voice socket (`/v1/voice/ws`): the WebSocket's upgrade and frames, and a voice note's messages.
//! - `rules`: the integer rules Verus checks.
//!
//! The kernel's network thread does the I/O over `fortuna-tls`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod api;
pub mod chat;
pub mod http;
pub mod multipart;
pub mod pairing;
pub mod rules;
pub mod token;
pub mod ws;
