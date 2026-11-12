// SPDX-License-Identifier: GPL-2.0-only
//! The lifetime, purge and byte-budget rules of a shareable pixel buffer (`lucina-buf`), and the size and sync-row arithmetic,
//! as pure functions that Verus checks as shipped (`verification/juno-surface/rules.rs`; the attributes are only seen by Verus,
//! rustc ignores them). What each function promises is in its `ensures`.
//!
//! One buffer is a [`Buf`]; the pool of bytes it is charged to is a [`Budget`]. [`step`] applies one [`Op`] to both:
//!
//! - `refs` is the number of open descriptors (`fds`) plus mappings plus GPU slots. A GPU batch that names the buffer
//!   (`gpu_pending`) keeps it alive on its own, so an app that dies while the GPU still reads its buffer does not free it.
//! - The buffer is *settled* the moment `refs == 0` and `gpu_pending == 0`: `Freed` (its bytes go back to the pool), or
//!   `Quarantined` when the GPU is marked failed (its bytes stay charged and are never handed out again).
//! - A buffer that is purgeable, mapped nowhere, named by no running batch, bound to no GPU slot and whose GPU has not failed
//!   may be `Purged`: its bytes go back to the pool, its descriptors stay valid, and it cannot be mapped, bound or named by a
//!   batch until `SetPurgeable(false)` restores it (charging the bytes again, contents lost) or the last reference goes.
//!
//! While the GPU can reach the buffer - a slot names it, or the GPU is marked failed - its bytes are never given back: a purge
//! would leave that slot pointing at pages the pool hands to someone else, or bypass the quarantine the failed GPU's bytes stay in.
//!
//! Not proved here: that the kernel's counters really are these numbers, the cache maintenance itself, or the pages' contents.
#![allow(unexpected_cfgs)]
// Written in the plain forms Verus reads (no `?`, `div_ceil`, `map` or `+=` on the proved values).
#![allow(
    clippy::manual_div_ceil,
    clippy::question_mark,
    clippy::manual_map,
    clippy::assign_op_pattern
)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Dimensions (width, height) are below this.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_DIM: u32 = 0x4000;
/// Pitch is a multiple of this (Asahi's and the GPU's alignment).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PITCH_ALIGN: u32 = 64;
/// A buffer's size is a multiple of this.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PAGE: u64 = 4096;
/// The largest pitch: 4 bytes of the widest row, rounded up to [`PITCH_ALIGN`].
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_PITCH: u32 = 0x1_0000;

/// Where a buffer is in its life.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Has memory; may be mapped, bound and named by batches.
    Live,
    /// Gave its memory back under pressure; the object still exists.
    Purged,
    /// No reference left; bytes returned to the pool.
    Freed,
    /// No reference left after a GPU failure; bytes stay charged for good.
    Quarantined,
}

/// The state of one buffer.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buf {
    /// Open descriptors + mappings + GPU slots.
    pub refs: u32,
    /// Open descriptors (part of `refs`).
    pub fds: u32,
    /// Mappings (part of `refs`).
    pub mappings: u32,
    /// GPU slots that import the buffer (part of `refs`).
    pub gpu_slots: u32,
    /// Running GPU batches that name the buffer. Each holds the buffer until its fence retires.
    pub gpu_pending: u32,
    /// Slot uses inside those batches.
    pub gpu_slots_in_batch: u32,
    pub purgeable: bool,
    pub phase: Phase,
    /// Bytes charged while `Live` or `Quarantined`.
    pub size: u64,
    /// The GPU is marked failed: a buffer reaching zero references is quarantined, not freed.
    pub gpu_failed: bool,
    /// Set when a purged buffer came back: its pixels are zeros, the owner must repaint.
    pub contents_lost: bool,
    /// Whether `size` is counted in the budget's `used` right now.
    pub charged: bool,
}

/// The pool of bytes buffers are charged to.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    pub used: u64,
    pub limit: u64,
}

/// What can be done to a buffer.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// A descriptor is duplicated or received.
    Open,
    /// A descriptor is closed (also by process death).
    Close,
    Map,
    Unmap,
    GpuBind,
    GpuUnbind,
    /// A batch naming this many of the buffer's slots is submitted.
    BatchStart(u32),
    /// A batch naming this many slots has passed its fence.
    BatchRetire(u32),
    SetPurgeable(bool),
    /// Memory pressure: give the bytes back. Needs no GPU slot bound and a GPU that has not failed, on top of purgeable,
    /// unmapped and named by no running batch.
    Purge,
    /// The GPU is marked failed.
    GpuFail,
}

