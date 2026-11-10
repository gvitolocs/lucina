//! Bus Clock Manager votes: how Linux's `drivers/interconnect/qcom/icc-rpmh.c` and `bcm-voter.c` (GPL-2.0) turn a bandwidth on an
//! interconnect path into one RPMh command per BCM, for the paths the video block's device tree node names (`video-mem` and
//! `cpu-cfg` on SC7180, nodes and widths from `sc7180.c`).
//!
//! Linux adds up every consumer of a node before it votes, so one write carries all of them. Here there is no such sum across
//! drivers: [`not_lower`] keeps a vote that is already in a TCS of this DRV if it is higher, so that a write never takes a BCM below
//! what someone else (the firmware's display path, for one) left there. Only active votes are sent, as everywhere else in LucinaOS.

use crate::rsc::TcsCmd;
use lucina_qcom_soc::cmd_db::CmdDb;
use lucina_qcom_soc::io::RegIo;

/// `struct bcm_db`: what the command DB holds for a BCM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aux {
    pub unit: u32,
    pub width: u16,
    pub vcd: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BcmError {
    NotInDatabase,
    /// No aux data, or a unit of 0, which Linux would divide by.
    BadAux,
}

impl core::fmt::Display for BcmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BcmError::NotInDatabase => f.write_str("BCM not in cmd-db"),
            BcmError::BadAux => f.write_str("BCM aux data unusable"),
        }
    }
}

/// `qcom_icc_bcm_init`: the BCM's address and aux data.
pub fn lookup<R: RegIo>(db: &CmdDb<R>, name: &str) -> Result<(u32, Aux), BcmError> {
    let entry = db.find(name).ok_or(BcmError::NotInDatabase)?;
    let mut raw = [0u8; 8];
    if db.aux(entry, &mut raw) < 8 {
        return Err(BcmError::BadAux);
    }
    let aux = Aux { unit: u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]), width: u16::from_le_bytes([raw[4], raw[5]]), vcd: raw[6] };
    if aux.unit == 0 {
        return Err(BcmError::BadAux);
    }
    Ok((entry.addr, aux))
}

/// One interconnect node of a path as `sc7180.c` declares it, and which BCMs it belongs to.
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub name: &'static str,
    pub channels: u32,
    pub buswidth: u32,
    pub bcms: &'static [&'static str],
}

/// `video-mem`: `MASTER_VIDEO_P0` to `SLAVE_EBI1`.
pub const VIDEO_MEM: [Node; 6] = [
    Node { name: "qxm_venus0", channels: 1, buswidth: 32, bcms: &["MM1"] },
    Node { name: "qns_mem_noc_sf", channels: 1, buswidth: 32, bcms: &["MM2"] },
    Node { name: "qnm_mnoc_sf", channels: 1, buswidth: 32, bcms: &[] },
    Node { name: "qns_llcc", channels: 1, buswidth: 16, bcms: &["SH0"] },
    Node { name: "llcc_mc", channels: 2, buswidth: 4, bcms: &[] },
    Node { name: "ebi", channels: 2, buswidth: 4, bcms: &["ACV", "MC0"] },
];

/// `cpu-cfg`: `MASTER_APPSS_PROC` to `SLAVE_VENUS_CFG`.
pub const CPU_CFG: [Node; 6] = [
    Node { name: "acm_apps0", channels: 1, buswidth: 16, bcms: &["SH4"] },
    Node { name: "qns_gem_noc_snoc", channels: 1, buswidth: 8, bcms: &[] },
    Node { name: "qnm_gemnoc", channels: 1, buswidth: 8, bcms: &["SN12"] },
    Node { name: "qns_cnoc", channels: 1, buswidth: 8, bcms: &[] },
    Node { name: "qnm_snoc", channels: 1, buswidth: 8, bcms: &["CN0"] },
    Node { name: "qhs_venus_cfg", channels: 1, buswidth: 4, bcms: &["CN0"] },
];

/// The BCMs of SC7180 with `keepalive` set (`bcm_mc0`, `bcm_sh0`, `bcm_cn0`, `bcm_sn0` in `sc7180.c`), and the one with an
/// `enable_mask` (`bcm_acv`, BIT(3)).
pub const KEEPALIVE: [&str; 4] = ["MC0", "SH0", "CN0", "SN0"];
const ACV: &str = "ACV";
const ACV_ENABLE_MASK: u32 = 1 << 3;

/// `bcm_div`: a small vote never rounds down to nothing.
pub fn bcm_div(num: u64, base: u64) -> u64 {
    if num != 0 && num < base { 1 } else { num / base }
}

/// `BCM_TCS_CMD_VOTE_MASK`: 14 bits each.
const VOTE_MASK: u64 = 0x3fff;

