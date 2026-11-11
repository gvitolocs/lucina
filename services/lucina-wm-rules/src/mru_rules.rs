// The most-recently-used order of the live apps: the search for an app, focusing an app and an app that ends, and the three moves
// they are made of (an app already listed goes to the front, a new app is put at the front, an ended app leaves). Verus checks this
// very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011); verification/juno-wm-rules/mutants.sh plants bugs that
// Verus must refuse. switcher.rs keeps the order (`Mru`) and the Meta+Tab snapshot (`Switcher`) and calls `index_of`, `focus` and
// `remove` for every search, focus and removal; it holds no copy of them.
//
// Proved for each move: the exact new order (every place is named), which ids the first `len` places hold afterwards (the same ones,
// or the same plus / minus the one moved), and that an order without duplicates stays without duplicates. Proved for `index_of`: a
// found place holds the id and is the first one that does, and None means the id is not among the first `len`. Proved for `focus` and
// `remove`, with the invariant of `Mru` (`len <= MAX`, the first `len` ids pairwise distinct) as `requires`: the invariant holds
// again afterwards, and exactly which ids are listed and where. `focus` puts `id` at place 0 and keeps the order of the others (or
// refuses, changing nothing, only when the list is full and `id` is not in it); `remove` takes `id` out, returns the place it had and
// moves only the ids after it, one place forward.
//
// Not proved here: that every `Mru` meets the invariant. It holds because `Mru::new` starts empty and the fields of `Mru` are private
// to switcher.rs, where only `focus` and `remove` (these functions) change them; that is read off the code, not checked. `Switcher`'s
// snapshot is a copy of a `Mru` made by `Switcher::begin` (so it meets the invariant by the same argument, unchecked), and `begin`,
// `step` and the selection arithmetic of `Switcher::remove` are not under Verus. "The live apps" is not a notion of this crate: above
// `MAX` apps `focus` returns false and the app is not listed.

/// How many ids the order holds at most (`switcher::MAX_APPS`).
pub const MAX: usize = 16;

//@ pub open spec fn distinct<T>(s: Seq<T>) -> bool {
//@     forall|i: int, j: int| 0 <= i < j < s.len() ==> s[i] != s[j]
//@ }

// Where place `m` of the order after `move_to_front(.., idx)` took its id from.
//@ spec fn moved_from(m: int, idx: int) -> int {
//@     if m == 0 { idx } else if m <= idx { m - 1 } else { m }
//@ }

// The ids and the absence of duplicates after `move_to_front`, from its positional `ensures` (`o` before, `n` after). The lemma
// needs only lengths and bounds, so a wrong body cannot fail it: it fails the function's `ensures`.
//@ proof fn move_to_front_ids<T>(o: Seq<T>, n: Seq<T>, len: int, idx: int)
//@     requires o.len() == MAX, n.len() == MAX, 0 <= idx < len <= MAX,
//@     ensures
//@         n[0] == o[idx] && (forall|j: int| 1 <= j <= idx ==> n[j] == o[j - 1]) && (forall|j: int| idx < j < MAX ==> n[j] == o[j]) ==>
//@             (forall|v: T| n.take(len).contains(v) <==> o.take(len).contains(v)) && (distinct(o.take(len)) ==> distinct(n.take(len))),
//@ {
//@     if n[0] == o[idx] && (forall|j: int| 1 <= j <= idx ==> n[j] == o[j - 1]) && (forall|j: int| idx < j < MAX ==> n[j] == o[j]) {
//@         assert(forall|m: int| 0 <= m < len ==> #[trigger] n[m] == o[moved_from(m, idx)]);
//@         assert forall|v: T| o.take(len).contains(v) implies n.take(len).contains(v) by {
//@             let k = choose|k: int| 0 <= k < len && o[k] == v;
//@             let m: int = if k == idx { 0 } else if k < idx { k + 1 } else { k };
//@             assert(moved_from(m, idx) == k);
//@             assert(n.take(len)[m] == v);
//@         }
//@         assert forall|v: T| n.take(len).contains(v) implies o.take(len).contains(v) by {
//@             let m = choose|m: int| 0 <= m < len && n[m] == v;
//@             assert(o.take(len)[moved_from(m, idx)] == v);
//@         }
//@         if distinct(o.take(len)) {
//@             assert forall|a: int, b: int| 0 <= a < b < len implies #[trigger] n.take(len)[a] != #[trigger] n.take(len)[b] by {
//@                 let ka = moved_from(a, idx);
//@                 let kb = moved_from(b, idx);
//@                 assert(ka != kb);
//@                 assert(n.take(len)[a] == o.take(len)[ka]);
//@                 assert(n.take(len)[b] == o.take(len)[kb]);
//@                 if ka < kb {
//@                     assert(o.take(len)[ka] != o.take(len)[kb]);
//@                 } else {
//@                     assert(o.take(len)[kb] != o.take(len)[ka]);
//@                 }
//@             }
//@         }
//@     }
//@ }

