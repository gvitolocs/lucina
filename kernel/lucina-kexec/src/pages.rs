// SPDX-License-Identifier: MPL-2.0

//! Staging a reload page by page (kernel patch 1055), as Linux's `kimage_load_segment` and `kimage_alloc_page` do.
//!
//! Until 1055 the kernel copied each file into one physically contiguous block (128 MiB for a 68 MB initramfs, as the
//! frame allocator rounds to a power of two), and a kernel that had run a while no longer had one: the laptop's reload
//! failed with ENOMEM after 20 minutes of uptime (docs/research/juno-kexec-enomem.md). Now every page of the image and
//! of the initramfs is a page of its own, wherever the allocator has one, and a list says where each page goes. The
//! relocation code walks that list with the MMU off, after the old kernel has stopped, and copies page by page.
//!
//! The list has the shape of Linux's `kimage` entries (`include/linux/kexec.h`): 64-bit physical addresses of pages with
//! a flag in the low bits. [`IND_DESTINATION`] sets where the next pages go, [`IND_SOURCE`] copies a page there and moves
//! on by a page, [`IND_INDIRECTION`] continues the list in another page, [`IND_DONE`] ends it. The list lives in pages of
//! [`ENTRIES_PER_PAGE`] entries whose last one links to the next page, so no allocation is larger than a page.
//!
//! The initramfs is not copied by the new kernel: it reads it where the device tree says it is. So it gets a destination
//! too, [`initrd_destination`]: RAM that the old kernel may give up, outside the new kernel's window. It need not be free
//! now, since it is written only after the old kernel has stopped; the pages staged for the reload must stay out of it.

#![allow(unexpected_cfgs)]

use core::ops::Range;

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The size of a page, the unit staged and copied.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PAGE: u64 = 4096;
/// The entry sets where the following source pages go.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IND_DESTINATION: u64 = 0x1;
/// The list continues at this page.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IND_INDIRECTION: u64 = 0x2;
/// The list ends here.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IND_DONE: u64 = 0x4;
/// This page is copied to the destination, which then moves on by a page.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const IND_SOURCE: u64 = 0x8;
/// Entries in a list page; the last one is the link to the next page.
pub const ENTRIES_PER_PAGE: usize = (PAGE / 8) as usize;

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const FLAGS: u64 = IND_DESTINATION | IND_INDIRECTION | IND_DONE | IND_SOURCE;
/// The bits of an entry below the page, where the flags live.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const LOW: u64 = PAGE - 1;

/// What the relocation code does with one entry. Verus proves the encoding round trip and, in
/// `verification/juno-kexec/rules.rs`, that a list built the way the kernel builds it copies every staged page exactly once
/// to its place and never writes a staged page.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// [`IND_DONE`]: enter the new kernel.
    Done,
    /// [`IND_DESTINATION`]: the following pages go here.
    Destination(u64),
    /// [`IND_INDIRECTION`]: the list goes on at this page.
    Indirection(u64),
    /// [`IND_SOURCE`]: copy this page to the destination, which moves on by a page.
    Copy(u64),
    /// No flag: nothing.
    Nothing,
}

/// One entry as the relocation code reads it, in its order (`bsp_boot.S`): done first, then destination, then
/// indirection, then source.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        entry & IND_DONE != 0 ==> r == Step::Done,
        entry & IND_DONE == 0 && entry & IND_DESTINATION != 0 ==> r == Step::Destination(entry & !LOW),
        entry & IND_DONE == 0 && entry & IND_DESTINATION == 0 && entry & IND_INDIRECTION != 0
            ==> r == Step::Indirection(entry & !LOW),
        entry & IND_DONE == 0 && entry & IND_DESTINATION == 0 && entry & IND_INDIRECTION == 0 && entry & IND_SOURCE != 0
            ==> r == Step::Copy(entry & !LOW),
        entry & IND_DONE == 0 && entry & IND_DESTINATION == 0 && entry & IND_INDIRECTION == 0 && entry & IND_SOURCE == 0
            ==> r == Step::Nothing,
))]
pub fn step(entry: u64) -> Step {
    let addr = entry & !LOW;
    if entry & IND_DONE != 0 {
        Step::Done
    } else if entry & IND_DESTINATION != 0 {
        Step::Destination(addr)
    } else if entry & IND_INDIRECTION != 0 {
        Step::Indirection(addr)
    } else if entry & IND_SOURCE != 0 {
        Step::Copy(addr)
    } else {
        Step::Nothing
    }
}

