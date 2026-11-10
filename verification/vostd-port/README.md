# vostd port: OSTD frame linked list (T-VER-05)

NOTICE: `frame_linked_list.rs` is derived from vostd (https://github.com/asterinas/vostd, rev
`020b85418e2d4edcd5089cbf818e477b1d07f3ea`, MPL-2.0), files `ostd/src/mm/frame/linked_list.rs` and
`ostd/specs/mm/frame/linked_list/*`. It is checked against Asterinas v0.19.0
(`c4b7ed575b7b32773475b194d2ad41fb6d03765b`), `ostd/src/mm/frame/linked_list.rs`. MPL-2.0 applies; the SPDX header is kept.

Run: `verify-gate -- scripts/verify.sh` (the file is picked up by the `verification/**/*.rs` glob; no change to
verify.sh needed). Result with Verus 0.2026.10.26.d17fbd0 (single file, 2026-10-31):
`verification results:: 37 verified, 0 errors` (two harmless warnings about `static`/`const` inside the
`external_body` helper). Earlier traversal-only version (T-VER-05): 11 verified.

## Drift (vostd copy vs v0.19.0)

- Upstream `linked_list.rs` v0.16.0 -> v0.19.0 changed 4 lines: import order, `MetaSlot` moved into the `use super::{..}`
  list, `log::error!` -> `crate::error!`. The list algorithm is unchanged, so nothing in the proofs goes stale.
- vostd's file is 1728 lines against 464 upstream. Its exec bodies are not upstream's: it rewrote them around
  `Repr<MetaSlotSmall>`, `simple_pptr`, `UniqueFrame` owners and `MetaRegionOwners`, backed by ~1,400 lines of
  `specs/mm/frame/linked_list/*` plus `unique.rs`, `meta_owners.rs`, `meta_region_owners.rs`, `vstd_extra::cast_ptr`.
  Those depend on v0.16.0 `meta.rs`/`unique.rs`/`mod.rs` (11-20% drift each), so a literal port needs those ported first.
- Verus mismatch: vostd builds with `cargo dv` and its own Verus fork (`tools/verus`, `dv` submodule empty here).
  Building its crate on our release (cargo-verus, `vstd =0.0.0-2026-10-25-0158`) fails in `vstd_extra`
  (`external/smallvec.rs`: `IteratorSpec::produces` needs `dyn`, `from_iter_ensures` signature changed; 3 errors, 32 s).
  That made the crate route a dead end for this budget (it would also need 28k lines of ostd/mm); the scratch copy is
  `~/scratch/lucinaos/vostd-build` (not part of the repo). Source syntax (`old(x)`/`final(x)`) is the same in both.

## What is proved (all inside one function-level module, no vostd dependency, only `vstd::simple_pptr`)

Model: `ListOwner` = ghost sequence `order` of node addresses front to back, one `PointsTo<Link<M>>` per node
(`perms`), one `PointsTo<u64>` per node for the slot's `in_list` field (`slots`), and the ghost id `list_id` the exec
list carries.
`ListOwner::inv`: addresses distinct, both permission domains == the sequence, every node initialised with
`next`/`prev` exactly its neighbours (None at the ends), every `in_list` cell initialised and equal to `list_id`,
and `list_id == 0` only for an empty list. `wf`: the `LinkedList` header (`size`, `front`, `back`, `list_id`) matches.
`CursorOwner` adds the cursor index (`idx == len` is the ghost non-element).
Frame stub: `UniqueFrame<F> { paddr }` plus the tracked `FrameOwner<F> { meta: PointsTo<F>, in_list: PointsTo<u64> }`
(`FrameOwner::inv(frame)`: both cells at the frame's address, initialised). `meta_to_frame`/`frame_to_meta` are the
identity, so the node address, the frame address and the metadata address coincide.

Proved functions (v0.19.0 names), 37 verified items in total:
- `LinkedList::{new, size, is_empty, cursor_front_mut, cursor_back_mut, cursor_at_ghost_mut, lazy_get_id}`,
  `CursorMut::{move_next, move_prev, as_list}`, `Link::new` (as before).
- `CursorMut::insert_before(frame, own, fo)`: ensures `order' == order.insert(idx, frame.paddr)`, `idx' == idx + 1`
  (the cursor keeps pointing at the same element), `size' == size + 1`, `wf`/`inv` preserved, the new node's
  `in_list` cell holds the (non-zero) list id, an existing id is kept, the new node keeps its metadata, every other
  node's metadata is unchanged. All four upstream cases are covered (current with prev, current at front, ghost
  with back, ghost on empty list).
- `CursorMut::take_current(own)`: for `idx < len` returns the frame at `order[idx]` with its `FrameOwner`
  (`inv`, `next`/`prev` None, `in_list == 0`, metadata unchanged), `order' == order.remove(idx)`, `idx' == idx`,
  `size' == size - 1`, `wf`/`inv` preserved. For the ghost element returns `None` and changes nothing.
- `CursorMut::current_meta(own)`: returns `&mut M` of `order[idx]` (None for the ghost). It proves
  `same_links(old, final)` (order, ids, slots and every `next`/`prev` unchanged, only metadata may change);
  `lemma_same_links_inv` turns that into `inv` for the final owner (not folded into the `ensures`).
