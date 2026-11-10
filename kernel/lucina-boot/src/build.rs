//! Host side: flat `Image` files from ELF files, and packages from parts.

use crate::{
    image::{self, Image},
    pkg::{self, Kind, Package},
};

/// `--text-offset` default for LucinaBoot: QEMU then loads it at RAM base + 0x80000, below the kernel's
/// 0x200000. On surya ABL places it wherever it likes; LucinaBoot moves itself if that is in the kernel's way.
pub const LOADER_TEXT_OFFSET: u64 = 0x8_0000;

const PT_LOAD: u32 = 1;
const EM_AARCH64: u16 = 183;

fn le<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N], String> {
    b.get(off..off + N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| "ELF file truncated".to_string())
}

fn u16_at(b: &[u8], off: usize) -> Result<u16, String> {
    Ok(u16::from_le_bytes(le(b, off)?))
}

fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    Ok(u32::from_le_bytes(le(b, off)?))
}

fn u64_at(b: &[u8], off: usize) -> Result<u64, String> {
    Ok(u64::from_le_bytes(le(b, off)?))
}

/// A flattened ELF: the bytes from the lowest loaded physical address to the end of the last file-backed byte,
/// and the size including every segment's memory size (BSS).
#[derive(Debug)]
pub struct Flat {
    pub base: u64,
    pub bytes: Vec<u8>,
    pub mem_size: u64,
}

/// Lays out every `PT_LOAD` segment of a little-endian AArch64 ELF64 file at its physical address (`p_paddr`,
/// which for Asterinas differs from the high virtual address), relative to the lowest one.
pub fn flatten_elf(elf: &[u8]) -> Result<Flat, String> {
    if elf.get(..4) != Some(b"\x7fELF") || elf.get(4) != Some(&2) || elf.get(5) != Some(&1) {
        return Err("not a little-endian ELF64 file".into());
    }
    if u16_at(elf, 18)? != EM_AARCH64 {
        return Err("not an AArch64 ELF file".into());
    }
    let phoff = u64_at(elf, 32)? as usize;
    let phentsize = u16_at(elf, 54)? as usize;
    let phnum = u16_at(elf, 56)? as usize;
    if phentsize < 56 {
        return Err("bad program header size".into());
    }
    let mut segs = Vec::new();
    for i in 0..phnum {
        let ph = phoff + i * phentsize;
        if u32_at(elf, ph)? != PT_LOAD {
            continue;
        }
        let (offset, paddr) = (u64_at(elf, ph + 8)?, u64_at(elf, ph + 24)?);
        let (filesz, memsz) = (u64_at(elf, ph + 32)?, u64_at(elf, ph + 40)?);
        if memsz == 0 {
            continue;
        }
        if filesz > memsz {
            return Err(format!("segment {i}: p_filesz above p_memsz"));
        }
        segs.push((offset, paddr, filesz, memsz));
    }
    let base = segs
        .iter()
        .map(|s| s.1)
        .min()
        .ok_or("no loadable segments")?;
    let mut file_end = base;
    let mut mem_end = base;
    for &(_, paddr, filesz, memsz) in &segs {
        file_end = file_end.max(paddr.checked_add(filesz).ok_or("segment overflows")?);
        mem_end = mem_end.max(paddr.checked_add(memsz).ok_or("segment overflows")?);
    }
    let len = usize::try_from(file_end - base).map_err(|_| "image too large")?;
    if len > (256 << 20) {
        return Err("image larger than 256 MiB".into());
    }
    segs.sort_by_key(|s| s.1);
    for (i, w) in segs.windows(2).enumerate() {
        if w[0].1 + w[0].3 > w[1].1 {
            return Err(format!("segment {} overlaps another", i + 1));
        }
    }
    let mut bytes = vec![0u8; len];
    for (i, &(offset, paddr, filesz, _)) in segs.iter().enumerate() {
        let src = elf
            .get(offset as usize..offset.saturating_add(filesz) as usize)
            .ok_or_else(|| format!("segment {i} lies outside the file"))?;
        let at = (paddr - base) as usize;
        bytes[at..at + src.len()].copy_from_slice(src);
    }
    Ok(Flat {
        base,
        bytes,
        mem_size: mem_end - base,
    })
}

