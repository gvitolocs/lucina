// SPDX-License-Identifier: MIT
//! The ir3 instruction set as data (Mesa's `isa/ir3*.xml`, read by `build.rs`), and Mesa's isaspec decoder over it: what
//! `src/compiler/isaspec/isaspec_decode_impl.c` does, in Rust. It finds the leaf bitset a word matches for a GPU generation,
//! resolves fields through cases, overrides, parameters and parents, evaluates the XML's C expressions, and prints the
//! display templates. The text is Mesa's, so its disassembler tests (`tests/data/mesa-disasm.c`) are the oracle.

use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::cell::RefCell;

/// A field's type (`isa.py`'s builtin types, an enum, or another bitset hierarchy by its root).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Branch,
    AbsBranch,
    Int,
    Uint,
    Hex,
    Offset,
    UOffset,
    Float,
    Bool,
    BoolInv,
    Assert,
    Enum(u16),
    Bitset(u16),
}

#[derive(Debug)]
pub struct Field {
    pub name: &'static str,
    pub low: u32,
    pub high: u32,
    pub ty: Ty,
    pub display: Option<&'static str>,
    pub expr: Option<u16>,
    pub params: &'static [(&'static str, &'static str)],
    pub call: bool,
    pub assert_val: u64,
}

#[derive(Debug)]
pub struct Case {
    pub expr: Option<u16>,
    pub display: Option<&'static str>,
    pub fields: &'static [Field],
}

#[derive(Debug)]
pub struct Bitset {
    pub name: &'static str,
    pub display_name: &'static str,
    pub size: u32,
    pub parent: Option<u16>,
    pub root: u16,
    pub gen_min: u32,
    pub gen_max: u32,
    pub match_: u64,
    pub dontcare: u64,
    pub mask: u64,
    pub field_mask: u64,
    pub leaf: bool,
    pub cases: &'static [Case],
}

include!(concat!(env!("OUT_DIR"), "/isa_tables.rs"));

/// The leaves under a root bitset.
pub fn leaves_of(root: u16) -> &'static [u16] {
    ROOT_LEAVES.iter().find(|(r, _)| *r == root).map(|(_, l)| *l).unwrap_or(&[])
}

/// The leaf bitset of `root` that `val` matches for `gpu_id` (`find_bitset`); `Err` names a conflict or no match.
pub fn find_bitset(root: u16, gpu_id: u32, val: u64) -> Result<u16, String> {
    let mut found: Option<u16> = None;
    for &n in leaves_of(root) {
        let b = &BITSETS[n as usize];
        if gpu_id > b.gen_max || gpu_id < b.gen_min {
            continue;
        }
        if (val & b.mask) & !b.dontcare != b.match_ {
            continue;
        }
        if let Some(m) = found {
            return Err(format!("bitset conflict: {} vs {}", BITSETS[m as usize].name, b.name));
        }
        found = Some(n);
    }
    found.ok_or_else(|| format!("no match: {:016x}", val))
}

struct Scope<'a> {
    bitset: u16,
    val: u64,
    params: &'static [(&'static str, &'static str)],
    parent: Option<&'a Scope<'a>>,
    cache: RefCell<BTreeMap<u16, u64>>,
}

/// The decoder's state: the GPU, the expression stack (Mesa's recursion guard), the output and its errors.
pub struct Decoder {
    pub gpu_id: u32,
    expr_stack: RefCell<Vec<u16>>,
    errors: RefCell<Vec<String>>,
    out: RefCell<String>,
}

fn extract(val: u64, low: u32, high: u32) -> u64 {
    let width = 1 + high - low;
    let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
    (val >> low) & mask
}

fn sign_extend(val: u64, width: u32) -> i64 {
    if width == 0 || width >= 64 {
        return val as i64;
    }
    let shift = 64 - width;
    ((val << shift) as i64) >> shift
}

impl Decoder {
    pub fn new(gpu_id: u32) -> Self {
        Self { gpu_id, expr_stack: RefCell::new(Vec::new()), errors: RefCell::new(Vec::new()), out: RefCell::new(String::new()) }
    }

