//! The HTT data path: the rx ring, MSDU pop and classification, and frame transmit.
//!
//! Derived from Linux `drivers/net/wireless/ath/ath10k/htt_rx.c` and `htt_tx.c` (SPDX ISC; Copyright (c) 2005-2011
//! Atheros Communications Inc., 2011-2017 Qualcomm Atheros, Inc., 2018 The Linux Foundation, Qualcomm Technologies,
//! Inc. and/or its subsidiaries).
//!
//! * [`HttRx`] is `struct ath10k_htt.rx_ring`: `ath10k_htt_rx_alloc`, `__ath10k_htt_rx_ring_fill_n`,
//!   `ath10k_htt_rx_msdu_buff_replenish`, `ath10k_htt_rx_netbuf_pop` and `ath10k_htt_rx_pop_paddr`, with the buffers
//!   kept by address. A popped buffer goes back on the spare list instead of being freed.
//! * [`HttRx::amsdu_pop`] is `ath10k_htt_rx_amsdu_pop`, [`HttRx::pop_in_ord`] is
//!   `ath10k_htt_rx_pop_paddr64_list` plus `ath10k_htt_rx_extract_amsdu`.
//! * [`classify`] does what `ath10k_htt_rx_h_ppdu`, `_h_unchain`, `_h_filter`, `_h_mpdu` decide, and what mac80211
//!   would then drop given their flags (there is no mac80211 behind this driver): management frames seen here (they
//!   also arrive over WMI), FCS errors, failed decryption and TKIP MIC errors. Headers are not rebuilt: a frame goes
//!   up as the device left it, with [`RxFrame::payload_off`] saying where its 802.11 header ends.
//! * [`HttTx`] is the frame side of `ath10k_htt_tx_64` with `continuous_frag_desc`: the tx buffer array, the fragment
//!   bank and a copy of each frame in a preallocated buffer, by MSDU id.
//! * [`PnWindow`] stands in for mac80211's replay check on the path where the host does that (rx indication).
//!   mac80211 first reorders by sequence number and then wants a rising PN; this has no reorder buffer, so it takes the
//!   IPsec form: anything not yet seen within 64 below the highest PN passes. Every PN still passes only once.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::ce::{Barrier, CeError, DescMem, SEND_FLAG_GATHER};
use crate::htt::{self, RX_BUF_SIZE, RX_DESC_SIZE, rx_desc as rd};
use crate::snoc_config::CE_POLL_PIPE;

use super::hif::{CTX_NONE, Hif};
use super::{Config, Dma, DmaBuf, Platform};

/// `ATH10K_HTT_MAX_NUM_REFILL`.
const MAX_NUM_REFILL: usize = 100;
/// `HTT_RX_RING_REFILL_RETRY_MS` (`htt_rx.c`): a refill that could not get a buffer tries again after this.
pub const REFILL_RETRY_MS: u64 = 50;
/// `HTT_RX_RING_REFILL_RESCHED_MS`: a refill that hit `ATH10K_HTT_MAX_NUM_REFILL` with more to fill goes on after this.
pub const REFILL_RESCHED_MS: u64 = 5;
/// `ath10k_htt_rx_msdu_size`.
const RX_MSDU_SIZE: usize = RX_BUF_SIZE as usize - RX_DESC_SIZE;
/// Payload bytes one A-MSDU may collect: the frames of one MPDU, far above any frame this driver can use.
const MAX_FRAME_BYTES: usize = 16 * 1024;

/// A descriptor copied out of the buffer the device wrote; every accessor is in bounds by construction.
#[derive(Clone)]
pub(crate) struct RxDesc([u8; RX_DESC_SIZE]);

impl RxDesc {
    fn w(&self, off: usize) -> u32 {
        u32::from_le_bytes([
            self.0[off],
            self.0[off + 1],
            self.0[off + 2],
            self.0[off + 3],
        ])
    }
    fn attention(&self) -> u32 {
        self.w(rd::ATTENTION)
    }
    fn more_count(&self) -> u8 {
        self.0[rd::RING2_MORE_COUNT]
    }
    fn enctype(&self) -> u32 {
        self.w(rd::MPDU_START_INFO0) >> 28
    }
    fn msdu_len(&self) -> usize {
        (self.w(rd::MSDU_START_INFO0) & rd::MSDU_LENGTH_MASK) as usize
    }
    fn decap(&self) -> u32 {
        (self.w(rd::MSDU_START_INFO1) >> 8) & 3
    }
    fn msdu_end(&self) -> u32 {
        self.w(rd::MSDU_END_INFO0)
    }
    fn tid(&self) -> u8 {
        (self.w(rd::MPDU_START_INFO1) >> 28) as u8
    }
    fn hdr(&self) -> &[u8] {
        &self.0[rd::HDR_STATUS..rd::HDR_STATUS + rd::HDR_STATUS_LEN]
    }
}

