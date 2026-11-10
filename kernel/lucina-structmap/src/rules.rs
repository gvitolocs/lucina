//! The rules every read of a program's state follows, as plain arithmetic. Verus proves this very file
//! (`verification/juno-structmap/rules.rs`; the attributes are only seen by Verus, rustc ignores them), and
//! `verification/juno-structmap/mutants.sh` shows that planted bugs are refused. What each function promises is in its `ensures`.
//!
//! Not proved here: parsing `/proc/<pid>/maps`, the map files or ELF notes, or that the kernel returns what the program holds.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// The most bytes one field may read.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_FIELD: u64 = 4096;

/// The most pointers one field's path may follow.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_DEPTH: u64 = 8;

/// The size of a pointer in the programs we read (AArch64).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const PTR: u64 = 8;

/// Shortest and longest build id we accept. GNU build ids are 20 bytes (SHA-1) or 16 (MD5/UUID).
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MIN_ID: usize = 8;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const MAX_ID: usize = 64;

/// One mapping of the target, as `/proc/<pid>/maps` lists it: `[start, end)`.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub start: u64,
    pub end: u64,
    pub readable: bool,
}

/// Why one step of a pointer path stopped.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// More than [`MAX_DEPTH`] pointers.
    TooDeep,
    /// A null pointer.
    Null,
    /// An offset that runs past the end of the address space.
    Overflow,
    /// The bytes are not inside one readable region.
    Unmapped,
}

/// The answer of a map lookup.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lookup {
    Found(usize),
    NotFound,
    /// More than one map has this id: none is used.
    Duplicate,
    /// The program's id is too short or too long to trust.
    BadId,
}

#[cfg(verus_keep_ghost)]
verus! {

/// `[addr, addr + len)` lies inside region `r`, which is readable.
pub open spec fn inside(r: Region, addr: int, len: int) -> bool {
    &&& r.readable
    &&& r.start <= addr
    &&& addr + len <= r.end
}

/// A read the rules allow: a non-empty field no longer than [`MAX_FIELD`], wholly inside one readable region.
pub open spec fn allowed(regions: Seq<Region>, addr: int, len: int) -> bool {
    &&& 0 < len <= MAX_FIELD
    &&& exists|i: int| 0 <= i < regions.len() && inside(#[trigger] regions[i], addr, len)
}

/// `len` bytes at `addr` lie inside the map's region `[base, base + size)` and inside the program's copy of it, which holds the
/// region's first `copy_len` bytes; the field is not empty and no longer than [`MAX_FIELD`].
pub open spec fn in_copy(base: int, size: int, copy_len: int, addr: int, len: int) -> bool {
    &&& 0 < len <= MAX_FIELD
    &&& base <= addr
    &&& addr + len <= base + size
    &&& addr - base + len <= copy_len
}

} // verus!

/// Where `len` bytes at the program's address `addr` sit in its copy of one published region: the map says the region starts at
/// `base` and is `size` bytes long, and the copy holds `copy_len` bytes from `base` on (a Game Boy's 8 KiB of WRAM from 0xC000).
/// `Some(off)` means `copy[off..off + len]` is inside both the region and the copy; `None` when any byte would fall outside either,
/// or the field is empty or longer than [`MAX_FIELD`]. The one door every field read of a copy goes through.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(off) ==> off == addr - base && off + len <= copy_len && off + len <= size,
        r is Some <==> in_copy(base as int, size as int, copy_len as int, addr as int, len as int),
))]
pub fn copy_offset(base: u64, size: u64, copy_len: u64, addr: u64, len: u64) -> Option<u64> {
    if len == 0 || len > MAX_FIELD {
        return None;
    }
    if addr < base {
        return None;
    }
    let off = addr - base;
    if off > size || len > size - off {
        return None;
    }
    if off > copy_len || len > copy_len - off {
        return None;
    }
    Some(off)
}

