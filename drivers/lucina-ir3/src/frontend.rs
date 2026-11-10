// SPDX-License-Identifier: MIT
//! naga's IR to [`crate::ir`], after the choices of Mesa's `ir3_compiler_nir.c` (`fdiv` as `mul.f` of `rcp`, `fsub` as `add.f`
//! with `(neg)`, `fdot` by `mul.f` and `add.f`). First slice: straight-line entry points over scalars, vectors and matrices,
//! with uniforms in the const file. Anything else is an error that names it.

use alloc::{collections::BTreeMap, format, string::String, vec, vec::Vec};

use naga::{
    BinaryOperator as B, Binding, BuiltIn, Expression as E, Handle, Literal, MathFunction as M, Module, ScalarKind, Statement, TypeInner,
};

use crate::{
    asm::{Reg, Type},
    backend,
    ir::{Op, Program, S, V, s},
};

/// A shader input or output: where it lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Location(u32),
    Position,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Io {
    pub slot: Slot,
    pub regs: Vec<Reg>,
}

/// A uniform's place in the const file, in components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uniform {
    pub group: u32,
    pub binding: u32,
    pub start: u16,
    pub comps: u16,
}

#[derive(Clone, Debug)]
pub struct Compiled {
    pub code: Vec<u64>,
    pub inputs: Vec<Io>,
    pub outputs: Vec<Io>,
    pub uniforms: Vec<Uniform>,
}

#[derive(Clone, Debug)]
struct Val {
    comps: Vec<V>,
    kind: ScalarKind,
    /// A matrix: columns and rows (`comps` by columns).
    mat: Option<(usize, usize)>,
    /// A struct's members (`comps` empty).
    fields: Vec<Val>,
}

impl Val {
    fn new(comps: Vec<V>, kind: ScalarKind, mat: Option<(usize, usize)>) -> Self {
        Self { comps, kind, mat, fields: Vec::new() }
    }
}

/// A pointer: to a uniform at a byte offset, or to local components from one on.
#[derive(Clone, Copy, Debug)]
enum Place {
    Uniform(u16),
    Local(Handle<naga::LocalVariable>, usize),
}

struct Cx<'a> {
    m: &'a Module,
    f: &'a naga::Function,
    p: Program,
    vals: BTreeMap<Handle<E>, Val>,
    places: BTreeMap<Handle<E>, (Place, Handle<naga::Type>)>,
    locals: BTreeMap<Handle<naga::LocalVariable>, Vec<V>>,
    args: Vec<Val>,
    uniforms: BTreeMap<Handle<naga::GlobalVariable>, u16>,
}

type R<T> = Result<T, String>;

fn comps_of(m: &Module, ty: Handle<naga::Type>) -> R<(usize, ScalarKind, Option<(usize, usize)>)> {
    Ok(match m.types[ty].inner {
        TypeInner::Scalar(s) => (1, s.kind, None),
        TypeInner::Vector { size, scalar } => (size as usize, scalar.kind, None),
        TypeInner::Matrix { columns, rows, scalar } => ((columns as usize) * (rows as usize), scalar.kind, Some((columns as usize, rows as usize))),
        ref other => return Err(format!("not a value type yet: {:?}", other)),
    })
}

