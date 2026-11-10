// The integer rules under Fortuna's game search: where a query matches a name, how a name is ranked, why typing one
// more letter may only narrow the previous results, and where a game's banner may lie in its file. Verus checks this
// very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011). `index.rs` calls these functions for
// every match it reports and keeps no copy of the rules.
//
// Names and queries reach these functions already folded (`fold`, ASCII only), so matching is plain byte equality.
//
// Not proved: the UTF-16 title decoding and the CRC of a banner (banner.rs, unit tests), the word-start ranking and
// the bucket order of results (index.rs, unit tests).

/// Rank of a name whose first bytes are the query.
pub const RANK_START: u8 = 0;
/// Rank of a name where the query starts a later word (index.rs decides the word part).
pub const RANK_WORD: u8 = 1;
/// Rank of a name that holds the query somewhere else.
pub const RANK_INSIDE: u8 = 2;
/// Rank of a name that does not hold the query.
pub const RANK_NONE: u8 = 3;

/// The longest name the rules accept: far beyond a FAT long name (255), small enough that no index overflows.
pub const MAX_HAY: usize = 0xffff_ffff;

/// Header bytes before the first place a banner may start (the NDS header is 0x200 bytes; 0 means "no banner").
pub const HEADER_END: u32 = 0x200;
/// Bytes of a version-1 banner, the part every version shares: icon, palette and the six titles.
pub const BANNER_V1_LEN: u64 = 0x840;

//@ pub open spec fn spec_fold(b: u8) -> u8 { if 65 <= b && b <= 90 { (b + 32) as u8 } else { b } }

//@ pub open spec fn spec_eq_at(h: Seq<u8>, n: Seq<u8>, at: int) -> bool {
//@     0 <= at && at + n.len() <= h.len() && forall|k: int| 0 <= k < n.len() ==> #[trigger] h[at + k] == n[k]
//@ }

//@ pub open spec fn spec_contains(h: Seq<u8>, n: Seq<u8>) -> bool { exists|at: int| spec_eq_at(h, n, at) }

/// ASCII upper case to lower case; every other byte unchanged (UTF-8 bytes of other letters stay as they are).
pub fn fold(b: u8) -> u8 //@ret
//@ ensures ret == spec_fold(b),
{
    if b >= 65 && b <= 90 {
        b + 32
    } else {
        b
    }
}

/// Whether `n` lies in `h` starting at byte `at`.
pub fn eq_at(h: &[u8], n: &[u8], at: usize) -> bool //@ret
//@ ensures ret == spec_eq_at(h@, n@, at as int),
{
    if at > h.len() || n.len() > h.len() - at {
        return false;
    }
    let mut k: usize = 0;
    while k < n.len()
    //@ invariant at + n.len() <= h.len(), k <= n.len(), forall|j: int| 0 <= j < k ==> #[trigger] h@[at + j] == n@[j],
    //@ decreases n.len() - k,
    {
        if h[at + k] != n[k] {
            //@ assert(h@[at + k as int] != n@[k as int]);
            return false;
        }
        k += 1;
    }
    true
}

/// The first place at or after `from` where `n` lies in `h`, or `None` when there is none.
pub fn find(h: &[u8], n: &[u8], from: usize) -> Option<usize> //@ret
//@ requires h.len() <= MAX_HAY,
//@ ensures match ret {
//@     Some(i) => from <= i && spec_eq_at(h@, n@, i as int) && forall|j: int| from <= j < i ==> !spec_eq_at(h@, n@, j),
//@     None => forall|j: int| from <= j ==> !spec_eq_at(h@, n@, j),
//@ },
{
    if n.len() > h.len() {
        return None;
    }
    let last = h.len() - n.len();
    if from > last {
        return None;
    }
    let mut i = from;
    while i <= last
    //@ invariant last == h.len() - n.len(), n.len() <= h.len(), h.len() <= MAX_HAY, from <= i, i <= last + 1,
    //@     forall|j: int| from <= j < i ==> !spec_eq_at(h@, n@, j),
    //@ decreases last + 1 - i,
    {
        if eq_at(h, n, i) {
            return Some(i);
        }
        i += 1;
    }
    //@ assert(forall|j: int| j > last ==> !spec_eq_at(h@, n@, j));
    None
}

/// How well `n` matches `h`: [`RANK_START`] when `h` starts with it, [`RANK_INSIDE`] when it lies anywhere else,
/// [`RANK_NONE`] when it does not lie in `h` at all. The word rank is index.rs's refinement of RANK_INSIDE.
pub fn rank(h: &[u8], n: &[u8]) -> u8 //@ret
//@ requires h.len() <= MAX_HAY,
//@ ensures ret == RANK_START || ret == RANK_INSIDE || ret == RANK_NONE,
//@     (ret == RANK_START) == spec_eq_at(h@, n@, 0),
//@     (ret == RANK_NONE) == !spec_contains(h@, n@),
{
    if eq_at(h, n, 0) {
        //@ assert(spec_contains(h@, n@));
        return RANK_START;
    }
    match find(h, n, 0) {
        Some(_i) => {
            //@ assert(spec_eq_at(h@, n@, _i as int));
            //@ assert(spec_contains(h@, n@));
            RANK_INSIDE
        }
        None => {
            //@ assert(!spec_contains(h@, n@));
            RANK_NONE
        }
    }
}