/// The entry `flag` of the page-aligned `page`. Verus proves that [`step`] reads it back as that flag's step of `page`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        page & LOW == 0,
        flag == IND_DESTINATION || flag == IND_INDIRECTION || flag == IND_SOURCE,
    ensures
        r == page | flag,
))]
pub fn encode(page: u64, flag: u64) -> u64 {
    page | flag
}
/// The alignment of the initramfs destination: the new kernel maps memory in 2 MiB blocks.
const BLOCK_2M: u64 = 1 << 21;

/// One page of the entry list, filled in memory and then copied to its staged page.
#[cfg_attr(verus_keep_ghost, verifier::external)]
#[derive(Clone)]
pub struct ListPage {
    entries: [u64; ENTRIES_PER_PAGE],
    used: usize,
}

#[cfg_attr(verus_keep_ghost, verifier::external)]
impl Default for ListPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(verus_keep_ghost, verifier::external)]
impl ListPage {
    /// An empty page: every entry 0, which the walk reads as "nothing" if it ever got there.
    pub const fn new() -> Self {
        Self {
            entries: [0; ENTRIES_PER_PAGE],
            used: 0,
        }
    }

    /// Adds `entry` and returns true, or returns false when only the link slot is left: then [`ListPage::link`] this
    /// page to a new one and push the entry there.
    #[must_use]
    pub fn push(&mut self, entry: u64) -> bool {
        if self.used == ENTRIES_PER_PAGE - 1 {
            return false;
        }
        self.entries[self.used] = entry;
        self.used += 1;
        true
    }

    /// Ends this page with a link to the list page at `next`, which must be page-aligned.
    ///
    /// # Panics
    /// Panics if `next` is not page-aligned.
    pub fn link(&mut self, next: u64) {
        assert_eq!(next % PAGE, 0, "a list page is page-aligned");
        self.entries[self.used] = next | IND_INDIRECTION;
        self.used += 1;
    }

    /// The page as the relocation code reads it.
    pub fn entries(&self) -> &[u64; ENTRIES_PER_PAGE] {
        &self.entries
    }
}

/// An entry that sets the destination to `page`.
///
/// # Panics
/// Panics if `page` is not page-aligned.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn destination(page: u64) -> u64 {
    assert_eq!(page % PAGE, 0, "a destination is page-aligned");
    encode(page, IND_DESTINATION)
}

/// An entry that copies the staged page at `page`.
///
/// # Panics
/// Panics if `page` is not page-aligned.
#[cfg_attr(verus_keep_ghost, verifier::external_body)]
pub fn source(page: u64) -> u64 {
    assert_eq!(page % PAGE, 0, "a staged page is page-aligned");
    encode(page, IND_SOURCE)
}

/// The list does not end where it should.
#[cfg_attr(verus_keep_ghost, verifier::external)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkError {
    /// More entries than `limit` without an [`IND_DONE`].
    NoEnd,
    /// A source entry before any destination.
    NoDestination,
}

/// Walks a list from the page at `head` as the relocation code does, calling `copy(destination, source)` for every page
/// it would copy. `read` gives the entries of a list page. Stops with an error after `limit` entries without an end.
#[cfg_attr(verus_keep_ghost, verifier::external)]
pub fn walk_entries(
    head: u64,
    mut read: impl FnMut(u64) -> [u64; ENTRIES_PER_PAGE],
    mut copy: impl FnMut(u64, u64),
    limit: usize,
) -> Result<(), WalkError> {
    let mut page = read(head);
    let mut at = 0;
    let mut dest = None;
    for _ in 0..limit {
        let entry = page[at];
        at += 1;
        // The relocation code's order: done, destination, indirection, source (`step`).
        match step(entry) {
            Step::Done => return Ok(()),
            Step::Destination(addr) => dest = Some(addr),
            Step::Indirection(addr) => {
                page = read(addr);
                at = 0;
            }
            Step::Copy(addr) => {
                let d = dest.ok_or(WalkError::NoDestination)?;
                copy(d, addr);
                dest = Some(d + PAGE);
            }
            Step::Nothing => {}
        }
        // An entry with none of the flags is nothing. The relocation code reads past the end of a page only after an
        // indirection, so `at` never passes the last slot here either.
        if at == ENTRIES_PER_PAGE {
            return Err(WalkError::NoEnd);
        }
    }
    Err(WalkError::NoEnd)
}

/// Whether `entry` carries any flag the list knows.
#[cfg_attr(verus_keep_ghost, verifier::external)]
pub fn is_entry(entry: u64) -> bool {
    entry & FLAGS != 0
}