/// One rx buffer's content.
pub(crate) struct Msdu {
    /// `None` for a chained continuation, which has no descriptor.
    desc: Option<RxDesc>,
    bytes: Vec<u8>,
}

/// `struct ath10k_htt.rx_ring`.
pub(crate) struct HttRx {
    size: usize,
    fill_level: usize,
    ring: DmaBuf,
    alloc_idx: DmaBuf,
    fill_cnt: usize,
    sw_rd_idx: usize,
    /// Everything the device may hold, by address (the C's `skb_table` plus `netbufs_ring`).
    held: BTreeMap<u64, DmaBuf>,
    by_idx: Vec<u64>,
    spare: Vec<DmaBuf>,
    /// `in_ord_rx`: buffers come back by address, not in ring order.
    pub in_ord: bool,
    /// `rx_confused`: the ring and the firmware disagree; the data path stays off, as in Linux.
    pub confused: bool,
    /// `num_mpdus_ready`.
    pub mpdus_ready: u32,
}

impl HttRx {
    /// `ath10k_htt_rx_alloc`, for a power-of-two `size` that [`Config::check`] has seen.
    pub fn alloc<D: Dma>(dma: &mut D, cfg: &Config) -> Option<HttRx> {
        let size = usize::from(cfg.htt_rx_ring_size);
        // dma_alloc_coherent in the C (htt_rx.c): the device writes the ring and its index
        let ring = dma.alloc_coherent(size * 8)?;
        let Some(alloc_idx) = dma.alloc_coherent(4) else {
            dma.free(ring);
            return None;
        };
        let zeros = [0u8; 64];
        let mut off = 0;
        while off < ring.len {
            dma.write(&ring, off, &zeros[..(ring.len - off).min(64)]);
            off += 64;
        }
        dma.write(&alloc_idx, 0, &[0; 4]);
        Some(HttRx {
            size,
            // hw_params.rx_ring_fill_level = HTT_RX_RING_FILL_LEVEL_DUAL_MAC: size - 1
            fill_level: size - 1,
            ring,
            alloc_idx,
            fill_cnt: 0,
            sw_rd_idx: 0,
            held: BTreeMap::new(),
            by_idx: alloc::vec![0; size],
            spare: Vec::new(),
            in_ord: false,
            confused: false,
            mpdus_ready: 0,
        })
    }

    pub fn ring_paddr(&self) -> u64 {
        self.ring.paddr
    }
    pub fn alloc_idx_paddr(&self) -> u64 {
        self.alloc_idx.paddr
    }
    pub fn size(&self) -> u16 {
        self.size as u16
    }
    pub fn fill_cnt(&self) -> usize {
        self.fill_cnt
    }

    /// The firmware index the host keeps in shared memory.
    pub fn alloc_idx_value<D: Dma>(&self, dma: &mut D) -> u32 {
        let mut b = [0u8; 4];
        dma.read(&self.alloc_idx, 0, &mut b);
        u32::from_le_bytes(b)
    }

    /// `__ath10k_htt_rx_ring_fill_n`. The index is read back from shared memory and checked, as in the C ("firmware
    /// malfunctioning?"); then masked and written anyway. Returns false when out of memory or the index was bad.
    fn fill_n<M: DescMem, D: Dma>(&mut self, mem: &mut M, dma: &mut D, mut num: usize) -> bool {
        let mut b = [0u8; 4];
        dma.read(&self.alloc_idx, 0, &mut b);
        let raw = u32::from_le_bytes(b) as usize;
        let mask = self.size - 1;
        let mut ok = raw < self.size;
        let mut idx = raw & mask;
        if !ok {
            num = 0;
        }
        while num > 0 {
            let Some(buf) = self.spare.pop().or_else(|| dma.alloc(RX_BUF_SIZE as usize)) else {
                ok = false;
                break;
            };
            // Clear rx_desc attention word before posting to Rx ring
            dma.write(&buf, rd::ATTENTION, &[0; 4]);
            let paddr = buf.paddr;
            self.held.insert(paddr, buf);
            self.by_idx[idx] = paddr;
            dma.write(&self.ring, idx * 8, &paddr.to_le_bytes());
            self.fill_cnt += 1;
            num -= 1;
            idx = (idx + 1) & mask;
        }
        // Make sure the rx buffer is updated before available buffer index
        mem.barrier(Barrier::Release);
        dma.write(&self.alloc_idx, 0, &(idx as u32).to_le_bytes());
        ok
    }

