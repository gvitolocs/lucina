//! The text printed by `lucina-fdt info`.

use std::{fmt::Write, string::String};

use crate::{Error, Fdt};

/// Describes memory, `/reserved-memory` (with `no-map` flags), `/chosen` and the memreserve block of `bytes`.
pub fn info(bytes: &[u8]) -> Result<String, Error> {
    let fdt = Fdt::new(bytes)?;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "version: {}, totalsize: {:#x}",
        fdt.version(),
        fdt.total_size()
    );

    let _ = writeln!(s, "memory:");
    for r in fdt.memory()? {
        let r = r?;
        let _ = writeln!(s, "  {:#x} +{:#x}", r.base, r.size);
    }

    let _ = writeln!(s, "reserved-memory:");
    match fdt.reserved_memory()? {
        None => {
            let _ = writeln!(s, "  (none)");
        }
        Some(children) => {
            for c in children {
                let c = c?;
                let mut flags = String::new();
                if c.no_map {
                    flags.push_str(" no-map");
                }
                if c.reusable {
                    flags.push_str(" reusable");
                }
                let mut any = false;
                for r in c.regs()? {
                    any = true;
                    let _ = writeln!(s, "  {}: {:#x} +{:#x}{flags}", c.name(), r.base, r.size);
                }
                if !any {
                    let _ = writeln!(s, "  {}: (no reg){flags}", c.name());
                }
            }
        }
    }

    let _ = writeln!(s, "chosen:");
    match fdt.chosen()? {
        None => {
            let _ = writeln!(s, "  (none)");
        }
        Some(c) => {
            if let Some(a) = c.bootargs {
                let _ = writeln!(s, "  bootargs = {a:?}");
            }
            if let Some(p) = c.stdout_path {
                let _ = writeln!(s, "  stdout-path = {p:?}");
            }
            if let Some((a, b)) = c.initrd {
                let _ = writeln!(s, "  initrd = {a:#x}..{b:#x}");
            }
            if let Some(fb) = c.framebuffer {
                let _ = writeln!(
                    s,
                    "  simple-framebuffer {}: {:#x} +{:#x} {}x{} stride {} {}",
                    fb.name, fb.base, fb.size, fb.width, fb.height, fb.stride, fb.format
                );
            }
        }
    }

    let _ = writeln!(s, "memreserve:");
    let mut any = false;
    for (a, z) in fdt.mem_reservations() {
        any = true;
        let _ = writeln!(s, "  {a:#x} +{z:#x}");
    }
    if !any {
        let _ = writeln!(s, "  (none)");
    }
    Ok(s)
}
