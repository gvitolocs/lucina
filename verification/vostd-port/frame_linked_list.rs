// SPDX-License-Identifier: MPL-2.0
//
// Verus proof: the Asterinas v0.19.0 OSTD frame linked list (ostd/src/mm/frame/linked_list.rs), traversal and pointer surgery.
// Derived from vostd (https://github.com/asterinas/vostd, rev 020b85418e2d4edcd5089cbf818e477b1d07f3ea, MPL-2.0),
// ostd/src/mm/frame/linked_list.rs and ostd/specs/mm/frame/linked_list/*, but rewritten as a self-contained single
// file: vostd proves v0.16.0 against its own vstd fork and ~3000 lines of frame/meta ownership specs that this
// file does not need. See README.md in this directory for what is proved, what changed and what is assumed.
//
// The exec bodies below are the v0.19.0 bodies; the only textual changes are listed in README.md
// (NonNull<Link<M>> -> vstd PPtr<Link<M>>, `unsafe { x.as_ref() }` -> `x.borrow(perm)`, debug_asserts -> proof asserts).

use core::marker::PhantomData;
use vstd::prelude::*;
use vstd::simple_pptr::*;

verus! {

// ---------------------------------------------------------------------------------------------
// Types (v0.19.0 field layout)
// ---------------------------------------------------------------------------------------------
/// A linked list of frames.
pub struct LinkedList<M> {
    pub front: Option<PPtr<Link<M>>>,
    pub back: Option<PPtr<Link<M>>>,
    pub size: usize,
    pub list_id: u64,
}

/// A mutable cursor for the linked list of frames.
pub struct CursorMut<'a, M> {
    pub list: &'a mut LinkedList<M>,
    pub current: Option<PPtr<Link<M>>>,
}

/// The metadata of a frame in a linked list.
pub struct Link<M> {
    pub next: Option<PPtr<Link<M>>>,
    pub prev: Option<PPtr<Link<M>>>,
    pub meta: M,
}

// ---------------------------------------------------------------------------------------------
// Ghost model: the sequence of node addresses front to back, and one permission per node.
// ---------------------------------------------------------------------------------------------
pub open spec fn fptr<F>(a: usize) -> PPtr<F> {
    PPtr::<F>(a, PhantomData)
}

pub open spec fn ptr_at<M>(a: usize) -> PPtr<Link<M>> {
    fptr::<Link<M>>(a)
}

/// The `in_list: AtomicU64` field of the `MetaSlot` of the frame at `a`, modelled as a separate `u64` cell.
pub open spec fn slot_ptr(a: usize) -> PPtr<u64> {
    PPtr::<u64>(a, PhantomData)
}

/// Node `i` of `order` is initialised in `perms`, and its `next` / `prev` are exactly its neighbours.
pub open spec fn node_ok_in<M>(
    order: Seq<usize>,
    perms: Map<usize, PointsTo<Link<M>>>,
    i: int,
) -> bool {
    let p = perms[order[i]];
    &&& p.pptr() == ptr_at::<M>(order[i])
    &&& p.is_init()
    &&& p.value().next == if i + 1 < order.len() {
        Some(ptr_at::<M>(order[i + 1]))
    } else {
        None
    }
    &&& p.value().prev == if i > 0 {
        Some(ptr_at::<M>(order[i - 1]))
    } else {
        None
    }
}

/// The `in_list` cell of node `i` of `order` holds `id`.
pub open spec fn slot_ok_in(
    order: Seq<usize>,
    slots: Map<usize, PointsTo<u64>>,
    id: u64,
    i: int,
) -> bool {
    let s = slots[order[i]];
    &&& s.pptr() == slot_ptr(order[i])
    &&& s.is_init()
    &&& s.value() == id
}

/// `new` is `old` with only `next` changed to `v`.
pub open spec fn next_set<M>(
    old: PointsTo<Link<M>>,
    new: PointsTo<Link<M>>,
    v: Option<PPtr<Link<M>>>,
) -> bool {
    &&& new.pptr() == old.pptr()
    &&& new.is_init()
    &&& new.value().next == v
    &&& new.value().prev == old.value().prev
    &&& new.value().meta == old.value().meta
}

/// `new` is `old` with only `prev` changed to `v`.
pub open spec fn prev_set<M>(
    old: PointsTo<Link<M>>,
    new: PointsTo<Link<M>>,
    v: Option<PPtr<Link<M>>>,
) -> bool {
    &&& new.pptr() == old.pptr()
    &&& new.is_init()
    &&& new.value().prev == v
    &&& new.value().next == old.value().next
    &&& new.value().meta == old.value().meta
}

pub tracked struct ListOwner<M> {
    /// Addresses of the links, front to back.
    pub ghost order: Seq<usize>,
    /// The id the exec list carries in `list_id` (0 = not yet allocated).
    pub ghost list_id: u64,
    pub tracked perms: Map<usize, PointsTo<Link<M>>>,
    /// The `in_list` cell of every listed frame; each holds `list_id`.
    pub tracked slots: Map<usize, PointsTo<u64>>,
}

impl<M> ListOwner<M> {
    /// Node `i` is initialised, and its `next` / `prev` are exactly its neighbours in `order`.
    #[verifier::inline]
    pub open spec fn node_ok(self, i: int) -> bool {
        node_ok_in(self.order, self.perms, i)
    }

