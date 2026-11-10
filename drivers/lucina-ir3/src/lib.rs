// SPDX-License-Identifier: MIT
//! Juno's shader compiler for the Adreno 618 (stage 2 of `docs/research/juno-gpu-3d.md`): naga's IR in, ir3 machine code out,
//! after Mesa's ir3 (`src/freedreno/ir3`, MIT), the original for behaviour.
//!
//! - [`isa`]: the instruction set as Mesa describes it (`isa/ir3*.xml`, vendored, read by `build.rs`), with Mesa's isaspec
//!   decoder: the disassembler whose text Mesa's own tests check.
#![no_std]

extern crate alloc;

pub mod asm;
pub mod backend;
pub mod delay;
pub mod frontend;
pub mod ir;
pub mod isa;
pub mod pack;
