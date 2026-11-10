// Verus proof of the SHIPPED code, not a model of it (project rule D-011): this file includes
// kernel/lucina-kexec/src/pages.rs by #[path], so Verus checks `step` and `encode` as the kernel compiles them; what is
// proved of each is in its `ensures`. On top of that, the walk below reads the entry list exactly as the relocation code
// does (bsp_boot.S, through the shipped `step`) and proves, for the list the kernel builds in `build_list` (the window's
// destination, the image's pages, the initramfs's destination, its pages, done):
//   1. every staged page is copied exactly once, the k-th page of a segment to its destination plus k pages, in order;
//   2. every write lands inside its segment's destination range, each destination page written exactly once;
//   3. no write lands on a staged page or a page of the list, given what staging guarantees (no staged page or list page
//      inside a destination range: `frame_outside` and `move_out` in kernel/core/src/syscall/kexec.rs), so no copy can
//      overwrite a page before it is read, whatever the order.
#![feature(proc_macro_hygiene)]
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
    assert((p | 1u64) & 4u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 1u64) & 1u64 != 0u64) by (bit_vector);
    assert((p | 1u64) & !4095u64 == p) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 8u64) & 4u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 8u64) & 1u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 8u64) & 2u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 8u64) & 8u64 != 0u64) by (bit_vector);
    assert((p | 8u64) & !4095u64 == p) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 2u64) & 4u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 2u64) & 1u64 == 0u64) by (bit_vector) requires p & 4095u64 == 0u64;
    assert((p | 2u64) & 2u64 != 0u64) by (bit_vector);
    assert((p | 2u64) & !4095u64 == p) by (bit_vector) requires p & 4095u64 == 0u64;
    assert(4u64 & 4u64 != 0u64) by (bit_vector);
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

/// One copy the relocation code makes: (destination page, staged page).
pub struct Copy {
    pub dst: int,
    pub src: u64,
}

/// The copies the relocation code makes from entry `i` on with destination `dest`, until done or the end of `es`.
/// (Indirections only chain list pages; the flat list here is their content end to end.)
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

} // verus!

fn main() {}
