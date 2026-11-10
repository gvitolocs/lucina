//! Reads Mesa's ir3 ISA description (`isa/ir3.xml` and its imports, MIT, Mesa 25.2.6) and writes `isa_tables.rs` into OUT_DIR:
//! every bitset with its pattern, cases, fields and display templates, the enums and the expressions, as static data for
//! `src/isa.rs`. The model is Mesa's `src/compiler/isaspec/isa.py` (copy the behaviour, write Rust): patterns merge up the
//! `extends` chain, the default case comes after the overrides, a leaf is a bitset nothing extends, a bitset-typed field decodes
//! over the leaves of its root type, and display templates are resolved here.

use std::{collections::BTreeMap, fmt::Write as _, path::Path};

#[derive(Clone, Debug)]
struct Field {
    name: String,
    low: u32,
    high: u32,
    ty: String,
    display: Option<String>,
    expr: Option<String>,
    params: Vec<(String, String)>,
    call: bool,
    assert_val: Option<u64>,
}

#[derive(Clone, Debug)]
struct Case {
    expr: Option<String>,
    display: Option<String>,
    fields: Vec<Field>,
}

#[derive(Clone, Debug)]
struct Bitset {
    name: String,
    display_name: String,
    size: Option<u32>,
    extends: Option<String>,
    gen_min: u32,
    gen_max: u32,
    match_: u64,
    dontcare: u64,
    mask: u64,
    field_mask: u64,
    cases: Vec<Case>,
}

#[derive(Default)]
struct Isa {
    exprs: BTreeMap<String, String>,
    anon: usize,
    templates: BTreeMap<String, String>,
    enums: BTreeMap<String, Vec<(u64, String)>>,
    bitsets: Vec<Bitset>,
}

fn bitrange(n: roxmltree::Node) -> (u32, u32) {
    if let Some(pos) = n.attribute("pos") {
        let p = pos.parse().unwrap();
        (p, p)
    } else {
        (n.attribute("low").unwrap().parse().unwrap(), n.attribute("high").unwrap().parse().unwrap())
    }
}

fn mask_of(low: u32, high: u32) -> u64 {
    let width = 1 + high - low;
    (if width == 64 { u64::MAX } else { (1u64 << width) - 1 }) << low
}

/// `extract_pattern`: a string of 0, 1 and x over `low..=high`.
fn pattern(n: roxmltree::Node, name: &str) -> (u64, u64, u64) {
    let (low, high) = bitrange(n);
    let text: String = n.text().unwrap_or("").trim().to_string();
    assert_eq!(text.len() as u32, 1 + high - low, "pattern length in {}", name);
    let (mut m, mut d) = (0u64, 0u64);
    for c in text.chars() {
        m <<= 1;
        d <<= 1;
        match c {
            '1' => m |= 1,
            'x' => d |= 1,
            '0' => {}
            _ => panic!("bad pattern char in {}", name),
        }
    }
    (m << low, d << low, mask_of(low, high))
}

fn parse_num(s: &str) -> u64 {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") { u64::from_str_radix(h, 16).unwrap() } else { s.parse().unwrap() }
}

impl Isa {
    fn expression(&mut self, n: roxmltree::Node) -> String {
        let name = match n.attribute("name") {
            Some(name) => name.to_string(),
            None => {
                self.anon += 1;
                format!("anon_{}", self.anon - 1)
            }
        };
        let text = n.text().unwrap_or("").trim().to_string();
        self.exprs.insert(name.clone(), text);
        name
    }

    fn one_expression(&mut self, n: roxmltree::Node) -> String {
        let exprs: Vec<_> = n.children().filter(|c| c.has_tag_name("expr")).collect();
        assert_eq!(exprs.len(), 1);
        self.expression(exprs[0])
    }

