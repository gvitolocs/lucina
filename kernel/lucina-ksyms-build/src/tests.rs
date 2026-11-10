use super::*;

mod built {
    use super::*;

    fn sym(start: u64, size: u64, name: &str) -> Symbol {
        Symbol {
            start,
            size,
            name: name.into(),
        }
    }

    /// A placeholder-sized buffer holding the table of `functions`.
    fn blob(functions: &[Symbol], capacity: usize) -> Vec<u8> {
        let (table, _) = table(functions, capacity).unwrap();
        let mut blob = vec![0u8; capacity];
        blob[..table.len()].copy_from_slice(&table);
        blob
    }

    /// Each address finds its function and how far into it, and nothing outside one.
    #[test]
    fn lookup_finds_the_function_around_an_address() {
        let base = 0xffff_ffff_a000_0000;
        let b = blob(
            &[sym(base + 0x100, 0x40, "_ZN4ostd5panic5begin17h0123456789abcdefE"), sym(base, 0x10, "first"), sym(base + 0x200, 0, "asm_label")],
            4096,
        );
        let t = Table::parse(&b).unwrap();
        assert_eq!(t.len(), 3);
        assert_eq!(t.lookup(base), Some(("first", 0)));
        assert_eq!(t.lookup(base + 0xf), Some(("first", 0xf)));
        assert_eq!(t.lookup(base + 0x10), None, "past the end of `first`, before the next");
        assert_eq!(t.lookup(base + 0x13c), Some(("ostd::panic::begin", 0x3c)));
        assert_eq!(t.lookup(base + 0x140), None);
        assert_eq!(t.lookup(base + 0x5000), Some(("asm_label", 0x4e00)), "a size of 0 reaches on");
        assert_eq!(t.lookup(base - 1), None);
    }

    /// An empty, a cut or a corrupt placeholder gives no table or no name, never a panic.
    #[test]
    fn a_bad_table_gives_nothing() {
        let empty = {
            let mut b = vec![0u8; 4096];
            b[..16].copy_from_slice(&MAGIC);
            b[16..20].copy_from_slice(&4096u32.to_le_bytes());
            b
        };
        assert!(Table::parse(&empty).is_none(), "not filled");
        let good = blob(&[sym(0x1000, 8, "a"), sym(0x2000, 8, "b")], 4096);
        assert!(Table::parse(&good[..100]).is_none(), "shorter than its capacity");
        let mut count = good.clone();
        count[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Table::parse(&count).is_none(), "more entries than room");
        let mut name = good.clone();
        name[HEADER + 8..HEADER + 12].copy_from_slice(&((0xff_ffffu32 << 8) | 9).to_le_bytes());
        let t = Table::parse(&name).unwrap();
        assert_eq!(t.lookup(0x1000), None, "a name outside the table");
        assert_eq!(t.lookup(0x2000), Some(("b", 0)));
        for cut in 0..60 {
            let _ = Table::parse(&good[..cut]).map(|t| t.lookup(0x1000));
        }
    }

    /// Rust names in short: no hash, no generic arguments, qualified paths kept.
    #[test]
    fn short_names() {
        assert_eq!(short_name("_ZN4ostd5panic20__ostd_panic_handler17h3b2a1c0d9e8f7a6bE"), "ostd::panic::__ostd_panic_handler");
        assert_eq!(
            short_name("_ZN63_$LT$aster_kernel..fs..Foo$u20$as$u20$core..ops..drop..Drop$GT$4drop17h0011223344556677E"),
            "<aster_kernel::fs::Foo as core::ops::drop::Drop>::drop"
        );
        assert_eq!(short_name("__trap_entry"), "__trap_entry");
        assert_eq!(without_generics("spin::once::Once<core::option::Option<ostd::io::IoMem>>::try_call_once_slow::<F>"), "spin::once::Once::try_call_once_slow");
        assert_eq!(
            without_generics("<alloc::vec::Vec<u8> as core::ops::drop::Drop>::drop"),
            "<alloc::vec::Vec as core::ops::drop::Drop>::drop"
        );
        assert_eq!(without_generics("<&mut T as core::fmt::Write>::write_str"), "<&mut T as core::fmt::Write>::write_str");
        assert_eq!(without_generics("a::b<c<d>, e>::f"), "a::b::f");
        assert_eq!(without_generics("broken<"), "broken");
        assert_eq!(without_generics("x>y"), "x>y");
        // A v0 name of this kernel.
        let v0 = "_RINvMs2_NtCs8hy6Un7g2OK_4spin4onceINtB6_4OnceFEuE18try_call_once_slowNCINvB2_9call_onceNCNvNtCs9kU8mVgy2BF_4ostd5crash16set_pet_watchdog0E0E";
        let short = short_name(v0);
        assert!(short.starts_with("<spin::once::Once>::try_call_once_slow") || short.starts_with("spin::once::Once::try_call_once_slow"), "{short}");
    }