/// Where the initramfs goes: `len` bytes (rounded up to a page) on a 2 MiB boundary, inside RAM the old kernel may give
/// up (`regions` with `true`; adjacent ones join), clear of `window` and of every `false` region. The lowest place at or
/// above the window's end, else the lowest below its start; `None` when nothing fits or `len` is 0.
#[cfg_attr(verus_keep_ghost, verifier::external)]
pub fn initrd_destination(regions: &[(Range<u64>, bool)], window: &Range<u64>, len: u64) -> Option<Range<u64>> {
    if len == 0 {
        return None;
    }
    let size = len.checked_next_multiple_of(PAGE)?;
    let overlaps = |a: &Range<u64>, b: &Range<u64>| a.start < b.end && b.start < a.end;
    let allowed = |r: &Range<u64>| {
        !overlaps(r, window)
            && regions.iter().all(|(reg, ok)| *ok || !overlaps(r, reg))
            && covered(regions, r)
    };
    // Candidates: each allowed region's start and the window's end, rounded up to 2 MiB. A lowest fit starts at one of
    // them, since anything below a candidate is outside every run or inside the window.
    let mut best_above: Option<u64> = None;
    let mut best_below: Option<u64> = None;
    let starts = regions
        .iter()
        .filter(|(_, ok)| *ok)
        .map(|(r, _)| r.start)
        .chain(core::iter::once(window.end));
    for start in starts {
        let Some(start) = start.checked_next_multiple_of(BLOCK_2M) else {
            continue;
        };
        let Some(end) = start.checked_add(size) else {
            continue;
        };
        let range = start..end;
        if !allowed(&range) {
            continue;
        }
        if start >= window.end {
            best_above = Some(best_above.map_or(start, |b| b.min(start)));
        } else if end <= window.start {
            best_below = Some(best_below.map_or(start, |b| b.min(start)));
        }
    }
    best_above.or(best_below).map(|start| start..start + size)
}