/// `bcm_aggregate` for one BCM of a path with `avg` and `peak` kBps on every node of it, then `tcs_cmd_gen` with commit and wait,
/// one command per BCM: x and y, clamped to 14 bits, `valid` unless both are 0.
pub fn vote(name: &str, aux: Aux, path: &[Node], avg_kbps: u64, peak_kbps: u64) -> (u32, u32) {
    if name == ACV {
        let any = avg_kbps != 0 || peak_kbps != 0;
        return if any { (0, ACV_ENABLE_MASK) } else { (0, 0) };
    }
    let mut agg_avg = 0;
    let mut agg_peak = 0;
    for n in path.iter().filter(|n| n.bcms.contains(&name)) {
        agg_avg = agg_avg.max(bcm_div(avg_kbps * u64::from(aux.width), u64::from(n.buswidth * n.channels)));
        agg_peak = agg_peak.max(bcm_div(peak_kbps * u64::from(aux.width), u64::from(n.buswidth)));
    }
    let mut x = bcm_div(agg_avg * 1000, u64::from(aux.unit));
    let mut y = bcm_div(agg_peak * 1000, u64::from(aux.unit));
    if KEEPALIVE.contains(&name) && x == 0 && y == 0 {
        x = 1;
        y = 1;
    }
    (x.min(VOTE_MASK) as u32, y.min(VOTE_MASK) as u32)
}

/// `BCM_TCS_CMD(commit, valid, x, y)`.
pub fn command_data(commit: bool, x: u32, y: u32) -> u32 {
    let valid = !(x == 0 && y == 0);
    ((commit as u32) << 30) | ((valid as u32) << 29) | ((x & 0x3fff) << 14) | (y & 0x3fff)
}

/// The x and y of a BCM command's data.
pub fn command_votes(data: u32) -> (u32, u32) {
    ((data >> 14) & 0x3fff, data & 0x3fff)
}

/// The larger of our vote and every one already written for this address in a TCS of the DRV (`slots` as `Rsc::slots` gives them).
pub fn not_lower(addr: u32, x: u32, y: u32, slots: &[(u32, u32, u32, u32)]) -> (u32, u32) {
    slots.iter().filter(|s| s.2 == addr).fold((x, y), |(x, y), s| {
        let (sx, sy) = command_votes(s.3);
        (x.max(sx), y.max(sy))
    })
}

/// The command for one BCM: commit and wait, as the last command of a VCD group is in `qcom_icc_bcm_voter_commit`.
pub fn command(addr: u32, x: u32, y: u32) -> TcsCmd {
    TcsCmd { addr, data: command_data(true, x, y), wait: true }
}

/// One BCM's vote before it is batched: its address, its VCD (from the aux data) and its x and y.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vote {
    pub addr: u32,
    pub vcd: u8,
    pub x: u32,
    pub y: u32,
}

/// `MAX_RPMH_PAYLOAD`: commands in one request.
pub const MAX_RPMH_PAYLOAD: usize = 16;

