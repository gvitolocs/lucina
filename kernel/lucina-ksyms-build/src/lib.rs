// SPDX-License-Identifier: MPL-2.0

//! The builder of `lucina-ksyms`'s table: reads the function symbols of the ELF that was just linked, shortens their names, and writes the table
//! into its `LUCINA_KSYMS` placeholder. OSDK runs [`fill_file`] before it makes the `Image`.
//!
//! The ELF reading follows `kernel/lucina-crash/examples/symbolize.rs`, which scripts/juno-symbolize.sh uses: the
//! symbol table, its function symbols, nothing else of the ELF.

use std::{collections::BTreeMap, path::Path, string::String, vec::Vec};

pub use lucina_ksyms::{CAPACITY, ENTRY, FILLED, HEADER, MAGIC, Table};

/// A function of the ELF.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    pub start: u64,
    pub size: u64,
    /// As the ELF has it (mangled).
    pub name: String,
}

/// What [`fill`] wrote.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Filled {
    pub functions: usize,
    pub bytes: usize,
    pub capacity: usize,
    /// Names were cut to this many bytes to fit.
    pub name_limit: usize,
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

/// An ELF64 symbol: name, type, section, value, size.
struct ElfSymbol<'a> {
    name: &'a str,
    kind: u8,
    section: u16,
    value: u64,
    size: u64,
}

/// The section headers' file offset and size, and the symbols.
struct Elf<'a> {
    elf: &'a [u8],
    shoff: usize,
    shentsize: usize,
    symbols: Vec<ElfSymbol<'a>>,
}

const SHT_SYMTAB: u32 = 2;
const SHF_EXECINSTR: u64 = 0x4;
const STT_NOTYPE: u8 = 0;
const STT_OBJECT: u8 = 1;
const STT_FUNC: u8 = 2;

fn read_elf(elf: &[u8]) -> Option<Elf<'_>> {
    if elf.get(..5)? != b"\x7fELF\x02" || elf.get(5)? != &1 {
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
        let name_at = str_off + u32_at(elf, at)? as usize;
        let name = elf.get(name_at..)?;
        let end = name.iter().position(|&b| b == 0)?;
        symbols.push(ElfSymbol {
            name: std::str::from_utf8(&name[..end]).unwrap_or(""),
            kind: elf.get(at + 4)? & 0xf,
            section: u16_at(elf, at + 6)?,
            value: u64_at(elf, at + 8)?,
            size: u64_at(elf, at + 16)?,
        });
    }
    Some(Elf {
        elf,
        shoff,
        shentsize,
        symbols,
    })
}

impl Elf<'_> {
    /// Where in the file the bytes at `address` of section `index` are.
    fn file_offset(&self, index: u16, address: u64) -> Option<usize> {
        let at = self.shoff + index as usize * self.shentsize;
        let (addr, offset, size) = (
            u64_at(self.elf, at + 0x10)?,
            u64_at(self.elf, at + 0x18)?,
            u64_at(self.elf, at + 0x20)?,
        );
        let into = address.checked_sub(addr)?;
        (into < size).then_some((offset + into) as usize)
    }

    /// Whether section `index` holds code.
    fn executable(&self, index: u16) -> bool {
        let at = self.shoff + index as usize * self.shentsize;
        index != 0 && u64_at(self.elf, at + 8).is_some_and(|flags| flags & SHF_EXECINSTR != 0)
    }
}

/// The function symbols, by address, and the labels in code (the assembly entry points such as `trap_common`, which have
/// no function type; their size is 0). Not the mapping symbols (`$x`, `$d`) or local labels (`.L…`).
pub fn functions(elf: &[u8]) -> Option<Vec<Symbol>> {
    let elf = read_elf(elf)?;
    let mut functions: Vec<Symbol> = elf
        .symbols
        .iter()
        .filter(|s| s.value != 0 && !s.name.is_empty() && !s.name.starts_with('$') && !s.name.starts_with(".L"))
        .filter(|s| s.kind == STT_FUNC || (s.kind == STT_NOTYPE && elf.executable(s.section)))
        .map(|s| Symbol {
            start: s.value,
            size: s.size,
            name: s.name.into(),
        })
        .collect();
    // Labels only among the functions: the early boot code runs at its physical address, far below them.
    let typed = || functions.iter().filter(|f| f.size != 0);
    if let (Some(low), Some(high)) = (typed().map(|f| f.start).min(), typed().map(|f| f.start + f.size).max()) {
        functions.retain(|f| f.size != 0 || (low..high).contains(&f.start));
    }
    functions.sort_by_key(|s| s.start);
    Some(functions)
}

