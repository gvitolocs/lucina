//! The checks on what `kexec_load` is given, as Linux's `sanity_check_segment_list`, for a kernel that copies the segments over
//! itself at the jump.

use alloc::vec::Vec;
use core::ops::Range;

pub const PAGE: u64 = 4096;
/// Linux's `KEXEC_SEGMENT_MAX`.
pub const MAX_SEGMENTS: usize = 16;

/// A segment as user space gives it: the bytes at `buf..buf + bufsz` go to `mem..mem + memsz`, the rest zeros.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawSegment {
    pub buf: u64,
    pub bufsz: u64,
    pub mem: u64,
    pub memsz: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentError {
    TooMany,
    /// The source does not fit the destination.
    TooLong(usize),
    /// A destination is not whole pages.
    NotPages(usize),
    Overlap(usize, usize),
    /// A destination is not RAM that a new kernel may have.
    NotRam(usize),
    /// The entry point is in no segment.
    EntryNotLoaded,
}

/// Checks the list, with `ram` being what the machine has for a new kernel. Returns the destinations.
pub fn check_segments(
    segments: &[RawSegment],
    entry: u64,
    ram: &[Range<u64>],
) -> Result<Vec<Range<u64>>, SegmentError> {
    if segments.len() > MAX_SEGMENTS {
        return Err(SegmentError::TooMany);
    }
    let mut destinations = Vec::with_capacity(segments.len());
    for (i, s) in segments.iter().enumerate() {
        if s.bufsz > s.memsz {
            return Err(SegmentError::TooLong(i));
        }
        let end = s.mem.checked_add(s.memsz).ok_or(SegmentError::NotRam(i))?;
        if s.memsz == 0 || !s.mem.is_multiple_of(PAGE) || !s.memsz.is_multiple_of(PAGE) {
            return Err(SegmentError::NotPages(i));
        }
        if !ram.iter().any(|r| r.start <= s.mem && end <= r.end) {
            return Err(SegmentError::NotRam(i));
        }
        for (j, other) in destinations.iter().enumerate() {
            let other: &Range<u64> = other;
            if s.mem < other.end && other.start < end {
                return Err(SegmentError::Overlap(j, i));
            }
        }
        destinations.push(s.mem..end);
    }
    if !destinations.iter().any(|d| d.contains(&entry)) {
        return Err(SegmentError::EntryNotLoaded);
    }
    Ok(destinations)
}

/// Whether nothing of `range` is in `destinations`: where the sources and the code that copies them can be.
pub fn clear_of_destinations(range: &Range<u64>, destinations: &[Range<u64>]) -> bool {
    destinations
        .iter()
        .all(|d| range.end <= d.start || d.end <= range.start)
}

/// The start of the first segment whose bytes begin with a device tree's magic number, which is how Linux on ARM finds what
/// to pass in `r2` (`machine_kexec_prepare`). `magic[i]` holds the first four bytes of segment `i`, if it has them.
pub fn device_tree_segment(segments: &[RawSegment], magic: &[Option<[u8; 4]>]) -> Option<u64> {
    segments
        .iter()
        .zip(magic)
        .find(|(_, m)| **m == Some([0xd0, 0x0d, 0xfe, 0xed]))
        .map(|(s, _)| s.mem)
}

#[cfg(test)]
mod segment_tests {
    use alloc::vec;

    use super::*;

    const RAM: Range<u64> = 0x8000_0000..0xa000_0000;

    fn seg(mem: u64, bufsz: u64, memsz: u64) -> RawSegment {
        RawSegment {
            buf: 0x1000,
            bufsz,
            mem,
            memsz,
        }
    }

    fn check(segments: &[RawSegment], entry: u64) -> Result<usize, SegmentError> {
        check_segments(segments, entry, &[RAM]).map(|d| d.len())
    }

    #[test]
    fn what_the_tool_stages_passes() {
        // A zImage, an initramfs below the device tree's slot, and the device tree.
        let segments = [
            seg(0x8000_8000, 0x10_0001, 0x10_1000),
            seg(0x87f0_0000, 0x8_0000, 0x8_0000),
            seg(0x8800_0000, 0x1000, 0x1000),
        ];
        assert_eq!(check(&segments, 0x8000_8000), Ok(3));
    }

