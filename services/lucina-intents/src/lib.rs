//! Juno app intents and Lucina's undo journal (docs/research/juno-ai-native.md, layer 2 and layer 6 level 1; the protocol is
//! docs/research/juno-intents.md).
//!
//! * [`rules`]: what Verus checks: effect classes, arguments, undo entries, the journal's order.
//! * [`spec`]: what an app declares (`IntentSpec`), what a call carries (`Call`) and what it answers (`Reply`).
//! * [`journal`]: Lucina's undo journal on disk.
pub mod journal;
// Verus reads rules.rs and has no `RangeInclusive::contains`.
#[allow(clippy::manual_range_contains)]
pub mod rules;
pub mod spec;

pub use rules::{Effect, Gate, Kind, State, WriteKind};
pub use spec::{ArgSpec, Call, Entity, IntentSpec, Reply, Undo, Write};