/// Whether `len` bytes at `addr` may be read from a target whose regions are `regions`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> allowed(regions@, addr as int, len as int),
))]
pub fn read_allowed(regions: &[Region], addr: u64, len: u64) -> bool {
    if len == 0 || len > MAX_FIELD {
        return false;
    }
    if addr > u64::MAX - len {
        // No region ends past u64::MAX, so nothing can hold these bytes.
        #[cfg(verus_keep_ghost)]
        proof! {
            assert forall|i: int| 0 <= i < regions@.len() implies !inside(#[trigger] regions@[i], addr as int, len as int) by {}
        }
        return false;
    }
    let end = addr + len;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= regions.len(),
            end == addr + len,
            0 < len <= MAX_FIELD,
            forall|j: int| 0 <= j < i ==> !inside(#[trigger] regions@[j], addr as int, len as int),
        decreases regions.len() - i,
    ))]
    while i < regions.len() {
        let r = regions[i];
        if r.readable && r.start <= addr && end <= r.end {
            #[cfg(verus_keep_ghost)]
            proof! { assert(inside(regions@[i as int], addr as int, len as int)); }
            return true;
        }
        i += 1;
    }
    false
}

/// `base + off`, or `None` past the end of the address space.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Some(a) ==> a == base + off,
        r is None ==> base + off > u64::MAX,
))]
pub fn add_offset(base: u64, off: u64) -> Option<u64> {
    if base > u64::MAX - off {
        None
    } else {
        Some(base + off)
    }
}

/// One pointer follow of a path: may the pointer at `at` be read, as follow number `depth` (counting from 0)?
/// `Ok` means the 8 bytes at `at` may be read; the caller reads them and goes on with `depth + 1`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r is Ok ==> depth < MAX_DEPTH && at != 0 && allowed(regions@, at as int, PTR as int),
        depth >= MAX_DEPTH ==> r == Err::<(), Stop>(Stop::TooDeep),
))]
pub fn follow_allowed(regions: &[Region], at: u64, depth: u64) -> Result<(), Stop> {
    if depth >= MAX_DEPTH {
        return Err(Stop::TooDeep);
    }
    if at == 0 {
        return Err(Stop::Null);
    }
    if !read_allowed(regions, at, PTR) {
        return Err(Stop::Unmapped);
    }
    Ok(())
}

/// Which of the `wanted` fields are read: the ones that exist and are not excluded, in the order asked.
/// `excluded[i]` is field `i`'s mark in its map.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        forall|k: int| 0 <= k < r@.len() ==> (#[trigger] r@[k]) < excluded@.len() && !excluded@[r@[k] as int],
        forall|k: int| 0 <= k < r@.len() ==> wanted@.contains(#[trigger] r@[k]),
))]
pub fn read_plan(excluded: &[bool], wanted: &[usize]) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= wanted.len(),
            forall|k: int| 0 <= k < out@.len() ==> (#[trigger] out@[k]) < excluded@.len() && !excluded@[out@[k] as int],
            forall|k: int| 0 <= k < out@.len() ==> exists|j: int| 0 <= j < i && wanted@[j] == #[trigger] out@[k],
        decreases wanted.len() - i,
    ))]
    while i < wanted.len() {
        let f = wanted[i];
        if f < excluded.len() && !excluded[f] {
            out.push(f);
        }
        i += 1;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert forall|k: int| 0 <= k < out@.len() implies wanted@.contains(#[trigger] out@[k]) by {
            let j = choose|j: int| 0 <= j < wanted@.len() && wanted@[j] == out@[k];
        }
    }
    out
}

/// Two build ids are the same id: same length, same bytes.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r <==> a@ == b@,
))]
pub fn same_id(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            a.len() == b.len(),
            i <= a.len(),
            forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    ))]
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        assert(a@ =~= b@);
    }
    true
}

