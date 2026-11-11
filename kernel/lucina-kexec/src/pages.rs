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

use alloc::vec::Vec;
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
#[cfg_attr(verus_keep_ghost, verus_verify)]
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

#[cfg(verus_keep_ghost)]
verus! {

/// The step [`step`] reads from `entry`, as a spec function: its `ensures` say exactly this, case by case.
pub open spec fn step_spec(entry: u64) -> Step {
    if entry & IND_DONE != 0 {
        Step::Done
    } else if entry & IND_DESTINATION != 0 {
        Step::Destination(entry & !LOW)
    } else if entry & IND_INDIRECTION != 0 {
        Step::Indirection(entry & !LOW)
    } else if entry & IND_SOURCE != 0 {
        Step::Copy(entry & !LOW)
    } else {
        Step::Nothing
    }
}

/// The bit facts behind the encoding, in one place: a page with no bit below the page and one flag keeps the page in
/// its high bits and that flag, and no other, in its low bits.
pub proof fn lemma_flag_bits(page: u64)
    requires
        page & LOW == 0,
    ensures
        (page | IND_DESTINATION) & IND_DONE == 0,
        (page | IND_DESTINATION) & IND_DESTINATION != 0,
        (page | IND_DESTINATION) & !LOW == page,
        (page | IND_SOURCE) & IND_DONE == 0,
        (page | IND_SOURCE) & IND_DESTINATION == 0,
        (page | IND_SOURCE) & IND_INDIRECTION == 0,
        (page | IND_SOURCE) & IND_SOURCE != 0,
        (page | IND_SOURCE) & !LOW == page,
        (page | IND_INDIRECTION) & IND_DONE == 0,
        (page | IND_INDIRECTION) & IND_DESTINATION == 0,
        (page | IND_INDIRECTION) & IND_INDIRECTION != 0,
        (page | IND_INDIRECTION) & !LOW == page,
        IND_DONE & IND_DONE != 0,
{
    assert(
        (page | 1u64) & 4u64 == 0u64 && (page | 1u64) & 1u64 != 0u64 && (page | 1u64) & !4095u64 == page
        && (page | 8u64) & 4u64 == 0u64 && (page | 8u64) & 1u64 == 0u64 && (page | 8u64) & 2u64 == 0u64
        && (page | 8u64) & 8u64 != 0u64 && (page | 8u64) & !4095u64 == page
        && (page | 2u64) & 4u64 == 0u64 && (page | 2u64) & 1u64 == 0u64 && (page | 2u64) & 2u64 != 0u64
        && (page | 2u64) & !4095u64 == page && 4u64 & 4u64 != 0u64
    ) by (bit_vector) requires page & 4095u64 == 0u64;
}

/// A page multiple has no bit below the page, so with one flag it reads back as that flag's step of the page.
pub proof fn lemma_page_flag(page: u64)
    requires
        page % PAGE == 0,
    ensures
        page & LOW == 0,
        step_spec(page | IND_DESTINATION) == Step::Destination(page),
        step_spec(page | IND_SOURCE) == Step::Copy(page),
        step_spec(page | IND_INDIRECTION) == Step::Indirection(page),
{
    assert(page & 4095u64 == 0u64) by (bit_vector) requires page % 4096u64 == 0u64;
    lemma_flag_bits(page);
}

} // verus!
/// The alignment of the initramfs destination: the new kernel maps memory in 2 MiB blocks.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const BLOCK_2M: u64 = 1 << 21;

/// One page of the entry list, filled in memory and then copied to its staged page.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone)]
pub struct ListPage {
    entries: [u64; ENTRIES_PER_PAGE],
    used: usize,
}

