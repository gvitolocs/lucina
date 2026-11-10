// SPDX-License-Identifier: MIT
//! The compiler's middle: scalar SSA in execution order, one value per component.

use alloc::vec::Vec;

/// An SSA value (a virtual register component).
pub type V = u32;

/// A source: a value with its modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct S {
    pub v: V,
    pub abs: bool,
    pub neg: bool,
}

pub const fn s(v: V) -> S {
    S { v, abs: false, neg: false }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Already in its register when the shader starts.
    Input,
    /// `mov` of a const-file component.
    Const(u16),
    /// `mov` of 32 raw immediate bits.
    Imm(u32),
    /// `mov` of another value (with its modifiers applied by `absneg.f` instead).
    Cat2(&'static str, Option<u64>),
    Cat2One(&'static str),
    /// Sources a, b (register), c.
    Cat3(&'static str),
    Cat4(&'static str),
    /// Conversions between 32-bit types: (from, to).
    Cov(crate::asm::Type, crate::asm::Type),
}

#[derive(Clone, Debug)]
pub struct Inst {
    pub op: Op,
    pub dst: V,
    pub srcs: Vec<S>,
    pub sat: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Program {
    pub insts: Vec<Inst>,
    /// Values the shader's end reads (its outputs).
    pub outputs: Vec<V>,
    pub next: V,
}

impl Program {
    pub fn value(&mut self) -> V {
        self.next += 1;
        self.next - 1
    }

    pub fn emit(&mut self, op: Op, srcs: Vec<S>) -> V {
        let dst = self.value();
        self.insts.push(Inst { op, dst, srcs, sat: false });
        dst
    }
}