/// Why an operation is refused; the state is unchanged.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The buffer is already freed or quarantined.
    Dead,
    /// The buffer is purged (map, bind, batch).
    Purged,
    /// The budget has no room (new buffer, restore).
    NoRoom,
    /// A counter would overflow.
    Overflow,
    /// Close with no descriptor open.
    NoDescriptor,
    /// Unmap with nothing mapped.
    NotMapped,
    /// Unbind with no slot bound.
    NotBound,
    /// Retire with no batch running.
    NoBatch,
    /// A retire that names more slots than the batches hold, or leaves slots named by no batch.
    BadBatch,
    /// Purge without its conditions.
    PurgeDenied,
    /// Size is zero.
    BadSize,
    /// The budget would go below zero (cannot happen from a state `wf` accepts).
    Accounting,
}

/// The budget has no room.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoRoom;

/// More returned than charged.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Underflow;

#[cfg(verus_keep_ghost)]
verus! {

/// Bytes of the buffer that are counted in the budget.
pub open spec fn cb(b: Buf) -> int {
    if b.charged { b.size as int } else { 0 }
}

/// A buffer and its budget fit together, except that a `Live` or `Purged` buffer may still sit at zero references (it is
/// about to be settled).
pub open spec fn wf_base(b: Buf, bud: Budget) -> bool {
    &&& b.fds as int + b.mappings as int + b.gpu_slots as int == b.refs as int
    &&& b.gpu_slots_in_batch > 0 ==> b.gpu_pending > 0
    &&& b.charged <==> (b.phase is Live || b.phase is Quarantined)
    &&& b.phase is Purged ==> b.purgeable && b.mappings == 0 && b.gpu_slots_in_batch == 0 && b.gpu_pending == 0
    &&& b.phase is Purged ==> b.gpu_slots == 0
    &&& (b.phase is Freed || b.phase is Quarantined) ==> b.refs == 0 && b.gpu_pending == 0
    &&& b.charged ==> b.size <= bud.used
    &&& bud.used <= bud.limit
}

/// A buffer that is alive keeps at least one reference or one running batch.
pub open spec fn settled(b: Buf) -> bool {
    (b.phase is Live || b.phase is Purged) ==> (b.refs > 0 || b.gpu_pending > 0)
}

/// What every state the rules produce satisfies.
pub open spec fn wf(b: Buf, bud: Budget) -> bool {
    wf_base(b, bud) && settled(b)
}

/// The pool outside this buffer is untouched: bytes move between the buffer and the pool, none appear or vanish.
pub open spec fn conserved(b: Buf, bud: Budget, nb: Buf, nbud: Budget) -> bool {
    &&& nbud.limit == bud.limit
    &&& nb.size == b.size
    &&& nbud.used as int - cb(nb) == bud.used as int - cb(b)
}

/// The buffer `new(size, ..)` creates: one descriptor and nothing else, live, charged, not purgeable, GPU not failed.
pub open spec fn fresh(size: u64) -> Buf {
    Buf {
        refs: 1,
        fds: 1,
        mappings: 0,
        gpu_slots: 0,
        gpu_pending: 0,
        gpu_slots_in_batch: 0,
        purgeable: false,
        phase: Phase::Live,
        size,
        gpu_failed: false,
        contents_lost: false,
        charged: true,
    }
}

/// What a buffer becomes once its counters have moved: at zero references and no running batch a `Live` buffer is `Freed`
/// (uncharged) or, on a failed GPU, `Quarantined` (still charged); a `Purged` one is `Freed` (it had no bytes charged).
/// Otherwise it is unchanged.
pub open spec fn settled_buf(n: Buf) -> Buf {
    if n.refs == 0 && n.gpu_pending == 0 {
        match n.phase {
            Phase::Live => if n.gpu_failed {
                Buf { phase: Phase::Quarantined, ..n }
            } else {
                Buf { phase: Phase::Freed, charged: false, ..n }
            },
            Phase::Purged => Buf { phase: Phase::Freed, ..n },
            _ => n,
        }
    } else {
        n
    }
}

/// When `step` accepts `op` (R-2). Nothing is accepted on a `Freed` or `Quarantined` buffer. Close, Unmap and GpuUnbind are
/// accepted whenever their own counter is above zero, and a retire whenever it names no more slots than the running batches
/// hold and the last batch takes the last slot uses with it: the kernel ignores the result of Close and Unmap, so a refusal
/// there would leak the buffer. Open, Map, GpuBind and BatchStart are refused only on a purged buffer (not Open) or at a
/// counter's limit; a restore only when the budget has no room; Purge only without its conditions; GpuFail never.
pub open spec fn accepts(b: Buf, bud: Budget, op: Op) -> bool {
    &&& !(b.phase is Freed || b.phase is Quarantined)
    &&& match op {
        Op::Open => b.refs < u32::MAX,
        Op::Close => b.fds > 0,
        Op::Map => !(b.phase is Purged) && b.refs < u32::MAX,
        Op::Unmap => b.mappings > 0,
        Op::GpuBind => !(b.phase is Purged) && b.refs < u32::MAX,
        Op::GpuUnbind => b.gpu_slots > 0,
        Op::BatchStart(s) => !(b.phase is Purged) && b.gpu_pending < u32::MAX
            && s as int + b.gpu_slots_in_batch as int <= u32::MAX as int,
        Op::BatchRetire(s) => b.gpu_pending > 0 && s <= b.gpu_slots_in_batch
            && (s == b.gpu_slots_in_batch || b.gpu_pending > 1),
        Op::SetPurgeable(on) => on || !(b.phase is Purged) || bud.used as int + b.size as int <= bud.limit as int,
        Op::Purge => b.phase is Live && b.purgeable && b.mappings == 0 && b.gpu_slots_in_batch == 0 && b.gpu_pending == 0
            && b.gpu_slots == 0 && !b.gpu_failed,
        Op::GpuFail => true,
    }
}

/// The state an accepted `op` leaves (R-1): exactly the named fields move, every other field is the same.
/// - Open / Map / GpuBind: `fds` / `mappings` / `gpu_slots` and `refs` up by one.
/// - Close / Unmap / GpuUnbind: the same counter and `refs` down by one, then settled.
/// - BatchStart(s): `gpu_pending` up by one, `gpu_slots_in_batch` up by `s`; BatchRetire(s): both down, then settled.
/// - SetPurgeable(true): `purgeable`; a live buffer's `contents_lost` is cleared (the app has seen the loss).
/// - SetPurgeable(false): `purgeable` cleared; a purged buffer comes back `Live`, charged again, `contents_lost`.
/// - Purge: `Purged`, uncharged (`purgeable` stays set). GpuFail: `gpu_failed`.
pub open spec fn post(b: Buf, op: Op) -> Buf {
    match op {
        Op::Open => Buf { fds: (b.fds + 1) as u32, refs: (b.refs + 1) as u32, ..b },
        Op::Close => settled_buf(Buf { fds: (b.fds - 1) as u32, refs: (b.refs - 1) as u32, ..b }),
        Op::Map => Buf { mappings: (b.mappings + 1) as u32, refs: (b.refs + 1) as u32, ..b },
        Op::Unmap => settled_buf(Buf { mappings: (b.mappings - 1) as u32, refs: (b.refs - 1) as u32, ..b }),
        Op::GpuBind => Buf { gpu_slots: (b.gpu_slots + 1) as u32, refs: (b.refs + 1) as u32, ..b },
        Op::GpuUnbind => settled_buf(Buf { gpu_slots: (b.gpu_slots - 1) as u32, refs: (b.refs - 1) as u32, ..b }),
        Op::BatchStart(s) => Buf {
            gpu_pending: (b.gpu_pending + 1) as u32,
            gpu_slots_in_batch: (b.gpu_slots_in_batch + s) as u32,
            ..b
        },
        Op::BatchRetire(s) => settled_buf(Buf {
            gpu_pending: (b.gpu_pending - 1) as u32,
            gpu_slots_in_batch: (b.gpu_slots_in_batch - s) as u32,
            ..b
        }),
        Op::SetPurgeable(on) => if on {
            Buf { purgeable: true, contents_lost: if b.phase is Live { false } else { b.contents_lost }, ..b }
        } else if b.phase is Purged {
            Buf { phase: Phase::Live, charged: true, purgeable: false, contents_lost: true, ..b }
        } else {
            Buf { purgeable: false, ..b }
        },
        Op::Purge => Buf { phase: Phase::Purged, charged: false, ..b },
        Op::GpuFail => Buf { gpu_failed: true, ..b },
    }
}

/// `wf` without the budget: the part of a buffer's invariant that is about the buffer alone.
pub open spec fn wf_own(b: Buf) -> bool {
    &&& b.fds as int + b.mappings as int + b.gpu_slots as int == b.refs as int
    &&& b.gpu_slots_in_batch > 0 ==> b.gpu_pending > 0
    &&& b.charged <==> (b.phase is Live || b.phase is Quarantined)
    &&& b.phase is Purged ==> b.purgeable && b.mappings == 0 && b.gpu_slots_in_batch == 0 && b.gpu_pending == 0
    &&& b.phase is Purged ==> b.gpu_slots == 0
    &&& (b.phase is Freed || b.phase is Quarantined) ==> b.refs == 0 && b.gpu_pending == 0
    &&& settled(b)
}

/// The bytes the buffers of a pool have charged.
pub open spec fn sum_cb(bufs: Seq<Buf>) -> int
    decreases bufs.len(),
{
    if bufs.len() == 0 {
        0
    } else {
        sum_cb(bufs.drop_last()) + cb(bufs.last())
    }
}

/// A pool of buffers sharing one budget (R-3): every buffer is well formed on its own, the budget's `used` is exactly the sum
/// of the bytes the buffers have charged (live and quarantined ones), and that sum is within the limit.
pub open spec fn pool_ok(bufs: Seq<Buf>, bud: Budget) -> bool {
    &&& forall|i: int| 0 <= i < bufs.len() ==> wf_own(#[trigger] bufs[i])
    &&& sum_cb(bufs) == bud.used as int
    &&& bud.used <= bud.limit
}

/// `wf_own` is `wf` minus its two budget clauses.
pub proof fn lemma_wf_own(b: Buf, bud: Budget)
    ensures
        wf(b, bud) <==> (wf_own(b) && (b.charged ==> b.size <= bud.used) && bud.used <= bud.limit),
{
}

pub proof fn lemma_sum_nonneg(s: Seq<Buf>)
    ensures
        sum_cb(s) >= 0,
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_sum_nonneg(s.drop_last());
    }
}