    pub open spec fn inv(self) -> bool {
        &&& self.order.no_duplicates()
        &&& self.perms.dom() =~= self.order.to_set()
        &&& self.slots.dom() =~= self.order.to_set()
        &&& forall|i: int| 0 <= i < self.order.len() ==> #[trigger] node_ok_in(self.order, self.perms, i)
        &&& forall|i: int|
            0 <= i < self.order.len() ==> #[trigger] slot_ok_in(
                self.order,
                self.slots,
                self.list_id,
                i,
            )
        &&& self.list_id == 0 ==> self.order.len() == 0
    }

    /// The exec list header agrees with the ghost sequence.
    pub open spec fn wf(self, l: LinkedList<M>) -> bool {
        &&& l.size == self.order.len()
        &&& l.list_id == self.list_id
        &&& l.front == if self.order.len() > 0 {
            Some(ptr_at::<M>(self.order[0]))
        } else {
            None
        }
        &&& l.back == if self.order.len() > 0 {
            Some(ptr_at::<M>(self.order[self.order.len() - 1]))
        } else {
            None
        }
    }

    /// The owner of a fresh `LinkedList::new()`.
    pub proof fn empty() -> (tracked o: ListOwner<M>)
        ensures
            o.inv(),
            o.order.len() == 0,
            o.list_id == 0,
            o.wf(LinkedList::<M> { front: None, back: None, size: 0, list_id: 0 }),
    {
        ListOwner {
            order: Seq::empty(),
            list_id: 0,
            perms: Map::tracked_empty(),
            slots: Map::tracked_empty(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Frame ownership stub (stands in for `UniqueFrame<F>` and the `MetaSlot` it points to).
// ---------------------------------------------------------------------------------------------
/// A unique reference to a frame whose metadata is an `F`. `paddr` doubles as the address of the metadata
/// (`mapping::meta_to_frame` / `frame_to_meta` are modelled as the identity).
pub struct UniqueFrame<F> {
    pub paddr: usize,
    pub _f: PhantomData<F>,
}

/// The permissions a `UniqueFrame` carries: the metadata cell and the slot's `in_list` cell.
pub tracked struct FrameOwner<F> {
    pub tracked meta: PointsTo<F>,
    pub tracked in_list: PointsTo<u64>,
}

impl<F> FrameOwner<F> {
    pub open spec fn inv(self, f: UniqueFrame<F>) -> bool {
        &&& self.meta.pptr() == fptr::<F>(f.paddr)
        &&& self.meta.is_init()
        &&& self.in_list.pptr() == slot_ptr(f.paddr)
        &&& self.in_list.is_init()
    }
}

impl<F> UniqueFrame<F> {
    /// The address of the metadata (`NonNull::from(frame.meta_mut())`).
    pub fn meta_ptr(&self) -> (p: PPtr<F>)
        ensures
            p == fptr::<F>(self.paddr),
    {
        PPtr::<F>::from_addr(self.paddr)
    }

    /// The address of the slot's `in_list` field.
    pub fn in_list_ptr(&self) -> (p: PPtr<u64>)
        ensures
            p == slot_ptr(self.paddr),
    {
        PPtr::<u64>::from_addr(self.paddr)
    }

    /// `frame.meta()`.
    pub fn meta<'a>(&self, Tracked(perm): Tracked<&'a PointsTo<F>>) -> (r: &'a F)
        requires
            perm.pptr() == fptr::<F>(self.paddr),
            perm.is_init(),
        ensures
            *r == perm.value(),
    {
        self.meta_ptr().borrow(Tracked(perm))
    }

    /// `frame.meta_mut()`.
    pub fn meta_mut<'a>(&self, Tracked(perm): Tracked<&'a mut PointsTo<F>>) -> (r: &'a mut F)
        requires
            old(perm).pptr() == fptr::<F>(self.paddr),
            old(perm).is_init(),
        ensures
            final(perm).pptr() == old(perm).pptr(),
            final(perm).is_init(),
            *r == old(perm).value(),
            final(perm).value() == *final(r),
    {
        self.meta_ptr().borrow_mut(Tracked(perm))
    }

    /// `frame.slot().in_list.load(Relaxed)`.
    pub fn in_list_load(&self, Tracked(slot): Tracked<&PointsTo<u64>>) -> (v: u64)
        requires
            slot.pptr() == slot_ptr(self.paddr),
            slot.is_init(),
        ensures
            v == slot.value(),
    {
        *self.in_list_ptr().borrow(Tracked(slot))
    }

    /// `frame.slot().in_list.store(v, Relaxed)`.
    pub fn in_list_store(&self, Tracked(slot): Tracked<&mut PointsTo<u64>>, v: u64)
        requires
            old(slot).pptr() == slot_ptr(self.paddr),
            old(slot).is_init(),
        ensures
            final(slot).pptr() == old(slot).pptr(),
            final(slot).is_init(),
            final(slot).value() == v,
    {
        let _old = self.in_list_ptr().replace(Tracked(slot), v);
    }

    /// `UniqueFrame::into_raw`: forgets the frame, leaving its memory to the list. The caller moves the
    /// permissions of the frame into the `ListOwner`.
    pub fn into_raw(self) -> (paddr: usize)
        ensures
            paddr == self.paddr,
    {
        self.paddr
    }

    /// `UniqueFrame::from_raw`: the caller takes the permissions of the frame out of the `ListOwner`.
    pub fn from_raw(paddr: usize) -> (f: Self)
        ensures
            f.paddr == paddr,
    {
        UniqueFrame { paddr, _f: PhantomData }
    }
}

/// `mapping::meta_to_frame` (identity in the model).
pub fn meta_to_frame<M>(meta: PPtr<Link<M>>) -> (paddr: usize)
    ensures
        paddr == meta.addr(),
{
    meta.addr()
}

// ---------------------------------------------------------------------------------------------
// Sequence lemmas
// ---------------------------------------------------------------------------------------------
proof fn lemma_insert_seq(order: Seq<usize>, idx: int, fa: usize)
    requires
        order.no_duplicates(),
        0 <= idx <= order.len(),
        forall|k: int| 0 <= k < order.len() ==> order[k] != fa,
    ensures
        order.insert(idx, fa).len() == order.len() + 1,
        forall|j: int| 0 <= j < idx ==> order.insert(idx, fa)[j] == order[j],
        order.insert(idx, fa)[idx] == fa,
        forall|j: int| idx < j <= order.len() ==> order.insert(idx, fa)[j] == order[j - 1],
        order.insert(idx, fa).no_duplicates(),
        order.insert(idx, fa).to_set() =~= order.to_set().insert(fa),
{
    let no = order.insert(idx, fa);
    order.insert_ensures(idx, fa);
    assert forall|a: int, b: int| 0 <= a < no.len() && 0 <= b < no.len() && a != b implies no[a]
        != no[b] by {
        if a < idx && b < idx {
        } else if a < idx && b == idx {
        } else if a < idx && b > idx {
            assert(no[b] == order[b - 1]);
        } else if a == idx && b < idx {
        } else if a == idx && b > idx {
            assert(no[b] == order[b - 1]);
        } else if a > idx && b < idx {
            assert(no[a] == order[a - 1]);
        } else if a > idx && b == idx {
            assert(no[a] == order[a - 1]);
        } else {
            assert(no[a] == order[a - 1]);
            assert(no[b] == order[b - 1]);
        }
    }
    order.to_set_ensures();
    no.to_set_ensures();
    assert forall|k: usize| no.to_set().contains(k) <==> order.to_set().insert(fa).contains(k) by {
        if k == fa {
            assert(no[idx] == fa);
            assert(no.contains(fa));
        } else {
            if order.contains(k) {
                let i = choose|i: int| 0 <= i < order.len() && order[i] == k;
                if i < idx {
                    assert(no[i] == k);
                } else {
                    assert(no[i + 1] == k);
                }
                assert(no.contains(k));
            }
            if no.contains(k) {
                let j = choose|j: int| 0 <= j < no.len() && no[j] == k;
                if j < idx {
                    assert(order[j] == k);
                } else if j > idx {
                    assert(order[j - 1] == k);
                }
                assert(order.contains(k));
            }
        }
    }
}

proof fn lemma_remove_seq(order: Seq<usize>, idx: int)
    requires
        order.no_duplicates(),
        0 <= idx < order.len(),
    ensures
        order.remove(idx).len() == order.len() - 1,
        forall|j: int| 0 <= j < idx ==> order.remove(idx)[j] == order[j],
        forall|j: int| idx <= j < order.len() - 1 ==> order.remove(idx)[j] == order[j + 1],
        order.remove(idx).no_duplicates(),
        order.remove(idx).to_set() =~= order.to_set().remove(order[idx]),
{
    let nr = order.remove(idx);
    order.remove_ensures(idx);
    assert forall|a: int, b: int| 0 <= a < nr.len() && 0 <= b < nr.len() && a != b implies nr[a]
        != nr[b] by {
        if a < idx {
            assert(nr[a] == order[a]);
        } else {
            assert(nr[a] == order[a + 1]);
        }
        if b < idx {
            assert(nr[b] == order[b]);
        } else {
            assert(nr[b] == order[b + 1]);
        }
    }
    order.to_set_ensures();
    nr.to_set_ensures();
    assert forall|k: usize| nr.to_set().contains(k) <==> order.to_set().remove(
        order[idx],
    ).contains(k) by {
        if order.contains(k) && k != order[idx] {
            let i = choose|i: int| 0 <= i < order.len() && order[i] == k;
            if i < idx {
                assert(nr[i] == k);
            } else {
                assert(nr[i - 1] == k);
            }
            assert(nr.contains(k));
        }
        if nr.contains(k) {
            let j = choose|j: int| 0 <= j < nr.len() && nr[j] == k;
            if j < idx {
                assert(order[j] == k);
            } else {
                assert(order[j + 1] == k);
            }
            assert(order.contains(k));
        }
    }
}

/// Inserting `fa` (whose permissions are `perms1[fa]`) at `idx` keeps every node's neighbours right, given that
/// only the two neighbours of the new node were relinked.
proof fn lemma_insert_ok<M>(
    order: Seq<usize>,
    perms0: Map<usize, PointsTo<Link<M>>>,
    perms1: Map<usize, PointsTo<Link<M>>>,
    idx: int,
    fa: usize,
)
    requires
        order.no_duplicates(),
        perms0.dom() =~= order.to_set(),
        forall|i: int| 0 <= i < order.len() ==> #[trigger] node_ok_in(order, perms0, i),
        0 <= idx <= order.len(),
        forall|k: int| 0 <= k < order.len() ==> order[k] != fa,
        perms1.dom() =~= perms0.dom().insert(fa),
        forall|k: usize|
            perms0.dom().contains(k) && !(idx > 0 && k == order[idx - 1]) && !(idx < order.len()
                && k == order[idx]) ==> perms1[k] == perms0[k],
        idx > 0 ==> next_set(
            perms0[order[idx - 1]],
            perms1[order[idx - 1]],
            Some(ptr_at::<M>(fa)),
        ),
        idx < order.len() ==> prev_set(
            perms0[order[idx]],
            perms1[order[idx]],
            Some(ptr_at::<M>(fa)),
        ),
        perms1[fa].pptr() == ptr_at::<M>(fa),
        perms1[fa].is_init(),
        perms1[fa].value().next == if idx < order.len() {
            Some(ptr_at::<M>(order[idx]))
        } else {
            None
        },
        perms1[fa].value().prev == if idx > 0 {
            Some(ptr_at::<M>(order[idx - 1]))
        } else {
            None
        },
    ensures
        perms1.dom() =~= order.insert(idx, fa).to_set(),
        forall|j: int|
            0 <= j < order.len() + 1 ==> #[trigger] node_ok_in(
                order.insert(idx, fa),
                perms1,
                j,
            ),
        forall|k: usize|
            perms0.dom().contains(k) ==> perms1[k].value().meta == perms0[k].value().meta,
{
    lemma_insert_seq(order, idx, fa);
    let no = order.insert(idx, fa);
    let n = order.len() as int;
    assert forall|k: usize| perms0.dom().contains(k) implies perms1[k].value().meta
        == perms0[k].value().meta by {
        order.to_set_ensures();
        assert(order.contains(k));
        let i = choose|i: int| 0 <= i < order.len() && order[i] == k;
        assert(node_ok_in(order, perms0, i));
        if idx > 0 && k == order[idx - 1] {
        } else if idx < n && k == order[idx] {
        }
    }
    assert forall|j: int| 0 <= j < n + 1 implies node_ok_in(no, perms1, j) by {
        if j < idx - 1 {
            assert(no[j] == order[j]);
            assert(no[j + 1] == order[j + 1]);
            if j > 0 {
                assert(no[j - 1] == order[j - 1]);
            }
            assert(node_ok_in(order, perms0, j));
            assert(order[j] != order[idx - 1]);
            if idx < n {
                assert(order[j] != order[idx]);
            }
            assert(order[j] != fa);
        } else if j == idx - 1 {
            assert(no[j] == order[j]);
            assert(no[j + 1] == fa);
            if j > 0 {
                assert(no[j - 1] == order[j - 1]);
            }
            assert(node_ok_in(order, perms0, j));
        } else if j == idx {
            assert(no[j] == fa);
            if idx < n {
                assert(no[j + 1] == order[idx]);
            }
            if idx > 0 {
                assert(no[j - 1] == order[idx - 1]);
            }
        } else if j == idx + 1 {
            assert(no[j] == order[idx]);
            assert(no[j - 1] == fa);
            if j + 1 < n + 1 {
                assert(no[j + 1] == order[j]);
            }
            assert(node_ok_in(order, perms0, idx));
        } else {
            assert(no[j] == order[j - 1]);
            assert(no[j - 1] == order[j - 2]);
            if j + 1 < n + 1 {
                assert(no[j + 1] == order[j]);
            }
            assert(node_ok_in(order, perms0, j - 1));
            if idx > 0 {
                assert(order[j - 1] != order[idx - 1]);
            }
            if idx < n {
                assert(order[j - 1] != order[idx]);
            }
            assert(order[j - 1] != fa);
        }
    }
}

/// Removing the node at `idx` (its permissions are gone from `perms1`) relinks only its two neighbours.
proof fn lemma_remove_ok<M>(
    order: Seq<usize>,
    perms0: Map<usize, PointsTo<Link<M>>>,
    perms1: Map<usize, PointsTo<Link<M>>>,
    idx: int,
)
    requires
        order.no_duplicates(),
        perms0.dom() =~= order.to_set(),
        forall|i: int| 0 <= i < order.len() ==> #[trigger] node_ok_in(order, perms0, i),
        0 <= idx < order.len(),
        perms1.dom() =~= perms0.dom().remove(order[idx]),
        forall|k: usize|
            perms1.dom().contains(k) && !(idx > 0 && k == order[idx - 1]) && !(idx + 1
                < order.len() && k == order[idx + 1]) ==> perms1[k] == perms0[k],
        idx > 0 ==> next_set(
            perms0[order[idx - 1]],
            perms1[order[idx - 1]],
            if idx + 1 < order.len() {
                Some(ptr_at::<M>(order[idx + 1]))
            } else {
                None
            },
        ),
        idx + 1 < order.len() ==> prev_set(
            perms0[order[idx + 1]],
            perms1[order[idx + 1]],
            if idx > 0 {
                Some(ptr_at::<M>(order[idx - 1]))
            } else {
                None
            },
        ),
    ensures
        perms1.dom() =~= order.remove(idx).to_set(),
        forall|j: int|
            0 <= j < order.len() - 1 ==> #[trigger] node_ok_in(order.remove(idx), perms1, j),
        forall|k: usize|
            perms1.dom().contains(k) ==> perms1[k].value().meta == perms0[k].value().meta,
{
    lemma_remove_seq(order, idx);
    let nr = order.remove(idx);
    let n = order.len() as int;
    assert forall|k: usize| perms1.dom().contains(k) implies perms1[k].value().meta
        == perms0[k].value().meta by {
        order.to_set_ensures();
        assert(order.contains(k));
        if idx > 0 && k == order[idx - 1] {
        } else if idx + 1 < n && k == order[idx + 1] {
        }
    }
    assert forall|j: int| 0 <= j < n - 1 implies node_ok_in(nr, perms1, j) by {
        if j < idx - 1 {
            assert(nr[j] == order[j]);
            assert(nr[j + 1] == order[j + 1]);
            if j > 0 {
                assert(nr[j - 1] == order[j - 1]);
            }
            assert(node_ok_in(order, perms0, j));
            if idx > 0 {
                assert(order[j] != order[idx - 1]);
            }
            if idx + 1 < n {
                assert(order[j] != order[idx + 1]);
            }
        } else if j == idx - 1 {
            assert(nr[j] == order[j]);
            if j + 1 < n - 1 {
                assert(nr[j + 1] == order[j + 2]);
            }
            if j > 0 {
                assert(nr[j - 1] == order[j - 1]);
            }
            assert(node_ok_in(order, perms0, j));
            assert(node_ok_in(order, perms0, idx));
        } else if j == idx {
            assert(nr[j] == order[j + 1]);
            if j + 1 < n - 1 {
                assert(nr[j + 1] == order[j + 2]);
            }
            if j > 0 {
                assert(nr[j - 1] == order[j - 1]);
            }
            assert(node_ok_in(order, perms0, j + 1));
            assert(node_ok_in(order, perms0, idx));
        } else {
            assert(nr[j] == order[j + 1]);
            if j + 1 < n - 1 {
                assert(nr[j + 1] == order[j + 2]);
            }
            assert(nr[j - 1] == order[j]);
            assert(node_ok_in(order, perms0, j + 1));
            if idx > 0 {
                assert(order[j + 1] != order[idx - 1]);
            }
            if idx + 1 < n {
                assert(order[j + 1] != order[idx + 1]);
            }
        }
    }
}

proof fn lemma_slots_insert(
    order: Seq<usize>,
    slots0: Map<usize, PointsTo<u64>>,
    slots1: Map<usize, PointsTo<u64>>,
    id: u64,
    idx: int,
    fa: usize,
)
    requires
        order.no_duplicates(),
        slots0.dom() =~= order.to_set(),
        forall|i: int| 0 <= i < order.len() ==> #[trigger] slot_ok_in(order, slots0, id, i),
        0 <= idx <= order.len(),
        forall|k: int| 0 <= k < order.len() ==> order[k] != fa,
        slots1 =~= slots0.insert(fa, slots1[fa]),
        slots1[fa].pptr() == slot_ptr(fa),
        slots1[fa].is_init(),
        slots1[fa].value() == id,
    ensures
        slots1.dom() =~= order.insert(idx, fa).to_set(),
        forall|j: int|
            0 <= j < order.len() + 1 ==> #[trigger] slot_ok_in(
                order.insert(idx, fa),
                slots1,
                id,
                j,
            ),
{
    lemma_insert_seq(order, idx, fa);
    let no = order.insert(idx, fa);
    assert forall|j: int| 0 <= j < order.len() + 1 implies slot_ok_in(no, slots1, id, j) by {
        if j < idx {
            assert(no[j] == order[j]);
            assert(slot_ok_in(order, slots0, id, j));
        } else if j > idx {
            assert(no[j] == order[j - 1]);
            assert(slot_ok_in(order, slots0, id, j - 1));
        }
    }
}

proof fn lemma_slots_remove(
    order: Seq<usize>,
    slots0: Map<usize, PointsTo<u64>>,
    slots1: Map<usize, PointsTo<u64>>,
    id: u64,
    idx: int,
)
    requires
        order.no_duplicates(),
        slots0.dom() =~= order.to_set(),
        forall|i: int| 0 <= i < order.len() ==> #[trigger] slot_ok_in(order, slots0, id, i),
        0 <= idx < order.len(),
        slots1 =~= slots0.remove(order[idx]),
    ensures
        slots1.dom() =~= order.remove(idx).to_set(),
        forall|j: int|
            0 <= j < order.len() - 1 ==> #[trigger] slot_ok_in(order.remove(idx), slots1, id, j),
{
    lemma_remove_seq(order, idx);
    let nr = order.remove(idx);
    assert forall|j: int| 0 <= j < order.len() - 1 implies slot_ok_in(nr, slots1, id, j) by {
        if j < idx {
            assert(nr[j] == order[j]);
            assert(slot_ok_in(order, slots0, id, j));
        } else {
            assert(nr[j] == order[j + 1]);
            assert(slot_ok_in(order, slots0, id, j + 1));
        }
    }
}

/// `o1` has the same nodes, order, ids and `next`/`prev` links as `o0`; only metadata may differ.
pub open spec fn same_links<M>(o0: ListOwner<M>, o1: ListOwner<M>) -> bool {
    &&& o1.order == o0.order
    &&& o1.list_id == o0.list_id
    &&& o1.slots == o0.slots
    &&& o1.perms.dom() =~= o0.perms.dom()
    &&& forall|b: usize|
        #[trigger] o0.perms.dom().contains(b) ==> {
            &&& o1.perms.dom().contains(b)
            &&& o1.perms[b].pptr() == o0.perms[b].pptr()
            &&& o1.perms[b].is_init() == o0.perms[b].is_init()
            &&& o1.perms[b].is_init() ==> o1.perms[b].value().next == o0.perms[b].value().next
            &&& o1.perms[b].is_init() ==> o1.perms[b].value().prev == o0.perms[b].value().prev
        }
}

