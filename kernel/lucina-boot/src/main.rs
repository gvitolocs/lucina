//! `lucina-boot`: build LucinaBoot's inputs on the host.
//!
//! ```text
//! lucina-boot kernel-image <kernel.elf> -o <Image>
//! lucina-boot loader-image <lucinaboot.elf> -o <Image> [--text-offset <hex>]
//! lucina-boot pack -o <payload.lbp> --kernel <Image> [--initramfs <file>] [--dtb <name>=<file>]...
//! lucina-boot info <payload.lbp | Image>
//! lucina-boot panel <cmdline>
//! lucina-boot surya-dtb -o <stub.dtb>
//! lucina-boot surya-reserved
//! lucina-boot check-tree <loader.dtb> <packaged.dtb>
//! lucina-boot check-band <fb.raw> --width <w> --height <h> --stride <bytes> --format <fmt>
//!                        --el <n> --stages <n> [--error <n>]
//! ```
//! Reads and writes regular files only (`lucina_bootimg::files`), so it can never touch a partition.

use std::{path::Path, process::ExitCode};

use lucina_boot::{
    Range,
    build::{LOADER_TEXT_OFFSET, kernel_image, loader_image, pack},
    image::Image,
    panel,
    pkg::Package,
    plan,
};
use lucina_bootimg::files::{read_image, write_image};
use lucina_fdt::Fdt;

const USAGE: &str = "usage:
  lucina-boot kernel-image <kernel.elf> -o <Image>
  lucina-boot loader-image <lucinaboot.elf> -o <Image> [--text-offset <hex>]
  lucina-boot pack -o <payload.lbp> --kernel <Image> [--initramfs <file>] [--dtb <name>=<file>]...
  lucina-boot info <payload.lbp | Image>
  lucina-boot panel <cmdline>
  lucina-boot surya-dtb -o <stub.dtb>
  lucina-boot surya-reserved
  lucina-boot check-tree <loader.dtb> <packaged.dtb>
  lucina-boot check-band <fb.raw> --width <w> --height <h> --stride <bytes> --format <fmt> --el <n> --stages <n> [--error <n>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("kernel-image") => convert(&args[1..], false),
        Some("loader-image") => convert(&args[1..], true),
        Some("pack") => pack_cmd(&args[1..]),
        Some("info") if args.len() == 2 => info(&args[1]),
        Some("panel") if args.len() == 2 => {
            println!("{:?}", panel::from_cmdline(&args[1]));
            Ok(())
        }
        Some("surya-dtb") if args.len() == 3 && args[1] == "-o" => {
            let dtb = lucina_boot::surya::abl_stub_dtb().map_err(|e| e.to_string());
            dtb.and_then(|d| {
                write(&args[2], &d).map(|()| println!("{}: {} bytes", args[2], d.len()))
            })
        }
        // One `lucina-fdt add-reserved --region` value per line: a test tree gets the phone's map from the same table.
        Some("surya-reserved") if args.len() == 1 => {
            for (name, base, size) in lucina_boot::surya::RESERVED {
                println!("{name}@{base:x}+{size:x},no-map");
            }
            Ok(())
        }
        Some("check-tree") if args.len() == 3 => check_tree(&args[1], &args[2]),
        Some("check-band") => check_band(&args[1..]),
        _ => Err(USAGE.into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-boot: {e}");
            ExitCode::from(1)
        }
    }
}

/// What LucinaBoot's `add_reserved` collects from a tree.
fn reserved_ranges(fdt: &Fdt<'_>) -> Result<Vec<Range>, String> {
    let err = |e: lucina_fdt::Error| e.to_string();
    let mut out: Vec<Range> = fdt
        .mem_reservations()
        .filter_map(|(addr, size)| Range::new(addr, size))
        .collect();
    if let Some(rm) = fdt.reserved_memory().map_err(err)? {
        for region in rm {
            for reg in region.map_err(err)?.regs().map_err(err)? {
                out.extend(Range::new(reg.base, reg.size));
            }
        }
    }
    if let Some(fb) = fdt.chosen().map_err(err)?.and_then(|c| c.framebuffer) {
        out.extend(Range::new(fb.base, fb.size));
    }
    Ok(out)
}

/// LucinaBoot's refusals of a packaged tree (errors 9 and 10), found on the host instead of on the phone.
fn check_tree(loader: &str, packaged: &str) -> Result<(), String> {
    let (loader_bytes, packaged_bytes) = (read(loader)?, read(packaged)?);
    let loader_fdt = Fdt::new(&loader_bytes).map_err(|e| format!("{loader}: {e}"))?;
    let packaged_fdt = Fdt::new(&packaged_bytes).map_err(|e| format!("{packaged}: {e}"))?;
    let need = reserved_ranges(&loader_fdt)?;
    let have = reserved_ranges(&packaged_fdt)?;
    if let Some(r) = plan::uncovered(&have, &need) {
        return Err(format!(
            "{packaged}: does not reserve {r}, which {loader} does"
        ));
    }
    let root = packaged_fdt.root().map_err(|e| e.to_string())?;
    let mut memory_nodes = 0;
    for c in root.children() {
        let name = c.map_err(|e| e.to_string())?.name();
        memory_nodes += usize::from(name == "memory" || name.starts_with("memory@"));
    }
    if memory_nodes > 1 {
        return Err(format!("{packaged}: {memory_nodes} memory nodes"));
    }
    println!(
        "{packaged}: covers all {} reserved ranges of {loader}",
        need.len()
    );
    Ok(())
}