pub proof fn lemma_sum_member(s: Seq<Buf>, i: int)
    requires
        0 <= i < s.len(),
    ensures
        cb(s[i]) <= sum_cb(s),
    decreases s.len(),
{
    if i == s.len() - 1 {
        lemma_sum_nonneg(s.drop_last());
    } else {
        lemma_sum_member(s.drop_last(), i);
    }
}

pub proof fn lemma_sum_update(s: Seq<Buf>, i: int, x: Buf)
    requires
        0 <= i < s.len(),
    ensures
        sum_cb(s.update(i, x)) == sum_cb(s) - cb(s[i]) + cb(x),
    decreases s.len(),
{
    let t = s.update(i, x);
    if i == s.len() - 1 {
        assert(t.drop_last() =~= s.drop_last());
    } else {
        lemma_sum_update(s.drop_last(), i, x);
        assert(t.drop_last() =~= s.drop_last().update(i, x));
    }
}

pub proof fn lemma_sum_remove(s: Seq<Buf>, i: int)
    requires
        0 <= i < s.len(),
    ensures
        sum_cb(s.remove(i)) == sum_cb(s) - cb(s[i]),
    decreases s.len(),
{
    if i == s.len() - 1 {
        assert(s.remove(i) =~= s.drop_last());
    } else {
        lemma_sum_remove(s.drop_last(), i);
        assert(s.remove(i).drop_last() =~= s.drop_last().remove(i));
    }
}

