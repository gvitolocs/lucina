// SPDX-License-Identifier: MIT
//! Packing a field into an instruction word, the one step every encoded bit goes through ([`crate::isa::encode`]). Verus
//! checks it as shipped (`verification/juno-ir3-pack/`): the field's bits get exactly the value, no other bit changes, and a
//! value too wide for its bits, or one that disagrees with bits already placed, is refused.
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Ones over `low..=high`.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires low <= high < 64,
    ensures r == ((if high - low == 63 { 0xffff_ffff_ffff_ffffu64 } else { ((1u64 << (high - low + 1) as u64) - 1) as u64 }) << low as u64),
))]
pub fn field_mask(low: u32, high: u32) -> u64 {
    #[cfg(verus_keep_ghost)]
    proof! {
        let w = (high - low + 1) as u64;
        assert(w < 64 ==> (1u64 << w) >= 1) by (bit_vector);
    }
    let ones = if high - low == 63 { u64::MAX } else { (1u64 << (high - low + 1) as u64) - 1 };
    ones << low as u64
}

/// `raw` at `low..=high` of `word`, given the bits `covered` so far: `(word, covered)` after, or `None` if `raw` is wider than
/// the field or disagrees with covered bits.
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    requires low <= high < 64,
    ensures
        r.is_some() ==> ({
            let m = field_mask_spec(low, high);
            &&& (r.unwrap().0 & m) == (raw << low as u64)
            &&& (r.unwrap().0 & !m) == (word & !m)
            &&& r.unwrap().1 == (covered | m)
        }),
        r.is_none() <==> (raw >> (high - low + 1) as u64 != 0 && high - low < 63)
            || ((covered & field_mask_spec(low, high)) != 0
                && (word & field_mask_spec(low, high)) != ((raw << low as u64) & field_mask_spec(low, high))
                && !(raw >> (high - low + 1) as u64 != 0 && high - low < 63)),
))]
pub fn place(word: u64, covered: u64, low: u32, high: u32, raw: u64) -> Option<(u64, u64)> {
    let width = high - low + 1;
    if width < 64 && raw >> width as u64 != 0 {
        return None;
    }
    let m = field_mask(low, high);
    let at = raw << low as u64;
    if covered & m != 0 && word & m != at & m {
        return None;
    }
    #[cfg(verus_keep_ghost)]
    proof! {
        lemma_place(word, raw, low, high);
    }
    Some(((word & !m) | at, covered | m))
}

#[cfg(verus_keep_ghost)]
verus! {

pub open spec fn field_mask_spec(low: u32, high: u32) -> u64 {
    (if high - low == 63 { 0xffff_ffff_ffff_ffffu64 } else { ((1u64 << (high - low + 1) as u64) - 1) as u64 }) << low as u64
}

proof fn lemma_place(word: u64, raw: u64, low: u32, high: u32)
    requires low <= high < 64, high - low == 63 || raw >> (high - low + 1) as u64 == 0,
    ensures ({
        let m = field_mask_spec(low, high);
        &&& (((word & !m) | (raw << low as u64)) & m) == (raw << low as u64)
        &&& (((word & !m) | (raw << low as u64)) & !m) == (word & !m)
    }),
{
    let l = low as u64;
    let w = (high - low + 1) as u64;
    if high - low == 63 {
        assert(l == 0);
        assert(forall|x: u64, y: u64| ((x & !(0xffff_ffff_ffff_ffffu64 << 0u64)) | (y << 0u64)) & (0xffff_ffff_ffff_ffffu64 << 0u64) == (y << 0u64)
            && ((x & !(0xffff_ffff_ffff_ffffu64 << 0u64)) | (y << 0u64)) & !(0xffff_ffff_ffff_ffffu64 << 0u64) == (x & !(0xffff_ffff_ffff_ffffu64 << 0u64))) by (bit_vector);
    } else {
        assert(w < 64 && l < 64 && l + w <= 64 && raw >> w == 0 ==> {
            let m = (((1u64 << w) - 1) as u64) << l;
            &&& (((word & !m) | (raw << l)) & m) == (raw << l)
            &&& (((word & !m) | (raw << l)) & !m) == (word & !m)
        }) by (bit_vector);
    }
}

} // verus!

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_refuses_and_keeps_other_bits() {
        assert_eq!(field_mask(4, 7), 0xf0);
        assert_eq!(field_mask(0, 63), u64::MAX);
        assert_eq!(place(0xff00, 0xff00, 4, 7, 0x5), Some((0xff50, 0xfff0)));
        assert_eq!(place(0, 0, 4, 7, 0x10), None);
        assert_eq!(place(0x50, 0xf0, 4, 7, 0x5), Some((0x50, 0xf0)));
        assert_eq!(place(0x50, 0xf0, 4, 7, 0x6), None);
        assert_eq!(place(0, 0, 0, 63, u64::MAX), Some((u64::MAX, u64::MAX)));
    }
}