    fn error(&self, msg: String) {
        self.errors.borrow_mut().push(msg);
    }

    /// One instruction word as Mesa prints it; `Err` with the decoder's errors if any. A word no leaf matches prints as
    /// `raw 0x...`, as ir3's disassembler does.
    pub fn disasm(&self, word: u64) -> Result<String, String> {
        self.out.borrow_mut().clear();
        self.errors.borrow_mut().clear();
        // ir3's `no_match_cb` (`disasm-a3xx.c`): the word as a raw instruction.
        let Ok(b) = find_bitset(INSTRUCTION, self.gpu_id, word) else {
            return Ok(format!("raw 0x{:016X}", word));
        };
        // Set don't-care bits are only a warning in Mesa (`find_bitset`); the text is printed as usual.
        let scope = Scope { bitset: b, val: word, params: &[], parent: None, cache: RefCell::new(BTreeMap::new()) };
        self.display(&scope);
        let errors = self.errors.borrow();
        if errors.is_empty() { Ok(self.out.borrow().clone()) } else { Err(errors.join("; ")) }
    }

    fn evaluate(&self, scope: &Scope, expr: u16) -> u64 {
        if let Some(v) = scope.cache.borrow().get(&expr) {
            return *v;
        }
        {
            let stack = self.expr_stack.borrow();
            // `push_expr`: Mesa's loop stops above index 0.
            if (1..stack.len()).rev().any(|i| stack[i] == expr) {
                return 0;
            }
        }
        self.expr_stack.borrow_mut().push(expr);
        let text = EXPRS[expr as usize].1;
        let ret = match Expr::new(text).eval(&mut |name| self.field_value(scope, name), self.gpu_id) {
            Ok(v) => v as u64,
            Err(e) => {
                self.error(format!("expression {}: {}", EXPRS[expr as usize].0, e));
                0
            }
        };
        self.expr_stack.borrow_mut().pop();
        scope.cache.borrow_mut().insert(expr, ret);
        ret
    }