#[cfg(verus_keep_ghost)]
verus! {

impl ListPage {
    /// The entries written so far, in order.
    pub closed spec fn view(&self) -> Seq<u64> {
        self.entries@.take(self.used as int)
    }

    /// At most a page of entries, and every slot after them still 0.
    pub closed spec fn wf(&self) -> bool {
        &&& self.used <= ENTRIES_PER_PAGE
        &&& forall|i: int| self.used <= i < ENTRIES_PER_PAGE ==> #[trigger] self.entries@[i] == 0
    }

    pub proof fn lemma_wf(&self)
        requires
            self.wf(),
        ensures
            self.view().len() <= ENTRIES_PER_PAGE,
    {
    }
}

} // verus!

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl Default for ListPage {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
impl ListPage {
    /// An empty page: every entry 0, which the walk reads as "nothing" if it ever got there.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        ensures
            r.wf(),
            r.view() == Seq::<u64>::empty(),
    ))]
    pub const fn new() -> Self {
        Self {
            entries: [0; ENTRIES_PER_PAGE],
            used: 0,
        }
    }

    /// Adds `entry` and returns true, or returns false when only the link slot is left: then [`ListPage::link`] this
    /// page to a new one and push the entry there.
    #[must_use]
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires
            old(self).wf(),
            old(self).view().len() < ENTRIES_PER_PAGE,
        ensures
            final(self).wf(),
            r == (old(self).view().len() < ENTRIES_PER_PAGE - 1),
            r ==> final(self).view() == old(self).view().push(entry),
            !r ==> final(self).view() == old(self).view(),
    ))]
    pub fn push(&mut self, entry: u64) -> bool {
        if self.used == ENTRIES_PER_PAGE - 1 {
            return false;
        }
        #[cfg(verus_keep_ghost)]
        proof_decl! { let ghost before = self.entries@; }
        self.entries[self.used] = entry;
        self.used += 1;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(self.entries@.take(self.used as int) =~= before.take(self.used as int - 1).push(entry));
        }
        true
    }

    /// Ends this page with a link to the list page at `next`, which must be page-aligned.
    ///
    /// # Panics
    /// Panics if `next` is not page-aligned.
    #[cfg_attr(verus_keep_ghost, verus_spec(
        requires
            old(self).wf(),
            old(self).view().len() < ENTRIES_PER_PAGE,
            next % PAGE == 0,
        ensures
            final(self).wf(),
            final(self).view() == old(self).view().push(next | IND_INDIRECTION),
    ))]
    pub fn link(&mut self, next: u64) {
        assert_eq!(next % PAGE, 0, "a list page is page-aligned");
        #[cfg(verus_keep_ghost)]
        proof_decl! { let ghost before = self.entries@; }
        self.entries[self.used] = next | IND_INDIRECTION;
        self.used += 1;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(self.entries@.take(self.used as int) =~= before.take(self.used as int - 1).push(next | IND_INDIRECTION));
        }
    }

    /// The page as the relocation code reads it.
    #[cfg_attr(verus_keep_ghost, verus_spec(r =>
        requires
            self.wf(),
        ensures
            r@.len() == ENTRIES_PER_PAGE,
            self.view().len() <= ENTRIES_PER_PAGE,
            forall|i: int| 0 <= i < self.view().len() ==> #[trigger] r@[i] == self.view()[i],
            forall|i: int| self.view().len() <= i < ENTRIES_PER_PAGE ==> #[trigger] r@[i] == 0,
    ))]
    pub fn entries(&self) -> &[u64; ENTRIES_PER_PAGE] {
        &self.entries
    }
}

/// An entry that sets the destination to `page`. Verus proves that [`step`] reads it back as `Destination(page)`; the
/// alignment it needs is checked here at run time, and a verified caller must prove it.
///
/// # Panics
/// Panics if `page` is not page-aligned.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        page % PAGE == 0,
    ensures
        r == page | IND_DESTINATION,
        step_spec(r) == Step::Destination(page),
))]
pub fn destination(page: u64) -> u64 {
    assert_eq!(page % PAGE, 0, "a destination is page-aligned");
    #[cfg(verus_keep_ghost)]
    proof! { lemma_page_flag(page); }
    encode(page, IND_DESTINATION)
}

/// An entry that copies the staged page at `page`. Verus proves that [`step`] reads it back as `Copy(page)`; the
/// alignment it needs is checked here at run time, and a verified caller must prove it.
///
/// # Panics
/// Panics if `page` is not page-aligned.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        page % PAGE == 0,
    ensures
        r == page | IND_SOURCE,
        step_spec(r) == Step::Copy(page),
))]
pub fn source(page: u64) -> u64 {
    assert_eq!(page % PAGE, 0, "a staged page is page-aligned");
    #[cfg(verus_keep_ghost)]
    proof! { lemma_page_flag(page); }
    encode(page, IND_SOURCE)
}

/// The list does not end where it should.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkError {
    /// More entries than `limit` without an [`IND_DONE`].
    NoEnd,
    /// A source entry before any destination.
    NoDestination,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The list pages as the walk reads them: page `p` holds `mem(p)`.
pub type ListMemory = spec_fn(u64) -> Seq<u64>;

/// What [`walk_entries`] does from slot `at` of the list page `page`, with destination `dest` and `fuel` entries left
/// to read, when list page `p` holds `mem(p)`: the copies it makes, in order, and how it ends. This is the shipped
/// function's loop written as a recursion, step for step (`step`'s order; a page's last slot is read only through
/// an indirection).
pub open spec fn walk_from(mem: ListMemory, page: Seq<u64>, at: int, dest: Option<u64>, fuel: nat)
    -> (Seq<(u64, u64)>, Result<(), WalkError>)
    decreases fuel,
{
    if fuel == 0 {
        (Seq::empty(), Err(WalkError::NoEnd))
    } else {
        let left = (fuel - 1) as nat;
        match step_spec(page[at]) {
            Step::Done => (Seq::empty(), Ok(())),
            Step::Destination(a) => if at + 1 == ENTRIES_PER_PAGE {
                (Seq::empty(), Err(WalkError::NoEnd))
            } else {
                walk_from(mem, page, at + 1, Some(a), left)
            },
            Step::Indirection(a) => walk_from(mem, mem(a), 0, dest, left),
            Step::Copy(a) => match dest {
                None => (Seq::empty(), Err(WalkError::NoDestination)),
                Some(d) => {
                    let rest = if at + 1 == ENTRIES_PER_PAGE {
                        (Seq::empty(), Err(WalkError::NoEnd))
                    } else {
                        walk_from(mem, page, at + 1, Some((d + PAGE) as u64), left)
                    };
                    (seq![(d, a)] + rest.0, rest.1)
                },
            },
            Step::Nothing => if at + 1 == ENTRIES_PER_PAGE {
                (Seq::empty(), Err(WalkError::NoEnd))
            } else {
                walk_from(mem, page, at + 1, dest, left)
            },
        }
    }
}

