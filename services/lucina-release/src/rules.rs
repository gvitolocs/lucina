//! What the net installer may do with a manifest's chunks, as plain arithmetic and booleans. Verus proves this very file, not a
//! copy of it (`verification/net-installer/rules.rs`; the attributes are only seen by Verus, rustc ignores them). What each
//! function promises is in its `ensures`.
//!
//! The two invariants of `docs/design/lucina-net-installer.md` section 5: no unverified chunk is ever written, and nothing is made
//! bootable before every chunk is written and the whole-image hash matches.
//!
//! Not proved here: the hashes and the signature themselves, the HTTP fetch, or that the device does what it is told.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// How many chunks an image of `size` bytes has: the size divided by the chunk size, rounded up. `None` for an empty image or a
/// chunk size of 0.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(n) ==> n >= 1 && size > 0 && chunk > 0 && ((n as int - 1) * (chunk as int)) < (size as int)
            && (size as int) <= (n as int) * (chunk as int),
        r is None ==> size == 0 || chunk == 0,
))]
pub fn chunk_count(size: u64, chunk: u64) -> Option<u64> {
    if size == 0 || chunk == 0 {
        return None;
    }
    let n = (size - 1) / chunk + 1;
    #[cfg(verus_keep_ghost)]
    proof! {
        let q = n - 1;
        let s1 = size as int - 1;
        let c = chunk as int;
        assert(q as int == s1 / c);
        assert(q as int * c <= s1) by (nonlinear_arith)
            requires q as int == s1 / c, c > 0;
        assert(s1 < (q as int + 1) * c) by (nonlinear_arith)
            requires q as int == s1 / c, c > 0;
    }
    Some(n)
}

/// Where chunk `index` lies in the image: its first byte and its length. Every chunk is `chunk` bytes except the last, which ends
/// exactly at `size`. `None` past the end.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some((start, len)) ==> (start as int) + (len as int) <= (size as int) && len > 0 && len <= chunk
            && (start as int) == (index as int) * (chunk as int)
            && (len < chunk ==> (start as int) + (len as int) == (size as int))
            && ((start as int) + (len as int) == (size as int) || len == chunk),
))]
// A plain `match`, not `?`: Verus reads this file as it is.
#[allow(clippy::question_mark)]
pub fn chunk_range(index: u64, size: u64, chunk: u64) -> Option<(u64, u64)> {
    let n = match chunk_count(size, chunk) {
        Some(n) => n,
        None => return None,
    };
    if index >= n {
        return None;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(index as int * chunk as int <= (n as int - 1) * chunk as int) by (nonlinear_arith)
            requires index as int <= n as int - 1, chunk > 0;
    }
    let start = index * chunk;
    let left = size - start;
    let len = if left < chunk { left } else { chunk };
    Some((start, len))
}

/// The chunk to fetch next: the first one not yet verified. The chunks are written in order, so what is done is a prefix.
/// `None` when every chunk is.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(i) ==> i < count && i == verified_prefix,
        r is None ==> verified_prefix >= count,
))]
pub fn next_to_fetch(verified_prefix: u64, count: u64) -> Option<u64> {
    if verified_prefix < count {
        Some(verified_prefix)
    } else {
        None
    }
}

/// Invariant 1: a chunk is written only when its hash matched the signed manifest and it is the next one in order.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> chunk_hash_ok && index == verified_prefix,
))]
pub fn may_write_chunk(index: u64, verified_prefix: u64, chunk_hash_ok: bool) -> bool {
    chunk_hash_ok && index == verified_prefix
}

/// Invariant 2: the slot's header, the kernel set and `BOOT.CFG` are written only when every chunk is written and verified and
/// the whole-image hash matches.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> verified_prefix == count && whole_hash_ok,
))]
pub fn may_make_bootable(verified_prefix: u64, count: u64, whole_hash_ok: bool) -> bool {
    verified_prefix == count && whole_hash_ok
}

/// Whether a manifest's sequence number may be installed: not lower than what the machine has, unless the typed yes says to go
/// back. Nothing installed yet allows any.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> typed_go_back || (installed matches Some(i) ==> new >= i),
        (installed matches Some(i) && new >= i) ==> r,
        installed is None ==> r,
        typed_go_back ==> r,
))]
pub fn seq_allowed(installed: Option<u64>, new: u64, typed_go_back: bool) -> bool {
    if typed_go_back {
        return true;
    }
    match installed {
        Some(i) => new >= i,
        None => true,
    }
}

/// Whether a target may be written: the rule compiled into the installer for this board allows it, and the manifest names it. A
/// manifest only narrows the rule, never widens it.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r ==> target_in_compiled_rule && named_in_manifest,
        r <==> (target_in_compiled_rule && named_in_manifest),
))]
pub fn target_allowed(target_in_compiled_rule: bool, named_in_manifest: bool) -> bool {
    target_in_compiled_rule && named_in_manifest
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: u64 = 4 << 20;

    #[test]
    fn counts() {
        assert_eq!(chunk_count(0, C), None);
        assert_eq!(chunk_count(5, 0), None);
        assert_eq!(chunk_count(1, C), Some(1));
        assert_eq!(chunk_count(C - 1, C), Some(1));
        assert_eq!(chunk_count(C, C), Some(1));
        assert_eq!(chunk_count(C + 1, C), Some(2));
        assert_eq!(chunk_count(u64::MAX, 1), Some(u64::MAX));
    }

    #[test]
    fn ranges_cover_the_image_exactly() {
        for size in [1, C - 1, C, C + 1, 3 * C + 7] {
            let n = chunk_count(size, C).unwrap();
            let mut at = 0;
            for i in 0..n {
                let (s, l) = chunk_range(i, size, C).unwrap();
                assert_eq!(s, at, "gap or overlap at chunk {i} of {size}");
                assert!(l > 0 && l <= C);
                at += l;
            }
            assert_eq!(at, size);
            assert_eq!(chunk_range(n, size, C), None);
        }
        assert_eq!(chunk_range(0, 0, C), None);
    }

    #[test]
    fn fetch_and_write_order() {
        assert_eq!(next_to_fetch(0, 3), Some(0));
        assert_eq!(next_to_fetch(2, 3), Some(2));
        assert_eq!(next_to_fetch(3, 3), None);
        for idx in 0..4 {
            for pre in 0..4 {
                for ok in [false, true] {
                    assert_eq!(may_write_chunk(idx, pre, ok), ok && idx == pre);
                }
            }
        }
        assert!(may_make_bootable(3, 3, true));
        assert!(!may_make_bootable(3, 3, false));
        assert!(!may_make_bootable(2, 3, true));
        assert!(!may_make_bootable(4, 3, true));
    }

    #[test]
    fn seq_truth_table() {
        assert!(seq_allowed(None, 0, false));
        assert!(seq_allowed(Some(5), 5, false));
        assert!(seq_allowed(Some(5), 6, false));
        assert!(!seq_allowed(Some(5), 4, false));
        assert!(seq_allowed(Some(5), 4, true));
        assert!(seq_allowed(None, 1, true));
    }

    #[test]
    fn target_truth_table() {
        assert!(target_allowed(true, true));
        assert!(!target_allowed(true, false));
        assert!(!target_allowed(false, true));
        assert!(!target_allowed(false, false));
    }
}