    fn find_field(&self, scope: &Scope, bitset: u16, name: &str) -> Option<&'static Field> {
        let b = &BITSETS[bitset as usize];
        for c in b.cases {
            if let Some(e) = c.expr {
                // While an expression is evaluated, assume it true for its own case (overrides may name their own fields).
                let current = self.expr_stack.borrow().last().copied();
                if current != Some(e) && self.evaluate(scope, e) == 0 {
                    continue;
                }
            }
            if let Some(f) = c.fields.iter().find(|f| f.name == name) {
                return Some(f);
            }
        }
        b.parent.and_then(|p| self.find_field(scope, p, name))
    }

    fn find_display(&self, scope: &Scope, bitset: u16) -> Option<&'static str> {
        let b = &BITSETS[bitset as usize];
        for c in b.cases {
            if let Some(e) = c.expr {
                if self.evaluate(scope, e) == 0 {
                    continue;
                }
            }
            for f in c.fields.iter().filter(|f| f.ty == Ty::Assert) {
                let v = extract(scope.val, f.low, f.high);
                if v != f.assert_val {
                    self.error(format!("unexpected bits[{}:{}] in {}: {:x} vs {:x}", f.low, f.high, b.name, v, f.assert_val));
                }
            }
            if let Some(d) = c.display {
                return Some(d);
            }
        }
        b.parent.and_then(|p| self.find_display(scope, p))
    }

    fn resolve<'s>(&self, scope: &'s Scope<'s>, name: &str) -> Option<(&'static Field, u64, &'s Scope<'s>)> {
        let mut field = self.find_field(scope, scope.bitset, name);
        if field.is_none() {
            if let Some((p, _)) = scope.params.iter().find(|(_, as_)| *as_ == name) {
                return self.resolve(scope.parent?, p);
            }
        }
        let f = field.take()?;
        let v = match f.expr {
            Some(e) => self.evaluate(scope, e),
            None => extract(scope.val, f.low, f.high),
        };
        Some((f, v, scope))
    }

    fn field_value(&self, scope: &Scope, name: &str) -> Result<i64, String> {
        match self.resolve(scope, name) {
            Some((_, v, _)) => Ok(v as i64),
            None => {
                self.error(format!("no field '{}'", name));
                Ok(0)
            }
        }
    }

    fn print(&self, s: &str) {
        self.out.borrow_mut().push_str(s);
    }

    fn column(&self) -> usize {
        self.out.borrow().rsplit('\n').next().map_or(0, |l| l.chars().count())
    }

    fn display_field(&self, scope: &Scope, spec: &str) {
        let (name, align) = match spec.find(":align=") {
            Some(at) => (&spec[..at], spec[at + 7..].parse::<usize>().unwrap_or(0)),
            None => (spec, 0),
        };
        if name.is_empty() {
            while self.column() < align {
                self.print(" ");
            }
            return;
        }
        if name == "NAME" {
            while self.column() < align {
                self.print(" ");
            }
            self.print(BITSETS[scope.bitset as usize].name);
            return;
        }
        let Some((field, val, _)) = self.resolve(scope, name) else {
            self.error(format!("no field '{}'", name));
            return;
        };
        let width = 1 + field.high - field.low;
        while self.column() < align {
            self.print(" ");
        }
        match field.ty {
            Ty::Branch | Ty::AbsBranch | Ty::Int => self.print(&format!("{}", sign_extend(val, width))),
            Ty::Uint => self.print(&format!("{}", val)),
            Ty::Hex => self.print(&format!("{:x}", val)),
            Ty::Offset => {
                if val != 0 {
                    self.print(&format!("{:+}", sign_extend(val, width)))
                }
            }
            Ty::UOffset => {
                if val != 0 {
                    self.print(&format!("+{}", val))
                }
            }
            Ty::Float => {
                let f = if width == 16 { half_to_f32(val as u16) } else { f32::from_bits(val as u32) };
                self.print(&format_f(f));
            }
            Ty::Bool => match field.display {
                Some(d) => {
                    if val != 0 {
                        self.print(d)
                    }
                }
                None => self.print(&format!("{}", (val != 0) as u32)),
            },
            Ty::BoolInv => match field.display {
                Some(d) => {
                    if val == 0 {
                        self.print(d)
                    }
                }
                None => self.print(&format!("{}", (val == 0) as u32)),
            },
            Ty::Enum(e) => {
                let (_, values) = ENUMS[e as usize];
                match values.iter().find(|(v, _)| *v == val) {
                    Some((_, d)) => self.print(d),
                    None => self.print(&format!("{}", val as u32)),
                }
            }
            Ty::Assert => self.error(format!("assert field {} displayed", field.name)),
            Ty::Bitset(root) => match find_bitset(root, self.gpu_id, val) {
                Ok(b) => {
                    let nested = Scope { bitset: b, val, params: field.params, parent: Some(scope), cache: RefCell::new(BTreeMap::new()) };
                    self.display(&nested);
                }
                Err(_) => self.error(format!("no match: FIELD: '{}.{}': {:x}", BITSETS[scope.bitset as usize].name, field.name, val)),
            },
        }
    }

    fn display(&self, scope: &Scope) {
        let Some(template) = self.find_display(scope, scope.bitset) else {
            self.error(format!("{}: no display template", BITSETS[scope.bitset as usize].name));
            return;
        };
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            self.print(&rest[..open]);
            let close = rest[open..].find('}').map(|c| open + c).unwrap_or(rest.len());
            self.display_field(scope, &rest[open + 1..close]);
            rest = rest.get(close + 1..).unwrap_or("");
        }
        self.print(rest);
    }
}

/// `printf("%f")`: six decimals, `inf`/`nan` as glibc spells them.
fn format_f(f: f32) -> String {
    if f.is_nan() {
        return String::from(if f.is_sign_negative() { "-nan" } else { "nan" });
    }
    if f.is_infinite() {
        return String::from(if f < 0.0 { "-inf" } else { "inf" });
    }
    format!("{:.6}", f as f64)
}

