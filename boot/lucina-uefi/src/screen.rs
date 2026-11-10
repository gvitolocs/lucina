//! The status band (`lucina_boot::band`, the layout LucinaBoot uses on surya) painted on the GOP framebuffer.
//!
//! Before ExitBootServices the firmware console shows LucinaUEFI's text; after it the band is the only output
//! left until the kernel's own framebuffer console starts: it shows the exception level the loader was entered
//! at and that the jump is next, or an error code.

use lucina_boot::{
    Range,
    band::{self, Format, Geometry, Status},
    uefi::Fb,
};

use crate::arch;

pub struct Screen {
    base: u64,
    g: Geometry,
    swapped: bool,
}

impl Screen {
    pub fn new(fb: &Fb) -> Option<Screen> {
        // The band is drawn inside the visible width; the stride covers any padding.
        let g = Geometry::new(fb.width, fb.height, fb.stride, Format::Xrgb8888)?;
        Some(Screen {
            base: fb.base,
            g,
            swapped: fb.swapped,
        })
    }

    /// The whole screen in one colour (0xRRGGBB).
    pub fn fill(&self, rgb: u32) {
        let rgb = if self.swapped {
            (rgb & 0x00ff00) | ((rgb >> 16) & 0xff) | ((rgb & 0xff) << 16)
        } else {
            rgb
        };
        let px = self.g.format.encode(rgb);
        for y in 0..self.g.height {
            let line = self.base + u64::from(y) * u64::from(self.g.stride);
            for x in 0..self.g.width {
                let at = line + u64::from(x) * 4;
                // SAFETY: as in `show`: inside the framebuffer GOP described, identity mapped.
                unsafe { core::ptr::write_volatile(at as usize as *mut u32, px) };
            }
        }
        if let Some(r) = Range::new(self.base, self.g.size()) {
            arch::dcache_clean(r);
        }
    }

    pub fn show(&self, s: &Status) {
        let (list, n) = band::paint_list(&self.g, s);
        for &(r, c) in &list[..n] {
            let mut rgb = c.rgb();
            if self.swapped {
                rgb = (rgb & 0x00ff00) | ((rgb >> 16) & 0xff) | ((rgb & 0xff) << 16);
            }
            let px = self.g.format.encode(rgb);
            for y in r.y..r.y + r.h {
                let line = self.base + u64::from(y) * u64::from(self.g.stride);
                for x in r.x..r.x + r.w {
                    let at = line + u64::from(x) * 4;
                    // SAFETY: `at` lies inside the framebuffer GOP described (Geometry::new and the size check
                    // in `lucina_boot::uefi::framebuffer`); it is memory the display scans out, identity mapped.
                    unsafe { core::ptr::write_volatile(at as usize as *mut u32, px) };
                }
            }
        }
        // The display reads memory, not the cache.
        if let Some(r) = Range::new(self.base, self.g.size()) {
            arch::dcache_clean(r);
        }
    }
}