    /// `ath10k_htt_rx_ring_refill`.
    pub fn refill<M: DescMem, D: Dma>(&mut self, mem: &mut M, dma: &mut D) -> bool {
        let n = self.fill_level.saturating_sub(self.fill_cnt);
        self.fill_n(mem, dma, n)
    }

    /// `ath10k_htt_rx_msdu_buff_replenish`: at most 100 at a time; the next poll is the retry timer.
    /// Returns when to refill again, as `ath10k_htt_rx_msdu_buff_replenish` arms its timer: [`REFILL_RETRY_MS`] after a
    /// buffer could not be had, [`REFILL_RESCHED_MS`] while the ring is still short, none once it is full.
    pub fn replenish<M: DescMem, D: Dma>(&mut self, mem: &mut M, dma: &mut D) -> Option<u64> {
        let n = self.fill_level.saturating_sub(self.fill_cnt);
        if n == 0 {
            return None;
        }
        if !self.fill_n(mem, dma, n.min(MAX_NUM_REFILL)) {
            Some(REFILL_RETRY_MS)
        } else if self.fill_cnt < self.fill_level {
            Some(REFILL_RESCHED_MS)
        } else {
            None
        }
    }

    /// `ath10k_htt_rx_netbuf_pop`.
    fn pop<D: Dma>(&mut self, dma: &mut D) -> Option<DmaBuf> {
        if self.fill_cnt == 0 {
            return None;
        }
        let idx = self.sw_rd_idx;
        let paddr = core::mem::replace(&mut self.by_idx[idx], 0);
        dma.write(&self.ring, idx * 8, &[0; 8]);
        self.sw_rd_idx = (idx + 1) & (self.size - 1);
        self.fill_cnt -= 1;
        self.held.remove(&paddr)
    }

    /// `ath10k_htt_rx_pop_paddr`.
    fn pop_paddr(&mut self, paddr: u64) -> Option<DmaBuf> {
        let b = self.held.remove(&paddr)?;
        self.fill_cnt = self.fill_cnt.saturating_sub(1);
        Some(b)
    }

    fn read_desc<D: Dma>(dma: &mut D, b: &DmaBuf) -> RxDesc {
        let mut d = [0u8; RX_DESC_SIZE];
        dma.read(b, 0, &mut d);
        RxDesc(d)
    }

    /// `ath10k_htt_rx_amsdu_pop`: one MPDU, as `(msdus, chained)`. `Err` is the C's negative return, on which the
    /// caller turns [`HttRx::confused`] on. The loop consumes a ring buffer per turn, so it ends with the ring.
    pub fn amsdu_pop<D: Dma>(&mut self, dma: &mut D) -> Result<(Vec<Msdu>, bool), ()> {
        let mut out = Vec::new();
        let mut chained = false;
        loop {
            let Some(buf) = self.pop(dma) else {
                return Err(());
            };
            let desc = Self::read_desc(dma, &buf);
            let att = desc.attention();
            if att & rd::ATT_MSDU_DONE == 0 {
                self.spare.push(buf);
                return Err(());
            }
            let invalid = att & (rd::ATT_MPDU_LENGTH_ERR | rd::ATT_MSDU_LENGTH_ERR) != 0;
            let mut msdu_len = if invalid { 0 } else { desc.msdu_len() };
            let n = msdu_len.min(RX_MSDU_SIZE);
            let mut bytes = alloc::vec![0u8; n];
            dma.read(&buf, RX_DESC_SIZE, &mut bytes);
            self.spare.push(buf);
            msdu_len -= n;
            let more = desc.more_count();
            let last = desc.msdu_end() & rd::MSDU_END_LAST != 0;
            out.push(Msdu {
                desc: Some(desc),
                bytes,
            });
            for _ in 0..more {
                let Some(buf) = self.pop(dma) else {
                    return Err(());
                };
                let n = msdu_len.min(RX_BUF_SIZE as usize);
                let mut bytes = alloc::vec![0u8; n];
                dma.read(&buf, 0, &mut bytes);
                self.spare.push(buf);
                msdu_len -= n;
                chained = true;
                out.push(Msdu { desc: None, bytes });
            }
            if last {
                return Ok((out, chained));
            }
        }
    }