/// `_mesa_half_to_float`.
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = if exp == 0 {
        if man == 0 {
            sign
        } else {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
    } else if exp == 0x1f {
        sign | 0x7f80_0000 | (man << 13)
    } else {
        sign | ((exp + 127 - 15) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

/// The XML's C expressions: integers, `{FIELD}`s, `ISA_GPU_ID()`, `! ~ * + - << >> < <= > >= == != & ^ | && || ?:`,
/// parentheses, comments, a trailing `;`. Evaluated in `int64_t`, as Mesa's generated functions declare their fields.
pub struct Expr<'a> {
    s: &'a [u8],
    at: usize,
}

type Lookup<'f> = dyn FnMut(&str) -> Result<i64, String> + 'f;

impl<'a> Expr<'a> {
    pub fn new(text: &'a str) -> Self {
        Self { s: text.as_bytes(), at: 0 }
    }

    pub fn eval(mut self, field: &mut Lookup, gpu_id: u32) -> Result<i64, String> {
        let v = self.ternary(field, gpu_id)?;
        self.skip();
        if self.at < self.s.len() && self.s[self.at] == b';' {
            self.at += 1;
            self.skip();
        }
        if self.at != self.s.len() {
            return Err(format!("trailing text at {}", self.at));
        }
        Ok(v)
    }

    fn skip(&mut self) {
        loop {
            while self.at < self.s.len() && (self.s[self.at] as char).is_ascii_whitespace() {
                self.at += 1;
            }
            if self.s[self.at..].starts_with(b"/*") {
                match self.s[self.at + 2..].windows(2).position(|w| w == b"*/") {
                    Some(p) => self.at += p + 4,
                    None => self.at = self.s.len(),
                }
                continue;
            }
            break;
        }
    }

    fn eat(&mut self, tok: &str) -> bool {
        self.skip();
        if self.s[self.at..].starts_with(tok.as_bytes()) {
            // `&` must not take `&&`, `|` not `||`, `<` not `<<` or `<=`, `!` not `!=`.
            let next = self.s.get(self.at + tok.len()).copied();
            let clash = match tok {
                "&" => next == Some(b'&'),
                "|" => next == Some(b'|'),
                "<" => next == Some(b'<') || next == Some(b'='),
                ">" => next == Some(b'>') || next == Some(b'='),
                "!" => next == Some(b'='),
                "=" => next == Some(b'='),
                _ => false,
            };
            if !clash {
                self.at += tok.len();
                return true;
            }
        }
        false
    }

    fn ternary(&mut self, f: &mut Lookup, g: u32) -> Result<i64, String> {
        let c = self.binary(f, g, 0)?;
        if self.eat("?") {
            let a = self.ternary(f, g)?;
            if !self.eat(":") {
                return Err(String::from("expected ':'"));
            }
            let b = self.ternary(f, g)?;
            return Ok(if c != 0 { a } else { b });
        }
        Ok(c)
    }

    /// Precedence climbing over C's binary operators.
    fn binary(&mut self, f: &mut Lookup, g: u32, level: usize) -> Result<i64, String> {
        const LEVELS: &[&[&str]] = &[
            &["||"],
            &["&&"],
            &["|"],
            &["^"],
            &["&"],
            &["==", "!="],
            &["<=", ">=", "<", ">"],
            &["<<", ">>"],
            &["+", "-"],
            &["*", "/", "%"],
        ];
        if level == LEVELS.len() {
            return self.unary(f, g);
        }
        let mut lhs = self.binary(f, g, level + 1)?;
        'outer: loop {
            for op in LEVELS[level] {
                if self.eat(op) {
                    let rhs = self.binary(f, g, level + 1)?;
                    lhs = match *op {
                        "||" => ((lhs != 0) || (rhs != 0)) as i64,
                        "&&" => ((lhs != 0) && (rhs != 0)) as i64,
                        "|" => lhs | rhs,
                        "^" => lhs ^ rhs,
                        "&" => lhs & rhs,
                        "==" => (lhs == rhs) as i64,
                        "!=" => (lhs != rhs) as i64,
                        "<=" => (lhs <= rhs) as i64,
                        ">=" => (lhs >= rhs) as i64,
                        "<" => (lhs < rhs) as i64,
                        ">" => (lhs > rhs) as i64,
                        "<<" => lhs.wrapping_shl(rhs as u32),
                        ">>" => lhs.wrapping_shr(rhs as u32),
                        "+" => lhs.wrapping_add(rhs),
                        "-" => lhs.wrapping_sub(rhs),
                        "*" => lhs.wrapping_mul(rhs),
                        "/" => lhs.checked_div(rhs).ok_or("division by zero")?,
                        _ => lhs.checked_rem(rhs).ok_or("division by zero")?,
                    };
                    continue 'outer;
                }
            }
            return Ok(lhs);
        }
    }

    fn unary(&mut self, f: &mut Lookup, g: u32) -> Result<i64, String> {
        if self.eat("!") {
            return Ok((self.unary(f, g)? == 0) as i64);
        }
        if self.eat("~") {
            return Ok(!self.unary(f, g)?);
        }
        if self.eat("-") {
            return Ok(self.unary(f, g)?.wrapping_neg());
        }
        if self.eat("(") {
            let v = self.ternary(f, g)?;
            if !self.eat(")") {
                return Err(String::from("expected ')'"));
            }
            return Ok(v);
        }
        if self.eat("{") {
            let start = self.at;
            while self.at < self.s.len() && self.s[self.at] != b'}' {
                self.at += 1;
            }
            let name = core::str::from_utf8(&self.s[start..self.at]).map_err(|_| "utf-8")?.to_string();
            self.at += 1;
            return f(&name);
        }
        if self.eat("ISA_GPU_ID()") {
            return Ok(i64::from(g));
        }
        self.skip();
        let start = self.at;
        while self.at < self.s.len() && (self.s[self.at] as char).is_ascii_alphanumeric() {
            self.at += 1;
        }
        let tok = core::str::from_utf8(&self.s[start..self.at]).map_err(|_| "utf-8")?;
        let digits = tok.trim_end_matches(|c| c == 'U' || c == 'L' || c == 'u' || c == 'l');
        let v = if let Some(h) = digits.strip_prefix("0x") {
            i64::from_str_radix(h, 16)
        } else {
            digits.parse::<i64>()
        };
        v.map_err(|_| format!("not a number: '{}'", tok))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(text: &str, fields: &[(&str, i64)]) -> i64 {
        Expr::new(text)
            .eval(&mut |n| fields.iter().find(|(k, _)| *k == n).map(|(_, v)| *v).ok_or(String::from("no field")), 630)
            .unwrap()
    }

    #[test]
    fn expressions_are_c() {
        assert_eq!(eval("2ULL << {W}", &[("W", 3)]), 16);
        assert_eq!(eval("({DST_HI} << 8) | {DST_LO}", &[("DST_HI", 1), ("DST_LO", 2)]), 0x102);
        assert_eq!(eval("{TYPE} >= 6 ? 0 /* u8 */ : ({TYPE_HALF} ? 1 : 2);", &[("TYPE", 2), ("TYPE_HALF", 1)]), 1);
        assert_eq!(eval("!{FULL}", &[("FULL", 0)]), 1);
        assert_eq!(eval("ISA_GPU_ID() >= 600 && ISA_GPU_ID() < 700", &[]), 1);
        assert_eq!(eval("({SRC_TYPE} == 3 /* u32 */) && ({IMMED} > 0x1000)", &[("SRC_TYPE", 3), ("IMMED", 0x1001)]), 1);
        assert_eq!(eval("{A} != {B}", &[("A", 1), ("B", 1)]), 0);
    }

    #[test]
    fn halves() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x0001), 5.960_464_5e-8);
    }
}