/// No copy's destination is the last page of the address space, so moving on by a page never overflows.
pub open spec fn no_overflow(cs: Seq<(u64, u64)>) -> bool {
    forall|k: int| 0 <= k < cs.len() ==> (#[trigger] cs[k]).0 + PAGE <= u64::MAX
}

} // verus!

/// Walks a list from the page at `head` as the relocation code does, appending `(destination, source)` to `copies` for
/// every page it would copy. `read` gives the entries of a list page. Stops with an error after `limit` entries
/// without an end.
///
/// Verus proves that it does exactly what [`walk_from`] says, for any `read` that returns `mem(p)` for page `p`: the
/// copies appended and the result. `verification/juno-kexec/rules.rs` proves what that is for the list the kernel
/// builds. A walk whose destination would run past the address space must not be given (`requires`).
///
/// This is the Rust walk, and nothing in the kernel calls it. The copy that runs during a reload is
/// `kexec_relocation_start` in `bsp_boot.S` (kernel patch 1055, MMU off). It matches this walk entry for entry on lists
/// the kernel builds, by reading only. On malformed lists they differ: the assembly has no entry limit, reads past slot
/// 511, copies a source seen before any destination to physical address 0, and has no overflow check: on the last page of the address space its end bound wraps, so it copies 16 bytes instead of a page and goes on with an unaligned destination.
/// The hypotheses of the theorems about the built list (theorems 3 and 4 in `verification/juno-kexec/rules.rs`) rest on
/// unverified kernel code (`build_list`, `frame_outside`, `move_out`, `write_list_page` in patch 1055).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    with Ghost(mem): Ghost<ListMemory>
    requires
        forall|p: u64| read.requires((p,)),
        forall|p: u64, e: [u64; ENTRIES_PER_PAGE]| read.ensures((p,), e) ==> e@ == mem(p),
        no_overflow(walk_from(mem, mem(head), 0, None, limit as nat).0),
    ensures
        final(copies)@ == old(copies)@ + walk_from(mem, mem(head), 0, None, limit as nat).0,
        r == walk_from(mem, mem(head), 0, None, limit as nat).1,
))]
pub fn walk_entries(
    head: u64,
    read: impl Fn(u64) -> [u64; ENTRIES_PER_PAGE],
    copies: &mut Vec<(u64, u64)>,
    limit: usize,
) -> Result<(), WalkError> {
    let mut page = read(head);
    let mut at: usize = 0;
    let mut dest: Option<u64> = None;
    let mut n: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            n <= limit,
            at < ENTRIES_PER_PAGE,
            forall|p: u64| read.requires((p,)),
            forall|p: u64, e: [u64; ENTRIES_PER_PAGE]| read.ensures((p,), e) ==> e@ == mem(p),
            ({
                let rest = walk_from(mem, page@, at as int, dest, (limit - n) as nat);
                &&& copies@ + rest.0 == old(copies)@ + walk_from(mem, mem(head), 0, None, limit as nat).0
                &&& rest.1 == walk_from(mem, mem(head), 0, None, limit as nat).1
                &&& no_overflow(rest.0)
            }),
        decreases limit - n,
    ))]
    while n < limit {
        #[cfg(verus_keep_ghost)]
        proof_decl! {
            let ghost rest = walk_from(mem, page@, at as int, dest, (limit - n) as nat);
            let ghost before = copies@;
        }
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
                let d = match dest {
                    Some(d) => d,
                    None => return Err(WalkError::NoDestination),
                };
                #[cfg(verus_keep_ghost)]
                proof! { assert(rest.0[0] == (d, addr)); }
                copies.push((d, addr));
                dest = Some(d + PAGE);
                #[cfg(verus_keep_ghost)]
                proof! {
                    let tail = walk_from(mem, page@, at as int, dest, (limit - n - 1) as nat);
                    if at < ENTRIES_PER_PAGE {
                        assert(rest.0 =~= seq![(d, addr)] + tail.0);
                        assert(copies@ + tail.0 =~= before + rest.0);
                        assert forall|k: int| 0 <= k < tail.0.len() implies (#[trigger] tail.0[k]).0 + PAGE <= u64::MAX by {
                            assert(tail.0[k] == rest.0[k + 1]);
                        }
                    } else {
                        assert(copies@ =~= before + rest.0);
                    }
                }
            }
            Step::Nothing => {}
        }
        // An entry with none of the flags is nothing. The relocation code reads past the end of a page only after an
        // indirection on lists the kernel builds (the assembly does not check), so `at` never passes the last slot here either.
        if at == ENTRIES_PER_PAGE {
            #[cfg(verus_keep_ghost)]
            proof! { assert(copies@ =~= before + rest.0); }
            return Err(WalkError::NoEnd);
        }
        n += 1;
    }
    Err(WalkError::NoEnd)
}

