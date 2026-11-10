//! LucinaBoot v1 (decision D-009): the stage ABL starts as the "kernel" of a surya boot image.
//!
//! 1. Report `CurrentEL` and the device tree the loader passed in `x0`.
//! 2. Find the payload package in the ramdisk (`/chosen` `linux,initrd-start/end`).
//! 3. Choose the device tree: the packaged one for the panel ABL names, else ABL's own.
//! 4. Plan RAM ([`lucina_boot::plan`]): move LucinaBoot itself or the package if the kernel's place needs it.
//! 5. Write the device tree with `/chosen` and `/memory` fixed up, copy the kernel, clean caches, jump.
//!
//! No storage, USB, display or clock driver: everything comes from RAM the loader already filled. Any error is
//! printed and the machine is reset through PSCI (back to Android on the phone; QEMU exits with `--no-reboot`).

#![no_std]
#![no_main]

mod arch;
mod console;
mod screen;

use core::{fmt, panic::PanicInfo};

use lucina_boot::{
    Range,
    image::{self, Image},
    panel,
    pkg::{self, Package},
    plan::{self, DTB_MAX, Inputs, MAX_REGIONS, MAX_RESERVED, Step},
};
use lucina_fdt::{Edit, Fdt};

core::arch::global_asm!(include_str!("entry.S"));

unsafe extern "C" {
    static _head: u8;
    static __file_end: u8;
    static __image_end: u8;
}

/// Largest device tree accepted from the loader (the boot protocol's limit for what we pass on).
const DTB_LIMIT: u64 = 2 << 20;
/// How long an error stays on the console and the screen before the reset.
const ERROR_WAIT_MS: u64 = 3000;
/// Status band code for a panic (the [`Fail`] codes are 1 to 7).
const PANIC_CODE: u8 = 8;

enum Fail {
    NoDeviceTree,
    Fdt(lucina_fdt::Error),
    NoPayload,
    Package(pkg::Error),
    Image(image::Error),
    Plan(plan::Error),
    TooManyRegions,
    /// The packaged device tree does not reserve this range, which the loader's tree does.
    NotReserved(Range),
    /// The packaged device tree has more than one memory node (LucinaBoot writes ABL's RAM into one).
    MemoryNodes,
}

impl Fail {
    /// The code the status band shows (`lucina_boot::band`); 8 is a panic ([`PANIC_CODE`]).
    fn code(&self) -> u8 {
        match self {
            Fail::NoDeviceTree => 1,
            Fail::Fdt(_) => 2,
            Fail::NoPayload => 3,
            Fail::Package(_) => 4,
            Fail::Image(_) => 5,
            Fail::Plan(_) => 6,
            Fail::TooManyRegions => 7,
            Fail::NotReserved(_) => 9,
            Fail::MemoryNodes => 10,
        }
    }
}

impl From<lucina_fdt::Error> for Fail {
    fn from(e: lucina_fdt::Error) -> Self {
        Fail::Fdt(e)
    }
}

impl From<pkg::Error> for Fail {
    fn from(e: pkg::Error) -> Self {
        Fail::Package(e)
    }
}

impl From<image::Error> for Fail {
    fn from(e: image::Error) -> Self {
        Fail::Image(e)
    }
}

impl From<plan::Error> for Fail {
    fn from(e: plan::Error) -> Self {
        Fail::Plan(e)
    }
}

impl fmt::Display for Fail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fail::NoDeviceTree => {
                f.write_str("x0 does not point to a device tree of at most 2 MiB")
            }
            Fail::Fdt(e) => write!(f, "device tree: {e}"),
            Fail::NoPayload => f.write_str("no payload: /chosen has no linux,initrd-start/end"),
            Fail::Package(e) => write!(f, "payload: {e}"),
            Fail::Image(e) => write!(f, "kernel: {e}"),
            Fail::Plan(e) => write!(f, "memory plan: {e}"),
            Fail::TooManyRegions => write!(
                f,
                "more than {MAX_REGIONS} memory or {MAX_RESERVED} reserved ranges, or one that overflows"
            ),
            Fail::NotReserved(r) => write!(
                f,
                "the packaged device tree does not reserve {r}, which the loader's tree reserves"
            ),
            Fail::MemoryNodes => {
                f.write_str("the packaged device tree has more than one memory node")
            }
        }
    }
}