// The ids and the absence of duplicates after `push_front`, from its positional `ensures`.
//@ proof fn push_front_ids<T>(o: Seq<T>, n: Seq<T>, len: int, id: T)
//@     requires o.len() == MAX, n.len() == MAX, 0 <= len < MAX,
//@     ensures
//@         n[0] == id && (forall|j: int| 1 <= j <= len ==> n[j] == o[j - 1]) ==>
//@             (forall|v: T| n.take(len + 1).contains(v) <==> (o.take(len).contains(v) || v == id))
//@             && (distinct(o.take(len)) && !o.take(len).contains(id) ==> distinct(n.take(len + 1))),
//@ {
//@     if n[0] == id && (forall|j: int| 1 <= j <= len ==> n[j] == o[j - 1]) {
//@         assert forall|v: T| o.take(len).contains(v) implies n.take(len + 1).contains(v) by {
//@             let k = choose|k: int| 0 <= k < len && o[k] == v;
//@             assert(n.take(len + 1)[k + 1] == v);
//@         }
//@         assert forall|v: T| n.take(len + 1).contains(v) implies (o.take(len).contains(v) || v == id) by {
//@             let m = choose|m: int| 0 <= m < len + 1 && n[m] == v;
//@             if m > 0 {
//@                 assert(o.take(len)[m - 1] == v);
//@             }
//@         }
//@         assert(n.take(len + 1)[0] == id);
//@         if distinct(o.take(len)) && !o.take(len).contains(id) {
//@             assert forall|a: int, b: int| 0 <= a < b < len + 1 implies #[trigger] n.take(len + 1)[a] != #[trigger] n.take(len + 1)[b] by {
//@                 if a == 0 {
//@                     assert(o.take(len)[b - 1] == n[b]);
//@                 } else {
//@                     assert(o.take(len)[a - 1] == n[a]);
//@                     assert(o.take(len)[b - 1] == n[b]);
//@                 }
//@             }
//@         }
//@     }
//@ }