fn read(path: &str) -> Result<Vec<u8>, String> {
    read_image(Path::new(path)).map_err(|e| format!("{path}: {e}"))
}

fn write(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_image(Path::new(path), bytes).map_err(|e| format!("{path}: {e}"))
}

fn parse_hex(s: &str) -> Result<u64, String> {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| format!("bad hex number {s:?}"))
}

fn convert(args: &[String], loader: bool) -> Result<(), String> {
    let (mut input, mut out, mut text_offset) = (None, None, LOADER_TEXT_OFFSET);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => out = Some(it.next().ok_or("-o needs a path")?),
            "--text-offset" if loader => {
                text_offset = parse_hex(it.next().ok_or("--text-offset needs a value")?)?
            }
            _ if input.is_none() && !a.starts_with('-') => input = Some(a),
            _ => return Err(format!("unexpected argument {a:?}\n{USAGE}")),
        }
    }
    let (input, out) = (input.ok_or(USAGE)?, out.ok_or("missing -o <out>")?);
    let elf = read(input)?;
    let image = if loader {
        loader_image(&elf, text_offset)?
    } else {
        kernel_image(&elf)?
    };
    write(out, &image)?;
    let h = Image::parse(&image).map_err(|e| e.to_string())?;
    println!(
        "{out}: {} bytes, text_offset {:#x}, image_size {:#x}",
        image.len(),
        h.text_offset,
        h.image_size
    );
    Ok(())
}

fn pack_cmd(args: &[String]) -> Result<(), String> {
    let (mut out, mut kernel, mut initramfs) = (None, None, None);
    let mut dtbs = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "-o" => out = Some(val()?),
            "--kernel" => kernel = Some(read(val()?)?),
            "--initramfs" => initramfs = Some(read(val()?)?),
            "--dtb" => {
                let spec = val()?;
                let (name, path) = spec
                    .split_once('=')
                    .ok_or(format!("--dtb wants <name>=<file>, got {spec:?}"))?;
                dtbs.push((name.to_string(), read(path)?));
            }
            _ => return Err(format!("unexpected argument {a:?}\n{USAGE}")),
        }
    }
    let out = out.ok_or("missing -o <out>")?;
    let kernel = kernel.ok_or("missing --kernel <Image>")?;
    let bytes = pack(&kernel, initramfs.as_deref(), &dtbs)?;
    write(out, &bytes)?;
    println!("{out}: {} bytes", bytes.len());
    Ok(())
}

fn info(path: &str) -> Result<(), String> {
    let bytes = read(path)?;
    if let Ok(p) = Package::parse(&bytes) {
        println!("package, {} bytes, {} entries", p.bytes().len(), p.len());
        for e in p.entries() {
            println!(
                "  {:<9} {:<12} offset {:#10x} size {:#10x}",
                format!("{:?}", e.kind),
                e.name,
                e.offset,
                e.data.len()
            );
        }
        let k = Image::parse(p.kernel().data).map_err(|e| format!("kernel: {e}"))?;
        println!(
            "  kernel text_offset {:#x}, image_size {:#x}",
            k.text_offset, k.image_size
        );
        return Ok(());
    }
    match Image::parse(&bytes) {
        Ok(h) => {
            println!(
                "arm64 Image, {} bytes, text_offset {:#x}, image_size {:#x}, flags {:#x}",
                h.file_size, h.text_offset, h.image_size, h.flags
            );
            Ok(())
        }
        Err(e) => Err(format!(
            "{path}: neither a package ({}) nor an arm64 Image ({e})",
            Package::parse(&bytes).unwrap_err()
        )),
    }
}

/// Reads LucinaBoot's status band back from a framebuffer dump and compares it with the expected status.
fn check_band(args: &[String]) -> Result<(), String> {
    use lucina_boot::band::{Format, Geometry, Status, read_back};
    let path = args.first().ok_or(USAGE)?;
    let (mut w, mut h, mut stride, mut format) = (0, 0, 0, None);
    let (mut el, mut stages, mut error) = (None, None, None);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        let v = it.next().ok_or(format!("{a} needs a value"))?;
        let num = || {
            v.parse::<u32>()
                .map_err(|_| format!("{a}: bad number {v:?}"))
        };
        match a.as_str() {
            "--width" => w = num()?,
            "--height" => h = num()?,
            "--stride" => stride = num()?,
            "--format" => format = Some(Format::parse(v).ok_or(format!("unknown format {v:?}"))?),
            "--el" => el = Some(num()? as u8),
            "--stages" => stages = Some(num()? as u8),
            "--error" => error = Some(num()? as u8),
            _ => return Err(format!("unexpected argument {a:?}\n{USAGE}")),
        }
    }
    let format = format.ok_or("missing --format")?;
    let g = Geometry::new(w, h, stride, format).ok_or("bad framebuffer geometry")?;
    let want = Status {
        el: el.ok_or("missing --el")?,
        stages: stages.ok_or("missing --stages")?,
        error,
    };
    let raw = read(path)?;
    if (raw.len() as u64) < g.size() {
        return Err(format!(
            "{path}: {} bytes, the framebuffer needs {}",
            raw.len(),
            g.size()
        ));
    }
    match read_back(&g, &raw) {
        Some(got) if got == want => {
            println!("PASS status band: {got:?}");
            Ok(())
        }
        Some(got) => Err(format!("status band is {got:?}, expected {want:?}")),
        None => Err("no status band (cell 0 is not white)".into()),
    }
}
