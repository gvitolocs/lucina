//! Named fields of a running program, read through a map made for that build. Design: `docs/research/juno-ai-g-memory.md`.
//!
//! - [`rules`]: the bounds, pointer-path, exclusion and lookup rules, proved by Verus.
//! - [`map`]: the map file and the reader over a program's own copy of its published regions.
pub mod map;
pub mod rules;