/// Metadata updates (which keep the links) keep the owner invariant.
pub proof fn lemma_same_links_inv<M>(o0: ListOwner<M>, o1: ListOwner<M>)
    requires
        o0.inv(),
        same_links(o0, o1),
    ensures
        o1.inv(),
{
    o0.order.to_set_ensures();
    assert forall|i: int| 0 <= i < o1.order.len() implies node_ok_in(o1.order, o1.perms, i) by {
        assert(node_ok_in(o0.order, o0.perms, i));
        assert(o0.perms.dom().contains(o0.order[i]));
        if i + 1 < o0.order.len() {
        }
    }
}

/// A frame that is not in the list (its `in_list` cell is a different cell than every listed one) is not
/// one of the listed addresses.
proof fn lemma_frame_fresh_upto<M>(tracked own: &ListOwner<M>, tracked fs: &mut PointsTo<u64>, k: int)
    requires
        own.inv(),
        0 <= k <= own.order.len(),
    ensures
        *final(fs) == *old(fs),
        forall|j: int| 0 <= j < k ==> own.order[j] != final(fs).addr(),
    decreases k,
{
    broadcast use vstd::layout::group_layout_axioms;

    if k > 0 {
        lemma_frame_fresh_upto(own, fs, k - 1);
        assert(slot_ok_in(own.order, own.slots, own.list_id, k - 1));
        own.order.to_set_ensures();
        assert(own.slots.dom().contains(own.order[k - 1]));
        let tracked s = own.slots.tracked_borrow(own.order[k - 1]);
        fs.is_distinct(s);
    }
}