/// The kernel `Image` from an ELF: flattened, and its own header checked against the ELF.
pub fn kernel_image(elf: &[u8]) -> Result<Vec<u8>, String> {
    let flat = flatten_elf(elf)?;
    let img = Image::parse(&flat.bytes).map_err(|e| format!("kernel: {e}"))?;
    // Placed at a 2 MiB-aligned base plus text_offset, the image must land where it was linked.
    if flat.base % image::BASE_ALIGN != img.text_offset % image::BASE_ALIGN {
        return Err(format!(
            "kernel linked at {:#x}, which no 2 MiB-aligned base plus text_offset {:#x} reaches",
            flat.base, img.text_offset
        ));
    }
    if img.image_size < flat.mem_size {
        return Err(format!(
            "kernel header image_size {:#x} is below the ELF's memory size {:#x}",
            img.image_size, flat.mem_size
        ));
    }
    Ok(flat.bytes)
}

/// LucinaBoot's `Image` from its ELF: flattened, with `text_offset` and `image_size` (BSS and stack included,
/// rounded to 4 KiB) written into the header the assembly leaves blank.
pub fn loader_image(elf: &[u8], text_offset: u64) -> Result<Vec<u8>, String> {
    let flat = flatten_elf(elf)?;
    if flat.base != 0 {
        return Err(format!(
            "LucinaBoot must be linked at 0, not {:#x}",
            flat.base
        ));
    }
    // LucinaBoot must not write past the bytes the loader placed before it has checked where things are, so BSS
    // and the stack have to be inside the file (boot/lucinaboot/linker.ld).
    if flat.mem_size != flat.bytes.len() as u64 {
        return Err(format!(
            "LucinaBoot occupies {:#x} bytes but its file has only {:#x}: BSS or stack outside the file",
            flat.mem_size,
            flat.bytes.len()
        ));
    }
    let mut bytes = flat.bytes;
    let size = flat.mem_size.div_ceil(4096) * 4096;
    image::set_header(&mut bytes, text_offset, size).map_err(|e| format!("LucinaBoot: {e}"))?;
    Ok(bytes)
}