    /// `ath10k_htt_rx_pop_paddr64_list` then `ath10k_htt_rx_extract_amsdu`: the indication's buffers grouped into
    /// MPDUs. An address the host never posted, a descriptor not done, or a list ending inside an MPDU is the C's
    /// `rx_confused`. A length beyond the buffer is the device lying: refused the same way (the C's `skb_put` would
    /// overrun).
    pub fn pop_in_ord<D: Dma>(
        &mut self,
        dma: &mut D,
        ind: &htt::InOrdInd,
    ) -> Result<Vec<Vec<Msdu>>, ()> {
        let mut mpdus = Vec::new();
        let mut cur = Vec::new();
        for m in &ind.msdus {
            let Some(buf) = self.pop_paddr(m.paddr) else {
                return Err(());
            };
            let desc = Self::read_desc(dma, &buf);
            let len = usize::from(m.msdu_len);
            if desc.attention() & rd::ATT_MSDU_DONE == 0 || len > RX_MSDU_SIZE {
                self.spare.push(buf);
                return Err(());
            }
            let mut bytes = alloc::vec![0u8; len];
            dma.read(&buf, RX_DESC_SIZE, &mut bytes);
            self.spare.push(buf);
            let last = desc.msdu_end() & rd::MSDU_END_LAST != 0;
            cur.push(Msdu {
                desc: Some(desc),
                bytes,
            });
            if last {
                mpdus.push(core::mem::take(&mut cur));
            }
        }
        if cur.is_empty() { Ok(mpdus) } else { Err(()) }
    }

    /// The buffers of an offload indication: no rx descriptor, a short header and the frame, whole in the buffer
    /// (`ath10k_htt_rx_h_rx_offload`). A buffer whose claim does not fit is dropped; an unknown address is confusion.
    pub fn pop_offload<D: Dma>(
        &mut self,
        dma: &mut D,
        ind: &htt::InOrdInd,
    ) -> Result<Vec<Vec<u8>>, ()> {
        let mut out = Vec::new();
        for m in &ind.msdus {
            let Some(buf) = self.pop_paddr(m.paddr) else {
                return Err(());
            };
            let mut all = alloc::vec![0u8; buf.len];
            dma.read(&buf, 0, &mut all);
            self.spare.push(buf);
            if let Ok(o) = htt::parse_rx_offload_msdu(&all) {
                out.push(o.payload.to_vec());
            }
        }
        Ok(out)
    }

    /// Hand everything back to the platform.
    pub fn free<D: Dma>(&mut self, dma: &mut D) {
        while let Some((_, b)) = self.held.pop_first() {
            dma.free(b);
        }
        for b in self.spare.drain(..) {
            dma.free(b);
        }
        dma.free(self.ring.clone());
        dma.free(self.alloc_idx.clone());
    }
}

// ---- receive classification ----------------------------------------------------------------------------------------

/// `ieee80211_hdrlen` for data and management frames.
pub(crate) fn hdrlen(fc: u16) -> usize {
    let ftype = (fc >> 2) & 3;
    let mut len = 24;
    if ftype == 2 {
        if fc & 0x0300 == 0x0300 {
            len = 30; // four addresses
        }
        if fc & 0x0080 != 0 {
            len += 2; // QoS control
        }
    }
    if fc & 0x8000 != 0 {
        len += 4; // HT control
    }
    len
}

/// How the device left the frame (`enum rx_msdu_decap_format`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decap {
    Raw,
    NativeWifi,
    Ethernet2,
    Snap8023,
}

/// `ath10k_htt_rx_h_rates`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxRate {
    /// The hardware rate code; `cck` for the 802.11b rates.
    Legacy {
        hw_rate: u8,
        cck: bool,
    },
    Ht {
        mcs: u8,
        bw40: bool,
        sgi: bool,
    },
    Vht {
        mcs: u8,
        nss: u8,
        bw: u8,
        sgi: bool,
    },
    Unknown,
}