proof fn lemma_frame_fresh<M>(tracked own: &ListOwner<M>, tracked fs: &mut PointsTo<u64>)
    requires
        own.inv(),
    ensures
        *final(fs) == *old(fs),
        forall|k: int| 0 <= k < own.order.len() ==> own.order[k] != final(fs).addr(),
{
    lemma_frame_fresh_upto(own, fs, own.order.len() as int);
}

/// `idx == order.len()` is the "ghost" non-element.
pub tracked struct CursorOwner<M> {
    pub tracked list_own: ListOwner<M>,
    pub ghost idx: int,
}

impl<M> CursorOwner<M> {
    pub open spec fn inv(self) -> bool {
        &&& self.list_own.inv()
        &&& 0 <= self.idx <= self.list_own.order.len()
    }

    pub open spec fn current(self) -> Option<PPtr<Link<M>>> {
        if self.idx < self.list_own.order.len() {
            Some(ptr_at::<M>(self.list_own.order[self.idx]))
        } else {
            None
        }
    }

    pub open spec fn wf(self, c: CursorMut<M>) -> bool {
        &&& self.inv()
        &&& self.list_own.wf(*c.list)
        &&& c.current == self.current()
    }
}

// ---------------------------------------------------------------------------------------------
// LinkedList
// ---------------------------------------------------------------------------------------------
impl<M> LinkedList<M> {
    /// Creates a new linked list.
    pub const fn new() -> (r: Self)
        ensures
            r.size == 0,
            r.front is None,
            r.back is None,
            r.list_id == 0,
    {
        Self { front: None, back: None, size: 0, list_id: 0 }
    }