/// Builds a package. Every part is checked: the kernel must be an arm64 `Image`, each device tree must parse.
pub fn pack(
    kernel: &[u8],
    initramfs: Option<&[u8]>,
    dtbs: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, String> {
    Image::parse(kernel).map_err(|e| format!("kernel: {e}"))?;
    for (name, dtb) in dtbs {
        lucina_fdt::Fdt::new(dtb).map_err(|e| format!("device tree {name}: {e}"))?;
    }
    let mut parts: Vec<(Kind, &str, &[u8])> = vec![(Kind::Kernel, "", kernel)];
    if let Some(rd) = initramfs {
        parts.push((Kind::Initramfs, "", rd));
    }
    parts.extend(
        dtbs.iter()
            .map(|(n, d)| (Kind::Dtb, n.as_str(), d.as_slice())),
    );
    let spec: Vec<(Kind, &str, u64)> = parts
        .iter()
        .map(|&(k, n, d)| (k, n, d.len() as u64))
        .collect();
    let table = pkg::HEADER_SIZE + spec.len() * pkg::ENTRY_SIZE;
    let mut head = vec![0u8; table];
    let (offsets, total) = pkg::layout(&spec, &mut head).map_err(|e| e.to_string())?;
    let mut out = vec![0u8; total as usize];
    out[..table].copy_from_slice(&head);
    for (i, (_, _, data)) in parts.iter().enumerate() {
        let o = offsets[i] as usize;
        out[o..o + data.len()].copy_from_slice(data);
    }
    Package::parse(&out).map_err(|e| format!("internal: built package does not parse: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF64 with the given (paddr, file bytes, memsz) segments.
    fn elf(segs: &[(u64, &[u8], u64)]) -> Vec<u8> {
        let phoff = 64usize;
        let data_off = phoff + 56 * segs.len();
        let mut b = vec![0u8; data_off];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[18..20].copy_from_slice(&EM_AARCH64.to_le_bytes());
        b[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&(segs.len() as u16).to_le_bytes());
        for (i, &(paddr, data, memsz)) in segs.iter().enumerate() {
            let ph = phoff + 56 * i;
            let off = b.len() as u64;
            b[ph..ph + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
            b[ph + 8..ph + 16].copy_from_slice(&off.to_le_bytes());
            // A high virtual address, as Asterinas has.
            b[ph + 16..ph + 24].copy_from_slice(&(paddr | 0xffff_ff00_0000_0000).to_le_bytes());
            b[ph + 24..ph + 32].copy_from_slice(&paddr.to_le_bytes());
            b[ph + 32..ph + 40].copy_from_slice(&(data.len() as u64).to_le_bytes());
            b[ph + 40..ph + 48].copy_from_slice(&memsz.to_le_bytes());
            b.extend_from_slice(data);
        }
        b
    }

    fn header(text_offset: u64, image_size: u64) -> Vec<u8> {
        let mut h = vec![0u8; 64];
        h[8..16].copy_from_slice(&text_offset.to_le_bytes());
        h[16..24].copy_from_slice(&image_size.to_le_bytes());
        h[24..32].copy_from_slice(&0b0010u64.to_le_bytes());
        h[56..60].copy_from_slice(&image::MAGIC.to_le_bytes());
        h
    }

    #[test]
    fn flattens_by_physical_address() {
        let e = elf(&[
            (0x4020_0000, &header(0x20_0000, 0x3000), 64),
            (0x4020_1000, b"TEXT", 0x2000),
        ]);
        let flat = flatten_elf(&e).unwrap();
        assert_eq!(flat.base, 0x4020_0000);
        assert_eq!(flat.bytes.len(), 0x1004);
        assert_eq!(&flat.bytes[0x1000..], b"TEXT");
        assert_eq!(flat.mem_size, 0x3000);
        assert_eq!(kernel_image(&e).unwrap().len(), 0x1004);
        // A header that claims less than the ELF needs is refused.
        let small = elf(&[
            (0x4020_0000, &header(0x20_0000, 0x2000), 64),
            (0x4020_1000, b"T", 0x2000),
        ]);
        assert!(kernel_image(&small).unwrap_err().contains("image_size"));
        // Linked 1 MiB off the 2 MiB grid: no base plus text_offset puts it there.
        let off = elf(&[(0x4030_0000, &header(0x20_0000, 0x3000), 64)]);
        assert!(kernel_image(&off).unwrap_err().contains("2 MiB-aligned"));
    }

    #[test]
    fn loader_header_is_filled() {
        let e = elf(&[(0, &header(0, 0), 64), (0x1000, &[7; 0x4123], 0x4123)]);
        let img = loader_image(&e, LOADER_TEXT_OFFSET).unwrap();
        let h = Image::parse(&img).unwrap();
        assert_eq!((h.text_offset, h.image_size), (0x8_0000, 0x6000));
        // BSS beyond the file is refused.
        let bss = elf(&[(0, &header(0, 0), 64), (0x1000, b"CODE", 0x5123)]);
        assert!(
            loader_image(&bss, LOADER_TEXT_OFFSET)
                .unwrap_err()
                .contains("outside the file")
        );
        let high = elf(&[(0x1000, &header(0, 0), 64)]);
        assert!(loader_image(&high, LOADER_TEXT_OFFSET).is_err());
    }

    #[test]
    fn rejects_bad_elf() {
        assert!(flatten_elf(b"\x7fELF").is_err());
        let mut e = elf(&[(0, b"x", 1)]);
        e[18] = 62; // x86-64
        assert!(flatten_elf(&e).unwrap_err().contains("AArch64"));
        let e = elf(&[(0, b"xy", 1)]);
        assert!(flatten_elf(&e).unwrap_err().contains("p_filesz"));
        let e = elf(&[(0, b"ab", 2), (1, b"cd", 2)]);
        assert!(flatten_elf(&e).unwrap_err().contains("overlaps"));
    }

    #[test]
    fn packs_and_checks_parts() {
        let kernel = header(0x20_0000, 0x1000);
        let dtb = include_bytes!("../../../kernel/lucina-fdt/tests/data/qemu-virt-gicv3-8g.dtb");
        let p = pack(&kernel, Some(b"cpio"), &[("default".into(), dtb.to_vec())]).unwrap();
        let pkg = Package::parse(&p).unwrap();
        assert_eq!(pkg.kernel().data, &kernel[..]);
        assert_eq!(pkg.initramfs().unwrap().data, b"cpio");
        assert_eq!(pkg.dtb("default").unwrap().data.len(), dtb.len());
        assert!(pack(b"not an image", None, &[]).is_err());
        assert!(
            pack(&kernel, None, &[("x".into(), vec![0; 64])])
                .unwrap_err()
                .contains("device tree x")
        );
    }
}