/// An empty pool at `used == 0` is a pool.
pub proof fn lemma_pool_empty(bud: Budget)
    requires
        bud.used == 0,
    ensures
        pool_ok(Seq::<Buf>::empty(), bud),
{
}

/// Every buffer of a pool meets `step`'s precondition against the pool's one budget, and the charged bytes of all of them
/// together stay within the limit.
pub proof fn lemma_pool_member(bufs: Seq<Buf>, bud: Budget, i: int)
    requires
        pool_ok(bufs, bud),
        0 <= i < bufs.len(),
    ensures
        wf(bufs[i], bud),
        sum_cb(bufs) <= bud.limit as int,
{
    lemma_sum_member(bufs, i);
    lemma_wf_own(bufs[i], bud);
}

/// One accepted `step` on buffer `i` (whose ensures give `wf` and `conserved`) keeps the pool: the other buffers are untouched
/// and still well formed against the new budget, whose `used` is again the sum of what all buffers charge.
pub proof fn lemma_pool_step(bufs: Seq<Buf>, bud: Budget, i: int, nb: Buf, nbud: Budget)
    requires
        pool_ok(bufs, bud),
        0 <= i < bufs.len(),
        wf(nb, nbud),
        conserved(bufs[i], bud, nb, nbud),
    ensures
        pool_ok(bufs.update(i, nb), nbud),
{
    lemma_sum_update(bufs, i, nb);
    lemma_wf_own(nb, nbud);
    let t = bufs.update(i, nb);
    assert forall|j: int| 0 <= j < t.len() implies wf_own(#[trigger] t[j]) by {
        if j != i {
            assert(t[j] == bufs[j]);
        }
    }
}

/// A buffer that `new` created (charged against the same budget) joins the pool.
pub proof fn lemma_pool_new(bufs: Seq<Buf>, bud: Budget, b: Buf, nbud: Budget)
    requires
        pool_ok(bufs, bud),
        wf(b, nbud),
        nbud.used as int == bud.used as int + cb(b),
    ensures
        pool_ok(bufs.push(b), nbud),
{
    let t = bufs.push(b);
    assert(t.drop_last() =~= bufs);
    lemma_wf_own(b, nbud);
    assert forall|j: int| 0 <= j < t.len() implies wf_own(#[trigger] t[j]) by {
        if j < bufs.len() {
            assert(t[j] == bufs[j]);
        }
    }
}