    /// Gets the number of frames in the linked list.
    pub fn size(&self, Tracked(owner): Tracked<&ListOwner<M>>) -> (s: usize)
        requires
            owner.inv(),
            owner.wf(*self),
        ensures
            s == owner.order.len(),
    {
        self.size
    }

    /// Tells if the linked list is empty.
    pub fn is_empty(&self, Tracked(owner): Tracked<&ListOwner<M>>) -> (b: bool)
        requires
            owner.inv(),
            owner.wf(*self),
        ensures
            b <==> owner.order.len() == 0,
            b <==> self.front is None,
            b <==> self.back is None,
    {
        let is_empty = self.size == 0;
        // debug_assert_eq!(is_empty, self.front.is_none());
        proof {
            assert(is_empty == self.front.is_none());
        }
        // debug_assert_eq!(is_empty, self.back.is_none());
        proof {
            assert(is_empty == self.back.is_none());
        }
        is_empty
    }

    /// Gets a cursor at the front that can mutate the linked list links.
    ///
    /// If the list is empty, the cursor points to the "ghost" non-element.
    pub fn cursor_front_mut(&mut self, Tracked(owner): Tracked<ListOwner<M>>) -> (res: (
        CursorMut<'_, M>,
        Tracked<CursorOwner<M>>,
    ))
        requires
            owner.inv(),
            owner.wf(*old(self)),
        ensures
            res.1@.wf(res.0),
            res.1@.idx == 0,
            res.1@.list_own == owner,
            *res.0.list == *old(self),
            *final(self) == *final(res.0.list),
    {
        let current = self.front;
        (CursorMut { list: self, current }, Tracked(CursorOwner { list_own: owner, idx: 0 }))
    }

    /// Gets a cursor at the back that can mutate the linked list links.
    ///
    /// If the list is empty, the cursor points to the "ghost" non-element.
    pub fn cursor_back_mut(&mut self, Tracked(owner): Tracked<ListOwner<M>>) -> (res: (
        CursorMut<'_, M>,
        Tracked<CursorOwner<M>>,
    ))
        requires
            owner.inv(),
            owner.wf(*old(self)),
        ensures
            res.1@.wf(res.0),
            res.1@.idx == if owner.order.len() > 0 {
                owner.order.len() - 1
            } else {
                0
            },
            res.1@.list_own == owner,
            *res.0.list == *old(self),
            *final(self) == *final(res.0.list),
    {
        let current = self.back;
        let ghost idx: int = if owner.order.len() > 0 {
            owner.order.len() - 1
        } else {
            0
        };
        (CursorMut { list: self, current }, Tracked(CursorOwner { list_own: owner, idx }))
    }

    /// Gets a cursor at the "ghost" non-element (used by `push_back`).
    fn cursor_at_ghost_mut(&mut self, Tracked(owner): Tracked<ListOwner<M>>) -> (res: (
        CursorMut<'_, M>,
        Tracked<CursorOwner<M>>,
    ))
        requires
            owner.inv(),
            owner.wf(*old(self)),
        ensures
            res.1@.wf(res.0),
            res.1@.idx == owner.order.len(),
            res.1@.list_own == owner,
            *res.0.list == *old(self),
            *final(self) == *final(res.0.list),
    {
        let ghost idx: int = owner.order.len() as int;
        (CursorMut { list: self, current: None }, Tracked(CursorOwner { list_own: owner, idx }))
    }

    /// Returns the list ID, allocating one on first use.
    fn lazy_get_id(&mut self) -> (id: u64)
        ensures
            id != 0,
            final(self).list_id == id,
            old(self).list_id != 0 ==> id == old(self).list_id,
            final(self).front == old(self).front,
            final(self).back == old(self).back,
            final(self).size == old(self).size,
    {
        // FIXME: Self-incrementing IDs may overflow, while `core::pin::Pin`
        // is not compatible with locks. Think about a better solution.
        // (The static atomic counter, the overflow check and `abort()` live in `next_list_id`.)
        if self.list_id == 0 {
            let id = next_list_id();
            self.list_id = id;
            id
        } else {
            self.list_id
        }
    }
}

/// `LIST_ID_ALLOCATOR.fetch_add(1, Ordering::Relaxed)` followed by the `id >= MAX_LIST_ID` check, which logs and
/// calls `abort()` (never returns). Trusted: the static `AtomicU64` is not modelled.
#[verifier::external_body]
fn next_list_id() -> (id: u64)
    ensures
        0 < id < 0x7fff_ffff_ffff_ffff,
{
    use core::sync::atomic::{AtomicU64, Ordering};
    static LIST_ID_ALLOCATOR: AtomicU64 = AtomicU64::new(1);
    const MAX_LIST_ID: u64 = i64::MAX as u64;

    let id = LIST_ID_ALLOCATOR.fetch_add(1, Ordering::Relaxed);
    if id >= MAX_LIST_ID {
        panic!("The frame list ID allocator has exhausted.");
    }
    id
}

