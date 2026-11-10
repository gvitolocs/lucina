//! An ir3 interpreter for the compiler's tests: one fiber, what the compiler emits, full-precision registers. It decodes real
//! encoded words through the ISA tables, and refuses code that breaks the timing rules (`delay::need`).
#![allow(dead_code)]

use std::collections::BTreeMap;

use lucina_ir3::{
    delay::{self, Kind, Need},
    frontend::{self, Compiled, Slot},
    isa::{BITSETS, Decoder, INSTRUCTION, Instr, Value},
};

pub struct Machine {
    pub r: [u32; 256],
    pub c: Vec<u32>,
    written: BTreeMap<u16, (u64, Kind)>,
    cycle: u64,
}

fn name(i: &Instr) -> &'static str {
    BITSETS[i.bitset as usize].name
}

fn bits(i: &Instr, f: &str) -> u64 {
    match i.fields.get(f).map(|p| &p.value) {
        Some(Value::Bits(v)) => *v,
        _ => 0,
    }
}

fn nested<'a>(i: &'a Instr, f: &str) -> &'a Instr {
    match i.fields.get(f).map(|p| &p.value) {
        Some(Value::Nested(n)) => n,
        other => panic!("{}.{}: {:?}", name(i), f, other),
    }
}

fn reg_num(i: &Instr) -> u16 {
    (bits(i, "GPR") * 4 + bits(i, "SWIZ")) as u16
}

/// Where a source reads: a register component, or a const component.
#[derive(Clone, Copy)]
enum Loc {
    R(u16),
    C(usize),
    Imm(u32),
}

fn multisrc(i: &Instr) -> (Loc, bool, bool) {
    let an = bits(i, "ABSNEG");
    let (neg, abs) = (an & 1 != 0, an & 2 != 0);
    let n = name(i);
    match n {
        "#multisrc-gpr" => (Loc::R(reg_num(nested(i, "SRC"))), abs, neg),
        "#multisrc-const" => {
            let c = nested(i, "SRC");
            (Loc::C((bits(c, "CONST") * 4 + bits(c, "SWIZ")) as usize), abs, neg)
        }
        _ => panic!("source {} not emulated", n),
    }
}

fn cat3src(i: &Instr) -> Loc {
    match name(i) {
        "#cat3-src-gpr" => Loc::R(reg_num(nested(i, "SRC"))),
        "#cat3-src-const-or-immed" => Loc::C((bits(i, "CONST") * 4 + bits(i, "SWIZ")) as usize),
        n => panic!("cat3 source {} not emulated", n),
    }
}

impl Machine {
    pub fn new(c: Vec<u32>) -> Self {
        Self { r: [0; 256], c, written: BTreeMap::new(), cycle: 0 }
    }

    fn read(&self, l: Loc) -> u32 {
        match l {
            Loc::R(r) => self.r[r as usize],
            Loc::C(c) => self.c[c],
            Loc::Imm(v) => v,
        }
    }

    fn check(&self, disasm: &str, src: Loc, n: u32, consumer: Kind, ss: bool) -> Result<(), String> {
        let Loc::R(r) = src else { return Ok(()) };
        let Some(&(at, producer)) = self.written.get(&r) else { return Ok(()) };
        match delay::need(producer, consumer, n, false) {
            Need::Slots(k) if self.cycle - at <= u64::from(k) => {
                Err(format!("{}: reads r{}.{} {} cycles after it was written, needs more than {}", disasm, r / 4, r % 4, self.cycle - at, k))
            }
            Need::Ss if !ss => Err(format!("{}: reads an SFU result without (ss)", disasm)),
            _ => Ok(()),
        }
    }