/// A decoded or to-be-encoded instruction: its leaf bitset, every encoded field (not derived ones) with its place, a field of
/// another bitset hierarchy as its own [`Instr`], and the bits the cases that apply assert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instr {
    pub bitset: u16,
    pub fields: BTreeMap<&'static str, Placed>,
    pub assert_mask: u64,
    pub assert_bits: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub low: u32,
    pub high: u32,
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Bits(u64),
    Nested(Instr),
}

/// The leaf of `root` named `name` that exists for `gpu_id`.
pub fn leaf_named(root: u16, name: &str, gpu_id: u32) -> Option<u16> {
    leaves_of(root).iter().copied().find(|&n| {
        let b = &BITSETS[n as usize];
        b.name == name && gpu_id >= b.gen_min && gpu_id <= b.gen_max
    })
}

impl Decoder {
    /// The fields of `word` under `root` (an instruction: [`INSTRUCTION`]): those of every case that applies, up the parent
    /// chain, as the display would resolve them; a default case's field under bits an applicable override defines is shadowed.
    pub fn fields(&self, root: u16, word: u64) -> Result<Instr, String> {
        self.errors.borrow_mut().clear();
        let b = find_bitset(root, self.gpu_id, word)?;
        let scope = Scope { bitset: b, val: word, params: &[], parent: None, cache: RefCell::new(BTreeMap::new()) };
        let instr = self.collect(&scope);
        let errors = self.errors.borrow();
        if errors.is_empty() { Ok(instr) } else { Err(errors.join("; ")) }
    }

