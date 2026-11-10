// The PMIC rules that can be wrong in a way a test would miss: which register names the last power-off and which of its
// bits is the reason. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011);
// verification/juno-pon/mutants.sh plants bugs that Verus must refuse. lib.rs calls these functions and holds no copy of
// the rules. Moved here from lucina-evlog (2026-11-08) so that the kernel's crash record and the event log decode alike.
//
// After Qualcomm's downstream qpnp-power-on.c (`qpnp_pon_read_gen2_pon_off_reason`: POFF before FAULT before S3, then
// `ffs`).

//@ pub open spec fn bit(v: u16, k: u16) -> bool { (v >> k) & 1u16 == 1u16 }

/// OFF_REASON (0xC7) bits: which sequence ended the last power-off.
pub const POFF_SEQ: u8 = 0x80;
pub const FAULT_SEQ: u8 = 0x40;
pub const S3_SEQ: u8 = 0x20;

/// The register that holds the reason of the last power-off and its offset in the driver's reason table: POFF_REASON1 at 0,
/// FAULT_REASON1/2 at 16, S3_RESET_REASON at 32. None when OFF_REASON names no sequence.
pub fn off_register(off_seq: u8, poff: u8, fault: u16, s3: u8) -> Option<(u16, u16)> //@ret
//@ ensures
//@     off_seq & POFF_SEQ != 0 ==> ret == Some((poff as u16, 0u16)),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ != 0 ==> ret == Some((fault, 16u16)),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ == 0 && off_seq & S3_SEQ != 0 ==> ret == Some((s3 as u16, 32u16)),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ == 0 && off_seq & S3_SEQ == 0 ==> ret.is_none(),
//@     match ret { Some((v, o)) => o <= 32, None => true },
{
    if off_seq & POFF_SEQ != 0 {
        Some((poff as u16, 0))
    } else if off_seq & FAULT_SEQ != 0 {
        Some((fault, 16))
    } else if off_seq & S3_SEQ != 0 {
        Some((s3 as u16, 32))
    } else {
        None
    }
}

/// The lowest set bit at or above `from`, as the driver's `ffs` takes the first: None when no bit from `from` up is set.
pub fn lowest_bit(v: u16, from: u16) -> Option<u16> //@ret
//@ requires from <= 16,
//@ ensures
//@     match ret {
//@         Some(b) => from <= b && b < 16 && bit(v, b) && forall|k: u16| from <= k < b ==> !bit(v, k),
//@         None => forall|k: u16| from <= k < 16 ==> !bit(v, k),
//@     },
{
    let mut b = from;
    while b < 16
    //@ invariant from <= b <= 16, forall|k: u16| from <= k < b ==> !bit(v, k),
    //@ decreases 16 - b,
    {
        if (v >> b) & 1 == 1 {
            return Some(b);
        }
        b += 1;
    }
    None
}

//@ pub open spec fn first_bit(v: u16, o: u16, ret: Option<u16>) -> bool {
//@     match ret {
//@         Some(i) => o <= i && i < o + 16 && bit(v, (i - o) as u16) && forall|k: u16| k < i - o ==> !bit(v, k),
//@         None => forall|k: u16| k < 16 ==> !bit(v, k),
//@     }
//@ }

/// The reason table's index of the last power-off's first reason, as the driver gives it; always inside the 48-entry table.
pub fn off_index(off_seq: u8, poff: u8, fault: u16, s3: u8) -> Option<u16> //@ret
//@ ensures
//@     match ret { Some(i) => i < 48, None => true },
//@     off_seq & POFF_SEQ != 0 ==> first_bit(poff as u16, 0, ret),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ != 0 ==> first_bit(fault, 16, ret),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ == 0 && off_seq & S3_SEQ != 0 ==> first_bit(s3 as u16, 32, ret),
//@     off_seq & POFF_SEQ == 0 && off_seq & FAULT_SEQ == 0 && off_seq & S3_SEQ == 0 ==> ret.is_none(),
{
    match off_register(off_seq, poff, fault, s3) {
        Some((v, o)) => match lowest_bit(v, 0) {
            Some(b) => Some(o + b),
            None => None,
        },
        None => None,
    }
}