    pub fn run(&mut self, code: &[u64]) -> Result<(), String> {
        let d = Decoder::new(600);
        for &w in code {
            let text = d.disasm(w).map_err(|e| format!("{:016x}: {}", w, e))?;
            let i = d.fields(INSTRUCTION, w).map_err(|e| format!("{}: {}", text, e))?;
            let leaf = name(&i);
            let ss = bits(&i, "SS") != 0;
            if ss {
                self.written.retain(|_, v| v.1 != Kind::Sfu);
            }
            match leaf {
                "nop" => {
                    self.cycle += 1 + bits(&i, "REPEAT");
                    continue;
                }
                "end" => return Ok(()),
                _ => {}
            }
            let (kind, srcs, mods, dst): (Kind, Vec<Loc>, Vec<(bool, bool)>, u16) = if leaf.starts_with("mov-") {
                let src = nested(&i, "SRC");
                let l = match leaf {
                    "mov-gpr" => Loc::R(reg_num(nested(src, "SRC"))),
                    "mov-const" => {
                        let c = nested(src, "CONST");
                        Loc::C((bits(c, "CONST") * 4 + bits(c, "SWIZ")) as usize)
                    }
                    "mov-immed" => Loc::Imm(bits(src, "IMMED") as u32),
                    n => return Err(format!("{} not emulated", n)),
                };
                (Kind::Alu, vec![l], vec![(false, false)], reg_num(nested(nested(&i, "DST"), "DST")))
            } else if i.fields.contains_key("SRC1_NEG") {
                let (a, b, c) = (cat3src(nested(&i, "SRC1")), Loc::R(reg_num(nested(&i, "SRC2"))), cat3src(nested(&i, "SRC3")));
                let m = ["SRC1_NEG", "SRC2_NEG", "SRC3_NEG"].map(|f| (false, bits(&i, f) != 0));
                let k = if leaf.starts_with("mad") { Kind::Mad } else { Kind::Alu };
                (k, vec![a, b, c], m.to_vec(), reg_num(nested(&i, "DST")))
            } else if i.fields.contains_key("SRC1") {
                let mut l = vec![];
                let mut m = vec![];
                for f in ["SRC1", "SRC2"] {
                    if i.fields.contains_key(f) {
                        let (loc, abs, neg) = multisrc(nested(&i, f));
                        l.push(loc);
                        m.push((abs, neg));
                    }
                }
                (Kind::Alu, l, m, reg_num(nested(&i, "DST")))
            } else {
                let (loc, abs, neg) = multisrc(nested(&i, "SRC"));
                (Kind::Sfu, vec![loc], vec![(abs, neg)], reg_num(nested(&i, "DST")))
            };
            for (n, s) in srcs.iter().enumerate() {
                self.check(&text, *s, n as u32, kind, ss)?;
            }
            let f = |m: &Self, n: usize| -> f32 {
                let mut x = f32::from_bits(m.read(srcs[n]));
                if mods[n].0 {
                    x = x.abs();
                }
                if mods[n].1 {
                    x = -x;
                }
                x
            };
            let u = |m: &Self, n: usize| m.read(srcs[n]);
            let bool32 = |b: bool| b as u32;
            let cond = bits(&i, "COND");
            let cmp = |o: std::cmp::Ordering| match cond {
                0 => o.is_lt(),
                1 => o.is_le(),
                2 => o.is_gt(),
                3 => o.is_ge(),
                4 => o.is_eq(),
                _ => o.is_ne(),
            };
            let fb = |v: f32| v.to_bits();
            let value: u32 = match leaf {
                "add.f" => fb(f(self, 0) + f(self, 1)),
                "mul.f" => fb(f(self, 0) * f(self, 1)),
                "min.f" => fb(f(self, 0).min(f(self, 1))),
                "max.f" => fb(f(self, 0).max(f(self, 1))),
                "absneg.f" => fb(f(self, 0)),
                "floor.f" => fb(f(self, 0).floor()),
                "ceil.f" => fb(f(self, 0).ceil()),
                "cmps.f" => match f(self, 0).partial_cmp(&f(self, 1)) {
                    Some(o) => bool32(cmp(o)),
                    None => bool32(cond == 5),
                },
                "cmps.u" => bool32(cmp(u(self, 0).cmp(&u(self, 1)))),
                "cmps.s" => bool32(cmp((u(self, 0) as i32).cmp(&(u(self, 1) as i32)))),
                "add.u" => u(self, 0).wrapping_add(u(self, 1)),
                "sub.u" => u(self, 0).wrapping_sub(u(self, 1)),
                "and.b" => u(self, 0) & u(self, 1),
                "or.b" => u(self, 0) | u(self, 1),
                "xor.b" => u(self, 0) ^ u(self, 1),
                "shl.b" => u(self, 0).wrapping_shl(u(self, 1)),
                "shr.b" => u(self, 0).wrapping_shr(u(self, 1)),
                "ashr.b" => ((u(self, 0) as i32).wrapping_shr(u(self, 1))) as u32,
                "mad.f32" => fb(f(self, 0) * f(self, 1) + f(self, 2)),
                "sel.b32" => {
                    if u(self, 1) != 0 { u(self, 0) } else { u(self, 2) }
                }
                "rcp" => fb(1.0 / f(self, 0)),
                "rsq" => fb(1.0 / f(self, 0).sqrt()),
                "sqrt" => fb(f(self, 0).sqrt()),
                "log2" => fb(f(self, 0).log2()),
                "exp2" => fb(f(self, 0).exp2()),
                "sin" => fb(f(self, 0).sin()),
                "cos" => fb(f(self, 0).cos()),
                n if n.starts_with("mov-") => {
                    let (from, to) = (bits(&i, "SRC_TYPE"), bits(&i, "DST_TYPE"));
                    let x = u(self, 0);
                    match (from, to) {
                        (a, b) if a == b => x,
                        (1, 5) => f32::from_bits(x) as i32 as u32,
                        (1, 3) => f32::from_bits(x) as u32,
                        (5, 1) => (x as i32 as f32).to_bits(),
                        (3, 1) => (x as f32).to_bits(),
                        (3, 5) | (5, 3) => x,
                        other => return Err(format!("{}: conversion {:?}", text, other)),
                    }
                }
                n => return Err(format!("{} not emulated", n)),
            };
            let value = if bits(&i, "SAT") != 0 { f32::from_bits(value).clamp(0.0, 1.0).to_bits() } else { value };
            self.r[dst as usize] = value;
            self.written.insert(dst, (self.cycle, kind));
            let nops = if kind != Kind::Sfu && (leaf.contains('.') && !leaf.starts_with("mov-")) { bits(&i, "SRC1_R") + bits(&i, "SRC2_R") * 2 } else { 0 };
            self.cycle += 1 + nops;
        }
        Err(String::from("ran off the end"))
    }
}

/// Compiles `src` and runs `entry` with `inputs` (location, raw components) and `uniform` raw words; outputs by slot.
pub fn run_raw(src: &str, entry: &str, inputs: &[(u32, Vec<u32>)], uniform: &[u32]) -> (Compiled, BTreeMap<u32, Vec<u32>>) {
    let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
    let c = frontend::compile(&module, entry).unwrap();
    let mut m = Machine::new(uniform.to_vec());
    for io in &c.inputs {
        let Slot::Location(l) = io.slot else { continue };
        let v = &inputs.iter().find(|x| x.0 == l).unwrap().1;
        for (r, x) in io.regs.iter().zip(v) {
            m.r[r.0 as usize] = *x;
        }
    }
    m.run(&c.code).unwrap();
    let mut out = BTreeMap::new();
    for io in &c.outputs {
        let key = match io.slot {
            Slot::Location(l) => l,
            Slot::Position => 1000,
        };
        out.insert(key, io.regs.iter().map(|r| m.r[r.0 as usize]).collect());
    }
    (c, out)
}

pub fn f(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}