    fn case(&mut self, n: roxmltree::Node, bitset: &str, expr: Option<String>, field_mask: &mut u64, default: bool) -> Case {
        let mut fields = Vec::new();
        for d in n.children().filter(|c| c.has_tag_name("derived")) {
            let high = d.attribute("width").map(|w| w.parse::<u32>().unwrap() - 1).unwrap_or(0);
            let expr = match d.attribute("expr") {
                Some(e) => e.to_string(),
                None => self.one_expression(d),
            };
            fields.push(Field {
                name: d.attribute("name").unwrap().into(),
                low: 0,
                high,
                ty: d.attribute("type").unwrap().into(),
                display: d.attribute("display").map(|s| s.trim().to_string()),
                expr: Some(expr),
                params: vec![],
                call: d.attribute("call") == Some("true"),
                assert_val: None,
            });
        }
        let count = fields.len();
        for (i, a) in n.children().filter(|c| c.has_tag_name("assert")).enumerate() {
            let (low, high) = bitrange(a);
            let (m, d, mask) = pattern(a, bitset);
            assert_eq!(d, 0);
            if default {
                *field_mask |= mask;
            }
            fields.push(Field {
                name: format!("{}#assert{}", bitset, count + i),
                low,
                high,
                ty: "assert".into(),
                display: None,
                expr: None,
                params: vec![],
                call: false,
                assert_val: Some(m >> low),
            });
        }
        for f in n.children().filter(|c| c.has_tag_name("field")) {
            let (low, high) = bitrange(f);
            if default {
                assert_eq!(*field_mask & mask_of(low, high), 0, "redefined bits in {}.{}", bitset, f.attribute("name").unwrap());
                *field_mask |= mask_of(low, high);
            }
            fields.push(Field {
                name: f.attribute("name").unwrap().into(),
                low,
                high,
                ty: f.attribute("type").unwrap().into(),
                display: f.attribute("display").map(|s| s.trim().to_string()),
                expr: None,
                params: f
                    .children()
                    .filter(|c| c.has_tag_name("param"))
                    .map(|p| {
                        let name = p.attribute("name").unwrap().to_string();
                        (name.clone(), p.attribute("as").map(String::from).unwrap_or(name))
                    })
                    .collect(),
                call: f.attribute("call") == Some("true"),
                assert_val: None,
            });
        }
        let display = n.children().filter(|c| c.has_tag_name("display")).last().map(|d| d.text().unwrap_or("").trim().to_string());
        Case { expr, display, fields }
    }

    fn bitset(&mut self, n: roxmltree::Node) {
        let name = n.attribute("name").unwrap().to_string();
        let (mut gen_min, mut gen_max) = (0u32, u32::MAX);
        for g in n.children().filter(|c| c.has_tag_name("gen")) {
            if let Some(v) = g.attribute("min") {
                gen_min = v.parse().unwrap();
            }
            if let Some(v) = g.attribute("max") {
                gen_max = v.parse().unwrap();
            }
        }
        let mut field_mask = 0u64;
        let dflt = self.case(n, &name, None, &mut field_mask, true);
        let mut cases = Vec::new();
        for o in n.children().filter(|c| c.has_tag_name("override")) {
            let expr = match o.attribute("expr") {
                Some(e) => e.to_string(),
                None => self.one_expression(o),
            };
            let mut ignored = 0;
            cases.push(self.case(o, &name, Some(expr), &mut ignored, false));
        }
        cases.push(dflt);
        let (mut match_, mut dontcare, mut mask) = (0, 0, 0);
        for p in n.children().filter(|c| c.has_tag_name("pattern")) {
            let (m, d, k) = pattern(p, &name);
            assert_eq!((field_mask | mask | dontcare | match_) & k, 0, "redefined bits in {} pattern", name);
            match_ |= m;
            dontcare |= d;
            mask |= k;
        }
        self.bitsets.push(Bitset {
            display_name: n.attribute("displayname").unwrap_or(&name).to_string(),
            name,
            size: n.attribute("size").map(|s| s.parse().unwrap()),
            extends: n.attribute("extends").map(String::from),
            gen_min,
            gen_max,
            match_,
            dontcare,
            mask,
            field_mask,
            cases,
        });
    }

    fn file(&mut self, dir: &Path, file: &str) {
        let path = dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = roxmltree::Document::parse(&text).unwrap();
        let root = doc.root_element();
        for i in root.children().filter(|c| c.has_tag_name("import")) {
            self.file(dir, i.attribute("file").unwrap());
        }
        for e in root.children().filter(|c| c.has_tag_name("expr")) {
            self.expression(e);
        }
        for t in root.children().filter(|c| c.has_tag_name("template")) {
            self.templates.insert(t.attribute("name").unwrap().into(), t.text().unwrap_or("").trim().to_string());
        }
        for e in root.children().filter(|c| c.has_tag_name("enum")) {
            let values = e
                .children()
                .filter(|c| c.has_tag_name("value"))
                .map(|v| (parse_num(v.attribute("val").unwrap()), v.attribute("display").unwrap().to_string()))
                .collect();
            self.enums.insert(e.attribute("name").unwrap().into(), values);
        }
        for b in root.children().filter(|c| c.has_tag_name("bitset")) {
            self.bitset(b);
        }
    }