// The ids and the absence of duplicates after `remove_at`, from its positional `ensures`.
//@ proof fn remove_at_ids<T>(o: Seq<T>, n: Seq<T>, len: int, idx: int)
//@     requires o.len() == MAX, n.len() == MAX, 0 <= idx < len <= MAX,
//@     ensures
//@         (forall|j: int| 0 <= j < idx ==> n[j] == o[j]) && (forall|j: int| idx <= j < len - 1 ==> n[j] == o[j + 1]) ==>
//@             (forall|v: T| n.take(len - 1).contains(v) ==> o.take(len).contains(v))
//@             && (forall|v: T| o.take(len).contains(v) && v != o[idx] ==> n.take(len - 1).contains(v))
//@             && (distinct(o.take(len)) ==> distinct(n.take(len - 1)) && !n.take(len - 1).contains(o[idx])),
//@ {
//@     if (forall|j: int| 0 <= j < idx ==> n[j] == o[j]) && (forall|j: int| idx <= j < len - 1 ==> n[j] == o[j + 1]) {
//@         assert forall|v: T| n.take(len - 1).contains(v) implies o.take(len).contains(v) by {
//@             let m = choose|m: int| 0 <= m < len - 1 && n[m] == v;
//@             let k: int = if m < idx { m } else { m + 1 };
//@             assert(o.take(len)[k] == v);
//@         }
//@         assert forall|v: T| o.take(len).contains(v) && v != o[idx] implies n.take(len - 1).contains(v) by {
//@             let k = choose|k: int| 0 <= k < len && o[k] == v;
//@             let m: int = if k < idx { k } else { k - 1 };
//@             assert(n.take(len - 1)[m] == v);
//@         }
//@         if distinct(o.take(len)) {
//@             assert forall|a: int, b: int| 0 <= a < b < len - 1 implies #[trigger] n.take(len - 1)[a] != #[trigger] n.take(len - 1)[b] by {
//@                 let ka: int = if a < idx { a } else { a + 1 };
//@                 let kb: int = if b < idx { b } else { b + 1 };
//@                 assert(o.take(len)[ka] == n[a]);
//@                 assert(o.take(len)[kb] == n[b]);
//@             }
//@             assert forall|m: int| 0 <= m < len - 1 implies #[trigger] n.take(len - 1)[m] != o[idx] by {
//@                 let k: int = if m < idx { m } else { m + 1 };
//@                 assert(o.take(len)[k] == n[m]);
//@                 if k < idx {
//@                     assert(o.take(len)[k] != o.take(len)[idx]);
//@                 } else {
//@                     assert(o.take(len)[idx] != o.take(len)[k]);
//@                 }
//@             }
//@         }
//@     }
//@ }

/// The id at `idx` moves to place 0 and the ones before it each go one place back; the rest stays.
#[allow(unused_variables)] // `len` only bounds the proof
pub fn move_to_front<T: Copy>(ids: &mut [T; MAX], len: usize, idx: usize)
//@ requires len <= MAX, idx < len,
//@ ensures
//@     final(ids)@[0] == old(ids)@[idx as int],
//@     forall|j: int| 1 <= j <= idx ==> final(ids)@[j] == old(ids)@[j - 1],
//@     forall|j: int| idx < j < MAX ==> final(ids)@[j] == old(ids)@[j],
//@     forall|v: T| final(ids)@.take(len as int).contains(v) <==> old(ids)@.take(len as int).contains(v),
//@     distinct(old(ids)@.take(len as int)) ==> distinct(final(ids)@.take(len as int)),
{
    let id = ids[idx];
    let mut i = idx;
    while i > 0
    //@ invariant
    //@     i <= idx, idx < len <= MAX, id == old(ids)@[idx as int],
    //@     ids@.len() == MAX,
    //@     forall|j: int| 0 <= j <= i ==> ids@[j] == old(ids)@[j],
    //@     forall|j: int| i < j <= idx ==> ids@[j] == old(ids)@[j - 1],
    //@     forall|j: int| idx < j < MAX ==> ids@[j] == old(ids)@[j],
    //@ decreases i,
    {
        ids[i] = ids[i - 1];
        i -= 1;
    }
    ids[0] = id;
    //@ proof { move_to_front_ids(old(ids)@, ids@, len as int, idx as int); }
}

