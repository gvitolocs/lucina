//! Model-checked protocols of LucinaOS: layer (c) and (d) of the specs in `docs/spec/` (docs/design/lucinaos-correctness.md).
//!
//! Each module is a Stateright model of one protocol: the threads, CPUs, interrupts and power cuts that touch its state, and
//! the properties the spec states. Time bounds (layer d) are `always` properties over an explicit clock, because Stateright's
//! `eventually` misses cycles. Each model has:
//! - a configuration that must hold (every property, checked exhaustively up to its bounds);
//! - witnesses: configurations with a known bug planted or an assumption dropped, which MUST produce a counterexample. A witness
//!   that passes means the model is too weak to see the bug it guards against (the TLC witness rule of `userspace/formal`);
//! - `sometimes` properties that show the interesting states are reached (non-vacuity);
//! - where the shipped code is a pure state machine, either the model calls it (`ab` calls `lucina_boot::trial`) or a
//!   conformance test replays the model's steps on it (`rcu` against `lucina_rcu::Monitor`).
//!
//! Run: `scripts/verify-models.sh` (also the last step of `scripts/verify.sh`).

pub mod ab;
pub mod fs;
pub mod kmsg_wake;
pub mod rcu;

use stateright::{Checker, Model};

/// What a check found, for the tests and the report line.
#[derive(Debug)]
pub struct Outcome {
    pub states: usize,
    pub unique: usize,
    /// The names of the properties with a discovery: a counterexample for `always`, an example for `sometimes`.
    pub discovered: Vec<&'static str>,
}

/// Breadth-first, exhaustively up to the model's boundary.
pub fn check<M>(model: M) -> Outcome
where
    M: Model + Send + Sync + 'static,
    M::State: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync,
    M::Action: Clone + std::fmt::Debug + PartialEq + Send + Sync,
{
    let checker = model.checker().threads(4).spawn_bfs().join();
    let mut discovered: Vec<&'static str> = checker.discoveries().keys().copied().collect();
    discovered.sort_unstable();
    Outcome {
        states: checker.state_count(),
        unique: checker.unique_state_count(),
        discovered,
    }
}

/// The path to a discovery, printed when a test fails or a witness is shown.
pub fn explain<M>(model: M, name: &'static str) -> Option<String>
where
    M: Model + Send + Sync + 'static,
    M::State: Clone + std::fmt::Debug + std::hash::Hash + Eq + Send + Sync,
    M::Action: Clone + std::fmt::Debug + PartialEq + Send + Sync,
{
    let checker = model.checker().threads(4).spawn_bfs().join();
    checker.discovery(name).map(|p| {
        let last = format!("{:?}", p.last_state());
        let mut out = String::new();
        for a in p.into_actions() {
            out.push_str(&format!("  {a:?}\n"));
        }
        out.push_str(&format!("  last state: {last}\n"));
        out
    })
}