impl Cx<'_> {
    fn imm(&mut self, bits: u32) -> V {
        self.p.emit(Op::Imm(bits), vec![])
    }

    fn fbin(&mut self, name: &'static str, a: S, b: S) -> V {
        self.p.emit(Op::Cat2(name, None), vec![a, b])
    }

    /// `ir3_nir_trig.py`, applied always: `sin` and `cos` take `2pi * fract(x / 2pi + 0.5) - pi`.
    fn trig_range(&mut self, x: V) -> V {
        let (inv, half, two_pi, pi) = (self.imm(0.15915494f32.to_bits()), self.imm(0.5f32.to_bits()), self.imm(6.2831853f32.to_bits()), self.imm(3.14159265f32.to_bits()));
        let scaled = self.fbin("mul.f", s(inv), s(x));
        let shifted = self.fbin("add.f", s(scaled), s(half));
        let floor = self.p.emit(Op::Cat2One("floor.f"), vec![s(shifted)]);
        let fract = self.fbin("add.f", s(shifted), S { neg: true, ..s(floor) });
        let y = self.fbin("mul.f", s(two_pi), s(fract));
        self.fbin("add.f", s(y), S { neg: true, ..s(pi) })
    }

    fn dot(&mut self, a: &[V], b: &[V]) -> V {
        let mut sum = self.fbin("mul.f", s(a[0]), s(b[0]));
        for i in 1..a.len() {
            let m = self.fbin("mul.f", s(a[i]), s(b[i]));
            sum = self.fbin("add.f", s(sum), s(m));
        }
        sum
    }

    /// `(columns, rows)` of a matrix's comps by column; components by row.
    fn mat_vec(&mut self, mat: &[V], (cols, rows): (usize, usize), v: &[V]) -> Vec<V> {
        (0..rows)
            .map(|r| {
                let row: Vec<V> = (0..cols).map(|c| mat[c * rows + r]).collect();
                self.dot(&row, v)
            })
            .collect()
    }

    fn eval(&mut self, h: Handle<E>) -> R<Val> {
        if let Some(v) = self.vals.get(&h) {
            return Ok(v.clone());
        }
        let v = self.lower(h)?;
        self.vals.insert(h, v.clone());
        Ok(v)
    }

    fn is_pointer(&self, h: Handle<E>) -> bool {
        match self.f.expressions[h] {
            E::GlobalVariable(_) | E::LocalVariable(_) => true,
            E::AccessIndex { base, .. } | E::Access { base, .. } => self.is_pointer(base),
            _ => false,
        }
    }

    fn place(&mut self, h: Handle<E>) -> R<(Place, Handle<naga::Type>)> {
        if let Some(p) = self.places.get(&h) {
            return Ok(*p);
        }
        let result = match self.f.expressions[h] {
            E::LocalVariable(l) => (Place::Local(l, 0), self.f.local_variables[l].ty),
            E::GlobalVariable(g) => {
                let base = *self.uniforms.get(&g).ok_or("only uniform globals so far")?;
                (Place::Uniform(base), self.m.global_variables[g].ty)
            }
            E::AccessIndex { base, index } => {
                let (at, ty) = self.place(base)?;
                match self.m.types[ty].inner {
                    TypeInner::Struct { ref members, .. } => {
                        let member = &members[index as usize];
                        match at {
                            Place::Uniform(c) => (Place::Uniform(c + (member.offset / 4) as u16), member.ty),
                            Place::Local(..) => return Err(String::from("local structs not yet")),
                        }
                    }
                    ref other => return Err(format!("access into {:?}", other)),
                }
            }
            ref other => return Err(format!("not a pointer yet: {:?}", other)),
        };
        self.places.insert(h, result);
        Ok(result)
    }

    fn load(&mut self, pointer: Handle<E>) -> R<Val> {
        // A swizzle-like access of a vector through a pointer.
        if let E::AccessIndex { base, index } = self.f.expressions[pointer] {
            let (_, ty) = self.place(base)?;
            if matches!(self.m.types[ty].inner, TypeInner::Vector { .. } | TypeInner::Matrix { .. }) {
                let whole = self.load(base)?;
                return match whole.mat {
                    Some((_, rows)) => Ok(Val::new(whole.comps[index as usize * rows..(index as usize + 1) * rows].to_vec(), whole.kind, None)),
                    None => Ok(Val::new(vec![whole.comps[index as usize]], whole.kind, None)),
                };
            }
        }
        let (place, ty) = self.place(pointer)?;
        let (n, kind, mat) = comps_of(self.m, ty)?;
        let comps = match place {
            Place::Local(l, at) => self.locals.get(&l).ok_or("a local read before it was written")?[at..at + n].to_vec(),
            Place::Uniform(c) => {
                // std140/WGSL layout: a matrix's columns stride 16 bytes except mat2xN's vec2 at 8.
                let stride = mat.map(|(_, rows)| if rows == 2 { 2 } else { 4 });
                (0..n)
                    .map(|i| {
                        let at = match (stride, mat) {
                            (Some(st), Some((_, rows))) => (i / rows) * st + i % rows,
                            _ => i,
                        };
                        self.p.emit(Op::Const(c + at as u16), vec![])
                    })
                    .collect()
            }
        };
        Ok(Val::new(comps, kind, mat))
    }

    fn math(&mut self, fun: M, arg: Handle<E>, arg1: Option<Handle<E>>, arg2: Option<Handle<E>>) -> R<Val> {
        let a = self.eval(arg)?;
        let b = match arg1 {
            Some(h) => Some(self.eval(h)?),
            None => None,
        };
        let c = match arg2 {
            Some(h) => Some(self.eval(h)?),
            None => None,
        };
        if a.kind != ScalarKind::Float {
            return Err(format!("{:?} on {:?}", fun, a.kind));
        }
        let each = |cx: &mut Self, f: &mut dyn FnMut(&mut Self, usize) -> V| -> Val {
            let comps = (0..a.comps.len()).map(|i| f(cx, i)).collect();
            Val::new(comps, ScalarKind::Float, None)
        };
        let sfu = |cx: &mut Self, name: &'static str, v: V| cx.p.emit(Op::Cat4(name), vec![s(v)]);
        Ok(match fun {
            M::Abs => each(self, &mut |cx, i| cx.p.emit(Op::Cat2One("absneg.f"), vec![S { abs: true, ..s(a.comps[i]) }])),
            M::Floor => each(self, &mut |cx, i| cx.p.emit(Op::Cat2One("floor.f"), vec![s(a.comps[i])])),
            M::Ceil => each(self, &mut |cx, i| cx.p.emit(Op::Cat2One("ceil.f"), vec![s(a.comps[i])])),
            M::Fract => each(self, &mut |cx, i| {
                let fl = cx.p.emit(Op::Cat2One("floor.f"), vec![s(a.comps[i])]);
                cx.fbin("add.f", s(a.comps[i]), S { neg: true, ..s(fl) })
            }),
            M::Sqrt => each(self, &mut |cx, i| sfu(cx, "sqrt", a.comps[i])),
            M::InverseSqrt => each(self, &mut |cx, i| sfu(cx, "rsq", a.comps[i])),
            M::Exp2 => each(self, &mut |cx, i| sfu(cx, "exp2", a.comps[i])),
            M::Log2 => each(self, &mut |cx, i| sfu(cx, "log2", a.comps[i])),
            M::Sin => each(self, &mut |cx, i| {
                let y = cx.trig_range(a.comps[i]);
                sfu(cx, "sin", y)
            }),
            M::Cos => each(self, &mut |cx, i| {
                let y = cx.trig_range(a.comps[i]);
                sfu(cx, "cos", y)
            }),
            M::Saturate => each(self, &mut |cx, i| {
                let v = cx.p.emit(Op::Cat2One("absneg.f"), vec![s(a.comps[i])]);
                cx.p.insts.last_mut().unwrap().sat = true;
                v
            }),
            M::Min | M::Max => {
                let b = b.ok_or("two arguments")?;
                let name = if fun == M::Min { "min.f" } else { "max.f" };
                each(self, &mut |cx, i| cx.fbin(name, s(a.comps[i]), s(b.comps[i])))
            }
            M::Clamp => {
                let (lo, hi) = (b.ok_or("clamp")?, c.ok_or("clamp")?);
                each(self, &mut |cx, i| {
                    let m = cx.fbin("max.f", s(a.comps[i]), s(lo.comps[i]));
                    cx.fbin("min.f", s(m), s(hi.comps[i]))
                })
            }
            M::Mix => {
                let (y, t) = (b.ok_or("mix")?, c.ok_or("mix")?);
                each(self, &mut |cx, i| {
                    let d = cx.fbin("add.f", s(y.comps[i]), S { neg: true, ..s(a.comps[i]) });
                    let m = cx.fbin("mul.f", s(d), s(t.comps[i]));
                    cx.fbin("add.f", s(a.comps[i]), s(m))
                })
            }
            M::Pow => {
                let y = b.ok_or("pow")?;
                each(self, &mut |cx, i| {
                    let l = sfu(cx, "log2", a.comps[i]);
                    let m = cx.fbin("mul.f", s(l), s(y.comps[i]));
                    sfu(cx, "exp2", m)
                })
            }
            M::Dot => {
                let y = b.ok_or("dot")?;
                Val::new(vec![self.dot(&a.comps, &y.comps)], ScalarKind::Float, None)
            }
            M::Length => {
                let d = self.dot(&a.comps, &a.comps);
                Val::new(vec![sfu(self, "sqrt", d)], ScalarKind::Float, None)
            }
            M::Normalize => {
                let d = self.dot(&a.comps, &a.comps);
                let inv = sfu(self, "rsq", d);
                each(self, &mut |cx, i| cx.fbin("mul.f", s(a.comps[i]), s(inv)))
            }
            M::Cross => {
                let y = b.ok_or("cross")?;
                let (p, q) = (&a.comps, &y.comps);
                let term = |cx: &mut Self, i: usize, j: usize| {
                    let l = cx.fbin("mul.f", s(p[i]), s(q[j]));
                    let r = cx.fbin("mul.f", s(p[j]), s(q[i]));
                    cx.fbin("add.f", s(l), S { neg: true, ..s(r) })
                };
                let comps = vec![term(self, 1, 2), term(self, 2, 0), term(self, 0, 1)];
                Val::new(comps, ScalarKind::Float, None)
            }
            other => return Err(format!("math {:?} not yet", other)),
        })
    }

    fn binary(&mut self, op: B, left: Handle<E>, right: Handle<E>) -> R<Val> {
        let (a, b) = (self.eval(left)?, self.eval(right)?);
        if let Some(cond) = compare(op) {
            let name = match a.kind {
                ScalarKind::Float => "cmps.f",
                ScalarKind::Sint => "cmps.s",
                ScalarKind::Uint | ScalarKind::Bool => "cmps.u",
                other => return Err(format!("comparison of {:?}", other)),
            };
            let n = a.comps.len().max(b.comps.len());
            let at = |v: &Val, i: usize| if v.comps.len() == 1 { v.comps[0] } else { v.comps[i] };
            let comps = (0..n).map(|i| self.p.emit(Op::Cat2(name, Some(cond)), vec![s(at(&a, i)), s(at(&b, i))])).collect();
            return Ok(Val::new(comps, ScalarKind::Bool, None));
        }
        if a.kind != ScalarKind::Float {
            return self.integer(op, &a, &b);
        }
        match (op, a.mat, b.mat) {
            (B::Multiply, Some(shape), None) if b.comps.len() == shape.0 => {
                return Ok(Val::new(self.mat_vec(&a.comps, shape, &b.comps), ScalarKind::Float, None));
            }
            (B::Multiply, Some((ac, ar)), Some((bc, br))) if ac == br => {
                let mut comps = Vec::new();
                for c in 0..bc {
                    let column = b.comps[c * br..(c + 1) * br].to_vec();
                    comps.extend(self.mat_vec(&a.comps, (ac, ar), &column));
                }
                return Ok(Val::new(comps, ScalarKind::Float, Some((bc, ar))));
            }
            (_, None, None) => {}
            _ => return Err(format!("{:?} on these matrix shapes not yet", op)),
        }
        let n = a.comps.len().max(b.comps.len());
        let at = |v: &Val, i: usize| if v.comps.len() == 1 { v.comps[0] } else { v.comps[i] };
        let mut comps = Vec::with_capacity(n);
        for i in 0..n {
            let (x, y) = (at(&a, i), at(&b, i));
            comps.push(match op {
                B::Add => self.fbin("add.f", s(x), s(y)),
                B::Subtract => self.fbin("add.f", s(x), S { neg: true, ..s(y) }),
                B::Multiply => self.fbin("mul.f", s(x), s(y)),
                B::Divide => {
                    let r = self.p.emit(Op::Cat4("rcp"), vec![s(y)]);
                    self.fbin("mul.f", s(x), s(r))
                }
                other => return Err(format!("{:?} not yet", other)),
            });
        }
        Ok(Val::new(comps, ScalarKind::Float, None))
    }

    fn integer(&mut self, op: B, a: &Val, b: &Val) -> R<Val> {
        let signed = a.kind == ScalarKind::Sint;
        let name = match op {
            B::Add => "add.u",
            B::Subtract => "sub.u",
            B::And | B::LogicalAnd => "and.b",
            B::InclusiveOr | B::LogicalOr => "or.b",
            B::ExclusiveOr => "xor.b",
            B::ShiftLeft => "shl.b",
            B::ShiftRight if signed => "ashr.b",
            B::ShiftRight => "shr.b",
            other => return Err(format!("integer {:?} not yet", other)),
        };
        let n = a.comps.len().max(b.comps.len());
        let at = |v: &Val, i: usize| if v.comps.len() == 1 { v.comps[0] } else { v.comps[i] };
        let comps = (0..n).map(|i| self.fbin(name, s(at(a, i)), s(at(b, i)))).collect();
        Ok(Val::new(comps, a.kind, None))
    }

    fn lower(&mut self, h: Handle<E>) -> R<Val> {
        match self.f.expressions[h] {
            E::Literal(Literal::F32(x)) => Ok(Val::new(vec![self.imm(x.to_bits())], ScalarKind::Float, None)),
            E::Literal(Literal::U32(x)) => Ok(Val::new(vec![self.imm(x)], ScalarKind::Uint, None)),
            E::Literal(Literal::I32(x)) => Ok(Val::new(vec![self.imm(x as u32)], ScalarKind::Sint, None)),
            E::FunctionArgument(i) => Ok(self.args[i as usize].clone()),
            E::Compose { ty, ref components } => {
                if matches!(self.m.types[ty].inner, TypeInner::Struct { .. }) {
                    let mut fields = Vec::new();
                    for c in components.clone() {
                        fields.push(self.eval(c)?);
                    }
                    return Ok(Val { comps: Vec::new(), kind: ScalarKind::Float, mat: None, fields });
                }
                let (_, kind, mat) = comps_of(self.m, ty)?;
                let mut comps = Vec::new();
                for c in components.clone() {
                    comps.extend(self.eval(c)?.comps);
                }
                Ok(Val::new(comps, kind, mat))
            }
            E::Splat { size, value } => {
                let v = self.eval(value)?;
                Ok(Val::new(vec![v.comps[0]; size as usize], v.kind, None))
            }
            E::Swizzle { size, vector, pattern } => {
                let v = self.eval(vector)?;
                let comps = pattern[..size as usize].iter().map(|c| v.comps[*c as usize]).collect();
                Ok(Val::new(comps, v.kind, None))
            }
            E::AccessIndex { base, index } => {
                let v = self.eval(base)?;
                if !v.fields.is_empty() {
                    return Ok(v.fields[index as usize].clone());
                }
                Ok(match v.mat {
                    Some((_, rows)) => Val::new(v.comps[index as usize * rows..(index as usize + 1) * rows].to_vec(), v.kind, None),
                    None => Val::new(vec![v.comps[index as usize]], v.kind, None),
                })
            }
            E::Load { pointer } => self.load(pointer),
            E::Unary { op: naga::UnaryOperator::Negate, expr } => {
                let v = self.eval(expr)?;
                if v.kind != ScalarKind::Float {
                    return Err(String::from("integer negate not yet"));
                }
                let comps = v.comps.iter().map(|c| self.p.emit(Op::Cat2One("absneg.f"), vec![S { neg: true, ..s(*c) }])).collect();
                Ok(Val::new(comps, v.kind, v.mat))
            }
            E::Binary { op, left, right } => self.binary(op, left, right),
            E::Select { condition, accept, reject } => {
                let (c, a, b) = (self.eval(condition)?, self.eval(accept)?, self.eval(reject)?);
                let at = |v: &Val, i: usize| if v.comps.len() == 1 { v.comps[0] } else { v.comps[i] };
                let n = a.comps.len().max(c.comps.len());
                // `sel.b32 dst, a, cond, b`: cond (the middle source) non-zero takes a (Mesa's `nir_op_bcsel`).
                let comps = (0..n).map(|i| self.p.emit(Op::Cat3("sel.b32"), vec![s(at(&a, i)), s(at(&c, i)), s(at(&b, i))])).collect();
                Ok(Val::new(comps, a.kind, None))
            }
            E::Math { fun, arg, arg1, arg2, .. } => self.math(fun, arg, arg1, arg2),
            E::As { expr, kind, convert: Some(4) } => {
                let v = self.eval(expr)?;
                let ty = |k| match k {
                    ScalarKind::Float => Ok(Type::F32),
                    ScalarKind::Uint => Ok(Type::U32),
                    ScalarKind::Sint => Ok(Type::S32),
                    other => Err(format!("conversion with {:?}", other)),
                };
                let (from, to) = (ty(v.kind)?, ty(kind)?);
                let comps = v.comps.iter().map(|c| self.p.emit(Op::Cov(from, to), vec![s(*c)])).collect();
                Ok(Val::new(comps, kind, None))
            }
            ref other => Err(format!("expression not yet: {:?}", other)),
        }
    }

    fn block(&mut self, b: &naga::Block) -> R<Option<Val>> {
        for st in b.iter() {
            match *st {
                Statement::Emit(ref range) => {
                    for h in range.clone() {
                        // A pointer is evaluated by the `Load` that uses it.
                        if !self.is_pointer(h) {
                            self.eval(h)?;
                        }
                    }
                }
                Statement::Store { pointer, value } => {
                    let v = self.eval(value)?;
                    match self.place(pointer)? {
                        (Place::Local(l, 0), _) => {
                            self.locals.insert(l, v.comps);
                        }
                        other => return Err(format!("store to {:?} not yet", other)),
                    }
                }
                Statement::Return { value } => {
                    return match value {
                        Some(h) => Ok(Some(self.eval(h)?)),
                        None => Ok(None),
                    };
                }
                ref other => return Err(format!("statement not yet: {:?}", other)),
            }
        }
        Ok(None)
    }
}

