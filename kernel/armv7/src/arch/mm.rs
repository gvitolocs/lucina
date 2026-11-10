//! Short-descriptor MMU (ARMv7-A without LPAE: the Cortex-A9 has no LPAE).
//!
//! Two levels: a 16 KiB L1 table of 4096 four-byte entries (1 MiB each: a section or a pointer to an L2 table)
//! and 1 KiB L2 tables of 256 four-byte entries (4 KiB small pages). `boot.S` builds the L1 table with sections;
//! this module drops the boot identity map and maps 4 KiB pages through L2 tables, which is the shape OSTD's
//! page-table cursor needs on this CPU (see the feasibility report, "Page tables").

use core::arch::asm;
use core::sync::atomic::{AtomicUsize, Ordering};

pub const PAGE_SIZE: usize = 4096;
pub const KERNEL_VBASE: usize = 0xC000_0000;
pub const LINEAR_SIZE: usize = 512 << 20;
/// Uncached pool for device DMA (the EPDC buffers): DRAM + 16 MiB, 16 MiB long (`boot.S`).
pub const DMA_OFFSET: usize = 16 << 20;
pub const DMA_SIZE: usize = 16 << 20;
/// First 1 MiB of the vmalloc window; Phase 0 maps test pages there.
pub const VMALLOC_BASE: usize = 0xE000_0000;
const IDMAP_MIB: usize = 16;

const L1_ENTRIES: usize = 4096;
const L2_ENTRIES: usize = 256;

/// L1 descriptor: pointer to an L2 table (domain 0).
const L1_COARSE: u32 = 0b01;
/// L2 small page, normal write-back write-allocate (TEX 001, C, B), AP = 01 (kernel RW).
pub const PAGE_NORMAL: u32 = 0x2 | (1 << 2) | (1 << 3) | (0b01 << 4) | (0b001 << 6);

static DRAM_BASE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "C" {
    static mut boot_l1: [u32; L1_ENTRIES];
}

/// Second-level tables for the vmalloc window (Phase 0 needs one).
#[repr(C, align(1024))]
struct L2Table([u32; L2_ENTRIES]);
static mut L2_POOL: [L2Table; 4] = [const { L2Table([0; L2_ENTRIES]) }; 4];
static L2_USED: AtomicUsize = AtomicUsize::new(0);

pub fn dram_base() -> usize {
    DRAM_BASE.load(Ordering::Relaxed)
}

pub fn phys_to_virt(pa: usize) -> usize {
    pa.wrapping_sub(dram_base()).wrapping_add(KERNEL_VBASE)
}

pub fn virt_to_phys(va: usize) -> usize {
    va.wrapping_sub(KERNEL_VBASE).wrapping_add(dram_base())
}

pub fn is_linear(pa: usize, len: usize) -> bool {
    let base = dram_base();
    pa >= base && len <= LINEAR_SIZE && pa - base <= LINEAR_SIZE - len
}

pub fn dma_pool() -> (usize, usize) {
    (dram_base() + DMA_OFFSET, DMA_SIZE)
}

fn l1() -> *mut u32 {
    core::ptr::addr_of_mut!(boot_l1) as *mut u32
}

/// Clean one data cache line to the point of coherency (the table walker reads memory, not L1).
pub fn clean_dcache_line(va: usize) {
    // SAFETY: DCCMVAC on a mapped address only writes back a line.
    unsafe { asm!("mcr p15, 0, {}, c7, c10, 1", in(reg) va, options(nostack)) };
}

fn tlb_flush_all() {
    // SAFETY: TLBIALL and BPIALL only drop cached translations and predictions.
    unsafe {
        asm!(
            "dsb",
            "mcr p15, 0, {z}, c8, c7, 0",
            "mcr p15, 0, {z}, c7, c5, 6",
            "dsb",
            "isb",
            z = in(reg) 0u32,
            options(nostack)
        )
    };
}

fn write_l1(index: usize, val: u32) {
    // SAFETY: index < 4096; the table lives in .bss and the CPU is the only writer.
    unsafe {
        let p = l1().add(index);
        core::ptr::write_volatile(p, val);
        clean_dcache_line(p as usize);
    }
}

fn read_l1(index: usize) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile(l1().add(index)) }
}

