// SPDX-License-Identifier: MIT
//! Which instruction may read what, and when: Mesa's `ir3_delayslots` (`ir3_delay.c`) for an a6xx with full registers only.
//! Verus checks it as shipped (`verification/juno-ir3-delay/`); the legaliser and the emulator's hazard check rest on it.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// `compiler->delay_slots.alu_to_alu`, `non_alu` and `cat3_src2_read` for a6xx.
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const ALU_TO_ALU: u32 = 3;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const NON_ALU: u32 = 6;
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub const CAT3_SRC2_READ: u32 = 2;

/// An instruction by what it means for timing.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// cat1, cat2 and cat3.
    Alu,
    /// `mad.*`/`madsh.*`: an ALU whose third source is read later.
    Mad,
    /// cat4: the result comes back through `(ss)`.
    Sfu,
    /// cat5, and memory loads: the result comes back through `(sy)`.
    Tex,
    /// cat0 branches.
    Flow,
    /// cat6 stores and other memory users, as consumers.
    Mem,
    /// `end` and `chmask`: "shader outputs don't need any delay".
    End,
}

/// What must separate a producer and a reader: empty slots, or a sync flag on the reader.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Slots(u32),
    Ss,
    Sy,
}

/// `ir3_delayslots(assigner, consumer, n, false)`. `src_n` is the source index of the reader; `writes_addr`: the producer
/// writes `a0.x` or `a1.x`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        writes_addr ==> r == Need::Slots(NON_ALU),
        !writes_addr && producer == Kind::Sfu ==> r == Need::Ss,
        !writes_addr && producer == Kind::Tex ==> r == Need::Sy,
        !writes_addr && (producer == Kind::Alu || producer == Kind::Mad) && consumer == Kind::End ==> r == Need::Slots(0),
        !writes_addr && (producer == Kind::Alu || producer == Kind::Mad)
            && (consumer == Kind::Flow || consumer == Kind::Sfu || consumer == Kind::Tex || consumer == Kind::Mem)
            ==> r == Need::Slots(NON_ALU),
        !writes_addr && (producer == Kind::Alu || producer == Kind::Mad) && consumer == Kind::Mad && src_n == 2
            ==> r == Need::Slots((ALU_TO_ALU - CAT3_SRC2_READ) as u32),
        !writes_addr && (producer == Kind::Alu || producer == Kind::Mad)
            && (consumer == Kind::Alu || (consumer == Kind::Mad && src_n != 2))
            ==> r == Need::Slots(ALU_TO_ALU),
        r matches Need::Slots(n) ==> n <= NON_ALU,
))]
pub fn need(producer: Kind, consumer: Kind, src_n: u32, writes_addr: bool) -> Need {
    if writes_addr {
        return Need::Slots(NON_ALU);
    }
    match producer {
        Kind::Sfu => return Need::Ss,
        Kind::Tex => return Need::Sy,
        _ => {}
    }
    match consumer {
        Kind::End => Need::Slots(0),
        Kind::Flow | Kind::Sfu | Kind::Tex | Kind::Mem => Need::Slots(NON_ALU),
        Kind::Mad if src_n == 2 => Need::Slots(ALU_TO_ALU - CAT3_SRC2_READ),
        Kind::Alu | Kind::Mad => Need::Slots(ALU_TO_ALU),
    }
}

/// The empty slots still to insert before an instruction at `cycle` that reads a value `need` slots after `written`
/// (`written` is the producer's cycle: `need` idle cycles lie strictly between).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires written < cycle, need <= NON_ALU,
    ensures
        cycle - written > need ==> r == 0,
        cycle - written <= need ==> r == need + 1 - (cycle - written),
        (cycle + r) - written > need,
))]
pub fn slots_left(written: u64, cycle: u64, need: u32) -> u64 {
    let gap = cycle - written;
    if gap > need as u64 { 0 } else { need as u64 + 1 - gap }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesas_rules() {
        assert_eq!(need(Kind::Alu, Kind::Alu, 0, false), Need::Slots(3));
        assert_eq!(need(Kind::Alu, Kind::Mad, 2, false), Need::Slots(1));
        assert_eq!(need(Kind::Alu, Kind::Mad, 1, false), Need::Slots(3));
        assert_eq!(need(Kind::Alu, Kind::Sfu, 0, false), Need::Slots(6));
        assert_eq!(need(Kind::Alu, Kind::Flow, 0, false), Need::Slots(6));
        assert_eq!(need(Kind::Sfu, Kind::Alu, 0, false), Need::Ss);
        assert_eq!(need(Kind::Tex, Kind::Tex, 0, false), Need::Sy);
        assert_eq!(need(Kind::Alu, Kind::End, 0, false), Need::Slots(0));
        assert_eq!(need(Kind::Alu, Kind::Alu, 0, true), Need::Slots(6));
        assert_eq!(slots_left(10, 11, 3), 3);
        assert_eq!(slots_left(10, 14, 3), 0);
    }
}
