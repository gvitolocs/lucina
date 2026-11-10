//! lucina-ax: Juno's AccessKit adapter. Apps publish what is on their screen as an AccessKit tree, through a socket instead of
//! AT-SPI or UIA, and take Lucina's actions by node id, so that she works an app from its real state rather than its pixels.
//! See docs/research/juno-ax.md (stage B of docs/research/juno-ai-native.md).
//!
//! - [`rules`]: what may leave an app, node ids, which widget an action reaches, the size limits. Verus checks that file (D-011).
//! - [`tree`]: the [`Builder`] a toolkit fills while `view` runs, the [`Snapshot`] it makes, its text and version, and
//!   [`Snapshot::from_update`] for egui's own updates.
//! - [`proto`]: the socket, both ends.
//! - [`client`]: for lucina-devd: the program in front, its tree, an action on it.

pub mod client;
pub mod devd;
pub mod proto;
pub mod rules;
pub mod state;
pub mod tree;

pub use accesskit;
pub use proto::{Request, Server};
pub use tree::{Builder, Placed, Planned, Refused, Snapshot, Source, Widget};