/// Translate with the MMU itself (ATS1CPR); `None` on a translation fault.
pub fn translate(va: usize) -> Option<usize> {
    let par: u32;
    // SAFETY: ATS1CPR + reading PAR has no side effects besides PAR.
    unsafe {
        asm!(
            "mcr p15, 0, {va}, c7, c8, 0",
            "isb",
            "mrc p15, 0, {par}, c7, c4, 0",
            va = in(reg) va,
            par = lateout(reg) par,
            options(nostack)
        )
    };
    if par & 1 != 0 {
        return None;
    }
    // Supersection results have bit 1 set; this kernel maps none.
    Some((par as usize & !0xfff) | (va & 0xfff))
}

/// Drop the boot identity map; from here on only the high mappings exist.
pub fn init(dram: usize) {
    DRAM_BASE.store(dram, Ordering::Relaxed);
    let first = dram >> 20;
    for i in first..first + IDMAP_MIB {
        write_l1(i, 0);
    }
    super::cpu::dsb();
    tlb_flush_all();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    Unaligned,
    /// The 1 MiB slot is already a section.
    Section,
    NoL2Table,
    AlreadyMapped,
}

/// Map one 4 KiB page `va -> pa` with the L2 attributes `attrs` (`PAGE_NORMAL`, `PAGE_DEVICE`).
pub fn map_page(va: usize, pa: usize, attrs: u32) -> Result<(), MapError> {
    if va % PAGE_SIZE != 0 || pa % PAGE_SIZE != 0 {
        return Err(MapError::Unaligned);
    }
    let i1 = va >> 20;
    let e1 = read_l1(i1);
    let l2: *mut u32 = match e1 & 0b11 {
        0b00 => {
            let n = L2_USED.fetch_add(1, Ordering::Relaxed);
            if n >= 4 {
                return Err(MapError::NoL2Table);
            }
            // SAFETY: each pool entry is handed out once (the counter above).
            let t = unsafe { core::ptr::addr_of_mut!(L2_POOL[n]) as *mut u32 };
            let t_pa = virt_to_phys(t as usize);
            write_l1(i1, (t_pa as u32 & !0x3ff) | L1_COARSE);
            t
        }
        0b01 => phys_to_virt(e1 as usize & !0x3ff) as *mut u32,
        _ => return Err(MapError::Section),
    };
    let i2 = (va >> 12) & 0xff;
    // SAFETY: i2 < 256 and l2 is a live L2 table from the pool.
    unsafe {
        let p = l2.add(i2);
        if core::ptr::read_volatile(p) & 0b10 != 0 {
            return Err(MapError::AlreadyMapped);
        }
        core::ptr::write_volatile(p, (pa as u32 & !0xfff) | attrs);
        clean_dcache_line(p as usize);
    }
    super::cpu::dsb();
    tlb_flush_all();
    Ok(())
}

/// Self-test of the two-level walk: map a page of a kernel buffer a second time in the vmalloc window, write
/// through one alias and read through the other, and check what the MMU's own translation says.
pub fn self_test() -> Result<(), &'static str> {
    #[repr(C, align(4096))]
    struct Frame([u32; 1024]);
    static mut FRAME: Frame = Frame([0; 1024]);

    let lin = core::ptr::addr_of_mut!(FRAME) as usize;
    let pa = virt_to_phys(lin);
    if translate(lin) != Some(pa) {
        return Err("linear-map translation mismatch");
    }
    if translate(VMALLOC_BASE).is_some() {
        return Err("vmalloc window mapped before the test");
    }
    map_page(VMALLOC_BASE, pa, PAGE_NORMAL).map_err(|_| "map_page failed")?;
    if translate(VMALLOC_BASE + 0x123) != Some(pa + 0x123) {
        return Err("L2 translation mismatch");
    }
    // SAFETY: both addresses map the same 4 KiB frame (VIPT A9 L1: no aliasing for 4 KiB pages).
    unsafe {
        core::ptr::write_volatile((VMALLOC_BASE + 64) as *mut u32, 0x4c75_6369); // "Luci"
        if core::ptr::read_volatile((lin + 64) as *const u32) != 0x4c75_6369 {
            return Err("alias read-back mismatch");
        }
    }
    if map_page(VMALLOC_BASE, pa, PAGE_NORMAL) != Err(MapError::AlreadyMapped) {
        return Err("double map not refused");
    }
    if map_page(KERNEL_VBASE, pa, PAGE_NORMAL) != Err(MapError::Section) {
        return Err("map over a section not refused");
    }
    Ok(())
}

/// Copy of the section attributes of the linear-map entry for `va` (for the boot log).
pub fn section_attrs(va: usize) -> u32 {
    read_l1(va >> 20) & 0xfffff
}
