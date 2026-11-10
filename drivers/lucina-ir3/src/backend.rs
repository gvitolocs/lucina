// SPDX-License-Identifier: MIT
//! From [`crate::ir`] to machine code: linear-scan register allocation over `r0.x` up, then legalisation (the slots and sync
//! flags of [`crate::delay`], Mesa's `ir3_legalize`), then encoding through [`crate::asm`].

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};

use crate::{
    asm::{self, Flags, Operand, Reg, Src},
    delay::{self, Kind, Need},
    ir::{Inst, Op, Program, S, V},
};

/// `r0.x` to `r47.w`: what a6xx programs may use (`compiler->max_reg` territory, here without half registers).
pub const COMPONENTS: u16 = 48 * 4;

/// Each value's register. `Err` if the shader needs more than the file holds.
pub fn allocate(p: &Program) -> Result<BTreeMap<V, Reg>, String> {
    let mut last = BTreeMap::new();
    for (i, inst) in p.insts.iter().enumerate() {
        for s in &inst.srcs {
            last.insert(s.v, i);
        }
        last.entry(inst.dst).or_insert(i);
    }
    for v in &p.outputs {
        last.insert(*v, p.insts.len());
    }
    let mut free: Vec<u16> = (0..COMPONENTS).rev().collect();
    let mut live: BTreeMap<usize, Vec<u16>> = BTreeMap::new();
    let mut regs = BTreeMap::new();
    for (i, inst) in p.insts.iter().enumerate() {
        // A source that ends here gives its register to this instruction's result only when it is the same width of
        // time: reads happen before the write.
        for r in live.remove(&i).unwrap_or_default() {
            free.push(r);
        }
        free.sort_unstable_by(|a, b| b.cmp(a));
        let r = free.pop().ok_or_else(|| format!("out of registers at instruction {}", i))?;
        regs.insert(inst.dst, Reg(r));
        live.entry(last[&inst.dst].max(i + 1)).or_default().push(r);
    }
    Ok(regs)
}

fn kind(op: Op) -> Kind {
    match op {
        Op::Cat3(n) if n.starts_with("mad") => Kind::Mad,
        Op::Cat4(_) => Kind::Sfu,
        _ => Kind::Alu,
    }
}

fn src_of(s: &S, regs: &BTreeMap<V, Reg>) -> Src {
    Src { op: Operand::R(regs[&s.v]), abs: s.abs, neg: s.neg }
}

fn encode(inst: &Inst, regs: &BTreeMap<V, Reg>, fl: Flags) -> Result<u64, String> {
    let dst = regs[&inst.dst];
    let fl = Flags { sat: inst.sat, ..fl };
    match inst.op {
        Op::Input => unreachable!(),
        Op::Const(c) => asm::mov(asm::Type::F32, asm::Type::F32, dst, Operand::C(c), fl),
        Op::Imm(bits) => asm::mov_imm(asm::Type::U32, dst, bits, fl),
        Op::Cat2(name, cond) => asm::cat2(name, dst, src_of(&inst.srcs[0], regs), src_of(&inst.srcs[1], regs), cond, fl),
        Op::Cat2One(name) => asm::cat2_1(name, dst, src_of(&inst.srcs[0], regs), fl),
        Op::Cat3(name) => {
            let b = &inst.srcs[1];
            asm::cat3(name, dst, src_of(&inst.srcs[0], regs), regs[&b.v], b.neg, src_of(&inst.srcs[2], regs), fl)
        }
        Op::Cat4(name) => asm::cat4(name, dst, src_of(&inst.srcs[0], regs), fl),
        Op::Cov(from, to) => {
            let s = &inst.srcs[0];
            asm::mov(from, to, dst, Operand::R(regs[&s.v]), fl)
        }
    }
}

/// The machine words of `p`, with its slots and sync flags, ending in `end`.
pub fn legalize(p: &Program, regs: &BTreeMap<V, Reg>) -> Result<Vec<u64>, String> {
    let mut code = Vec::new();
    // For each register written: when, and by what. `cycle` counts every instruction and nop issued.
    let mut written: BTreeMap<u16, (u64, Kind)> = BTreeMap::new();
    let mut cycle = 0u64;
    for inst in p.insts.iter().filter(|i| i.op != Op::Input) {
        let mut wait = 0;
        let mut ss = false;
        let k = kind(inst.op);
        for (n, s) in inst.srcs.iter().enumerate() {
            let Some(&(at, producer)) = written.get(&regs[&s.v].0) else { continue };
            match delay::need(producer, k, n as u32, false) {
                Need::Slots(slots) => wait = wait.max(delay::slots_left(at, cycle, slots)),
                Need::Ss => ss = true,
                Need::Sy => return Err(String::from("no (sy) producers yet")),
            }
        }
        if wait > 0 {
            code.push(asm::nop((wait - 1) as u8, Flags::default())?);
            cycle += wait;
        }
        code.push(encode(inst, regs, Flags { ss, ..Flags::default() })?);
        if ss {
            // `(ss)` waited for every SFU result.
            written.retain(|_, w| w.1 != Kind::Sfu);
        }
        written.insert(regs[&inst.dst].0, (cycle, k));
        cycle += 1;
    }
    let ss = p.outputs.iter().any(|v| written.get(&regs[v].0).is_some_and(|w| w.1 == Kind::Sfu));
    code.push(asm::end(Flags { ss, ..Flags::default() })?);
    Ok(code)
}

/// [`allocate`] then [`legalize`]; the registers go back for the shader's interface.
pub fn compile(p: &Program) -> Result<(Vec<u64>, BTreeMap<V, Reg>), String> {
    let regs = allocate(p)?;
    Ok((legalize(p, &regs)?, regs))
}