/// `id` is put at place 0 and the `len` ids before it each go one place back; `len` must be below `MAX`.
pub fn push_front<T: Copy>(ids: &mut [T; MAX], len: usize, id: T)
//@ requires len < MAX,
//@ ensures
//@     final(ids)@[0] == id,
//@     forall|j: int| 1 <= j <= len ==> final(ids)@[j] == old(ids)@[j - 1],
//@     forall|j: int| len < j < MAX ==> final(ids)@[j] == old(ids)@[j],
//@     forall|v: T| final(ids)@.take(len as int + 1).contains(v) <==> (old(ids)@.take(len as int).contains(v) || v == id),
//@     distinct(old(ids)@.take(len as int)) && !old(ids)@.take(len as int).contains(id) ==> distinct(final(ids)@.take(len as int + 1)),
{
    let mut i = len;
    while i > 0
    //@ invariant
    //@     i <= len < MAX,
    //@     ids@.len() == MAX,
    //@     forall|j: int| 0 <= j <= i ==> ids@[j] == old(ids)@[j],
    //@     forall|j: int| i < j <= len ==> ids@[j] == old(ids)@[j - 1],
    //@     forall|j: int| len < j < MAX ==> ids@[j] == old(ids)@[j],
    //@ decreases i,
    {
        ids[i] = ids[i - 1];
        i -= 1;
    }
    ids[0] = id;
    //@ proof { push_front_ids(old(ids)@, ids@, len as int, id); }
}

/// The id at `idx` leaves: the ones after it each go one place forward (the caller then counts one fewer).
pub fn remove_at<T: Copy>(ids: &mut [T; MAX], len: usize, idx: usize)
//@ requires len <= MAX, idx < len,
//@ ensures
//@     forall|j: int| 0 <= j < idx ==> final(ids)@[j] == old(ids)@[j],
//@     forall|j: int| idx <= j < len - 1 ==> final(ids)@[j] == old(ids)@[j + 1],
//@     forall|j: int| len - 1 <= j < MAX ==> final(ids)@[j] == old(ids)@[j],
//@     forall|v: T| final(ids)@.take(len as int - 1).contains(v) ==> old(ids)@.take(len as int).contains(v),
//@     forall|v: T| old(ids)@.take(len as int).contains(v) && v != old(ids)@[idx as int] ==> final(ids)@.take(len as int - 1).contains(v),
//@     distinct(old(ids)@.take(len as int)) ==>
//@         distinct(final(ids)@.take(len as int - 1)) && !final(ids)@.take(len as int - 1).contains(old(ids)@[idx as int]),
{
    let mut i = idx;
    while i + 1 < len
    //@ invariant
    //@     idx <= i, i + 1 <= len, len <= MAX,
    //@     ids@.len() == MAX,
    //@     forall|j: int| 0 <= j < idx ==> ids@[j] == old(ids)@[j],
    //@     forall|j: int| idx <= j < i ==> ids@[j] == old(ids)@[j + 1],
    //@     forall|j: int| i <= j < MAX ==> ids@[j] == old(ids)@[j],
    //@ decreases len - i,
    {
        ids[i] = ids[i + 1];
        i += 1;
    }
    //@ proof { remove_at_ids(old(ids)@, ids@, len as int, idx as int); }
}

/// A running application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppId(pub u16);

/// The first place of `id` among the first `len` entries of `ids`, None when it is not there.
pub fn index_of(ids: &[AppId; MAX], len: usize, id: AppId) -> Option<usize> //@ret
//@ requires len <= MAX,
//@ ensures
//@     match ret {
//@         Some(i) => i < len && ids@[i as int] == id && forall|j: int| 0 <= j < i ==> ids@[j] != id,
//@         None => !ids@.take(len as int).contains(id),
//@     },
{
    let mut i = 0;
    while i < len && i < MAX
    //@ invariant
    //@     i <= len <= MAX,
    //@     forall|j: int| 0 <= j < i ==> ids@[j] != id,
    //@ decreases len - i,
    {
        if ids[i].0 == id.0 {
            return Some(i);
        }
        i += 1;
    }
    // Only an implication, so a loop that stops early fails the `ensures`, not this assert.
    //@ assert((forall|j: int| 0 <= j < len ==> ids@[j] != id) ==> !ids@.take(len as int).contains(id)) by {
    //@     if (forall|j: int| 0 <= j < len ==> ids@[j] != id) && ids@.take(len as int).contains(id) {
    //@         let k = choose|k: int| 0 <= k < len && ids@.take(len as int)[k] == id;
    //@         assert(ids@[k] == id);
    //@     }
    //@ }
    None
}