/// A freed buffer can leave the pool; the budget does not change (it charged nothing). A quarantined one cannot leave
/// without breaking `used == sum`: its bytes stay charged for good.
pub proof fn lemma_pool_drop(bufs: Seq<Buf>, bud: Budget, i: int)
    requires
        pool_ok(bufs, bud),
        0 <= i < bufs.len(),
        bufs[i].phase is Freed,
    ensures
        pool_ok(bufs.remove(i), bud),
{
    lemma_sum_remove(bufs, i);
    let t = bufs.remove(i);
    assert(wf_own(bufs[i]));
    assert forall|j: int| 0 <= j < t.len() implies wf_own(#[trigger] t[j]) by {
        if j < i {
            assert(t[j] == bufs[j]);
        } else {
            assert(t[j] == bufs[j + 1]);
        }
    }
}

/// A sync rectangle `(y, h)` lies inside a buffer of `height` rows.
pub open spec fn row_inside(r: (u32, u32), height: u32) -> bool {
    0 < r.1 && r.0 < height && r.0 as int + r.1 as int <= height as int
}

/// The rows a list of sync rectangles names, counting a row once per rectangle.
pub open spec fn rows_sum(rows: Seq<(u32, u32)>) -> int
    decreases rows.len(),
{
    if rows.len() == 0 {
        0
    } else {
        rows_sum(rows.drop_last()) + rows.last().1
    }
}

pub proof fn lemma_rows_prefix_le(s: Seq<(u32, u32)>, k: int)
    requires
        0 <= k <= s.len(),
    ensures
        rows_sum(s.subrange(0, k)) <= rows_sum(s),
    decreases s.len(),
{
    if k == s.len() {
        assert(s.subrange(0, k) =~= s);
    } else {
        lemma_rows_prefix_le(s.drop_last(), k);
        assert(s.drop_last().subrange(0, k) =~= s.subrange(0, k));
    }
}

} // verus!

/// Add `size` bytes to the budget, or say there is no room. Checked: `used + size` is never computed unless it fits.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires bud.used <= bud.limit,
    ensures
        r matches Ok(nb) ==> nb.used as int == bud.used as int + size as int && nb.used <= nb.limit && nb.limit == bud.limit,
        r matches Err(_) ==> bud.used as int + size as int > bud.limit as int,
))]
pub fn charge(bud: Budget, size: u64) -> Result<Budget, NoRoom> {
    if size > bud.limit - bud.used {
        return Err(NoRoom);
    }
    Ok(Budget {
        used: bud.used + size,
        limit: bud.limit,
    })
}

/// Take `size` bytes off the budget; never below zero.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires bud.used <= bud.limit,
    ensures
        r matches Ok(nb) ==> nb.used as int == bud.used as int - size as int && nb.used <= nb.limit && nb.limit == bud.limit,
        r matches Err(_) ==> size > bud.used,
))]
pub fn release(bud: Budget, size: u64) -> Result<Budget, Underflow> {
    if size > bud.used {
        return Err(Underflow);
    }
    Ok(Budget {
        used: bud.used - size,
        limit: bud.limit,
    })
}

/// A new buffer with one descriptor, charged to the budget.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires bud.used <= bud.limit,
    ensures
        r matches Ok((b, nbud)) ==> wf(b, nbud) && b.phase is Live && b.refs == 1 && b.size == size
            && nbud.used as int == bud.used as int + size as int,
        r is Ok <==> (size > 0 && bud.used as int + size as int <= bud.limit as int),
        r matches Ok((b, nbud)) ==> b == fresh(size) && nbud.limit == bud.limit,
        r matches Err(e) ==> ((e is BadSize <==> size == 0) && (e is BadSize || e is NoRoom)),
))]
pub fn new(size: u64, bud: Budget) -> Result<(Buf, Budget), Refusal> {
    if size == 0 {
        return Err(Refusal::BadSize);
    }
    let nbud = match charge(bud, size) {
        Ok(n) => n,
        Err(_) => return Err(Refusal::NoRoom),
    };
    let b = Buf {
        refs: 1,
        fds: 1,
        mappings: 0,
        gpu_slots: 0,
        gpu_pending: 0,
        gpu_slots_in_batch: 0,
        purgeable: false,
        phase: Phase::Live,
        size,
        gpu_failed: false,
        contents_lost: false,
        charged: true,
    };
    Ok((b, nbud))
}