/// Whether `entry` carries any flag the list knows.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == (entry & FLAGS != 0),
))]
pub fn is_entry(entry: u64) -> bool {
    entry & FLAGS != 0
}

#[cfg(verus_keep_ghost)]
verus! {

/// `x` rounded up to a multiple of `m`.
pub open spec fn round_up(x: int, m: int) -> int {
    if x % m == 0 { x } else { x + (m - x % m) }
}

/// `[a0, a1)` and `[b0, b1)` share an address (the test the code makes, also for empty or reversed ranges).
pub open spec fn overlap(a0: int, a1: int, b0: int, b1: int) -> bool {
    a0 < b1 && b0 < a1
}

/// Region `i` is RAM the old kernel may give up and holds `x`.
pub open spec fn holds(regions: Seq<(Range<u64>, bool)>, i: int, x: int) -> bool {
    &&& regions[i].1
    &&& regions[i].0.start <= x
    &&& x < regions[i].0.end
}

/// `x` lies in a `true` region.
pub open spec fn usable_at(regions: Seq<(Range<u64>, bool)>, x: int) -> bool {
    exists|i: int| 0 <= i < regions.len() && #[trigger] holds(regions, i, x)
}

/// Every address of `[s, e)` lies in a `true` region (adjacent and overlapping ones join).
pub open spec fn covered_spec(regions: Seq<(Range<u64>, bool)>, s: int, e: int) -> bool {
    forall|x: int| s <= x < e ==> #[trigger] usable_at(regions, x)
}

/// `[s, e)` shares no address with a `false` region.
pub open spec fn clear_of_reserved(regions: Seq<(Range<u64>, bool)>, s: int, e: int) -> bool {
    forall|i: int| 0 <= i < regions.len() ==> (#[trigger] regions[i]).1 || !overlap(s, e, regions[i].0.start as int, regions[i].0.end as int)
}

/// `size` bytes at `s` are a place for the initramfs: inside the address space, clear of the window and of every `false`
/// region, and wholly inside the `true` ones.
pub open spec fn place_ok(regions: Seq<(Range<u64>, bool)>, window: Range<u64>, s: int, size: int) -> bool {
    &&& 0 <= s
    &&& s + size <= u64::MAX
    &&& !overlap(s, s + size, window.start as int, window.end as int)
    &&& clear_of_reserved(regions, s, s + size)
    &&& covered_spec(regions, s, s + size)
}

/// Where candidate `k` of the search comes from: the start of region `k`, or the window's end for `k == regions.len()`.
pub open spec fn raw_candidate(regions: Seq<(Range<u64>, bool)>, window: Range<u64>, k: int) -> int {
    if k < regions.len() { regions[k].0.start as int } else { window.end as int }
}

/// Candidate `k` is searched: the window's end always, a region's start only for a `true` region.
pub open spec fn live_candidate(regions: Seq<(Range<u64>, bool)>, k: int) -> bool {
    0 <= k <= regions.len() && (k == regions.len() || regions[k].1)
}

/// The code's search rule: `c` is a `true` region's start or the window's end, rounded up to 2 MiB, in the address
/// space.
pub open spec fn candidate(regions: Seq<(Range<u64>, bool)>, window: Range<u64>, c: int) -> bool {
    &&& c <= u64::MAX
    &&& exists|k: int| live_candidate(regions, k) && c == round_up(#[trigger] raw_candidate(regions, window, k), BLOCK_2M as int)
}

/// A candidate where `size` bytes fit.
pub open spec fn fits(regions: Seq<(Range<u64>, bool)>, window: Range<u64>, size: int, c: int) -> bool {
    candidate(regions, window, c) && place_ok(regions, window, c, size)
}

/// Candidate `k` fits `size` bytes.
pub open spec fn fits_k(regions: Seq<(Range<u64>, bool)>, window: Range<u64>, size: int, k: int) -> bool {
    let c = round_up(raw_candidate(regions, window, k), BLOCK_2M as int);
    live_candidate(regions, k) && c <= u64::MAX && place_ok(regions, window, c, size)
}

/// The bests so far are at or below candidate `c`, on its side of the window.
pub open spec fn lowest_so_far(best_above: Option<u64>, best_below: Option<u64>, c: int, window_end: int) -> bool {
    if c >= window_end {
        best_above matches Some(b) && b <= c
    } else {
        best_below matches Some(b) && b <= c
    }
}

pub proof fn lemma_round_up(x: int, m: int)
    requires
        0 <= x,
        0 < m,
    ensures
        round_up(x, m) % m == 0,
        x <= round_up(x, m) < x + m,
{
    if x % m != 0 {
        vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x, m);
        vstd::arithmetic::div_mod::lemma_mod_multiples_vanish(x / m + 1, 0, m);
        assert(x + (m - x % m) == m * (x / m + 1)) by (nonlinear_arith)
            requires x == m * (x / m) + x % m;
        vstd::arithmetic::div_mod::lemma_mod_multiples_basic(x / m + 1, m);
    }
}

} // verus!

/// `x` rounded up to a multiple of `m`, or `None` past the end of the address space: `u64::checked_next_multiple_of`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires
        m > 0,
    ensures
        r matches Some(v) ==> v == round_up(x as int, m as int),
        r is None ==> round_up(x as int, m as int) > u64::MAX,
))]
fn next_multiple(x: u64, m: u64) -> Option<u64> {
    let rem = x % m;
    if rem == 0 {
        Some(x)
    } else {
        x.checked_add(m - rem)
    }
}

