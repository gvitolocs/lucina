//! `lucina-bootimg`: inspect and pack Android boot images (header v0-v2).
//!
//! ```text
//! lucina-bootimg info <boot.img>
//! lucina-bootimg pack -o <out.img> --kernel <f> [--ramdisk <f>] [--second <f>] [--recovery-dtbo <f>] [--dtb <f>]
//!                     [--header-version N] [--pagesize N] [--base N] [--kernel-offset N] [--ramdisk-offset N]
//!                     [--second-offset N] [--tags-offset N] [--dtb-offset N] [--os-version-raw N]
//!                     [--board S] [--cmdline S] [--surya]
//! ```
//! `--surya` (first option only) presets header v0, page 4096, base 0. Reads and writes regular files only
//! (see `lucina_bootimg::files`), so it can never read or write a partition.

use std::{path::Path, process::ExitCode};

use lucina_bootimg::{
    build::{BuildSpec, build},
    files::{read_image, write_image},
    parse,
};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("info") if args.len() == 2 => info(&args[1]),
        Some("pack") => pack(&args[1..]),
        _ => Err("usage: lucina-bootimg info <img> | pack -o <out> --kernel <f> [options]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lucina-bootimg: {e}");
            ExitCode::from(1)
        }
    }
}

fn info(path: &str) -> Result<(), String> {
    let bytes = read_image(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let img = parse(&bytes).map_err(|e| format!("{path}: {e:?}"))?;
    let h = &img.header;
    println!("header_version {}", h.version);
    println!("page_size      {}", h.page_size);
    println!(
        "kernel         {:>10} bytes @ file 0x{:x}, load 0x{:08x}",
        h.sizes.kernel, img.layout.kernel, h.kernel_addr
    );
    println!(
        "ramdisk        {:>10} bytes @ file 0x{:x}, load 0x{:08x}",
        h.sizes.ramdisk, img.layout.ramdisk, h.ramdisk_addr
    );
    println!(
        "second         {:>10} bytes @ file 0x{:x}, load 0x{:08x}",
        h.sizes.second, img.layout.second, h.second_addr
    );
    if h.version >= 1 {
        println!(
            "recovery_dtbo  {:>10} bytes @ file 0x{:x}",
            h.sizes.recovery_dtbo, img.layout.recovery_dtbo
        );
    }
    if h.version >= 2 {
        println!(
            "dtb            {:>10} bytes @ file 0x{:x}, load 0x{:x}",
            h.sizes.dtb, img.layout.dtb, h.dtb_addr
        );
    }
    println!("tags_addr      0x{:08x}", h.tags_addr);
    println!("os_version     0x{:08x}", h.os_version);
    println!("board          {:?}", String::from_utf8_lossy(h.name));
    println!(
        "cmdline        {:?}",
        String::from_utf8_lossy(&[h.cmdline, h.extra_cmdline].concat())
    );
    println!(
        "image end      0x{:x} (file {} bytes)",
        img.layout.end,
        bytes.len()
    );
    Ok(())
}

fn num<T: TryFrom<u64>>(s: &str) -> Result<T, String> {
    let v = match s.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => s.parse(),
    }
    .map_err(|e| format!("{s}: {e}"))?;
    T::try_from(v).map_err(|_| format!("{s}: out of range"))
}

fn pack(args: &[String]) -> Result<(), String> {
    let mut spec = BuildSpec::default();
    let mut args = args;
    // A preset, so it must come first: later flags override it, it never silently overrides them.
    if args.first().map(String::as_str) == Some("--surya") {
        spec = BuildSpec::surya_v0();
        args = &args[1..];
    }
    let mut out = None;
    let mut files: [(&str, Option<String>); 5] = [
        ("kernel", None),
        ("ramdisk", None),
        ("second", None),
        ("recovery-dtbo", None),
        ("dtb", None),
    ];
    let (mut board, mut cmdline) = (String::new(), String::new());
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if flag == "--surya" {
            return Err("--surya must be the first option after pack".into());
        }
        let val = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "-o" => out = Some(val.clone()),
            "--header-version" => spec.header_version = num(val)?,
            "--pagesize" => spec.page_size = num(val)?,
            "--base" => spec.base = num(val)?,
            "--kernel-offset" => spec.kernel_offset = num(val)?,
            "--ramdisk-offset" => spec.ramdisk_offset = num(val)?,
            "--second-offset" => spec.second_offset = num(val)?,
            "--tags-offset" => spec.tags_offset = num(val)?,
            "--dtb-offset" => spec.dtb_offset = num(val)?,
            "--os-version-raw" => spec.os_version = num(val)?,
            "--board" => board = val.clone(),
            "--cmdline" => cmdline = val.clone(),
            f => match files
                .iter_mut()
                .find(|(name, _)| f.strip_prefix("--") == Some(*name))
            {
                Some(slot) => slot.1 = Some(val.clone()),
                None => return Err(format!("unknown option {f}")),
            },
        }
    }
    let out = out.ok_or("-o <out> is required")?;
    let data: Vec<Vec<u8>> = files
        .iter()
        .map(|(_, path)| match path {
            Some(p) => read_image(Path::new(p)).map_err(|e| format!("{p}: {e}")),
            None => Ok(Vec::new()),
        })
        .collect::<Result<_, _>>()?;
    if files[0].1.is_none() {
        return Err("--kernel is required".into());
    }
    spec.name = board.as_bytes();
    spec.cmdline = cmdline.as_bytes();
    spec.kernel = &data[0];
    spec.ramdisk = &data[1];
    spec.second = &data[2];
    spec.recovery_dtbo = &data[3];
    spec.dtb = &data[4];
    let img = build(&spec).map_err(|e| format!("{e:?}"))?;
    write_image(Path::new(&out), &img).map_err(|e| format!("{out}: {e}"))?;
    println!("wrote {out} ({} bytes)", img.len());
    Ok(())
}