/// What happens when the last reference and the last batch are gone: `Freed` and the bytes returned, or `Quarantined` (GPU
/// failed) and the bytes kept; a purged buffer has no bytes to return. Otherwise nothing.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires wf_base(b, bud), b.phase is Live || b.phase is Purged,
    ensures
        r matches Ok((nb, nbud)) ==> wf(nb, nbud) && conserved(b, bud, nb, nbud)
            && nb.refs == b.refs && nb.gpu_pending == b.gpu_pending
            && (nb.phase is Freed ==> !nb.gpu_failed || b.phase is Purged)
            && (nb.phase is Quarantined ==> nbud.used == bud.used)
            && nb.mappings == b.mappings && nb.gpu_slots == b.gpu_slots && nb.gpu_slots_in_batch == b.gpu_slots_in_batch
            && nb.purgeable == b.purgeable && nb.contents_lost == b.contents_lost
            && (b.phase is Purged ==> nb.phase is Purged || nb.phase is Freed)
            && (b.phase is Live ==> !(nb.phase is Purged)),
        r is Ok,
        r matches Ok((nb, nbud)) ==> nb == settled_buf(b) && nbud.limit == bud.limit,
))]
fn settle(b: Buf, bud: Budget) -> Result<(Buf, Budget), Refusal> {
    if b.refs != 0 || b.gpu_pending != 0 {
        return Ok((b, bud));
    }
    let mut n = b;
    match b.phase {
        Phase::Live => {
            if b.gpu_failed {
                n.phase = Phase::Quarantined;
                Ok((n, bud))
            } else {
                match release(bud, b.size) {
                    Ok(nbud) => {
                        n.phase = Phase::Freed;
                        n.charged = false;
                        Ok((n, nbud))
                    }
                    Err(_) => Err(Refusal::Accounting),
                }
            }
        }
        Phase::Purged => {
            n.phase = Phase::Freed;
            Ok((n, bud))
        }
        _ => Ok((b, bud)),
    }
}

