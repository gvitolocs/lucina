//! The builder against Mesa's own vectors: each word and text is a line of `tests/data/mesa-disasm.c`, except where noted.

use lucina_ir3::{
    asm::{self, Flags, Operand, Reg, Src, Type},
    isa::Decoder,
};

const fn r(g: u16, c: u16) -> Reg {
    Reg(g * 4 + c)
}
const fn c(n: u16, comp: u16) -> u16 {
    n * 4 + comp
}

#[test]
fn builds_what_mesa_encodes() {
    let no = Flags::default();
    let cases: Vec<(Result<u64, String>, u64, &str)> = vec![
        (asm::end(no), 0x0300000000000000, "end"),
        (asm::nop(2, no), 0x0000020000000000, "(rpt2)nop"),
        (asm::br(false, 4, no), 0x0080000000000004, "br p0.x, #4"),
        (asm::br(true, 3, no), 0x0090000000000003, "br !p0.x, #3"),
        (asm::mov(Type::F32, Type::F32, r(0, 0), Operand::C(c(8, 0)), no), 0x2024400000000020, "mov.f32f32 r0.x, c8.x"),
        (
            asm::cat2("cmps.f", r(1, 0), Src { abs: true, ..Src::r(r(1, 0)) }, Src::c(c(16, 0)), Some(0), Flags { nop: 3, ..no }),
            0x40b8080410408004,
            "(nop3) cmps.f.lt r1.x, (abs)r1.x, c16.x",
        ),
        (
            asm::cat3("mad.f32", r(1, 1), Src { neg: true, ..Src::c(c(12, 0)) }, r(1, 0), false, Src::c(c(12, 1)), no),
            0x6382000510315030,
            "mad.f32 r1.y, (neg)c12.x, r1.x, c12.y",
        ),
        (asm::cat4("rcp", r(2, 2), Src::r(r(0, 3)), no), 0x8010000a00000003, "rcp r2.z, r0.w"),
    ];
    let d = Decoder::new(600);
    for (built, word, text) in cases {
        let got = built.unwrap_or_else(|e| panic!("{}: {}", text, e));
        assert_eq!(d.disasm(got).unwrap().trim(), text, "{:016x}", got);
        assert_eq!(got, word, "{}", text);
    }
}