/// Where the placeholder is in the file, and its capacity: the `LUCINA_KSYMS` object, which must start with [`MAGIC`].
/// `Ok(None)`: this kernel has none (an architecture without the crash screen's names).
pub fn find_placeholder(elf: &[u8]) -> Result<Option<(usize, usize)>, String> {
    let parsed = read_elf(elf).ok_or("not an ELF64 little-endian file with a symbol table")?;
    let Some(symbol) = parsed
        .symbols
        .iter()
        .find(|s| s.kind == STT_OBJECT && s.name == "LUCINA_KSYMS")
    else {
        return Ok(None);
    };
    let at = parsed
        .file_offset(symbol.section, symbol.value)
        .ok_or("LUCINA_KSYMS is not inside its section")?;
    if elf.get(at..at + 16) != Some(&MAGIC[..]) {
        return Err("LUCINA_KSYMS does not start with the magic (another layout)".into());
    }
    let capacity = u32_at(elf, at + 16).ok_or("LUCINA_KSYMS is cut short")? as usize;
    if capacity as u64 != symbol.size || at + capacity > elf.len() {
        return Err(std::format!(
            "LUCINA_KSYMS says {capacity} bytes, the symbol {} and the file has {} after it",
            symbol.size,
            elf.len() - at
        ));
    }
    Ok(Some((at, capacity)))
}

/// Drops generic arguments: a `<...>` right after a name (`Vec<T>`) and a turbofish (`::<T>`). A `<` that opens a
/// qualified path (`<T as Trait>::f`, at the start or after a space, `(`, `&`, `*`, `[`) stays, with its insides
/// shortened the same way.
fn without_generics(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    // For each `<` open: whether it is kept.
    let mut open: Vec<bool> = Vec::new();
    for c in name.chars() {
        let dropping = open.iter().any(|kept| !kept);
        match c {
            '<' => {
                let kept = !dropping
                    && match out.chars().last() {
                        None => true,
                        Some(p) => matches!(p, ' ' | '(' | '&' | '*' | '[' | ','),
                    };
                if !dropping && !kept && out.ends_with("::") {
                    out.truncate(out.len() - 2);
                }
                open.push(kept);
                if kept {
                    out.push(c);
                }
            }
            '>' if !open.is_empty() => {
                if open.pop() == Some(true) && !dropping {
                    out.push(c);
                }
            }
            _ if dropping => {}
            _ => out.push(c),
        }
    }
    out
}

/// A Rust name in short, as a crash screen line has room for: demangled (legacy or v0) without hashes, crate
/// disambiguators or generic arguments, e.g. `<alloc::vec::Vec as core::ops::drop::Drop>::drop` or
/// `spin::once::Once::try_call_once_slow`. Anything that is not a Rust name stays as it is.
pub fn short_name(name: &str) -> String {
    match rustc_demangle::try_demangle(name) {
        Ok(demangled) => without_generics(&std::format!("{demangled:#}")),
        Err(_) => name.into(),
    }
}

/// `name` in at most `limit` bytes: whole if it fits, else its start, `...` and its end, at character boundaries, so that a
/// long qualified path (`<A as B>::method`) keeps the method.
fn cut(name: &str, limit: usize) -> std::borrow::Cow<'_, str> {
    if name.len() <= limit {
        return name.into();
    }
    let tail = limit / 3;
    let mut head = limit.saturating_sub(tail + 3);
    while !name.is_char_boundary(head) {
        head -= 1;
    }
    let mut from = name.len() - tail;
    while !name.is_char_boundary(from) {
        from += 1;
    }
    std::format!("{}...{}", &name[..head], &name[from..]).into()
}

