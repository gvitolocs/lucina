//! Names the addresses of a crash report: reads text on stdin and repeats each line, followed by a line for each kernel
//! text address in it (16 hex digits) with the function it falls in. scripts/juno-symbolize.sh runs this.
//!
//! usage: symbolize <kernel ELF> < report.txt

use std::io::BufRead;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: symbolize <kernel ELF> < report.txt");
    let elf = std::fs::read(&path).expect("read the kernel");
    let symbols = read_symbols(&elf).expect("an ELF64 file with a symbol table");
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("read stdin");
        println!("{line}");
        for token in line.split(|c: char| !c.is_ascii_hexdigit()) {
            if token.len() != 16 {
                continue;
            }
            let Ok(address) = u64::from_str_radix(token, 16) else {
                continue;
            };
            // A return address follows its call, which may be the last instruction of the function.
            if let Some((name, offset)) = lookup(&symbols, address.wrapping_sub(4)) {
                println!("    {address:016x} {}+{offset:#x}", demangle(name));
            }
        }
    }
}

struct Symbol<'a> {
    start: u64,
    size: u64,
    name: &'a str,
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// The function symbols of an ELF64 file, sorted by address.
fn read_symbols(elf: &[u8]) -> Option<Vec<Symbol<'_>>> {
    const SHT_SYMTAB: u32 = 2;
    const STT_FUNC: u8 = 2;
    if elf.get(..5)? != b"\x7fELF\x02" {
        return None;
    }
    let shoff = u64_at(elf, 0x28)? as usize;
    let shentsize = u16_at(elf, 0x3a)? as usize;
    let shnum = u16_at(elf, 0x3c)? as usize;
    let section = |i: usize| shoff + i * shentsize;
    let symtab = (0..shnum).find(|&i| u32_at(elf, section(i) + 4) == Some(SHT_SYMTAB))?;
    let (sym_off, sym_size) = (
        u64_at(elf, section(symtab) + 0x18)? as usize,
        u64_at(elf, section(symtab) + 0x20)? as usize,
    );
    let strtab = section(u32_at(elf, section(symtab) + 0x28)? as usize);
    let str_off = u64_at(elf, strtab + 0x18)? as usize;

    let mut symbols = Vec::new();
    for at in (sym_off..sym_off + sym_size).step_by(24) {
        if elf.get(at + 4)? & 0xf != STT_FUNC {
            continue;
        }
        let name_at = str_off + u32_at(elf, at)? as usize;
        let name = elf.get(name_at..)?;
        let end = name.iter().position(|&b| b == 0)?;
        symbols.push(Symbol {
            start: u64_at(elf, at + 8)?,
            size: u64_at(elf, at + 16)?,
            name: std::str::from_utf8(&name[..end]).ok()?,
        });
    }
    symbols.sort_by_key(|s| s.start);
    Some(symbols)
}

fn lookup<'a>(symbols: &[Symbol<'a>], address: u64) -> Option<(&'a str, u64)> {
    let i = symbols.partition_point(|s| s.start <= address);
    let s = symbols.get(i.checked_sub(1)?)?;
    (address - s.start < s.size.max(1)).then_some((s.name, address - s.start))
}

/// Rust's mangled names in short: the identifiers in order (`_RNv...` v0 and `_ZN...` legacy names are both length-prefixed),
/// which reads well enough to find the function. Anything else stays as it is.
fn demangle(name: &str) -> String {
    let Some(rest) = name.strip_prefix("_R").or_else(|| name.strip_prefix("_ZN")) else {
        return name.into();
    };
    let bytes = rest.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // A disambiguator or a back reference: a tag, base-62 digits, `_`. Its digits are no length.
        if matches!(bytes[i], b's' | b'B') {
            let end = bytes[i + 1..]
                .iter()
                .position(|b| !b.is_ascii_alphanumeric());
            if let Some(end) = end.filter(|&end| bytes[i + 1 + end] == b'_') {
                i += end + 2;
                continue;
            }
        }
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let digits = bytes[i..].iter().take_while(|b| b.is_ascii_digit()).count();
        let len: usize = rest[i..i + digits].parse().unwrap_or(usize::MAX);
        let start = i + digits + usize::from(bytes.get(i + digits) == Some(&b'_'));
        match start.checked_add(len).and_then(|end| rest.get(start..end)) {
            Some(part) => {
                // The hash of a legacy name says nothing.
                if !(part.len() == 17 && part.starts_with('h')) {
                    parts.push(part);
                }
                i = start + len;
            }
            None => i += digits,
        }
    }
    if parts.is_empty() {
        return name.into();
    }
    parts.join("::")
}