    #[test]
    fn the_entry_must_be_loaded() {
        let segments = [seg(0x8000_8000, 0x1000, 0x1000)];
        assert_eq!(
            check(&segments, 0x8000_9fff),
            Err(SegmentError::EntryNotLoaded)
        );
        assert_eq!(check(&segments, 0x8000_8fff), Ok(1));
        assert_eq!(check(&[], 0x8000_8000), Err(SegmentError::EntryNotLoaded));
    }

    #[test]
    fn nonsense_is_refused() {
        // Longer than its place, not pages, empty, overlapping.
        assert_eq!(
            check(&[seg(0x8000_8000, 0x2000, 0x1000)], 0x8000_8000),
            Err(SegmentError::TooLong(0))
        );
        assert_eq!(
            check(&[seg(0x8000_8100, 0x100, 0x1000)], 0x8000_8100),
            Err(SegmentError::NotPages(0))
        );
        assert_eq!(
            check(&[seg(0x8000_8000, 0x100, 0x100)], 0x8000_8000),
            Err(SegmentError::NotPages(0))
        );
        assert_eq!(
            check(&[seg(0x8000_8000, 0, 0)], 0x8000_8000),
            Err(SegmentError::NotPages(0))
        );
        assert_eq!(
            check(
                &[
                    seg(0x8000_8000, 0x2000, 0x2000),
                    seg(0x8000_9000, 0x1000, 0x1000)
                ],
                0x8000_8000
            ),
            Err(SegmentError::Overlap(0, 1))
        );
        assert_eq!(
            check(
                &[
                    seg(0x8000_9000, 0x1000, 0x1000),
                    seg(0x8000_8000, 0x2000, 0x2000)
                ],
                0x8000_8000
            ),
            Err(SegmentError::Overlap(0, 1))
        );
        let many = vec![seg(0x8000_8000, 0x1000, 0x1000); MAX_SEGMENTS + 1];
        assert_eq!(check(&many, 0x8000_8000), Err(SegmentError::TooMany));
    }

    #[test]
    fn only_ram_can_be_written() {
        assert_eq!(
            check(&[seg(0x7fff_f000, 0x1000, 0x1000)], 0x7fff_f000),
            Err(SegmentError::NotRam(0))
        );
        assert_eq!(
            check(&[seg(0x9fff_f000, 0x1000, 0x2000)], 0x9fff_f000),
            Err(SegmentError::NotRam(0))
        );
        assert_eq!(
            check(&[seg(0x9fff_f000, 0x1000, 0x1000)], 0x9fff_f000),
            Ok(1)
        );
        // No wrapping past the top of the addresses.
        assert_eq!(
            check(&[seg(0xffff_ffff_ffff_f000, 0x1000, 0x2000)], 0x8000_8000),
            Err(SegmentError::NotRam(0))
        );
        // A hole in what is allowed is not allowed, even between two pieces.
        let ram = [0x8000_0000..0x8001_0000, 0x8002_0000..0x8003_0000];
        assert_eq!(
            check_segments(&[seg(0x8000_f000, 0x1000, 0x2000)], 0x8000_f000, &ram),
            Err(SegmentError::NotRam(0))
        );
    }

    #[test]
    fn sources_keep_clear() {
        let destinations = [0x1000..0x2000, 0x4000..0x5000];
        assert!(clear_of_destinations(&(0x2000..0x4000), &destinations));
        assert!(!clear_of_destinations(&(0x1fff..0x2001), &destinations));
        assert!(!clear_of_destinations(&(0x0..0x1001), &destinations));
        assert!(!clear_of_destinations(&(0x4800..0x4900), &destinations));
    }

    #[test]
    fn the_device_tree_is_found_by_its_magic() {
        let segments = [seg(0x8000_8000, 4, 0x1000), seg(0x8800_0000, 4, 0x1000)];
        let magic = [Some([1, 2, 3, 4]), Some([0xd0, 0x0d, 0xfe, 0xed])];
        assert_eq!(device_tree_segment(&segments, &magic), Some(0x8800_0000));
        assert_eq!(device_tree_segment(&segments, &[None, None]), None);
    }
}
