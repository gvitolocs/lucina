// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// kernel/lucina-kexec/src/pages.rs by #[path], so Verus checks `step` and `encode` as the kernel compiles them; what is
// proved of each is in its `ensures`. On top of that, the walk below reads the entry list the way the relocation code
// is meant to (bsp_boot.S, through the shipped `step`) and proves, for the list the kernel builds in `build_list` (the window's
// destination, the image's pages, the initramfs's destination, its pages, done):
//   1. every staged page is copied exactly once, the k-th page of a segment to its destination plus k pages, in order;
//   2. every write lands inside its segment's destination range, each destination page written exactly once;
//   3. no write lands on a staged page or a page of the list, given what staging guarantees (no staged page or list page
//      inside a destination range: `frame_outside` and `move_out` in kernel/core/src/syscall/kexec.rs), so no copy can
//      overwrite a page before it is read, whatever the order.
//   4. the SHIPPED `walk_entries`, reading that list cut into list pages by the shipped `ListPage` the way `build_list`
//      cuts it (`build_pages`, `laid_out`), returns Ok and appends exactly the copies of 1 (`theorem_shipped_walk`,
//      `walk_of_the_built_list`): the paged list flattens to the flat list `walk` reads (`lemma_paged_is_flat`).
//      Assumed there: `read` returns what the list pages hold (`holds_pages`), which 3 justifies (no copy writes a list
//      page); the loop of the kernel's `build_list` is mirrored by `build_pages`, not verified itself.
//
// LIMITS of what "proved" means here (independent review, docs/ci/TRUST-FINDINGS.md section "kexec boundary"):
//   F1. Theorem 4 and `walk_entries` are about the RUST walk. The kernel does not call `walk_entries`. The copy that
//       runs during a reload is `kexec_relocation_start` in bsp_boot.S (kernel patch 1055, MMU off). It matches the
//       proved walk entry for entry on lists the kernel builds, BY READING ONLY. On malformed lists they differ: the
//       assembly has no entry limit, reads past slot 511 into the next physical page, copies a source seen before any
//       destination to physical address 0, and has no overflow check: on the last page of the address space its end bound wraps, so it copies 16 bytes instead of a page and goes on with an unaligned destination.
//   F2. The hypotheses of theorems 3 and 4 rest on unverified kernel code: `build_list`, `frame_outside`, `move_out`
//       and `write_list_page` in patch 1055. Only the alignment asserts, the device-tree/relocation-page separation
//       asserts and the initramfs-destination-apart-from-the-window assert (patch line 695) are checked at run time.
#![feature(proc_macro_hygiene)]
extern crate alloc;
#[path = "../../kernel/lucina-kexec/src/pages.rs"]
pub mod pages;

use pages::*;
use vstd::prelude::*;

