//! The decisions LucinaBoot makes, kept apart from its assembly so they run and are tested on the host.
//!
//! LucinaBoot (decision D-009, `boot/lucinaboot/`) is what ABL starts as the "kernel" of a surya boot image. It
//! finds a payload package in the ramdisk ABL hands over, chooses the panel's device tree, places the kernel at
//! the address the arm64 boot protocol gives it, fixes up the device tree and jumps.
//!
//! * [`image`]: the 64-byte arm64 `Image` header (Linux `Documentation/arch/arm64/booting.rst`).
//! * [`pkg`]: the payload package format: one kernel `Image`, an optional initramfs and named device trees.
//! * [`panel`]: the panel from ABL's `msm_drm.dsi_display0=` and the device tree that matches it.
//! * [`plan`]: where everything goes in RAM: kernel, device tree, and what has to move out of the way first.
//! * [`band`]: the status band LucinaBoot paints on the boot framebuffer, its only output on surya.
//! * [`menu`]: what LucinaLoader (the UEFI boot menu that replaces GRUB for Linux) decides: menu file, keys, theme.
//! * [`uefi`]: what LucinaUEFI (the UEFI loader for laptops) decides: config, memory map, framebuffer, tree edits.
//!
//! The library is `no_std`, allocation-free and `unsafe`-free. With the default `std` feature, [`build`] packs
//! packages and turns ELF files into flat `Image` files for the `lucina-boot` CLI, and [`surya`] writes the
//! device tree appended to LucinaBoot in the surya boot image.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

pub mod band;
pub mod image;
pub mod menu;
pub mod panel;
pub mod pkg;
pub mod plan;
pub mod rng_marker;
pub mod trial;
pub mod uefi;

#[cfg(feature = "std")]
pub mod build;
#[cfg(feature = "std")]
pub mod surya;

/// An address range `[start, end)` in physical memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

impl Range {
    /// `[start, start + size)`, or `None` if it does not fit a u64.
    pub fn new(start: u64, size: u64) -> Option<Range> {
        Some(Range {
            start,
            end: start.checked_add(size)?,
        })
    }

    pub fn size(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// True when both ranges are non-empty and share at least one byte.
    pub fn overlaps(&self, other: &Range) -> bool {
        !self.is_empty() && !other.is_empty() && self.start < other.end && other.start < self.end
    }

    /// True when `other` lies entirely inside `self`.
    pub fn contains(&self, other: &Range) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

impl core::fmt::Display for Range {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:#x}-{:#x}", self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::Range;

    #[test]
    fn range_overlap_and_contains() {
        let a = Range { start: 10, end: 20 };
        assert!(a.overlaps(&Range { start: 19, end: 30 }));
        assert!(!a.overlaps(&Range { start: 20, end: 30 }));
        assert!(
            !a.overlaps(&Range { start: 15, end: 15 }),
            "empty never overlaps"
        );
        assert!(a.contains(&Range { start: 10, end: 20 }));
        assert!(!a.contains(&Range { start: 9, end: 20 }));
        assert_eq!(Range::new(u64::MAX, 1), None);
        assert_eq!(Range::new(5, 3).unwrap().size(), 3);
    }
}
