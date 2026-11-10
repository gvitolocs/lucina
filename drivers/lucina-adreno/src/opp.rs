// SPDX-License-Identifier: GPL-2.0-only
//! RPMh votes for the performance table: `a6xx_gmu_rpmh_arc_votes_init` of Linux.

use crate::{Result, err};

/// One operating point of the device tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opp {
    pub khz: u32,
    pub level: u32,
}

/// A vote from a level: `primary` and `secondary` are the ARC level tables of the command DB (`gfx.lvl` or `cx.lvl`, and
/// `mx.lvl`), u16 values.
pub fn vote(level: u32, primary: &[u16], secondary: &[u16]) -> Result<u32> {
    let Some(pindex) = primary.iter().position(|v| u32::from(*v) >= level) else {
        return err("no ARC level for this level", level);
    };
    let mut sindex = 0usize;
    for (index, value) in secondary.iter().enumerate() {
        if u32::from(*value) >= level {
            sindex = index;
            break;
        }
        if *value != 0 {
            sindex = index;
        }
    }
    Ok((u32::from(primary[pindex]) << 16) | ((sindex as u32) << 8) | pindex as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vote_is_the_first_level_at_or_above() {
        let gfx = [0u16, 48, 64, 128, 192, 256, 384];
        let mx = [0u16, 128, 256, 384];
        assert_eq!(vote(0, &gfx, &mx).unwrap(), 0);
        assert_eq!(vote(64, &gfx, &mx).unwrap(), (64 << 16) | (1 << 8) | 2);
        assert_eq!(vote(192, &gfx, &mx).unwrap(), (192 << 16) | (2 << 8) | 4);
        assert!(vote(416, &gfx, &mx).is_err());
    }

    #[test]
    fn the_secondary_falls_back_to_its_highest_vote() {
        assert_eq!(
            vote(384, &[384], &[0, 128]).unwrap(),
            (384 << 16) | (1 << 8)
        );
    }
}