/// `id` becomes the first app: an app already listed moves to the front, a new one is put there. Returns false, changing
/// nothing, only when the app is absent and the list is full.
pub fn focus(ids: &mut [AppId; MAX], len: &mut usize, id: AppId) -> bool //@ret
//@ requires *old(len) <= MAX, distinct(old(ids)@.take(*old(len) as int)),
//@ ensures
//@     *final(len) <= MAX,
//@     distinct(final(ids)@.take(*final(len) as int)),
//@     ret == (old(ids)@.take(*old(len) as int).contains(id) || *old(len) < MAX),
//@     ret ==> final(ids)@[0] == id,
//@     ret ==> forall|v: AppId| final(ids)@.take(*final(len) as int).contains(v)
//@         <==> (old(ids)@.take(*old(len) as int).contains(v) || v == id),
//@     ret && old(ids)@.take(*old(len) as int).contains(id) ==> *final(len) == *old(len)
//@         && exists|i: int| 0 <= i < *old(len) && #[trigger] old(ids)@[i] == id
//@             && (forall|j: int| 1 <= j <= i ==> final(ids)@[j] == old(ids)@[j - 1])
//@             && (forall|j: int| i < j < MAX ==> final(ids)@[j] == old(ids)@[j]),
//@     ret && !old(ids)@.take(*old(len) as int).contains(id) ==> *final(len) == *old(len) + 1
//@         && (forall|j: int| 1 <= j <= *old(len) ==> final(ids)@[j] == old(ids)@[j - 1])
//@         && (forall|j: int| *old(len) < j < MAX ==> final(ids)@[j] == old(ids)@[j]),
//@     !ret ==> *old(len) == MAX && !old(ids)@.take(*old(len) as int).contains(id)
//@         && final(ids)@ == old(ids)@ && *final(len) == *old(len),
{
    match index_of(ids, *len, id) {
        Some(i) => {
            move_to_front(ids, *len, i);
            //@ assert(old(ids)@.take(*old(len) as int)[i as int] == id);
            true
        }
        None => {
            if *len < MAX {
                push_front(ids, *len, id);
                *len += 1;
                true
            } else {
                false
            }
        }
    }
}

/// `id` leaves the order; returns the place it had, None when it was not listed.
pub fn remove(ids: &mut [AppId; MAX], len: &mut usize, id: AppId) -> Option<usize> //@ret
//@ requires *old(len) <= MAX, distinct(old(ids)@.take(*old(len) as int)),
//@ ensures
//@     *final(len) <= MAX,
//@     distinct(final(ids)@.take(*final(len) as int)),
//@     ret.is_some() == old(ids)@.take(*old(len) as int).contains(id),
//@     forall|v: AppId| final(ids)@.take(*final(len) as int).contains(v)
//@         <==> (old(ids)@.take(*old(len) as int).contains(v) && v != id),
//@     match ret {
//@         Some(i) => i < *old(len) && old(ids)@[i as int] == id && *final(len) == *old(len) - 1
//@             && (forall|j: int| 0 <= j < i ==> final(ids)@[j] == old(ids)@[j])
//@             && (forall|j: int| i <= j < *final(len) ==> final(ids)@[j] == old(ids)@[j + 1]),
//@         None => final(ids)@ == old(ids)@ && *final(len) == *old(len),
//@     },
{
    match index_of(ids, *len, id) {
        Some(i) => {
            remove_at(ids, *len, i);
            *len -= 1;
            //@ assert(old(ids)@.take(*old(len) as int)[i as int] == id);
            Some(i)
        }
        None => None,
    }
}
