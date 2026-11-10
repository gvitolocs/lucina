// SPDX-License-Identifier: MIT
//! Building ir3 instructions by name over the ISA tables ([`crate::isa`]), for the compiler: the fields Mesa's XML names,
//! packed by [`crate::isa::encode`]. Full-precision registers only so far.

use alloc::{format, string::String, vec, vec::Vec};

use crate::isa::{self, BITSETS, Instr, Placed, Ty, Value};

/// The generation whose instruction forms are built: the A618 is a6xx.
const GPU: u32 = 600;

/// A register component: `gpr * 4 + comp`, as Mesa numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reg(pub u16);

/// `p0.x`, the predicate.
pub const P0: Reg = Reg(62 * 4);

/// A source operand: a register, or a component of the const file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    R(Reg),
    C(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Src {
    pub op: Operand,
    pub abs: bool,
    pub neg: bool,
}

impl Src {
    pub const fn r(r: Reg) -> Self {
        Self { op: Operand::R(r), abs: false, neg: false }
    }
    pub const fn c(c: u16) -> Self {
        Self { op: Operand::C(c), abs: false, neg: false }
    }
}

/// The sync, delay and repeat flags every category shares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub ss: bool,
    pub sy: bool,
    pub jp: bool,
    pub sat: bool,
    pub nop: u8,
}

/// `cat1` types, by their values in the XML.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    F32 = 1,
    U32 = 3,
    S32 = 5,
}

type Fields = Vec<(&'static str, Value)>;

fn bits(v: u64) -> Value {
    Value::Bits(v)
}

/// The field `name` of `leaf`'s chain: the default cases first, then the overrides.
fn field(leaf: u16, name: &str) -> Option<&'static isa::Field> {
    let mut at = Some(leaf);
    while let Some(n) = at {
        let b = &BITSETS[n as usize];
        for c in b.cases.iter().rev() {
            if let Some(f) = c.fields.iter().find(|f| f.name == name && f.expr.is_none()) {
                return Some(f);
            }
        }
        at = b.parent;
    }
    None
}

/// An [`Instr`] of the leaf `name` under `root`, with these fields. The default cases' asserted bits are set.
pub fn make(root: u16, name: &str, values: Fields) -> Result<Instr, String> {
    let leaf = isa::leaf_named(root, name, GPU).ok_or_else(|| format!("no leaf {}", name))?;
    let (mut assert_mask, mut assert_bits) = (0, 0);
    let mut at = Some(leaf);
    while let Some(n) = at {
        let b = &BITSETS[n as usize];
        for f in b.cases.last().unwrap().fields.iter().filter(|f| f.ty == Ty::Assert) {
            assert_mask |= crate::pack::field_mask(f.low, f.high);
            assert_bits |= f.assert_val << f.low;
        }
        at = b.parent;
    }
    let mut fields = alloc::collections::BTreeMap::new();
    for (k, value) in values {
        let f = field(leaf, k).ok_or_else(|| format!("{} has no field {}", name, k))?;
        fields.insert(f.name, Placed { low: f.low, high: f.high, value });
    }
    Ok(Instr { bitset: leaf, fields, assert_mask, assert_bits })
}

fn root_of(leaf_parent: &str) -> Result<u16, String> {
    isa::ROOT_LEAVES
        .iter()
        .map(|(r, _)| *r)
        .find(|r| BITSETS[*r as usize].name == leaf_parent)
        .ok_or_else(|| format!("no root {}", leaf_parent))
}

fn gpr(r: Reg) -> Result<Value, String> {
    let root = root_of("#reg-gpr")?;
    Ok(Value::Nested(make(root, "#reg-gpr", vec![("GPR", bits(u64::from(r.0 >> 2))), ("SWIZ", bits(u64::from(r.0 & 3)))])?))
}

fn konst(c: u16) -> Result<Value, String> {
    let root = root_of("#reg-const")?;
    Ok(Value::Nested(make(root, "#reg-const", vec![("CONST", bits(u64::from(c >> 2))), ("SWIZ", bits(u64::from(c & 3)))])?))
}

fn absneg(s: &Src) -> u64 {
    u64::from(s.neg) | (u64::from(s.abs) << 1)
}

fn multisrc(s: &Src) -> Result<Value, String> {
    let root = root_of("#multisrc")?;
    let (leaf, reg) = match s.op {
        Operand::R(r) => ("#multisrc-gpr", gpr(r)?),
        Operand::C(c) => ("#multisrc-const", konst(c)?),
    };
    Ok(Value::Nested(make(root, leaf, vec![("SRC", reg), ("ABSNEG", bits(absneg(s)))])?))
}

fn common(v: &mut Fields, fl: Flags) {
    v.push(("SS", bits(fl.ss.into())));
    v.push(("SY", bits(fl.sy.into())));
    v.push(("JP", bits(fl.jp.into())));
}

fn done(i: Instr) -> Result<u64, String> {
    isa::encode(&i)
}

fn root() -> u16 {
    isa::INSTRUCTION
}

/// `(nopN)` of a cat2 or cat3 is written in its two `(r)` bits (`#cat2-cat3-nop-value`), with no repeat.
fn nop_bits(v: &mut Fields, nop: u8, a: &'static str, b: &'static str) -> Result<(), String> {
    if nop > 3 {
        return Err(format!("(nop{}): at most 3", nop));
    }
    v.push((a, bits(u64::from(nop & 1))));
    v.push((b, bits(u64::from(nop >> 1))));
    Ok(())
}