/// One received MSDU.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RxFrame {
    /// As the device left it: native wifi is the 802.11 header (no QoS control, padded to 4), then LLC and payload.
    pub frame: Vec<u8>,
    pub decap: Decap,
    /// Where the 802.11 header ends in [`RxFrame::frame`] for native wifi (`ath10k_htt_rx_nwifi_hdrlen`; this firmware
    /// lacks `NO_NWIFI_DECAP_4ADDR_PADDING`); 0 otherwise.
    pub payload_off: usize,
    /// The first bytes of the air frame, with the original 802.11 header (`rx_hdr_status`).
    pub air_hdr: [u8; 64],
    /// Encrypted on the air and decrypted by the device. False is a frame sent in the clear, EAPOL before the keys:
    /// whoever reads this decides whether a clear frame is acceptable now.
    pub decrypted: bool,
    pub mcast: bool,
    pub tid: u8,
    pub peer_id: u16,
    pub freq: u32,
    /// `ATH10K_DEFAULT_NOISE_FLOOR + rssi_comb` of the PPDU.
    pub rssi: i32,
    pub rate: RxRate,
    /// From the CCMP header when the frame carried one, else 0. On the in-order and offload paths the firmware keeps
    /// the replay state and strips the header, so it is 0 there.
    pub pn: u64,
}

/// `struct ieee80211_rx_status` where it spans a PPDU: the first MPDU of a PPDU sets it, the others reuse it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PpduStatus {
    pub rssi: i32,
    pub rate: RxRate,
}

impl Default for PpduStatus {
    fn default() -> Self {
        PpduStatus {
            rssi: 0,
            rate: RxRate::Unknown,
        }
    }
}

/// Why a frame was not delivered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxDrops {
    pub no_channel: u32,
    pub bad_amsdu: u32,
    pub unchain: u32,
    pub mgmt: u32,
    pub fcs: u32,
    pub crypto: u32,
    pub mic: u32,
    pub replay: u32,
}

fn rate_of(d: &RxDesc) -> RxRate {
    let info1 = d.w(rd::PPDU_INFO1);
    let info2 = d.w(rd::PPDU_INFO2);
    let info3 = d.w(rd::PPDU_INFO3);
    match info1 >> 24 {
        rd::PREAMBLE_LEGACY => RxRate::Legacy {
            hw_rate: (info1 & 0xf) as u8 & !0x8, // RX_PPDU_START_RATE_FLAG
            cck: info1 & (1 << 4) != 0,
        },
        rd::PREAMBLE_HT | rd::PREAMBLE_HT_TXBF => RxRate::Ht {
            mcs: (info2 & 0x1f) as u8,
            bw40: (info2 >> 7) & 1 != 0,
            sgi: (info3 >> 7) & 1 != 0,
        },
        rd::PREAMBLE_VHT | rd::PREAMBLE_VHT_TXBF => {
            let group = (info2 >> 4) & 0x3f;
            let (mcs, nss) = if group == 0 || group == 63 {
                // GROUP_ID_IS_SU_MIMO
                let nsts = (info2 >> 10) & 7;
                let nss = if (info2 >> 3) & 1 != 0 {
                    (nsts >> 2) + 1
                } else {
                    nsts + 1
                };
                (((info3 >> 4) & 0xf) as u8, nss as u8)
            } else {
                (0, 1)
            };
            RxRate::Vht {
                mcs,
                nss,
                bw: (info2 & 3) as u8,
                sgi: info3 & 1 != 0,
            }
        }
        _ => RxRate::Unknown,
    }
}

/// `ath10k_htt_rx_h_signal`, the combined value only.
fn signal_of(d: &RxDesc) -> i32 {
    super::NOISE_FLOOR + i32::from(d.0[rd::PPDU_RSSI_COMB])
}

