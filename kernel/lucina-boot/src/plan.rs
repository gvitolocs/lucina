//! Where LucinaBoot puts things in RAM.
//!
//! The kernel's place is fixed by the arm64 boot protocol: the lowest 2 MiB-aligned base in RAM plus the image's
//! `text_offset` (Asterinas cannot relocate itself). Everything else is placed around it:
//!
//! 1. If LucinaBoot itself sits where the kernel goes, it first copies itself elsewhere and starts again
//!    ([`Step::Relocate`]); the second run then finds no overlap.
//! 2. If the package (the ramdisk) sits where the kernel goes, it is moved; the initramfs stays inside it.
//! 3. The patched device tree gets [`DTB_MAX`] bytes of free RAM.
//!
//! "Free" means inside a memory region and clear of every reserved range, LucinaBoot, the loader's device tree,
//! the package and the kernel. Memory ranges from the loader are sorted and merged first (the mainline U-Boot
//! notes that ABL sometimes reports odd entries).

use crate::{Range, image::Image};

/// Most memory regions taken into account; extra ones are ignored.
pub const MAX_REGIONS: usize = 16;
/// Most reserved ranges (after merging); more is an error, since ignoring one could overwrite firmware memory.
pub const MAX_RESERVED: usize = 128;
/// Room for the patched device tree: the arm64 boot protocol's limit.
pub const DTB_MAX: u64 = 2 << 20;
/// Alignment of LucinaBoot's own copy when it moves (its link-time base is 0 and it is position independent).
pub const LOADER_ALIGN: u64 = 64 << 10;
const PAGE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No usable memory region.
    NoMemory,
    /// More than [`MAX_RESERVED`] reserved ranges.
    TooManyReserved,
    /// The kernel's fixed place is not inside one memory region.
    KernelNotInRam(Range),
    /// The kernel's fixed place overlaps a reserved range.
    KernelOverlapsReserved(Range),
    /// The loader put its device tree or the package on top of LucinaBoot.
    LoaderOverlaps(Range),
    /// No free RAM for the named item.
    NoRoom(&'static str),
    Overflow,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoMemory => f.write_str("no memory regions in the device tree"),
            Error::TooManyReserved => write!(f, "more than {MAX_RESERVED} reserved ranges"),
            Error::KernelNotInRam(r) => {
                write!(f, "kernel place {r} is not inside one memory region")
            }
            Error::KernelOverlapsReserved(r) => write!(f, "kernel place overlaps reserved {r}"),
            Error::LoaderOverlaps(r) => write!(f, "{r} overlaps LucinaBoot itself"),
            Error::NoRoom(what) => write!(f, "no free memory for the {what}"),
            Error::Overflow => f.write_str("address arithmetic overflow"),
        }
    }
}

/// What LucinaBoot knows when it plans.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'i> {
    /// RAM, from the loader's device tree.
    pub memory: &'i [Range],
    /// Reserved ranges: `/memreserve/`, `/reserved-memory` children (of both device trees), a framebuffer.
    pub reserved: &'i [Range],
    /// LucinaBoot's whole image, BSS and stack included.
    pub loader: Range,
    /// The device tree the loader passed in `x0`.
    pub loader_dtb: Range,
    /// The package, as the loader placed it.
    pub package: Range,
    pub kernel: Image,
}

/// Where everything ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub kernel: Range,
    /// Where the package is when the kernel starts (equal to the input when it does not move).
    pub package: Range,
    /// Room for the patched device tree ([`DTB_MAX`] bytes).
    pub dtb: Range,
}