// ---------------------------------------------------------------------------------------------
// CursorMut
// ---------------------------------------------------------------------------------------------
impl<M> CursorMut<'_, M> {
    /// Moves the cursor to the next element.
    ///
    /// If the cursor is pointing to the "ghost" non-element then this will move it to the first element of the
    /// `LinkedList`. If it is pointing to the last element of the LinkedList then this will move it to the
    /// "ghost" non-element.
    pub fn move_next(&mut self, Tracked(own): Tracked<&mut CursorOwner<M>>)
        requires
            old(own).wf(*old(self)),
        ensures
            final(own).wf(*final(self)),
            final(own).list_own == old(own).list_own,
            final(own).idx == if old(own).idx < old(own).list_own.order.len() {
                old(own).idx + 1
            } else {
                0
            },
            *final(self).list == *old(self).list,
    {
        let ghost idx = own.idx;
        self.current = match self.current {
            // SAFETY: The cursor is pointing to a valid element.
            Some(current) => {
                proof {
                    assert(own.list_own.node_ok(idx));
                }
                let tracked perm = own.list_own.perms.tracked_borrow(current.addr());
                current.borrow(Tracked(perm)).next
            },
            None => self.list.front,
        };
        proof {
            own.idx = if idx < own.list_own.order.len() {
                idx + 1
            } else {
                0
            };
        }
    }

    /// Moves the cursor to the previous element.
    ///
    /// If the cursor is pointing to the "ghost" non-element then this will move it to the last element of the
    /// `LinkedList`. If it is pointing to the first element of the LinkedList then this will move it to the
    /// "ghost" non-element.
    pub fn move_prev(&mut self, Tracked(own): Tracked<&mut CursorOwner<M>>)
        requires
            old(own).wf(*old(self)),
        ensures
            final(own).wf(*final(self)),
            final(own).list_own == old(own).list_own,
            final(own).idx == if old(own).idx < old(own).list_own.order.len() {
                if old(own).idx > 0 {
                    old(own).idx - 1
                } else {
                    old(own).list_own.order.len() as int
                }
            } else {
                if old(own).list_own.order.len() > 0 {
                    old(own).list_own.order.len() - 1
                } else {
                    0
                }
            },
            *final(self).list == *old(self).list,
    {
        let ghost idx = own.idx;
        let ghost len = own.list_own.order.len() as int;
        self.current = match self.current {
            // SAFETY: The cursor is pointing to a valid element.
            Some(current) => {
                proof {
                    assert(own.list_own.node_ok(idx));
                }
                let tracked perm = own.list_own.perms.tracked_borrow(current.addr());
                current.borrow(Tracked(perm)).prev
            },
            None => self.list.back,
        };
        proof {
            own.idx = if idx < len {
                if idx > 0 {
                    idx - 1
                } else {
                    len
                }
            } else {
                if len > 0 {
                    len - 1
                } else {
                    0
                }
            };
        }
    }

    /// Gets the list that the cursor is mutating.
    pub fn as_list(&self) -> (r: &LinkedList<M>)
        ensures
            *r == *old(self.list),
    {
        &*self.list
    }
}