- `LinkedList::{push_front, push_back, pop_front, pop_back}`, written like upstream as cursor + `insert_before` /
  `take_current`: `push_front` gives `order.insert(0, p)`, `push_back` gives `order.push(p)`, `pop_front` gives
  `order.remove(0)`, `pop_back` gives `order.drop_last()`; all preserve `inv`/`wf` and update `size` and `in_list`.
- Lemmas: `lemma_insert_seq/remove_seq` (sequence insert/remove: indices, no duplicates, `to_set`),
  `lemma_insert_ok/remove_ok` (every node's neighbours after relinking), `lemma_slots_insert/remove`,
  `lemma_frame_fresh` (a frame whose `in_list` cell is a different cell than all listed ones is not listed),
  `lemma_same_links_inv`.
`move_next`/`move_prev` follow the sequence exactly (including wrap through the ghost element) and preserve `wf`.
`lazy_get_id` returns a non-zero id, keeps an existing id, and does not touch front/back/size.

## What changed from upstream text

- `NonNull<Link<M>>` -> vstd `PPtr<Link<M>>`; `unsafe { current.as_ref().next }` -> `current.borrow(Tracked(perm)).next`;
  `unsafe { x.as_mut() }` -> `x.borrow_mut(Tracked(&mut perm))` with the permission taken out of (and put back into)
  the owner map with `tracked_remove`/`tracked_insert`.
- `UniqueFrame` is the stub above. `frame.meta()`/`meta_mut()` take the metadata permission explicitly and `&self`
  (not `&mut self`); `slot().in_list.load/store` are `in_list_load/in_list_store` on a `PointsTo<u64>` cell;
  `from_raw`/`into_raw` only convert the address, the caller moves the permissions between `ListOwner` and
  `FrameOwner` in proof code. `NonNull::from(next_mut)` -> the `PPtr` of the neighbour. `self.current?` -> `match`.
- The three `debug_assert!`s at the top of `insert_before` (frame has no `next`/`prev`, `in_list == 0`) are `requires`
  (they hold for any frame that is in no list; the runtime check cannot be derived from the types). The other
  `debug_assert*!`s are `proof { assert(..) }` and are proved from the owner invariant.
- `insert_before` also `requires order.len() < usize::MAX` (so `size += 1` cannot overflow).
- Extra `Tracked(..)` parameters / return values for the ghost owners (the list wrappers take and return the
  `ListOwner` by value); `requires`/`ensures` added; `cursor_*_mut`, `insert_before`, `take_current` state
  `*final(final(self).list) == ...` (Verus needs it to see through the `&mut LinkedList` held by the cursor).
- Struct fields are `pub` (Verus rejects spec access to private fields from `pub` specs); upstream fields are private.
- No `M: AnyFrameMeta`/`Link<M>: AnyFrameMeta` bounds (the trait is not modelled).
- `lazy_get_id`: the `static LIST_ID_ALLOCATOR`, `fetch_add`, overflow check, log and `abort()` moved into `next_list_id`.
- `fn main() {}` added (needed for a standalone `verus <file>`).

## Assumed / trusted (complete list)

- `next_list_id` is `#[verifier::external_body]`: ensures `0 < id < i64::MAX`. The atomic counter, the exhaustion
  check and `abort()` are not verified. This is the only `external_body` in the file; there is no `assume`, `admit` or
  `assume_specification` anywhere.
- Modelling choices (definitions, not axioms, but they are what the proof is about):
  - The slot's `in_list: AtomicU64` is a separate `PointsTo<u64>` cell at the frame's address, and `Relaxed`
    atomics are plain reads and writes. `UniqueFrame::{into_raw, from_raw}` are address conversions only; the real
    "forgotten frame" ownership transfer is the caller's tracked move of the permissions.
  - `meta_to_frame` is the identity; `UniqueFrame` uniqueness is the owner token, not proved from `Frame` refcounts.
  - The cursor/list owners are not tied to the real `MetaSlot` memory.
- Verus, Z3, vstd (`simple_pptr`, `Map`/`Seq` axioms).

## Not ported (remaining work)

- `contains` and `cursor_mut_at`: they compare a slot's `in_list` with `lazy_get_id()` for an arbitrary address. A
  correct spec needs a global invariant "every non-zero `in_list` value was issued by the allocator and belongs to
  exactly one list" (a ghost model of `LIST_ID_ALLOCATOR`, so that a freshly allocated id matches no frame), plus a
  model of `get_slot` failing for addresses outside the metadata region. Not started.
- `Drop for LinkedList` (a loop of `take_current` until `None`; needs a loop invariant on the owner), `Deref`/`DerefMut`
  for `Link`, and `AnyFrameMeta for Link`.
- `frame/meta.rs` (658 lines, 11% drift from v0.16.0): not started. It holds `mapping::{frame_to_meta, meta_to_frame}`
  (currently the identity in the model), `MetaSlot`/`get_slot`, `AnyFrameMeta`, `init`, and the `impl_frame_meta_for!`
  macros. `unique.rs` (202 lines) and `mod.rs` (379 lines) would be next for a real `UniqueFrame`, replacing the stub.