/// `ath10k_htt_rx_validate_amsdu`.
fn validate_amsdu(first: &RxDesc) -> bool {
    let info = first.msdu_end();
    let is_first = info & rd::MSDU_END_FIRST != 0;
    let is_last = info & rd::MSDU_END_LAST != 0;
    if is_first && is_last {
        return true;
    }
    if !is_first {
        return false;
    }
    let hdr = first.hdr();
    let fc = u16::from_le_bytes([hdr[0], hdr[1]]);
    let crypto = match first.enctype() {
        rd::ENCRYPT_NONE => 0,
        // ath10k_htt_rx_crypto_param_len: WEP40, WEP104
        0 | 1 => 4,
        // TKIP with and without MIC, CCMP, CCMP-256, GCMP, GCMP-256
        2 | 4 | 6 | 8 | 9 | 10 => 8,
        _ => 0,
    };
    const RFC1042: [u8; 6] = [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00];
    // the first subframe's DA comes after the header; an LLC header in its place is a flipped A-MSDU bit
    hdr.get(hdrlen(fc) + crypto..hdrlen(fc) + crypto + 6) != Some(&RFC1042[..])
}

/// The input of [`classify`] that does not come from the frames.
pub(crate) struct RxCtx {
    pub freq: Option<u32>,
    pub peer_id: u16,
    pub check_pn: bool,
}

/// `ath10k_htt_rx_h_ppdu`, `_h_unchain`, `_h_filter` and `_h_mpdu` for one MPDU, pushing what passes. The caller has
/// checked nothing; a frame with no descriptor at the head is impossible from the pop functions and is simply dropped.
pub(crate) fn classify(
    msdus: Vec<Msdu>,
    chained: bool,
    ctx: &RxCtx,
    ppdu: &mut PpduStatus,
    win: &mut PnWindows,
    drops: &mut RxDrops,
    out: &mut Vec<RxFrame>,
) {
    let Some(first) = msdus.first().and_then(|m| m.desc.clone()) else {
        return;
    };
    let att_first = first.attention();
    if att_first & rd::ATT_FIRST_MPDU != 0 {
        ppdu.rssi = signal_of(&first);
        ppdu.rate = rate_of(&first);
    }
    // ath10k_htt_rx_h_unchain: only a single raw MSDU can be put back together
    let msdus = if chained {
        if first.decap() != rd::DECAP_RAW || msdus.len() != 1 + usize::from(first.more_count()) {
            drops.unchain += 1;
            return;
        }
        let total: usize = msdus.iter().map(|m| m.bytes.len()).sum();
        if total > MAX_FRAME_BYTES {
            drops.unchain += 1;
            return;
        }
        let mut bytes = Vec::with_capacity(total);
        for m in &msdus {
            bytes.extend_from_slice(&m.bytes);
        }
        alloc::vec![Msdu {
            desc: Some(first.clone()),
            bytes,
        }]
    } else {
        msdus
    };
    // ath10k_htt_rx_amsdu_allowed
    let Some(freq) = ctx.freq else {
        drops.no_channel += 1;
        return;
    };
    if !validate_amsdu(&first) {
        drops.bad_amsdu += 1;
        return;
    }
    // "Some attention flags are valid only in the last MSDU."
    let last_att = msdus
        .iter()
        .rev()
        .find_map(|m| m.desc.as_ref())
        .map_or(att_first, RxDesc::attention);
    if att_first & rd::ATT_MGMT_TYPE != 0 {
        // RX_FLAG_ONLY_MONITOR: "Firmware reports all necessary management frames via WMI already."
        drops.mgmt += 1;
        return;
    }
    if last_att & rd::ATT_FCS_ERR != 0 {
        drops.fcs += 1;
        return;
    }
    if last_att & rd::ATT_TKIP_MIC_ERR != 0 {
        drops.mic += 1;
        return;
    }
    let enctype = first.enctype();
    let encrypted = enctype != rd::ENCRYPT_NONE;
    let decrypted = encrypted
        && last_att & rd::ATT_DECRYPT_ERR == 0
        && last_att & rd::ATT_PEER_IDX_INVALID == 0;
    if encrypted && !decrypted {
        // the device could not decrypt it and nothing here can
        drops.crypto += 1;
        return;
    }
    let hdr = first.hdr();
    let fc = u16::from_le_bytes([hdr[0], hdr[1]]);
    let mcast = last_att & rd::ATT_MCAST_BCAST != 0;
    let tid = first.tid();
    let mut pn = 0;
    if decrypted && enctype == rd::ENCRYPT_AES_CCM_WPA2 && ctx.check_pn {
        // ath10k_htt_rx_h_get_pn over the crypto header kept in rx_hdr_status (decap_align_bytes is 1 here)
        if let Some(e) = hdr.get(hdrlen(fc)..hdrlen(fc) + 8) {
            pn = u64::from(e[0])
                | u64::from(e[1]) << 8
                | u64::from(e[4]) << 16
                | u64::from(e[5]) << 24
                | u64::from(e[6]) << 32
                | u64::from(e[7]) << 40;
            if !win.accept(mcast, tid, pn) {
                drops.replay += 1;
                return;
            }
        }
    }
    let mut air_hdr = [0u8; 64];
    air_hdr.copy_from_slice(hdr);
    for m in msdus {
        let d = m.desc.as_ref().unwrap_or(&first);
        let decap = match d.decap() {
            rd::DECAP_RAW => Decap::Raw,
            rd::DECAP_NATIVE_WIFI => Decap::NativeWifi,
            rd::DECAP_ETHERNET2 => Decap::Ethernet2,
            _ => Decap::Snap8023,
        };
        let payload_off = if decap == Decap::NativeWifi {
            let nw = m
                .bytes
                .get(..2)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
            hdrlen(nw).div_ceil(4) * 4
        } else {
            0
        };
        out.push(RxFrame {
            frame: m.bytes,
            decap,
            payload_off,
            air_hdr,
            decrypted,
            mcast,
            tid,
            peer_id: ctx.peer_id,
            freq,
            rssi: ppdu.rssi,
            rate: ppdu.rate,
            pn,
        });
    }
}