/// `start` if it is lower than `best`, else `best`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == Some(match best { Some(b) => if b <= start { b } else { start }, None => start }),
))]
fn lowest(best: Option<u64>, start: u64) -> Option<u64> {
    match best {
        Some(b) if b <= start => Some(b),
        _ => Some(start),
    }
}

/// The first `true` region that holds `at`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(i) ==> i < regions@.len() && holds(regions@, i as int, at as int),
        r is None ==> forall|i: int| 0 <= i < regions@.len() ==> !holds(regions@, i, at as int),
))]
fn holding(regions: &[(Range<u64>, bool)], at: u64) -> Option<usize> {
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= regions@.len(),
            forall|j: int| 0 <= j < i ==> !holds(regions@, j, at as int),
        decreases regions@.len() - i,
    ))]
    while i < regions.len() {
        let (r, ok) = (&regions[i].0, regions[i].1);
        if ok && r.start <= at && at < r.end {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Whether `[start, end)` shares no address with a `false` region.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == clear_of_reserved(regions@, start as int, end as int),
))]
fn clear(regions: &[(Range<u64>, bool)], start: u64, end: u64) -> bool {
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= regions@.len(),
            forall|j: int| 0 <= j < i ==> (#[trigger] regions@[j]).1
                || !overlap(start as int, end as int, regions@[j].0.start as int, regions@[j].0.end as int),
        decreases regions@.len() - i,
    ))]
    while i < regions.len() {
        let (r, ok) = (&regions[i].0, regions[i].1);
        if !ok && start < r.end && r.start < end {
            return false;
        }
        i += 1;
    }
    true
}

/// The candidate from `raw` (a `true` region's start or the window's end) where `size` bytes fit, if they do.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(s) ==> s == round_up(raw as int, BLOCK_2M as int) && (s as int) % (BLOCK_2M as int) == 0
            && place_ok(regions@, *window, s as int, size as int),
        r is None ==> round_up(raw as int, BLOCK_2M as int) > u64::MAX
            || !place_ok(regions@, *window, round_up(raw as int, BLOCK_2M as int), size as int),
))]
fn fit_at(regions: &[(Range<u64>, bool)], window: &Range<u64>, raw: u64, size: u64) -> Option<u64> {
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(BLOCK_2M == 0x20_0000u64) by (compute);
        lemma_round_up(raw as int, BLOCK_2M as int);
    }
    let start = next_multiple(raw, BLOCK_2M)?;
    let end = start.checked_add(size)?;
    let range = start..end;
    if !(start < window.end && window.start < end)
        && clear(regions, start, end)
        && covered(regions, &range)
    {
        Some(start)
    } else {
        None
    }
}