/// Called from `entry.S` with the loader's `x0` and the address LucinaBoot runs at.
#[unsafe(no_mangle)]
extern "C" fn lucinaboot_main(dtb_pa: u64, base: u64) -> ! {
    let e = match run(dtb_pa, base) {
        Ok(never) => match never {},
        Err(e) => e,
    };
    println!("LucinaBoot: error: {e}");
    screen::failed(e.code());
    stop()
}

fn stop() -> ! {
    println!("LucinaBoot: resetting");
    arch::delay_ms(ERROR_WAIT_MS);
    arch::reset()
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    println!("LucinaBoot: panic: {info}");
    screen::failed(PANIC_CODE);
    stop()
}

/// Ranges collected from device trees, merged as they come in, at most [`MAX_RESERVED`] after merging.
struct Ranges {
    items: [Range; MAX_RESERVED],
    len: usize,
}

impl Ranges {
    fn new() -> Ranges {
        Ranges {
            items: [Range::default(); MAX_RESERVED],
            len: 0,
        }
    }

    /// Adds `r` (`None`: its end does not fit a u64, an error rather than a range to ignore). A range that
    /// overlaps or touches one already there extends it, so the same carve-out in two trees counts once.
    fn push(&mut self, r: Option<Range>) -> Result<(), Fail> {
        let r = r.ok_or(Fail::TooManyRegions)?;
        if r.is_empty() {
            return Ok(());
        }
        if let Some(m) = self.items[..self.len]
            .iter_mut()
            .find(|m| m.start <= r.end && r.start <= m.end)
        {
            m.start = m.start.min(r.start);
            m.end = m.end.max(r.end);
            return Ok(());
        }
        *self.items.get_mut(self.len).ok_or(Fail::TooManyRegions)? = r;
        self.len += 1;
        Ok(())
    }

    /// `/memreserve/` entries, `/reserved-memory` children and a `/chosen` framebuffer of `fdt`.
    fn add_reserved(&mut self, fdt: &Fdt<'_>) -> Result<(), Fail> {
        for (addr, size) in fdt.mem_reservations() {
            self.push(Range::new(addr, size))?;
        }
        if let Some(rm) = fdt.reserved_memory()? {
            for region in rm {
                for reg in region?.regs()? {
                    self.push(Range::new(reg.base, reg.size))?;
                }
            }
        }
        if let Some(fb) = fdt.chosen()?.and_then(|c| c.framebuffer) {
            self.push(Range::new(fb.base, fb.size))?;
        }
        Ok(())
    }

    fn as_slice(&self) -> &[Range] {
        &self.items[..self.len]
    }
}

