//! The status band on the boot framebuffer (`lucina_boot::band`): the only output LucinaBoot has on surya.
//!
//! The framebuffer is the `/chosen` `simple-framebuffer` of the loader's device tree (the ABL stub tree names the
//! splash at 0x9c000000). With the MMU off the writes go straight to memory, which the display keeps scanning
//! out; no display register is touched.

use core::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use lucina_boot::band::{self, Format, Geometry, Status};
use lucina_fdt::Fdt;

// Relaxed atomics are plain LDR/STR (see `arch::psci_init`); 0 = no framebuffer.
static BASE: AtomicUsize = AtomicUsize::new(0);
static WIDTH: AtomicU32 = AtomicU32::new(0);
static HEIGHT: AtomicU32 = AtomicU32::new(0);
static STRIDE: AtomicU32 = AtomicU32::new(0);
static RGB565: AtomicU32 = AtomicU32::new(0);
static EL: AtomicU8 = AtomicU8::new(0);
static STAGES: AtomicU8 = AtomicU8::new(0);

pub fn init(fdt: &Fdt<'_>) {
    let Some(fb) = fdt.chosen().ok().flatten().and_then(|c| c.framebuffer) else {
        return;
    };
    let Some(format) = Format::parse(fb.format) else {
        return;
    };
    let Some(g) = Geometry::new(fb.width, fb.height, fb.stride, format) else {
        return;
    };
    if g.size() > fb.size {
        return;
    }
    WIDTH.store(g.width, Ordering::Relaxed);
    HEIGHT.store(g.height, Ordering::Relaxed);
    STRIDE.store(g.stride, Ordering::Relaxed);
    RGB565.store(u32::from(format == Format::Rgb565), Ordering::Relaxed);
    BASE.store(fb.base as usize, Ordering::Relaxed);
}

/// The framebuffer range, so the memory plan keeps clear of it (it is in `/chosen` anyway).
pub fn geometry() -> Option<(usize, Geometry)> {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        return None;
    }
    let format = if RGB565.load(Ordering::Relaxed) == 1 {
        Format::Rgb565
    } else {
        Format::Xrgb8888
    };
    let g = Geometry::new(
        WIDTH.load(Ordering::Relaxed),
        HEIGHT.load(Ordering::Relaxed),
        STRIDE.load(Ordering::Relaxed),
        format,
    )?;
    Some((base, g))
}

/// Records the exception level and that stage `n` (1 to 5, see `lucina_boot::band`) is reached, and repaints.
pub fn reached(el: u8, n: u8) {
    EL.store(el, Ordering::Relaxed);
    STAGES.store(n, Ordering::Relaxed);
    show(None);
}

/// Repaints with the error `code` (the band's lower half).
pub fn failed(code: u8) {
    show(Some(code));
}

fn show(error: Option<u8>) {
    let s = Status {
        el: EL.load(Ordering::Relaxed),
        stages: STAGES.load(Ordering::Relaxed),
        error,
    };
    let Some((base, g)) = geometry() else {
        return;
    };
    let (list, n) = band::paint_list(&g, &s);
    let bpp = g.format.bytes_per_pixel() as usize;
    for &(r, c) in &list[..n] {
        let px = g.format.encode(c.rgb());
        for y in r.y..r.y + r.h {
            let line = base + y as usize * g.stride as usize;
            for x in r.x..r.x + r.w {
                let at = line + x as usize * bpp;
                // SAFETY: `at` lies inside the framebuffer the device tree describes (Geometry::new and the size
                // check in `init`); it is RAM the display scans out and nothing else uses.
                unsafe {
                    if bpp == 4 {
                        core::ptr::write_volatile(at as *mut u32, px);
                    } else {
                        core::ptr::write_volatile(at as *mut u16, px as u16);
                    }
                }
            }
        }
    }
}