/// The one map whose build id is the program's, `want`. A second map with the same id makes the lookup refuse.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        r matches Lookup::Found(i) ==> i < ids@.len() && ids@[i as int]@ == want@ && MIN_ID <= want@.len() <= MAX_ID
            && forall|j: int| 0 <= j < ids@.len() && j != i ==> (#[trigger] ids@[j])@ != want@,
        (want@.len() < MIN_ID || want@.len() > MAX_ID) ==> r == Lookup::BadId,
))]
pub fn find_map(ids: &[Vec<u8>], want: &[u8]) -> Lookup {
    if want.len() < MIN_ID || want.len() > MAX_ID {
        return Lookup::BadId;
    }
    let mut found: Option<usize> = None;
    let mut i: usize = 0;
    #[cfg_attr(verus_keep_ghost, verus_spec(
        invariant
            i <= ids.len(),
            MIN_ID <= want.len() <= MAX_ID,
            found matches Some(f) ==> f < i && ids@[f as int]@ == want@,
            forall|j: int| 0 <= j < i && (found matches Some(f) ==> j != f) ==> (#[trigger] ids@[j])@ != want@,
        decreases ids.len() - i,
    ))]
    while i < ids.len() {
        if same_id(ids[i].as_slice(), want) {
            if found.is_some() {
                return Lookup::Duplicate;
            }
            found = Some(i);
        }
        i += 1;
    }
    match found {
        Some(f) => Lookup::Found(f),
        None => Lookup::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: [Region; 2] = [
        Region {
            start: 0x1000,
            end: 0x2000,
            readable: true,
        },
        Region {
            start: 0x2000,
            end: 0x3000,
            readable: false,
        },
    ];

    #[test]
    fn bounds() {
        assert!(read_allowed(&R, 0x1000, 8));
        assert!(read_allowed(&R, 0x1ff8, 8));
        assert!(
            !read_allowed(&R, 0x1ff9, 8),
            "runs into the unreadable region"
        );
        assert!(!read_allowed(&R, 0x2000, 1), "unreadable");
        assert!(!read_allowed(&R, 0x1000, 0));
        assert!(!read_allowed(&R, 0x1000, MAX_FIELD + 1));
        assert!(!read_allowed(&R, u64::MAX - 3, 8));
    }

    #[test]
    fn chains() {
        assert_eq!(follow_allowed(&R, 0x1000, 0), Ok(()));
        assert_eq!(follow_allowed(&R, 0x1000, MAX_DEPTH), Err(Stop::TooDeep));
        assert_eq!(follow_allowed(&R, 0, 0), Err(Stop::Null));
        assert_eq!(follow_allowed(&R, 0x2008, 0), Err(Stop::Unmapped));
        assert_eq!(add_offset(u64::MAX, 1), None);
        assert_eq!(add_offset(5, 7), Some(12));
    }

    #[test]
    fn copies() {
        // WRAM at 0xC000, 8 KiB, all of it copied.
        assert_eq!(copy_offset(0xc000, 0x2000, 0x2000, 0xd35e, 1), Some(0x135e));
        assert_eq!(copy_offset(0xc000, 0x2000, 0x2000, 0xdfff, 1), Some(0x1fff));
        assert_eq!(
            copy_offset(0xc000, 0x2000, 0x2000, 0xdfff, 2),
            None,
            "runs past the region"
        );
        assert_eq!(
            copy_offset(0xc000, 0x2000, 0x2000, 0xbfff, 1),
            None,
            "before the region"
        );
        assert_eq!(
            copy_offset(0xc000, 0x2000, 0x1000, 0xd000, 1),
            None,
            "past the copy"
        );
        assert_eq!(copy_offset(0xc000, 0x2000, 0x2000, 0xc000, 0), None);
        assert_eq!(
            copy_offset(0xc000, 0x2000, 0x2000, 0xc000, MAX_FIELD + 1),
            None
        );
        assert_eq!(
            copy_offset(0, u64::MAX, u64::MAX, u64::MAX - 1, 8),
            None,
            "no overflow"
        );
    }

    #[test]
    fn plan_drops_excluded_and_unknown() {
        assert_eq!(read_plan(&[false, true, false], &[0, 1, 2, 9]), vec![0, 2]);
    }

    #[test]
    fn lookup() {
        let a = vec![1u8; 20];
        let b = vec![2u8; 20];
        assert_eq!(find_map(&[a.clone(), b.clone()], &b), Lookup::Found(1));
        assert_eq!(find_map(&[a.clone(), a.clone()], &a), Lookup::Duplicate);
        assert_eq!(
            find_map(&[a[..19].to_vec()], &a),
            Lookup::NotFound,
            "a prefix is not the id"
        );
        assert_eq!(find_map(&[vec![]], &[]), Lookup::BadId);
    }
}