verus! {

/// A page-aligned address.
pub open spec fn aligned(a: u64) -> bool {
    a & LOW == 0
}

/// The bit facts behind the encoding: an aligned page with one flag reads back as that flag's step of the page.
proof fn lemma_flags(p: u64)
    requires
        aligned(p),
    ensures
        (p | IND_DESTINATION) & IND_DONE == 0,
        (p | IND_DESTINATION) & IND_DESTINATION != 0,
        (p | IND_DESTINATION) & !LOW == p,
        (p | IND_SOURCE) & IND_DONE == 0,
        (p | IND_SOURCE) & IND_DESTINATION == 0,
        (p | IND_SOURCE) & IND_INDIRECTION == 0,
        (p | IND_SOURCE) & IND_SOURCE != 0,
        (p | IND_SOURCE) & !LOW == p,
        (p | IND_INDIRECTION) & IND_DONE == 0,
        (p | IND_INDIRECTION) & IND_DESTINATION == 0,
        (p | IND_INDIRECTION) & IND_INDIRECTION != 0,
        (p | IND_INDIRECTION) & !LOW == p,
        IND_DONE & IND_DONE != 0,
{
    // The same facts, proved once next to the shipped encoding.
    lemma_flag_bits(p);
}

/// The shipped `encode` and `step` agree: what is written is read back as meant.
fn encode_then_step(p: u64)
    requires
        aligned(p),
{
    proof { lemma_flags(p); }
    let d = encode(p, IND_DESTINATION);
    let rd = step(d);
    assert(rd == Step::Destination(p));
    let s = encode(p, IND_SOURCE);
    let rs = step(s);
    assert(rs == Step::Copy(p));
    let i = encode(p, IND_INDIRECTION);
    let ri = step(i);
    assert(ri == Step::Indirection(p));
    let done = step(IND_DONE);
    assert(done == Step::Done);
}

/// The step of an entry, as `step` ensures (a spec function, so that the walk below can use it).
pub open spec fn spec_step(e: u64) -> Step {
    if e & IND_DONE != 0 {
        Step::Done
    } else if e & IND_DESTINATION != 0 {
        Step::Destination(e & !LOW)
    } else if e & IND_INDIRECTION != 0 {
        Step::Indirection(e & !LOW)
    } else if e & IND_SOURCE != 0 {
        Step::Copy(e & !LOW)
    } else {
        Step::Nothing
    }
}

/// The shipped `step` is `spec_step`.
fn step_is_spec_step(e: u64) -> (r: Step)
    ensures
        r == spec_step(e),
{
    step(e)
}

/// The shipped `destination` and `source`, which `build_list` uses for every entry but the end, make exactly the entries
/// the list below is made of, and the walk reads them back as meant.
fn destination_and_source_read_back(p: u64)
    requires
        p % PAGE == 0,
{
    let d = destination(p);
    assert(d == p | IND_DESTINATION && spec_step(d) == Step::Destination(p));
    let s = source(p);
    assert(s == p | IND_SOURCE && spec_step(s) == Step::Copy(p));
    proof { lemma_page_flag(p); }
    assert(aligned(p));
}

/// One copy the relocation code makes: (destination page, staged page).
pub struct Copy {
    pub dst: int,
    pub src: u64,
}

/// The copies the relocation code makes from entry `i` on with destination `dest`, until done or the end of `es`.
/// (Indirections only chain list pages; the flat list here is their content end to end.)
/// That the shipped paged walk is this walk of the flat list is proved below (`theorem_shipped_walk`).
pub open spec fn walk(es: Seq<u64>, i: int, dest: int) -> Seq<Copy>
    decreases es.len() - i,
{
    if i < 0 || i >= es.len() {
        Seq::empty()
    } else {
        match spec_step(es[i]) {
            Step::Done => Seq::empty(),
            Step::Destination(a) => walk(es, i + 1, a as int),
            Step::Indirection(_) => walk(es, i + 1, dest),
            Step::Copy(s) => seq![Copy { dst: dest, src: s }] + walk(es, i + 1, dest + (PAGE as int)),
            Step::Nothing => walk(es, i + 1, dest),
        }
    }
}

/// The entries of a segment's staged pages.
pub open spec fn sources(ps: Seq<u64>) -> Seq<u64> {
    ps.map(|k: int, p: u64| p | IND_SOURCE)
}

/// The copies of a segment: its k-th page to `base` plus k pages.
pub open spec fn copies(base: int, ps: Seq<u64>) -> Seq<Copy> {
    ps.map(|k: int, p: u64| Copy { dst: base + k * (PAGE as int), src: p })
}

/// The list the kernel builds (`build_list`): the window's destination, the image's pages, the initramfs's destination,
/// its pages, done.
pub open spec fn list(w: u64, image: Seq<u64>, d: u64, initrd: Seq<u64>) -> Seq<u64> {
    seq![w | IND_DESTINATION] + sources(image) + seq![d | IND_DESTINATION] + sources(initrd) + seq![IND_DONE]
}

pub open spec fn all_aligned(ps: Seq<u64>) -> bool {
    forall|k: int| 0 <= k < ps.len() ==> aligned(#[trigger] ps[k])
}

/// Walking a run of source entries copies each page in turn, then goes on after the run.
proof fn lemma_run(es: Seq<u64>, i: int, dest: int, ps: Seq<u64>)
    requires
        0 <= i,
        i + ps.len() <= es.len(),
        all_aligned(ps),
        forall|k: int| 0 <= k < ps.len() ==> es[i + k] == #[trigger] (ps[k] | IND_SOURCE),
    ensures
        walk(es, i, dest) == copies(dest, ps) + walk(es, i + ps.len(), dest + ps.len() * (PAGE as int)),
    decreases ps.len(),
{
    if ps.len() == 0 {
        assert(copies(dest, ps) =~= Seq::<Copy>::empty());
        assert(copies(dest, ps) + walk(es, i, dest) =~= walk(es, i, dest));
    } else {
        let p = ps[0];
        lemma_flags(p);
        assert(es[i] == p | IND_SOURCE);
        assert(spec_step(es[i]) == Step::Copy(p));
        let rest = ps.drop_first();
        assert forall|k: int| 0 <= k < rest.len() implies es[(i + 1) + k] == #[trigger] (rest[k] | IND_SOURCE) by {
            assert(rest[k] == ps[k + 1]);
            assert(es[i + (k + 1)] == ps[k + 1] | IND_SOURCE);
        }
        assert(all_aligned(rest)) by {
            assert forall|k: int| 0 <= k < rest.len() implies aligned(#[trigger] rest[k]) by {
                assert(rest[k] == ps[k + 1]);
            }
        }
        lemma_run(es, i + 1, dest + (PAGE as int), rest);
        assert(copies(dest, ps) =~= seq![Copy { dst: dest, src: p }] + copies(dest + (PAGE as int), rest)) by {
            assert forall|k: int| 0 <= k < ps.len() implies #[trigger] copies(dest, ps)[k]
                == (seq![Copy { dst: dest, src: p }] + copies(dest + (PAGE as int), rest))[k] by {
                if k > 0 {
                    assert(rest[k - 1] == ps[k]);
                    assert(dest + (PAGE as int) + (k - 1) * (PAGE as int) == dest + k * (PAGE as int)) by (nonlinear_arith);
                }
            }
        }
        assert(dest + (PAGE as int) + rest.len() * (PAGE as int) == dest + ps.len() * (PAGE as int)) by (nonlinear_arith)
            requires rest.len() + 1 == ps.len();
        assert(seq![Copy { dst: dest, src: p }] + (copies(dest + (PAGE as int), rest) + walk(es, i + ps.len(), dest + ps.len() * (PAGE as int)))
            =~= (seq![Copy { dst: dest, src: p }] + copies(dest + (PAGE as int), rest)) + walk(es, i + ps.len(), dest + ps.len() * (PAGE as int)));
    }
}

/// 1. The list the kernel builds copies the image's pages to the window and the initramfs's to its destination, each
/// staged page once, in order, and nothing else.
proof fn theorem_copies(w: u64, image: Seq<u64>, d: u64, initrd: Seq<u64>)
    requires
        aligned(w),
        aligned(d),
        all_aligned(image),
        all_aligned(initrd),
    ensures
        walk(list(w, image, d, initrd), 0, 0) == copies(w as int, image) + copies(d as int, initrd),
{
    let es = list(w, image, d, initrd);
    let n = image.len() as int;
    let m = initrd.len() as int;
    lemma_flags(w);
    lemma_flags(d);
    assert(es.len() == n + m + 3);
    assert(es[0] == w | IND_DESTINATION);
    assert(spec_step(es[0]) == Step::Destination(w));
    assert forall|k: int| 0 <= k < image.len() implies es[1 + k] == #[trigger] (image[k] | IND_SOURCE) by {}
    lemma_run(es, 1, w as int, image);
    assert(es[1 + n] == d | IND_DESTINATION);
    assert(spec_step(es[1 + n]) == Step::Destination(d));
    assert forall|k: int| 0 <= k < initrd.len() implies es[(2 + n) + k] == #[trigger] (initrd[k] | IND_SOURCE) by {}
    lemma_run(es, 2 + n, d as int, initrd);
    assert(es[2 + n + m] == IND_DONE);
    assert(spec_step(IND_DONE) == Step::Done) by (bit_vector);
    assert(walk(es, 2 + n + m, d as int + m * (PAGE as int)) =~= Seq::<Copy>::empty());
    assert(walk(es, 0, 0) == walk(es, 1, w as int));
    assert(walk(es, 1 + n, w as int + n * (PAGE as int)) == walk(es, 2 + n, d as int));
    assert(copies(d as int, initrd) + Seq::<Copy>::empty() =~= copies(d as int, initrd));
    assert(copies(w as int, image) + (copies(d as int, initrd) + Seq::<Copy>::empty())
        =~= copies(w as int, image) + copies(d as int, initrd));
}

/// The pages of `[base, base + len)`.
pub open spec fn inside(a: int, base: int, len: int) -> bool {
    base <= a < base + len
}

/// 2 and 3. Each copy writes inside its segment's destination (the window holds the image: `parse_header` refuses an
/// image larger than its window; the initramfs's destination is its size rounded up to pages), each destination page
/// once; and when no staged page and no list page lies in a destination (staging's guarantee), no copy writes one.
proof fn theorem_writes(w: u64, wlen: int, image: Seq<u64>, d: u64, dlen: int, initrd: Seq<u64>, list_pages: Seq<u64>)
    requires
        aligned(w),
        aligned(d),
        all_aligned(image),
        all_aligned(initrd),
        image.len() * (PAGE as int) <= wlen,
        initrd.len() * (PAGE as int) <= dlen,
        // Staging's guarantee: nothing read lies in a destination.
        forall|k: int| 0 <= k < image.len() ==> !inside(#[trigger] image[k] as int, w as int, wlen) && !inside(image[k] as int, d as int, dlen),
        forall|k: int| 0 <= k < initrd.len() ==> !inside(#[trigger] initrd[k] as int, w as int, wlen) && !inside(initrd[k] as int, d as int, dlen),
        forall|k: int| 0 <= k < list_pages.len() ==> !inside(#[trigger] list_pages[k] as int, w as int, wlen) && !inside(list_pages[k] as int, d as int, dlen),
    ensures
        ({
            let cs = walk(list(w, image, d, initrd), 0, 0);
            &&& forall|i: int| 0 <= i < cs.len() ==> inside(#[trigger] cs[i].dst, w as int, wlen) || inside(cs[i].dst, d as int, dlen)
            &&& forall|i: int, j: int| 0 <= i < j < image.len() ==> #[trigger] cs[i].dst != #[trigger] cs[j].dst
            &&& forall|i: int, j: int| image.len() <= i < j < cs.len() ==> #[trigger] cs[i].dst != #[trigger] cs[j].dst
            &&& forall|i: int, k: int| 0 <= i < cs.len() && 0 <= k < image.len() ==> #[trigger] cs[i].dst != #[trigger] image[k] as int
            &&& forall|i: int, k: int| 0 <= i < cs.len() && 0 <= k < initrd.len() ==> #[trigger] cs[i].dst != #[trigger] initrd[k] as int
            &&& forall|i: int, k: int| 0 <= i < cs.len() && 0 <= k < list_pages.len() ==> #[trigger] cs[i].dst != #[trigger] list_pages[k] as int
        }),
{
    theorem_copies(w, image, d, initrd);
    let cs = walk(list(w, image, d, initrd), 0, 0);
    let n = image.len() as int;
    assert(cs == copies(w as int, image) + copies(d as int, initrd));
    assert forall|i: int| 0 <= i < cs.len() implies inside(#[trigger] cs[i].dst, w as int, wlen) || inside(cs[i].dst, d as int, dlen) by {
        if i < n {
            assert(cs[i].dst == w as int + i * (PAGE as int));
            assert(i * (PAGE as int) + (PAGE as int) <= n * (PAGE as int)) by (nonlinear_arith) requires i < n;
        } else {
            let k = i - n;
            assert(cs[i].dst == d as int + k * (PAGE as int));
            assert(k * (PAGE as int) + (PAGE as int) <= initrd.len() * (PAGE as int)) by (nonlinear_arith) requires k < initrd.len();
        }
    }
    assert forall|i: int, j: int| 0 <= i < j < image.len() implies #[trigger] cs[i].dst != #[trigger] cs[j].dst by {
        assert(i * (PAGE as int) < j * (PAGE as int)) by (nonlinear_arith) requires i < j;
    }
    assert forall|i: int, j: int| image.len() <= i < j < cs.len() implies #[trigger] cs[i].dst != #[trigger] cs[j].dst by {
        assert((i - n) * (PAGE as int) < (j - n) * (PAGE as int)) by (nonlinear_arith) requires i < j;
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// The paged list: what the shipped `walk_entries` reads is the flat list above, cut into list pages by the shipped
// `ListPage`, and the theorems above hold of the shipped function.

/// Entries of a list page before its link slot.
pub open spec fn per_page() -> int {
    (ENTRIES_PER_PAGE - 1) as int
}

/// Flat index `x` is slot `x % 511` of list page `x / 511`.
proof fn lemma_div511(x: int)
    requires
        0 <= x,
    ensures
        per_page() == 511,
        x == 511 * (x / 511) + x % 511,
        0 <= x % 511 < 511,
        0 <= x / 511,
{
    assert(per_page() == 511) by (compute);
    vstd::arithmetic::div_mod::lemma_fundamental_div_mod(x, 511);
    vstd::arithmetic::div_mod::lemma_mod_pos_bound(x, 511);
    vstd::arithmetic::div_mod::lemma_div_pos_is_pos(x, 511);
}

/// `pages` are the flat entries `es` cut into list pages at `addrs`, as `build_list` cuts them: page k holds
/// `es[511 k ..]` in its first 511 slots and, but for the last page, a link to page k + 1 in its last slot.
pub open spec fn laid_out(es: Seq<u64>, addrs: Seq<u64>, pages: Seq<Seq<u64>>) -> bool {
    &&& es.len() >= 1
    &&& pages.len() == (es.len() - 1) / per_page() + 1
    &&& addrs.len() >= pages.len()
    &&& forall|k: int| 0 <= k < addrs.len() ==> (#[trigger] addrs[k]) % PAGE == 0
    &&& forall|k: int| 0 <= k < pages.len() ==> (#[trigger] pages[k]).len() == ENTRIES_PER_PAGE
    &&& forall|k: int, s: int| 0 <= k < pages.len() && 0 <= s < per_page() && per_page() * k + s < es.len()
            ==> #[trigger] pages[k][s] == es[per_page() * k + s]
    &&& forall|k: int| 0 <= k < pages.len() - 1 ==> #[trigger] pages[k][per_page()] == addrs[k + 1] | IND_INDIRECTION
}

/// List page `addrs[k]` holds `pages[k]`.
pub open spec fn holds_pages(mem: ListMemory, addrs: Seq<u64>, pages: Seq<Seq<u64>>) -> bool {
    forall|k: int| 0 <= k < pages.len() ==> mem(#[trigger] addrs[k]) == pages[k]
}

/// A flat list that ends where it should: neither an end nor a link before its last entry, which is the end.
pub open spec fn flat_ok(es: Seq<u64>) -> bool {
    &&& es.len() >= 1
    &&& spec_step(es[es.len() - 1]) == Step::Done
    &&& forall|j: int| 0 <= j < es.len() - 1 ==> !(#[trigger] spec_step(es[j]) is Done) && !(spec_step(es[j]) is Indirection)
}

/// The copies as the shipped walk reports them: (destination, staged page).
pub open spec fn pairs(cs: Seq<Copy>) -> Seq<(u64, u64)> {
    cs.map_values(|c: Copy| (c.dst as u64, c.src))
}

/// Every destination is an address, and moving on by a page from it stays one.
pub open spec fn in_range(cs: Seq<Copy>) -> bool {
    forall|k: int| 0 <= k < cs.len() ==> 0 <= (#[trigger] cs[k]).dst && cs[k].dst + PAGE <= u64::MAX
}

/// Entries the walk reads from flat index `i` to the end: the rest of the list and the links it crosses.
pub open spec fn steps_left(len: int, i: int) -> int {
    (len - i) + ((len - 1) / per_page() - i / per_page())
}

/// From flat index `i` (slot `i % 511` of page `i / 511`) with destination `dest`, the paged walk makes the flat walk's
/// copies and ends well.
proof fn lemma_paged_is_flat(es: Seq<u64>, addrs: Seq<u64>, pages: Seq<Seq<u64>>, mem: ListMemory, i: int, dest: int,
    fuel: nat)
    requires
        laid_out(es, addrs, pages),
        holds_pages(mem, addrs, pages),
        flat_ok(es),
        0 <= i < es.len(),
        0 <= dest <= u64::MAX,
        in_range(walk(es, i, dest)),
        fuel >= steps_left(es.len() as int, i),
    ensures
        walk_from(mem, pages[i / per_page()], i % per_page(), Some(dest as u64), fuel)
            == (pairs(walk(es, i, dest)), Ok::<(), WalkError>(())),
    decreases es.len() - i, 1int,
{
    lemma_div511(i);
    lemma_div511(es.len() - 1);
    let k = i / per_page();
    let s = i % per_page();
    assert(per_page() * k + s == i);
    assert(0 <= k < pages.len());
    assert(pages[k][s] == es[i]);
    assert(fuel > 0);
    let left = (fuel - 1) as nat;
    if i == es.len() - 1 {
        assert(spec_step(es[i]) == Step::Done);
        assert(pairs(walk(es, i, dest)) =~= Seq::<(u64, u64)>::empty());
    } else {
        assert(!(spec_step(es[i]) is Done) && !(spec_step(es[i]) is Indirection));
        match spec_step(es[i]) {
            Step::Destination(a) => {
                assert(walk(es, i, dest) == walk(es, i + 1, a as int));
                lemma_paged_next(es, addrs, pages, mem, i, a as int, left);
            },
            Step::Copy(a) => {
                let tail = walk(es, i + 1, dest + (PAGE as int));
                assert(walk(es, i, dest) == seq![Copy { dst: dest, src: a }] + tail);
                assert(walk(es, i, dest)[0] == Copy { dst: dest, src: a });
                assert(in_range(tail)) by {
                    assert forall|j: int| 0 <= j < tail.len() implies 0 <= (#[trigger] tail[j]).dst && tail[j].dst + PAGE <= u64::MAX by {
                        assert(tail[j] == walk(es, i, dest)[j + 1]);
                    }
                }
                lemma_paged_next(es, addrs, pages, mem, i, dest + (PAGE as int), left);
                assert(((dest as u64) + PAGE) as u64 == (dest + (PAGE as int)) as u64);
                assert(pairs(walk(es, i, dest)) =~= seq![(dest as u64, a)] + pairs(tail));
            },
            Step::Nothing => {
                assert(walk(es, i, dest) == walk(es, i + 1, dest));
                lemma_paged_next(es, addrs, pages, mem, i, dest, left);
            },
            _ => {},
        }
    }
}

/// After the entry at flat index `i`, the paged walk goes on at slot `i % 511 + 1` (the link slot if `i` ends its page)
/// and makes the flat walk's copies from `i + 1`.
proof fn lemma_paged_next(es: Seq<u64>, addrs: Seq<u64>, pages: Seq<Seq<u64>>, mem: ListMemory, i: int, dest: int,
    fuel: nat)
    requires
        laid_out(es, addrs, pages),
        holds_pages(mem, addrs, pages),
        flat_ok(es),
        0 <= i,
        i + 1 < es.len(),
        0 <= dest <= u64::MAX,
        in_range(walk(es, i + 1, dest)),
        fuel + 1 >= steps_left(es.len() as int, i),
    ensures
        walk_from(mem, pages[i / per_page()], i % per_page() + 1, Some(dest as u64), fuel)
            == (pairs(walk(es, i + 1, dest)), Ok::<(), WalkError>(())),
    decreases es.len() - i, 0int,
{
    lemma_div511(i);
    lemma_div511(i + 1);
    lemma_div511(es.len() - 1);
    let k = i / per_page();
    let s = i % per_page();
    assert(per_page() * k + s == i);
    if s + 1 < per_page() {
        assert((i + 1) / per_page() == k && (i + 1) % per_page() == s + 1);
        lemma_paged_is_flat(es, addrs, pages, mem, i + 1, dest, fuel);
    } else {
        // The link slot: on to page k + 1, which holds flat index i + 1 in its first slot.
        assert((i + 1) / per_page() == k + 1 && (i + 1) % per_page() == 0);
        assert(k + 1 < pages.len());
        let next = addrs[k + 1];
        assert(next % PAGE == 0);
        lemma_page_flag(next);
        assert(pages[k][per_page()] == next | IND_INDIRECTION);
        assert(step_spec(pages[k][s + 1]) == Step::Indirection(next));
        assert(mem(next) == pages[k + 1]);
        assert(fuel > 0);
        lemma_paged_is_flat(es, addrs, pages, mem, i + 1, dest, (fuel - 1) as nat);
    }
}

/// The list `build_list` makes ends where it should.
proof fn lemma_list_flat_ok(w: u64, image: Seq<u64>, d: u64, initrd: Seq<u64>)
    requires
        aligned(w),
        aligned(d),
        all_aligned(image),
        all_aligned(initrd),
    ensures
        flat_ok(list(w, image, d, initrd)),
{
    let es = list(w, image, d, initrd);
    let n = image.len() as int;
    let m = initrd.len() as int;
    lemma_flag_bits(w);
    lemma_flag_bits(d);
    assert(es[es.len() - 1] == IND_DONE);
    assert forall|j: int| 0 <= j < es.len() - 1 implies !(#[trigger] spec_step(es[j]) is Done) && !(spec_step(es[j]) is Indirection) by {
        if 1 <= j < 1 + n {
            assert(es[j] == image[j - 1] | IND_SOURCE);
            lemma_flag_bits(image[j - 1]);
        } else if 2 + n <= j < 2 + n + m {
            assert(es[j] == initrd[j - 2 - n] | IND_SOURCE);
            lemma_flag_bits(initrd[j - 2 - n]);
        }
    }
}

/// 4. The SHIPPED `walk_entries`, walking the list `build_list` makes, laid out in list pages as `build_pages` (the
/// shipped `ListPage`) lays it out, ends well and copies exactly theorem 1's pages: the image's to the window and the
/// initramfs's to its destination, each staged page once, in order. So theorems 1 to 3 hold of the shipped walk, given
/// enough `limit` and destinations that end inside the address space.
proof fn theorem_shipped_walk(w: u64, image: Seq<u64>, d: u64, initrd: Seq<u64>, addrs: Seq<u64>, pages: Seq<Seq<u64>>,
    mem: ListMemory, limit: nat)
    requires
        aligned(w),
        aligned(d),
        all_aligned(image),
        all_aligned(initrd),
        w + image.len() * (PAGE as int) <= u64::MAX,
        d + initrd.len() * (PAGE as int) <= u64::MAX,
        laid_out(list(w, image, d, initrd), addrs, pages),
        holds_pages(mem, addrs, pages),
        limit >= steps_left(list(w, image, d, initrd).len() as int, 0),
    ensures
        walk_from(mem, mem(addrs[0]), 0, None, limit)
            == (pairs(copies(w as int, image) + copies(d as int, initrd)), Ok::<(), WalkError>(())),
        no_overflow(walk_from(mem, mem(addrs[0]), 0, None, limit).0),
{
    let es = list(w, image, d, initrd);
    let n = image.len() as int;
    let cs = copies(w as int, image) + copies(d as int, initrd);
    theorem_copies(w, image, d, initrd);
    lemma_list_flat_ok(w, image, d, initrd);
    lemma_flag_bits(w);
    assert(es[0] == w | IND_DESTINATION);
    assert(spec_step(es[0]) == Step::Destination(w));
    assert(walk(es, 0, 0) == walk(es, 1, w as int));
    assert(in_range(cs)) by {
        assert forall|i: int| 0 <= i < cs.len() implies 0 <= (#[trigger] cs[i]).dst && cs[i].dst + PAGE <= u64::MAX by {
            if i < n {
                assert(cs[i].dst == w as int + i * (PAGE as int));
                assert(i * (PAGE as int) + (PAGE as int) <= n * (PAGE as int)) by (nonlinear_arith) requires i < n;
                assert(0 <= i * (PAGE as int)) by (nonlinear_arith) requires 0 <= i;
            } else {
                let k = i - n;
                assert(cs[i].dst == d as int + k * (PAGE as int));
                assert(k * (PAGE as int) + (PAGE as int) <= initrd.len() * (PAGE as int)) by (nonlinear_arith)
                    requires k < initrd.len();
                assert(0 <= k * (PAGE as int)) by (nonlinear_arith) requires 0 <= k;
            }
        }
    }
    assert(mem(addrs[0]) == pages[0]);
    assert(pages[0][0] == es[0]);
    lemma_paged_next(es, addrs, pages, mem, 0, w as int, (limit - 1) as nat);
    lemma_div511(0);
    let out = pairs(cs);
    assert forall|k: int| 0 <= k < out.len() implies (#[trigger] out[k]).0 + PAGE <= u64::MAX by {
        assert(out[k] == (cs[k].dst as u64, cs[k].src));
    }
}

/// The shipped `ListPage`, driven as `build_list` drives it (push each entry; when only the link slot is left, link to
/// the next list page at `addrs`, write the page out, start a new one and push the entry there), lays the flat list out
/// as `laid_out` says. This mirrors the kernel's loop (kernel/patches/1055); that loop itself is not verified.
fn build_pages(es: &Vec<u64>, addrs: &Vec<u64>) -> (pages: Vec<[u64; ENTRIES_PER_PAGE]>)
    requires
        es.len() >= 1,
        addrs.len() >= (es.len() - 1) / per_page() + 1,
        forall|k: int| 0 <= k < addrs.len() ==> (#[trigger] addrs@[k]) % PAGE == 0,
    ensures
        laid_out(es@, addrs@, pages@.map_values(|p: [u64; ENTRIES_PER_PAGE]| p@)),
{
    let mut pages: Vec<[u64; ENTRIES_PER_PAGE]> = Vec::new();
    let mut page = ListPage::new();
    let mut j: usize = 0;
    proof { lemma_div511((es.len() - 1) as int); }
    while j < es.len()
        invariant
            j <= es.len(),
            es.len() >= 1,
            addrs.len() >= (es.len() - 1) / per_page() + 1,
            forall|k: int| 0 <= k < addrs.len() ==> (#[trigger] addrs@[k]) % PAGE == 0,
            page.wf(),
            per_page() == 511,
            j == 0 ==> pages.len() == 0,
            j > 0 ==> per_page() * pages.len() < j,
            j <= per_page() * pages.len() + per_page(),
            page.view() == es@.subrange(per_page() * pages.len(), j as int),
            forall|k: int| 0 <= k < pages.len() ==> (#[trigger] pages@[k])@.len() == ENTRIES_PER_PAGE,
            forall|k: int, s: int| 0 <= k < pages.len() && 0 <= s < per_page()
                ==> #[trigger] pages@[k]@[s] == es@[per_page() * k + s],
            forall|k: int| 0 <= k < pages.len() ==> #[trigger] pages@[k]@[per_page()] == addrs@[k + 1] | IND_INDIRECTION,
        decreases es.len() - j,
    {
        let e = es[j];
        proof { page.lemma_wf(); }
        if !page.push(e) {
            proof { lemma_div511((es.len() - 1) as int); }
            let next = addrs[pages.len() + 1];
            page.link(next);
            let full = *page.entries();
            pages.push(full);
            page = ListPage::new();
            let pushed = page.push(e);
            assert(pushed);
            assert(page.view() =~= es@.subrange(per_page() * pages.len(), j as int + 1));
        } else {
            assert(page.view() =~= es@.subrange(per_page() * pages.len(), j as int + 1));
        }
        j += 1;
    }
    proof { page.lemma_wf(); lemma_div511((es.len() - 1) as int); }
    let last = *page.entries();
    pages.push(last);
    let ghost views = pages@.map_values(|p: [u64; ENTRIES_PER_PAGE]| p@);
    assert forall|k: int, s: int| 0 <= k < views.len() && 0 <= s < per_page() && per_page() * k + s < es.len()
        implies #[trigger] views[k][s] == es@[per_page() * k + s] by {
        if k == views.len() - 1 {
            assert(page.view()[s] == es@[per_page() * k + s]);
        }
    }
    pages
}

/// Theorem 4 through the shipped function itself: for any `read` that returns list page `p` as `mem(p)`, with the list
/// laid out as `build_pages` lays it out, the shipped `walk_entries` returns Ok and appends exactly the copies of
/// theorem 1.
fn walk_of_the_built_list(head: u64, read: impl Fn(u64) -> [u64; ENTRIES_PER_PAGE], out: &mut Vec<(u64, u64)>,
    limit: usize, Ghost(w): Ghost<u64>, Ghost(image): Ghost<Seq<u64>>, Ghost(d): Ghost<u64>,
    Ghost(initrd): Ghost<Seq<u64>>, Ghost(addrs): Ghost<Seq<u64>>, Ghost(pages): Ghost<Seq<Seq<u64>>>,
    Ghost(mem): Ghost<ListMemory>) -> (r: Result<(), WalkError>)
    requires
        aligned(w),
        aligned(d),
        all_aligned(image),
        all_aligned(initrd),
        w + image.len() * (PAGE as int) <= u64::MAX,
        d + initrd.len() * (PAGE as int) <= u64::MAX,
        laid_out(list(w, image, d, initrd), addrs, pages),
        holds_pages(mem, addrs, pages),
        head == addrs[0],
        limit >= steps_left(list(w, image, d, initrd).len() as int, 0),
        forall|p: u64| read.requires((p,)),
        forall|p: u64, e: [u64; ENTRIES_PER_PAGE]| read.ensures((p,), e) ==> e@ == mem(p),
    ensures
        r == Ok::<(), WalkError>(()),
        final(out)@ == old(out)@ + pairs(copies(w as int, image) + copies(d as int, initrd)),
{
    proof { theorem_shipped_walk(w, image, d, initrd, addrs, pages, mem, limit as nat); }
    #[verus_spec(with Ghost(mem))]
    walk_entries(head, read, out, limit)
}

} // verus!

fn main() {}
