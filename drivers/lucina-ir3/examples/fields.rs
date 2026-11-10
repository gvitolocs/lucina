fn main() {
    let w = u64::from_str_radix(&std::env::args().nth(1).unwrap(), 16).unwrap();
    let d = lucina_ir3::isa::Decoder::new(600);
    println!("{:?}", d.disasm(w));
    let i = d.fields(lucina_ir3::isa::INSTRUCTION, w).unwrap();
    fn show(i: &lucina_ir3::isa::Instr, depth: usize) {
        println!("{}{}", "  ".repeat(depth), lucina_ir3::isa::BITSETS[i.bitset as usize].name);
        for (k, p) in &i.fields {
            match &p.value {
                lucina_ir3::isa::Value::Bits(b) => println!("{}  {} = {:#x}", "  ".repeat(depth), k, b),
                lucina_ir3::isa::Value::Nested(n) => { println!("{}  {} =", "  ".repeat(depth), k); show(n, depth + 2) }
            }
        }
    }
    show(&i, 0);
    println!("{:?}", lucina_ir3::isa::encode(&i).map(|x| format!("{:016x}", x)));
}