/// A cat2 instruction with two sources (`add.f`, `mul.f`, `min.f`, `and.b`, ...); `cond` for `cmps.*`.
pub fn cat2(name: &str, dst: Reg, a: Src, b: Src, cond: Option<u64>, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("DST", gpr(dst)?), ("FULL", bits(1)), ("SRC1", multisrc(&a)?), ("SRC2", multisrc(&b)?), ("SAT", bits(fl.sat.into()))];
    common(&mut v, fl);
    nop_bits(&mut v, fl.nop, "SRC1_R", "SRC2_R")?;
    if let Some(c) = cond {
        v.push(("COND", bits(c)));
    }
    done(make(root(), name, v)?)
}

/// A cat2 instruction with one source (`floor.f`, `absneg.f`, ...). Its `SRC2` bits are don't-care.
pub fn cat2_1(name: &str, dst: Reg, a: Src, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("DST", gpr(dst)?), ("FULL", bits(1)), ("SRC1", multisrc(&a)?), ("SAT", bits(fl.sat.into()))];
    common(&mut v, fl);
    nop_bits(&mut v, fl.nop, "SRC1_R", "SRC2_R")?;
    done(make(root(), name, v)?)
}

fn cat3_src(s: &Src) -> Result<Value, String> {
    let root = root_of("#cat3-src")?;
    Ok(Value::Nested(match s.op {
        Operand::R(r) => make(root, "#cat3-src-gpr", vec![("SRC", gpr(r)?)])?,
        Operand::C(c) => make(root, "#cat3-src-const-or-immed", vec![("CONST", bits(u64::from(c >> 2))), ("SWIZ", bits(u64::from(c & 3)))])?,
    }))
}

/// A cat3 instruction (`mad.f32`, `sel.b32`, ...): `b` must be a register. `(abs)` does not exist here.
pub fn cat3(name: &str, dst: Reg, a: Src, b: Reg, b_neg: bool, c: Src, fl: Flags) -> Result<u64, String> {
    if a.abs || c.abs {
        return Err(String::from("cat3 has no (abs)"));
    }
    let mut v: Fields = vec![
        ("DST", gpr(dst)?),
        ("SRC1", cat3_src(&a)?),
        ("SRC1_NEG", bits(a.neg.into())),
        ("SRC2", gpr(b)?),
        ("SRC2_NEG", bits(b_neg.into())),
        ("SRC3", cat3_src(&c)?),
        ("SRC3_NEG", bits(c.neg.into())),
        ("SAT", bits(fl.sat.into())),
    ];
    common(&mut v, fl);
    nop_bits(&mut v, fl.nop, "SRC1_R", "SRC2_R")?;
    done(make(root(), name, v)?)
}

/// A cat4 instruction (`rcp`, `sqrt`, ...), which the SFU runs: a reader needs `(ss)`.
pub fn cat4(name: &str, dst: Reg, a: Src, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("DST", gpr(dst)?), ("FULL", bits(1)), ("SRC", multisrc(&a)?), ("SAT", bits(fl.sat.into()))];
    common(&mut v, fl);
    done(make(root(), name, v)?)
}

/// `mov` or `cov` between 32-bit types.
pub fn mov(from: Type, to: Type, dst: Reg, s: Operand, fl: Flags) -> Result<u64, String> {
    let (rt, leaf, field) = match s {
        Operand::R(r) => ("#cat1-gpr-src", "mov-gpr", gpr(r)?),
        Operand::C(c) => ("#cat1-const-src", "mov-const", konst(c)?),
    };
    let inner = match s {
        Operand::R(_) => vec![("SRC", field)],
        Operand::C(_) => vec![("CONST", field)],
    };
    let src = Value::Nested(make(root_of(rt)?, rt, inner)?);
    let d = Value::Nested(make(root_of("#cat1-dst")?, "#cat1-dst", vec![("DST", gpr(dst)?)])?);
    let mut v: Fields = vec![("DST", d), ("SRC", src), ("SRC_TYPE", bits(from as u64)), ("DST_TYPE", bits(to as u64))];
    common(&mut v, fl);
    done(make(root(), leaf, v)?)
}

/// `mov` of a 32-bit immediate, the raw bits.
pub fn mov_imm(ty: Type, dst: Reg, bits_: u32, fl: Flags) -> Result<u64, String> {
    let d = Value::Nested(make(root_of("#cat1-dst")?, "#cat1-dst", vec![("DST", gpr(dst)?)])?);
    let src = Value::Nested(make(root_of("#cat1-immed-src")?, "#cat1-immed-src", vec![("IMMED", bits(u64::from(bits_)))])?);
    let mut v: Fields = vec![("DST", d), ("SRC", src), ("SRC_TYPE", bits(ty as u64)), ("DST_TYPE", bits(ty as u64))];
    common(&mut v, fl);
    done(make(root(), "mov-immed", v)?)
}

/// `nop`, with `(rptN)` for N further cycles.
pub fn nop(rpt: u8, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("REPEAT", bits(rpt.into()))];
    common(&mut v, fl);
    done(make(root(), "nop", v)?)
}

/// `end`.
pub fn end(fl: Flags) -> Result<u64, String> {
    let mut v = Vec::new();
    common(&mut v, fl);
    done(make(root(), "end", v)?)
}

/// `br p0.x, #offset`, inverted for `!p0.x`. Offset in instructions from this one.
pub fn br(inverted: bool, offset: i32, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("IMMED", bits(u64::from(offset as u32))), ("INV1", bits(inverted.into()))];
    common(&mut v, fl);
    done(make(root(), "br", v)?)
}

/// `jump #offset`.
pub fn jump(offset: i32, fl: Flags) -> Result<u64, String> {
    let mut v: Fields = vec![("IMMED", bits(u64::from(offset as u32)))];
    common(&mut v, fl);
    done(make(root(), "jump", v)?)
}