/// The `#cond` value of a comparison (`ir3-cat2.xml`), if `op` is one.
fn compare(op: B) -> Option<u64> {
    Some(match op {
        B::Less => 0,
        B::LessEqual => 1,
        B::Greater => 2,
        B::GreaterEqual => 3,
        B::Equal => 4,
        B::NotEqual => 5,
        _ => return None,
    })
}

fn slot_of(binding: &Option<Binding>) -> R<Slot> {
    match binding {
        Some(Binding::Location { location, .. }) => Ok(Slot::Location(*location)),
        Some(Binding::BuiltIn(BuiltIn::Position { .. })) => Ok(Slot::Position),
        other => Err(format!("binding {:?} not yet", other)),
    }
}

/// Compiles the entry point `name` of `module`.
pub fn compile(module: &Module, name: &str) -> R<Compiled> {
    let ep = module.entry_points.iter().find(|e| e.name == name).ok_or_else(|| format!("no entry point {}", name))?;
    let f = &ep.function;
    let mut layouter = naga::proc::Layouter::default();
    layouter.update(module.to_ctx()).map_err(|e| format!("{:?}", e))?;
    let mut p = Program::default();
    // Uniforms, consecutively in the const file, each on a vec4 boundary.
    let (mut uniforms, mut places, mut next) = (Vec::new(), BTreeMap::new(), 0u16);
    for (h, g) in module.global_variables.iter() {
        if g.space != naga::AddressSpace::Uniform {
            continue;
        }
        let rb = g.binding.as_ref().ok_or("a uniform without a binding")?;
        let bytes = layouter[g.ty].size;
        let comps = (bytes.div_ceil(4)) as u16;
        uniforms.push(Uniform { group: rb.group, binding: rb.binding, start: next, comps });
        places.insert(h, next);
        next += comps.next_multiple_of(4);
    }
    let mut inputs = Vec::new();
    let mut args = Vec::new();
    let mut input_values: Vec<(Slot, Vec<V>)> = Vec::new();
    for a in &f.arguments {
        if let TypeInner::Struct { ref members, .. } = module.types[a.ty].inner {
            let mut fields = Vec::new();
            for member in members {
                let (n, kind, mat) = comps_of(module, member.ty)?;
                let comps: Vec<V> = (0..n).map(|_| p.emit(Op::Input, vec![])).collect();
                input_values.push((slot_of(&member.binding)?, comps.clone()));
                fields.push(Val::new(comps, kind, mat));
            }
            args.push(Val { comps: Vec::new(), kind: ScalarKind::Float, mat: None, fields });
            continue;
        }
        let (n, kind, mat) = comps_of(module, a.ty)?;
        let comps: Vec<V> = (0..n).map(|_| p.emit(Op::Input, vec![])).collect();
        input_values.push((slot_of(&a.binding)?, comps.clone()));
        args.push(Val::new(comps, kind, mat));
    }
    let mut cx = Cx { m: module, f, p, vals: BTreeMap::new(), places: BTreeMap::new(), locals: BTreeMap::new(), args, uniforms: places };
    let ret = cx.block(&f.body)?;
    let mut output_values: Vec<(Slot, Vec<V>)> = Vec::new();
    if let (Some(result), Some(v)) = (&f.result, ret) {
        if let TypeInner::Struct { ref members, .. } = module.types[result.ty].inner {
            for (member, field) in members.iter().zip(&v.fields) {
                output_values.push((slot_of(&member.binding)?, field.comps.clone()));
                cx.p.outputs.extend(field.comps.iter().copied());
            }
        } else {
            output_values.push((slot_of(&result.binding)?, v.comps.clone()));
            cx.p.outputs.extend(v.comps);
        }
    }
    let (code, regs) = backend::compile(&cx.p)?;
    for (slot, comps) in input_values {
        inputs.push(Io { slot, regs: comps.iter().map(|c| regs[c]).collect() });
    }
    let outputs = output_values.into_iter().map(|(slot, c)| Io { slot, regs: c.iter().map(|v| regs[v]).collect() }).collect();
    Ok(Compiled { code, inputs, outputs, uniforms })
}