// Typing one more letter can only narrow the results: every name that holds the longer query `m` also holds every
// prefix `n` of it. index.rs relies on this to filter the previous results instead of scanning the library again.
//@ pub proof fn lemma_narrow(h: Seq<u8>, n: Seq<u8>, m: Seq<u8>)
//@     requires n.len() <= m.len(), m.subrange(0, n.len() as int) == n, spec_contains(h, m),
//@     ensures spec_contains(h, n),
//@ {
//@     let at = choose|at: int| spec_eq_at(h, m, at);
//@     assert forall|k: int| 0 <= k < n.len() implies #[trigger] h[at + k] == n[k] by {
//@         assert(m.subrange(0, n.len() as int)[k] == m[k]);
//@         assert(h[at + k] == m[k]);
//@     }
//@     assert(spec_eq_at(h, n, at));
//@ }

// The search's queries have several words: index.rs folds what was typed into words joined by a 0 byte (no name
// holds one), and a game matches when every word is in its key. `spec_word(q, i, k)`: q[i..k] is one whole word
// (non-empty, no 0 inside, a 0 or an end on both sides).
//@ pub open spec fn spec_word(q: Seq<u8>, i: int, k: int) -> bool {
//@     0 <= i < k <= q.len() && (i == 0 || q[i - 1] == 0) && (k == q.len() || q[k] == 0)
//@     && forall|j: int| i <= j < k ==> #[trigger] q[j] != 0
//@ }

//@ pub open spec fn spec_matches_all(h: Seq<u8>, q: Seq<u8>) -> bool {
//@     forall|i: int, k: int| #[trigger] spec_word(q, i, k) ==> spec_contains(h, q.subrange(i, k))
//@ }

// A run of non-0 bytes from i to k in q2 continues to the end of its word somewhere at or after k.
//@ pub proof fn lemma_word_ends(q2: Seq<u8>, i: int, k: int)
//@     requires 0 <= i < k <= q2.len(), (i == 0 || q2[i - 1] == 0), forall|j: int| i <= j < k ==> #[trigger] q2[j] != 0,
//@     ensures exists|k2: int| k <= k2 && #[trigger] spec_word(q2, i, k2),
//@     decreases q2.len() - k,
//@ {
//@     if k == q2.len() || q2[k] == 0 {
//@         assert(spec_word(q2, i, k));
//@     } else {
//@         assert forall|j: int| i <= j < k + 1 implies #[trigger] q2[j] != 0 by {
//@             if j < k { assert(q2[j] != 0); } else { assert(j == k); }
//@         }
//@         lemma_word_ends(q2, i, k + 1);
//@     }
//@ }

// Typing more (letters, a space and a new word) only narrows a multi-word search: a game that matches the longer
// query q2 matches every query q it starts with. This is the condition index.rs's Search checks before it filters
// the previous matches instead of scanning again (the folded query only grew at its end).
//@ pub proof fn lemma_narrow_words(h: Seq<u8>, q: Seq<u8>, q2: Seq<u8>)
//@     requires q.len() <= q2.len(), q2.subrange(0, q.len() as int) == q, spec_matches_all(h, q2),
//@     ensures spec_matches_all(h, q),
//@ {
//@     assert forall|i: int, k: int| #[trigger] spec_word(q, i, k) implies spec_contains(h, q.subrange(i, k)) by {
//@         assert forall|j: int| i <= j < k implies #[trigger] q2[j] != 0 by {
//@             assert(q2.subrange(0, q.len() as int)[j] == q2[j]);
//@             assert(q[j] != 0);
//@         }
//@         if i > 0 {
//@             assert(q2.subrange(0, q.len() as int)[i - 1] == q2[i - 1]);
//@         }
//@         lemma_word_ends(q2, i, k);
//@         let k2 = choose|k2: int| k <= k2 && #[trigger] spec_word(q2, i, k2);
//@         assert(spec_contains(h, q2.subrange(i, k2)));
//@         assert(q2.subrange(i, k2).subrange(0, k - i) == q.subrange(i, k)) by {
//@             assert forall|j: int| 0 <= j < k - i implies #[trigger] q2.subrange(i, k2).subrange(0, k - i)[j] == q.subrange(i, k)[j] by {
//@                 assert(q2.subrange(0, q.len() as int)[i + j] == q2[i + j]);
//@             }
//@         }
//@         lemma_narrow(h, q.subrange(i, k), q2.subrange(i, k2));
//@     }
//@ }

/// Where the shared part of a game's banner lies in its file, from the header's banner offset: `(start, end)` with
/// `end - start` = [`BANNER_V1_LEN`], or `None` when the offset is 0, points into the header, or runs past the file.
pub fn banner_span(file_len: u64, off: u32) -> Option<(u64, u64)> //@ret
//@ ensures match ret {
//@     Some((s, e)) => s == off as u64 && e == s + BANNER_V1_LEN && e <= file_len && off >= HEADER_END,
//@     None => off < HEADER_END || (off as u64) + BANNER_V1_LEN > file_len,
//@ },
{
    if off < HEADER_END {
        return None;
    }
    let s = off as u64;
    if s + BANNER_V1_LEN > file_len {
        return None;
    }
    Some((s, s + BANNER_V1_LEN))
}