// ---- replay ---------------------------------------------------------------------------------------------------------

/// One key's replay state for one TID: see the module comment.
#[derive(Clone, Copy, Debug, Default)]
pub struct PnWindow {
    max: u64,
    seen: u64,
    any: bool,
}

impl PnWindow {
    pub fn accept(&mut self, pn: u64) -> bool {
        if !self.any {
            self.any = true;
            self.max = pn;
            self.seen = 1;
            return true;
        }
        if pn > self.max {
            let shift = pn - self.max;
            self.seen = if shift >= 64 { 0 } else { self.seen << shift };
            self.seen |= 1;
            self.max = pn;
            return true;
        }
        let back = self.max - pn;
        if back >= 64 || self.seen & (1 << back) != 0 {
            return false;
        }
        self.seen |= 1 << back;
        true
    }
}

/// The windows of the current keys: one per TID for the pairwise key, one for the group key.
#[derive(Clone, Debug, Default)]
pub(crate) struct PnWindows {
    pairwise: [PnWindow; 17],
    group: PnWindow,
}

impl PnWindows {
    fn accept(&mut self, mcast: bool, tid: u8, pn: u64) -> bool {
        if mcast {
            self.group.accept(pn)
        } else {
            self.pairwise[usize::from(tid).min(16)].accept(pn)
        }
    }
    pub fn reset(&mut self, pairwise: bool) {
        if pairwise {
            self.pairwise = Default::default();
        } else {
            self.group = PnWindow::default();
        }
    }
}

// ---- transmit -------------------------------------------------------------------------------------------------------

/// What an MSDU id holds, to tell its completion apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxKind {
    Data,
    Mgmt,
}

/// `htt->txbuf`, `htt->frag_desc`, `pending_tx` and a frame buffer for each id.
pub(crate) struct HttTx {
    txbuf: DmaBuf,
    frag: DmaBuf,
    frames: Vec<DmaBuf>,
    slots: Vec<Option<TxKind>>,
    pending: usize,
}

/// Why a frame was not queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TxError {
    Busy,
    TooLong,
    Ce(CeError),
}

impl HttTx {
    /// `ath10k_htt_tx_alloc_cont_txbuf_64`, `ath10k_htt_tx_alloc_cont_frag_desc_64` and the frames.
    pub fn alloc<D: Dma>(dma: &mut D, cfg: &Config) -> Option<HttTx> {
        let n = usize::from(cfg.max_pending_tx);
        // dma_alloc_coherent in the C (htt_tx.c): the device writes the tx buffer array and the frag bank. The frame
        // copies stay on `alloc`: Linux maps packet buffers streaming, cached here (recorded).
        let txbuf = dma.alloc_coherent(n * htt::TXBUF_64_LEN)?;
        let Some(frag) = dma.alloc_coherent(n * htt::MSDU_EXT_DESC_64_LEN) else {
            dma.free(txbuf);
            return None;
        };
        let mut frames = Vec::with_capacity(n);
        for _ in 0..n {
            match dma.alloc(cfg.tx_frame_size) {
                Some(b) => frames.push(b),
                None => {
                    for b in frames {
                        dma.free(b);
                    }
                    dma.free(txbuf);
                    dma.free(frag);
                    return None;
                }
            }
        }
        let mut slots = Vec::with_capacity(n);
        slots.resize(n, None);
        Some(HttTx {
            txbuf,
            frag,
            frames,
            slots,
            pending: 0,
        })
    }