/// One operation on one buffer and the budget it is charged to. A refused operation changes nothing (the caller keeps its
/// state). What every accepted one keeps:
/// - `wf`: bytes charged only while `Live` or `Quarantined`, `Freed` and `Quarantined` only at zero references and no pending
///   batch, a `Purged` buffer unmapped, bound to no slot and named by no batch, `used <= limit`;
/// - `conserved`: the pool outside the buffer is untouched, so bytes are returned once, never twice, and a quarantined
///   buffer's are never returned;
/// - a buffer becomes `Purged` only from `Live`, purgeable, unmapped, in no running batch, bound to no GPU slot, its GPU not
///   failed (R-5);
/// - while purged, nothing new maps, binds or names it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires wf(b, bud),
    ensures
        r matches Ok((nb, nbud)) ==> wf(nb, nbud),
        r matches Ok((nb, nbud)) ==> conserved(b, bud, nb, nbud),
        r matches Ok((nb, nbud)) ==> ((nb.phase is Freed || nb.phase is Quarantined) ==> nb.refs == 0 && nb.gpu_pending == 0),
        r matches Ok((nb, nbud)) ==> (nb.phase is Purged && !(b.phase is Purged) ==> b.phase is Live && b.purgeable
            && b.mappings == 0 && b.gpu_slots_in_batch == 0 && b.gpu_pending == 0),
        r matches Ok((nb, nbud)) ==> (b.phase is Purged && nb.phase is Purged ==> nb.mappings == 0 && nb.gpu_pending == 0
            && nb.gpu_slots_in_batch == 0 && nb.gpu_slots <= b.gpu_slots),
        r matches Ok((nb, nbud)) ==> (nb.phase is Quarantined ==> nbud.used == bud.used),
        r matches Ok((nb, nbud)) ==> (nb.phase is Freed && b.phase is Live ==> nbud.used as int == bud.used as int - b.size as int),
        r matches Ok((nb, nbud)) ==> (nb.phase is Freed && b.phase is Purged ==> nbud.used == bud.used),
        r matches Ok((nb, nbud)) ==> (nb.phase is Purged && b.phase is Live ==> nbud.used as int == bud.used as int - b.size as int),
        r matches Ok((nb, nbud)) ==> (nb.phase is Live && b.phase is Purged
            ==> nbud.used as int == bud.used as int + b.size as int && nb.contents_lost),
        // R-2: accepted exactly when `accepts` says so, and the refusals the kernel turns into errnos have their causes.
        r is Ok <==> accepts(b, bud, op),
        r matches Err(e) ==> ((e is Dead) <==> (b.phase is Freed || b.phase is Quarantined)),
        r matches Err(e) ==> (e is Purged ==> b.phase is Purged),
        r matches Err(e) ==> (e is NoRoom ==> b.phase is Purged && op == Op::SetPurgeable(false)
            && bud.used as int + b.size as int > bud.limit as int),
        r matches Err(e) ==> !(e is Accounting),
        // R-1: exactly the fields the operation names move.
        r matches Ok((nb, nbud)) ==> nb == post(b, op) && nbud.limit == bud.limit,
        // R-4: a GPU failure is never forgotten, and a live buffer of a failed GPU is never freed into the pool.
        r matches Ok((nb, nbud)) ==> (b.gpu_failed ==> nb.gpu_failed),
        r matches Ok((nb, nbud)) ==> (nb.phase is Freed && b.phase is Live ==> !b.gpu_failed),
        // R-5: a buffer a GPU slot names, or whose GPU failed, is never purged; a purged buffer is bound to no slot (in `wf`).
        r matches Ok((nb, nbud)) ==> (nb.phase is Purged && !(b.phase is Purged) ==> b.gpu_slots == 0 && !b.gpu_failed),
))]
#[cfg_attr(verus_keep_ghost, verifier::spinoff_prover)]
pub fn step(b: Buf, bud: Budget, op: Op) -> Result<(Buf, Budget), Refusal> {
    match b.phase {
        Phase::Freed | Phase::Quarantined => return Err(Refusal::Dead),
        _ => {}
    }
    let mut n = b;
    match op {
        Op::Open => {
            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.fds = b.fds + 1;
            n.refs = b.refs + 1;
            Ok((n, bud))
        }
        Op::Close => {
            if b.fds == 0 {
                return Err(Refusal::NoDescriptor);
            }
            n.fds = b.fds - 1;
            n.refs = b.refs - 1;
            settle(n, bud)
        }
        Op::Map => {
            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.mappings = b.mappings + 1;
            n.refs = b.refs + 1;
            Ok((n, bud))
        }
        Op::Unmap => {
            if b.mappings == 0 {
                return Err(Refusal::NotMapped);
            }
            n.mappings = b.mappings - 1;
            n.refs = b.refs - 1;
            settle(n, bud)
        }
        Op::GpuBind => {
            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.refs == u32::MAX {
                return Err(Refusal::Overflow);
            }
            n.gpu_slots = b.gpu_slots + 1;
            n.refs = b.refs + 1;
            Ok((n, bud))
        }
        Op::GpuUnbind => {
            if b.gpu_slots == 0 {
                return Err(Refusal::NotBound);
            }
            n.gpu_slots = b.gpu_slots - 1;
            n.refs = b.refs - 1;
            settle(n, bud)
        }
        Op::BatchStart(slots) => {
            if let Phase::Purged = b.phase {
                return Err(Refusal::Purged);
            }
            if b.gpu_pending == u32::MAX || slots > u32::MAX - b.gpu_slots_in_batch {
                return Err(Refusal::Overflow);
            }
            n.gpu_pending = b.gpu_pending + 1;
            n.gpu_slots_in_batch = b.gpu_slots_in_batch + slots;
            Ok((n, bud))
        }
        Op::BatchRetire(slots) => {
            if b.gpu_pending == 0 {
                return Err(Refusal::NoBatch);
            }
            if slots > b.gpu_slots_in_batch
                || (b.gpu_slots_in_batch - slots > 0 && b.gpu_pending - 1 == 0)
            {
                return Err(Refusal::BadBatch);
            }
            n.gpu_pending = b.gpu_pending - 1;
            n.gpu_slots_in_batch = b.gpu_slots_in_batch - slots;
            settle(n, bud)
        }
        Op::SetPurgeable(on) => {
            if on {
                n.purgeable = true;
                if let Phase::Live = b.phase {
                    n.contents_lost = false;
                }
                return Ok((n, bud));
            }
            match b.phase {
                Phase::Purged => {
                    let nbud = match charge(bud, b.size) {
                        Ok(x) => x,
                        Err(_) => return Err(Refusal::NoRoom),
                    };
                    n.phase = Phase::Live;
                    n.charged = true;
                    n.purgeable = false;
                    n.contents_lost = true;
                    Ok((n, nbud))
                }
                _ => {
                    n.purgeable = false;
                    Ok((n, bud))
                }
            }
        }
        Op::Purge => {
            match b.phase {
                Phase::Live => {}
                _ => return Err(Refusal::PurgeDenied),
            }
            if !b.purgeable || b.mappings != 0 || b.gpu_slots_in_batch != 0 || b.gpu_pending != 0 {
                return Err(Refusal::PurgeDenied);
            }
            if b.gpu_slots != 0 || b.gpu_failed {
                return Err(Refusal::PurgeDenied);
            }
            match release(bud, b.size) {
                Ok(nbud) => {
                    n.phase = Phase::Purged;
                    n.charged = false;
                    Ok((n, nbud))
                }
                Err(_) => Err(Refusal::Accounting),
            }
        }
        Op::GpuFail => {
            n.gpu_failed = true;
            Ok((n, bud))
        }
    }
}