impl Plan {
    pub fn moves_package(&self, inputs: &Inputs<'_>) -> bool {
        self.package != inputs.package
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Copy LucinaBoot to this address and start it again from there.
    Relocate(u64),
    Boot(Plan),
}

fn align_up(v: u64, a: u64) -> Option<u64> {
    Some(v.checked_add(a - 1)? & !(a - 1))
}

/// Sorted, merged, non-empty memory regions (at most [`MAX_REGIONS`]).
fn normalize(memory: &[Range]) -> ([Range; MAX_REGIONS], usize) {
    let mut out = [Range::default(); MAX_REGIONS];
    let mut n = 0;
    for r in memory.iter().filter(|r| !r.is_empty()).take(MAX_REGIONS) {
        out[n] = *r;
        n += 1;
    }
    // Insertion sort: n is tiny and this runs before anything else is set up.
    for i in 1..n {
        let mut j = i;
        while j > 0 && out[j - 1].start > out[j].start {
            out.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut m = 0;
    for i in 0..n {
        if m > 0 && out[i].start <= out[m - 1].end {
            out[m - 1].end = out[m - 1].end.max(out[i].end);
        } else {
            out[m] = out[i];
            m += 1;
        }
    }
    (out, m)
}

/// The lowest `align`-aligned address of `size` free bytes: inside one of `memory` (sorted), clear of `busy`.
pub fn find_free(memory: &[Range], busy: &[Range], size: u64, align: u64) -> Option<u64> {
    if size == 0 || !align.is_power_of_two() {
        return None;
    }
    for region in memory {
        let mut cand = align_up(region.start, align)?;
        'next: loop {
            let r = Range::new(cand, size)?;
            if !region.contains(&r) {
                break;
            }
            for b in busy {
                if b.overlaps(&r) {
                    cand = align_up(b.end, align)?;
                    continue 'next;
                }
            }
            return Some(cand);
        }
    }
    None
}

/// The first range of `need` that the union of `have` does not cover entirely, if any.
///
/// LucinaBoot uses it to refuse a packaged device tree that leaves out a firmware carve-out the loader's tree
/// reserves: the kernel would take that memory for itself.
pub fn uncovered(have: &[Range], need: &[Range]) -> Option<Range> {
    need.iter().copied().find(|n| {
        let mut at = n.start;
        while at < n.end {
            match have.iter().find(|h| h.start <= at && at < h.end) {
                Some(h) => at = h.end,
                None => return true,
            }
        }
        false
    })
}

/// The list of ranges nothing new may be placed on.
struct Busy {
    items: [Range; MAX_RESERVED + 6],
    len: usize,
}

impl Busy {
    fn push(&mut self, r: Range) {
        if !r.is_empty() {
            self.items[self.len] = r;
            self.len += 1;
        }
    }

    fn as_slice(&self) -> &[Range] {
        &self.items[..self.len]
    }
}

pub fn plan(inputs: &Inputs<'_>) -> Result<Step, Error> {
    let (mem, n) = normalize(inputs.memory);
    let mem = &mem[..n];
    let first = mem.first().ok_or(Error::NoMemory)?;
    if inputs.reserved.len() > MAX_RESERVED {
        return Err(Error::TooManyReserved);
    }

    let start = inputs
        .kernel
        .load_address(first.start)
        .ok_or(Error::Overflow)?;
    let kernel = Range::new(start, inputs.kernel.image_size).ok_or(Error::Overflow)?;
    if !mem.iter().any(|m| m.contains(&kernel)) {
        return Err(Error::KernelNotInRam(kernel));
    }
    if let Some(r) = inputs.reserved.iter().find(|r| r.overlaps(&kernel)) {
        return Err(Error::KernelOverlapsReserved(*r));
    }
    // The loader's device tree may lie where the kernel goes: LucinaBoot reads it only before the kernel copy,
    // which is its last write. It must not lie on LucinaBoot itself, and neither may the package.
    for r in [inputs.loader_dtb, inputs.package] {
        if r.overlaps(&inputs.loader) {
            return Err(Error::LoaderOverlaps(r));
        }
    }

    let mut busy = Busy {
        items: [Range::default(); MAX_RESERVED + 6],
        len: 0,
    };
    for r in inputs.reserved {
        busy.push(*r);
    }
    busy.push(kernel);
    busy.push(inputs.loader_dtb);
    busy.push(inputs.loader);
    busy.push(inputs.package);

    if inputs.loader.overlaps(&kernel) {
        let size = align_up(inputs.loader.size(), PAGE).ok_or(Error::Overflow)?;
        let to = find_free(mem, busy.as_slice(), size, LOADER_ALIGN)
            .ok_or(Error::NoRoom("LucinaBoot copy"))?;
        return Ok(Step::Relocate(to));
    }

    let package = if inputs.package.overlaps(&kernel) {
        let to = find_free(mem, busy.as_slice(), inputs.package.size(), PAGE)
            .ok_or(Error::NoRoom("package"))?;
        let moved = Range::new(to, inputs.package.size()).ok_or(Error::Overflow)?;
        busy.push(moved);
        moved
    } else {
        inputs.package
    };

    let dtb = find_free(mem, busy.as_slice(), DTB_MAX, PAGE).ok_or(Error::NoRoom("device tree"))?;
    Ok(Step::Boot(Plan {
        kernel,
        package,
        dtb: Range::new(dtb, DTB_MAX).ok_or(Error::Overflow)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn r(start: u64, end: u64) -> Range {
        Range { start, end }
    }

    fn asterinas(image_size: u64) -> Image {
        Image {
            text_offset: 0x20_0000,
            image_size,
            flags: 0b0010,
            file_size: image_size / 2,
        }
    }

    /// QEMU virt, 8 GiB: LucinaBoot at 0x40080000 (text_offset 0x80000), initrd at 0x48000000, DTB after it.
    fn qemu<'a>(memory: &'a [Range], reserved: &'a [Range]) -> Inputs<'a> {
        Inputs {
            memory,
            reserved,
            loader: r(0x4008_0000, 0x400b_0000),
            loader_dtb: r(0x4a00_0000, 0x4a10_0000),
            package: r(0x4800_0000, 0x4900_0000),
            kernel: asterinas(10 * MIB),
        }
    }

    #[test]
    fn qemu_layout() {
        let mem = [r(0x4000_0000, 0x2_4000_0000)];
        let i = qemu(&mem, &[]);
        let Step::Boot(p) = plan(&i).unwrap() else {
            panic!("no relocation expected")
        };
        assert_eq!(p.kernel, r(0x4020_0000, 0x40c0_0000));
        assert_eq!(p.package, i.package);
        assert!(!p.moves_package(&i));
        // The lowest free 2 MiB: right after the kernel.
        assert_eq!(p.dtb, r(0x40c0_0000, 0x40e0_0000));
    }

    #[test]
    fn surya_like_layout() {
        // RAM at 0x80000000 with firmware holes; LucinaBoot where ABL puts a kernel (base + 0x8000).
        let mem = [r(0x8000_0000, 0x8570_0000), r(0x8c00_0000, 0x1_8000_0000)];
        let reserved = [r(0x85700000, 0x85d0_0000), r(0x9c00_0000, 0x9d70_0000)];
        let i = Inputs {
            memory: &mem,
            reserved: &reserved,
            loader: r(0x8000_8000, 0x8003_8000),
            loader_dtb: r(0x8300_0000, 0x8305_0000),
            package: r(0x8100_0000, 0x8300_0000),
            kernel: asterinas(16 * MIB),
        };
        let Step::Boot(p) = plan(&i).unwrap() else {
            panic!()
        };
        assert_eq!(p.kernel, r(0x8020_0000, 0x8120_0000));
        // The kernel reaches into the package at 0x81000000, so the package moves (above the loader's DTB).
        assert!(p.moves_package(&i));
        assert_eq!(p.package, r(0x8305_0000, 0x8505_0000));
        // The device tree goes after the moved package, still below the hypervisor's region.
        assert_eq!(p.dtb.start, 0x8505_0000);
        for a in [p.kernel, p.package, p.dtb, i.loader, i.loader_dtb] {
            for b in [p.kernel, p.package, p.dtb, i.loader, i.loader_dtb] {
                assert!(a == b || !a.overlaps(&b), "{a} overlaps {b}");
            }
            assert!(!reserved.iter().any(|x| x.overlaps(&a)));
        }
    }

    #[test]
    fn loader_in_the_way_relocates() {
        let mem = [r(0x4000_0000, 0x2_4000_0000)];
        let mut i = qemu(&mem, &[]);
        i.loader = r(0x4020_0000, 0x4023_0000);
        let Step::Relocate(to) = plan(&i).unwrap() else {
            panic!("expected a relocation")
        };
        assert_eq!(to % LOADER_ALIGN, 0);
        let copy = r(to, to + 0x3_0000);
        for b in [
            r(0x4020_0000, 0x40c0_0000),
            i.loader,
            i.loader_dtb,
            i.package,
        ] {
            assert!(!copy.overlaps(&b), "{copy} overlaps {b}");
        }
        // The copy lands in the gap after the kernel and plans a normal boot.
        i.loader = copy;
        assert!(matches!(plan(&i).unwrap(), Step::Boot(_)));
    }

    #[test]
    fn errors() {
        let mem = [r(0x4000_0000, 0x4080_0000)];
        assert_eq!(
            plan(&qemu(&mem, &[])),
            Err(Error::KernelNotInRam(r(0x4020_0000, 0x40c0_0000)))
        );
        assert_eq!(plan(&qemu(&[], &[])), Err(Error::NoMemory));
        let mem = [r(0x4000_0000, 0x2_4000_0000)];
        let res = [r(0x4040_0000, 0x4050_0000)];
        assert_eq!(
            plan(&qemu(&mem, &res)),
            Err(Error::KernelOverlapsReserved(res[0]))
        );
        // The loader's tree may lie where the kernel goes (it is read before the kernel copy), but the patched tree
        // must not be written over it.
        let mut i = qemu(&mem, &[]);
        i.loader_dtb = r(0x4040_0000, 0x4041_0000);
        let Ok(Step::Boot(p)) = plan(&i) else {
            panic!("expected a plan")
        };
        assert!(p.kernel.overlaps(&i.loader_dtb) && !p.dtb.overlaps(&i.loader_dtb));
        // Nothing the loader placed may lie on LucinaBoot.
        let mut i = qemu(&mem, &[]);
        i.loader_dtb = r(0x4009_0000, 0x400a_0000);
        assert_eq!(plan(&i), Err(Error::LoaderOverlaps(i.loader_dtb)));
        let mut i = qemu(&mem, &[]);
        i.package = r(0x4000_0000, 0x4009_0000);
        assert_eq!(plan(&i), Err(Error::LoaderOverlaps(i.package)));
        let many = [r(0x1, 0x2); MAX_RESERVED + 1];
        assert_eq!(plan(&qemu(&mem, &many)), Err(Error::TooManyReserved));
        // RAM that ends right after the kernel: no room for the device tree.
        let tight = [r(0x4000_0000, 0x40c0_1000)];
        let mut i = qemu(&tight, &[]);
        i.package = r(0x4000_0000, 0x4008_0000);
        i.loader_dtb = r(0x400b_0000, 0x400c_0000);
        assert_eq!(plan(&i), Err(Error::NoRoom("device tree")));
    }

    #[test]
    fn uncovered_finds_gaps_in_the_union() {
        let have = [r(0x1000, 0x3000), r(0x3000, 0x5000), r(0x8000, 0x9000)];
        assert_eq!(
            uncovered(&have, &[r(0x1000, 0x5000)]),
            None,
            "covered by two adjacent ranges"
        );
        assert_eq!(
            uncovered(&have, &[r(0x2000, 0x2800), r(0x8000, 0x8001)]),
            None
        );
        assert_eq!(
            uncovered(&have, &[r(0x4000, 0x6000)]),
            Some(r(0x4000, 0x6000))
        );
        assert_eq!(
            uncovered(&have, &[r(0x9000, 0x9001)]),
            Some(r(0x9000, 0x9001))
        );
        assert_eq!(
            uncovered(&[], &[r(1, 1)]),
            None,
            "an empty range needs nothing"
        );
        assert_eq!(uncovered(&[], &[r(1, 2)]), Some(r(1, 2)));
    }

    #[test]
    fn memory_is_sorted_and_merged() {
        let (m, n) = normalize(&[
            r(0x9000, 0xa000),
            r(0, 0),
            r(0x1000, 0x5000),
            r(0x4000, 0x6000),
            r(0x6000, 0x7000),
        ]);
        assert_eq!(&m[..n], &[r(0x1000, 0x7000), r(0x9000, 0xa000)]);
    }

    #[test]
    fn find_free_skips_busy_and_respects_alignment() {
        let mem = [r(0x1000, 0x10000), r(0x20000, 0x40000)];
        assert_eq!(find_free(&mem, &[], 0x1000, 0x1000), Some(0x1000));
        assert_eq!(
            find_free(&mem, &[r(0x1000, 0x1001)], 0x1000, 0x1000),
            Some(0x2000)
        );
        // Does not fit the first region: the second one.
        assert_eq!(find_free(&mem, &[], 0x10000, 0x1000), Some(0x20000));
        assert_eq!(find_free(&mem, &[], 0x10000, 0x10000), Some(0x20000));
        assert_eq!(
            find_free(&mem, &[r(0x20000, 0x38000)], 0x10000, 0x1000),
            None
        );
        assert_eq!(find_free(&mem, &[], 0, 0x1000), None);
        assert_eq!(find_free(&mem, &[], 1, 3), None);
        // No overflow at the top of the address space.
        let top = [r(u64::MAX - 0x2000, u64::MAX)];
        assert_eq!(
            find_free(&top, &[r(u64::MAX - 0x1fff, u64::MAX)], 0x1000, 0x1000),
            None
        );
    }

    /// Random layouts: whatever `plan` returns never overlaps anything it must avoid.
    #[test]
    fn random_plans_are_sound() {
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for _ in 0..20_000 {
            let base = 0x4000_0000 + rnd(4) * 0x4000_0000;
            let mem = [
                r(base, base + (16 + rnd(64)) * MIB),
                r(base + 128 * MIB, base + 256 * MIB),
            ];
            let mut reserved = [Range::default(); 3];
            for res in reserved.iter_mut() {
                let s = base + rnd(256) * MIB;
                *res = r(s, s + rnd(4) * MIB);
            }
            let at = |rnd: &mut dyn FnMut(u64) -> u64, size: u64| {
                let s = base + rnd(255 * 16) * 64 * 1024;
                r(s, s + size)
            };
            let loader = at(&mut rnd, 0x3_0000);
            let loader_dtb = at(&mut rnd, 0x1_0000);
            let package_size = (1 + rnd(16)) * MIB;
            let package = at(&mut rnd, package_size);
            let kernel = asterinas((1 + rnd(16)) * MIB);
            let i = Inputs {
                memory: &mem,
                reserved: &reserved,
                loader,
                loader_dtb,
                package,
                kernel,
            };
            match plan(&i) {
                Ok(Step::Relocate(to)) => {
                    let copy = r(to, to + 0x3_0000);
                    assert!(mem.iter().any(|m| m.contains(&copy)));
                    let k =
                        Range::new(kernel.load_address(base).unwrap(), kernel.image_size).unwrap();
                    for b in reserved.iter().chain(&[k, loader, loader_dtb, package]) {
                        assert!(!copy.overlaps(b));
                    }
                }
                Ok(Step::Boot(p)) => {
                    assert!(!loader.overlaps(&p.kernel));
                    let placed = [p.kernel, p.package, p.dtb];
                    for (j, a) in placed.iter().enumerate() {
                        let unmoved = j == 1 && *a == package;
                        assert!(
                            unmoved || mem.iter().any(|m| m.contains(a)),
                            "{a} outside RAM"
                        );
                        assert!(
                            !reserved.iter().any(|res| res.overlaps(a)) || j == 1 && *a == package
                        );
                        assert!(!a.overlaps(&loader) || unmoved);
                        // Only the kernel copy, the last write, may cover the loader's tree.
                        assert!(!a.overlaps(&loader_dtb) || unmoved || j == 0);
                        for b in &placed[j + 1..] {
                            assert!(!a.overlaps(b), "{a} overlaps {b}");
                        }
                    }
                }
                Err(_) => {}
            }
        }
    }
}