/// The table of `functions` with names cut at `limit`, if it fits `capacity`.
fn table_with(functions: &[(u64, u64, String)], capacity: usize, limit: usize) -> Option<Vec<u8>> {
    let base = functions.first().map_or(0, |f| f.0);
    let mut names: Vec<u8> = Vec::new();
    let mut seen: BTreeMap<String, u32> = BTreeMap::new();
    let mut entries: Vec<u8> = Vec::with_capacity(functions.len() * ENTRY);
    for (start, size, name) in functions {
        let name = cut(name, limit.min(255));
        let offset = match seen.get(name.as_ref()) {
            Some(&offset) => offset,
            None => {
                let offset = u32::try_from(names.len()).ok().filter(|&o| o < 1 << 24)?;
                names.extend_from_slice(name.as_bytes());
                seen.insert(name.clone().into_owned(), offset);
                offset
            }
        };
        let relative = u32::try_from(start - base).ok()?;
        entries.extend_from_slice(&relative.to_le_bytes());
        entries.extend_from_slice(&u32::try_from(*size).unwrap_or(u32::MAX).to_le_bytes());
        entries.extend_from_slice(&((offset << 8) | name.len() as u32).to_le_bytes());
    }
    let names_at = HEADER + entries.len();
    let total = names_at + names.len();
    if total > capacity {
        return None;
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&(capacity as u32).to_le_bytes());
    out.extend_from_slice(&FILLED.to_le_bytes());
    out.extend_from_slice(&(functions.len() as u32).to_le_bytes());
    out.extend_from_slice(&(names_at as u32).to_le_bytes());
    out.extend_from_slice(&base.to_le_bytes());
    out.extend_from_slice(&entries);
    out.extend_from_slice(&names);
    Some(out)
}

/// The table of `functions` (by address), for a placeholder of `capacity` bytes. One function per address: of several
/// at the same address (aliases), the one with a size, then the shortest name. Names are cut at 96 bytes, or shorter
/// until the table fits.
pub fn table(functions: &[Symbol], capacity: usize) -> Result<(Vec<u8>, usize), String> {
    let mut by_address: BTreeMap<u64, (u64, String)> = BTreeMap::new();
    for f in functions {
        let name = short_name(&f.name);
        let better = |(size, kept): &(u64, String)| (f.size != 0 && *size == 0) || (f.size != 0) == (*size != 0) && name.len() < kept.len();
        if by_address.get(&f.start).is_none_or(better) {
            by_address.insert(f.start, (f.size, name));
        }
    }
    let functions: Vec<(u64, u64, String)> = by_address.into_iter().map(|(start, (size, name))| (start, size, name)).collect();
    if let (Some(first), Some(last)) = (functions.first(), functions.last())
        && last.0 - first.0 > u64::from(u32::MAX)
    {
        return Err("the functions span more than 4 GiB".into());
    }
    for limit in [96, 64, 48, 32, 24] {
        if let Some(table) = table_with(&functions, capacity, limit) {
            return Ok((table, limit));
        }
    }
    Err(std::format!(
        "{} functions do not fit {capacity} bytes even with names of 24 bytes",
        functions.len()
    ))
}

/// Writes the table of `elf`'s functions into its placeholder. `Ok(None)`: it has no placeholder.
pub fn fill(elf: &mut [u8]) -> Result<Option<Filled>, String> {
    let Some((at, capacity)) = find_placeholder(elf)? else {
        return Ok(None);
    };
    let functions = functions(elf).ok_or("no function symbols")?;
    let (table, name_limit) = table(&functions, capacity)?;
    elf[at..at + table.len()].copy_from_slice(&table);
    Ok(Some(Filled {
        functions: u32_at(&table, 24).unwrap_or(0) as usize,
        bytes: table.len(),
        capacity,
        name_limit,
    }))
}

/// [`fill`] on a file, in place. A file whose table is already right is not written, so that its time stays and a build
/// that did not relink reuses what it made of it.
pub fn fill_file(path: &Path) -> Result<Option<Filled>, String> {
    let before = std::fs::read(path).map_err(|e| std::format!("read {}: {e}", path.display()))?;
    let mut elf = before.clone();
    let filled = fill(&mut elf)?;
    if filled.is_some() && elf != before {
        std::fs::write(path, &elf).map_err(|e| std::format!("write {}: {e}", path.display()))?;
    }
    Ok(filled)
}

const _: () = assert!(CAPACITY < u32::MAX as usize);

#[cfg(test)]
mod tests;