    fn resolve_templates(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (name, t) in &self.templates {
            out = out.replace(&format!("{{{}}}", name), t);
        }
        out
    }
}

fn quote(s: &str) -> String {
    format!("{:?}", s)
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("isa");
    let mut isa = Isa::default();
    isa.file(&dir, "ir3.xml");
    for b in &mut isa.bitsets {
        for c in &mut b.cases {
            if let Some(d) = &c.display {
                let mut s = d.clone();
                for (name, t) in &isa.templates {
                    s = s.replace(&format!("{{{}}}", name), t);
                }
                c.display = Some(s);
            }
        }
    }
    let _ = Isa::resolve_templates;

    // Index by name: the unique definition (several definitions of one name are gen variants of a leaf).
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, b) in isa.bitsets.iter().enumerate() {
        by_name.entry(b.name.clone()).or_default().push(i);
    }
    let parent_of = |b: &Bitset| -> Option<usize> {
        b.extends.as_ref().map(|e| {
            let v = &by_name[e];
            assert_eq!(v.len(), 1, "{} extends {}, which has several definitions", b.name, e);
            v[0]
        })
    };
    let extended: std::collections::BTreeSet<String> = isa.bitsets.iter().filter_map(|b| b.extends.clone()).collect();
    let n = isa.bitsets.len();
    let mut root = vec![0usize; n];
    let mut size = vec![0u32; n];
    let mut gmin = vec![0u32; n];
    let mut gmax = vec![0u32; n];
    let mut pat = vec![(0u64, 0u64, 0u64, 0u64); n];
    for i in 0..n {
        let mut chain = vec![i];
        while let Some(p) = parent_of(&isa.bitsets[*chain.last().unwrap()]) {
            chain.push(p);
        }
        let top = *chain.last().unwrap();
        root[i] = top;
        size[i] = isa.bitsets[top].size.unwrap();
        let (mut lo, mut hi) = (0u32, u32::MAX);
        let (mut m, mut d, mut k, mut f) = (0u64, 0u64, 0u64, 0u64);
        for &c in &chain {
            let b = &isa.bitsets[c];
            let defined = b.match_ | b.dontcare | b.mask | b.field_mask;
            assert_eq!((m | d | k | f) & defined, 0, "bitset conflict in {}", isa.bitsets[i].name);
            m |= b.match_;
            d |= b.dontcare;
            k |= b.mask;
            f |= b.field_mask;
            lo = lo.max(b.gen_min);
            hi = hi.min(b.gen_max);
        }
        gmin[i] = lo;
        gmax[i] = hi;
        pat[i] = (m, d, k, f);
    }
    let leaves: Vec<usize> = (0..n).filter(|&i| !extended.contains(&isa.bitsets[i].name)).collect();
    for &l in &leaves {
        let (_, _, k, f) = pat[l];
        let full = mask_of(0, size[l] - 1);
        assert_eq!(k | f, full, "leaf {} has undefined bits", isa.bitsets[l].name);
    }
    let roots: Vec<usize> = (0..n).filter(|&i| isa.bitsets[i].size.is_some()).collect();
    let root_index = |name: &str| -> usize {
        let v = &by_name[name];
        assert_eq!(v.len(), 1);
        assert!(isa.bitsets[v[0]].size.is_some(), "field type {} is not a root", name);
        v[0]
    };
    let enum_names: Vec<String> = isa.enums.keys().cloned().collect();
    let expr_names: Vec<String> = isa.exprs.keys().cloned().collect();

    let mut out = String::new();
    writeln!(out, "// Generated by build.rs from Mesa's isa/ir3*.xml (MIT). Do not edit.").unwrap();
    writeln!(out, "pub static EXPRS: &[(&str, &str)] = &[").unwrap();
    for name in &expr_names {
        writeln!(out, "    ({}, {}),", quote(name), quote(&isa.exprs[name])).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "pub static ENUMS: &[(&str, &[(u64, &str)])] = &[").unwrap();
    for name in &enum_names {
        write!(out, "    ({}, &[", quote(name)).unwrap();
        for (v, d) in &isa.enums[name] {
            write!(out, "({}, {}), ", v, quote(d)).unwrap();
        }
        writeln!(out, "]),").unwrap();
    }
    writeln!(out, "];").unwrap();
    // Leaves of each root, for bitset-typed fields and for the instruction root.
    writeln!(out, "pub static ROOT_LEAVES: &[(u16, &[u16])] = &[").unwrap();
    for &r in &roots {
        let ls: Vec<String> = leaves.iter().filter(|&&l| root[l] == r).map(|l| l.to_string()).collect();
        writeln!(out, "    ({}, &[{}]),", r, ls.join(", ")).unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "pub static BITSETS: &[Bitset] = &[").unwrap();
    for i in 0..n {
        let b = &isa.bitsets[i];
        let (m, d, k, f) = pat[i];
        writeln!(
            out,
            "    Bitset {{ name: {}, display_name: {}, size: {}, parent: {}, root: {}, gen_min: {}, gen_max: {}, match_: {:#x}, dontcare: {:#x}, mask: {:#x}, field_mask: {:#x}, leaf: {}, cases: &[",
            quote(&b.name),
            quote(&b.display_name),
            size[i],
            match parent_of(b) {
                Some(p) => format!("Some({})", p),
                None => "None".into(),
            },
            root[i],
            gmin[i],
            gmax[i],
            m,
            d,
            k,
            f,
            leaves.contains(&i)
        )
        .unwrap();
        for c in &b.cases {
            let expr = match &c.expr {
                Some(e) => format!("Some({})", expr_names.iter().position(|x| x == e).unwrap_or_else(|| panic!("no expr {}", e))),
                None => "None".into(),
            };
            let display = match &c.display {
                Some(d) => format!("Some({})", quote(d)),
                None => "None".into(),
            };
            writeln!(out, "        Case {{ expr: {}, display: {}, fields: &[", expr, display).unwrap();
            for fl in &c.fields {
                let ty = match fl.ty.as_str() {
                    "branch" => "Ty::Branch".to_string(),
                    "absbranch" => "Ty::AbsBranch".into(),
                    "int" => "Ty::Int".into(),
                    "uint" => "Ty::Uint".into(),
                    "hex" => "Ty::Hex".into(),
                    "offset" => "Ty::Offset".into(),
                    "uoffset" => "Ty::UOffset".into(),
                    "float" => "Ty::Float".into(),
                    "bool" => "Ty::Bool".into(),
                    "bool_inv" => "Ty::BoolInv".into(),
                    "assert" => "Ty::Assert".into(),
                    t if isa.enums.contains_key(t) => format!("Ty::Enum({})", enum_names.iter().position(|x| x == t).unwrap()),
                    t => format!("Ty::Bitset({})", root_index(t)),
                };
                let fexpr = match &fl.expr {
                    Some(e) => format!("Some({})", expr_names.iter().position(|x| x == e).unwrap_or_else(|| panic!("no expr {}", e))),
                    None => "None".into(),
                };
                let params: Vec<String> = fl.params.iter().map(|(a, b)| format!("({}, {})", quote(a), quote(b))).collect();
                writeln!(
                    out,
                    "            Field {{ name: {}, low: {}, high: {}, ty: {}, display: {}, expr: {}, params: &[{}], call: {}, assert_val: {} }},",
                    quote(&fl.name),
                    fl.low,
                    fl.high,
                    ty,
                    match &fl.display {
                        Some(d) => format!("Some({})", quote(d)),
                        None => "None".into(),
                    },
                    fexpr,
                    params.join(", "),
                    fl.call,
                    fl.assert_val.unwrap_or(0)
                )
                .unwrap();
            }
            writeln!(out, "        ] }},").unwrap();
        }
        writeln!(out, "    ] }},").unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "pub const INSTRUCTION: u16 = {};", root_index("#instruction")).unwrap();
    let path = Path::new(&std::env::var("OUT_DIR").unwrap()).join("isa_tables.rs");
    std::fs::write(path, out).unwrap();
}