/// `qcom_icc_bcm_voter_commit` for the active bucket: the BCMs sorted by VCD, keeping their order within one (`list_sort` with
/// `cmp_vcd` is stable), then `tcs_list_gen`: commit only on the last BCM of each VCD, wait there too (the apps voter's default
/// `tcs_wait` covers the active bucket), and requests of at most [`MAX_RPMH_PAYLOAD`] that never split a VCD. Each request goes as
/// one `rpmh_write_batch` payload; the command's own `wait` says which answers to wait for.
pub fn batches(votes: &[Vote]) -> alloc::vec::Vec<alloc::vec::Vec<TcsCmd>> {
    let mut sorted = votes.to_vec();
    sorted.sort_by_key(|v| v.vcd);
    let mut n: alloc::vec::Vec<usize> = alloc::vec![0];
    let mut cmds = alloc::vec::Vec::with_capacity(sorted.len());
    let mut cur_vcd_size = 0;
    for (i, v) in sorted.iter().enumerate() {
        cur_vcd_size += 1;
        let commit = i + 1 == sorted.len() || sorted[i + 1].vcd != v.vcd;
        if commit {
            cur_vcd_size = 0;
        }
        cmds.push(TcsCmd { addr: v.addr, data: command_data(commit, v.x, v.y), wait: commit });
        let batch = n.len() - 1;
        n[batch] += 1;
        if n[batch] >= MAX_RPMH_PAYLOAD {
            if commit {
                n.push(0);
            } else {
                n[batch] -= cur_vcd_size;
                n.push(cur_vcd_size);
            }
        }
    }
    let mut out = alloc::vec::Vec::new();
    let mut at = 0;
    for count in n.into_iter().filter(|c| *c != 0) {
        out.push(cmds[at..at + count].to_vec());
        at += count;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUX: Aux = Aux { unit: 1000, width: 4, vcd: 0 };

    #[test]
    fn small_votes_round_up() {
        assert_eq!(bcm_div(0, 7), 0);
        assert_eq!(bcm_div(3, 7), 1);
        assert_eq!(bcm_div(14, 7), 2);
    }

    #[test]
    fn the_narrowest_node_decides() {
        // CN0 on qnm_snoc (8 bytes) and qhs_venus_cfg (4 bytes): 125 kBps * 4 / 4 = 125, * 1000 / 1000.
        assert_eq!(vote("CN0", AUX, &CPU_CFG, 125, 0), (125, 0));
        // MC0 on ebi, two channels of 4 bytes: 2500 * 4 / 8 = 1250.
        assert_eq!(vote("MC0", AUX, &VIDEO_MEM, 2500, 0), (1250, 0));
    }

    #[test]
    fn keepalive_and_acv() {
        assert_eq!(vote("SH0", AUX, &VIDEO_MEM, 0, 0), (1, 1));
        assert_eq!(vote("SN0", AUX, &VIDEO_MEM, 0, 0), (1, 1));
        assert_eq!(vote("MM2", AUX, &VIDEO_MEM, 0, 0), (0, 0));
        assert_eq!(vote("ACV", AUX, &VIDEO_MEM, 1, 0), (0, 8));
        assert_eq!(vote("ACV", AUX, &VIDEO_MEM, 0, 0), (0, 0));
    }

    #[test]
    fn commands_and_clamps() {
        assert_eq!(command_data(true, 1, 2), (1 << 30) | (1 << 29) | (1 << 14) | 2);
        assert_eq!(command_data(true, 0, 0), 1 << 30);
        assert_eq!(command_votes(command_data(false, 0x3fff, 5)), (0x3fff, 5));
        assert_eq!(vote("MM1", Aux { unit: 1, width: 32, vcd: 0 }, &VIDEO_MEM, 1 << 40, 0).0, 0x3fff);
    }

    fn v(addr: u32, vcd: u8) -> Vote {
        Vote { addr, vcd, x: 1, y: 0 }
    }

    #[test]
    fn sorted_by_vcd_with_a_commit_on_the_last_of_each() {
        let b = batches(&[v(0xa, 1), v(0xb, 0), v(0xc, 1), v(0xd, 0)]);
        assert_eq!(b.len(), 1);
        let order: alloc::vec::Vec<u32> = b[0].iter().map(|c| c.addr).collect();
        assert_eq!(order, [0xb, 0xd, 0xa, 0xc], "stable within a VCD");
        let commits: alloc::vec::Vec<bool> = b[0].iter().map(|c| c.data & (1 << 30) != 0).collect();
        assert_eq!(commits, [false, true, false, true]);
        let waits: alloc::vec::Vec<bool> = b[0].iter().map(|c| c.wait).collect();
        assert_eq!(waits, commits);
    }

    #[test]
    fn a_full_request_never_splits_a_vcd() {
        // 15 BCMs of VCD 0, then 3 of VCD 1: the 16th command is not a commit, so VCD 1's first goes to the next request with the rest.
        let mut votes: alloc::vec::Vec<Vote> = (0..15).map(|i| v(0x100 + i, 0)).collect();
        votes.extend((0..3).map(|i| v(0x200 + i, 1)));
        let b = batches(&votes);
        assert_eq!(b.iter().map(|r| r.len()).collect::<alloc::vec::Vec<_>>(), [15, 3]);
        assert!(b[0][14].data & (1 << 30) != 0 && b[1][2].data & (1 << 30) != 0);
        // Exactly 16 ending on a commit: one full request, nothing empty after it.
        let sixteen: alloc::vec::Vec<Vote> = (0..16).map(|i| v(0x300 + i, i as u8)).collect();
        assert_eq!(batches(&sixteen).iter().map(|r| r.len()).collect::<alloc::vec::Vec<_>>(), [16]);
        assert!(batches(&[]).is_empty());
    }

    #[test]
    fn an_existing_higher_vote_stays() {
        let slots = [(0, 0, 0x5000, command_data(true, 900, 3)), (1, 0, 0x6000, command_data(true, 5000, 0))];
        assert_eq!(not_lower(0x5000, 1250, 0, &slots), (1250, 3));
        assert_eq!(not_lower(0x6000, 10, 0, &slots), (5000, 0));
        assert_eq!(not_lower(0x7000, 10, 1, &slots), (10, 1));
    }
}