    fn collect(&self, scope: &Scope) -> Instr {
        let mut fields = BTreeMap::new();
        let (mut shadow, mut assert_mask, mut assert_bits) = (0u64, 0u64, 0u64);
        let mut at = Some(scope.bitset);
        while let Some(n) = at {
            let b = &BITSETS[n as usize];
            for c in b.cases {
                let is_default = c.expr.is_none();
                if let Some(e) = c.expr {
                    if self.evaluate(scope, e) == 0 {
                        continue;
                    }
                }
                let mut defined = 0u64;
                for f in c.fields.iter().filter(|f| f.expr.is_none()) {
                    let m = mask(f.low, f.high);
                    if is_default && m & shadow != 0 {
                        continue;
                    }
                    if f.ty == Ty::Assert {
                        assert_mask |= m;
                        assert_bits |= f.assert_val << f.low;
                        continue;
                    }
                    defined |= m;
                    if fields.contains_key(f.name) {
                        continue;
                    }
                    let raw = extract(scope.val, f.low, f.high);
                    let value = match f.ty {
                        Ty::Bitset(root) => match find_bitset(root, self.gpu_id, raw) {
                            Ok(nb) => {
                                let nested = Scope { bitset: nb, val: raw, params: f.params, parent: Some(scope), cache: RefCell::new(BTreeMap::new()) };
                                Value::Nested(self.collect(&nested))
                            }
                            Err(_) => Value::Bits(raw),
                        },
                        _ => Value::Bits(raw),
                    };
                    fields.insert(f.name, Placed { low: f.low, high: f.high, value });
                }
                if !is_default {
                    shadow |= defined;
                }
            }
            at = b.parent;
        }
        Instr { bitset: scope.bitset, fields, assert_mask, assert_bits }
    }
}

/// Encodes `instr`: the leaf's pattern, the asserted bits, and each field at its place. `Err` if a value is too wide or two
/// fields disagree on a bit.
pub fn encode(instr: &Instr) -> Result<u64, String> {
    let b = &BITSETS[instr.bitset as usize];
    let mut word = b.match_ | instr.assert_bits;
    let mut covered = b.mask | instr.assert_mask;
    for (name, p) in &instr.fields {
        let raw = match &p.value {
            Value::Bits(x) => *x,
            Value::Nested(i) => encode(i)?,
        };
        (word, covered) = crate::pack::place(word, covered, p.low, p.high, raw)
            .ok_or_else(|| format!("{}.{}: {:#x} does not fit {}..{} or disagrees with bits already set", b.name, name, raw, p.low, p.high))?;
    }
    Ok(word)
}

fn mask(low: u32, high: u32) -> u64 {
    let width = 1 + high - low;
    (if width >= 64 { u64::MAX } else { (1u64 << width) - 1 }) << low
}