/// Whether `range` lies wholly inside the union of the `true` regions.
#[cfg_attr(verus_keep_ghost, verifier::external)]
fn covered(regions: &[(Range<u64>, bool)], range: &Range<u64>) -> bool {
    let mut at = range.start;
    // At most one pass per region: each step moves `at` to the end of a region that holds it.
    for _ in 0..=regions.len() {
        if at >= range.end {
            return true;
        }
        match regions.iter().find(|(r, ok)| *ok && r.start <= at && at < r.end) {
            Some((r, _)) => at = r.end,
            None => return false,
        }
    }
    at >= range.end
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, vec, vec::Vec};

    use super::*;

    const MIB: u64 = 1 << 20;

    /// Builds a list as the kernel does, in pages taken from `next_page`, and returns its head and the pages.
    fn build(entries: &[u64], first_page: u64) -> (u64, BTreeMap<u64, [u64; ENTRIES_PER_PAGE]>) {
        let mut pages = BTreeMap::new();
        let mut next_free = first_page;
        let mut take = || {
            let p = next_free;
            next_free += PAGE;
            p
        };
        let head = take();
        let mut at = head;
        let mut page = ListPage::new();
        for &e in entries.iter().chain(core::iter::once(&IND_DONE)) {
            if !page.push(e) {
                let next = take();
                page.link(next);
                pages.insert(at, *page.entries());
                at = next;
                page = ListPage::new();
                assert!(page.push(e));
            }
        }
        pages.insert(at, *page.entries());
        (head, pages)
    }

    fn walk(head: u64, pages: &BTreeMap<u64, [u64; ENTRIES_PER_PAGE]>) -> Result<Vec<(u64, u64)>, WalkError> {
        let mut copies = Vec::new();
        walk_entries(head, |p| pages[&p], |d, s| copies.push((d, s)), 1 << 20)?;
        Ok(copies)
    }

    #[test]
    fn a_list_gives_back_every_page_in_order() {
        for n in [0usize, 1, 509, 510, 511, 512, 1000, 1533, 3000] {
            let sources: Vec<u64> = (0..n as u64).map(|i| 0x9000_0000 + i * 3 * PAGE).collect();
            let entries: Vec<u64> = core::iter::once(destination(0xa000_0000))
                .chain(sources.iter().map(|&s| source(s)))
                .collect();
            let (head, pages) = build(&entries, 0x8000_0000);
            let copies = walk(head, &pages).unwrap();
            let want: Vec<(u64, u64)> = sources
                .iter()
                .enumerate()
                .map(|(i, &s)| (0xa000_0000 + i as u64 * PAGE, s))
                .collect();
            assert_eq!(copies, want, "n = {n}");
            // Pages needed: 511 entries per page (the last slot links), the destination and the end counted too.
            assert_eq!(pages.len(), (n + 2).div_ceil(ENTRIES_PER_PAGE - 1).max(1), "n = {n}");
        }
    }

    #[test]
    fn two_destinations_in_one_list() {
        let entries = [
            destination(0xa000_0000),
            source(0x1000),
            source(0x5000),
            destination(0xc000_0000),
            source(0x3000),
        ];
        let (head, pages) = build(&entries, 0x8000_0000);
        assert_eq!(
            walk(head, &pages).unwrap(),
            [(0xa000_0000, 0x1000), (0xa000_1000, 0x5000), (0xc000_0000, 0x3000)]
        );
    }

    #[test]
    fn bad_lists_are_caught() {
        let mut page = [0u64; ENTRIES_PER_PAGE];
        page[0] = source(0x1000);
        page[1] = IND_DONE;
        assert_eq!(walk_entries(0, |_| page, |_, _| (), 100), Err(WalkError::NoDestination));
        let empty = [0u64; ENTRIES_PER_PAGE];
        assert_eq!(walk_entries(0, |_| empty, |_, _| (), 100_000), Err(WalkError::NoEnd));
        assert!(!is_entry(0) && is_entry(IND_DONE) && is_entry(source(0x1000)));
    }

    #[test]
    #[should_panic]
    fn an_unaligned_source_is_refused() {
        let _ = source(0x1001);
    }

    #[test]
    fn the_destination_follows_the_window() {
        let regions = [(0x8000_0000..0x1_0000_0000, true)];
        let window = 0xa000_0000..0xa1a0_0000;
        assert_eq!(initrd_destination(&regions, &window, 68 * MIB), Some(0xa1a0_0000..0xa1a0_0000 + 68 * MIB));
        // Rounded up to a page and placed on 2 MiB.
        let window = 0xa000_0000..0xa1a1_2345;
        assert_eq!(initrd_destination(&regions, &window, 5), Some(0xa1c0_0000..0xa1c0_1000));
    }

    #[test]
    fn a_reserved_hole_is_skipped_and_runs_join() {
        let window = 0xa000_0000..0xa200_0000;
        let regions = [
            (0x8000_0000..0xa400_0000, true),
            (0xa400_0000..0xa410_0000, false), // pstore, say
            (0xa410_0000..0xb000_0000, true),
            (0xb000_0000..0xc000_0000, true), // joins the one before
        ];
        // 64 MiB does not fit between the window and the hole (32 MiB): the next 2 MiB start after the hole.
        let got = initrd_destination(&regions, &window, 64 * MIB).unwrap();
        assert_eq!(got.start, 0xa420_0000);
        // Across the join of the last two regions.
        let got = initrd_destination(&regions, &window, 400 * MIB).unwrap();
        assert_eq!(got.start, 0xa420_0000);
        assert!(got.end > 0xb000_0000);
    }

    #[test]
    fn below_the_window_when_nothing_is_above() {
        let regions = [(0x8000_0000..0xa000_0000, true), (0xa000_0000..0xa200_0000, true)];
        let window = 0xa000_0000..0xa200_0000;
        assert_eq!(initrd_destination(&regions, &window, 3 * MIB), Some(0x8000_0000..0x8030_0000));
        assert_eq!(initrd_destination(&regions, &window, 0), None);
        assert_eq!(initrd_destination(&regions, &window, 600 * MIB), None);
    }

    #[test]
    fn a_destination_never_touches_the_window_or_a_reserved_range() {
        // Every layout of a few regions on a coarse grid, every window and length on it.
        let grid = |i: u64| 0x8000_0000 + i * 4 * MIB;
        let mut checked = 0;
        for mask in 0u32..(1 << 6) {
            let regions: Vec<(Range<u64>, bool)> =
                (0..6).map(|i| (grid(i)..grid(i + 1), mask & (1 << i) != 0)).collect();
            for ws in 0..6 {
                for we in ws + 1..=6 {
                    let window = grid(ws)..grid(we);
                    for len in [1, PAGE, 3 * MIB, 4 * MIB, 9 * MIB] {
                        if let Some(d) = initrd_destination(&regions, &window, len) {
                            checked += 1;
                            assert!(d.end - d.start >= len && d.start % BLOCK_2M == 0);
                            assert!(d.end <= window.start || window.end <= d.start);
                            for (r, ok) in &regions {
                                assert!(*ok || d.end <= r.start || r.end <= d.start, "{d:x?} in {r:x?}");
                            }
                            assert!(covered(&regions, &d));
                        }
                    }
                }
            }
        }
        assert!(checked > 100);
        let _ = vec![0u8; 1];
    }
}