fn run(dtb_pa: u64, base: u64) -> Result<core::convert::Infallible, Fail> {
    // Timing for the boot log (after a restart from a copy: since the copy started).
    let t_entry = arch::now_us();
    // SAFETY: the boot protocol puts a device tree in RAM at x0 and leaves it alone.
    let dtb = unsafe { arch::device_tree(dtb_pa, DTB_LIMIT) }.ok_or(Fail::NoDeviceTree)?;
    let fdt = Fdt::new(dtb)?;
    console::init(&fdt);
    arch::psci_init(&fdt);
    screen::init(&fdt);
    let el = arch::current_el() as u8;
    screen::reached(el, 1);

    let head = &raw const _head as u64;
    let loader = Range {
        start: base,
        end: base + (&raw const __image_end as u64 - head),
    };
    let file_bytes = &raw const __file_end as u64 - head;
    println!("LucinaBoot {}: CurrentEL=EL{el}", env!("CARGO_PKG_VERSION"));
    println!(
        "LucinaBoot: device tree at {dtb_pa:#x} ({} bytes), running at {loader}",
        fdt.total_size()
    );
    if let Some((fb, g)) = screen::geometry() {
        println!(
            "LucinaBoot: status band on the framebuffer at {fb:#x} ({}x{})",
            g.width, g.height
        );
    }

    let chosen = fdt.chosen()?;
    let bootargs = chosen.and_then(|c| c.bootargs).unwrap_or("");
    let (rd_start, rd_end) = chosen.and_then(|c| c.initrd).ok_or(Fail::NoPayload)?;
    let ramdisk = Range {
        start: rd_start,
        end: rd_end.max(rd_start),
    };
    arch::dcache_clean_invalidate(ramdisk);
    // SAFETY: the loader placed the ramdisk there and nothing else writes it until we move it.
    let pkg = Package::parse(unsafe { arch::phys(ramdisk.start, ramdisk.size()) })?;
    let kernel = Image::parse(pkg.kernel().data)?;
    screen::reached(el, 2);
    let panel = panel::from_cmdline(bootargs);
    let (dtb_entry, choice) = panel::choose(&pkg, panel);
    let dtb_name = dtb_entry.map(|e| e.name);
    println!(
        "LucinaBoot: payload at {ramdisk}: kernel {} bytes (image {:#x}, text_offset {:#x}), initramfs {} bytes, \
         panel {panel:?}, device tree {} ({choice:?})",
        pkg.kernel().data.len(),
        kernel.image_size,
        kernel.text_offset,
        pkg.initramfs().map_or(0, |e| e.data.len()),
        dtb_name.unwrap_or("from the loader")
    );

    let mut memory = [Range::default(); MAX_REGIONS];
    let mut n = 0;
    // Every memory range counts: dropping one could leave the lowest bank out and put the kernel in the wrong place.
    for r in fdt.memory()? {
        let r = r?;
        let range = Range::new(r.base, r.size).ok_or(Fail::TooManyRegions)?;
        *memory.get_mut(n).ok_or(Fail::TooManyRegions)? = range;
        n += 1;
    }
    let mut reserved = Ranges::new();
    reserved.add_reserved(&fdt)?;
    if let Some(e) = dtb_entry {
        // The kernel only learns about reserved memory from the packaged tree, so that tree must reserve at least
        // what the loader's tree does, and have one memory node for the loader's RAM.
        let packaged = Fdt::new(e.data)?;
        let mut own = Ranges::new();
        own.add_reserved(&packaged)?;
        if let Some(r) = plan::uncovered(own.as_slice(), reserved.as_slice()) {
            return Err(Fail::NotReserved(r));
        }
        let mut memory_nodes = 0;
        for c in packaged.root()?.children() {
            let name = c?.name();
            memory_nodes += usize::from(name == "memory" || name.starts_with("memory@"));
        }
        if memory_nodes > 1 {
            return Err(Fail::MemoryNodes);
        }
        reserved.add_reserved(&packaged)?;
    }
    let inputs = Inputs {
        memory: &memory[..n],
        reserved: reserved.as_slice(),
        loader,
        loader_dtb: Range::new(dtb_pa, fdt.total_size() as u64).ok_or(Fail::NoDeviceTree)?,
        package: Range::new(ramdisk.start, pkg.bytes().len() as u64).ok_or(Fail::NoPayload)?,
        kernel,
    };

    let plan = match plan::plan(&inputs)? {
        Step::Relocate(to) => {
            println!("LucinaBoot: in the kernel's place, moving to {to:#x} and starting again");
            arch::dcache_clean_invalidate(Range::new(to, loader.size()).ok_or(Fail::NoDeviceTree)?);
            // SAFETY: the plan picked free RAM for the whole image, clear of the running copy.
            unsafe { arch::restart_at(to, base, file_bytes, dtb_pa) }
        }
        Step::Boot(p) => p,
    };
    screen::reached(el, 3);

    let t_move = arch::now_us();
    let pkg = if plan.moves_package(&inputs) {
        println!("LucinaBoot: moving the payload to {}", plan.package);
        arch::dcache_clean_invalidate(plan.package);
        // SAFETY: the plan keeps the new place clear of everything in use, including the old place.
        let dst = unsafe { arch::phys_mut(plan.package.start, plan.package.size()) };
        dst.copy_from_slice(pkg.bytes());
        arch::dcache_clean_invalidate(plan.package);
        Package::parse(dst)?
    } else {
        pkg
    };
    let move_us = arch::now_us() - t_move;

    // The device tree for the kernel: the chosen packaged one with ABL's cmdline and RAM, or ABL's own.
    let initrd = pkg
        .initramfs()
        .map(|e| (plan.package.start + e.offset, e.data.len() as u64));
    let target = match dtb_name.and_then(|name| pkg.dtb(name)) {
        Some(e) => Fdt::new(e.data)?,
        None => fdt,
    };
    let use_loader_dtb = dtb_name.is_none();
    let (rd_start, rd_end) = initrd.map_or((0, 0), |(s, l)| (s, s + l));
    let (rd_start, rd_end) = (rd_start.to_be_bytes(), rd_end.to_be_bytes());
    let mut reg = [0u8; MAX_REGIONS * 16];
    let reg_len = encode_memory(&target, &memory[..n], &mut reg)?;
    let raw_bootargs = fdt
        .property("/chosen", "bootargs")?
        .map_or(&b"\0"[..], |p| p.value);

    let mut edits = [Edit::Remove {
        node: "/chosen",
        name: "lucina,unused",
    }; 6];
    let mut k = 0;
    let mut add = |e| {
        edits[k] = e;
        k += 1;
    };
    if initrd.is_some() {
        add(Edit::Set {
            node: "/chosen",
            name: "linux,initrd-start",
            value: &rd_start,
        });
        add(Edit::Set {
            node: "/chosen",
            name: "linux,initrd-end",
            value: &rd_end,
        });
    } else {
        add(Edit::Remove {
            node: "/chosen",
            name: "linux,initrd-start",
        });
        add(Edit::Remove {
            node: "/chosen",
            name: "linux,initrd-end",
        });
    }
    if !use_loader_dtb {
        add(Edit::Set {
            node: "/chosen",
            name: "bootargs",
            value: raw_bootargs,
        });
        add(Edit::Set {
            node: "/memory",
            name: "reg",
            value: &reg[..reg_len],
        });
        // If the tree has no memory node, the edit above creates one, which needs this to count as memory.
        add(Edit::Set {
            node: "/memory",
            name: "device_type",
            value: b"memory\0",
        });
    }
    let edits = &edits[..k];

    let t_dtb = arch::now_us();
    arch::dcache_clean_invalidate(plan.dtb);
    // SAFETY: the plan reserved DTB_MAX free bytes there.
    let out = unsafe { arch::phys_mut(plan.dtb.start, DTB_MAX) };
    let dtb_len = lucina_fdt::patch(&target, edits, out)? as u64;
    let dtb_us = arch::now_us() - t_dtb;

    let t_copy = arch::now_us();
    arch::dcache_clean_invalidate(plan.kernel);
    // SAFETY: the plan placed the kernel in RAM clear of everything else that is live.
    let dst = unsafe { arch::phys_mut(plan.kernel.start, plan.kernel.size()) };
    let image = pkg.kernel().data;
    dst[..image.len()].copy_from_slice(image);
    dst[image.len()..].fill(0);
    arch::dcache_clean_invalidate(plan.kernel);
    arch::dcache_clean_invalidate(Range::new(plan.dtb.start, dtb_len).ok_or(Fail::NoDeviceTree)?);
    arch::icache_invalidate_all();
    let copy_us = arch::now_us() - t_copy;
    screen::reached(el, 4);

    match initrd {
        Some((s, l)) => println!(
            "LucinaBoot: kernel at {}, device tree at {:#x} ({dtb_len} bytes), initramfs at {:#x}-{:#x}",
            plan.kernel,
            plan.dtb.start,
            s,
            s + l
        ),
        None => println!(
            "LucinaBoot: kernel at {}, device tree at {:#x} ({dtb_len} bytes), no initramfs",
            plan.kernel, plan.dtb.start
        ),
    }
    println!(
        "LucinaBoot: {} us from entry to the kernel (copying the kernel {copy_us} us, moving the payload \
         {move_us} us, device tree {dtb_us} us)",
        arch::now_us() - t_entry
    );
    println!("LucinaBoot: starting the kernel");
    screen::reached(el, 5);
    // SAFETY: image and device tree are in place and cleaned to the point of coherency.
    unsafe { arch::enter_kernel(plan.kernel.start, plan.dtb.start) }
}

/// Encodes `memory` as a `reg` value in `target`'s root `#address-cells`/`#size-cells`.
fn encode_memory(target: &Fdt<'_>, memory: &[Range], out: &mut [u8]) -> Result<usize, Fail> {
    let root = target.root()?;
    let (ac, sc) = (root.address_cells()? as usize, root.size_cells()? as usize);
    if !(1..=2).contains(&ac) || !(1..=2).contains(&sc) {
        return Err(Fail::Fdt(lucina_fdt::Error::BadCells));
    }
    let mut n = 0;
    for r in memory {
        for (value, cells) in [(r.start, ac), (r.size(), sc)] {
            if cells == 1 && value > u64::from(u32::MAX) {
                return Err(Fail::Fdt(lucina_fdt::Error::BadCells));
            }
            let bytes = value.to_be_bytes();
            let part = &bytes[8 - cells * 4..];
            out.get_mut(n..n + part.len())
                .ok_or(Fail::TooManyRegions)?
                .copy_from_slice(part);
            n += part.len();
        }
    }
    Ok(n)
}
