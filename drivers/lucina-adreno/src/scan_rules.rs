// SPDX-License-Identifier: MPL-2.0

//! When a batch that draws down to row `bottom` may start, given where the panel's scan is (kernel patch 1166, the display's
//! vsync by interrupt): at once when the scan is not on the rows the batch draws, else at the next vertical blank, which the
//! INTF_1 vsync interrupt marks (`dpu_6_2_sc7180.h`: `intr_vsync = DPU_IRQ_IDX(MDP_SSPP_TOP0_INTR, 27)`).
//!
//! Verus proves this very file (`verification/juno-vsync/rules.rs`, D-011).
#![allow(unexpected_cfgs)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// What the batch does.
#[cfg_attr(verus_keep_ghost, verus_verify)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scan {
    /// Start now.
    Go,
    /// Sleep until the next vertical blank (the vsync interrupt), then look again.
    WaitVblank,
}

#[cfg(verus_keep_ghost)]
verus! {

/// The scan is on a row the batch draws: inside the picture, above the batch's last row.
pub open spec fn on_batch_rows(r: i32, height: i32, bottom: u32) -> bool {
    &&& height > 0
    &&& 0 <= r
    &&& r < height
    &&& (r as int) < (bottom as int)
}

} // verus!

/// `row` is the scan's row and the picture's height (`None`: the timing engine is off, nothing is scanned).
#[cfg_attr(verus_keep_ghost, verus_spec(r =>
    ensures
        row.is_none() ==> r == Scan::Go,
        row.is_some() ==> (r == Scan::WaitVblank <==> on_batch_rows(row.unwrap().0, row.unwrap().1, bottom)),
))]
pub fn decide(row: Option<(i32, i32)>, bottom: u32) -> Scan {
    match row {
        None => Scan::Go,
        Some((r, height)) => {
            if height <= 0 || r < 0 || r >= height || r as i64 >= bottom as i64 {
                Scan::Go
            } else {
                Scan::WaitVblank
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_batch_waits_only_while_the_scan_is_on_its_rows() {
        assert_eq!(decide(None, 1080), Scan::Go);
        assert_eq!(decide(Some((-5, 1080)), 1080), Scan::Go, "in the blanking before row 0");
        assert_eq!(decide(Some((1085, 1080)), 1080), Scan::Go, "in the blanking after the last row");
        assert_eq!(decide(Some((500, 1080)), 400), Scan::Go, "below the batch's rows");
        assert_eq!(decide(Some((300, 1080)), 400), Scan::WaitVblank);
        assert_eq!(decide(Some((0, 1080)), 1080), Scan::WaitVblank);
        assert_eq!(decide(Some((0, 0)), 1080), Scan::Go, "no timing");
    }
}
