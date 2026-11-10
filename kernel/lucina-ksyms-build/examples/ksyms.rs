//! The builder by hand, for a kernel ELF.
//!
//! usage: ksyms fill <kernel ELF>        writes the table into its placeholder (what OSDK does after linking)
//!        ksyms size <kernel ELF> [capacity]  how large the table of its functions is
//!        ksyms name <kernel ELF> <hex address>...  names addresses from the filled placeholder

use lucina_ksyms_build::{self as build, CAPACITY, Table};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: ksyms fill|size|name <kernel ELF> ...";
    let (command, path) = (args.get(1).expect(usage), args.get(2).expect(usage));
    match command.as_str() {
        "fill" => match build::fill_file(path.as_ref()) {
            Ok(Some(f)) => println!(
                "ksyms: {} functions, {} of {} bytes, names cut at {}",
                f.functions, f.bytes, f.capacity, f.name_limit
            ),
            Ok(None) => println!("ksyms: no LUCINA_KSYMS placeholder"),
            Err(e) => {
                eprintln!("ksyms: {e}");
                std::process::exit(1);
            }
        },
        "size" => {
            let elf = std::fs::read(path).expect("read the ELF");
            let functions = build::functions(&elf).expect("an ELF64 with a symbol table");
            let capacity = args.get(3).map_or(CAPACITY, |c| c.parse().expect("a number"));
            match build::table(&functions, capacity) {
                Ok((table, limit)) => println!(
                    "{} symbols, table {} bytes of {capacity}, names cut at {limit}",
                    functions.len(),
                    table.len()
                ),
                Err(e) => println!("{} symbols: {e}", functions.len()),
            }
        }
        "name" => {
            let elf = std::fs::read(path).expect("read the ELF");
            let (at, capacity) = build::find_placeholder(&elf)
                .expect("a placeholder")
                .expect("a placeholder");
            let table = Table::parse(&elf[at..at + capacity]).expect("a filled placeholder");
            for a in &args[3..] {
                let address = u64::from_str_radix(a.trim_start_matches("0x"), 16).expect("hex");
                match table.lookup(address.wrapping_sub(1)) {
                    Some((name, offset)) => println!("{address:016x} {name}+{:#x}", offset + 1),
                    None => println!("{address:016x} ?"),
                }
            }
        }
        _ => panic!("{usage}"),
    }
}