impl<M> CursorMut<'_, M> {
    /// Gets the mutable reference to the current frame's metadata.
    pub fn current_meta<'a>(&'a mut self, Tracked(own): Tracked<&'a mut CursorOwner<M>>) -> (r: Option<
        &'a mut M,
    >)
        requires
            old(own).wf(*old(self)),
        ensures
            same_links(old(own).list_own, final(own).list_own),
            *final(final(self).list) == *final(old(self).list),
            final(own).idx == old(own).idx,
            final(own).list_own.order == old(own).list_own.order,
            final(own).list_own.list_id == old(own).list_own.list_id,
            final(own).list_own.slots == old(own).list_own.slots,
            old(own).idx >= old(own).list_own.order.len() ==> r is None && final(own).list_own
                == old(own).list_own,
            old(own).idx < old(own).list_own.order.len() ==> {
                let o0 = old(own).list_own;
                let a = o0.order[old(own).idx];
                &&& r is Some
                &&& *r->Some_0 == o0.perms[a].value().meta
                &&& final(own).list_own.perms.dom() =~= o0.perms.dom()
                &&& final(own).list_own.perms[a].value().meta == *final(r->Some_0)
                &&& forall|b: usize|
                    o0.perms.dom().contains(b) && b != a ==> final(own).list_own.perms[b]
                        == o0.perms[b]
            },
    {
        match self.current {
            Some(current) => {
                proof {
                    assert(own.list_own.node_ok(own.idx));
                    own.list_own.order.to_set_ensures();
                }
                let tracked perm = own.list_own.perms.tracked_borrow_mut(current.addr());
                // SAFETY: `&mut self` ensures we have exclusive access to the frame metadata.
                let link_mut = current.borrow_mut(Tracked(perm));
                // We should not allow `&mut Link<M>` to modify the original links, which would break the
                // linked list. So we just return the inner metadata `M`.
                Some(&mut link_mut.meta)
            },
            None => None,
        }
    }

    /// Takes the current pointing frame out of the linked list.
    ///
    /// If successful, the frame is returned and the cursor is moved to the next frame. If the cursor is
    /// pointing to the back of the list then it is moved to the "ghost" non-element.
    pub fn take_current(&mut self, Tracked(own): Tracked<&mut CursorOwner<M>>) -> (res: (
        Option<UniqueFrame<Link<M>>>,
        Tracked<Option<FrameOwner<Link<M>>>>,
    ))
        requires
            old(own).wf(*old(self)),
        ensures
            final(own).wf(*final(self)),
            *final(final(self).list) == *final(old(self).list),
            final(own).list_own.list_id == old(own).list_own.list_id,
            final(self).list.list_id == old(self).list.list_id,
            old(own).idx < old(own).list_own.order.len() ==> {
                let o0 = old(own).list_own;
                let idx = old(own).idx;
                &&& res.0 is Some
                &&& res.1@ is Some
                &&& res.0->Some_0.paddr == o0.order[idx]
                &&& res.1@->Some_0.inv(res.0->Some_0)
                &&& res.1@->Some_0.meta.value().next is None
                &&& res.1@->Some_0.meta.value().prev is None
                &&& res.1@->Some_0.in_list.value() == 0
                &&& res.1@->Some_0.meta.value().meta == o0.perms[o0.order[idx]].value().meta
                &&& final(own).list_own.order == o0.order.remove(idx)
                &&& final(own).idx == idx
                &&& final(self).list.size == old(self).list.size - 1
                &&& forall|k: usize|
                    final(own).list_own.perms.dom().contains(k) ==> final(own).list_own.perms[k].value().meta == o0.perms[k].value().meta
            },
            old(own).idx >= old(own).list_own.order.len() ==> {
                &&& res.0 is None
                &&& res.1@ is None
                &&& final(own).list_own == old(own).list_own
                &&& final(own).idx == old(own).idx
                &&& *final(self).list == *old(self).list
            },
    {
        let ghost idx = own.idx;
        let ghost o0 = own.list_own;
        let ghost n = o0.order.len() as int;
        let current = match self.current {
            Some(c) => c,
            None => {
                return (None, Tracked(None));
            },
        };
        proof {
            assert(o0.node_ok(idx));
            o0.order.to_set_ensures();
            lemma_remove_seq(o0.order, idx);
        }
        let tracked mut cperm = own.list_own.perms.tracked_remove(current.addr());
        let tracked mut cslot = own.list_own.slots.tracked_remove(current.addr());
        proof {
            assert(slot_ok_in(o0.order, o0.slots, o0.list_id, idx));
        }

        let mut frame = {
            let meta_ptr = current;
            let paddr = meta_to_frame::<M>(meta_ptr);
            // SAFETY: The frame was forgotten when inserted into the linked list.
            UniqueFrame::<Link<M>>::from_raw(paddr)
        };

        let next_ptr = frame.meta(Tracked(&cperm)).next;
        let ghost perms_a;
        if let Some(prev) = frame.meta(Tracked(&cperm)).prev {
            // We own the previous node by `&mut self` and the node is initialized.
            proof {
                assert(idx > 0);
                assert(o0.node_ok(idx - 1));
            }
            let tracked mut pperm = own.list_own.perms.tracked_remove(prev.addr());
            let ghost p0 = pperm;
            let prev_mut = prev.borrow_mut(Tracked(&mut pperm));

            // debug_assert_eq!(prev_mut.next, Some(current));
            proof {
                assert(prev_mut.next == Some(current));
            }
            prev_mut.next = next_ptr;
            proof {
                assert(next_set(p0, pperm, next_ptr));
                own.list_own.perms.tracked_insert(prev.addr(), pperm);
            }
        } else {
            self.list.front = next_ptr;
        }
        proof {
            perms_a = own.list_own.perms;
        }
        let prev_ptr = frame.meta(Tracked(&cperm)).prev;
        if let Some(next) = frame.meta(Tracked(&cperm)).next {
            // We own the next node by `&mut self` and the node is initialized.
            proof {
                assert(idx + 1 < n);
                assert(o0.node_ok(idx + 1));
            }
            let tracked mut nperm = own.list_own.perms.tracked_remove(next.addr());
            let ghost n0 = nperm;
            let next_mut = next.borrow_mut(Tracked(&mut nperm));

            // debug_assert_eq!(next_mut.prev, Some(current));
            proof {
                assert(next_mut.prev == Some(current));
            }
            next_mut.prev = prev_ptr;
            self.current = Some(next);
            proof {
                assert(prev_set(n0, nperm, prev_ptr));
                own.list_own.perms.tracked_insert(next.addr(), nperm);
            }
        } else {
            self.list.back = prev_ptr;
            self.current = None;
        }

        frame.meta_mut(Tracked(&mut cperm)).next = None;
        frame.meta_mut(Tracked(&mut cperm)).prev = None;

        frame.in_list_store(Tracked(&mut cslot), 0);

        self.list.size -= 1;

        proof {
            let nr = o0.order.remove(idx);
            lemma_remove_ok(o0.order, o0.perms, own.list_own.perms, idx);
            lemma_slots_remove(o0.order, o0.slots, own.list_own.slots, o0.list_id, idx);
            own.list_own.order = nr;
            if idx + 1 < n {
                assert(nr[idx] == o0.order[idx + 1]);
            }
            if idx > 0 {
                assert(nr[idx - 1] == o0.order[idx - 1]);
            }
            if n - 1 > 0 {
                assert(nr[0] == if idx > 0 { o0.order[0] } else { o0.order[1] });
                assert(nr[n - 2] == if idx < n - 1 { o0.order[n - 1] } else { o0.order[n - 2] });
            }
            // `list_id == 0 ==> empty` still holds: the order only shrank.
        }
        (Some(frame), Tracked(Some(FrameOwner { meta: cperm, in_list: cslot })))
    }

    /// Inserts a frame before the current frame.
    ///
    /// If the cursor is pointing at the "ghost" non-element then the new element is inserted at the back of
    /// the [`LinkedList`].
    pub fn insert_before(
        &mut self,
        frame: UniqueFrame<Link<M>>,
        Tracked(own): Tracked<&mut CursorOwner<M>>,
        Tracked(fo): Tracked<FrameOwner<Link<M>>>,
    )
        requires
            old(own).wf(*old(self)),
            fo.inv(frame),
            // The three `debug_assert!`s of the upstream function: the frame is in no list.
            fo.meta.value().next is None,
            fo.meta.value().prev is None,
            fo.in_list.value() == 0,
            old(own).list_own.order.len() < usize::MAX,
        ensures
            final(own).wf(*final(self)),
            *final(final(self).list) == *final(old(self).list),
            final(own).list_own.order == old(own).list_own.order.insert(
                old(own).idx,
                frame.paddr,
            ),
            final(own).idx == old(own).idx + 1,
            final(self).list.size == old(self).list.size + 1,
            final(self).list.list_id != 0,
            old(self).list.list_id != 0 ==> final(self).list.list_id == old(self).list.list_id,
            final(own).list_own.slots[frame.paddr].value() == final(self).list.list_id,
            final(own).list_own.perms[frame.paddr].value().meta == fo.meta.value().meta,
            forall|a: usize|
                old(own).list_own.perms.dom().contains(a) ==> final(own).list_own.perms[a].value().meta == old(own).list_own.perms[a].value().meta,
    {
        let ghost idx = own.idx;
        let ghost o0 = own.list_own;
        let ghost n = o0.order.len() as int;
        let ghost fa = frame.paddr;
        let tracked FrameOwner { meta: mut fmeta, in_list: mut fslot } = fo;
        proof {
            lemma_frame_fresh(&own.list_own, &mut fslot);
            lemma_insert_seq(o0.order, idx, fa);
            o0.order.to_set_ensures();
        }

        let frame_ptr = frame.meta_ptr();

        if let Some(current) = self.current {
            // We own the current node by `&mut self` and the node is initialized.
            proof {
                assert(o0.node_ok(idx));
            }
            let tracked mut cperm = own.list_own.perms.tracked_remove(current.addr());
            let ghost c0 = cperm;
            let current_mut = current.borrow_mut(Tracked(&mut cperm));

            if let Some(prev) = current_mut.prev {
                // We own the previous node by `&mut self` and the node is initialized.
                proof {
                    assert(idx > 0);
                    assert(o0.node_ok(idx - 1));
                }
                let tracked mut pperm = own.list_own.perms.tracked_remove(prev.addr());
                let ghost p0 = pperm;
                let prev_mut = prev.borrow_mut(Tracked(&mut pperm));

                // debug_assert_eq!(prev_mut.next, Some(*current));
                proof {
                    assert(prev_mut.next == Some(current));
                }
                prev_mut.next = Some(frame_ptr);

                frame.meta_mut(Tracked(&mut fmeta)).prev = Some(prev);
                frame.meta_mut(Tracked(&mut fmeta)).next = Some(current);
                current_mut.prev = Some(frame_ptr);
                proof {
                    assert(next_set(p0, pperm, Some(ptr_at::<M>(fa))));
                    assert(prev_set(c0, cperm, Some(ptr_at::<M>(fa))));
                    own.list_own.perms.tracked_insert(current.addr(), cperm);
                    own.list_own.perms.tracked_insert(prev.addr(), pperm);
                    own.list_own.perms.tracked_insert(fa, fmeta);
                    lemma_insert_ok(o0.order, o0.perms, own.list_own.perms, idx, fa);
                }
            } else {
                // debug_assert_eq!(self.list.front, Some(*current));
                proof {
                    assert(idx == 0);
                }
                frame.meta_mut(Tracked(&mut fmeta)).next = Some(current);
                current_mut.prev = Some(frame_ptr);
                self.list.front = Some(frame_ptr);
                proof {
                    assert(prev_set(c0, cperm, Some(ptr_at::<M>(fa))));
                    own.list_own.perms.tracked_insert(current.addr(), cperm);
                    own.list_own.perms.tracked_insert(fa, fmeta);
                    lemma_insert_ok(o0.order, o0.perms, own.list_own.perms, idx, fa);
                }
            }
        } else {
            // We are at the "ghost" non-element.
            proof {
                assert(idx == n);
            }
            if let Some(back) = self.list.back {
                // We have ownership of the links via `&mut self`.
                proof {
                    assert(n > 0);
                    assert(o0.node_ok(n - 1));
                }
                let tracked mut bperm = own.list_own.perms.tracked_remove(back.addr());
                let ghost b0 = bperm;
                let back_mut = back.borrow_mut(Tracked(&mut bperm));
                // debug_assert!(back.as_mut().next.is_none());
                proof {
                    assert(back_mut.next is None);
                }
                back_mut.next = Some(frame_ptr);
                frame.meta_mut(Tracked(&mut fmeta)).prev = Some(back);
                self.list.back = Some(frame_ptr);
                proof {
                    assert(next_set(b0, bperm, Some(ptr_at::<M>(fa))));
                    own.list_own.perms.tracked_insert(back.addr(), bperm);
                    own.list_own.perms.tracked_insert(fa, fmeta);
                    lemma_insert_ok(o0.order, o0.perms, own.list_own.perms, idx, fa);
                }
            } else {
                // debug_assert_eq!(self.list.front, None);
                proof {
                    assert(n == 0);
                }
                self.list.front = Some(frame_ptr);
                self.list.back = Some(frame_ptr);
                proof {
                    own.list_own.perms.tracked_insert(fa, fmeta);
                    lemma_insert_ok(o0.order, o0.perms, own.list_own.perms, idx, fa);
                }
            }
        }

        let id = self.list.lazy_get_id();
        frame.in_list_store(Tracked(&mut fslot), id);

        // Forget the frame to transfer the ownership to the list.
        let _ = frame.into_raw();

        self.list.size += 1;

        proof {
            let no = o0.order.insert(idx, fa);
            own.list_own.slots.tracked_insert(fa, fslot);
            lemma_slots_insert(o0.order, o0.slots, own.list_own.slots, id, idx, fa);
            own.list_own.order = no;
            own.list_own.list_id = id;
            own.idx = idx + 1;
            assert(no[idx] == fa);
            if idx + 1 <= n {
                assert(no[idx + 1] == o0.order[idx]);
            }
        }
    }
}