/// Where the initramfs goes: `len` bytes (rounded up to a page) on a 2 MiB boundary, inside RAM the old kernel may give
/// up (`regions` with `true`; adjacent ones join), clear of `window` and of every `false` region. The lowest place at or
/// above the window's end, else the lowest below its start; `None` when nothing fits or `len` is 0.
///
/// Verus proves the `ensures`: a place it gives is that many bytes, 2 MiB-aligned, clear of the window and of every
/// `false` region, and every byte of it lies in a `true` region; it is a candidate of the search (a `true` region's start
/// or the window's end, rounded up to 2 MiB) and the lowest that fits above the window, else the lowest below. `None`
/// means no candidate fits. Not stated: that a fit at an address that is no candidate implies one at a candidate (true
/// when regions do not overlap; see the tests).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        len == 0 ==> r is None,
        r matches Some(d) ==> {
            let size = round_up(len as int, PAGE as int);
            &&& d.end - d.start == size
            &&& len <= size < len + PAGE
            &&& size % (PAGE as int) == 0
            &&& (d.start as int) % (BLOCK_2M as int) == 0
            &&& d.end <= window.start || window.end <= d.start
            &&& forall|i: int| 0 <= i < regions@.len() && !(#[trigger] regions@[i]).1
                    ==> d.end <= regions@[i].0.start || regions@[i].0.end <= d.start
            &&& forall|x: int| d.start <= x < d.end ==> #[trigger] usable_at(regions@, x)
            &&& place_ok(regions@, *window, d.start as int, size)
            &&& candidate(regions@, *window, d.start as int)
            &&& d.start >= window.end ==> forall|c: int| #[trigger] fits(regions@, *window, size, c) && c >= window.end
                    ==> d.start <= c
            &&& d.start < window.end ==> forall|c: int| #[trigger] fits(regions@, *window, size, c)
                    ==> c < window.end && d.start <= c
        },
        r is None ==> len == 0 || round_up(len as int, PAGE as int) > u64::MAX
            || forall|c: int| !(#[trigger] fits(regions@, *window, round_up(len as int, PAGE as int), c)),
))]
pub fn initrd_destination(
    regions: &[(Range<u64>, bool)],
    window: &Range<u64>,
    len: u64,
) -> Option<Range<u64>> {
    if len == 0 {
        return None;
    }
    let size = next_multiple(len, PAGE)?;
    #[cfg(verus_keep_ghost)]
    proof! { lemma_round_up(len as int, PAGE as int); }
    // Candidates: each allowed region's start and the window's end, rounded up to 2 MiB. A lowest fit starts at one of
    // them, since anything below a candidate is outside every run or inside the window.
    let mut best_above: Option<u64> = None;
    let mut best_below: Option<u64> = None;
    let mut k: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            k <= regions@.len(),
            size as int == round_up(len as int, PAGE as int),
            best_above matches Some(b) ==> b >= window.end && place_ok(regions@, *window, b as int, size as int)
                && candidate(regions@, *window, b as int) && (b as int) % (BLOCK_2M as int) == 0,
            best_below matches Some(b) ==> b < window.end && place_ok(regions@, *window, b as int, size as int)
                && candidate(regions@, *window, b as int) && (b as int) % (BLOCK_2M as int) == 0,
            forall|j: int| 0 <= j < k && #[trigger] fits_k(regions@, *window, size as int, j) ==>
                lowest_so_far(best_above, best_below, round_up(raw_candidate(regions@, *window, j), BLOCK_2M as int), window.end as int),
        decreases regions@.len() - k,
    ))]
    while k < regions.len() {
        // Only a `true` region's start is a candidate.
        let fit = if regions[k].1 {
            fit_at(regions, window, regions[k].0.start, size)
        } else {
            None
        };
        if let Some(start) = fit {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(regions@[k as int].0.start as int == raw_candidate(regions@, *window, k as int));
                assert(live_candidate(regions@, k as int));
                assert(candidate(regions@, *window, start as int));
            }
            if start >= window.end {
                best_above = lowest(best_above, start);
            } else if start + size <= window.start {
                best_below = lowest(best_below, start);
            }
        }
        k += 1;
    }
    // Last, the window's end.
    if let Some(start) = fit_at(regions, window, window.end, size) {
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(window.end as int == raw_candidate(regions@, *window, regions@.len() as int));
            assert(live_candidate(regions@, regions@.len() as int));
            assert(candidate(regions@, *window, start as int));
        }
        if start >= window.end {
            best_above = lowest(best_above, start);
        } else if start + size <= window.start {
            best_below = lowest(best_below, start);
        }
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert forall|j: int| 0 <= j <= regions@.len() && #[trigger] fits_k(regions@, *window, size as int, j) implies
            lowest_so_far(best_above, best_below, round_up(raw_candidate(regions@, *window, j), BLOCK_2M as int), window.end as int) by {}
        assert forall|c: int| #[trigger] fits(regions@, *window, size as int, c) implies
            lowest_so_far(best_above, best_below, c, window.end as int) by {
            let j = choose|j: int| live_candidate(regions@, j) && c == round_up(#[trigger] raw_candidate(regions@, *window, j), BLOCK_2M as int);
            assert(fits_k(regions@, *window, size as int, j));
        }
    }
    let start = match best_above {
        Some(b) => b,
        None => best_below?,
    };
    Some(start..start + size)
}

