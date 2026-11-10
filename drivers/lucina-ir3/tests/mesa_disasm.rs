//! Mesa's own disassembler tests (`src/freedreno/ir3/tests/disasm.c`, Mesa 25.2.6, vendored as `tests/data/mesa-disasm.c`):
//! every a6xx word must print exactly as Mesa prints it (`gpu_id` 600, as the test's `dev_info->chip * 100`).

use lucina_ir3::isa::Decoder;

/// The `INSTR_6XX(hi_lo, "text")` lines that are not commented out.
fn vectors() -> Vec<(u64, String, usize)> {
    let src = include_str!("data/mesa-disasm.c");
    let mut out = Vec::new();
    let mut in_comment = false;
    for (n, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if in_comment {
            if t.contains("*/") {
                in_comment = false;
            }
            continue;
        }
        if t.starts_with("/*") && !t.contains("*/") {
            in_comment = true;
            continue;
        }
        let Some(rest) = t.strip_prefix("INSTR_6XX(") else { continue };
        let Some((hex, rest)) = rest.split_once(',') else { continue };
        let Some((hi, lo)) = hex.trim().split_once('_') else { continue };
        let word = (u64::from_str_radix(hi, 16).unwrap() << 32) | u64::from_str_radix(lo, 16).unwrap();
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let mut text = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => text.push(chars.next().unwrap()),
                '"' => break,
                c => text.push(c),
            }
        }
        out.push((word, text, n + 1));
    }
    out
}

#[test]
fn every_a6xx_vector_prints_as_mesa_prints_it() {
    let d = Decoder::new(600);
    let v = vectors();
    assert!(v.len() > 150, "only {} vectors", v.len());
    let mut wrong = Vec::new();
    for (word, expected, line) in &v {
        match d.disasm(*word) {
            Ok(got) if got.trim() == expected.as_str() => {}
            Ok(got) => wrong.push(format!("line {}: {:016x}: expected {:?}, got {:?}", line, word, expected, got.trim())),
            Err(e) => wrong.push(format!("line {}: {:016x}: expected {:?}, error {}", line, word, expected, e)),
        }
    }
    for w in &wrong {
        println!("{}", w);
    }
    assert!(wrong.is_empty(), "{} of {} vectors differ", wrong.len(), v.len());
}

#[test]
fn every_a6xx_vector_reencodes_to_its_bits() {
    let d = Decoder::new(600);
    let mut wrong = Vec::new();
    let mut n = 0;
    for (word, expected, line) in vectors() {
        if expected.starts_with("raw ") {
            continue;
        }
        n += 1;
        let instr = d.fields(lucina_ir3::isa::INSTRUCTION, word).unwrap();
        match lucina_ir3::isa::encode(&instr) {
            // Don't-care bits set in a vector come back clear.
            Ok(back) => {
                let dc = lucina_ir3::isa::BITSETS[instr.bitset as usize].dontcare;
                if back != word & !dc {
                    wrong.push(format!("line {}: {:016x} -> {:016x} ({})", line, word, back, expected));
                }
            }
            Err(e) => wrong.push(format!("line {}: {:016x}: {} ({})", line, word, e, expected)),
        }
    }
    for w in &wrong {
        println!("{}", w);
    }
    assert!(wrong.is_empty(), "{} of {} vectors do not re-encode", wrong.len(), n);
}