/// The row pitch of a `width`-pixel XRGB/ARGB row: `width * 4` rounded up to 64 bytes. `None` for a width outside `1..0x4000`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        (1 <= width && width < 0x4000) <==> r.is_some(),
        r matches Some(p) ==> p % 64 == 0 && p as int >= width as int * 4 && (p as int) < width as int * 4 + 64 && p <= MAX_PITCH,
))]
pub fn pitch(width: u32) -> Option<u32> {
    if width == 0 || width >= MAX_DIM {
        return None;
    }
    let bytes = width * 4;
    let p = (bytes + 63) / 64 * 64;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(p % 64 == 0) by (nonlinear_arith)
            requires p == (bytes + 63) / 64 * 64;
        assert(p >= bytes && p < bytes + 64) by (nonlinear_arith)
            requires p == (bytes + 63) / 64 * 64, bytes % 64 >= 0;
    }
    Some(p)
}

/// The bytes of a buffer of `height` rows of `pitch` bytes, rounded up to whole pages. `None` for a zero or oversized pitch or
/// a height outside `1..0x4000`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(b) ==> b % 4096 == 0 && b as int >= pitch as int * height as int
            && (b as int) < pitch as int * height as int + 4096 && b > 0,
        (1 <= pitch && pitch <= MAX_PITCH && 1 <= height && height < 0x4000) <==> r.is_some(),
))]
pub fn buffer_bytes(pitch: u32, height: u32) -> Option<u64> {
    if pitch == 0 || pitch > MAX_PITCH || height == 0 || height >= MAX_DIM {
        return None;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(pitch as u64 * height as u64 <= 0x1_0000 * 0x3fff) by (nonlinear_arith)
            requires pitch <= 0x1_0000, height < 0x4000;
    }
    let raw = pitch as u64 * height as u64;
    let b = (raw + 4095) / 4096 * 4096;
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(b % 4096 == 0) by (nonlinear_arith)
            requires b == (raw + 4095) / 4096 * 4096;
        assert(b >= raw && b < raw + 4096) by (nonlinear_arith)
            requires b == (raw + 4095) / 4096 * 4096;
        assert(raw >= 1) by (nonlinear_arith)
            requires raw == pitch as u64 * height as u64, pitch >= 1, height >= 1;
    }
    Some(b)
}

/// The pitch and size of a `width` x `height` XRGB/ARGB buffer: `Some` exactly for both below 0x4000 and above zero, and no
/// arithmetic overflows for any of them.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        (1 <= width && width < 0x4000 && 1 <= height && height < 0x4000) <==> r.is_some(),
        r matches Some((p, bytes)) ==> p % 64 == 0 && p as int >= width as int * 4 && bytes % 4096 == 0
            && bytes as int >= p as int * height as int,
))]
pub fn surface_size(width: u32, height: u32) -> Option<(u32, u64)> {
    let p = match pitch(width) {
        Some(p) => p,
        None => return None,
    };
    match buffer_bytes(p, height) {
        Some(b) => Some((p, b)),
        None => None,
    }
}

/// Whether every sync rectangle `(y, h)` lies inside a buffer of `height` rows: `h > 0` and `y + h <= height` without
/// overflow. Returns the number of rows to clean (rectangles may overlap; each row counts once per rectangle), or `None` for
/// the first bad rectangle.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(n) ==> (forall|j: int| 0 <= j < rows@.len() ==> 0 < (#[trigger] rows@[j]).1
            && rows@[j].0 as int + rows@[j].1 as int <= height as int && rows@[j].0 < height)
            && n >= rows@.len(),
        // R-6: `Some` exactly when every rectangle is inside and the row count fits in u64, and then it is that count.
        r is Some <==> ((forall|j: int| 0 <= j < rows@.len() ==> row_inside(#[trigger] rows@[j], height))
            && rows_sum(rows@) <= u64::MAX as int),
        r matches Some(n) ==> n as int == rows_sum(rows@),
))]
pub fn rows_ok(rows: &[(u32, u32)], height: u32) -> Option<u64> {
    let mut total: u64 = 0;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            0 <= i <= rows@.len(),
            total >= i,
            forall|j: int| 0 <= j < i ==> 0 < (#[trigger] rows@[j]).1
                && rows@[j].0 as int + rows@[j].1 as int <= height as int && rows@[j].0 < height,
            total as int == rows_sum(rows@.subrange(0, i as int)),
        decreases rows@.len() - i
    ))]
    while i < rows.len() {
        let (y, h) = rows[i];
        if h == 0 || y >= height || h > height - y {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(!row_inside(rows@[i as int], height));
            }
            return None;
        }
        if h as u64 > u64::MAX - total {
            #[cfg(verus_keep_ghost)]
            proof! {
                assert(rows@.subrange(0, i as int + 1).drop_last() =~= rows@.subrange(0, i as int));
                lemma_rows_prefix_le(rows@, i as int + 1);
            }
            return None;
        }
        total = total + h as u64;
        i += 1;
        #[cfg(verus_keep_ghost)]
        proof! {
            assert(rows@.subrange(0, i as int).drop_last() =~= rows@.subrange(0, i as int - 1));
        }
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(rows@.subrange(0, i as int) =~= rows@);
    }
    Some(total)
}