    pub fn frag_paddr(&self) -> u64 {
        self.frag.paddr
    }
    pub fn max_id(&self) -> u16 {
        (self.slots.len() - 1) as u16
    }
    pub fn pending(&self) -> usize {
        self.pending
    }
    pub fn has_room(&self) -> bool {
        self.pending < self.slots.len()
    }

    /// `ath10k_htt_tx_64` for `flags0`/`flags1` the caller composed: copy the frame into id's buffer, fill its fragment
    /// descriptor (`mgmt`: none, the descriptor points at the frame) and tx buffer, and send the two gather items on
    /// CE4 with no context. Space for both is checked first; a second send that fails anyway is taken back
    /// (`__ath10k_ce_send_revert`).
    #[allow(clippy::too_many_arguments)]
    pub fn send<P: Platform>(
        &mut self,
        plat: &mut P,
        hif: &mut Hif,
        eid: u8,
        kind: TxKind,
        flags0: u8,
        flags1: u16,
        frame: &[u8],
    ) -> Result<u16, TxError> {
        let Some(id) = self.slots.iter().position(Option::is_none) else {
            return Err(TxError::Busy);
        };
        let buf = self.frames[id].clone();
        if frame.len() > buf.len || frame.len() > usize::from(u16::MAX) {
            return Err(TxError::TooLong);
        }
        if hif.ce.num_free_src_entries(CE_POLL_PIPE).unwrap_or(0) < 2 {
            hif.drain_send(plat, CE_POLL_PIPE);
            if hif.ce.num_free_src_entries(CE_POLL_PIPE).unwrap_or(0) < 2 {
                return Err(TxError::Busy);
            }
        }
        let (regs, mem, dma) = plat.parts();
        dma.write(&buf, 0, frame);
        let frags_paddr = if kind == TxKind::Mgmt {
            buf.paddr
        } else {
            let d = htt::MsduExtDesc64 {
                paddr: buf.paddr,
                len: frame.len() as u16,
                checksum_offload: false,
            };
            let off = id * htt::MSDU_EXT_DESC_64_LEN;
            dma.write(&self.frag, off, &d.to_le_bytes());
            self.frag.paddr + off as u64
        };
        let prefetch = frame.len().min(htt::PREFETCH_LEN).div_ceil(4) * 4;
        let desc = htt::DataTxDesc64 {
            flags0,
            flags1,
            len: frame.len() as u16,
            id: id as u16,
            frags_paddr,
            freq: None,
        };
        let item = htt::tx_item_64(eid, &desc, prefetch);
        let tb_off = id * htt::TXBUF_64_LEN + htt::TXBUF_64_HTC_HDR_OFF;
        dma.write(&self.txbuf, tb_off, &item);
        let item_paddr = self.txbuf.paddr + tb_off as u64;
        hif.ce
            .send(
                regs,
                mem,
                CE_POLL_PIPE,
                CTX_NONE,
                item_paddr,
                item.len() as u32,
                0,
                SEND_FLAG_GATHER,
            )
            .map_err(TxError::Ce)?;
        if let Err(e) = hif.ce.send(
            regs,
            mem,
            CE_POLL_PIPE,
            CTX_NONE,
            buf.paddr,
            prefetch as u32,
            0,
            0,
        ) {
            hif.ce.send_revert(regs, CE_POLL_PIPE);
            return Err(TxError::Ce(e));
        }
        self.slots[id] = Some(kind);
        self.pending += 1;
        Ok(id as u16)
    }

    /// `ath10k_txrx_tx_unref`: what the id held, `None` for an id out of range or not pending.
    pub fn complete(&mut self, id: u16) -> Option<TxKind> {
        let k = self.slots.get_mut(usize::from(id))?.take()?;
        self.pending -= 1;
        Some(k)
    }

    pub fn free<D: Dma>(&mut self, dma: &mut D) {
        for b in self.frames.drain(..) {
            dma.free(b);
        }
        dma.free(self.txbuf.clone());
        dma.free(self.frag.clone());
    }
}