impl<M> LinkedList<M> {
    /// Pushes a frame to the front of the linked list.
    pub fn push_front(
        &mut self,
        frame: UniqueFrame<Link<M>>,
        Tracked(owner): Tracked<ListOwner<M>>,
        Tracked(fo): Tracked<FrameOwner<Link<M>>>,
    ) -> (res: Tracked<ListOwner<M>>)
        requires
            owner.inv(),
            owner.wf(*old(self)),
            fo.inv(frame),
            fo.meta.value().next is None,
            fo.meta.value().prev is None,
            fo.in_list.value() == 0,
            owner.order.len() < usize::MAX,
        ensures
            res@.inv(),
            res@.wf(*final(self)),
            res@.order == owner.order.insert(0, frame.paddr),
            final(self).size == old(self).size + 1,
            final(self).list_id != 0,
            res@.slots[frame.paddr].value() == final(self).list_id,
    {
        let (mut cursor, Tracked(mut cown)) = self.cursor_front_mut(Tracked(owner));
        cursor.insert_before(frame, Tracked(&mut cown), Tracked(fo));
        Tracked(cown.list_own)
    }

    /// Pops a frame from the front of the linked list.
    pub fn pop_front(&mut self, Tracked(owner): Tracked<ListOwner<M>>) -> (res: (
        Option<UniqueFrame<Link<M>>>,
        Tracked<Option<FrameOwner<Link<M>>>>,
        Tracked<ListOwner<M>>,
    ))
        requires
            owner.inv(),
            owner.wf(*old(self)),
        ensures
            res.2@.inv(),
            res.2@.wf(*final(self)),
            owner.order.len() == 0 ==> res.0 is None && res.2@.order == owner.order,
            owner.order.len() > 0 ==> {
                &&& res.0 is Some
                &&& res.1@ is Some
                &&& res.0->Some_0.paddr == owner.order[0]
                &&& res.1@->Some_0.inv(res.0->Some_0)
                &&& res.1@->Some_0.meta.value().next is None
                &&& res.1@->Some_0.meta.value().prev is None
                &&& res.1@->Some_0.in_list.value() == 0
                &&& res.2@.order == owner.order.remove(0)
                &&& final(self).size == old(self).size - 1
            },
    {
        let (mut cursor, Tracked(mut cown)) = self.cursor_front_mut(Tracked(owner));
        let (frame, fo) = cursor.take_current(Tracked(&mut cown));
        (frame, fo, Tracked(cown.list_own))
    }

    /// Pushes a frame to the back of the linked list.
    pub fn push_back(
        &mut self,
        frame: UniqueFrame<Link<M>>,
        Tracked(owner): Tracked<ListOwner<M>>,
        Tracked(fo): Tracked<FrameOwner<Link<M>>>,
    ) -> (res: Tracked<ListOwner<M>>)
        requires
            owner.inv(),
            owner.wf(*old(self)),
            fo.inv(frame),
            fo.meta.value().next is None,
            fo.meta.value().prev is None,
            fo.in_list.value() == 0,
            owner.order.len() < usize::MAX,
        ensures
            res@.inv(),
            res@.wf(*final(self)),
            res@.order == owner.order.push(frame.paddr),
            final(self).size == old(self).size + 1,
            final(self).list_id != 0,
            res@.slots[frame.paddr].value() == final(self).list_id,
    {
        let (mut cursor, Tracked(mut cown)) = self.cursor_at_ghost_mut(Tracked(owner));
        cursor.insert_before(frame, Tracked(&mut cown), Tracked(fo));
        proof {
            assert(cown.list_own.order =~= owner.order.push(frame.paddr));
        }
        Tracked(cown.list_own)
    }

    /// Pops a frame from the back of the linked list.
    pub fn pop_back(&mut self, Tracked(owner): Tracked<ListOwner<M>>) -> (res: (
        Option<UniqueFrame<Link<M>>>,
        Tracked<Option<FrameOwner<Link<M>>>>,
        Tracked<ListOwner<M>>,
    ))
        requires
            owner.inv(),
            owner.wf(*old(self)),
        ensures
            res.2@.inv(),
            res.2@.wf(*final(self)),
            owner.order.len() == 0 ==> res.0 is None && res.2@.order == owner.order,
            owner.order.len() > 0 ==> {
                &&& res.0 is Some
                &&& res.1@ is Some
                &&& res.0->Some_0.paddr == owner.order[owner.order.len() - 1]
                &&& res.1@->Some_0.inv(res.0->Some_0)
                &&& res.1@->Some_0.meta.value().next is None
                &&& res.1@->Some_0.meta.value().prev is None
                &&& res.1@->Some_0.in_list.value() == 0
                &&& res.2@.order == owner.order.drop_last()
                &&& final(self).size == old(self).size - 1
            },
    {
        let (mut cursor, Tracked(mut cown)) = self.cursor_back_mut(Tracked(owner));
        let (frame, fo) = cursor.take_current(Tracked(&mut cown));
        proof {
            if owner.order.len() > 0 {
                assert(cown.list_own.order =~= owner.order.drop_last());
            }
        }
        (frame, fo, Tracked(cown.list_own))
    }
}

impl<M> Link<M> {
    /// Creates a new linked list link.
    pub const fn new(meta: M) -> (r: Self)
        ensures
            r.next is None,
            r.prev is None,
            r.meta == meta,
    {
        Self { next: None, prev: None, meta }
    }
}

} // verus!

fn main() {}
