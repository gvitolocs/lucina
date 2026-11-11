// The moves of the most-recently-used order: an app already listed goes to the front, a new app is put at the front, an ended app
// leaves. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011);
// verification/juno-wm-rules/mutants.sh plants bugs that Verus must refuse. switcher.rs calls these functions and holds no copy of
// the moves. They work on any `Copy` element (switcher.rs keeps `AppId`s), so they never compare ids: the search for an id
// (`Mru::index_of`) stays in switcher.rs and is NOT under Verus. Proved for each move: the exact new order (every place is
// named), which ids the first `len` places hold afterwards (the same ones, or the same plus / minus the one moved), and that an
// order without duplicates stays without duplicates. Not stated as a multiset: with "no duplicates before => none after" and the
// set of ids, the ids are the same exactly once each.

/// How many ids the order holds at most (`switcher::MAX_APPS`).
pub const MAX: usize = 16;

//@ pub open spec fn distinct<T>(s: Seq<T>) -> bool {
//@     forall|i: int, j: int| 0 <= i < j < s.len() ==> s[i] != s[j]
//@ }

// Where place `m` of the order after `move_to_front(.., idx)` took its id from.
//@ spec fn moved_from(m: int, idx: int) -> int {
//@     if m == 0 { idx } else if m <= idx { m - 1 } else { m }
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
    //@ assert(forall|m: int| 0 <= m < len ==> #[trigger] ids@[m] == old(ids)@[moved_from(m, idx as int)]);
    //@ assert(forall|v: T| old(ids)@.take(len as int).contains(v) ==> ids@.take(len as int).contains(v)) by {
    //@     assert forall|v: T| old(ids)@.take(len as int).contains(v) implies ids@.take(len as int).contains(v) by {
    //@         let k = choose|k: int| 0 <= k < len && old(ids)@[k] == v;
    //@         let m: int = if k == idx as int { 0 } else if k < idx as int { k + 1 } else { k };
    //@         assert(moved_from(m, idx as int) == k);
    //@         assert(ids@.take(len as int)[m] == v);
    //@     }
    //@ }
    //@ assert(forall|v: T| ids@.take(len as int).contains(v) ==> old(ids)@.take(len as int).contains(v)) by {
    //@     assert forall|v: T| ids@.take(len as int).contains(v) implies old(ids)@.take(len as int).contains(v) by {
    //@         let m = choose|m: int| 0 <= m < len && ids@[m] == v;
    //@         assert(old(ids)@.take(len as int)[moved_from(m, idx as int)] == v);
    //@     }
    //@ }
    //@ assert(distinct(old(ids)@.take(len as int)) ==> distinct(ids@.take(len as int))) by {
    //@     if distinct(old(ids)@.take(len as int)) {
    //@         assert forall|a: int, b: int| 0 <= a < b < len implies #[trigger] ids@.take(len as int)[a] != #[trigger] ids@.take(len as int)[b] by {
    //@             let ka = moved_from(a, idx as int);
    //@             let kb = moved_from(b, idx as int);
    //@             assert(ka != kb);
    //@             assert(ids@.take(len as int)[a] == old(ids)@.take(len as int)[ka]);
    //@             assert(ids@.take(len as int)[b] == old(ids)@.take(len as int)[kb]);
    //@             if ka < kb {
    //@                 assert(old(ids)@.take(len as int)[ka] != old(ids)@.take(len as int)[kb]);
    //@             } else {
    //@                 assert(old(ids)@.take(len as int)[kb] != old(ids)@.take(len as int)[ka]);
    //@             }
    //@         }
    //@     }
    //@ }
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
    //@ assert(forall|v: T| old(ids)@.take(len as int).contains(v) ==> ids@.take(len as int + 1).contains(v)) by {
    //@     assert forall|v: T| old(ids)@.take(len as int).contains(v) implies ids@.take(len as int + 1).contains(v) by {
    //@         let k = choose|k: int| 0 <= k < len && old(ids)@[k] == v;
    //@         assert(ids@.take(len as int + 1)[k + 1] == v);
    //@     }
    //@ }
    //@ assert(forall|v: T| ids@.take(len as int + 1).contains(v) ==> (old(ids)@.take(len as int).contains(v) || v == id)) by {
    //@     assert forall|v: T| ids@.take(len as int + 1).contains(v) implies (old(ids)@.take(len as int).contains(v) || v == id) by {
    //@         let m = choose|m: int| 0 <= m < len + 1 && ids@[m] == v;
    //@         if m > 0 {
    //@             assert(old(ids)@.take(len as int)[m - 1] == v);
    //@         }
    //@     }
    //@ }
    //@ assert(ids@.take(len as int + 1)[0] == id);
    //@ assert(distinct(old(ids)@.take(len as int)) && !old(ids)@.take(len as int).contains(id) ==> distinct(ids@.take(len as int + 1))) by {
    //@     if distinct(old(ids)@.take(len as int)) && !old(ids)@.take(len as int).contains(id) {
    //@         assert forall|a: int, b: int| 0 <= a < b < len + 1 implies #[trigger] ids@.take(len as int + 1)[a] != #[trigger] ids@.take(len as int + 1)[b] by {
    //@             if a == 0 {
    //@                 assert(old(ids)@.take(len as int)[b - 1] == ids@[b]);
    //@             } else {
    //@                 assert(old(ids)@.take(len as int)[a - 1] == ids@[a]);
    //@                 assert(old(ids)@.take(len as int)[b - 1] == ids@[b]);
    //@             }
    //@         }
    //@     }
    //@ }
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
    //@ assert(forall|v: T| ids@.take(len as int - 1).contains(v) ==> old(ids)@.take(len as int).contains(v)) by {
    //@     assert forall|v: T| ids@.take(len as int - 1).contains(v) implies old(ids)@.take(len as int).contains(v) by {
    //@         let m = choose|m: int| 0 <= m < len - 1 && ids@[m] == v;
    //@         let k: int = if m < idx as int { m } else { m + 1 };
    //@         assert(old(ids)@.take(len as int)[k] == v);
    //@     }
    //@ }
    //@ assert(forall|v: T| old(ids)@.take(len as int).contains(v) && v != old(ids)@[idx as int] ==> ids@.take(len as int - 1).contains(v)) by {
    //@     assert forall|v: T| old(ids)@.take(len as int).contains(v) && v != old(ids)@[idx as int]
    //@         implies ids@.take(len as int - 1).contains(v) by {
    //@         let k = choose|k: int| 0 <= k < len && old(ids)@[k] == v;
    //@         let m: int = if k < idx as int { k } else { k - 1 };
    //@         assert(ids@.take(len as int - 1)[m] == v);
    //@     }
    //@ }
    //@ assert(distinct(old(ids)@.take(len as int)) ==>
    //@     distinct(ids@.take(len as int - 1)) && !ids@.take(len as int - 1).contains(old(ids)@[idx as int])) by {
    //@     if distinct(old(ids)@.take(len as int)) {
    //@         assert forall|a: int, b: int| 0 <= a < b < len - 1 implies #[trigger] ids@.take(len as int - 1)[a] != #[trigger] ids@.take(len as int - 1)[b] by {
    //@             let ka: int = if a < idx as int { a } else { a + 1 };
    //@             let kb: int = if b < idx as int { b } else { b + 1 };
    //@             assert(old(ids)@.take(len as int)[ka] == ids@[a]);
    //@             assert(old(ids)@.take(len as int)[kb] == ids@[b]);
    //@         }
    //@         assert forall|m: int| 0 <= m < len - 1 implies #[trigger] ids@.take(len as int - 1)[m] != old(ids)@[idx as int] by {
    //@             let k: int = if m < idx as int { m } else { m + 1 };
    //@             assert(old(ids)@.take(len as int)[k] == ids@[m]);
    //@             if k < idx as int {
    //@                 assert(old(ids)@.take(len as int)[k] != old(ids)@.take(len as int)[idx as int]);
    //@             } else {
    //@                 assert(old(ids)@.take(len as int)[idx as int] != old(ids)@.take(len as int)[k]);
    //@             }
    //@         }
    //@     }
    //@ }
}