/// Whether `range` lies wholly inside the union of the `true` regions.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r == covered_spec(regions@, range.start as int, range.end as int),
))]
fn covered(regions: &[(Range<u64>, bool)], range: &Range<u64>) -> bool {
    let mut at = range.start;
    // At most one pass per region: each step moves `at` to the end of a region that holds it.
    let mut n: usize = 0;
    #[cfg(verus_keep_ghost)]
    proof_decl! { let ghost mut used: Set<int> = Set::empty(); }
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            n <= regions@.len(),
            range.start <= at,
            covered_spec(regions@, range.start as int, at as int),
            used.len() == n,
            forall|j: int| #[trigger] used.contains(j) ==> 0 <= j < regions@.len() && regions@[j].0.end <= at,
        decreases regions@.len() - n,
    ))]
    loop {
        if at >= range.end {
            return true;
        }
        match holding(regions, at) {
            Some(i) => {
                #[cfg(verus_keep_ghost)]
                proof! {
                    assert(!used.contains(i as int));
                    assert forall|x: int| range.start <= x < regions@[i as int].0.end implies #[trigger] usable_at(regions@, x) by {
                        if x >= at {
                            assert(holds(regions@, i as int, x));
                        }
                    }
                    used = used.insert(i as int);
                }
                at = regions[i].0.end;
            }
            None => {
                #[cfg(verus_keep_ghost)]
                proof! { assert(!usable_at(regions@, at as int)); }
                return false;
            }
        }
        if n == regions.len() {
            #[cfg(verus_keep_ghost)]
            proof! {
                // n + 1 distinct region indices below n: impossible, so this is never reached.
                let all = vstd::set_lib::set_int_range(0, regions@.len() as int);
                vstd::set_lib::lemma_int_range(0, regions@.len() as int);
                assert(used.subset_of(all));
                vstd::set_lib::lemma_len_subset(used, all);
                assert(false);
            }
            return at >= range.end;
        }
        n += 1;
    }
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

    fn walk(
        head: u64,
        pages: &BTreeMap<u64, [u64; ENTRIES_PER_PAGE]>,
    ) -> Result<Vec<(u64, u64)>, WalkError> {
        let mut copies = Vec::new();
        walk_entries(head, |p| pages[&p], &mut copies, 1 << 20)?;
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
            assert_eq!(
                pages.len(),
                (n + 2).div_ceil(ENTRIES_PER_PAGE - 1).max(1),
                "n = {n}"
            );
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
            [
                (0xa000_0000, 0x1000),
                (0xa000_1000, 0x5000),
                (0xc000_0000, 0x3000)
            ]
        );
    }

    #[test]
    fn bad_lists_are_caught() {
        let mut page = [0u64; ENTRIES_PER_PAGE];
        page[0] = source(0x1000);
        page[1] = IND_DONE;
        assert_eq!(
            walk_entries(0, |_| page, &mut Vec::new(), 100),
            Err(WalkError::NoDestination)
        );
        let empty = [0u64; ENTRIES_PER_PAGE];
        assert_eq!(
            walk_entries(0, |_| empty, &mut Vec::new(), 100_000),
            Err(WalkError::NoEnd)
        );
        assert!(!is_entry(0) && is_entry(IND_DONE) && is_entry(source(0x1000)));
    }

    #[test]
    #[should_panic(expected = "page-aligned")]
    fn an_unaligned_source_is_refused() {
        let _ = source(0x1001);
    }

    #[test]
    #[should_panic(expected = "page-aligned")]
    fn an_unaligned_destination_is_refused() {
        let _ = destination(0x1001);
    }

    #[test]
    #[should_panic(expected = "page-aligned")]
    fn an_unaligned_list_page_link_is_refused() {
        ListPage::new().link(0x1001);
    }

    #[test]
    fn aligned_pages_are_accepted_by_the_runtime_asserts() {
        assert_eq!(destination(0x2000), 0x2000 | IND_DESTINATION);
        assert_eq!(source(0x3000), 0x3000 | IND_SOURCE);
        let mut page = ListPage::new();
        page.link(0x4000);
        assert_eq!(page.entries()[0], 0x4000 | IND_INDIRECTION);
    }

    #[test]
    fn the_destination_follows_the_window() {
        let regions = [(0x8000_0000..0x1_0000_0000, true)];
        let window = 0xa000_0000..0xa1a0_0000;
        assert_eq!(
            initrd_destination(&regions, &window, 68 * MIB),
            Some(0xa1a0_0000..0xa1a0_0000 + 68 * MIB)
        );
        // Rounded up to a page and placed on 2 MiB.
        let window = 0xa000_0000..0xa1a1_2345;
        assert_eq!(
            initrd_destination(&regions, &window, 5),
            Some(0xa1c0_0000..0xa1c0_1000)
        );
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
        let regions = [
            (0x8000_0000..0xa000_0000, true),
            (0xa000_0000..0xa200_0000, true),
        ];
        let window = 0xa000_0000..0xa200_0000;
        assert_eq!(
            initrd_destination(&regions, &window, 3 * MIB),
            Some(0x8000_0000..0x8030_0000)
        );
        assert_eq!(initrd_destination(&regions, &window, 0), None);
        assert_eq!(initrd_destination(&regions, &window, 600 * MIB), None);
    }

    #[test]
    fn a_destination_never_touches_the_window_or_a_reserved_range() {
        // Every layout of a few regions on a coarse grid, every window and length on it.
        let grid = |i: u64| 0x8000_0000 + i * 4 * MIB;
        let mut checked = 0;
        for mask in 0u32..(1 << 6) {
            let regions: Vec<(Range<u64>, bool)> = (0..6)
                .map(|i| (grid(i)..grid(i + 1), mask & (1 << i) != 0))
                .collect();
            for ws in 0..6 {
                for we in ws + 1..=6 {
                    let window = grid(ws)..grid(we);
                    for len in [1, PAGE, 3 * MIB, 4 * MIB, 9 * MIB] {
                        if let Some(d) = initrd_destination(&regions, &window, len) {
                            checked += 1;
                            assert!(d.end - d.start >= len && d.start % BLOCK_2M == 0);
                            assert!(d.end <= window.start || window.end <= d.start);
                            for (r, ok) in &regions {
                                assert!(
                                    *ok || d.end <= r.start || r.end <= d.start,
                                    "{d:x?} in {r:x?}"
                                );
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

    /// `initrd_destination` and `covered` as they were before Verus proved them (iterators and closures), kept to show
    /// that the index loops behave the same.
    fn old_initrd_destination(
        regions: &[(Range<u64>, bool)],
        window: &Range<u64>,
        len: u64,
    ) -> Option<Range<u64>> {
        if len == 0 {
            return None;
        }
        let size = len.checked_next_multiple_of(PAGE)?;
        let overlaps = |a: &Range<u64>, b: &Range<u64>| a.start < b.end && b.start < a.end;
        let allowed = |r: &Range<u64>| {
            !overlaps(r, window)
                && regions.iter().all(|(reg, ok)| *ok || !overlaps(r, reg))
                && old_covered(regions, r)
        };
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

    fn old_covered(regions: &[(Range<u64>, bool)], range: &Range<u64>) -> bool {
        let mut at = range.start;
        for _ in 0..=regions.len() {
            if at >= range.end {
                return true;
            }
            match regions
                .iter()
                .find(|(r, ok)| *ok && r.start <= at && at < r.end)
            {
                Some((r, _)) => at = r.end,
                None => return false,
            }
        }
        at >= range.end
    }

    #[test]
    fn the_index_loops_behave_as_the_iterators_did() {
        // Every layout of up to three regions (overlapping, empty, reversed and unaligned ones too) on a grid of 1 MiB
        // steps, every window on it, lengths around the page and 2 MiB, and edges of the address space.
        let pts: [u64; 7] = [0, MIB, 2 * MIB, 3 * MIB + 5, 4 * MIB, 6 * MIB, 8 * MIB];
        let lens = [
            0,
            1,
            PAGE,
            PAGE + 1,
            MIB,
            2 * MIB,
            3 * MIB,
            5 * MIB,
            u64::MAX - 10,
        ];
        let mut regions_all: Vec<(Range<u64>, bool)> = Vec::new();
        for &a in &pts {
            for &b in &pts {
                for ok in [true, false] {
                    regions_all.push((a..b, ok));
                }
            }
        }
        let mut compared = 0u64;
        let mut covered_compared = 0u64;
        for n in 0..=2usize {
            // A sample of every n-tuple of regions: stride through the list to keep the run short.
            let stride = [1usize, 7, 29][n];
            let count = regions_all.len().pow(n as u32);
            let mut idx = 0;
            while idx < count {
                let mut rs = Vec::new();
                let mut x = idx;
                for _ in 0..n {
                    rs.push(regions_all[x % regions_all.len()].clone());
                    x /= regions_all.len();
                }
                rs.push((0..8 * MIB, true));
                for &ws in &pts {
                    for &we in &pts {
                        let window = ws..we;
                        for &len in &lens {
                            assert_eq!(
                                initrd_destination(&rs, &window, len),
                                old_initrd_destination(&rs, &window, len),
                                "{rs:x?} {window:x?} {len:x}"
                            );
                            compared += 1;
                        }
                        assert_eq!(
                            covered(&rs, &window),
                            old_covered(&rs, &window),
                            "{rs:x?} {window:x?}"
                        );
                        covered_compared += 1;
                    }
                }
                idx += stride;
            }
        }
        // Near the top of the address space: rounding and the end overflow.
        let top = u64::MAX - 3 * MIB;
        let rs = [(top..u64::MAX, true)];
        for window in [0..MIB, top..top + MIB, u64::MAX - 1..u64::MAX] {
            for len in [1, MIB, 2 * MIB, 4 * MIB] {
                assert_eq!(
                    initrd_destination(&rs, &window, len),
                    old_initrd_destination(&rs, &window, len)
                );
                compared += 1;
            }
        }
        assert!(
            compared > 100_000 && covered_compared > 10_000,
            "{compared} {covered_compared}"
        );
    }

    #[test]
    fn overlapping_regions_can_hide_a_place() {
        // Verus proves `None` only for the search's candidates. With a `false` region inside a `true` one (a memory map
        // the kernel's own map does not produce: Asterinas makes it non-overlapping), the one candidate below the
        // window does not fit while 4..7 MiB would.
        let regions = [(0..8 * MIB, true), (2 * MIB..3 * MIB, false)];
        let window = 100 * MIB..101 * MIB;
        assert_eq!(initrd_destination(&regions, &window, 3 * MIB), None);
        let place = 4 * MIB..7 * MIB;
        assert!(covered(&regions, &place) && place.end <= window.start);
        assert!(
            regions
                .iter()
                .all(|(r, ok)| *ok || place.end <= r.start || r.end <= place.start)
        );
    }
}
