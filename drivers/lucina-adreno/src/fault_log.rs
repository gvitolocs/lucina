// SPDX-License-Identifier: GPL-2.0-only
//! The GPU SMMU's fault records between its interrupt and the retire step (patch 1173). The handler (Linux's
//! `arm_smmu_context_fault` with `qcom_adreno_smmu_get_fault_info`) reads bank 0's `FSR`, `FAR`, `FSYNR0` and `TTBR0` at
//! the fault and acknowledges it; the record waits in a small ring under an interrupt-disabling lock until the retire step
//! puts it down to the program whose table `TTBR0` names. A full ring drops the record and counts it.
//!
//! These are the ring's indices and the blame rule, as pure functions that Verus checks as shipped
//! (`verification/juno-gpu-faults/`); the kernel keeps the records in an array of [`CAPACITY`] at the slots given here.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Records the ring holds. Faults come in bursts (one per bad access of a command buffer); the retire step counts them per
/// program, so what is dropped only loses the detail, never the count of drops.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CAPACITY: u32 = 16;

/// The ring's state: the oldest record's slot, how many there are, how many were dropped.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ring {
    pub head: u32,
    pub len: u32,
    pub dropped: u64,
}

#[cfg(verus_keep_ghost)]
verus! {

/// A ring the functions below may be given: its head inside the array, no more records than slots.
pub open spec fn well_formed(r: Ring) -> bool {
    r.head < CAPACITY && r.len <= CAPACITY
}

/// The slot of the record `i` places after the oldest.
pub open spec fn slot_of(r: Ring, i: int) -> int {
    (r.head as int + i) % (CAPACITY as int)
}

} // verus!

/// An empty ring.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures well_formed(r), r.len == 0, r.dropped == 0,
))]
pub const fn empty() -> Ring {
    Ring { head: 0, len: 0, dropped: 0 }
}

/// A new record: the slot to write it to, or `None` (dropped and counted) when the ring is full. Records already there keep
/// their slots and their order.
#[cfg_attr(verus_keep_ghost, verus_spec(out =>
    requires well_formed(r),
    ensures
        well_formed(out.0),
        r.len < CAPACITY ==> out.1 == Some(((r.head + r.len) % (CAPACITY as int)) as u32)
            && out.0.head == r.head && out.0.len == r.len + 1 && out.0.dropped == r.dropped,
        r.len == CAPACITY ==> out.1.is_none() && out.0.head == r.head && out.0.len == r.len
            && out.0.dropped == if r.dropped < u64::MAX { r.dropped + 1 } else { r.dropped as int },
        out.1.is_some() ==> out.1.unwrap() < CAPACITY
            && forall|i: int| 0 <= i < r.len ==> slot_of(r, i) != out.1.unwrap() as int,
))]
pub fn push(r: Ring) -> (Ring, Option<u32>) {
    if r.len < CAPACITY {
        let slot = (r.head + r.len) % CAPACITY;
        (Ring { head: r.head, len: r.len + 1, dropped: r.dropped }, Some(slot))
    } else {
        (Ring { head: r.head, len: r.len, dropped: r.dropped.saturating_add(1) }, None)
    }
}

/// The oldest record's slot, and the ring without it; `None` when empty.
#[cfg_attr(verus_keep_ghost, verus_spec(out =>
    requires well_formed(r),
    ensures
        well_formed(out.0),
        out.0.dropped == r.dropped,
        r.len == 0 ==> out.1.is_none() && out.0 == r,
        r.len > 0 ==> out.1 == Some(r.head) && out.0.len == r.len - 1
            && out.0.head == (r.head + 1) % (CAPACITY as int)
            && forall|i: int| 0 <= i < out.0.len ==> slot_of(out.0, i) == slot_of(r, i + 1),
))]
pub fn take(r: Ring) -> (Ring, Option<u32>) {
    if r.len == 0 {
        return (r, None);
    }
    (Ring { head: (r.head + 1) % CAPACITY, len: r.len - 1, dropped: r.dropped }, Some(r.head))
}

/// The table a fault happened under: `TTBR0` without its ASID (bits 48..63), as the kernel stores a table's root.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures r < 0x1_0000_0000_0000, r == ttbr0 % 0x1_0000_0000_0000,
))]
pub fn table_of(ttbr0: u64) -> u64 {
    ttbr0 % 0x1_0000_0000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_keeps_order_drops_when_full_and_counts() {
        let mut r = empty();
        let mut slots = [0u32; 20];
        for (i, s) in slots.iter_mut().enumerate().take(16) {
            let (next, slot) = push(r);
            r = next;
            *s = slot.unwrap();
            assert_eq!(*s, i as u32);
        }
        let (full, none) = push(r);
        assert_eq!((none, full.dropped, full.len), (None, 1, 16));
        let (r2, first) = take(full);
        assert_eq!(first, Some(0));
        let (r3, slot) = push(r2);
        // The freed slot is reused at the end.
        assert_eq!(slot, Some(0));
        let mut r = r3;
        let mut seen = std::vec::Vec::new();
        while let (next, Some(s)) = take(r) {
            seen.push(s);
            r = next;
        }
        assert_eq!(seen, (1..16).chain(0..1).collect::<std::vec::Vec<u32>>());
        assert_eq!(r.dropped, 1);
    }

    #[test]
    fn the_table_is_ttbr0_without_its_asid() {
        assert_eq!(table_of(0x0001_0000_ab73_6000), 0xab73_6000);
        assert_eq!(table_of(0xab73_6000), 0xab73_6000);
    }

    extern crate std;
}
