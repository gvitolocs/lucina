// SPDX-License-Identifier: GPL-2.0-only
// Derived from arch/arm64/kernel/smp_spin_table.c (SPDX GPL-2.0-only; Copyright (C) 2013 ARM Ltd.) of AsahiLinux/linux
// (branch asahi, rev 6ba0637d1ed9), and from the behaviour of m1n1 src/kboot.c and src/smp.c (rev cf37927d6bf4; m1n1 is
// GPL-2.0-or-later from v2.0.0, MIT before; no code copied). Re-expressed in Rust for LucinaOS; see the crate README.
// This is a Rust port of the behaviour of arch/arm64/kernel/smp_spin_table.c; the original is the source of truth.

//! Starting a CPU by spin-table, next to PSCI `CPU_ON`.
//!
//! The Mac's device tree has no PSCI. Its `/cpus` nodes say `enable-method = "spin-table"` and carry a `cpu-release-addr`
//! (Asahi `t8132.dtsi:64-65`, one pair per CPU up to :163-164). m1n1 fills the property in place for each CPU it started
//! (`src/kboot.c:618-620`, `fdt_setprop_inplace_u64`; nodes of CPUs that did not start are removed, :602-611).
//!
//! The protocol, from m1n1 (`src/smp.c`, rev cf37927d6bf4) and Linux's `arch/arm64/kernel/smp_spin_table.c` (rev 6ba0637d1ed9):
//!
//! - the property is the physical address of an 8-byte word. For m1n1 it is `&spin_table[cpu].target` (`smp.c:560-572`, the
//!   struct at :31-37, `target` at byte 16), which `smp_get_release_addr` also prepares by zeroing the four argument words
//!   (:567-570);
//! - a parked secondary spins on that word with `wfe` (`smp.c:118-121`, the loop `while (!(target = me->target)) wfe`), with
//!   the MMU and caches off (stage0.md and the m1n1 sheet, section 5: `mmu_init_secondary` is only reached from the proxy and
//!   hypervisor paths), so it reads memory, not the caches;
//! - when the word is non-zero it calls it as a function with `x0..x3` = the four argument words (all zero here,
//!   `smp.c:138-139`); if it returns, it stores the result and zeroes the word and parks again (:140-142). Our entry never
//!   returns;
//! - so the starter writes the entry address with one 64-bit little-endian store, cleans that cache line to the point of
//!   coherency (the reader bypasses the cache; Linux does `dcache_clean_inval_poc` over the word, `smp_spin_table.c:93-95`),
//!   orders it, and sends an event (`sev`, :100; m1n1's wait is `wfe`, `smp.c:121`).
//!
//! Linux's version of the protocol writes the address of a holding pen and releases the CPU in a second step
//! (`smp_spin_table.c:92`, :112-117); here the entry address itself goes into the word, as m1n1 calls it directly.

/// How a CPU is started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// PSCI `CPU_ON` (the default; QEMU virt, the Galaxy Book Go).
    Psci,
    /// Write the entry address to the 8-byte word at this physical address, then send an event.
    SpinTable { release_addr: u64 },
}

/// A CPU node that cannot be started the way it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MethodError {
    /// `enable-method = "spin-table"` with no `cpu-release-addr`, or a zero one (Linux, `smp_spin_table.c:57-59, 71-72`:
    /// missing or invalid is an error, a zero address is `-ENODEV`). m1n1 leaves `<0 0>` in a node it did not fill in.
    NoReleaseAddr,
}

/// The way to start the CPU of a `/cpus` node with this `enable-method` and `cpu-release-addr`.
///
/// Only the string `spin-table` selects the spin table. Everything else, `psci`, a missing property and an unknown string
/// alike, is PSCI, which is what the kernel did before it knew spin tables: a board that works today does not change.
pub fn method(
    enable_method: Option<&str>,
    release_addr: Option<u64>,
) -> Result<Method, MethodError> {
    match enable_method {
        Some("spin-table") => match release_addr {
            Some(addr) if addr != 0 => Ok(Method::SpinTable { release_addr: addr }),
            _ => Err(MethodError::NoReleaseAddr),
        },
        _ => Ok(Method::Psci),
    }
}

/// What the starting CPU does to memory and to the other CPU, one operation each.
pub trait ReleaseOps {
    /// One 64-bit little-endian store of `value` to the physical address `paddr` (a single access, 8-byte aligned).
    fn store_u64(&mut self, paddr: u64, value: u64);
    /// Cleans the data cache lines covering `[paddr, paddr + len)` to the point of coherency.
    fn clean_dcache_to_poc(&mut self, paddr: u64, len: u64);
    /// `dsb sy`.
    fn dsb_sy(&mut self);
    /// `sev`.
    fn sev(&mut self);
}