    /// Aliases at one address keep the one with a size, then the shortest name; long names are cut to fit.
    #[test]
    fn aliases_and_long_names() {
        let long = "x".repeat(300);
        let b = blob(&[sym(0x1000, 0, "zz_alias"), sym(0x1000, 8, "longer_name"), sym(0x1000, 8, "short"), sym(0x2000, 8, &long)], 4096);
        let t = Table::parse(&b).unwrap();
        assert_eq!(t.len(), 2);
        assert_eq!(t.lookup(0x1000), Some(("short", 0)));
        assert_eq!(t.lookup(0x2000).unwrap().0.len(), 96);
        let qualified = "<aster_core::fs::fs_impls::procfs::crash_test::CrashTestFileOps as aster_core::fs::file::file_handle::FileOps>::write";
        let b = blob(&[sym(0x3000, 8, qualified)], 4096);
        let name = Table::parse(&b).unwrap().lookup(0x3000).unwrap().0;
        assert!(name.len() <= 96 && name.starts_with("<aster_core::fs::fs_impls::") && name.ends_with("FileOps>::write") && name.contains("..."), "{name}");
        // So many that only shorter names fit.
        let many: Vec<_> = (0..100).map(|i| sym(0x1000 + i * 16, 16, &std::format!("{i:03}{}", "n".repeat(90)))).collect();
        let (small, limit) = table(&many, HEADER + 100 * ENTRY + 100 * 48).unwrap();
        assert_eq!(limit, 48);
        assert!(small.len() <= HEADER + 100 * ENTRY + 100 * 48);
        assert!(table(&many, HEADER + 100 * ENTRY).is_err());
    }

    /// A minimal ELF64: a data section holding a placeholder named LUCINA_KSYMS, and some functions.
    fn elf(capacity: usize, functions: &[(u64, u64, &str)]) -> Vec<u8> {
        let data_addr = 0xffff_ffff_a100_0000u64;
        let mut strtab = vec![0u8];
        let mut name_of = |s: &str| {
            let at = strtab.len() as u32;
            strtab.extend_from_slice(s.as_bytes());
            strtab.push(0);
            at
        };
        let mut syms = vec![[0u8; 24]];
        let mut push = |name: u32, kind: u8, section: u16, value: u64, size: u64| {
            let mut s = [0u8; 24];
            s[..4].copy_from_slice(&name.to_le_bytes());
            s[4] = 0x10 | kind;
            s[6..8].copy_from_slice(&section.to_le_bytes());
            s[8..16].copy_from_slice(&value.to_le_bytes());
            s[16..24].copy_from_slice(&size.to_le_bytes());
            syms.push(s);
        };
        let n = name_of("LUCINA_KSYMS");
        push(n, 1, 1, data_addr + 64, capacity as u64);
        for &(start, size, name) in functions {
            let n = name_of(name);
            push(n, 2, 2, start, size);
        }
        // Layout: header, data section, symtab, strtab, section headers.
        let data_off = 64usize;
        let data_size = 64 + capacity;
        let sym_off = data_off + data_size;
        let sym_size = syms.len() * 24;
        let str_off = sym_off + sym_size;
        let sh_off = str_off + strtab.len();
        let mut out = vec![0u8; sh_off + 5 * 64];
        out[..5].copy_from_slice(b"\x7fELF\x02");
        out[5] = 1;
        out[0x28..0x30].copy_from_slice(&(sh_off as u64).to_le_bytes());
        out[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        out[0x3c..0x3e].copy_from_slice(&5u16.to_le_bytes());
        let p = data_off + 64;
        out[p..p + 16].copy_from_slice(&MAGIC);
        out[p + 16..p + 20].copy_from_slice(&(capacity as u32).to_le_bytes());
        for (i, s) in syms.iter().enumerate() {
            out[sym_off + i * 24..sym_off + i * 24 + 24].copy_from_slice(s);
        }
        out[str_off..str_off + strtab.len()].copy_from_slice(&strtab);
        let mut section = |i: usize, kind: u32, addr: u64, offset: usize, size: usize, link: u32| {
            let at = sh_off + i * 64;
            out[at + 4..at + 8].copy_from_slice(&kind.to_le_bytes());
            out[at + 0x10..at + 0x18].copy_from_slice(&addr.to_le_bytes());
            out[at + 0x18..at + 0x20].copy_from_slice(&(offset as u64).to_le_bytes());
            out[at + 0x20..at + 0x28].copy_from_slice(&(size as u64).to_le_bytes());
            out[at + 0x28..at + 0x2c].copy_from_slice(&link.to_le_bytes());
        };
        section(1, 1, data_addr, data_off, data_size, 0);
        section(2, 1, 0xffff_ffff_a000_0000, 0, 0, 0);
        section(3, 2, 0, sym_off, sym_size, 4);
        section(4, 3, 0, str_off, strtab.len(), 0);
        out
    }

    /// The builder finds the placeholder by its symbol, fills it, and the table reads back from the file.
    #[test]
    fn fill_writes_the_table_into_the_placeholder() {
        let mut file = elf(8192, &[(0xffff_ffff_a000_1000, 0x20, "_ZN3foo3bar17h0011223344556677E"), (0xffff_ffff_a000_2000, 0x10, "baz")]);
        let (at, capacity) = find_placeholder(&file).unwrap().unwrap();
        assert_eq!((at, capacity), (128, 8192));
        let filled = fill(&mut file).unwrap().unwrap();
        assert_eq!(filled.functions, 2);
        assert!(filled.bytes <= 8192);
        let t = Table::parse(&file[at..at + capacity]).unwrap();
        assert_eq!(t.lookup(0xffff_ffff_a000_1004), Some(("foo::bar", 4)));
        assert_eq!(t.lookup(0xffff_ffff_a000_2000), Some(("baz", 0)));
        // A table starts with the magic too: filling again writes the same table.
        let once = file.clone();
        assert_eq!(fill(&mut file).unwrap(), Some(filled));
        assert_eq!(file, once);
    }

    /// A kernel without the placeholder is left as it is.
    #[test]
    fn no_placeholder_no_change() {
        let mut file = elf(64, &[(0x1000, 8, "f")]);
        // Rename the symbol: no LUCINA_KSYMS any more.
        let pos = file.windows(12).position(|w| w == b"LUCINA_KSYMS").unwrap();
        file[pos] = b'X';
        let before = file.clone();
        assert_eq!(fill(&mut file).unwrap(), None);
        assert_eq!(file, before);
    }
}