/// Why a release was not done. Nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseError {
    /// The release word is not 8-byte aligned, so one 64-bit store could not be single-copy atomic.
    Unaligned,
    /// The entry address is zero, which the CPU would take for "not released".
    ZeroEntry,
}

/// Releases a parked CPU at `entry` (a physical address; the CPU enters it with the MMU and caches off and `x0..x3` = 0).
///
/// The operations, in order: the store of `entry` at `release_addr`, the cache clean of that word, `dsb sy`, `sev`.
pub fn release<O: ReleaseOps>(
    ops: &mut O,
    release_addr: u64,
    entry: u64,
) -> Result<(), ReleaseError> {
    if release_addr & 7 != 0 {
        return Err(ReleaseError::Unaligned);
    }
    if entry == 0 {
        return Err(ReleaseError::ZeroEntry);
    }
    ops.store_u64(release_addr, entry);
    ops.clean_dcache_to_poc(release_addr, 8);
    ops.dsb_sy();
    ops.sev();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[derive(Debug, PartialEq, Eq)]
    enum Op {
        Store(u64, u64),
        Clean(u64, u64),
        Dsb,
        Sev,
    }

    #[derive(Default)]
    struct Rec(Vec<Op>);

    impl ReleaseOps for Rec {
        fn store_u64(&mut self, paddr: u64, value: u64) {
            self.0.push(Op::Store(paddr, value));
        }
        fn clean_dcache_to_poc(&mut self, paddr: u64, len: u64) {
            self.0.push(Op::Clean(paddr, len));
        }
        fn dsb_sy(&mut self) {
            self.0.push(Op::Dsb);
        }
        fn sev(&mut self) {
            self.0.push(Op::Sev);
        }
    }

    #[test]
    fn psci_is_the_default() {
        assert_eq!(method(Some("psci"), None), Ok(Method::Psci));
        // A release address on a PSCI node does not matter.
        assert_eq!(method(Some("psci"), Some(0x8_0000_0000)), Ok(Method::Psci));
        // No property: as before this change.
        assert_eq!(method(None, None), Ok(Method::Psci));
        assert_eq!(method(None, Some(0x1000)), Ok(Method::Psci));
        // An unknown string: as before this change.
        assert_eq!(method(Some("mystery"), Some(0x1000)), Ok(Method::Psci));
        assert_eq!(method(Some("Spin-Table"), Some(0x1000)), Ok(Method::Psci));
        assert_eq!(method(Some(""), None), Ok(Method::Psci));
    }

    #[test]
    fn spin_table_needs_a_release_address() {
        assert_eq!(
            method(Some("spin-table"), Some(0x8_0123_4560)),
            Ok(Method::SpinTable {
                release_addr: 0x8_0123_4560
            })
        );
        // The device tree's own placeholder (t8132.dtsi:65: `cpu-release-addr = <0 0>`) that m1n1 never filled in.
        assert_eq!(
            method(Some("spin-table"), Some(0)),
            Err(MethodError::NoReleaseAddr)
        );
        assert_eq!(
            method(Some("spin-table"), None),
            Err(MethodError::NoReleaseAddr)
        );
    }

    #[test]
    fn the_release_sequence() {
        let mut r = Rec::default();
        release(&mut r, 0x8_0123_4570, 0x8_0000_1000).unwrap();
        assert_eq!(
            r.0,
            [
                Op::Store(0x8_0123_4570, 0x8_0000_1000),
                Op::Clean(0x8_0123_4570, 8),
                Op::Dsb,
                Op::Sev
            ]
        );
    }

    #[test]
    fn a_refused_release_does_nothing() {
        let mut r = Rec::default();
        assert_eq!(
            release(&mut r, 0x8_0123_4574, 0x1000),
            Err(ReleaseError::Unaligned)
        );
        assert_eq!(
            release(&mut r, 0x8_0123_4570, 0),
            Err(ReleaseError::ZeroEntry)
        );
        assert_eq!(release(&mut r, 1, 0x1000), Err(ReleaseError::Unaligned));
        assert!(r.0.is_empty());
    }

    #[test]
    fn two_cpus_are_released_one_after_the_other() {
        // The kernel starts one CPU and waits for it before the next: the sequences do not interleave.
        let mut r = Rec::default();
        release(&mut r, 0x1000, 0x9000).unwrap();
        release(&mut r, 0x1010, 0x9000).unwrap();
        assert_eq!(r.0.len(), 8);
        assert_eq!(r.0[0], Op::Store(0x1000, 0x9000));
        assert_eq!(r.0[3], Op::Sev);
        assert_eq!(r.0[4], Op::Store(0x1010, 0x9000));
        assert_eq!(r.0[7], Op::Sev);
    }
}
